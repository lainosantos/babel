//! Session-owned original STT. Device selection controls capture, never the
//! lifetime of already captured speech or an in-flight transcription result.
use super::*;
use crate::{audio::PcmFrame, provider::SpeechProvider};
use std::collections::VecDeque;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const QUEUED_SAMPLES: usize = INPUT_RATE as usize * 20;
const QUEUED_FRAMES: usize = 2048;
const MAX_TIMELINE_SPANS: usize = 4096;
// Leave time for the TXT writer to persist the incomplete marker before the
// supervisor's 15-second file/processing deadline.
const FINALIZE_TIMEOUT: Duration = Duration::from_secs(12);

struct Original {
    frame: PcmFrame,
    _budget: OwnedSemaphorePermit,
}

#[derive(Clone)]
pub(super) struct Sink {
    sender: mpsc::Sender<Original>,
    budget: Arc<Semaphore>,
    connected: Arc<AtomicBool>,
    losses: Arc<AtomicU64>,
}

impl Sink {
    pub(super) fn available(&self) -> bool {
        !self.sender.is_closed()
    }
    pub(super) fn connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// Called only on the processing executor, after original audio forwarding.
    /// Neither provider startup nor network/inference congestion blocks capture.
    pub(super) fn submit(&self, frame: PcmFrame, metrics: &RouteMetrics) {
        if frame.samples.is_empty() {
            return;
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
    }
}

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
        let provider = provider::stt::create(recognition, &config.transcription.providers)?;
        let settings = provider::stt::session_config(recognition, &config.transcription.providers)?;
        let transcript = Some(transcript);
        let result = run(
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
        .await;
        connected.store(false, Ordering::Relaxed);
        if result.is_err() {
            let _ = record(&transcript, TranscriptRecord::Gap);
        }
        result
    };
    (sink, worker)
}

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
    let mut finish_deadline = None;
    loop {
        if received.is_closed() && finish_deadline.is_none() {
            finish_deadline = Some(tokio::time::Instant::now() + FINALIZE_TIMEOUT);
        }
        let dropped = losses.load(Ordering::Relaxed);
        if dropped != seen_losses {
            record(transcript, TranscriptRecord::Gap)?;
            seen_losses = dropped;
        }
        if !source_open && pending.is_empty() && ready {
            // STT EOF explicitly flushes the final captured utterance. It does
            // not cancel network work or retain any device/playback ownership.
            audio.take();
        }
        tokio::select! {
            biased;
            _ = async { tokio::time::sleep_until(finish_deadline.unwrap()).await }, if finish_deadline.is_some() => {
                bail!("Timed out finalizing original transcription; the transcript may be incomplete");
            }
            result = &mut task => {
                let result = result.map_err(|_| anyhow!("Recognition component failed internally"))?;
                // The event sender can close just before task completion.
                while let Ok(event) = output.try_recv() {
                    persist(event, transcript, metrics, &timeline)?;
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
                            _ => {}
                        }
                        persist(event, transcript, metrics, &timeline)?;
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
            // A full startup queue disables recv(), so sender closure alone
            // cannot wake it. Observe Stop even if setup never acknowledges.
            _ = tokio::time::sleep(Duration::from_millis(250)), if pending.len() >= QUEUED_FRAMES && finish_deadline.is_none() => {}
        }
    }
}

fn persist(
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
    record_recognition_event_at(event, transcript, metrics, 0)
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
