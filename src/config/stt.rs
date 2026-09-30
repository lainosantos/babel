//! Original speech recognition configuration, independent of translation/voices.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use super::{AppConfig, CloudProviderConfig, GEMINI_ENDPOINT};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SttRouteConfig {
    pub provider: String,
    pub language: String,
}
impl Default for SttRouteConfig {
    fn default() -> Self {
        Self {
            provider: "gemini".into(),
            language: "auto".into(),
        }
    }
}
impl SttRouteConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            matches!(
                self.provider.as_str(),
                "gemini" | "openai" | "deepgram" | "whisper"
            ),
            "Unknown transcription provider"
        );
        ensure!(
            !self.language.is_empty()
                && self.language.len() <= 35
                && self
                    .language
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
            "Invalid transcription language; use auto or a code such as pt-BR"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CloudSttConfig {
    pub api_key_env: String,
    pub endpoint: String,
    pub model: String,
    pub connect_timeout_secs: u64,
    pub max_reconnect_attempts: u32,
}
impl Default for CloudSttConfig {
    fn default() -> Self {
        Self {
            api_key_env: "GEMINI_API_KEY".into(),
            endpoint: GEMINI_ENDPOINT.into(),
            model: "gemini-3.5-transcribe-live".into(),
            connect_timeout_secs: 15,
            max_reconnect_attempts: 5,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DeepgramSttConfig {
    pub api_key_env: String,
    pub endpoint: String,
    pub model: String,
    pub connect_timeout_secs: u64,
    pub max_reconnect_attempts: u32,
    pub diarize: bool,
    pub punctuate: bool,
}
impl Default for DeepgramSttConfig {
    fn default() -> Self {
        Self {
            api_key_env: "DEEPGRAM_API_KEY".into(),
            endpoint: "wss://api.deepgram.com/v1/listen".into(),
            model: "nova-3".into(),
            connect_timeout_secs: 15,
            max_reconnect_attempts: 3,
            diarize: true,
            punctuate: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WhisperSttConfig {
    pub endpoint: String,
    pub model: String,
    pub api_key_env: String,
    pub segment_ms: u32,
    pub silence_ms: u32,
    pub vad_threshold: f32,
    pub request_timeout_secs: u64,
}
impl Default for WhisperSttConfig {
    fn default() -> Self {
        Self {
            endpoint: "auto".into(),
            model: super::DEFAULT_WHISPER_MODEL.into(),
            api_key_env: String::new(),
            segment_ms: 2000,
            silence_ms: 300,
            vad_threshold: 0.01,
            request_timeout_secs: 30,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SttProviderProfiles {
    pub gemini: CloudSttConfig,
    #[serde(deserialize_with = "deserialize_openai")]
    pub openai: CloudSttConfig,
    pub deepgram: DeepgramSttConfig,
    pub whisper: WhisperSttConfig,
}

fn deserialize_openai<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<CloudSttConfig, D::Error> {
    // A partial [transcription.providers.openai] table needs OpenAI defaults,
    // not CloudSttConfig's Gemini defaults.
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Fields {
        api_key_env: Option<String>,
        endpoint: Option<String>,
        model: Option<String>,
        connect_timeout_secs: Option<u64>,
        max_reconnect_attempts: Option<u32>,
    }
    let fields = Fields::deserialize(deserializer)?;
    let defaults = SttProviderProfiles::default().openai;
    Ok(CloudSttConfig {
        api_key_env: fields.api_key_env.unwrap_or(defaults.api_key_env),
        endpoint: fields.endpoint.unwrap_or(defaults.endpoint),
        model: fields.model.unwrap_or(defaults.model),
        connect_timeout_secs: fields
            .connect_timeout_secs
            .unwrap_or(defaults.connect_timeout_secs),
        max_reconnect_attempts: fields
            .max_reconnect_attempts
            .unwrap_or(defaults.max_reconnect_attempts),
    })
}
impl Default for SttProviderProfiles {
    fn default() -> Self {
        Self {
            gemini: CloudSttConfig::default(),
            openai: CloudSttConfig {
                api_key_env: "OPENAI_API_KEY".into(),
                endpoint: String::new(),
                model: "gpt-live-transcribe".into(),
                ..CloudSttConfig::default()
            },
            deepgram: DeepgramSttConfig::default(),
            whisper: WhisperSttConfig::default(),
        }
    }
}
impl SttProviderProfiles {
    pub fn api_key_env(&self, provider: &str) -> Option<&str> {
        match provider {
            "gemini" => Some(&self.gemini.api_key_env),
            "openai" => Some(&self.openai.api_key_env),
            "deepgram" => Some(&self.deepgram.api_key_env),
            "whisper"
                if self.whisper.endpoint != "auto" && !self.whisper.api_key_env.is_empty() =>
            {
                Some(&self.whisper.api_key_env)
            }
            _ => None,
        }
    }

    pub fn validate_selected(&self, route: &SttRouteConfig) -> Result<()> {
        route.validate()?;
        match route.provider.as_str() {
            "gemini" | "openai" => {
                let profile = if route.provider == "gemini" {
                    &self.gemini
                } else {
                    &self.openai
                };
                validate_cloud(
                    &profile.api_key_env,
                    &profile.model,
                    profile.connect_timeout_secs,
                    profile.max_reconnect_attempts,
                )?;
                if route.provider == "gemini" {
                    ensure!(
                        profile.endpoint == GEMINI_ENDPOINT,
                        "The Gemini STT adapter uses the fixed official endpoint"
                    );
                } else if !profile.endpoint.is_empty() {
                    let url = validate_endpoint(&profile.endpoint, true)?;
                    ensure!(
                        !url.path().trim_end_matches('/').ends_with("/translations"),
                        "OpenAI STT requires a recognition endpoint; configure /realtime instead of /translations"
                    );
                }
            }
            "deepgram" => {
                let profile = &self.deepgram;
                validate_cloud(
                    &profile.api_key_env,
                    &profile.model,
                    profile.connect_timeout_secs,
                    profile.max_reconnect_attempts,
                )?;
                validate_endpoint(&profile.endpoint, true)?;
            }
            "whisper" => {
                let profile = &self.whisper;
                if profile.endpoint == "auto" {
                    ensure!(
                        super::is_managed_whisper_model(&profile.model),
                        "Unknown managed Whisper model"
                    );
                } else {
                    validate_endpoint(&profile.endpoint, false)?;
                }
                if !profile.api_key_env.is_empty() {
                    validate_key_name(&profile.api_key_env)?;
                }
                ensure!(
                    (500..=10_000).contains(&profile.segment_ms),
                    "Whisper STT segment: 500 to 10000 ms"
                );
                ensure!(
                    (100..=2000).contains(&profile.silence_ms)
                        && profile.silence_ms < profile.segment_ms,
                    "Whisper STT silence: 100 to 2000 ms, shorter than the segment"
                );
                ensure!(
                    profile.vad_threshold.is_finite()
                        && (0.0001..=0.5).contains(&profile.vad_threshold),
                    "Whisper STT threshold: 0.0001 to 0.5 RMS"
                );
                ensure!(
                    (1..=120).contains(&profile.request_timeout_secs),
                    "Whisper STT timeout: 1 to 120 seconds"
                );
            }
            _ => unreachable!("validated STT provider"),
        }
        Ok(())
    }
}

fn validate_key_name(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 128
            && !value.as_bytes()[0].is_ascii_digit()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
        "Invalid environment variable name for STT"
    );
    Ok(())
}
fn validate_cloud(key: &str, model: &str, timeout: u64, retries: u32) -> Result<()> {
    validate_key_name(key)?;
    ensure!(
        !model.is_empty()
            && model.len() <= 128
            && model
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-._/".contains(&byte)),
        "Invalid STT model identifier"
    );
    ensure!(
        (1..=120).contains(&timeout),
        "STT timeout: 1 to 120 seconds"
    );
    ensure!(retries <= 20, "At most 20 reconnections for STT");
    Ok(())
}
fn validate_endpoint(endpoint: &str, websocket: bool) -> Result<reqwest::Url> {
    ensure!(endpoint.len() <= 2048, "STT endpoint exceeds 2048 bytes");
    let url = reqwest::Url::parse(endpoint).map_err(|_| anyhow::anyhow!("Invalid STT endpoint"))?;
    let host = url.host_str().unwrap_or("");
    let local = host == "localhost"
        || host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    let secure = if websocket { "wss" } else { "https" };
    let cleartext = if websocket { "ws" } else { "http" };
    ensure!(
        !host.is_empty()
            && (url.scheme() == secure || (local && url.scheme() == cleartext))
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "STT endpoints require a secure connection, except on localhost, with no credentials, query or fragment"
    );
    Ok(url)
}

fn legacy_cloud(profile: &CloudProviderConfig, default: &CloudSttConfig) -> CloudSttConfig {
    CloudSttConfig {
        api_key_env: profile.api_key_env.clone(),
        endpoint: profile.endpoint.clone(),
        model: if profile.transcription_model.is_empty() {
            default.model.clone()
        } else {
            profile.transcription_model.clone()
        },
        connect_timeout_secs: profile.connect_timeout_secs,
        max_reconnect_attempts: profile.max_reconnect_attempts,
    }
}

/// Migrate only absent disk fields. API deserialization intentionally skips this.
pub(super) fn migrate(config: &mut AppConfig, document: &toml::Value) -> Result<bool> {
    let stored = document.get("transcription");
    let mut changed = false;
    for (field, route, recognition) in [
        (
            "microphone_recognition",
            &config.microphone,
            &mut config.transcription.microphone_recognition,
        ),
        (
            "speaker_recognition",
            &config.speaker,
            &mut config.transcription.speaker_recognition,
        ),
    ] {
        if stored.and_then(|value| value.get(field)).is_none() {
            recognition.provider = match route.provider.as_str() {
                "local" => "whisper",
                provider => provider,
            }
            .into();
            recognition.language.clone_from(&route.source_language);
            changed = true;
        }
    }
    let profiles = stored.and_then(|value| value.get("providers"));
    if profiles.and_then(|value| value.get("gemini")).is_none() {
        config.transcription.providers.gemini =
            legacy_cloud(&config.providers.gemini, &CloudSttConfig::default());
        changed = true;
    }
    if profiles.and_then(|value| value.get("openai")).is_none() {
        let mut profile = legacy_cloud(
            &config.providers.openai,
            &SttProviderProfiles::default().openai,
        );
        // Normalize only the known official address. Preserve custom drafts
        // without guessing a new destination or blocking unrelated features.
        // If selected, validate_selected rejects an incompatible STT endpoint.
        if let Ok(mut url) = validate_endpoint(&profile.endpoint, true)
            && url.scheme() == "wss"
            && url.host_str() == Some("api.openai.com")
            && url.port().is_none()
            && url.path().trim_end_matches('/') == "/v1/realtime/translations"
        {
            url.set_path("/v1/realtime");
            profile.endpoint = url.to_string();
        }
        config.transcription.providers.openai = profile;
        changed = true;
    }
    if profiles.and_then(|value| value.get("whisper")).is_none() {
        let local = &config.providers.local;
        config.transcription.providers.whisper = WhisperSttConfig {
            endpoint: if document
                .get("providers")
                .and_then(|value| value.get("local"))
                .and_then(|value| value.get("whisper_endpoint"))
                .is_some()
            {
                local.whisper_endpoint.clone()
            } else {
                "auto".into()
            },
            segment_ms: local.segment_ms,
            silence_ms: local.silence_ms,
            vad_threshold: local.vad_threshold,
            request_timeout_secs: local.request_timeout_secs,
            ..WhisperSttConfig::default()
        };
        changed = true;
    }
    if profiles.and_then(|value| value.get("deepgram")).is_none() {
        changed = true;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn legacy_document(config: &AppConfig) -> toml::Value {
        let mut document = toml::Value::try_from(config).unwrap();
        let table = document
            .get_mut("transcription")
            .unwrap()
            .as_table_mut()
            .unwrap();
        for key in ["microphone_recognition", "speaker_recognition", "providers"] {
            table.remove(key);
        }
        document
    }

    fn write(path: &std::path::Path, document: &toml::Value) {
        fs::write(path, toml::to_string(document).unwrap()).unwrap();
    }

    #[test]
    fn defaults_and_api_values_do_not_inherit_translation_settings() {
        let mut config = AppConfig::default();
        config.microphone.provider = "openai".into();
        config.microphone.source_language = "es-ES".into();
        let api: AppConfig =
            serde_json::from_value(serde_json::to_value(legacy_document(&config)).unwrap())
                .unwrap();
        assert_eq!(api.transcription.microphone_recognition.provider, "gemini");
        assert_eq!(api.transcription.microphone_recognition.language, "auto");
        assert_eq!(api.transcription.providers.whisper.endpoint, "auto");
        let partial: AppConfig =
            toml::from_str("[transcription.providers.openai]\napi_key_env = 'MY_ASR_KEY'\n")
                .unwrap();
        assert_eq!(
            partial.transcription.providers.openai.model,
            "gpt-live-transcribe"
        );
        assert!(partial.transcription.providers.openai.endpoint.is_empty());
        assert_eq!(
            partial.transcription.providers.openai.api_key_env,
            "MY_ASR_KEY"
        );
    }

    #[test]
    fn legacy_recognition_is_migrated_once_without_changing_recording_or_cloud_supplier() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy.toml");
        let mut config = AppConfig::default();
        config.microphone.provider = "local".into();
        config.microphone.enabled = false;
        config.microphone.source_language = "fr-FR".into();
        config.speaker.provider = "openai".into();
        config.speaker.enabled = false;
        config.speaker.source_language = "pt-BR".into();
        config.transcription.enabled = false;
        config.transcription.microphone = true;
        config.transcription.speaker = false;
        config.transcription.directory = "original-text".into();
        config.recording.enabled = true;
        config.recording.directory = "original-audio".into();
        config.providers.openai.api_key_env = "EXISTING_OPENAI_ASR".into();
        config.providers.openai.endpoint = "wss://api.openai.com/v1/realtime/translations".into();
        config.providers.openai.transcription_model = "gpt-transcribe".into();
        config.providers.openai.connect_timeout_secs = 23;
        config.providers.local.whisper_endpoint = "http://127.0.0.1:54321/inference".into();
        config.providers.local.segment_ms = 1500;
        config.providers.local.silence_ms = 200;
        config.providers.local.vad_threshold = 0.03;
        config.providers.local.request_timeout_secs = 45;
        write(&path, &legacy_document(&config));
        let migrated = AppConfig::load(&path).unwrap();
        assert_eq!(
            migrated.transcription.microphone_recognition.provider,
            "whisper"
        );
        assert_eq!(
            migrated.transcription.microphone_recognition.language,
            "fr-FR"
        );
        assert_eq!(
            migrated.transcription.speaker_recognition.provider,
            "openai"
        );
        assert_eq!(migrated.transcription.speaker_recognition.language, "pt-BR");
        assert_eq!(
            migrated.transcription.providers.openai.endpoint,
            "wss://api.openai.com/v1/realtime"
        );
        assert_eq!(
            migrated.providers.openai.endpoint,
            config.providers.openai.endpoint
        );
        assert_eq!(
            migrated.transcription.providers.openai.api_key_env,
            "EXISTING_OPENAI_ASR"
        );
        assert_eq!(
            migrated.transcription.providers.openai.model,
            "gpt-transcribe"
        );
        assert_eq!(
            migrated.transcription.providers.openai.connect_timeout_secs,
            23
        );
        let whisper = &migrated.transcription.providers.whisper;
        assert_eq!(whisper.endpoint, config.providers.local.whisper_endpoint);
        assert_eq!(
            (
                whisper.segment_ms,
                whisper.silence_ms,
                whisper.request_timeout_secs
            ),
            (1500, 200, 45)
        );
        assert_eq!(whisper.vad_threshold, 0.03);
        assert!(whisper.api_key_env.is_empty());
        let mut original = serde_json::to_value(&config).unwrap();
        let mut after = serde_json::to_value(&migrated).unwrap();
        for document in [&mut original, &mut after] {
            let transcription = document["transcription"].as_object_mut().unwrap();
            for key in ["microphone_recognition", "speaker_recognition", "providers"] {
                transcription.remove(key);
            }
        }
        assert_eq!(
            original, after,
            "Only the new recognition fields may change"
        );
        let once = fs::read_to_string(&path).unwrap();
        let commented = format!("# Already migrated.\n{once}");
        fs::write(&path, &commented).unwrap();
        AppConfig::load(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), commented);
    }

    #[test]
    fn migration_preserves_explicit_stt_and_never_redirects_a_custom_translation_endpoint() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy.toml");
        let mut config = AppConfig::default();
        config.microphone.enabled = false;
        config.speaker.enabled = false;
        config.providers.openai.endpoint = "wss://private.example/v1/realtime/translations".into();
        let legacy = legacy_document(&config);
        write(&path, &legacy);
        let mut migrated = AppConfig::load(&path).unwrap();
        assert_eq!(
            migrated.transcription.providers.openai.endpoint,
            config.providers.openai.endpoint
        );
        // Unused custom profiles cannot prevent original routing/recording.
        migrated.transcription.enabled = true;
        migrated.transcription.microphone_recognition.provider = "openai".into();
        let error = migrated.validate().unwrap_err().to_string();
        assert!(error.contains("STT requires a recognition endpoint"));
        assert!(!error.contains("private.example"));
        config.transcription.microphone_recognition.provider = "deepgram".into();
        config.transcription.microphone_recognition.language = "de-DE".into();
        config.transcription.providers.openai.endpoint = "wss://private.example/stt".into();
        let explicit = toml::Value::try_from(&config).unwrap();
        write(&path, &explicit);
        let loaded = AppConfig::load(&path).unwrap();
        assert_eq!(
            serde_json::to_value(loaded).unwrap(),
            serde_json::to_value(config).unwrap()
        );
    }

    #[test]
    fn absent_legacy_whisper_endpoint_never_introduces_an_assumed_port() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("minimal.toml");
        fs::write(
            &path,
            "[microphone]\nprovider = 'local'\nenabled = false\n[speaker]\nenabled = false\n",
        )
        .unwrap();
        let loaded = AppConfig::load(&path).unwrap();
        assert_eq!(
            loaded.transcription.microphone_recognition.provider,
            "whisper"
        );
        assert_eq!(loaded.transcription.providers.whisper.endpoint, "auto");
        assert!(!loaded.transcription.enabled);
    }

    #[test]
    fn active_stt_is_validated_even_during_translation_and_inactive_whisper_is_optional() {
        let mut config = AppConfig::default();
        config.transcription.microphone_recognition.provider = "whisper".into();
        config.validate().unwrap();
        config.transcription.enabled = true;
        config.transcription.speaker = false;
        assert!(config.microphone.enabled);
        config.validate().unwrap();
        config.transcription.providers.whisper.endpoint.clear();
        assert!(config.validate().is_err());
        config.transcription.providers.whisper.endpoint = "http://127.0.0.1:54321/inference".into();
        config.validate().unwrap();
        config.transcription.microphone_recognition.provider = "openai".into();
        config.transcription.providers.openai.model = "gpt-realtime-translate".into();
        assert!(
            config.validate().is_err(),
            "A translation model cannot be used as STT"
        );
    }

    #[test]
    fn only_selected_recognition_credentials_are_required() {
        let mut config = super::super::tests::configured_routes();
        config.microphone.enabled = false;
        config.speaker.enabled = false;
        config.transcription.enabled = true;
        config.transcription.speaker = false;
        config.transcription.microphone_recognition.provider = "whisper".into();
        config.transcription.providers.whisper.endpoint = "http://127.0.0.1:54321/inference".into();
        config.providers.gemini.api_key_env = format!("BABEL_UNUSED_STS_{}", rand::random::<u64>());
        let missing = format!("BABEL_MISSING_STT_{}", rand::random::<u64>());
        config
            .transcription
            .providers
            .deepgram
            .api_key_env
            .clone_from(&missing);
        config.transcription.speaker_recognition.provider = "deepgram".into();
        config.validate_for_start().unwrap();
        config.transcription.speaker = true;
        assert!(config.validate_for_start().is_err());
        config.transcription.speaker = false;
        config.transcription.providers.whisper.api_key_env = missing;
        assert!(config.validate_for_start().is_err());
        config.transcription.enabled = false;
        config.recording.enabled = true;
        config.validate_for_start().unwrap();
    }

    #[test]
    fn managed_whisper_preserves_an_external_credential_draft_without_requiring_it() {
        let mut config = super::super::tests::configured_routes();
        config.microphone.enabled = false;
        config.speaker.enabled = false;
        config.transcription.enabled = true;
        config.transcription.speaker = false;
        config.transcription.microphone_recognition.provider = "whisper".into();
        let draft = format!("BABEL_EXTERNAL_STT_DRAFT_{}", rand::random::<u64>());
        config.transcription.providers.whisper.api_key_env = draft.clone();
        config.validate_for_start().unwrap();
        let session = crate::provider::stt::session_config(
            &config.transcription.microphone_recognition,
            &config.transcription.providers,
        )
        .unwrap();
        assert!(session.api_key_env.is_empty());
        assert_eq!(config.transcription.providers.whisper.api_key_env, draft);
        config.transcription.providers.whisper.endpoint = "http://127.0.0.1:49251/inference".into();
        assert!(
            config.validate_for_start().is_err(),
            "external mode must require its chosen credential"
        );
    }
}
