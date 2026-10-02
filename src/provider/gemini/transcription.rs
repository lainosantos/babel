//! Persistent original-audio recognition with explicit, acknowledged turns.
//! Manual VAD limits recognition to one unacknowledged turn. Its final
//! inputTranscription commits words. A matching server activity end can close
//! a wordless turn without an empty transcript object.
//! https://ai.google.dev/gemini-api/docs/live-api/live-transcribe#manual-vad-push-to-talk
use super::*;

const FINAL_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_TURN_SAMPLES: usize = 16_000 * 5;
const WINDOW_FINAL_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_WINDOW_SAMPLES: u64 = 16_000 * 31;
const STREAM_CHUNK_SAMPLES: usize = 1600;

#[derive(Clone, Copy, PartialEq)]
enum Boundary {
    Streaming,
    Window,
}
impl Boundary {
    fn final_timeout(self) -> Duration {
        match self {
            Self::Streaming => FINAL_TIMEOUT,
            Self::Window => WINDOW_FINAL_TIMEOUT,
        }
    }
}

pub(super) fn final_timeout() -> Failure {
    // A stalled live recognizer must not disable this source for the rest of
    // the session. The supervisor bounds retries and records the discontinuity;
    // when the source has closed, it still returns an incomplete-transcript error.
    let mut failure = Failure::retry(
        "Gemini transcription final acknowledgement timed out; transcript may be incomplete",
    );
    failure.needs_transcription_context = true;
    failure
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
    interim_text: bool,
}

/// The wire duration is relative to submitted PCM, excluding locally skipped
/// leading zeros. Reject malformed, rounded, old, or future activity boundaries.
fn activity_end_matches(value: &Value, submitted_samples: u64) -> bool {
    let activity = &value["voiceActivity"];
    if activity["type"] != "ACTIVITY_END" {
        return false;
    }
    let Some(offset) = activity["audioOffset"]
        .as_str()
        .and_then(|text| text.strip_suffix('s'))
    else {
        return false;
    };
    if offset.len() > 32 {
        return false;
    }
    let (seconds, fraction) = offset.split_once('.').unwrap_or((offset, ""));
    if seconds.is_empty()
        || !seconds.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.len() > 9
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return false;
    }
    let nanos = seconds
        .parse::<u64>()
        .ok()
        .and_then(|seconds| seconds.checked_mul(1_000_000_000))
        .and_then(|seconds| {
            let fractional = if fraction.is_empty() {
                0
            } else {
                fraction.parse::<u64>().ok()? * 10u64.pow(9 - fraction.len() as u32)
            };
            seconds.checked_add(fractional)
        });
    nanos.is_some_and(|nanos| nanos % 62_500 == 0 && nanos / 62_500 == submitted_samples)
}

async fn end_turn(socket: &mut Socket, boundary: Boundary) -> SessionResult<Instant> {
    io_deadline(
        socket.send(Message::Text(
            json!({"realtimeInput":{"activityEnd":{}}})
                .to_string()
                .into(),
        )),
    )
    .await?;
    Ok(Instant::now() + boundary.final_timeout())
}

#[derive(Default)]
struct State {
    turn: Option<Turn>,
    position: u64,
    submitted_samples: u64,
    eof: bool,
}
impl Turn {
    fn new(start_sample: u64) -> Self {
        Self {
            start_sample,
            samples: 0,
            trailing_zeros: 0,
            last_input: Instant::now(),
            interim_text: false,
        }
    }
}

/// Run only the configured Live Transcribe model. The engine owns retained
/// originals and retries the unconfirmed window through this same protocol.
pub(super) async fn session(
    config: &SessionConfig,
    api_key: &str,
    endpoint: &str,
    mut audio: mpsc::Receiver<Vec<i16>>,
    events: mpsc::Sender<ProviderEvent>,
    cancel: CancellationToken,
) -> Result<()> {
    let work = async {
        let socket = open_socket(config, api_key, endpoint, None, true).await?;
        emit(&events, ProviderEvent::Connected).await?;
        // The caller already bounded this retained window. Keep its overflow
        // capture chunk and quiet tail in the same explicit activity; splitting
        // again at five seconds can strand a tiny, unacknowledged second turn.
        run_live(
            socket,
            &mut audio,
            &events,
            config.vad_silence_ms,
            &mut State::default(),
            Boundary::Window,
        )
        .await
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => bail!("Original transcription was cancelled before completion"),
        result = work => result.map_err(anyhow::Error::new),
    }
}

// Single-connection entry point used by historical/protocol workflows.
pub(super) async fn run(
    socket: Socket,
    audio: &mut mpsc::Receiver<Vec<i16>>,
    events: &mpsc::Sender<ProviderEvent>,
    silence_ms: u32,
) -> SessionResult<()> {
    run_live(
        socket,
        audio,
        events,
        silence_ms,
        &mut State::default(),
        Boundary::Streaming,
    )
    .await
}

async fn run_live(
    mut socket: Socket,
    audio: &mut mpsc::Receiver<Vec<i16>>,
    events: &mpsc::Sender<ProviderEvent>,
    silence_ms: u32,
    state: &mut State,
    boundary: Boundary,
) -> SessionResult<()> {
    let silence = Duration::from_millis(u64::from(silence_ms.clamp(100, 2000)));
    let silence_samples = silence.as_millis() as usize * 16;
    let mut final_deadline = None;
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
            _ = idle.tick(), if boundary == Boundary::Streaming && state.turn.is_some() && final_deadline.is_none() => {
                if state.turn.as_ref().is_some_and(|turn| turn.last_input.elapsed() >= silence) {
                    final_deadline = Some(end_turn(&mut socket, boundary).await?);
                }
            }
            samples = audio.recv(), if !state.eof && final_deadline.is_none() => {
                let Some(samples) = samples else {
                    state.eof = true;
                    if state.turn.is_none() { return Ok(()); }
                    final_deadline = Some(end_turn(&mut socket, boundary).await?);
                    continue;
                };
                if samples.len() > MAX_INPUT_SAMPLES {
                    return Err(Failure::fatal("input audio chunk exceeds one second"));
                }
                if samples.is_empty() { continue; }
                let start = state.position;
                state.position = state.position.saturating_add(samples.len() as u64);
                if boundary == Boundary::Window && state.position > MAX_WINDOW_SAMPLES {
                    return Err(Failure::fatal("Gemini retained transcription window exceeds 31 seconds"));
                }
                // Never classify quiet speech as silence. Only exact digital
                // zero is known empty, and its duration still advances offsets.
                if state.turn.is_none() && samples.iter().all(|sample| *sample == 0) {
                    continue;
                }
                let starting = state.turn.is_none();
                let current = state.turn.get_or_insert_with(|| Turn::new(start));
                // Original PCM remains in the engine's encrypted journal.
                current.samples = current.samples.saturating_add(samples.len());
                let zeros = samples.iter().rev().take_while(|sample| **sample == 0).count();
                current.trailing_zeros = if zeros == samples.len() { current.trailing_zeros + zeros } else { zeros };
                current.last_input = Instant::now();
                let ending = boundary == Boundary::Streaming && (current.samples >= MAX_TURN_SAMPLES || current.trailing_zeros >= silence_samples);
                if starting {
                    io_deadline(socket.send(Message::Text(
                        json!({"realtimeInput":{"activityStart":{}}}).to_string().into()
                    ))).await?;
                }
                // The official Live Transcribe guidance recommends 100 ms
                // PCM messages, including replay of retained originals.
                for chunk in samples.chunks(STREAM_CHUNK_SAMPLES) {
                    io_deadline(socket.send(audio_message(chunk)?)).await?;
                    state.submitted_samples = state.submitted_samples.saturating_add(chunk.len() as u64);
                }
                if ending {
                    final_deadline = Some(end_turn(&mut socket, boundary).await?);
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
                    if content.get("interrupted").and_then(Value::as_bool) == Some(true) {
                        // The engine retains this window and retries it without
                        // changing the configured provider or model.
                        return Err(Failure::retry("Gemini interrupted original transcription"));
                    }
                    let final_text = transcription_final(content)?;
                    if content["interimInputTranscription"]["text"].as_str().is_some_and(|text| !text.trim().is_empty())
                        && let Some(turn) = state.turn.as_mut() {
                        turn.interim_text = true;
                        // Once any words were hypothesized, only an authoritative
                        // final can commit this turn, never an activity marker.
                    }
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
                            metadata.alignment_ms = state.turn.as_ref().map(|turn| turn.start_sample / 16);
                        }
                        let committed = matches!(event, ProviderEvent::Transcript { .. }) && final_text;
                        if state.turn.is_none() && final_text {
                            // Text already reached the consumer. Never replay it because
                            // a later newline/control event could not be delivered.
                            emit(events, event).await?;
                        } else {
                            before_final_deadline(final_deadline, emit(events, event)).await?;
                        }
                        if committed { state.turn = None; }
                    }
                    if final_text {
                        state.turn = None;
                        final_deadline = None;
                        if state.eof { return Ok(()); }
                    }
                }
                if final_deadline.is_some()
                    && state.turn.as_ref().is_some_and(|turn| !turn.interim_text)
                    && activity_end_matches(&value, state.submitted_samples)
                {
                    // Live Transcribe can emit only a timestamped activity end
                    // for wordless audio. This observed server boundary, rather
                    // than silence, VAD, or timeout alone, permits an empty result.
                    before_final_deadline(final_deadline, emit(events, ProviderEvent::TurnComplete)).await?;
                    state.turn = None;
                    final_deadline = None;
                    if state.eof { return Ok(()); }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod session_tests;
