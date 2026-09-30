//! Dedicated original-audio recognition. No translation/voice profile crosses
//! this boundary, including when STS and STT run concurrently on the same route.
use std::sync::Arc;

use anyhow::{Result, bail, ensure};

use super::{SessionConfig, SpeechProvider, deepgram, gemini, local, openai};
use crate::config::{LocalProviderConfig, SttProviderProfiles, SttRouteConfig};

pub fn create(
    route: &SttRouteConfig,
    profiles: &SttProviderProfiles,
) -> Result<Arc<dyn SpeechProvider>> {
    profiles.validate_selected(route)?;
    match route.provider.as_str() {
        "gemini" => {
            let model = profiles
                .gemini
                .model
                .strip_prefix("models/")
                .unwrap_or(&profiles.gemini.model);
            ensure!(
                model == gemini::TRANSCRIBE_MODEL,
                "Gemini ASR requires gemini-3.5-transcribe-live"
            );
            Ok(Arc::new(gemini::GeminiTranscriptionProvider {
                model: model.into(),
            }))
        }
        "openai" => {
            ensure!(
                !super::known_model_family(&profiles.openai.model, "gpt-realtime-whisper")
                    || !route.language.eq_ignore_ascii_case("auto"),
                "OpenAI gpt-realtime-whisper STT requires an explicit source language"
            );
            Ok(Arc::new(openai::OpenAiProvider::transcription(
                profiles.openai.endpoint.clone(),
                profiles.openai.model.clone(),
            )?))
        }
        "deepgram" => {
            let recognizer = deepgram::DeepgramProvider::new(profiles.deepgram.clone())?;
            recognizer.url(&route.language)?;
            Ok(Arc::new(recognizer))
        }
        "whisper" => {
            let profile = &profiles.whisper;
            let provider = local::LocalProvider::transcription(LocalProviderConfig {
                whisper_endpoint: profile.endpoint.clone(),
                whisper_model: profile.model.clone(),
                ollama_endpoint: String::new(),
                translation_api: "ollama".into(),
                piper_endpoint: String::new(),
                translation_model: String::new(),
                piper_voice: String::new(),
                segment_ms: profile.segment_ms,
                silence_ms: profile.silence_ms,
                vad_threshold: profile.vad_threshold,
                request_timeout_secs: profile.request_timeout_secs,
            })?
            .with_whisper_auth(if profile.endpoint == "auto" {
                String::new()
            } else {
                profile.api_key_env.clone()
            });
            Ok(Arc::new(provider))
        }
        _ => bail!("unknown STT provider"),
    }
}

pub fn session_config(
    route: &SttRouteConfig,
    profiles: &SttProviderProfiles,
) -> Result<SessionConfig> {
    profiles.validate_selected(route)?;
    let (model, timeout, retries) = match route.provider.as_str() {
        "gemini" => (
            &profiles.gemini.model,
            profiles.gemini.connect_timeout_secs,
            profiles.gemini.max_reconnect_attempts,
        ),
        "openai" => (
            &profiles.openai.model,
            profiles.openai.connect_timeout_secs,
            profiles.openai.max_reconnect_attempts,
        ),
        "deepgram" => (
            &profiles.deepgram.model,
            profiles.deepgram.connect_timeout_secs,
            profiles.deepgram.max_reconnect_attempts,
        ),
        "whisper" => (
            &profiles.whisper.endpoint,
            profiles.whisper.request_timeout_secs,
            0,
        ),
        _ => bail!("unknown STT provider"),
    };
    Ok(SessionConfig {
        model: if route.provider == "whisper" {
            String::new()
        } else {
            model.clone()
        },
        api_key_env: profiles
            .api_key_env(&route.provider)
            .unwrap_or_default()
            .into(),
        source_language: route.language.clone(),
        target_language: String::new(),
        prompt: String::new(),
        voice: String::new(),
        vad_silence_ms: if route.provider == "whisper" {
            profiles.whisper.silence_ms
        } else {
            400
        },
        connect_timeout_secs: timeout,
        max_reconnect_attempts: retries,
        input_transcription: true,
        output_transcription: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognition_uses_only_its_own_profile() {
        let mut profiles = SttProviderProfiles::default();
        profiles.openai.api_key_env = "SEPARATE_STT_KEY".into();
        profiles.whisper.endpoint = "http://127.0.0.1:12345/inference".into();
        for kind in ["gemini", "openai", "deepgram", "whisper"] {
            let route = SttRouteConfig {
                provider: kind.into(),
                language: "pt-BR".into(),
            };
            create(&route, &profiles).unwrap();
            let config = session_config(&route, &profiles).unwrap();
            assert_eq!(config.source_language, "pt-BR");
            assert!(
                config.target_language.is_empty()
                    && config.prompt.is_empty()
                    && config.voice.is_empty()
            );
            assert!(config.input_transcription && !config.output_transcription);
            if kind == "openai" {
                assert_eq!(config.api_key_env, "SEPARATE_STT_KEY");
            }
            if kind == "whisper" {
                assert!(config.api_key_env.is_empty());
            }
        }
    }

    #[test]
    fn translation_models_and_endpoints_cannot_be_used_as_stt() {
        let mut profiles = SttProviderProfiles::default();
        profiles.gemini.model = "gemini-3.8-live".into();
        assert!(create(&SttRouteConfig::default(), &profiles).is_err());
        let route = SttRouteConfig {
            provider: "openai".into(),
            ..SttRouteConfig::default()
        };
        profiles.openai.model = "gpt-realtime-translate".into();
        assert!(create(&route, &profiles).is_err());
        profiles.openai.model = "gpt-live-transcribe".into();
        profiles.openai.endpoint = "wss://api.openai.com/v1/realtime/translations".into();
        assert!(create(&route, &profiles).is_err());
    }

    #[test]
    fn incompatible_model_language_is_rejected_before_starting_audio() {
        let mut profiles = SttProviderProfiles::default();
        profiles.deepgram.model = "nova-2-phonecall".into();
        let mut route = SttRouteConfig {
            provider: "deepgram".into(),
            ..SttRouteConfig::default()
        };
        assert!(create(&route, &profiles).is_err());
        route.language = "en".into();
        assert!(create(&route, &profiles).is_ok());
        route.provider = "openai".into();
        profiles.openai.model = "gpt-realtime-whisper".into();
        assert!(create(&route, &profiles).is_ok());
        route.language = "auto".into();
        assert!(create(&route, &profiles).is_err());
    }
}
