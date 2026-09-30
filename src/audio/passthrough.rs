//! Bounded original-PCM bridge with an optional in-memory original history tap.
//! No provider, transcript, recording, or file I/O.
//! The supervisor owns route selection and prevents virtual-cable feedback.
use super::{AudioOptions, AudioStats, PcmFrame, PlaybackCommand, switching};
use crate::{history::HistoryBuffer, recording::RecordingLane};
use anyhow::{Context, Result, anyhow, ensure};
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

pub struct RouteDevices {
    pub capture: watch::Receiver<String>,
    pub playback: watch::Receiver<String>,
}

struct LevelReset(Arc<AudioStats>);
impl Drop for LevelReset {
    fn drop(&mut self) {
        self.0.passthrough_level.store(0, Ordering::Relaxed);
    }
}

/// Moves the original mono PCM directly between selected devices at the same
/// sample rate. Each selection can be replaced without replacing the route.
/// Use 48 kHz / 10 ms for idle routing; callbacks remain in the audio backends.
pub async fn run_route(
    devices: RouteDevices,
    options: AudioOptions,
    cancel: CancellationToken,
    stats: Arc<AudioStats>,
) -> Result<()> {
    run_route_inner(devices, options, cancel, stats, None).await
}

/// Adds history to an already-selected route; it never starts independent
/// capture or changes which device/application activates the original route.
pub async fn run_route_with_history(
    devices: RouteDevices,
    options: AudioOptions,
    cancel: CancellationToken,
    stats: Arc<AudioStats>,
    history: Arc<HistoryBuffer>,
    lane: RecordingLane,
) -> Result<()> {
    run_route_inner(devices, options, cancel, stats, Some((history, lane))).await
}

async fn run_route_inner(
    devices: RouteDevices,
    options: AudioOptions,
    cancel: CancellationToken,
    stats: Arc<AudioStats>,
    history: Option<(Arc<HistoryBuffer>, RecordingLane)>,
) -> Result<()> {
    let options = options.validate()?;
    let options = AudioOptions {
        queue_ms: options.queue_ms.min(100).max(options.frame_ms),
        ..options
    };
    let capture = devices.capture.borrow().clone();
    let playback = devices.playback.borrow().clone();
    ensure!(
        !capture.trim().is_empty() && !playback.trim().is_empty(),
        "A passagem de áudio exige dispositivos explícitos de captura e reprodução"
    );
    // A failed route cancels its own workers, never its supervisor's token.
    let cancel = cancel.child_token();
    let _cancel_on_drop = cancel.clone().drop_guard();
    // Pulse/CoreAudio callbacks arrive in batches. A two-frame queue cannot
    // absorb a normal 30–40 ms callback when forwarding 10 ms frames.
    let queue_frames = (options.queue_ms / options.frame_ms).clamp(1, 20) as usize;
    let (captured_tx, captured_rx) = mpsc::channel(queue_frames);
    let (play_tx, play_rx) = mpsc::channel(queue_frames);
    let mut jobs = JoinSet::new();
    let worker_cancel = cancel.clone();
    let worker_stats = stats.clone();
    jobs.spawn(async move {
        switching::capture(
            &capture,
            options,
            captured_tx,
            devices.capture,
            worker_cancel,
            worker_stats,
        )
        .await
        .context("Captura da passagem original")
    });
    let worker_cancel = cancel.clone();
    let worker_stats = stats.clone();
    jobs.spawn(async move {
        switching::playback(
            &playback,
            options,
            play_rx,
            devices.playback,
            worker_cancel,
            worker_stats,
        )
        .await
        .context("Reprodução da passagem original")
    });
    let worker_cancel = cancel.clone();
    jobs.spawn(async move {
        bridge_with_history(captured_rx, play_tx, options, worker_cancel, stats, history).await
    });
    let mut result = tokio::select! {
        biased;
        _=cancel.cancelled()=>Ok(()),
        completed=jobs.join_next()=>match completed {
            Some(Ok(Err(error)))=>Err(error),
            Some(Err(error))=>Err(anyhow!(error).context("Worker da passagem original interrompido")),
            _ if cancel.is_cancelled()=>Ok(()),
            _=>Err(anyhow!("A passagem original encerrou inesperadamente")),
        },
    };
    cancel.cancel();
    let shutdown = tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(completed) = jobs.join_next().await {
            let completed = completed
                .context("Worker da passagem original interrompido")
                .and_then(|r| r);
            if result.is_ok() && completed.is_err() {
                result = completed;
            }
        }
    })
    .await;
    if shutdown.is_err() {
        jobs.abort_all();
        while jobs.join_next().await.is_some() {}
        return Err(anyhow!(
            "A passagem original não encerrou seus dispositivos dentro de 3 segundos"
        ));
    }
    result
}

#[cfg(test)]
async fn bridge(
    captured: mpsc::Receiver<PcmFrame>,
    playback: mpsc::Sender<PlaybackCommand>,
    options: AudioOptions,
    cancel: CancellationToken,
    stats: Arc<AudioStats>,
) -> Result<()> {
    bridge_with_history(captured, playback, options, cancel, stats, None).await
}

struct HistoryTap {
    resampler: super::resample::Resampler,
    input: Vec<f32>,
    output: Vec<f32>,
    pcm: Vec<i16>,
    last_capture: Option<std::time::Instant>,
}
impl HistoryTap {
    fn new(sample_rate: u32) -> Self {
        Self {
            resampler: super::resample::Resampler::new(sample_rate, 16_000),
            input: Vec::new(),
            output: Vec::new(),
            pcm: Vec::new(),
            last_capture: None,
        }
    }

    fn push(&mut self, frame: &PcmFrame, history: &HistoryBuffer, lane: RecordingLane) {
        let frame_duration =
            Duration::from_secs_f64(frame.samples.len() as f64 / f64::from(frame.sample_rate));
        if self.last_capture.is_some_and(|previous| {
            frame.captured_at.saturating_duration_since(previous)
                > frame_duration + Duration::from_millis(50)
        }) {
            // Do not carry a filter tail across a dropped capture interval.
            self.resampler = super::resample::Resampler::new(frame.sample_rate, 16_000);
        }
        self.last_capture = Some(frame.captured_at);
        if frame.sample_rate == 16_000 {
            history.push(lane, &frame.samples, frame.captured_at);
            return;
        }
        self.input.clear();
        self.output.clear();
        self.pcm.clear();
        self.input.extend(
            frame
                .samples
                .iter()
                .map(|&sample| f32::from(sample) / 32768.0),
        );
        self.resampler.process(&self.input, &mut self.output);
        self.pcm.extend(
            self.output
                .iter()
                .map(|sample| (sample * 32768.0).round().clamp(-32768.0, 32767.0) as i16),
        );
        history.push(lane, &self.pcm, frame.captured_at);
    }
}

async fn bridge_with_history(
    mut captured: mpsc::Receiver<PcmFrame>,
    playback: mpsc::Sender<PlaybackCommand>,
    options: AudioOptions,
    cancel: CancellationToken,
    stats: Arc<AudioStats>,
    history: Option<(Arc<HistoryBuffer>, RecordingLane)>,
) -> Result<()> {
    let _level_reset = LevelReset(stats.clone());
    let mut history_tap = None;
    loop {
        let frame = tokio::select! {
            biased;
            _=cancel.cancelled()=>return Ok(()),
            frame=captured.recv()=>match frame {
                Some(frame)=>frame,
                None if cancel.is_cancelled()=>return Ok(()),
                None=>return Err(anyhow!("A captura da passagem original foi encerrada")),
            },
        };
        ensure!(
            frame.sample_rate == options.sample_rate
                && frame.samples.len() == options.frame_samples(),
            "Formato PCM inesperado na passagem original"
        );
        if frame.captured_at.elapsed() > Duration::from_millis(100) {
            stats.dropped_frames.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        if let Some((history, lane)) = &history {
            if history.enabled() {
                history_tap
                    .get_or_insert_with(|| HistoryTap::new(options.sample_rate))
                    .push(&frame, history, *lane);
            } else {
                history_tap = None;
            }
        }
        let level = (frame
            .samples
            .iter()
            .map(|&sample| {
                let sample = f64::from(sample) / 32768.0;
                sample * sample
            })
            .sum::<f64>()
            / frame.samples.len() as f64)
            .sqrt() as f32;
        stats
            .passthrough_level
            .store(level.to_bits(), Ordering::Relaxed);
        let generation = stats.playback_generation.load(Ordering::Acquire);
        // This is live audio: bounded drops under backpressure prevent a growing
        // delay. Recovery and hot switching never replay an unbounded backlog.
        match playback.try_send(PlaybackCommand::Audio {
            samples: frame.samples,
            generation,
        }) {
            Ok(()) => (),
            Err(mpsc::error::TrySendError::Full(_)) => {
                stats.dropped_frames.fetch_add(1, Ordering::Relaxed);
            }
            Err(mpsc::error::TrySendError::Closed(_)) if cancel.is_cancelled() => return Ok(()),
            Err(mpsc::error::TrySendError::Closed(_)) => {
                return Err(anyhow!("A reprodução da passagem original foi encerrada"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HistoryConfig;
    use std::time::Instant;
    fn options() -> AudioOptions {
        AudioOptions {
            sample_rate: 48000,
            frame_ms: 10,
            latency_ms: 30,
            queue_ms: 80,
        }
    }
    fn frame(value: i16) -> PcmFrame {
        PcmFrame {
            samples: vec![value; 480],
            sample_rate: 48000,
            captured_at: Instant::now(),
        }
    }
    #[tokio::test]
    async fn original_history_resamples_without_changing_live_playback() {
        let (capture_tx, capture_rx) = mpsc::channel(2);
        let (play_tx, mut play_rx) = mpsc::channel(2);
        let cancel = CancellationToken::new();
        let history = Arc::new(HistoryBuffer::new(&HistoryConfig::default()));
        let worker_cancel = cancel.clone();
        let worker_history = history.clone();
        let worker = tokio::spawn(async move {
            bridge_with_history(
                capture_rx,
                play_tx,
                options(),
                worker_cancel,
                Arc::new(AudioStats::default()),
                Some((worker_history, RecordingLane::Speaker)),
            )
            .await
        });
        for _ in 0..2 {
            capture_tx.send(frame(4000)).await.unwrap();
            let Some(PlaybackCommand::Audio { samples, .. }) = play_rx.recv().await else {
                panic!("missing original playback")
            };
            assert_eq!(samples, vec![4000; 480]);
        }
        let snapshot = history.snapshot(600, Instant::now());
        assert_eq!(snapshot.frames.len(), 2);
        assert!(
            snapshot
                .frames
                .iter()
                .all(|frame| frame.lane == RecordingLane::Speaker)
        );
        let pcm = snapshot
            .frames
            .iter()
            .flat_map(|frame| frame.samples())
            .copied()
            .collect::<Vec<_>>();
        assert!((300..=320).contains(&pcm.len()));
        assert!(pcm[20..].iter().all(|&value| (value - 4000).abs() <= 1));
        cancel.cancel();
        worker.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn disabled_history_keeps_passthrough_and_releases_its_filter_tail() {
        let (capture_tx, capture_rx) = mpsc::channel(2);
        let (play_tx, mut play_rx) = mpsc::channel(2);
        let cancel = CancellationToken::new();
        let history = Arc::new(HistoryBuffer::new(&HistoryConfig::default()));
        let worker_cancel = cancel.clone();
        let worker_history = history.clone();
        let worker = tokio::spawn(async move {
            bridge_with_history(
                capture_rx,
                play_tx,
                options(),
                worker_cancel,
                Arc::new(AudioStats::default()),
                Some((worker_history, RecordingLane::Microphone)),
            )
            .await
        });
        capture_tx.send(frame(10000)).await.unwrap();
        assert!(play_rx.recv().await.is_some());
        assert!(!history.snapshot(600, Instant::now()).frames.is_empty());
        history.configure(&HistoryConfig {
            enabled: false,
            ..HistoryConfig::default()
        });
        capture_tx.send(frame(20000)).await.unwrap();
        assert!(play_rx.recv().await.is_some());
        assert!(history.snapshot(600, Instant::now()).frames.is_empty());
        history.configure(&HistoryConfig::default());
        capture_tx.send(frame(0)).await.unwrap();
        assert!(play_rx.recv().await.is_some());
        let snapshot = history.snapshot(600, Instant::now());
        assert_eq!(snapshot.frames.len(), 1);
        assert!(
            snapshot.frames[0]
                .samples()
                .iter()
                .all(|&sample| sample == 0)
        );
        cancel.cancel();
        worker.await.unwrap().unwrap();
    }
    #[tokio::test]
    async fn pcm_is_bit_exact_and_tracks_the_current_playback_generation() {
        let (capture_tx, capture_rx) = mpsc::channel(2);
        let (play_tx, mut play_rx) = mpsc::channel(2);
        let cancel = CancellationToken::new();
        let worker_cancel = cancel.clone();
        let stats = Arc::new(AudioStats::default());
        let worker_stats = stats.clone();
        let worker = tokio::spawn(async move {
            bridge(capture_rx, play_tx, options(), worker_cancel, worker_stats).await
        });
        let mut original = frame(0);
        for (i, sample) in original.samples.iter_mut().enumerate() {
            *sample = (i as i16).wrapping_mul(171);
        }
        let expected = original.samples.clone();
        capture_tx.send(original).await.unwrap();
        let Some(PlaybackCommand::Audio {
            samples,
            generation,
        }) = play_rx.recv().await
        else {
            panic!("missing original PCM")
        };
        assert_eq!(samples, expected);
        assert_eq!(generation, 0);
        assert!(f32::from_bits(stats.passthrough_level.load(Ordering::Relaxed)) > 0.1);
        stats.playback_generation.store(17, Ordering::Release);
        capture_tx.send(frame(-1234)).await.unwrap();
        let Some(PlaybackCommand::Audio {
            samples,
            generation,
        }) = play_rx.recv().await
        else {
            panic!("missing switched PCM")
        };
        assert_eq!(samples, vec![-1234; 480]);
        assert_eq!(generation, 17);
        cancel.cancel();
        worker.await.unwrap().unwrap();
        assert_eq!(stats.passthrough_level.load(Ordering::Relaxed), 0);
    }
    #[tokio::test]
    async fn a_saturated_output_drops_bounded_frames_and_remains_cancellable() {
        let (capture_tx, capture_rx) = mpsc::channel(2);
        let (play_tx, mut play_rx) = mpsc::channel(1);
        let cancel = CancellationToken::new();
        let worker_cancel = cancel.clone();
        let stats = Arc::new(AudioStats::default());
        let worker_stats = stats.clone();
        let worker = tokio::spawn(async move {
            bridge(capture_rx, play_tx, options(), worker_cancel, worker_stats).await
        });
        for _ in 0..8 {
            capture_tx.send(frame(999)).await.unwrap();
        }
        tokio::time::timeout(Duration::from_secs(1), async {
            while stats.dropped_frames.load(Ordering::Relaxed) < 7 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(play_rx.len(), 1);
        assert!(play_rx.try_recv().is_ok());
        cancel.cancel();
        tokio::time::timeout(Duration::from_millis(100), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    #[tokio::test]
    async fn unexpected_eof_or_format_is_an_error_but_normal_cancellation_is_clean() {
        let (tx, rx) = mpsc::channel(1);
        let (play_tx, _play_rx) = mpsc::channel(1);
        drop(tx);
        assert!(
            bridge(
                rx,
                play_tx,
                options(),
                CancellationToken::new(),
                Arc::new(AudioStats::default())
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("captura")
        );
        let (tx, rx) = mpsc::channel(1);
        let (play_tx, _play_rx) = mpsc::channel(1);
        let mut wrong = frame(1);
        wrong.sample_rate = 16000;
        tx.send(wrong).await.unwrap();
        assert!(
            bridge(
                rx,
                play_tx,
                options(),
                CancellationToken::new(),
                Arc::new(AudioStats::default())
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("PCM")
        );
        let (tx, rx) = mpsc::channel(1);
        let (play_tx, _play_rx) = mpsc::channel(1);
        drop(tx);
        let cancel = CancellationToken::new();
        cancel.cancel();
        bridge(
            rx,
            play_tx,
            options(),
            cancel,
            Arc::new(AudioStats::default()),
        )
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn empty_device_selection_fails_without_opening_audio() {
        let (_capture_tx, capture) = watch::channel(String::new());
        let (_playback_tx, playback) = watch::channel("not-opened".into());
        assert!(
            run_route(
                RouteDevices { capture, playback },
                options(),
                CancellationToken::new(),
                Arc::new(AudioStats::default())
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn stale_capture_is_discarded_instead_of_replayed_after_a_stall() {
        let (tx, rx) = mpsc::channel(2);
        let (play_tx, mut play_rx) = mpsc::channel(2);
        let cancel = CancellationToken::new();
        let worker_cancel = cancel.clone();
        let stats = Arc::new(AudioStats::default());
        let worker_stats = stats.clone();
        let mut stale = frame(1111);
        stale.captured_at = Instant::now() - Duration::from_millis(150);
        tx.send(stale).await.unwrap();
        tx.send(frame(2222)).await.unwrap();
        let worker = tokio::spawn(async move {
            bridge(rx, play_tx, options(), worker_cancel, worker_stats).await
        });
        let Some(PlaybackCommand::Audio { samples, .. }) = play_rx.recv().await else {
            panic!("Fresh PCM missing")
        };
        assert_eq!(samples, vec![2222; 480]);
        assert_eq!(stats.dropped_frames.load(Ordering::Relaxed), 1);
        assert!(play_rx.try_recv().is_err());
        cancel.cancel();
        worker.await.unwrap().unwrap();
    }
}
