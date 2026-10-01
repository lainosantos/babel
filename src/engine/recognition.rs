//! Session-owned original STT. Device selection controls capture, never the
//! lifetime of already captured speech or an in-flight transcription result.
use super::*;
use crate::audio::PcmFrame;
#[cfg(test)]
use crate::provider::SpeechProvider;
use std::collections::VecDeque;
#[cfg(test)]
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[cfg(test)]
const QUEUED_SAMPLES: usize = INPUT_RATE as usize * 20;
#[cfg(test)]
const QUEUED_FRAMES: usize = 2048;
const MAX_TIMELINE_SPANS: usize = 4096;

#[cfg(test)]
struct Original {
    frame: PcmFrame,
    _budget: OwnedSemaphorePermit,
}

#[cfg(test)]
#[derive(Clone)]
pub(super) struct Sink {
    sender: mpsc::Sender<Original>,
    budget: Arc<Semaphore>,
    connected: Arc<AtomicBool>,
    losses: Arc<AtomicU64>,
}

#[cfg(test)]
impl Sink {
    pub(super) fn available(&self) -> bool {
        !self.sender.is_closed()
    }
    #[cfg(test)]
    pub(super) fn connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// Called only on the processing executor, after original audio forwarding.
    /// Neither provider startup nor network/inference congestion blocks capture.
    pub(super) fn submit(&self, frame: PcmFrame, metrics: &RouteMetrics) -> bool {
        if frame.samples.is_empty() {
            return true;
        }
        let count = frame.samples.len();
        let budget = (count <= INPUT_RATE as usize && frame.sample_rate == INPUT_RATE)
            .then(|| {
                self.budget
                    .clone()
                    .try_acquire_many_owned(count as u32)
                    .ok()
            })
            .flatten();
        let accepted = budget.is_some_and(|budget| {
            self.sender
                .try_send(Original {
                    frame,
                    _budget: budget,
                })
                .is_ok()
        });
        if !accepted {
            self.losses.fetch_add(1, Ordering::Relaxed);
            metrics
                .audio
                .processing_dropped_frames
                .fetch_add(1, Ordering::Relaxed);
            metrics.report_processing_error("Transcription could not retain original audio; its bounded queue is full or the recognizer is unavailable");
        }
        accepted
    }
}

#[cfg(test)]
pub(super) fn start(
    config: AppConfig,
    transcript: TranscriptSink,
    metrics: Arc<RouteMetrics>,
    origin: Instant,
) -> (Sink, impl Future<Output = Result<()>> + Send + 'static) {
    let (sender, mut received) = mpsc::channel(QUEUED_FRAMES);
    let sink = Sink {
        sender,
        budget: Arc::new(Semaphore::new(QUEUED_SAMPLES)),
        connected: Arc::new(AtomicBool::new(false)),
        losses: Arc::new(AtomicU64::new(0)),
    };
    let connected = sink.connected.clone();
    let losses = sink.losses.clone();
    let worker = async move {
        // An enabled but unused route must not open a paid/cloud connection.
        let Some(first) = received.recv().await else {
            return Ok(());
        };
        let recognition = match transcript.origin {
            TranscriptOrigin::Microphone => &config.transcription.microphone_recognition,
            TranscriptOrigin::Speaker => &config.transcription.speaker_recognition,
        };
        let transcript = Some(transcript);
        let result = async {
            let provider = provider::stt::create(recognition, &config.transcription.providers)?;
            let settings =
                provider::stt::session_config(recognition, &config.transcription.providers)?;
            run(
                provider,
                settings,
                received,
                first,
                &transcript,
                &metrics,
                origin,
                &connected,
                &losses,
            )
            .await
        }
        .await;
        connected.store(false, Ordering::Relaxed);
        if result.is_err() {
            let _ = record(&transcript, TranscriptRecord::Gap).await;
        }
        result
    };
    (sink, worker)
}

/// Replay the session-owned originals with backpressure on this processing
/// worker only. Capture and routing never wait for inference to catch up.
pub(super) async fn start_retained(
    config: AppConfig,
    transcript: TranscriptSink,
    metrics: Arc<RouteMetrics>,
    origin: Instant,
    originals: Arc<retained::RetainedSession>,
) -> Result<()> {
    let recognition = match transcript.origin {
        TranscriptOrigin::Microphone => &config.transcription.microphone_recognition,
        TranscriptOrigin::Speaker => &config.transcription.speaker_recognition,
    };
    let selected =
        provider::stt::create(recognition, &config.transcription.providers).and_then(|provider| {
            Ok((
                provider,
                provider::stt::session_config(recognition, &config.transcription.providers)?,
            ))
        });
    let (provider, settings) = match selected {
        Ok(selected) => selected,
        Err(error) => {
            let _ = record(&Some(transcript), TranscriptRecord::Gap).await;
            return Err(error);
        }
    };
    let result = run_retained(
        provider,
        settings,
        transcript.clone(),
        metrics.clone(),
        origin,
        originals,
    )
    .await;
    if result.is_err() {
        metrics.recovery("transcription", false);
        let _ = record(&Some(transcript), TranscriptRecord::Gap).await;
    }
    result
}

async fn run_retained(
    provider: Arc<dyn provider::SpeechProvider>,
    mut settings: SessionConfig,
    transcript: TranscriptSink,
    metrics: Arc<RouteMetrics>,
    origin: Instant,
    originals: Arc<retained::RetainedSession>,
) -> Result<()> {
    let retries = settings.max_reconnect_attempts;
    // The engine owns the exact window and its retry budget. Do not multiply
    // outer attempts by a second provider retry loop.
    settings.max_reconnect_attempts = 0;
    // Finite original windows use the configured recognizer's protocol for
    // both normal processing and replay. Recovery never substitutes a model.
    let mut reader = originals.reader(transcript.origin);
    loop {
        let Some(first) = reader.next().await? else {
            return Ok(());
        };
        let mut samples = first.samples.clone();
        let mut timeline = Timeline::default();
        timeline.push(
            &PcmFrame {
                samples: first.samples,
                captured_at: first.captured_at,
                sample_rate: INPUT_RATE,
            },
            origin,
        );
        while samples.len() < INPUT_RATE as usize * 5 {
            match tokio::time::timeout(Duration::from_millis(400), reader.next()).await {
                Ok(Ok(Some(frame))) => {
                    timeline.push(
                        &PcmFrame {
                            samples: frame.samples.clone(),
                            captured_at: frame.captured_at,
                            sample_rate: INPUT_RATE,
                        },
                        origin,
                    );
                    samples.extend_from_slice(&frame.samples);
                }
                Ok(Ok(None)) | Err(_) => break,
                Ok(Err(error)) => return Err(error),
            }
        }
        if samples.iter().all(|sample| *sample == 0) {
            continue;
        }
        let mut attempts = 0u32;
        let results = loop {
            match resilience::segment(provider.clone(), settings.clone(), &samples, true).await {
                Ok(results) => break results,
                Err(error) => {
                    attempts = attempts.saturating_add(1);
                    metrics.recovery_error("transcription", Some(&format!("{error:#}")));
                    resilience::retry_live(
                        &error,
                        attempts,
                        retries,
                        originals.status().capture_closed,
                    )?;
                    metrics.recovery("transcription", true);
                    metrics.reconnects.fetch_add(1, Ordering::Relaxed);
                    // Keep this exact segment and its timeline. No speculative
                    // text or gap is persisted before successful completion.
                    resilience::backoff(&originals, attempts, retries).await;
                    resilience::retry_live(
                        &error,
                        attempts,
                        retries,
                        originals.status().capture_closed,
                    )?;
                }
            }
        };
        for event in results {
            persist(event, &Some(transcript.clone()), &metrics, &timeline).await?;
        }
        metrics.recovery_error("transcription", None);
        metrics.recovery("transcription", false);
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
async fn run(
    provider: Arc<dyn SpeechProvider>,
    settings: SessionConfig,
    mut received: mpsc::Receiver<Original>,
    first: Original,
    transcript: &Option<TranscriptSink>,
    metrics: &RouteMetrics,
    origin: Instant,
    connected: &AtomicBool,
    losses: &AtomicU64,
) -> Result<()> {
    let (audio, input) = mpsc::channel(8);
    let (events, mut output) = mpsc::channel(32);
    let cancel = CancellationToken::new();
    let _cancel_on_drop = cancel.clone().drop_guard();
    let mut task = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        provider.run(settings, input, events, cancel).await
    }));
    let mut audio = Some(audio);
    let mut pending = VecDeque::from([first]);
    let mut source_open = true;
    let mut events_open = true;
    let mut ready = false;
    let mut seen_losses = 0;
    let mut timeline = Timeline::default();
    loop {
        let dropped = losses.load(Ordering::Relaxed);
        if dropped != seen_losses {
            record(transcript, TranscriptRecord::Gap).await?;
            seen_losses = dropped;
        }
        if !source_open && pending.is_empty() && ready {
            // STT EOF flushes every accepted original and the final utterance.
            // Provider request timeouts still report failures, but total drain
            // time is not bounded by Stop or device/playback ownership.
            audio.take();
        }
        tokio::select! {
            biased;
            result = &mut task => {
                let result = result.map_err(|_| anyhow!("Recognition component failed internally"))?;
                // The event sender can close just before task completion.
                while let Ok(event) = output.try_recv() {
                    persist(event, transcript, metrics, &timeline).await?;
                }
                result?;
                ensure!(!source_open && pending.is_empty(), "Recognizer stopped before consuming the original audio");
                return Ok(());
            }
            event = output.recv(), if events_open => {
                match event {
                    Some(event) => {
                        match &event {
                            ProviderEvent::Connected => { ready = true; connected.store(true, Ordering::Relaxed); }
                            ProviderEvent::Reconnecting { .. } => {
                                ready = false;
                                connected.store(false, Ordering::Relaxed);
                                timeline = Timeline::default();
                                metrics.reconnects.fetch_add(1, Ordering::Relaxed);
                            }
                            ProviderEvent::RecoveringOriginal { .. } => {
                                ready = false;
                                connected.store(false, Ordering::Relaxed);
                                metrics.reconnects.fetch_add(1, Ordering::Relaxed);
                            }
                            _ => {}
                        }
                        persist(event, transcript, metrics, &timeline).await?;
                    }
                    None => events_open = false,
                }
            }
            permit = async { audio.as_ref().unwrap().reserve().await }, if ready && audio.is_some() && !pending.is_empty() => {
                let permit = permit.context("Recognizer closed its original-audio queue")?;
                let original = pending.pop_front().unwrap();
                timeline.push(&original.frame, origin);
                permit.send(original.frame.samples);
            }
            original = received.recv(), if source_open && pending.len() < QUEUED_FRAMES => {
                match original {
                    Some(original) => pending.push_back(original),
                    None => source_open = false,
                }
            }
        }
    }
}

async fn persist(
    mut event: ProviderEvent,
    transcript: &Option<TranscriptSink>,
    metrics: &RouteMetrics,
    timeline: &Timeline,
) -> Result<()> {
    if let ProviderEvent::Transcript { metadata, .. } = &mut event {
        metadata.start_ms = metadata.start_ms.and_then(|ms| timeline.map(ms, false));
        metadata.end_ms = metadata.end_ms.and_then(|ms| timeline.map(ms, true));
        metadata.alignment_ms = metadata.alignment_ms.and_then(|ms| timeline.map(ms, false));
    }
    let record = match event {
        ProviderEvent::Transcript {
            input: true,
            text,
            metadata,
        } => {
            metrics
                .view
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .last_input_transcript = Some(text.chars().take(2048).collect());
            TranscriptRecord::Text {
                input: true,
                text,
                metadata,
                received_at: chrono::Utc::now().to_rfc3339(),
            }
        }
        ProviderEvent::TurnComplete => TranscriptRecord::TurnComplete,
        ProviderEvent::Warning { message } => {
            metrics.report_processing_error(&message);
            TranscriptRecord::Gap
        }
        ProviderEvent::Reconnecting { .. } | ProviderEvent::Interrupted => TranscriptRecord::Gap,
        // A recognizer never saves generated speech or translated text.
        _ => return Ok(()),
    };
    self::record(transcript, record).await
}

async fn record(transcript: &Option<TranscriptSink>, record: TranscriptRecord) -> Result<()> {
    if let Some(sink) = transcript {
        if matches!(record, TranscriptRecord::Gap)
            && let Some(retained) = &sink.retained
        {
            retained.mark_incomplete(
                "Original transcription requires recovery after an interrupted result",
            );
        }
        // This is a processing worker: a slow TXT writer may backpressure
        // recognition without blocking capture or dropping accepted finals.
        sink.sender
            .send(TranscriptRecord::Routed {
                origin: sink.origin,
                record: Box::new(record),
            })
            .await
            .map_err(|_| {
                if let Some(retained) = &sink.retained {
                    retained.mark_incomplete(
                        "Transcript delivery failed; original audio is retained for recovery",
                    );
                }
                anyhow!("Transcript writer closed before accepted original results were saved")
            })?;
    }
    Ok(())
}

#[derive(Default)]
struct Timeline {
    samples: u64,
    spans: VecDeque<Span>,
}
struct Span {
    start: u64,
    end: u64,
    session_ms: u64,
}
impl Timeline {
    fn push(&mut self, frame: &PcmFrame, origin: Instant) {
        let duration = Duration::from_secs_f64(frame.samples.len() as f64 / f64::from(INPUT_RATE));
        let start = frame
            .captured_at
            .checked_sub(duration)
            .unwrap_or(frame.captured_at);
        let session_ms = start
            .saturating_duration_since(origin)
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        let end = self.samples.saturating_add(frame.samples.len() as u64);
        if let Some(last) = self.spans.back_mut()
            && (last.session_ms + (self.samples - last.start) * 1000 / u64::from(INPUT_RATE))
                .abs_diff(session_ms)
                <= 50
        {
            last.end = end;
        } else {
            if self.spans.len() == MAX_TIMELINE_SPANS {
                self.spans.pop_front();
            }
            self.spans.push_back(Span {
                start: self.samples,
                end,
                session_ms,
            });
        }
        self.samples = end;
    }
    fn map(&self, ms: u64, end_boundary: bool) -> Option<u64> {
        let sample = ms.checked_mul(u64::from(INPUT_RATE))? / 1000;
        let span = if end_boundary {
            self.spans
                .iter()
                .find(|span| sample > span.start && sample <= span.end)
        } else {
            self.spans
                .iter()
                .rev()
                .find(|span| sample >= span.start && sample < span.end)
        }
        .or_else(|| self.spans.back().filter(|span| sample == span.end))?;
        Some(
            span.session_ms
                .saturating_add((sample - span.start) * 1000 / u64::from(INPUT_RATE)),
        )
    }
}

#[cfg(test)]
mod tests;
