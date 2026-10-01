//! Transcripts are written on a separate bounded worker, never on an audio callback.
use crate::{config::TranscriptionConfig, provider::TranscriptMetadata};
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    fs::{self, OpenOptions},
    sync::mpsc,
};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub enum TranscriptRecord {
    Routed {
        origin: TranscriptOrigin,
        record: Box<TranscriptRecord>,
    },
    Text {
        input: bool,
        text: String,
        metadata: TranscriptMetadata,
        received_at: String,
    },
    TurnComplete,
    Gap,
    Section(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TranscriptOrigin {
    Microphone,
    Speaker,
}
impl TranscriptOrigin {
    fn label(self) -> &'static str {
        match self {
            Self::Microphone => "microphone",
            Self::Speaker => "received output",
        }
    }
}

pub struct TranscriptWriter {
    original: TextFile,
}

struct TextFile {
    file: crate::storage::resilient::ResilientFile,
    line_open: bool,
    timestamps: bool,
    speaker: Option<String>,
    origin: Option<TranscriptOrigin>,
}

impl TextFile {
    async fn create(path: &Path, label: &str, timestamps: bool) -> Result<Self> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options
            .open(path)
            .await
            .with_context(|| format!("Could not create the transcript at {}", path.display()))?;
        let mut writer = Self {
            file: crate::storage::resilient::ResilientFile::new(file),
            line_open: false,
            timestamps,
            speaker: None,
            origin: None,
        };
        writer.file.write_all(format!("Babel · {label}\nStarted: {}\nIncremental text supplied by the model; not a reviewed transcript.\n\n", Utc::now().to_rfc3339()).as_bytes()).await?;
        writer.file.flush().await?;
        Ok(writer)
    }
    async fn append(
        &mut self,
        text: &str,
        metadata: &TranscriptMetadata,
        received_at: &str,
    ) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        if metadata.speaker != self.speaker
            || metadata.start_ms.is_some()
            || metadata.alignment_ms.is_some()
        {
            self.newline().await?;
        }
        if !self.line_open {
            if let Some(origin) = self.origin {
                self.file
                    .write_all(format!("[{}] ", origin.label()).as_bytes())
                    .await?;
            }
            if self.timestamps {
                let stamp = if let Some(start) = metadata.start_ms {
                    match metadata.end_ms {
                        Some(end) => format!("[audio +{}–{}s] ", seconds(start), seconds(end)),
                        None => format!("[audio +{}s] ", seconds(start)),
                    }
                } else if let Some(alignment) = metadata.alignment_ms {
                    format!("[alignment +{}s] ", seconds(alignment))
                } else {
                    format!("[received at {received_at}] ")
                };
                self.file.write_all(stamp.as_bytes()).await?;
            }
            if let Some(speaker) = &metadata.speaker {
                self.file
                    .write_all(format!("[speaker {speaker}] ").as_bytes())
                    .await?;
            }
        }
        self.speaker = metadata.speaker.clone();
        self.line_open = true;
        self.file.write_all(text.as_bytes()).await?;
        // Fragments can end in the middle of a decimal, abbreviation or token.
        // Only actual boundaries/metadata start a new line, not punctuation.

        Ok(())
    }
    async fn newline(&mut self) -> Result<()> {
        if self.line_open {
            self.file.write_all(b"\n").await?;
            self.line_open = false;
        }
        Ok(())
    }
    async fn set_origin(&mut self, origin: Option<TranscriptOrigin>) -> Result<()> {
        if self.origin != origin {
            self.newline().await?;
            self.origin = origin;
            self.speaker = None;
        }
        Ok(())
    }
}

fn seconds(milliseconds: u64) -> String {
    format!("{}.{:03}", milliseconds / 1000, milliseconds % 1000)
}

impl TranscriptWriter {
    pub(crate) fn observe_recovery(
        &mut self,
        observer: crate::storage::resilient::RecoveryObserver,
    ) {
        self.original.file.observe(observer);
    }
    pub async fn create_merged(
        config: &TranscriptionConfig,
        stem: &str,
        session: &str,
        name: &str,
    ) -> Result<Self> {
        crate::session::validate_file_stem(stem)?;
        crate::session::validate_name(name)?;
        ensure!(
            !session.is_empty()
                && session.len() <= 128
                && session.chars().all(|c| c.is_alphanumeric() || c == '-'),
            "Invalid session identifier"
        );
        let directory = PathBuf::from(&config.directory);
        fs::create_dir_all(&directory)
            .await
            .context("Could not create the transcript folder")?;
        let label = format!(
            "original microphone and output audio\nSession: {name}\nIdentifier: {session}\nOrder: fragment arrival; labels identify the source, not the person"
        );
        let original = TextFile::create(
            &directory.join(format!("{stem}.txt")),
            &label,
            config.timestamps,
        )
        .await?;
        Ok(Self { original })
    }
    pub async fn create(config: &TranscriptionConfig, route: &str, session: &str) -> Result<Self> {
        Self::create_with_name(config, route, session, None).await
    }
    pub async fn create_with_name(
        config: &TranscriptionConfig,
        route: &str,
        session: &str,
        name: Option<&str>,
    ) -> Result<Self> {
        if let Some(name) = name {
            crate::session::validate_name(name)?;
        }
        ensure!(
            matches!(route, "microphone" | "speaker"),
            "Invalid transcription route"
        );
        ensure!(
            !session.is_empty()
                && session.len() <= 128
                && session.chars().all(|c| c.is_alphanumeric() || c == '-'),
            "Invalid transcription session"
        );
        let directory = PathBuf::from(&config.directory);
        fs::create_dir_all(&directory)
            .await
            .context("Could not create the transcript folder")?;
        let label = match name {
            Some(name) => format!("{route} / original\nSession: {name}\nIdentifier: {session}"),
            None => format!("{route} / original"),
        };
        let original = TextFile::create(
            &directory.join(format!("{session}-{route}-original.txt")),
            &label,
            config.timestamps,
        )
        .await?;
        Ok(Self { original })
    }

    pub async fn run(mut self, mut records: mpsc::Receiver<TranscriptRecord>) -> Result<()> {
        // Batch filesystem flushes instead of dispatching one blocking write
        // per network fragment. BufWriter bounds memory; EOF still syncs data.
        let mut flush = tokio::time::interval(Duration::from_millis(100));
        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let record = tokio::select! {
                record = records.recv() => { let Some(record) = record else { break; }; record },
                _ = flush.tick() => { self.original.file.flush().await?; continue; },
            };
            let (origin, record) = match record {
                TranscriptRecord::Routed { origin, record } => (Some(origin), *record),
                record => (None, record),
            };
            match record {
                TranscriptRecord::Section(title) => {
                    ensure!(title.len() <= 8192, "Transcript section exceeds the limit");
                    self.original.newline().await?;
                    self.original.set_origin(None).await?;
                    self.original
                        .file
                        .write_all(format!("\n[{title}]\n\n").as_bytes())
                        .await?;
                }
                TranscriptRecord::Text {
                    input,
                    text,
                    metadata,
                    received_at,
                } => {
                    if !input {
                        continue;
                    }
                    self.original.set_origin(origin).await?;
                    ensure!(text.len() <= 32768, "Transcript segment exceeds 32 KiB");
                    ensure!(
                        received_at.len() <= 128 && !received_at.chars().any(char::is_control),
                        "Invalid transcript receipt time"
                    );
                    ensure!(
                        metadata
                            .speaker
                            .as_ref()
                            .is_none_or(|speaker| !speaker.is_empty()
                                && speaker.len() <= 128
                                && !speaker.chars().any(char::is_control)),
                        "Invalid transcript speaker identifier"
                    );
                    ensure!(
                        metadata
                            .start_ms
                            .zip(metadata.end_ms)
                            .is_none_or(|(start, end)| end >= start),
                        "Invalid transcript time interval"
                    );
                    self.original.append(&text, &metadata, &received_at).await?;
                }
                TranscriptRecord::TurnComplete => {
                    if self.original.origin == origin {
                        self.original.newline().await?;
                    }
                }
                TranscriptRecord::Gap => {
                    self.original.set_origin(origin).await?;
                    self.original.newline().await?;
                    self.original
                        .append(
                            "[reconnection/interruption: session offsets may restart]",
                            &TranscriptMetadata::default(),
                            &Utc::now().to_rfc3339(),
                        )
                        .await?;
                    self.original.newline().await?;
                }
                TranscriptRecord::Routed { .. } => {
                    anyhow::bail!("Invalid nested transcript source")
                }
            }
        }
        self.original.newline().await?;
        self.original.file.flush().await?;
        self.original.file.sync_data().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn merged_transcript_contains_both_original_lanes_in_one_file() {
        let directory = tempfile::tempdir().unwrap();
        let cfg = TranscriptionConfig {
            directory: directory.path().to_string_lossy().into_owned(),
            timestamps: false,
            ..Default::default()
        };
        let writer =
            TranscriptWriter::create_merged(&cfg, "reunião-1234", "session-1234", "Reunião")
                .await
                .unwrap();
        let (tx, rx) = mpsc::channel(8);
        for (origin, text, input) in [
            (TranscriptOrigin::Microphone, "Olá, equipe.", true),
            (TranscriptOrigin::Speaker, "Hello, team.", true),
            (TranscriptOrigin::Speaker, "TRADUÇÃO NÃO SALVAR", false),
            (TranscriptOrigin::Microphone, " Como estão?", true),
        ] {
            tx.send(TranscriptRecord::Routed {
                origin,
                record: Box::new(TranscriptRecord::Text {
                    input,
                    text: text.into(),
                    metadata: TranscriptMetadata::default(),
                    received_at: "2026-09-29T12:00:00Z".into(),
                }),
            })
            .await
            .unwrap();
        }
        drop(tx);
        writer.run(rx).await.unwrap();
        let text = fs::read_to_string(directory.path().join("reunião-1234.txt"))
            .await
            .unwrap();
        assert!(text.contains(
            "[microphone] Olá, equipe.\n[received output] Hello, team.\n[microphone]  Como estão?"
        ));
        assert!(!text.contains("TRADUÇÃO NÃO SALVAR"));
        assert!(!text.contains("[received at "));
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
    #[tokio::test]
    async fn preserves_original_fragments_and_never_saves_translated_text() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = TranscriptionConfig {
            enabled: true,
            directory: dir.path().to_string_lossy().into_owned(),
            ..Default::default()
        };
        let writer = TranscriptWriter::create(&cfg, "microphone", "session-1")
            .await
            .unwrap();
        let (tx, rx) = mpsc::channel(8);
        tx.send(TranscriptRecord::Text {
            input: true,
            text: "Olá".into(),
            metadata: TranscriptMetadata::default(),
            received_at: "test-time".into(),
        })
        .await
        .unwrap();
        tx.send(TranscriptRecord::Text {
            input: true,
            text: " mundo".into(),
            metadata: TranscriptMetadata::default(),
            received_at: "test-time".into(),
        })
        .await
        .unwrap();
        tx.send(TranscriptRecord::Text {
            input: false,
            text: "Hello".into(),
            metadata: TranscriptMetadata::default(),
            received_at: "test-time".into(),
        })
        .await
        .unwrap();
        tx.send(TranscriptRecord::TurnComplete).await.unwrap();
        drop(tx);
        writer.run(rx).await.unwrap();
        let text = fs::read_to_string(dir.path().join("session-1-microphone-original.txt"))
            .await
            .unwrap();
        assert!(text.contains("Olá mundo\n"));
        assert!(!text.contains("Hello"));
        assert!(!dir.path().join("session-1-microphone-output.txt").exists());
        assert!(
            TranscriptWriter::create(&cfg, "microphone", "session-1")
                .await
                .is_err(),
            "must not overwrite a previous session"
        );
    }
    #[tokio::test]
    async fn unwritable_destination_fails_before_recording() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let cfg = TranscriptionConfig {
            enabled: true,
            directory: file.path().to_string_lossy().into_owned(),
            ..Default::default()
        };
        assert!(
            TranscriptWriter::create(&cfg, "speaker", "session-1")
                .await
                .is_err()
        );
    }
    async fn save_records(records: Vec<TranscriptRecord>, timestamps: bool) -> String {
        let dir = tempfile::tempdir().unwrap();
        let cfg = TranscriptionConfig {
            enabled: true,
            timestamps,
            directory: dir.path().to_string_lossy().into_owned(),
            ..Default::default()
        };
        let writer = TranscriptWriter::create(&cfg, "speaker", "metadata-test")
            .await
            .unwrap();
        let (tx, rx) = mpsc::channel(records.len().max(1));
        for record in records {
            tx.send(record).await.unwrap();
        }
        drop(tx);
        writer.run(rx).await.unwrap();
        fs::read_to_string(dir.path().join("metadata-test-speaker-original.txt"))
            .await
            .unwrap()
    }
    fn original(text: &str, metadata: TranscriptMetadata) -> TranscriptRecord {
        TranscriptRecord::Text {
            input: true,
            text: text.into(),
            metadata,
            received_at: "2026-09-29T00:00:00Z".into(),
        }
    }
    #[tokio::test]
    async fn punctuation_inside_fragments_does_not_corrupt_numbers_or_spacing() {
        let text = save_records(
            vec![
                original("Versão 1.", TranscriptMetadata::default()),
                original("23: U.", TranscriptMetadata::default()),
                original("S. mantém o texto", TranscriptMetadata::default()),
                TranscriptRecord::TurnComplete,
            ],
            false,
        )
        .await;
        assert!(text.ends_with("Versão 1.23: U.S. mantém o texto\n"));
        assert!(!text.contains("[received at ") && !text.contains("[speaker"));
    }
    #[tokio::test]
    async fn authentic_speakers_and_timestamp_sources_remain_distinct() {
        let text = save_records(
            vec![
                original(
                    "áudio",
                    TranscriptMetadata {
                        speaker: Some("speaker-a".into()),
                        start_ms: Some(1250),
                        end_ms: Some(1875),
                        alignment_ms: Some(1900),
                    },
                ),
                original(
                    "alinhado",
                    TranscriptMetadata {
                        alignment_ms: Some(2500),
                        ..Default::default()
                    },
                ),
                TranscriptRecord::TurnComplete,
                original("recebimento", TranscriptMetadata::default()),
                TranscriptRecord::Gap,
                original(
                    "sessão nova",
                    TranscriptMetadata {
                        start_ms: Some(10),
                        ..Default::default()
                    },
                ),
            ],
            true,
        )
        .await;
        assert!(text.contains("[audio +1.250–1.875s] [speaker speaker-a] áudio\n"));
        assert!(text.contains("[alignment +2.500s] alinhado\n"));
        assert!(text.contains("[received at 2026-09-29T00:00:00Z] recebimento\n"));
        assert_eq!(text.matches("[speaker").count(), 1);
        assert!(text.contains("session offsets may restart"));
        assert!(text.ends_with("[audio +0.010s] sessão nova\n"));
    }
    #[tokio::test]
    async fn timestamp_toggle_does_not_remove_real_speaker_labels() {
        let text = save_records(
            vec![original(
                "fala",
                TranscriptMetadata {
                    speaker: Some("42".into()),
                    start_ms: Some(1000),
                    end_ms: Some(2000),
                    alignment_ms: None,
                },
            )],
            false,
        )
        .await;
        assert!(text.ends_with("[speaker 42] fala\n"));
        assert!(
            !text.contains("[audio")
                && !text.contains("[received at ")
                && !text.contains("[alignment")
        );
    }
    #[test]
    fn timestamp_format_does_not_round_large_millisecond_offsets() {
        assert_eq!(seconds(5), "0.005");
        assert_eq!(seconds(u64::MAX), "18446744073709551.615");
    }
    #[tokio::test]
    async fn invalid_metadata_and_unsafe_session_names_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = TranscriptionConfig {
            directory: dir.path().to_string_lossy().into_owned(),
            ..Default::default()
        };
        for session in ["", "../outside", "folder/name"] {
            assert!(
                TranscriptWriter::create(&cfg, "speaker", session)
                    .await
                    .is_err()
            );
        }
        for (i, metadata) in [
            TranscriptMetadata {
                speaker: Some("injected\nheader".into()),
                ..Default::default()
            },
            TranscriptMetadata {
                start_ms: Some(2000),
                end_ms: Some(1000),
                ..Default::default()
            },
        ]
        .into_iter()
        .enumerate()
        {
            let writer = TranscriptWriter::create(&cfg, "speaker", &format!("invalid-{i}"))
                .await
                .unwrap();
            let (tx, rx) = mpsc::channel(1);
            tx.send(original("test", metadata)).await.unwrap();
            drop(tx);
            assert!(writer.run(rx).await.is_err());
        }
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn transcript_file_is_private_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let cfg = TranscriptionConfig {
            directory: dir.path().to_string_lossy().into_owned(),
            ..Default::default()
        };
        let _writer = TranscriptWriter::create(&cfg, "speaker", "private")
            .await
            .unwrap();
        let permissions = fs::metadata(dir.path().join("private-speaker-original.txt"))
            .await
            .unwrap()
            .permissions();
        assert_eq!(permissions.mode() & 0o777, 0o600);
    }
    #[tokio::test]
    async fn named_session_keeps_title_and_safe_filename_with_timestamps_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = TranscriptionConfig {
            directory: dir.path().to_string_lossy().into_owned(),
            timestamps: false,
            ..Default::default()
        };
        let session = crate::session::SessionIdentity::new(Some("Reunião João / cliente")).unwrap();
        let writer = TranscriptWriter::create_with_name(
            &cfg,
            "microphone",
            &session.id,
            Some(&session.name),
        )
        .await
        .unwrap();
        let (tx, rx) = mpsc::channel(1);
        tx.send(original("Olá", TranscriptMetadata::default()))
            .await
            .unwrap();
        drop(tx);
        writer.run(rx).await.unwrap();
        let text = fs::read_to_string(
            dir.path()
                .join(format!("{}-microphone-original.txt", session.id)),
        )
        .await
        .unwrap();
        assert!(text.contains("Session: Reunião João / cliente\n"));
        assert!(text.contains(&format!("Identifier: {}\n", session.id)));
        assert!(text.contains("Olá"));
        assert!(!text.contains("[received at "));
        assert!(
            TranscriptWriter::create_with_name(&cfg, "speaker", "valid-id", Some("Título\nfalso"))
                .await
                .is_err()
        );
    }
}
