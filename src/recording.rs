//! One bounded recording of the two original capture streams.
//! Input is PCM16/16 kHz after device resampling, before translation/gain.
use crate::config::{RecordingLayout, RecordingMixConfig};
use anyhow::{Context, Result, ensure};

mod mix;
use mix::RecordingMixer;
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::{
    fs::{self, OpenOptions},
    sync::mpsc,
};

const SAMPLE_RATE: u64 = 16_000;
const HOLDBACK_SAMPLES: u64 = SAMPLE_RATE * 2;
const JITTER_SAMPLES: u64 = SAMPLE_RATE / 20; // 50 ms scheduler jitter.
const MAX_FRAME_SAMPLES: usize = SAMPLE_RATE as usize;
const WRITE_FRAMES: usize = 4096;
const PCM_SUBFORMAT: [u8; 16] = [1, 0, 0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingLane {
    Microphone,
    Speaker,
}
impl RecordingLane {
    fn index(self) -> usize {
        match self {
            Self::Microphone => 0,
            Self::Speaker => 1,
        }
    }
}
#[derive(Debug)]
pub struct AudioRecord {
    pub lane: RecordingLane,
    pub samples: Vec<i16>,
    pub channels: u16,
    /// Capture-frame completion on the same monotonic clock as the session origin.
    pub captured_at: Instant,
}
#[derive(Default)]
struct LaneClock {
    next_sample: Option<u64>,
    last_capture: Option<Instant>,
}

pub struct SessionAudioRecorder {
    path: PathBuf,
    file: crate::storage::resilient::ResilientFile,
    origin: Instant,
    active: [bool; 2],
    mixer: Option<RecordingMixer>,
    clocks: [LaneClock; 2],
    /// Separate sources for [processed, processed+pending.len()), aligned
    /// independently of callback and chunk arrival order.
    pending: VecDeque<[[i16; 2]; 2]>,
    processed: u64,
    /// WAV frames written. The mixer's bounded lookahead can trail processed PCM.
    written: u64,
    latest_end: u64,
    bytes: Vec<u8>,
}

impl SessionAudioRecorder {
    pub async fn create(
        directory: &Path,
        stem: &str,
        origin: Instant,
        microphone: bool,
        speaker: bool,
    ) -> Result<Self> {
        Self::create_with_mix(
            directory,
            stem,
            origin,
            microphone,
            speaker,
            &RecordingMixConfig::transparent(),
        )
        .await
    }

    pub async fn create_with_mix(
        directory: &Path,
        stem: &str,
        origin: Instant,
        microphone: bool,
        speaker: bool,
        mix: &RecordingMixConfig,
    ) -> Result<Self> {
        Self::create_with_layout(
            directory,
            stem,
            origin,
            microphone,
            speaker,
            mix,
            RecordingLayout::Mixed,
        )
        .await
    }

    pub(crate) async fn create_with_layout(
        directory: &Path,
        stem: &str,
        origin: Instant,
        microphone: bool,
        speaker: bool,
        mix: &RecordingMixConfig,
        layout: RecordingLayout,
    ) -> Result<Self> {
        // Invalid DSP settings must not create directories or partial files.
        mix.validate()?;
        ensure!(microphone || speaker, "Select at least one input to record");
        ensure!(
            origin <= Instant::now(),
            "Recording start time is in the future"
        );
        crate::session::validate_file_stem(stem)?;
        fs::create_dir_all(directory)
            .await
            .context("Could not create the recording folder")?;
        let path = directory.join(format!("{stem}.wav"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options.open(&path).await.with_context(|| {
            format!(
                "Could not create the recording at {}; existing files are never overwritten",
                path.display()
            )
        })?;
        let mut file = crate::storage::resilient::ResilientFile::new(file);
        let mixer = (layout == RecordingLayout::Mixed)
            .then(|| RecordingMixer::new(mix, microphone, speaker));
        let channels = channel_count(&mixer);
        file.write_all(&wav_header(0, channels)?).await?;
        file.flush().await?;
        Ok(Self {
            path,
            file,
            origin,
            active: [microphone, speaker],
            mixer,
            clocks: Default::default(),
            pending: VecDeque::with_capacity((HOLDBACK_SAMPLES + SAMPLE_RATE) as usize),
            processed: 0,
            written: 0,
            latest_end: 0,
            bytes: Vec::with_capacity(WRITE_FRAMES * usize::from(channels) * 2),
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub(crate) fn observe_recovery(
        &mut self,
        observer: crate::storage::resilient::RecoveryObserver,
    ) {
        self.file.observe(observer);
    }
    /// File creation can precede the capture boundary so slow storage never
    /// pauses original routing. Rebase only before any PCM has been accepted.
    pub(crate) fn set_origin(&mut self, origin: Instant) -> Result<()> {
        ensure!(
            self.processed == 0
                && self.written == 0
                && self.latest_end == 0
                && self.pending.is_empty(),
            "Cannot change the start after receiving audio"
        );
        ensure!(origin <= Instant::now(), "Recording time is in the future");
        self.origin = origin;
        Ok(())
    }

    /// Supervisor closes all senders after capture stops. EOF drains queued
    /// frames and finalizes the WAV; cancelling this worker early loses its tail.
    pub async fn run(self, records: mpsc::Receiver<AudioRecord>) -> Result<()> {
        self.run_with_history(records, std::iter::empty()).await
    }
    /// Historical originals precede live frames on the same monotonic clock.
    /// The iterator materializes only one frame at a time, keeping PCM bounded.
    pub async fn run_with_history(
        mut self,
        mut records: mpsc::Receiver<AudioRecord>,
        history: impl IntoIterator<Item = AudioRecord>,
    ) -> Result<()> {
        let result = async {
            for frame in history {
                self.append(frame).await?;
            }
            self.receive(&mut records).await
        }
        .await;
        // Try to leave already accepted samples playable even when a later
        // record is invalid. A disk failure remains an observable session error.
        let finalized = self.finalize().await;
        result.and(finalized)
    }
    async fn receive(&mut self, records: &mut mpsc::Receiver<AudioRecord>) -> Result<()> {
        let mut checkpoint = tokio::time::interval(Duration::from_secs(1));
        checkpoint.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                record=records.recv()=>match record { Some(record)=>self.append(record).await?,None=>return Ok(()) },
                _=checkpoint.tick()=>self.checkpoint().await?,
            }
        }
    }
    async fn append(&mut self, record: AudioRecord) -> Result<()> {
        let lane = record.lane.index();
        if !self.active[lane] {
            return Ok(());
        }
        ensure!(
            matches!(record.channels, 1 | 2)
                && !record.samples.is_empty()
                && record
                    .samples
                    .len()
                    .is_multiple_of(usize::from(record.channels))
                && record.samples.len() <= MAX_FRAME_SAMPLES * usize::from(record.channels),
            "Invalid recording frame; maximum 1 second of PCM16 at 16 kHz"
        );
        ensure!(
            record.captured_at <= Instant::now(),
            "Recording frame has a capture time in the future"
        );
        let elapsed = record
            .captured_at
            .checked_duration_since(self.origin)
            .context("Recording frame predates the session start")?;
        ensure!(
            self.clocks[lane]
                .last_capture
                .is_none_or(|previous| record.captured_at >= previous),
            "Recording capture clock moved backwards"
        );
        let end_by_clock = (elapsed.as_nanos() * u128::from(SAMPLE_RATE) / 1_000_000_000)
            .min(u128::from(u64::MAX)) as u64;
        let channels = usize::from(record.channels);
        let frames = record.samples.len() / channels;
        let candidate = end_by_clock.saturating_sub(frames as u64);
        let start = match self.clocks[lane].next_sample {
            Some(expected) if candidate <= expected.saturating_add(JITTER_SAMPLES) => expected,
            _ => candidate,
        };
        let end = start
            .checked_add(frames as u64)
            .context("Recording duration exceeded")?;
        ensure!(
            end <= max_wav_frames(channel_count(&self.mixer)),
            "Recording reached the WAV RIFF size limit; start a new session"
        );
        ensure!(
            start >= self.processed,
            "A capture arrived more than 2 seconds late for recording; the session stopped to prevent silent data loss"
        );
        let latest = self.latest_end.max(end);
        self.flush_until(latest.saturating_sub(HOLDBACK_SAMPLES))
            .await?;
        let needed = end.saturating_sub(self.processed) as usize;
        ensure!(
            needed <= (HOLDBACK_SAMPLES + SAMPLE_RATE) as usize,
            "Recording alignment window exceeded the memory limit"
        );
        if self.pending.len() < needed {
            self.pending.resize(needed, [[0; 2]; 2]);
        }
        let offset = (start - self.processed) as usize;
        for (index, frame) in record.samples.chunks_exact(channels).enumerate() {
            self.pending[offset + index][lane] = [frame[0], *frame.get(1).unwrap_or(&frame[0])];
        }
        self.latest_end = latest;
        self.clocks[lane].next_sample = Some(end);
        self.clocks[lane].last_capture = Some(record.captured_at);
        Ok(())
    }
    async fn flush_until(&mut self, end: u64) -> Result<()> {
        ensure!(
            end <= max_wav_frames(channel_count(&self.mixer)),
            "Maximum WAV file duration exceeded"
        );
        while self.processed < end {
            let count = (end - self.processed).min(WRITE_FRAMES as u64) as usize;
            self.bytes.clear();
            for _ in 0..count {
                let sources = self.pending.pop_front().unwrap_or([[0; 2]; 2]);
                if let Some(mixer) = &mut self.mixer {
                    let mono =
                        sources.map(|[left, right]| (i32::from(left) + i32::from(right)) / 2);
                    if let Some(sample) = mixer.push(mono) {
                        self.bytes.extend_from_slice(&sample.to_le_bytes());
                    }
                } else {
                    // Microphone L/R followed by incoming audio L/R.
                    for sample in sources.into_iter().flatten() {
                        self.bytes.extend_from_slice(&sample.to_le_bytes());
                    }
                }
            }
            self.write_audio_bytes().await?;
            self.processed += count as u64;
        }
        Ok(())
    }
    async fn write_audio_bytes(&mut self) -> Result<()> {
        if !self.bytes.is_empty() {
            self.file
                .write_all(&self.bytes)
                .await
                .context("Failed to write session audio")?;
            self.written +=
                (self.bytes.len() / (usize::from(channel_count(&self.mixer)) * 2)) as u64;
        }
        Ok(())
    }
    async fn checkpoint(&mut self) -> Result<()> {
        let channels = channel_count(&self.mixer);
        self.file
            .flush()
            .await
            .context("Failed to flush the recording")?;
        self.file.seek(std::io::SeekFrom::Start(0)).await?;
        self.file
            .write_all(&wav_header(self.written, channels)?)
            .await?;
        self.file.flush().await?;
        self.file
            .seek(std::io::SeekFrom::Start(
                header_bytes(channels) + self.written * u64::from(channels) * 2,
            ))
            .await?;
        Ok(())
    }
    async fn finalize(&mut self) -> Result<()> {
        self.flush_until(self.latest_end).await?;
        // Only the fixed lookahead remains; do not pad the saved timeline or
        // discard its final originals when the session ends.
        if let Some(mixer) = &mut self.mixer {
            self.bytes.clear();
            while let Some(sample) = mixer.finish_sample() {
                self.bytes.extend_from_slice(&sample.to_le_bytes());
            }
            self.write_audio_bytes().await?;
        }
        self.checkpoint().await?;
        self.file
            .sync_data()
            .await
            .context("Failed to finalize the recording on disk")
    }
}
fn channel_count(mixer: &Option<RecordingMixer>) -> u16 {
    if mixer.is_some() { 1 } else { 4 }
}

fn header_bytes(channels: u16) -> u64 {
    if channels > 2 { 68 } else { 44 }
}

fn max_wav_frames(channels: u16) -> u64 {
    (u64::from(u32::MAX) - (header_bytes(channels) - 8)) / (u64::from(channels) * 2)
}

fn wav_header(frames: u64, channels: u16) -> Result<Vec<u8>> {
    ensure!(
        frames <= max_wav_frames(channels),
        "Maximum WAV file duration exceeded"
    );
    let bytes_per_frame = channels * 2;
    let data_bytes = (frames * u64::from(bytes_per_frame)) as u32;
    let extensible = channels > 2;
    let mut header = Vec::with_capacity(header_bytes(channels) as usize);
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&(data_bytes + (header_bytes(channels) as u32 - 8)).to_le_bytes());
    header.extend_from_slice(b"WAVEfmt ");
    let format_size: u32 = if extensible { 40 } else { 16 };
    let format: u16 = if extensible { 0xfffe } else { 1 };
    header.extend_from_slice(&format_size.to_le_bytes());
    header.extend_from_slice(&format.to_le_bytes());
    header.extend_from_slice(&channels.to_le_bytes());
    header.extend_from_slice(&(SAMPLE_RATE as u32).to_le_bytes());
    header.extend_from_slice(&(SAMPLE_RATE as u32 * u32::from(bytes_per_frame)).to_le_bytes());
    header.extend_from_slice(&bytes_per_frame.to_le_bytes());
    header.extend_from_slice(&16_u16.to_le_bytes());
    if extensible {
        header.extend_from_slice(&22_u16.to_le_bytes());
        header.extend_from_slice(&16_u16.to_le_bytes());
        // No speaker positions. These channels are ordered by source.
        header.extend_from_slice(&0_u32.to_le_bytes());
        header.extend_from_slice(&PCM_SUBFORMAT);
    }
    header.extend_from_slice(b"data");
    header.extend_from_slice(&data_bytes.to_le_bytes());
    Ok(header)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record(
        lane: RecordingLane,
        origin: Instant,
        start_ms: u64,
        samples: Vec<i16>,
    ) -> AudioRecord {
        let captured_at = origin
            + Duration::from_millis(start_ms)
            + Duration::from_nanos(samples.len() as u64 * 1_000_000_000 / SAMPLE_RATE);
        AudioRecord {
            lane,
            samples,
            channels: 1,
            captured_at,
        }
    }
    fn pcm(path: &Path) -> Vec<i16> {
        let mut wav = hound::WavReader::open(path).unwrap();
        assert_eq!(wav.spec().sample_rate, 16000);
        assert_eq!(wav.spec().channels, 1);
        assert_eq!(wav.spec().bits_per_sample, 16);
        wav.samples::<i16>().map(Result::unwrap).collect()
    }

    async fn balanced_wave(
        chunk_samples: usize,
        reverse_lanes: bool,
        history_samples: usize,
    ) -> Vec<i16> {
        use std::f64::consts::TAU;
        let directory = tempfile::tempdir().unwrap();
        let origin = Instant::now() - Duration::from_secs(10);
        let config = RecordingMixConfig {
            microphone_gain_db: 12.0,
            speaker_gain_db: 12.0,
            ..Default::default()
        };
        let recorder = SessionAudioRecorder::create_with_mix(
            directory.path(),
            "balance",
            origin,
            true,
            true,
            &config,
        )
        .await
        .unwrap();
        let path = recorder.path().to_owned();
        let mut history = Vec::new();
        let mut live = Vec::new();
        let microphone: Vec<_> = (0..48_000)
            .map(|index| {
                if (8000..24_000).contains(&index) {
                    (260.0 * (TAU * 300.0 * index as f64 / 16_000.0).sin()).round() as i16
                } else {
                    0
                }
            })
            .collect();
        let speaker: Vec<_> = (0..48_000)
            .map(|index| (22_000.0 * (TAU * 900.0 * index as f64 / 16_000.0).sin()).round() as i16)
            .collect();
        for start in (0..48_000).step_by(chunk_samples) {
            let end = start + chunk_samples;
            let mut sources = [RecordingLane::Microphone, RecordingLane::Speaker];
            if reverse_lanes {
                sources.reverse();
            }
            for lane in sources {
                let samples = match lane {
                    RecordingLane::Microphone => microphone[start..end].to_vec(),
                    RecordingLane::Speaker => speaker[start..end].to_vec(),
                };
                let record = record(lane, origin, start as u64 / 16, samples);
                if end <= history_samples {
                    history.push(record);
                } else {
                    live.push(record);
                }
            }
        }
        let (sender, received) = mpsc::channel(live.len().max(1));
        for frame in live {
            sender.send(frame).await.unwrap();
        }
        drop(sender);
        recorder.run_with_history(received, history).await.unwrap();
        pcm(&path)
    }

    #[tokio::test]
    async fn balanced_mix_is_invariant_to_chunk_sizes_lane_order_and_history_boundary() {
        let continuous = balanced_wave(160, false, 0).await;
        let reordered = balanced_wave(800, true, 0).await;
        let prefix = balanced_wave(800, false, 16_000).await;
        assert_eq!(
            continuous.len(),
            48_000,
            "EOF retains the limiter lookahead without padding"
        );
        assert_eq!(continuous, reordered);
        assert_eq!(continuous, prefix);
        assert!(
            continuous
                .iter()
                .all(|sample| i32::from(*sample).abs() <= 32_113)
        );
    }

    #[tokio::test]
    async fn boosted_checkpoint_and_eof_preserve_exact_sample_count_with_bounded_storage() {
        let directory = tempfile::tempdir().unwrap();
        let origin = Instant::now() - Duration::from_secs(10);
        let config = RecordingMixConfig {
            microphone_gain_db: 12.0,
            ..Default::default()
        };
        let mut recorder = SessionAudioRecorder::create_with_mix(
            directory.path(),
            "limited",
            origin,
            true,
            true,
            &config,
        )
        .await
        .unwrap();
        let path = recorder.path().to_owned();
        for second in 0..4 {
            for lane in [RecordingLane::Microphone, RecordingLane::Speaker] {
                recorder
                    .append(record(lane, origin, second * 1000, vec![16_000; 16_000]))
                    .await
                    .unwrap();
                assert!(recorder.pending.len() <= (HOLDBACK_SAMPLES + SAMPLE_RATE) as usize);
                assert!(recorder.processed - recorder.written <= 80);
                assert!(recorder.bytes.capacity() <= WRITE_FRAMES * 2);
            }
        }
        recorder.checkpoint().await.unwrap();
        assert_eq!(pcm(&path).len() as u64, recorder.written);
        let (sender, received) = mpsc::channel(1);
        drop(sender);
        recorder.run(received).await.unwrap();
        let final_pcm = pcm(&path);
        assert_eq!(final_pcm.len(), 64_000);
        assert!(
            final_pcm[63_920..].iter().all(|sample| *sample != 0),
            "the fixed lookahead tail is saved exactly once"
        );
    }

    #[tokio::test]
    async fn invalid_recording_mix_does_not_create_a_folder_or_file() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("not-created").join("nested");
        let config = RecordingMixConfig {
            microphone_gain_db: f32::NAN,
            ..Default::default()
        };
        assert!(
            SessionAudioRecorder::create_with_mix(
                &output,
                "invalid",
                Instant::now(),
                true,
                true,
                &config
            )
            .await
            .is_err()
        );
        assert!(!directory.path().join("not-created").exists());
    }
    #[tokio::test]
    async fn historical_prefix_and_live_audio_share_one_mixed_timeline_without_duplicates() {
        let directory = tempfile::tempdir().unwrap();
        let origin = Instant::now() - Duration::from_secs(3);
        let recorder =
            SessionAudioRecorder::create(directory.path(), "late-start", origin, true, true)
                .await
                .unwrap();
        let path = recorder.path().to_owned();
        let past = [
            record(RecordingLane::Microphone, origin, 0, vec![1000; 16000]),
            record(RecordingLane::Speaker, origin, 0, vec![3000; 16000]),
        ];
        let (tx, rx) = mpsc::channel(4);
        tx.send(record(
            RecordingLane::Microphone,
            origin,
            1000,
            vec![2000; 16000],
        ))
        .await
        .unwrap();
        tx.send(record(
            RecordingLane::Speaker,
            origin,
            1000,
            vec![4000; 16000],
        ))
        .await
        .unwrap();
        drop(tx);
        recorder.run_with_history(rx, past).await.unwrap();
        let samples = pcm(&path);
        assert_eq!(samples.len(), 32000);
        assert!(samples[..16000].iter().all(|&sample| sample == 2000));
        assert!(samples[16000..].iter().all(|&sample| sample == 3000));
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
    #[tokio::test]
    async fn merged_wave_overlaps_lanes_and_absorbs_small_capture_jitter() {
        let directory = tempfile::tempdir().unwrap();
        let origin = Instant::now() - Duration::from_secs(1);
        let recorder =
            SessionAudioRecorder::create(directory.path(), "same-session", origin, true, true)
                .await
                .unwrap();
        let path = recorder.path().to_owned();
        let (tx, rx) = mpsc::channel(8);
        tx.send(record(
            RecordingLane::Microphone,
            origin,
            0,
            vec![1000; 1600],
        ))
        .await
        .unwrap();
        // Capture arrives 5 ms late; this same lane must remain sample-contiguous.
        tx.send(record(
            RecordingLane::Microphone,
            origin,
            105,
            vec![1000; 1600],
        ))
        .await
        .unwrap();
        // The other lane arrives afterwards but belongs to the same timeline.
        tx.send(record(RecordingLane::Speaker, origin, 50, vec![3000; 1600]))
            .await
            .unwrap();
        drop(tx);
        recorder.run(rx).await.unwrap();
        let samples = pcm(&path);
        assert_eq!(samples.len(), 3200);
        assert!(samples[..800].iter().all(|&s| s == 500));
        assert!(samples[800..2400].iter().all(|&s| s == 2000));
        assert!(samples[2400..].iter().all(|&s| s == 500));
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
    #[tokio::test]
    async fn real_discontinuities_keep_silence_and_single_lane_keeps_unity_gain() {
        let directory = tempfile::tempdir().unwrap();
        let origin = Instant::now() - Duration::from_secs(1);
        let recorder = SessionAudioRecorder::create(directory.path(), "gaps", origin, true, false)
            .await
            .unwrap();
        let path = recorder.path().to_owned();
        let (tx, rx) = mpsc::channel(4);
        tx.send(record(
            RecordingLane::Microphone,
            origin,
            100,
            vec![4000; 1600],
        ))
        .await
        .unwrap();
        tx.send(record(
            RecordingLane::Speaker,
            origin,
            200,
            vec![30000; 1600],
        ))
        .await
        .unwrap();
        tx.send(record(
            RecordingLane::Microphone,
            origin,
            500,
            vec![-4000; 1600],
        ))
        .await
        .unwrap();
        drop(tx);
        recorder.run(rx).await.unwrap();
        let samples = pcm(&path);
        assert_eq!(samples.len(), 9600);
        assert!(samples[..1600].iter().all(|&s| s == 0));
        assert!(samples[1600..3200].iter().all(|&s| s == 4000));
        assert!(samples[3200..8000].iter().all(|&s| s == 0));
        assert!(samples[8000..].iter().all(|&s| s == -4000));
    }
    #[tokio::test]
    async fn full_scale_overlapping_audio_has_headroom_without_integer_wrap() {
        let directory = tempfile::tempdir().unwrap();
        let origin = Instant::now() - Duration::from_secs(20);
        let recorder =
            SessionAudioRecorder::create(directory.path(), "fullscale", origin, true, true)
                .await
                .unwrap();
        let path = recorder.path().to_owned();
        let (tx, rx) = mpsc::channel(2);
        for lane in [RecordingLane::Microphone, RecordingLane::Speaker] {
            tx.send(record(lane, origin, 0, vec![i16::MAX, i16::MIN]))
                .await
                .unwrap();
        }
        drop(tx);
        recorder.run(rx).await.unwrap();
        assert_eq!(pcm(&path), [i16::MAX, i16::MIN]);
    }
    #[tokio::test]
    async fn long_gaps_are_written_in_bounded_blocks_and_old_frames_fail_explicitly() {
        let directory = tempfile::tempdir().unwrap();
        let origin = Instant::now() - Duration::from_secs(20);
        let mut recorder =
            SessionAudioRecorder::create(directory.path(), "late", origin, true, true)
                .await
                .unwrap();
        let path = recorder.path().to_owned();
        recorder
            .append(record(
                RecordingLane::Microphone,
                origin,
                0,
                vec![4000; 1600],
            ))
            .await
            .unwrap();
        recorder
            .append(record(
                RecordingLane::Microphone,
                origin,
                5000,
                vec![4000; 1600],
            ))
            .await
            .unwrap();
        assert!(recorder.pending.len() <= (HOLDBACK_SAMPLES + SAMPLE_RATE) as usize);
        assert!(recorder.bytes.capacity() <= WRITE_FRAMES * 2);
        let (tx, rx) = mpsc::channel(1);
        tx.send(record(RecordingLane::Speaker, origin, 0, vec![4000; 1600]))
            .await
            .unwrap();
        drop(tx);
        assert!(
            recorder
                .run(rx)
                .await
                .unwrap_err()
                .to_string()
                .contains("late")
        );
        // Even the error path finalizes accepted data into a valid WAV.
        assert_eq!(pcm(&path).len(), 81600);
    }
    #[tokio::test]
    async fn empty_recording_is_valid_and_existing_files_are_never_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let origin = Instant::now() - Duration::from_secs(20);
        let recorder = SessionAudioRecorder::create(directory.path(), "empty", origin, true, false)
            .await
            .unwrap();
        let path = recorder.path().to_owned();
        let before = std::fs::read(&path).unwrap();
        assert!(
            SessionAudioRecorder::create(directory.path(), "empty", origin, true, false)
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let (tx, rx) = mpsc::channel(1);
        drop(tx);
        recorder.run(rx).await.unwrap();
        assert!(pcm(&path).is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    #[tokio::test]
    async fn preparing_the_file_before_capture_does_not_add_silence_to_the_recording() {
        let directory = tempfile::tempdir().unwrap();
        let origin = Instant::now() - Duration::from_secs(1);
        let mut recorder = SessionAudioRecorder::create(
            directory.path(),
            "prepared",
            origin - Duration::from_secs(10),
            true,
            false,
        )
        .await
        .unwrap();
        recorder.set_origin(origin).unwrap();
        let path = recorder.path().to_owned();
        let (sender, receiver) = mpsc::channel(1);
        sender
            .send(AudioRecord {
                lane: RecordingLane::Microphone,
                samples: vec![777; 320],
                channels: 1,
                captured_at: origin + Duration::from_millis(20),
            })
            .await
            .unwrap();
        drop(sender);
        recorder.run(receiver).await.unwrap();
        assert_eq!(pcm(&path), vec![777; 320]);
    }
    #[tokio::test]
    async fn invalid_names_and_oversized_frames_fail_before_consuming_unbounded_data() {
        let directory = tempfile::tempdir().unwrap();
        let origin = Instant::now() - Duration::from_secs(20);
        for name in [
            "../private",
            "a/b",
            "a\\b",
            ".",
            "CON",
            "bad\nname",
            "trailing.",
        ] {
            assert!(
                SessionAudioRecorder::create(directory.path(), name, origin, true, false)
                    .await
                    .is_err()
            );
        }
        assert!(
            std::fs::read_dir(directory.path())
                .unwrap()
                .next()
                .is_none()
        );
        assert!(
            SessionAudioRecorder::create(directory.path(), "none", origin, false, false)
                .await
                .is_err()
        );
        let recorder =
            SessionAudioRecorder::create(directory.path(), "bounded", origin, true, false)
                .await
                .unwrap();
        let (tx, rx) = mpsc::channel(1);
        tx.send(record(
            RecordingLane::Microphone,
            origin,
            0,
            vec![0; MAX_FRAME_SAMPLES + 1],
        ))
        .await
        .unwrap();
        drop(tx);
        assert!(
            recorder
                .run(rx)
                .await
                .unwrap_err()
                .to_string()
                .contains("maximum")
        );
        for channels in [1, 4] {
            assert!(wav_header(max_wav_frames(channels), channels).is_ok());
            assert!(wav_header(max_wav_frames(channels) + 1, channels).is_err());
        }
    }
    #[tokio::test]
    async fn multichannel_wav_preserves_both_stereo_pairs_and_silence() {
        let directory = tempfile::tempdir().unwrap();
        let origin = Instant::now() - Duration::from_secs(5);
        let mut recorder = SessionAudioRecorder::create_with_layout(
            directory.path(),
            "separate",
            origin,
            true,
            true,
            &RecordingMixConfig {
                microphone_gain_db: 12.0,
                speaker_gain_db: -12.0,
                ..Default::default()
            },
            RecordingLayout::SeparateChannels,
        )
        .await
        .unwrap();
        let path = recorder.path().to_owned();
        let stereo = |lane, end_ms, pair: [i16; 2]| AudioRecord {
            lane,
            samples: pair.repeat(16_000),
            channels: 2,
            captured_at: origin + Duration::from_millis(end_ms),
        };
        for (lane, end, pair) in [
            (RecordingLane::Speaker, 1000, [i16::MAX, 3000]),
            (RecordingLane::Microphone, 1000, [i16::MIN, 2000]),
            (RecordingLane::Microphone, 3000, [1234, -4321]),
        ] {
            recorder.append(stereo(lane, end, pair)).await.unwrap();
        }
        recorder.checkpoint().await.unwrap();
        let wave = hound::WavReader::open(&path).unwrap();
        assert_eq!(wave.spec().channels, 4);
        assert_eq!(wave.spec().sample_rate, 16_000);
        assert_eq!(wave.duration(), 16_000);
        drop(wave);

        let (tx, rx) = mpsc::channel(1);
        tx.send(stereo(RecordingLane::Speaker, 3000, [-3000, 5000]))
            .await
            .unwrap();
        drop(tx);
        recorder.run(rx).await.unwrap();
        let mut wave = hound::WavReader::open(&path).unwrap();
        assert_eq!(wave.duration(), 48_000);
        let samples = wave
            .samples::<i16>()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            samples[..64_000]
                .as_chunks::<4>()
                .0
                .iter()
                .all(|&frame| frame == [i16::MIN, 2000, i16::MAX, 3000])
        );
        assert!(samples[64_000..128_000].iter().all(|sample| *sample == 0));
        assert!(
            samples[128_000..]
                .as_chunks::<4>()
                .0
                .iter()
                .all(|&frame| frame == [1234, -4321, -3000, 5000])
        );
    }

    #[tokio::test]
    async fn checkpoint_header_exposes_only_committed_audio_and_eof_drains_the_tail() {
        let directory = tempfile::tempdir().unwrap();
        let origin = Instant::now() - Duration::from_secs(20);
        let mut recorder =
            SessionAudioRecorder::create(directory.path(), "checkpoint", origin, false, true)
                .await
                .unwrap();
        let path = recorder.path().to_owned();
        for index in 0..3 {
            recorder
                .append(record(
                    RecordingLane::Speaker,
                    origin,
                    index * 1000,
                    vec![2000; 16000],
                ))
                .await
                .unwrap();
        }
        recorder.checkpoint().await.unwrap();
        assert_eq!(pcm(&path).len(), 16000);
        let (tx, rx) = mpsc::channel(1);
        drop(tx);
        recorder.run(rx).await.unwrap();
        let samples = pcm(&path);
        assert_eq!(samples.len(), 48000);
        assert!(samples.iter().all(|&s| s == 2000));
    }

    #[tokio::test]
    async fn invalid_monotonic_times_are_rejected_without_padding_a_future_file() {
        let directory = tempfile::tempdir().unwrap();
        let origin = Instant::now() - Duration::from_secs(20);
        assert!(
            SessionAudioRecorder::create(
                directory.path(),
                "future-origin",
                Instant::now() + Duration::from_secs(60),
                true,
                false
            )
            .await
            .is_err()
        );
        assert!(
            std::fs::read_dir(directory.path())
                .unwrap()
                .next()
                .is_none()
        );
        for (name, captured_at, error_text) in [
            (
                "before-start",
                origin - Duration::from_millis(1),
                "predates",
            ),
            (
                "future-frame",
                Instant::now() + Duration::from_secs(3600),
                "future",
            ),
        ] {
            let recorder =
                SessionAudioRecorder::create(directory.path(), name, origin, true, false)
                    .await
                    .unwrap();
            let path = recorder.path().to_owned();
            let (tx, rx) = mpsc::channel(1);
            tx.send(AudioRecord {
                lane: RecordingLane::Microphone,
                samples: vec![1000; 1600],
                channels: 1,
                captured_at,
            })
            .await
            .unwrap();
            drop(tx);
            assert!(
                recorder
                    .run(rx)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains(error_text)
            );
            assert_eq!(std::fs::metadata(&path).unwrap().len(), 44);
            assert!(pcm(&path).is_empty());
        }
        let recorder =
            SessionAudioRecorder::create(directory.path(), "backward", origin, true, false)
                .await
                .unwrap();
        let path = recorder.path().to_owned();
        let (tx, rx) = mpsc::channel(2);
        tx.send(record(
            RecordingLane::Microphone,
            origin,
            100,
            vec![1000; 1600],
        ))
        .await
        .unwrap();
        tx.send(record(
            RecordingLane::Microphone,
            origin,
            50,
            vec![1000; 1600],
        ))
        .await
        .unwrap();
        drop(tx);
        assert!(
            recorder
                .run(rx)
                .await
                .unwrap_err()
                .to_string()
                .contains("moved backwards")
        );
        assert_eq!(pcm(&path).len(), 3200);
    }
}
