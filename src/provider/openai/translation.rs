//! Translation EOF is a protocol operation, not a WebSocket cancellation.
use super::*;

#[cfg(test)]
mod tests;

const FINAL_TIMEOUT: Duration = Duration::from_secs(120);

pub(super) async fn run(
    socket: Socket,
    audio: &mut mpsc::Receiver<Vec<i16>>,
    events: &mpsc::Sender<ProviderEvent>,
    config: &SessionConfig,
) -> SessionResult<()> {
    let mut submitted = false;
    let result = if is_translation(&config.model) {
        continuous(socket, audio, events, config, &mut submitted).await
    } else {
        // Segmentation owns consumed PCM, so errors cannot safely restart an
        // empty socket even before its first complete utterance was submitted.
        submitted = true;
        turns(socket, audio, events, config).await
    };
    result.map_err(|mut failure| {
        if submitted {
            failure.retryable = false;
        }
        failure
    })
}

fn append(samples: &[f32], translation: bool) -> Message {
    let bytes: Vec<u8> = samples
        .iter()
        .flat_map(|sample| ((sample.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes())
        .collect();
    let kind = if translation {
        "session.input_audio_buffer.append"
    } else {
        "input_audio_buffer.append"
    };
    Message::Text(
        json!({"type":kind,"audio":STANDARD.encode(bytes)})
            .to_string()
            .into(),
    )
}

async fn continuous(
    mut socket: Socket,
    audio: &mut mpsc::Receiver<Vec<i16>>,
    events: &mpsc::Sender<ProviderEvent>,
    config: &SessionConfig,
    submitted: &mut bool,
) -> SessionResult<()> {
    let mut resampler = Resampler::new(16000, 24000);
    let mut input_samples = 0usize;
    let mut output_samples = 0usize;
    let mut closing = false;
    let deadline = tokio::time::sleep(FINAL_TIMEOUT);
    tokio::pin!(deadline);
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    heartbeat.tick().await;
    let mut utterances = HashMap::new();
    loop {
        tokio::select! {
            samples = audio.recv(), if !closing => {
                match samples {
                    Some(samples) => {
                        if samples.len() > 16000 { return Err(Failure::fatal("input audio chunk exceeds one second")); }
                        *submitted |= !samples.is_empty();
                        input_samples = input_samples.saturating_add(samples.len());
                        let input: Vec<_> = samples.iter().map(|s| f32::from(*s) / 32768.0).collect();
                        let mut output = Vec::new();
                        resampler.process(&input, &mut output);
                        output_samples += output.len();
                        if !output.is_empty() { io_deadline(socket.send(append(&output, true))).await?; }
                    }
                    None => {
                        // Drain the filter's lookahead without appending extra
                        // source time. Even a sub-frame utterance reaches EOF.
                        let mut tail = Vec::new();
                        resampler.process(&[0.0; 64], &mut tail);
                        tail.truncate((input_samples * 3 / 2).saturating_sub(output_samples));
                        if !tail.is_empty() { io_deadline(socket.send(append(&tail, true))).await?; }
                        io_deadline(socket.send(Message::Text(json!({"type":"session.close"}).to_string().into()))).await?;
                        closing = true;
                        deadline.as_mut().reset(Instant::now() + FINAL_TIMEOUT);
                    }
                }
            }
            value = receive_setup(&mut socket) => {
                let value = value?;
                if value["type"] == "session.closed" && closing {
                    emit(events, ProviderEvent::TurnComplete).await?;
                    return Ok(());
                }
                for event in decode_event(&value, config, &mut utterances)? { emit(events, event).await?; }
            }
            _ = &mut deadline, if closing => return Err(Failure::fatal("OpenAI translation final acknowledgement timed out")),
            _ = heartbeat.tick() => io_deadline(socket.send(Message::Ping(Vec::new().into()))).await?,
        }
    }
}

/// Conversational Realtime has no session.close operation. Use one explicit
/// turn in flight so an earlier response.done cannot acknowledge later audio.
/// The input transcript can arrive after response.done and must also drain.
async fn turns(
    mut socket: Socket,
    audio: &mut mpsc::Receiver<Vec<i16>>,
    events: &mpsc::Sender<ProviderEvent>,
    config: &SessionConfig,
) -> SessionResult<()> {
    let options = super::super::local::history_options(config.vad_silence_ms);
    let (tx, mut rx) = mpsc::channel(2);
    let producer = async {
        super::super::local::segment_translation_borrowed(audio, tx, &options)
            .await
            .map_err(|_| Failure::fatal("OpenAI translation audio segmentation failed"))
    };
    let consumer = async {
        let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
        heartbeat.tick().await;
        loop {
            let segment = tokio::select! {
                segment = rx.recv() => match segment { Some(segment) => segment, None => return Ok(()) },
                value = receive_setup(&mut socket) => {
                    for event in decode_event(&value?, config, &mut HashMap::new())? { emit(events, event).await?; }
                    continue;
                }
                _ = heartbeat.tick() => {
                    io_deadline(socket.send(Message::Ping(Vec::new().into()))).await?;
                    continue;
                }
            };
            timeout(FINAL_TIMEOUT, async {
                let mut input: Vec<_> = segment
                    .samples
                    .iter()
                    .map(|s| f32::from(*s) / 32768.0)
                    .collect();
                // Explicit commits require 100 ms; retain all shorter speech.
                input.resize(input.len().max(1600), 0.0);
                let expected = input.len() * 3 / 2;
                input.extend_from_slice(&[0.0; 64]);
                let mut output = Vec::new();
                Resampler::new(16000, 24000).process(&input, &mut output);
                output.truncate(expected);
                for chunk in output.chunks(24000) {
                    io_deadline(socket.send(append(chunk, false))).await?;
                }
                io_deadline(
                    socket.send(Message::Text(
                        json!({"type":"input_audio_buffer.commit"})
                            .to_string()
                            .into(),
                    )),
                )
                .await?;
                let mut committed = None;
                let mut response_id = None;
                let mut response_complete = false;
                let mut transcription_complete = !config.input_transcription;
                let mut utterances = HashMap::new();
                loop {
                    let value = receive_setup(&mut socket).await?;
                    match value["type"].as_str() {
                        Some("input_audio_buffer.committed") => {
                            let id = value["item_id"]
                                .as_str()
                                .filter(|id| !id.is_empty() && id.len() <= 512)
                                .ok_or_else(|| {
                                    Failure::fatal("OpenAI translation commit omitted its item ID")
                                })?;
                            if committed.is_some() {
                                return Err(Failure::fatal(
                                    "OpenAI unexpectedly committed another translation turn",
                                ));
                            }
                            committed = Some(id.to_owned());
                            utterances.insert(
                                id.to_owned(),
                                TranscriptMetadata {
                                    alignment_ms: Some(segment.start_sample / 16),
                                    ..Default::default()
                                },
                            );
                            io_deadline(socket.send(Message::Text(
                                json!({"type":"response.create"}).to_string().into(),
                            )))
                            .await?;
                        }
                        Some("response.created") => {
                            if committed.is_none() || response_id.is_some() {
                                return Err(Failure::fatal(
                                    "OpenAI created an unexpected translation response",
                                ));
                            }
                            response_id = Some(
                                value["response"]["id"]
                                    .as_str()
                                    .filter(|id| !id.is_empty() && id.len() <= 512)
                                    .ok_or_else(|| {
                                        Failure::fatal("OpenAI translation response omitted its ID")
                                    })?
                                    .to_owned(),
                            );
                        }
                        Some("response.done") => {
                            if response_id.as_deref() != value["response"]["id"].as_str()
                                || response_id.is_none()
                                || value["response"]["status"] != "completed"
                            {
                                return Err(Failure::fatal(
                                    "OpenAI could not complete the pending translation response",
                                ));
                            }
                            response_complete = true;
                        }
                        Some("conversation.item.input_audio_transcription.completed") => {
                            if committed.as_deref() != value["item_id"].as_str()
                                || committed.is_none()
                            {
                                return Err(Failure::fatal(
                                    "OpenAI finalized an unknown translation input",
                                ));
                            }
                            transcription_complete = true;
                        }
                        _ => (),
                    }
                    for event in decode_event(&value, config, &mut utterances)? {
                        if !matches!(event, ProviderEvent::TurnComplete) {
                            emit(events, event).await?;
                        }
                    }
                    if response_complete && transcription_complete {
                        emit(events, ProviderEvent::TurnComplete).await?;
                        return Ok(());
                    }
                }
            })
            .await
            .map_err(|_| {
                Failure::fatal("OpenAI translation turn final acknowledgement timed out")
            })??;
        }
    };
    tokio::try_join!(producer, consumer)?;
    Ok(())
}
