//! Live Translate has continuous output, without a terminal turn acknowledgement.
use super::*;

// Google's Live Translate example allows four seconds after input EOF. Require
// that interval after the last audible output too, rather than cutting a late
// phrase off at a fixed time. This is a drain heuristic, not a provider receipt.
pub(super) const QUIET_TAIL: Duration = Duration::from_secs(4);

fn drain_timeout() -> Failure {
    Failure::fatal("Gemini translation output drain timed out; original audio remains unconfirmed")
}

pub(super) async fn run(
    mut socket: Socket,
    audio: &mut mpsc::Receiver<Vec<i16>>,
    events: &mpsc::Sender<ProviderEvent>,
    resume_handle: &mut Option<String>,
    submitted: &mut bool,
) -> SessionResult<()> {
    let mut eof = false;
    let mut stream_open = false;
    let mut pending_speech = false;
    let mut activity = crate::audio::speech::VoiceActivity::default();
    let mut last_input = Instant::now();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    heartbeat.tick().await;
    let mut inactivity = tokio::time::interval(Duration::from_secs(1));
    inactivity.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let quiet = tokio::time::sleep(QUIET_TAIL);
    let deadline = tokio::time::sleep(FINAL_TIMEOUT);
    tokio::pin!(quiet, deadline);
    loop {
        tokio::select! {
            samples = audio.recv(), if !eof => {
                match samples {
                    Some(samples) => {
                        if samples.is_empty() { continue; }
                        io_deadline(socket.send(audio_message(&samples)?)).await?;
                        *submitted = true;
                        stream_open = true;
                        pending_speech |= activity.speech(&samples);
                        last_input = Instant::now();
                    }
                    None => {
                        eof = true;
                        if !*submitted { return Ok(()); }
                        if stream_open { end_stream(&mut socket).await?; }
                        // VAD does not edit provider input. Give any output it
                        // missed the same observation window as recognized speech.
                        if activity.no_detected_speech() { pending_speech = false; }
                        quiet.as_mut().reset(Instant::now() + QUIET_TAIL);
                        deadline.as_mut().reset(Instant::now() + FINAL_TIMEOUT);
                    }
                }
            }
            value = receive(&mut socket) => {
                let value = value?;
                let output = dispatch(&value, events, resume_handle, eof);
                let audible = if eof {
                    timeout(deadline.deadline().saturating_duration_since(Instant::now()), output)
                        .await.map_err(|_| drain_timeout())??
                } else {
                    output.await?
                };
                if audible {
                    pending_speech = false;
                    if eof { quiet.as_mut().reset(Instant::now() + QUIET_TAIL); }
                } else if eof && value["serverContent"]["outputTranscription"]["text"]
                    .as_str().is_some_and(|text| !text.trim().is_empty()) {
                    // Translated text can precede its PCM. It extends observation
                    // but cannot substitute for translated audio delivery.
                    quiet.as_mut().reset(Instant::now() + QUIET_TAIL);
                }
            }
            _ = &mut quiet, if eof && !pending_speech => {
                // This is the local stream drain boundary, not an invented
                // Gemini generationComplete acknowledgement.
                timeout(deadline.deadline().saturating_duration_since(Instant::now()),
                    emit(events, ProviderEvent::TurnComplete))
                    .await.map_err(|_| drain_timeout())??;
                return Ok(());
            }
            _ = &mut deadline, if eof => {
                return Err(drain_timeout());
            }
            _ = inactivity.tick(), if !eof && stream_open => {
                if last_input.elapsed() >= Duration::from_secs(1) {
                    end_stream(&mut socket).await?;
                    stream_open = false;
                }
            }
            _ = heartbeat.tick() => io_deadline(socket.send(Message::Ping(Vec::new().into()))).await?,
        }
    }
}
