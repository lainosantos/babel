//! Dedicated ASR protocol. Source PCM is committed with local energy VAD;
//! no response.create, translation instructions, output modality or voice.
use std::collections::VecDeque;

use super::*;

const FINAL_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
struct InputFinished {
    commits: u64,
    at: Instant,
}

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
    fn finish(&mut self) -> Option<usize> {
        if self.active == 0 {
            return None;
        }
        let padding = 1600usize.saturating_sub(self.active);
        self.active = 0;
        self.trailing = 0;
        Some(padding)
    }
}

/// Source EOF ends capture, not recognition. Count submitted commits separately
/// from server acknowledgements so a delayed acknowledgement cannot make an
/// empty receive queue look like successful completion.
pub(super) async fn run_live(
    socket: Socket,
    audio: &mut mpsc::Receiver<Vec<i16>>,
    events: &mpsc::Sender<ProviderEvent>,
    silence_ms: u32,
) -> SessionResult<()> {
    let (writer, reader) = socket.split();
    let (control_tx, control_rx) = mpsc::channel(8);
    let (closed_tx, closed_rx) = tokio::sync::watch::channel(None::<InputFinished>);
    let mut deadline_closed = closed_rx.clone();
    let final_deadline = async {
        let finished = *deadline_closed
            .wait_for(Option::is_some)
            .await
            .map_err(|_| Failure::fatal("OpenAI ASR input did not finish"))?;
        tokio::time::sleep_until(finished.unwrap().at + FINAL_TIMEOUT).await;
        Err(Failure::fatal(
            "OpenAI ASR final transcript acknowledgement timed out",
        ))
    };
    tokio::select! {
        biased;
        result = final_deadline => result,
        result = send_audio(writer, audio, control_rx, silence_ms, closed_tx) => result,
        result = receive_finals(reader, events, control_tx, closed_rx) => result,
    }
}

async fn send_audio(
    mut writer: SplitSink<Socket, Message>,
    audio: &mut mpsc::Receiver<Vec<i16>>,
    mut control: mpsc::Receiver<Message>,
    silence_ms: u32,
    closed: tokio::sync::watch::Sender<Option<InputFinished>>,
) -> SessionResult<()> {
    let mut input_open = true;
    let mut commits = 0u64;
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
            samples = audio.recv(), if input_open => {
                let Some(samples) = samples else {
                    if let Some(padding) = gate.finish() {
                        finish_audio(&mut writer, &mut resampler, padding).await?;
                        commits += 1;
                    }
                    input_open = false;
                    closed.send_replace(Some(InputFinished { commits, at: Instant::now() }));
                    continue;
                };
                if samples.len()>16000 { return Err(Failure::fatal("input audio chunk exceeds one second")); }
                if samples.is_empty() { continue; }
                last_audio = Instant::now();
                let (samples, commit) = gate.push(&samples);
                if samples.is_empty() { continue; }
                input.clear(); output.clear(); bytes.clear();
                input.extend(samples.iter().map(|s| f32::from(*s) / 32768.0));
                resampler.process(&input,&mut output);
                bytes.extend(output.iter().flat_map(|sample| ((sample.clamp(-1.0,1.0)*32767.0).round() as i16).to_le_bytes()));
                if !bytes.is_empty() {
                    io_deadline(writer.send(Message::Text(json!({"type":"input_audio_buffer.append","audio":STANDARD.encode(&bytes)}).to_string().into()))).await?;
                }
                if commit {
                    finish_audio(&mut writer, &mut resampler, 0).await?;
                    commits += 1;
                }
            }
            _ = inactivity.tick(), if input_open => {
                if last_audio.elapsed() >= Duration::from_millis(u64::from(silence_ms))
                    && let Some(padding) = gate.finish()
                {
                    finish_audio(&mut writer, &mut resampler, padding).await?;
                    commits += 1;
                }
            }
            _ = heartbeat.tick() => { io_deadline(writer.send(Message::Ping(Vec::new().into()))).await?; }
        }
    }
}

async fn finish_audio(
    writer: &mut SplitSink<Socket, Message>,
    resampler: &mut Resampler,
    padding: usize,
) -> SessionResult<()> {
    let mut output = Vec::with_capacity((padding + 64) * 3 / 2);
    // Only an already captured voiced turn is padded to the API's minimum
    // commit duration; idle silence never creates a new turn.
    resampler.process(&vec![0.0; padding + 64], &mut output);
    let bytes: Vec<u8> = output
        .iter()
        .flat_map(|sample| ((sample.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes())
        .collect();
    if !bytes.is_empty() {
        io_deadline(
            writer.send(Message::Text(
                json!({"type":"input_audio_buffer.append","audio":STANDARD.encode(&bytes)})
                    .to_string()
                    .into(),
            )),
        )
        .await?;
    }
    commit_audio(writer).await?;
    *resampler = Resampler::new(16000, 24000);
    Ok(())
}

async fn receive_finals(
    mut reader: SplitStream<Socket>,
    events: &mpsc::Sender<ProviderEvent>,
    control: mpsc::Sender<Message>,
    mut closed: tokio::sync::watch::Receiver<Option<InputFinished>>,
) -> SessionResult<()> {
    let mut transcription = FinalTranscripts::default();
    let mut completed = 0u64;
    loop {
        let finished = *closed.borrow_and_update();
        if let Some(finished) = finished {
            if completed > finished.commits {
                return Err(Failure::fatal(
                    "OpenAI ASR returned an unexpected final transcript",
                ));
            }
            if completed == finished.commits
                && transcription.order.is_empty()
                && transcription.finals.is_empty()
            {
                return Ok(());
            }
        }
        let message = tokio::select! {
            biased;
            result = closed.changed(), if finished.is_none() => {
                result.map_err(|_| Failure::fatal("OpenAI ASR input did not finish"))?;
                continue;
            }
            message = timeout(Duration::from_secs(45), reader.next()) => message
                .map_err(|_| Failure::retry("OpenAI connection stopped responding"))?
                .ok_or_else(|| Failure::retry("OpenAI WebSocket closed before transcription completed"))?
                .map_err(socket_failure)?,
        };
        let value = match message {
            Message::Text(text) => parse_json(text.as_bytes())?,
            Message::Binary(bytes) => parse_json(&bytes)?,
            Message::Ping(data) => {
                control
                    .try_send(Message::Pong(data))
                    .map_err(|_| Failure::retry("OpenAI control channel is congested"))?;
                continue;
            }
            Message::Pong(_) => continue,
            Message::Close(frame) => {
                return Err(match frame.map(|frame| u16::from(frame.code)) {
                    Some(1008) => {
                        Failure::fatal("OpenAI rejected session policy or authentication")
                    }
                    _ => Failure::retry("OpenAI WebSocket closed before transcription completed"),
                });
            }
            Message::Frame(_) => return Err(Failure::fatal("invalid raw OpenAI frame")),
        };
        for event in transcription.decode(&value)? {
            if matches!(event, ProviderEvent::TurnComplete) {
                completed += 1;
            }
            emit(events, event).await?;
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

/// One historical utterance in flight keeps provider ordering and memory
/// bounded. Each explicit commit must receive its own committed + completed
/// events before the next utterance is submitted; EOF alone is not success.
pub(super) async fn run_history(
    mut socket: Socket,
    audio: &mut mpsc::Receiver<Vec<i16>>,
    events: &mpsc::Sender<ProviderEvent>,
    silence_ms: u32,
) -> SessionResult<()> {
    let options = super::super::local::history_options(silence_ms);
    let (tx, mut rx) = mpsc::channel(2);
    let producer = async {
        super::super::local::segment_history_borrowed(audio, tx, &options)
            .await
            .map_err(|_| Failure::fatal("OpenAI historical audio segmentation failed"))
    };
    let consumer = async {
        while let Some(segment) = rx.recv().await {
            timeout(Duration::from_secs(120), async {
                let mut input: Vec<f32> = segment.samples.iter().map(|s| f32::from(*s) / 32768.0).collect();
                // Realtime commits require at least 100 ms. Padding does not
                // alter the captured timeline associated with this utterance.
                input.resize(input.len().max(1600), 0.0);
                let expected = input.len() * 3 / 2;
                input.extend_from_slice(&[0.0; 64]);
                let mut output = Vec::with_capacity(expected + 128);
                Resampler::new(16000, 24000).process(&input, &mut output);
                output.truncate(expected);
                for chunk in output.chunks(24000) {
                    let bytes: Vec<u8> = chunk.iter().flat_map(|sample| ((sample.clamp(-1.0,1.0)*32767.0).round() as i16).to_le_bytes()).collect();
                    io_deadline(socket.send(Message::Text(json!({"type":"input_audio_buffer.append","audio":STANDARD.encode(&bytes)}).to_string().into()))).await?;
                }
                io_deadline(socket.send(Message::Text(json!({"type":"input_audio_buffer.commit"}).to_string().into()))).await?;
                let mut state = FinalTranscripts::default();
                loop {
                    let value = receive_setup(&mut socket).await?;
                    let decoded = state.decode(&value)?;
                    let complete = decoded.iter().any(|event| matches!(event, ProviderEvent::TurnComplete));
                    for mut event in decoded {
                        if let ProviderEvent::Transcript { metadata, .. } = &mut event {
                            // Captured segment alignment, not invented word timestamps.
                            metadata.alignment_ms = Some(segment.start_sample / 16);
                        }
                        emit(events, event).await?;
                    }
                    if complete { return Ok(()); }
                }
            }).await.map_err(|_| Failure::fatal("OpenAI historical transcription final acknowledgement timed out"))??;
        }
        Ok(())
    };
    tokio::try_join!(producer, consumer)?;
    Ok(())
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
        assert_eq!(gate.finish(), None);
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

    #[tokio::test]
    async fn live_idle_flush_then_eof_waits_for_final_acknowledgements() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let (last_committed, last_commit) = tokio::sync::oneshot::channel();
        let (finish, finishing) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let mut socket = tokio_tungstenite::accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            let mut turn = 0;
            let mut bytes = 0;
            loop {
                let message = socket.next().await.unwrap().unwrap();
                let Message::Text(text) = message else {
                    panic!("unexpected frame: {message:?}");
                };
                let value: Value = serde_json::from_str(&text).unwrap();
                match value["type"].as_str().unwrap() {
                    "input_audio_buffer.append" => {
                        bytes += STANDARD
                            .decode(value["audio"].as_str().unwrap())
                            .unwrap()
                            .len()
                    }
                    "input_audio_buffer.commit" => {
                        assert!(
                            bytes >= 4800,
                            "a short tail must meet the minimum commit size"
                        );
                        bytes = 0;
                        if turn == 1 {
                            // No acknowledgement exists yet; an empty ordering
                            // queue must not make the provider finish early.
                            last_committed.send(()).unwrap();
                            finishing.await.unwrap();
                            // Completed may arrive before its committed event.
                            socket.send(Message::Text(json!({"type":"conversation.item.input_audio_transcription.completed","item_id":"last","transcript":"final tail"}).to_string().into())).await.unwrap();
                            socket
                                .send(Message::Text(
                                    json!({"type":"input_audio_buffer.committed","item_id":"last"})
                                        .to_string()
                                        .into(),
                                ))
                                .await
                                .unwrap();
                            return;
                        }
                        socket
                            .send(Message::Text(
                                json!({"type":"input_audio_buffer.committed","item_id":"first"})
                                    .to_string()
                                    .into(),
                            ))
                            .await
                            .unwrap();
                        socket.send(Message::Text(json!({"type":"conversation.item.input_audio_transcription.completed","item_id":"first","transcript":"before idle"}).to_string().into())).await.unwrap();
                        turn += 1;
                    }
                    other => panic!("unexpected request: {other}"),
                }
            }
        });
        let (socket, _) = tokio_tungstenite::connect_async(endpoint).await.unwrap();
        let (audio, mut input) = mpsc::channel(4);
        let (events, mut output) = mpsc::channel(8);
        let worker = tokio::spawn(async move { run_live(socket, &mut input, &events, 100).await });
        audio.send(vec![5000; 1600]).await.unwrap();
        assert!(
            matches!(timeout(Duration::from_secs(2), output.recv()).await.unwrap(), Some(ProviderEvent::Transcript { text, .. }) if text == "before idle")
        );
        assert_eq!(output.recv().await, Some(ProviderEvent::TurnComplete));
        assert!(!worker.is_finished());
        audio.send(vec![5000; 160]).await.unwrap();
        drop(audio);
        timeout(Duration::from_secs(2), last_commit)
            .await
            .unwrap()
            .unwrap();
        assert!(!worker.is_finished());
        finish.send(()).unwrap();
        timeout(Duration::from_secs(2), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            matches!(output.recv().await, Some(ProviderEvent::Transcript { text, .. }) if text == "final tail")
        );
        assert_eq!(output.recv().await, Some(ProviderEvent::TurnComplete));
        assert_eq!(output.recv().await, None);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn live_silence_eof_does_not_commit_or_wait_for_generated_text() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let socket = tokio_tungstenite::accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
            drop(socket);
        });
        let (socket, _) = tokio_tungstenite::connect_async(endpoint).await.unwrap();
        let (audio, mut input) = mpsc::channel(1);
        audio.send(vec![0; 1600]).await.unwrap();
        drop(audio);
        let (events, mut output) = mpsc::channel(1);
        timeout(
            Duration::from_secs(1),
            run_live(socket, &mut input, &events, 100),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(output.try_recv().is_err());
        server.abort();
    }

    #[tokio::test]
    async fn live_eof_missing_final_acknowledgement_is_a_bounded_failure() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let (committed, commit) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let mut socket = tokio_tungstenite::accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            while let Some(Ok(message)) = socket.next().await {
                if let Message::Text(text) = message
                    && serde_json::from_str::<Value>(&text).unwrap()["type"]
                        == "input_audio_buffer.commit"
                {
                    committed.send(()).unwrap();
                    std::future::pending::<()>().await;
                    break;
                }
            }
        });
        let (socket, _) = tokio_tungstenite::connect_async(endpoint).await.unwrap();
        let (audio, mut input) = mpsc::channel(1);
        audio.send(vec![5000; 1600]).await.unwrap();
        drop(audio);
        let (events, _output) = mpsc::channel(8);
        let worker = tokio::spawn(async move { run_live(socket, &mut input, &events, 100).await });
        timeout(Duration::from_secs(2), commit)
            .await
            .unwrap()
            .unwrap();
        tokio::time::pause();
        tokio::time::advance(FINAL_TIMEOUT + Duration::from_millis(1)).await;
        let failure = worker.await.unwrap().unwrap_err();
        assert!(!failure.retryable);
        assert!(failure.message.contains("acknowledgement timed out"));
        server.abort();
    }
}
