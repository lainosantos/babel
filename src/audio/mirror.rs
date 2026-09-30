//! Playback-only sharing of speaker PCM before or after translation. The speaker route remains the
//! only capture owner; this transport has no speech, recording or command tap.
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, anyhow, ensure};
use tokio::{
    sync::{broadcast, mpsc, watch},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

use super::{AudioOptions, AudioStats, OriginalFrame, PlaybackCommand};

const QUEUE_FRAMES: usize = 8;
const MAX_AGE: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug)]
pub(super) struct Format {
    pub(super) epoch: u64,
    pub(super) options: AudioOptions,
}

#[derive(Clone, Debug)]
pub(super) struct Frame {
    pub(super) epoch: u64,
    pub(super) original: OriginalFrame,
}

#[derive(Debug)]
pub(crate) struct Source {
    next_epoch: AtomicU64,
    pub(super) format: watch::Sender<Option<Format>>,
    pub(super) frames: broadcast::Sender<Frame>,
}

impl Source {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            next_epoch: AtomicU64::new(0),
            format: watch::channel(None).0,
            frames: broadcast::channel(QUEUE_FRAMES).0,
        })
    }

    pub(crate) fn begin(self: &Arc<Self>, options: AudioOptions) -> Publisher {
        let epoch = self
            .next_epoch
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        self.format.send_replace(Some(Format { epoch, options }));
        Publisher {
            source: self.clone(),
            epoch,
        }
    }
}

pub(crate) struct Publisher {
    source: Arc<Source>,
    epoch: u64,
}

impl Publisher {
    pub(crate) fn has_subscribers(&self) -> bool {
        self.source.frames.receiver_count() > 0
    }

    pub(crate) fn prepare_playback(
        &self,
        command: &PlaybackCommand,
        options: AudioOptions,
    ) -> Option<OriginalFrame> {
        if !self.has_subscribers() {
            return None;
        }
        let samples = match command {
            PlaybackCommand::Original { samples, .. } => samples.clone(),
            PlaybackCommand::Audio { samples, .. } => samples
                .iter()
                .map(|sample| f32::from(*sample) / 32768.0)
                .collect::<Vec<_>>()
                .into(),
            PlaybackCommand::Flush => return None,
        };
        Some(OriginalFrame {
            samples,
            sample_rate: options.sample_rate,
            channels: options.channels,
            captured_at: std::time::Instant::now(),
        })
    }

    pub(crate) fn publish(&self, original: &OriginalFrame) {
        // Arc-backed samples are shared at full precision, never copied or
        // resampled. A slow mirror cannot delay the primary speaker route.
        if self.has_subscribers() {
            let _ = self.source.frames.send(Frame {
                epoch: self.epoch,
                original: original.clone(),
            });
        }
    }
}

impl Drop for Publisher {
    fn drop(&mut self) {
        self.source.format.send_if_modified(|format| {
            if format.is_some_and(|format| format.epoch == self.epoch) {
                *format = None;
                true
            } else {
                false
            }
        });
    }
}

/// Opens only the virtual microphone's playback endpoint. Format changes and
/// source loss close that endpoint before a new stream can open it again.
pub(crate) async fn run(
    source: Arc<Source>,
    mut playback: watch::Receiver<String>,
    latency_ms: u32,
    cancel: CancellationToken,
    stats: Arc<AudioStats>,
) -> Result<()> {
    let runtime = crate::execution::audio_handle()?;
    let mut formats = source.format.subscribe();
    loop {
        let current = *formats.borrow_and_update();
        let device = playback.borrow_and_update().clone();
        let Some(format) = current.filter(|_| !device.is_empty()) else {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Ok(()),
                changed = formats.changed() => changed.context("Original speaker source closed")?,
                changed = playback.changed() => changed.context("Virtual microphone selection closed")?,
            }
            continue;
        };
        let options = AudioOptions {
            latency_ms,
            queue_ms: 80.max(format.options.frame_ms),
            ..format.options
        }
        .validate()?;
        let queue_frames = (options.queue_ms / options.frame_ms).clamp(1, 20) as usize;
        let (tx, rx) = mpsc::channel(queue_frames);
        // Subscribe after selecting the format: old frames are never replayed
        // when a microphone consumer starts or a source reconnects.
        let frames = source.frames.subscribe();
        let active = cancel.child_token();
        let _guard = active.clone().drop_guard();
        let mut jobs = JoinSet::new();
        let playback_cancel = active.clone();
        let playback_stats = stats.clone();
        let playback_selection = playback.clone();
        jobs.spawn_on(
            async move {
                super::switching::playback(
                    &device,
                    options,
                    rx,
                    playback_selection,
                    playback_cancel,
                    playback_stats,
                )
                .await
            },
            &runtime,
        );
        let forward_cancel = active.clone();
        let forward_stats = stats.clone();
        jobs.spawn_on(
            async move { forward(frames, tx, format, forward_cancel, forward_stats).await },
            &runtime,
        );
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => Ok(()),
            changed = formats.changed() => changed.context("Original speaker source closed"),
            completed = jobs.join_next() => match completed {
                Some(Ok(result)) => result.and_then(|()| if cancel.is_cancelled() { Ok(()) } else { Err(anyhow!("Original microphone mirror ended unexpectedly")) }),
                Some(Err(error)) => Err(error.into()),
                None => Err(anyhow!("Original microphone mirror has no workers")),
            },
        };
        stats.playback_generation.fetch_add(1, Ordering::AcqRel);
        active.cancel();
        let stopped = tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(result) = jobs.join_next().await {
                result.context("Original microphone mirror worker interrupted")??;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await;
        stats.passthrough_level.store(0, Ordering::Relaxed);
        match stopped {
            Ok(stopped) => stopped?,
            Err(_) => {
                jobs.abort_all();
                return Err(anyhow!(
                    "Timed out while stopping original microphone mirror"
                ));
            }
        }
        result?;
        if cancel.is_cancelled() {
            return Ok(());
        }
    }
}

async fn forward(
    mut frames: broadcast::Receiver<Frame>,
    playback: mpsc::Sender<PlaybackCommand>,
    format: Format,
    cancel: CancellationToken,
    stats: Arc<AudioStats>,
) -> Result<()> {
    loop {
        let received = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(()),
            frame = frames.recv() => frame,
        };
        let frame = match received {
            Ok(frame) => frame,
            Err(broadcast::error::RecvError::Lagged(count)) => {
                stats.dropped_frames.fetch_add(count, Ordering::Relaxed);
                continue;
            }
            Err(broadcast::error::RecvError::Closed) if cancel.is_cancelled() => return Ok(()),
            Err(broadcast::error::RecvError::Closed) => {
                return Err(anyhow!("Original speaker source closed"));
            }
        };
        let original = frame.original;
        if frame.epoch != format.epoch || original.captured_at.elapsed() > MAX_AGE {
            stats.dropped_frames.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        ensure!(
            original.sample_rate == format.options.sample_rate
                && original.channels == format.options.channels
                && !original.samples.is_empty()
                && original.samples.len() % usize::from(original.channels) == 0
                && original.samples.len() <= format.options.frame_samples(),
            "Unexpected PCM format in original microphone mirror"
        );
        let level = (original
            .samples
            .iter()
            .map(|value| f64::from(*value).powi(2))
            .sum::<f64>()
            / original.samples.len() as f64)
            .sqrt() as f32;
        stats
            .passthrough_level
            .store(level.to_bits(), Ordering::Relaxed);
        stats.captured_frames.fetch_add(1, Ordering::Relaxed);
        let generation = stats.playback_generation.load(Ordering::Acquire);
        match playback.try_send(PlaybackCommand::Original {
            samples: original.samples,
            generation,
        }) {
            Ok(()) => (),
            Err(mpsc::error::TrySendError::Full(_)) => {
                stats.dropped_frames.fetch_add(1, Ordering::Relaxed);
            }
            Err(mpsc::error::TrySendError::Closed(_)) if cancel.is_cancelled() => return Ok(()),
            Err(mpsc::error::TrySendError::Closed(_)) => {
                return Err(anyhow!("Virtual microphone mirror playback closed"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn options() -> AudioOptions {
        AudioOptions {
            sample_rate: 48_000,
            channels: 2,
            frame_ms: 10,
            latency_ms: 30,
            queue_ms: 80,
        }
    }

    fn original() -> OriginalFrame {
        OriginalFrame {
            samples: [0.123_456_79_f32, -0.876_543_2].repeat(480).into(),
            sample_rate: 48_000,
            channels: 2,
            captured_at: Instant::now(),
        }
    }

    #[tokio::test]
    async fn mirror_retains_stereo_precision_and_generation_without_a_second_capture() {
        let source = Source::new();
        let publisher = source.begin(options());
        let format = source.format.borrow().unwrap();
        let receiver = source.frames.subscribe();
        let (playback, mut played) = mpsc::channel(2);
        let cancel = CancellationToken::new();
        let stats = Arc::new(AudioStats::default());
        stats.playback_generation.store(7, Ordering::Release);
        let worker = tokio::spawn(forward(
            receiver,
            playback,
            format,
            cancel.clone(),
            stats.clone(),
        ));
        let frame = original();
        publisher.publish(&frame);
        let received = tokio::time::timeout(Duration::from_secs(2), played.recv())
            .await
            .unwrap()
            .unwrap();
        let PlaybackCommand::Original {
            samples,
            generation,
        } = received
        else {
            panic!("mirror must send original PCM");
        };
        assert_eq!(generation, 7);
        assert!(Arc::ptr_eq(&samples, &frame.samples));
        assert_eq!(samples.as_ref(), frame.samples.as_ref());
        assert!(stats.command_tap.is_none());
        assert_eq!(stats.captured_frames.load(Ordering::Relaxed), 1);
        cancel.cancel();
        worker.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn translated_partial_tail_reaches_playback_without_padding_or_quantization_loss() {
        let options = AudioOptions {
            sample_rate: 24_000,
            channels: 1,
            frame_ms: 20,
            ..options()
        };
        let source = Source::new();
        let publisher = source.begin(options);
        let receiver = source.frames.subscribe();
        let format = source.format.borrow().unwrap();
        let samples = vec![i16::MIN, -1, 0, 1, i16::MAX];
        let prepared = publisher
            .prepare_playback(
                &PlaybackCommand::Audio {
                    samples: samples.clone(),
                    generation: 0,
                },
                options,
            )
            .unwrap();
        let (playback, mut played) = mpsc::channel(1);
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(forward(
            receiver,
            playback,
            format,
            cancel.clone(),
            Arc::default(),
        ));
        publisher.publish(&prepared);
        let received = tokio::time::timeout(Duration::from_secs(2), played.recv())
            .await
            .unwrap()
            .unwrap();
        let PlaybackCommand::Original {
            samples: output, ..
        } = received
        else {
            panic!("expected mirrored PCM");
        };
        assert_eq!(output.len(), samples.len());
        assert_eq!(
            output.as_ref(),
            samples
                .iter()
                .map(|sample| f32::from(*sample) / 32768.0)
                .collect::<Vec<_>>()
        );
        cancel.cancel();
        worker.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn stalled_mirror_is_bounded_and_never_backpressures_the_publisher() {
        let source = Source::new();
        let publisher = source.begin(options());
        let mut stalled = source.frames.subscribe();
        for _ in 0..100 {
            publisher.publish(&original());
        }
        assert!(matches!(
            stalled.try_recv(),
            Err(broadcast::error::TryRecvError::Lagged(92))
        ));
        let mut retained = 0;
        while stalled.try_recv().is_ok() {
            retained += 1;
        }
        assert_eq!(retained, QUEUE_FRAMES);
        // Starting a new mirror never replays buffered audio for another reader.
        assert!(matches!(
            source.frames.subscribe().try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn source_generations_fence_late_frames_and_old_publishers_cannot_clear_the_new_format() {
        let source = Source::new();
        let old = source.begin(options());
        let current = source.begin(options());
        let format = source.format.borrow().unwrap();
        assert_eq!(format.epoch, current.epoch);
        let receiver = source.frames.subscribe();
        let (playback, mut played) = mpsc::channel(2);
        let cancel = CancellationToken::new();
        let stats = Arc::new(AudioStats::default());
        let worker = tokio::spawn(forward(
            receiver,
            playback,
            format,
            cancel.clone(),
            stats.clone(),
        ));
        old.publish(&original());
        drop(old);
        assert_eq!(source.format.borrow().unwrap().epoch, current.epoch);
        let frame = original();
        current.publish(&frame);
        let received = tokio::time::timeout(Duration::from_secs(2), played.recv())
            .await
            .unwrap()
            .unwrap();
        let PlaybackCommand::Original { samples, .. } = received else {
            panic!("expected original PCM");
        };
        assert!(Arc::ptr_eq(&samples, &frame.samples));
        assert_eq!(stats.dropped_frames.load(Ordering::Relaxed), 1);
        drop(current);
        assert!(source.format.borrow().is_none());
        cancel.cancel();
        worker.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn full_playback_queue_drops_only_mirror_frames_and_cancellation_remains_bounded() {
        let source = Source::new();
        let publisher = source.begin(options());
        let format = source.format.borrow().unwrap();
        let receiver = source.frames.subscribe();
        let (playback, _stalled) = mpsc::channel(1);
        let cancel = CancellationToken::new();
        let stats = Arc::new(AudioStats::default());
        let worker = tokio::spawn(forward(
            receiver,
            playback,
            format,
            cancel.clone(),
            stats.clone(),
        ));
        publisher.publish(&original());
        publisher.publish(&original());
        tokio::time::timeout(Duration::from_secs(2), async {
            while stats.dropped_frames.load(Ordering::Relaxed) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(stats.captured_frames.load(Ordering::Relaxed), 2);
        assert_eq!(stats.processing_dropped_frames.load(Ordering::Relaxed), 0);
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
