//! Provider boundary: mono PCM16 at 16 kHz in, mono PCM16 at 24 kHz out.
//!
//! All serialization/network work runs outside audio callbacks. Callers own the
//! bounded channels and cancel a session before changing its configuration.

mod deepgram;
mod gemini;
mod local;
mod openai;
pub mod stt;

use std::sync::Arc;

use anyhow::{Result, bail};
use async_trait::async_trait;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub struct SessionConfig {
    pub model: String,
    /// Name of an environment variable, never an API key itself.
    pub api_key_env: String,
    pub voice: String,
    pub source_language: String,
    pub target_language: String,
    pub prompt: String,
    pub vad_silence_ms: u32,
    pub connect_timeout_secs: u64,
    pub max_reconnect_attempts: u32,
    pub input_transcription: bool,
    pub output_transcription: bool,
}

#[derive(Debug, PartialEq)]
pub enum ProviderEvent {
    Connected,
    Reconnecting {
        attempt: u32,
    },
    /// A recoverable processing limitation. Diagnostics must not contain speech
    /// or credentials; the provider continues and keeps its queues bounded.
    Warning {
        message: String,
    },
    Audio {
        samples: Vec<i16>,
        sample_rate: u32,
    },
    Interrupted,
    Transcript {
        input: bool,
        text: String,
        metadata: TranscriptMetadata,
    },
    TurnComplete,
}

/// Metadata supplied by a provider or aligned to the actual captured segment
/// submitted to finite STT, never inferred from text or receipt time.
/// Current Gemini Live models do not promise speaker IDs or word timestamps;
/// the optional fields preserve actual metadata if it is supplied in a reply.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TranscriptMetadata {
    pub speaker: Option<String>,
    /// Offset from the start of provider-session audio, not wall-clock time.
    pub start_ms: Option<u64>,
    /// Offset from the start of provider-session audio, not wall-clock time.
    pub end_ms: Option<u64>,
    /// Provider alignment point or source segment start; not a word boundary.
    pub alignment_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProviderCapabilities {
    /// Translation proceeds while speech continues, without a turn boundary.
    pub continuous_audio: bool,
    pub custom_prompt: bool,
    pub fixed_voice: bool,
    /// Best-effort preservation performed by the model, not an enrolled clone.
    pub automatic_voice_preservation: bool,
    pub speaker_diarization: bool,
    pub word_timestamps: bool,
    pub voice_enrollment: bool,
}

/// Documented model capabilities, checked against official docs on 2026-09-29.
/// Unknown model IDs conservatively advertise no confirmed capabilities.
pub fn capabilities(model: &str) -> ProviderCapabilities {
    match model.strip_prefix("models/").unwrap_or(model) {
        "gemini-3.5-live-translate-preview" => ProviderCapabilities {
            continuous_audio: true,
            automatic_voice_preservation: true,
            ..ProviderCapabilities::default()
        },
        "gemini-3.8-live" | "gemini-3.8-live-extended-thinking" => ProviderCapabilities {
            custom_prompt: true,
            fixed_voice: true,
            ..ProviderCapabilities::default()
        },
        model if known_model_family(model, "gpt-realtime-translate") => ProviderCapabilities {
            continuous_audio: true,
            ..ProviderCapabilities::default()
        },
        model if known_model_family(model, "gpt-realtime-2.1") => ProviderCapabilities {
            custom_prompt: true,
            fixed_voice: true,
            ..ProviderCapabilities::default()
        },
        _ => ProviderCapabilities::default(),
    }
}

/// Dated snapshots share their known family's protocol; unrelated suffixes do not.
fn known_model_family(model: &str, family: &str) -> bool {
    model == family
        || model
            .strip_prefix(family)
            .and_then(|suffix| suffix.strip_prefix('-'))
            .is_some_and(|date| {
                date.len() == 10
                    && date.bytes().enumerate().all(|(index, byte)| {
                        if matches!(index, 4 | 7) {
                            byte == b'-'
                        } else {
                            byte.is_ascii_digit()
                        }
                    })
                    && chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok()
            })
}

#[async_trait]
pub trait SpeechProvider: Send + Sync {
    fn id(&self) -> &'static str;

    /// Runs until cancellation or an error. An unexpected source-channel EOF is
    /// an error, so the supervisor cannot mistake failed audio capture for success.
    async fn run(
        &self,
        config: SessionConfig,
        audio: Receiver<Vec<i16>>,
        events: Sender<ProviderEvent>,
        cancel: CancellationToken,
    ) -> Result<()>;

    /// Transcribes a finite original-audio stream. Channel EOF flushes pending
    /// speech; success means every submitted segment received a final result.
    /// Input is bounded mono PCM16/16 kHz (at most one second per message).
    /// Offsets refer to this stream, including silence. Never generates audio
    /// or retries an ambiguous request, which could duplicate saved text.
    async fn run_history(
        &self,
        _config: SessionConfig,
        _audio: Receiver<Vec<i16>>,
        _events: Sender<ProviderEvent>,
        _cancel: CancellationToken,
    ) -> Result<()> {
        bail!("this provider does not support finite original-audio transcription")
    }
}

pub fn create_provider(kind: &str) -> Result<Arc<dyn SpeechProvider>> {
    match kind {
        "gemini" => Ok(Arc::new(gemini::GeminiProvider)),
        _ => bail!("unknown speech provider; use a configured gemini, openai or local provider"),
    }
}

pub fn create_configured_provider(
    kind: &str,
    cloud: &crate::config::CloudProviderConfig,
    local: &crate::config::LocalProviderConfig,
) -> Result<Arc<dyn SpeechProvider>> {
    create_route_provider(kind, cloud, local, true)
}

/// Local translation can bypass Piper when a separate streaming TTS supplies the voice.
pub fn create_route_provider(
    kind: &str,
    cloud: &crate::config::CloudProviderConfig,
    local: &crate::config::LocalProviderConfig,
    native_synthesis: bool,
) -> Result<Arc<dyn SpeechProvider>> {
    match kind {
        "openai" => Ok(Arc::new(openai::OpenAiProvider::new(
            cloud.endpoint.clone(),
            cloud.transcription_model.clone(),
        )?)),
        "local" => Ok(Arc::new(if native_synthesis {
            local::LocalProvider::new(local.clone())?
        } else {
            local::LocalProvider::translation(local.clone(), false)?
        })),
        _ => create_provider(kind),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removed_diagnostic_provider_cannot_be_created() {
        let profiles = crate::config::ProviderProfiles::default();
        assert!(create_provider("loopback").is_err());
        assert!(create_configured_provider("loopback", &profiles.gemini, &profiles.local).is_err());
        assert!(
            create_route_provider("loopback", &profiles.gemini, &profiles.local, false).is_err()
        );
    }

    #[test]
    fn openai_catalog_matches_translation_and_conversational_modes() {
        for model in [
            "gpt-realtime-translate",
            "gpt-realtime-translate-2026-09-01",
        ] {
            assert_eq!(
                capabilities(model),
                ProviderCapabilities {
                    continuous_audio: true,
                    ..ProviderCapabilities::default()
                }
            );
        }
        for model in ["gpt-realtime-2.1", "gpt-realtime-2.1-2026-09-01"] {
            assert_eq!(
                capabilities(model),
                ProviderCapabilities {
                    custom_prompt: true,
                    fixed_voice: true,
                    ..ProviderCapabilities::default()
                }
            );
        }
    }

    #[test]
    fn unknown_families_and_unverified_suffixes_do_not_inherit_capabilities() {
        for model in [
            "gpt-realtime-unknown",
            "gpt-realtime-translate-experimental",
            "gpt-realtime-2.1-unverified",
            "gpt-realtime-2.1-2026-02-30",
            "gpt-realtime-2.1-2026-9-1",
            "gpt-realtime-translate-",
        ] {
            assert_eq!(
                capabilities(model),
                ProviderCapabilities::default(),
                "{model}"
            );
        }
    }
}
