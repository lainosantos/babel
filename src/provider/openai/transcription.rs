//! Dedicated ASR protocol. Source PCM is committed with local energy VAD;
//! no response.create, translation instructions, output modality or voice.
use std::collections::VecDeque;

use super::*;

pub(super) fn validate(config: &SessionConfig) -> Result<()> {
    ensure!(
        (1..=120).contains(&config.connect_timeout_secs) && config.max_reconnect_attempts <= 20,
        "invalid OpenAI ASR timeout/reconnect limits"
    );
    ensure!(
        (100..=2000).contains(&config.vad_silence_ms),
        "OpenAI ASR silence must be 100..2000 ms"
    );
    ensure!(
        config.source_language.len() <= 80
            && (config.source_language.is_empty()
                || config.source_language == "auto"
                || config
                    .source_language
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-')),
        "OpenAI ASR source must be a language code or auto"
    );
    Ok(())
}

pub(super) fn setup(config: &SessionConfig, model: &str) -> Value {
    let mut transcription = json!({"model": model});
    if !config.source_language.is_empty() && config.source_language != "auto" {
        let language = config.source_language.to_ascii_lowercase();
        let language = if language.starts_with("zh-") {
            language.as_str()
        } else {
            language.split('-').next().unwrap_or(&language)
        };
        if super::super::known_model_family(model, "gpt-realtime-whisper") {
            transcription["language"] = json!(language);
        } else {
            transcription["languages"] = json!([language]);
        }
    }
    json!({"type":"session.update", "session":{"type":"transcription", "audio":{"input":{
        "format":{"type":"audio/pcm","rate":24000}, "transcription":transcription, "turn_detection":null
    }}}})
}

/// Final events can arrive out of order. Commit acknowledgements establish the
/// actual input order; bounded storage prevents stalled turns growing memory.
#[derive(Default)]
pub(super) struct FinalTranscripts {
    order: VecDeque<String>,
    finals: HashMap<String, String>,
}

impl FinalTranscripts {
    pub(super) fn decode(&mut self, value: &Value) -> SessionResult<Vec<ProviderEvent>> {
        match value["type"].as_str().unwrap_or("") {
            "input_audio_buffer.committed" => {
                let id = item_id(value)?;
                if self.order.len() >= 64 || self.order.iter().any(|item| item == id) {
                    return Err(Failure::fatal(
                        "OpenAI ASR commit queue exceeded safe limits",
                    ));
                }
                self.order.push_back(id.into());
            }
            "conversation.item.input_audio_transcription.completed" => {
                let id = item_id(value)?;
                let text = value["transcript"]
                    .as_str()
                    .ok_or_else(|| Failure::fatal("OpenAI ASR final transcript is missing"))?;
                if text.len() > 32768 || self.finals.len() >= 64 || self.finals.contains_key(id) {
                    return Err(Failure::fatal(
                        "OpenAI ASR final transcript exceeds safe limits",
                    ));
                }
                self.finals.insert(id.into(), text.into());
            }
            "conversation.item.input_audio_transcription.failed" => {
                return Err(Failure::fatal("OpenAI input transcription failed"));
            }
            "session.closed" => return Err(Failure::retry("OpenAI ASR session expired")),
            // Never treat an accidental conversational session as successful ASR.
            kind if kind.starts_with("response.") || kind.starts_with("session.output_") => {
                return Err(Failure::fatal(
                    "OpenAI ASR unexpectedly returned generated content",
                ));
            }
            // Partial hypotheses are intentionally not appended to final TXT.
            _ => (),
        }
        let mut result = Vec::new();
        while let Some(id) = self.order.front() {
            let Some(text) = self.finals.remove(id) else {
                break;
            };
            self.order.pop_front();
            if !text.is_empty() {
                result.push(ProviderEvent::Transcript {
                    input: true,
                    text,
                    metadata: TranscriptMetadata::default(),
                });
            }
            result.push(ProviderEvent::TurnComplete);
        }
        Ok(result)
    }
}

fn item_id(value: &Value) -> SessionResult<&str> {
    value["item_id"]
        .as_str()
        .filter(|id| !id.is_empty() && id.len() <= 512 && !id.chars().any(char::is_control))
        .ok_or_else(|| Failure::fatal("invalid OpenAI ASR item identifier"))
}

struct Gate {
    preroll: VecDeque<i16>,
    active: usize,
    trailing: usize,
    silence: usize,
}
impl Gate {
    fn new(silence_ms: u32) -> Self {
        Self {
            preroll: VecDeque::with_capacity(1600),
            active: 0,
            trailing: 0,
            silence: silence_ms as usize * 16,
        }
    }
    fn push(&mut self, samples: &[i16]) -> (Vec<i16>, bool) {
        let mut output = Vec::with_capacity(samples.len() + 1600);
        if samples.is_empty() {
            return (output, false);
        }
        let power = samples
            .iter()
            .map(|s| (f32::from(*s) / 32768.0).powi(2))
            .sum::<f32>()
            / samples.len() as f32;
        let voiced = power >= 0.0001; // 0.01 RMS; silence frames remain inside an active utterance.
        if self.active == 0 && !voiced {
            for sample in samples {
                if self.preroll.len() == 1600 {
                    self.preroll.pop_front();
                }
                self.preroll.push_back(*sample);
            }
            return (output, false);
        }
        if self.active == 0 {
            output.extend(self.preroll.drain(..));
        }
        output.extend_from_slice(samples);
        self.active += output.len();
        self.trailing = if voiced {
            0
        } else {
            self.trailing + samples.len()
        };
        let commit =
            self.active >= 160_000 || (self.active >= 1600 && self.trailing >= self.silence);
        if commit {
            self.active = 0;
            self.trailing = 0;
        }
        (output, commit)
    }
    fn finish(&mut self) -> bool {
        if self.active < 1600 {
            return false;
        }
        self.active = 0;
        self.trailing = 0;
        true
    }
}

pub(super) async fn send_audio(
    mut writer: SplitSink<Socket, Message>,
    audio: &mut mpsc::Receiver<Vec<i16>>,
    mut control: mpsc::Receiver<Message>,
    silence_ms: u32,
) -> SessionResult<()> {
    let mut gate = Gate::new(silence_ms);
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    heartbeat.tick().await;
    let mut inactivity = tokio::time::interval(Duration::from_millis(100));
    inactivity.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_audio = Instant::now();
    let mut resampler = Resampler::new(16000, 24000);
    let mut input = Vec::with_capacity(17600);
    let mut output = Vec::with_capacity(26400);
    let mut bytes = Vec::with_capacity(52800);
    loop {
        tokio::select! {
            message = control.recv() => {
                io_deadline(writer.send(message.ok_or_else(||Failure::retry("OpenAI ASR receive loop stopped"))?)).await?;
            }
            samples = audio.recv() => {
                let samples = samples.ok_or_else(||Failure::fatal("audio source closed unexpectedly"))?;
                if samples.len()>16000 { return Err(Failure::fatal("input audio chunk exceeds one second")); }
                if samples.is_empty() { continue; }
                last_audio = Instant::now();
                let (samples, commit) = gate.push(&samples);
                if samples.is_empty() { continue; }
                input.clear(); output.clear(); bytes.clear();
                input.extend(samples.iter().map(|s| f32::from(*s) / 32768.0));
                resampler.process(&input,&mut output);
                if commit { resampler.process(&[0.0;64],&mut output); }
                bytes.extend(output.iter().flat_map(|sample| ((sample.clamp(-1.0,1.0)*32767.0).round() as i16).to_le_bytes()));
                if !bytes.is_empty() {
                    io_deadline(writer.send(Message::Text(json!({"type":"input_audio_buffer.append","audio":STANDARD.encode(&bytes)}).to_string().into()))).await?;
                }
                if commit { commit_audio(&mut writer).await?; }
            }
            _ = inactivity.tick() => {
                if last_audio.elapsed() >= Duration::from_millis(u64::from(silence_ms)) && gate.finish() {
                    output.clear(); bytes.clear();
                    resampler.process(&[0.0;64],&mut output);
                    bytes.extend(output.iter().flat_map(|sample| ((sample.clamp(-1.0,1.0)*32767.0).round() as i16).to_le_bytes()));
                    if !bytes.is_empty() { io_deadline(writer.send(Message::Text(json!({"type":"input_audio_buffer.append","audio":STANDARD.encode(&bytes)}).to_string().into()))).await?; }
                    commit_audio(&mut writer).await?;
                }
            }
            _ = heartbeat.tick() => { io_deadline(writer.send(Message::Ping(Vec::new().into()))).await?; }
        }
    }
}

async fn commit_audio(writer: &mut SplitSink<Socket, Message>) -> SessionResult<()> {
    io_deadline(
        writer.send(Message::Text(
            json!({"type":"input_audio_buffer.commit"})
                .to_string()
                .into(),
        )),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_text_replaces_partials_and_keeps_committed_order() {
        let mut state = FinalTranscripts::default();
        for id in ["a", "b"] {
            assert!(
                state
                    .decode(&json!({"type":"input_audio_buffer.committed","item_id":id}))
                    .unwrap()
                    .is_empty()
            );
        }
        assert!(state.decode(&json!({"type":"conversation.item.input_audio_transcription.delta","item_id":"a","delta":"wrong"})).unwrap().is_empty());
        assert!(state.decode(&json!({"type":"conversation.item.input_audio_transcription.completed","item_id":"b","transcript":" second"})).unwrap().is_empty());
        let events = state.decode(&json!({"type":"conversation.item.input_audio_transcription.completed","item_id":"a","transcript":"first"})).unwrap();
        assert_eq!(
            events,
            vec![
                ProviderEvent::Transcript {
                    input: true,
                    text: "first".into(),
                    metadata: Default::default()
                },
                ProviderEvent::TurnComplete,
                ProviderEvent::Transcript {
                    input: true,
                    text: " second".into(),
                    metadata: Default::default()
                },
                ProviderEvent::TurnComplete
            ]
        );
    }

    #[test]
    fn no_silence_commits_bounded_utterances_or_audio_generation() {
        let mut gate = Gate::new(300);
        for _ in 0..20 {
            assert_eq!(gate.push(&[0; 1600]), (vec![], false));
        }
        assert_eq!(gate.push(&[5000; 1600]).0.len(), 3200);
        assert!(!gate.push(&[0; 1600]).1);
        assert!(!gate.push(&[0; 1600]).1);
        assert!(gate.push(&[0; 1600]).1);
        assert!(!gate.finish());
        let mut gate = Gate::new(300);
        for _ in 0..9 {
            assert!(!gate.push(&[5000; 16000]).1);
        }
        assert!(gate.push(&[5000; 16000]).1);
        assert!(
            FinalTranscripts::default()
                .decode(&json!({"type":"response.output_audio.delta","delta":"AAAA"}))
                .is_err()
        );
    }
}
