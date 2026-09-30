//! Persistent original-audio recognition with explicit, acknowledged turns.
//! Manual VAD gives each activityEnd one final inputTranscription; unlike an
//! automatic boundary, that final cannot acknowledge a later audio turn.
//! https://ai.google.dev/gemini-api/docs/live-api/live-transcribe#manual-vad-push-to-talk
use super::*;

const FINAL_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_TURN_SAMPLES: usize = 16_000 * 5;

fn final_timeout() -> Failure {
    Failure::fatal(
        "Gemini transcription final acknowledgement timed out; transcript may be incomplete",
    )
}

async fn before_final_deadline<T>(
    deadline: Option<Instant>,
    operation: impl Future<Output = SessionResult<T>>,
) -> SessionResult<T> {
    match deadline {
        Some(deadline) => {
            // timeout_at checks its future first: reject an already expired
            // deadline even when an event can be delivered immediately.
            if Instant::now() >= deadline {
                return Err(final_timeout());
            }
            tokio::time::timeout_at(deadline, operation)
                .await
                .map_err(|_| final_timeout())?
        }
        None => operation.await,
    }
}

struct Turn {
    start_sample: u64,
    samples: usize,
    trailing_zeros: usize,
    last_input: Instant,
}

async fn end_turn(socket: &mut Socket) -> SessionResult<Instant> {
    io_deadline(
        socket.send(Message::Text(
            json!({"realtimeInput":{"activityEnd":{}}})
                .to_string()
                .into(),
        )),
    )
    .await?;
    Ok(Instant::now() + FINAL_TIMEOUT)
}

pub(super) async fn run(
    mut socket: Socket,
    audio: &mut mpsc::Receiver<Vec<i16>>,
    events: &mpsc::Sender<ProviderEvent>,
    silence_ms: u32,
) -> SessionResult<()> {
    let silence = Duration::from_millis(u64::from(silence_ms.clamp(100, 2000)));
    let silence_samples = silence.as_millis() as usize * 16;
    let mut turn: Option<Turn> = None;
    let mut final_deadline = None;
    let mut eof = false;
    let mut position = 0u64;
    let mut last_received = Instant::now();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    heartbeat.tick().await;
    let mut idle = tokio::time::interval(Duration::from_millis(50));
    idle.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let deadline = final_deadline.unwrap_or(last_received + RECEIVE_TIMEOUT);
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                return Err(if final_deadline.is_some() {
                    final_timeout()
                } else {
                    Failure::retry("Gemini transcription connection stopped responding")
                });
            }
            _ = heartbeat.tick() => {
                before_final_deadline(final_deadline, io_deadline(socket.send(Message::Ping(Vec::new().into())))).await?;
            }
            _ = idle.tick(), if turn.is_some() && final_deadline.is_none() => {
                if turn.as_ref().is_some_and(|turn| turn.last_input.elapsed() >= silence) {
                    final_deadline = Some(end_turn(&mut socket).await?);
                }
            }
            samples = audio.recv(), if !eof && final_deadline.is_none() => {
                let Some(samples) = samples else {
                    eof = true;
                    if turn.is_none() { return Ok(()); }
                    final_deadline = Some(end_turn(&mut socket).await?);
                    continue;
                };
                if samples.len() > MAX_INPUT_SAMPLES {
                    return Err(Failure::fatal("input audio chunk exceeds one second"));
                }
                if samples.is_empty() { continue; }
                let start = position;
                position = position.saturating_add(samples.len() as u64);
                // Never classify quiet speech as silence. Only exact digital
                // zero is known empty, and its duration still advances offsets.
                if turn.is_none() && samples.iter().all(|sample| *sample == 0) {
                    continue;
                }
                if turn.is_none() {
                    io_deadline(socket.send(Message::Text(
                        json!({"realtimeInput":{"activityStart":{}}}).to_string().into()
                    ))).await?;
                    turn = Some(Turn { start_sample: start, samples: 0, trailing_zeros: 0, last_input: Instant::now() });
                }
                io_deadline(socket.send(audio_message(&samples)?)).await?;
                let current = turn.as_mut().expect("activity started before audio");
                let zeros = samples.iter().rev().take_while(|sample| **sample == 0).count();
                current.trailing_zeros = if zeros == samples.len() { current.trailing_zeros + zeros } else { zeros };
                current.samples += samples.len();
                current.last_input = Instant::now();
                if current.samples >= MAX_TURN_SAMPLES || current.trailing_zeros >= silence_samples {
                    final_deadline = Some(end_turn(&mut socket).await?);
                }
            }
            incoming = socket.next() => {
                let incoming = incoming
                    .ok_or_else(|| Failure::retry("Gemini transcription connection closed before completion"))?
                    .map_err(socket_error)?;
                last_received = Instant::now();
                let value = match incoming {
                    Message::Text(text) => parse_json(text.as_bytes())?,
                    Message::Binary(bytes) => parse_json(&bytes)?,
                    Message::Ping(data) => { before_final_deadline(final_deadline, io_deadline(socket.send(Message::Pong(data)))).await?; continue; }
                    Message::Pong(_) => continue,
                    Message::Close(frame) => return Err(close_failure(frame.as_ref().map(|frame| u16::from(frame.code)), CloseStage::Streaming)),
                    Message::Frame(_) => return Err(Failure::fatal("unexpected raw WebSocket frame")),
                };
                if value.get("toolCall").is_some() {
                    return Err(Failure::fatal("Gemini ASR requested an unsupported tool call"));
                }
                if value.get("goAway").is_some() {
                    return Err(Failure::retry("Gemini requested session rotation"));
                }
                if let Some(content) = value.get("serverContent") {
                    let final_text = content.get("inputTranscription")
                        .and_then(|value| value.get("text"))
                        .and_then(Value::as_str).is_some();
                    let decoded = decode_transcription_content(content)?;
                    if final_text && final_deadline.is_none() {
                        return Err(Failure::fatal("Gemini ASR finalized before the explicit audio boundary"));
                    }
                    for mut event in decoded {
                        if let ProviderEvent::Transcript { metadata, .. } = &mut event {
                            // Gemini Live has no word timings. This alignment is
                            // the actual submitted PCM turn, including prior zeros.
                            metadata.start_ms = None;
                            metadata.end_ms = None;
                            metadata.alignment_ms = turn.as_ref().map(|turn| turn.start_sample / 16);
                        }
                        before_final_deadline(final_deadline, emit(events, event)).await?;
                    }
                    if final_text {
                        turn = None;
                        final_deadline = None;
                        if eof { return Ok(()); }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
