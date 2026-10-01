//! Drain translated speech through explicit input and output boundaries.
use super::*;

#[cfg(test)]
mod tests;

const FINAL_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) async fn run(
    socket: Socket,
    audio: &mut mpsc::Receiver<Vec<i16>>,
    events: &mpsc::Sender<ProviderEvent>,
    config: &SessionConfig,
    resume_handle: &mut Option<String>,
) -> SessionResult<()> {
    let mut submitted = false;
    let result = continuous(
        socket,
        audio,
        events,
        resume_handle,
        &mut submitted,
        is_translation_model(config),
    )
    .await;
    result.map_err(|mut failure| {
        // A fresh connection cannot replay PCM already consumed from the
        // retained stream cursor. Surface failure so its original stays saved.
        if submitted {
            failure.retryable = false;
        }
        failure
    })
}

async fn receive(socket: &mut Socket) -> SessionResult<Value> {
    loop {
        match socket
            .next()
            .await
            .ok_or_else(|| Failure::retry("Gemini WebSocket closed before translation completed"))?
            .map_err(socket_error)?
        {
            Message::Text(text) => return parse_json(text.as_bytes()),
            Message::Binary(bytes) => return parse_json(&bytes),
            Message::Ping(data) => io_deadline(socket.send(Message::Pong(data))).await?,
            Message::Pong(_) => (),
            Message::Close(frame) => {
                return Err(close_failure(
                    frame.as_ref().map(|f| u16::from(f.code)),
                    CloseStage::Streaming,
                ));
            }
            Message::Frame(_) => return Err(Failure::fatal("unexpected raw Gemini frame")),
        }
    }
}

async fn dispatch(
    value: &Value,
    events: &mpsc::Sender<ProviderEvent>,
    resume_handle: &mut Option<String>,
) -> SessionResult<()> {
    if let Some(update) = value.get("sessionResumptionUpdate") {
        if update["resumable"] == true {
            if let Some(handle) = update["newHandle"].as_str() {
                if handle.len() > 16_384 {
                    return Err(Failure::fatal(
                        "Gemini resume handle exceeds the memory limit",
                    ));
                }
                if !handle.is_empty() {
                    *resume_handle = Some(handle.to_owned());
                }
            }
        } else {
            *resume_handle = None;
        }
    }
    if let Some(content) = value.get("serverContent") {
        if content["interrupted"] == true {
            return Err(Failure::fatal(
                "Gemini interrupted pending translated speech",
            ));
        }
        for event in decode_content(content)? {
            emit(events, event).await?;
        }
    }
    if value.get("goAway").is_some() {
        return Err(Failure::retry("Gemini requested session rotation"));
    }
    if value.get("toolCall").is_some() {
        return Err(Failure::fatal("Gemini requested an unsupported tool call"));
    }
    Ok(())
}

/// Live Translate is continuous. End its audio stream, keep receiving PCM and
/// text, and require generationComplete for the dedicated interpreter, or the
/// conversational turnComplete for a generic Live agent. Once an
/// inactivity boundary is sent, do not submit a new stream epoch until that
/// acknowledgement arrives; otherwise an older final could consume newer EOF.
async fn continuous(
    mut socket: Socket,
    audio: &mut mpsc::Receiver<Vec<i16>>,
    events: &mpsc::Sender<ProviderEvent>,
    resume_handle: &mut Option<String>,
    submitted: &mut bool,
    translation_pipeline: bool,
) -> SessionResult<()> {
    let mut eof = false;
    let mut pending = false;
    let mut response_pending = false;
    let mut input_revision = 0u64;
    let mut generation_revision = None;
    let mut flushing = false;
    let mut last_audio = Instant::now();
    let mut activity = crate::audio::speech::VoiceActivity::default();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    heartbeat.tick().await;
    let mut inactivity = tokio::time::interval(Duration::from_secs(1));
    inactivity.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let deadline = tokio::time::sleep(FINAL_TIMEOUT);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            samples = audio.recv(), if !eof && !flushing => {
                match samples {
                    Some(samples) => {
                        if samples.is_empty() { continue; }
                        *submitted = true;
                        io_deadline(socket.send(audio_message(&samples)?)).await?;
                        // Forward unchanged PCM, but residual hardware noise
                        // must not create a new generation revision after a
                        // completed speech turn. Audible new input still waits
                        // for its own terminal acknowledgement.
                        if activity.speech(&samples) {
                            input_revision = input_revision.saturating_add(1);
                            pending = true;
                        }
                        last_audio = Instant::now();
                    }
                    None => {
                        eof = true;
                        if translation_pipeline && activity.no_detected_speech() && !response_pending {
                            pending = false;
                        }
                        if !pending { return Ok(()); }
                        end_stream(&mut socket).await?;
                        flushing = true;
                        deadline.as_mut().reset(Instant::now() + FINAL_TIMEOUT);
                    }
                }
            }
            value = receive(&mut socket) => {
                let value = value?;
                dispatch(&value, events, resume_handle).await?;
                let content = &value["serverContent"];
                if content.get("modelTurn").is_some() || content.get("inputTranscription").is_some() || content.get("outputTranscription").is_some() {
                    response_pending = true;
                }
                // Use one completion boundary per model protocol. Consuming
                // generationComplete and its later turnComplete as separate
                // finals would acknowledge the next input epoch.
                if content["generationComplete"] == true {
                    if translation_pipeline {
                        // The dedicated interpreter completes generation without
                        // requiring a conversational turn/playback acknowledgement.
                        pending = false;
                        response_pending = false;
                        flushing = false;
                        if content["turnComplete"] != true {
                            emit(events, ProviderEvent::TurnComplete).await?;
                        }
                        if eof { return Ok(()); }
                    } else {
                        generation_revision = Some(input_revision);
                    }
                }
                if !translation_pipeline && content["turnComplete"] == true {
                    // generationComplete can precede turnComplete by playback
                    // time. Audio submitted between those markers belongs to
                    // later work and must not be acknowledged by the old turn.
                    let completed = generation_revision.take().unwrap_or(input_revision);
                    if completed == input_revision {
                        pending = false;
                        flushing = false;
                        if eof { return Ok(()); }
                    }
                } else if content["generationComplete"] != true && (content.get("modelTurn").is_some() || content.get("outputTranscription").is_some()) {
                    pending = true;
                }
            }
            _ = inactivity.tick(), if !flushing && pending => {
                if last_audio.elapsed() >= Duration::from_secs(1) {
                    end_stream(&mut socket).await?;
                    flushing = true;
                    deadline.as_mut().reset(Instant::now() + FINAL_TIMEOUT);
                }
            }
            _ = &mut deadline, if flushing => return Err(Failure::fatal("Gemini translation final acknowledgement timed out")),
            _ = heartbeat.tick() => io_deadline(socket.send(Message::Ping(Vec::new().into()))).await?,
        }
    }
}

async fn end_stream(socket: &mut Socket) -> SessionResult<()> {
    io_deadline(
        socket.send(Message::Text(
            json!({"realtimeInput":{"audioStreamEnd":true}})
                .to_string()
                .into(),
        )),
    )
    .await
}
