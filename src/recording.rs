//! One bounded, mixed recording of the two original capture streams.
//! Input is mono PCM16/16 kHz after device resampling, before translation/gain.
use anyhow::{Context, Result, ensure};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::{
    fs::{self, File, OpenOptions},
    io::{AsyncSeekExt, AsyncWriteExt, BufWriter},
    sync::mpsc,
};

const SAMPLE_RATE: u64 = 16_000;
const HOLDBACK_SAMPLES: u64 = SAMPLE_RATE * 2;
const JITTER_SAMPLES: u64 = SAMPLE_RATE / 20; // 50 ms scheduler jitter.
const MAX_FRAME_SAMPLES: usize = SAMPLE_RATE as usize;
const MAX_WAV_SAMPLES: u64 = (u32::MAX as u64 - 36) / 2;
const WRITE_SAMPLES: usize = 4096;

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
    file: BufWriter<File>,
    origin: Instant,
    active: [bool; 2],
    divisor: i32,
    clocks: [LaneClock; 2],
    /// The sum for samples [written, written+pending.len()). Both lanes add here.
    pending: VecDeque<i32>,
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
        ensure!(
            microphone || speaker,
            "Selecione ao menos uma entrada para gravar"
        );
        ensure!(
            origin <= Instant::now(),
            "Relógio de início da gravação está no futuro"
        );
        crate::session::validate_file_stem(stem)?;
        fs::create_dir_all(directory)
            .await
            .context("Não foi possível criar a pasta de gravações")?;
        let path = directory.join(format!("{stem}.wav"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options.open(&path).await.with_context(|| {
            format!(
                "Não foi possível criar a gravação em {}; nenhum arquivo existente é sobrescrito",
                path.display()
            )
        })?;
        let mut file = BufWriter::with_capacity(64 * 1024, file);
        file.write_all(&wav_header(0)?).await?;
        file.flush().await?;
        Ok(Self {
            path,
            file,
            origin,
            active: [microphone, speaker],
            divisor: if microphone && speaker { 2 } else { 1 },
            clocks: Default::default(),
            pending: VecDeque::with_capacity((HOLDBACK_SAMPLES + SAMPLE_RATE) as usize),
            written: 0,
            latest_end: 0,
            bytes: Vec::with_capacity(WRITE_SAMPLES * 2),
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Supervisor closes all senders after capture stops. EOF drains queued
    /// frames and finalizes the WAV; cancelling this worker early loses its tail.
    pub async fn run(mut self, mut records: mpsc::Receiver<AudioRecord>) -> Result<()> {
        let result = self.receive(&mut records).await;
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
            !record.samples.is_empty() && record.samples.len() <= MAX_FRAME_SAMPLES,
            "Quadro de gravação inválido; máximo de 1 segundo PCM16 a 16 kHz"
        );
        ensure!(
            record.captured_at <= Instant::now(),
            "Quadro de gravação com horário de captura no futuro"
        );
        let elapsed = record
            .captured_at
            .checked_duration_since(self.origin)
            .context("Quadro de gravação anterior ao início da sessão")?;
        ensure!(
            self.clocks[lane]
                .last_capture
                .is_none_or(|previous| record.captured_at >= previous),
            "Relógio da captura de gravação retrocedeu"
        );
        let end_by_clock = (elapsed.as_nanos() * u128::from(SAMPLE_RATE) / 1_000_000_000)
            .min(u128::from(u64::MAX)) as u64;
        let candidate = end_by_clock.saturating_sub(record.samples.len() as u64);
        let start = match self.clocks[lane].next_sample {
            Some(expected) if candidate <= expected.saturating_add(JITTER_SAMPLES) => expected,
            _ => candidate,
        };
        let end = start
            .checked_add(record.samples.len() as u64)
            .context("Duração da gravação excedida")?;
        ensure!(
            end <= MAX_WAV_SAMPLES,
            "Gravação atingiu o limite WAV RIFF de aproximadamente 37 horas; inicie uma nova sessão"
        );
        ensure!(
            start >= self.written,
            "Uma captura chegou mais de 2 segundos atrasada para a gravação; a sessão foi interrompida para evitar perda silenciosa"
        );
        let latest = self.latest_end.max(end);
        self.flush_until(latest.saturating_sub(HOLDBACK_SAMPLES))
            .await?;
        let needed = end.saturating_sub(self.written) as usize;
        ensure!(
            needed <= (HOLDBACK_SAMPLES + SAMPLE_RATE) as usize,
            "Janela de mixagem excedeu o limite de memória"
        );
        if self.pending.len() < needed {
            self.pending.resize(needed, 0);
        }
        let offset = (start - self.written) as usize;
        for (index, sample) in record.samples.into_iter().enumerate() {
            self.pending[offset + index] += i32::from(sample);
        }
        self.latest_end = latest;
        self.clocks[lane].next_sample = Some(end);
        self.clocks[lane].last_capture = Some(record.captured_at);
        Ok(())
    }
    async fn flush_until(&mut self, end: u64) -> Result<()> {
        ensure!(
            end <= MAX_WAV_SAMPLES,
            "Duração máxima do arquivo WAV excedida"
        );
        while self.written < end {
            let count = (end - self.written).min(WRITE_SAMPLES as u64) as usize;
            self.bytes.clear();
            for _ in 0..count {
                let mixed = self.pending.pop_front().unwrap_or(0) / self.divisor;
                let sample = mixed.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
                self.bytes.extend_from_slice(&sample.to_le_bytes());
            }
            self.file
                .write_all(&self.bytes)
                .await
                .context("Falha ao escrever áudio da sessão")?;
            self.written += count as u64;
        }
        Ok(())
    }
    async fn checkpoint(&mut self) -> Result<()> {
        self.file
            .flush()
            .await
            .context("Falha ao descarregar a gravação")?;
        self.file.seek(std::io::SeekFrom::Start(0)).await?;
        self.file.write_all(&wav_header(self.written)?).await?;
        self.file.flush().await?;
        self.file
            .seek(std::io::SeekFrom::Start(44 + self.written * 2))
            .await?;
        Ok(())
    }
    async fn finalize(&mut self) -> Result<()> {
        self.flush_until(self.latest_end).await?;
        self.checkpoint().await?;
        self.file
            .get_ref()
            .sync_data()
            .await
            .context("Falha ao finalizar a gravação em disco")
    }
}
fn wav_header(samples: u64) -> Result<[u8; 44]> {
    ensure!(
        samples <= MAX_WAV_SAMPLES,
        "Duração máxima do arquivo WAV excedida"
    );
    let data_bytes = (samples * 2) as u32;
    let mut header = [0_u8; 44];
    header[..4].copy_from_slice(b"RIFF");
    header[4..8].copy_from_slice(&(data_bytes + 36).to_le_bytes());
    header[8..16].copy_from_slice(b"WAVEfmt ");
    header[16..20].copy_from_slice(&16_u32.to_le_bytes());
    header[20..22].copy_from_slice(&1_u16.to_le_bytes());
    header[22..24].copy_from_slice(&1_u16.to_le_bytes());
    header[24..28].copy_from_slice(&(SAMPLE_RATE as u32).to_le_bytes());
    header[28..32].copy_from_slice(&(SAMPLE_RATE as u32 * 2).to_le_bytes());
    header[32..34].copy_from_slice(&2_u16.to_le_bytes());
    header[34..36].copy_from_slice(&16_u16.to_le_bytes());
    header[36..40].copy_from_slice(b"data");
    header[40..44].copy_from_slice(&data_bytes.to_le_bytes());
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
        assert!(recorder.bytes.capacity() <= WRITE_SAMPLES * 2);
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
                .contains("atrasada")
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
                .contains("máximo")
        );
        assert!(wav_header(MAX_WAV_SAMPLES + 1).is_err());
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
                "anterior",
            ),
            (
                "future-frame",
                Instant::now() + Duration::from_secs(3600),
                "futuro",
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
                .contains("retrocedeu")
        );
        assert_eq!(pcm(&path).len(), 3200);
    }
}
