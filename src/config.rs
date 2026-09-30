use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{fs, io::Write, path::Path};

mod stt;
pub use stt::{
    CloudSttConfig, DeepgramSttConfig, SttProviderProfiles, SttRouteConfig, WhisperSttConfig,
};

pub const TRANSLATE_MODEL: &str = "gemini-3.5-live-translate-preview";
pub const GEMINI_ENDPOINT: &str = "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub version: u32,
    pub interface: InterfaceConfig,
    pub agent: crate::commands::AgentConfig,
    pub providers: ProviderProfiles,
    pub local_runtime: LocalRuntimeConfig,
    pub audio: AudioConfig,
    pub microphone: RouteConfig,
    pub speaker: RouteConfig,
    pub transcription: TranscriptionConfig,
    pub recording: RecordingConfig,
    pub history: HistoryConfig,
    pub files: FileConfig,
}

/// Managed inference assets live in an OS cache unless an absolute directory is chosen.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LocalRuntimeConfig {
    pub directory: String,
    pub threads: u32,
    pub idle_unload_secs: u32,
}
impl Default for LocalRuntimeConfig {
    fn default() -> Self {
        Self {
            directory: String::new(),
            threads: std::thread::available_parallelism().map_or(1, |count| count.get().min(2))
                as u32,
            idle_unload_secs: 60,
        }
    }
}
impl LocalRuntimeConfig {
    pub fn validate(&self) -> Result<()> {
        if !self.directory.is_empty() {
            crate::storage::resolve_base(&self.directory)?;
        }
        ensure!(
            (1..=64).contains(&self.threads),
            "Local inference threads must be 1..64"
        );
        ensure!(
            (1..=3600).contains(&self.idle_unload_secs),
            "Local model idle unload must be 1..3600 seconds"
        );
        Ok(())
    }
}

/// Display language only; independent of translation and transcription languages.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InterfaceConfig {
    pub language: String,
}
impl Default for InterfaceConfig {
    fn default() -> Self {
        Self {
            language: "system".into(),
        }
    }
}
impl InterfaceConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.language == "system" || crate::i18n::is_supported_language(&self.language),
            "Unsupported interface language"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileConfig {
    pub base_path: String,
    pub name_pattern: String,
}
impl Default for FileConfig {
    fn default() -> Self {
        Self {
            base_path: crate::storage::default_base_path(),
            name_pattern: "{date}-{time}-{session}-{id}".into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RecordingConfig {
    pub enabled: bool,
    pub microphone: bool,
    pub speaker: bool,
    pub directory: String,
    pub mix: RecordingMixConfig,
}
impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            microphone: true,
            speaker: true,
            directory: "recordings".into(),
            mix: RecordingMixConfig::default(),
        }
    }
}

/// Recording-only levels. Original routing, STT and retained history stay raw.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RecordingMixConfig {
    pub microphone_gain_db: f32,
    pub speaker_gain_db: f32,
    pub microphone_priority: bool,
    pub ducking_db: f32,
    pub microphone_threshold_db: f32,
}
impl Default for RecordingMixConfig {
    fn default() -> Self {
        Self {
            microphone_gain_db: 0.0,
            speaker_gain_db: 0.0,
            microphone_priority: true,
            ducking_db: 12.0,
            microphone_threshold_db: -50.0,
        }
    }
}
impl RecordingMixConfig {
    /// The previous fixed-headroom sum, without added gain or priority.
    pub fn transparent() -> Self {
        Self {
            microphone_priority: false,
            ..Self::default()
        }
    }
    pub fn validate(&self) -> Result<()> {
        for (name, gain) in [
            ("microphone", self.microphone_gain_db),
            ("incoming audio", self.speaker_gain_db),
        ] {
            ensure!(
                gain.is_finite() && (-24.0..=24.0).contains(&gain),
                "Recording {name} gain must be between -24 and 24 dB"
            );
        }
        ensure!(
            self.ducking_db.is_finite() && (0.0..=30.0).contains(&self.ducking_db),
            "Recording incoming-audio reduction must be between 0 and 30 dB"
        );
        ensure!(
            self.microphone_threshold_db.is_finite()
                && (-60.0..=-20.0).contains(&self.microphone_threshold_db),
            "Recording microphone activity threshold must be between -60 and -20 dBFS"
        );
        Ok(())
    }
}

/// Original audio retained only in memory, independently of file sessions.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HistoryConfig {
    pub enabled: bool,
    pub duration_secs: u32,
}
impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            duration_secs: 600,
        }
    }
}
impl HistoryConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=3600).contains(&self.duration_secs),
            "Audio history: 1 to 3600 seconds"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderProfiles {
    pub gemini: CloudProviderConfig,
    pub openai: CloudProviderConfig,
    #[serde(skip_serializing)]
    pub elevenlabs: RemovedSetting,
    pub local: LocalProviderConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CloudProviderConfig {
    pub api_key_env: String,
    pub endpoint: String,
    pub model: String,
    #[serde(skip_serializing)]
    pub voice: RemovedSetting,
    #[serde(skip_serializing)]
    pub tts_model: RemovedSetting,
    pub transcription_model: String,
    pub connect_timeout_secs: u64,
    pub max_reconnect_attempts: u32,
}
impl Default for CloudProviderConfig {
    fn default() -> Self {
        Self {
            api_key_env: "GEMINI_API_KEY".into(),
            endpoint: GEMINI_ENDPOINT.into(),
            model: TRANSLATE_MODEL.into(),
            voice: RemovedSetting,
            tts_model: RemovedSetting,
            transcription_model: String::new(),
            connect_timeout_secs: 15,
            max_reconnect_attempts: 5,
        }
    }
}
impl Default for ProviderProfiles {
    fn default() -> Self {
        Self {
            gemini: CloudProviderConfig::default(),
            openai: CloudProviderConfig {
                api_key_env: "OPENAI_API_KEY".into(),
                endpoint: String::new(),
                model: "gpt-realtime-translate".into(),
                ..CloudProviderConfig::default()
            },
            elevenlabs: RemovedSetting,
            local: LocalProviderConfig::default(),
        }
    }
}

pub const DEFAULT_WHISPER_MODEL: &str = "base-q5_1";

pub fn is_managed_whisper_model(model: &str) -> bool {
    matches!(
        model,
        "tiny-q5_1" | "base-q5_1" | "small-q5_1" | "tiny" | "base" | "small"
    )
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LocalProviderConfig {
    pub whisper_endpoint: String,
    pub whisper_model: String,
    pub ollama_endpoint: String,
    pub translation_api: String,
    pub translation_model: String,
    pub piper_endpoint: String,
    #[serde(skip_serializing)]
    pub piper_voice: RemovedSetting,
    pub segment_ms: u32,
    pub silence_ms: u32,
    pub vad_threshold: f32,
    pub request_timeout_secs: u64,
}
impl Default for LocalProviderConfig {
    fn default() -> Self {
        Self {
            whisper_endpoint: "auto".into(),
            whisper_model: DEFAULT_WHISPER_MODEL.into(),
            ollama_endpoint: "auto".into(),
            translation_api: "ollama".into(),
            translation_model: "qwen3-0.6b".into(),
            piper_endpoint: "auto".into(),
            piper_voice: RemovedSetting,
            segment_ms: 2000,
            silence_ms: 300,
            vad_threshold: 0.01,
            request_timeout_secs: 30,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TranscriptionConfig {
    pub enabled: bool,
    pub microphone: bool,
    pub speaker: bool,
    pub timestamps: bool,
    pub directory: String,
    pub microphone_recognition: SttRouteConfig,
    pub speaker_recognition: SttRouteConfig,
    pub providers: SttProviderProfiles,
}
impl Default for TranscriptionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            microphone: true,
            speaker: true,
            timestamps: true,
            directory: "transcripts".into(),
            microphone_recognition: SttRouteConfig::default(),
            speaker_recognition: SttRouteConfig::default(),
            providers: SttProviderProfiles::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioConfig {
    pub quality: Quality,
    pub capture_queue_ms: u32,
    pub playback_queue_ms: u32,
    pub max_capture_age_ms: u32,
    pub device_latency_ms: u32,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    LowLatency,
    #[default]
    Balanced,
    HighQuality,
}
impl Quality {
    pub fn frame_ms(self) -> u32 {
        match self {
            Self::LowLatency => 10,
            Self::Balanced => 20,
            Self::HighQuality => 40,
        }
    }
    pub fn vad_silence_ms(self) -> u32 {
        match self {
            Self::LowLatency => 200,
            Self::Balanced => 400,
            Self::HighQuality => 700,
        }
    }
}
impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            quality: Quality::Balanced,
            capture_queue_ms: 200,
            playback_queue_ms: 2000,
            max_capture_age_ms: 200,
            device_latency_ms: 30,
        }
    }
}

/// Accept legacy custom-voice settings without retaining or reserializing any
/// identifiers, prompts, credentials or reference data. New sessions use only
/// the selected Live provider's native default synthesis.
#[derive(Clone, Debug, Default)]
pub struct RemovedSetting;
impl<'de> Deserialize<'de> for RemovedSetting {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        serde::de::IgnoredAny::deserialize(deserializer)?;
        Ok(Self)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RouteConfig {
    /// Enables translation only; the original audio route remains active.
    pub enabled: bool,
    pub provider: String,
    pub capture_device: String,
    pub playback_device: String,
    pub source_language: String,
    pub target_language: String,
    pub prompt: String,
    pub gain: f32,
    #[serde(skip_serializing)]
    pub voice: RemovedSetting,
    /// Internal, language-selected embedded Piper voice. Never saved or accepted
    /// from settings; cloud providers use their own native default voice.
    #[serde(skip)]
    pub resolved_voice: String,
}
impl Default for RouteConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: "gemini".into(),
            capture_device: String::new(),
            playback_device: String::new(),
            source_language: "pt-BR".into(),
            target_language: "en-US".into(),
            prompt: String::new(),
            gain: 1.0,
            voice: RemovedSetting,
            resolved_voice: String::new(),
        }
    }
}
impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: 1,
            interface: InterfaceConfig::default(),
            agent: crate::commands::AgentConfig::default(),
            providers: ProviderProfiles::default(),
            local_runtime: LocalRuntimeConfig::default(),
            audio: AudioConfig::default(),
            microphone: RouteConfig {
                playback_device: if cfg!(target_os = "linux") {
                    "babel_mic_bus".into()
                } else {
                    String::new()
                },
                ..RouteConfig::default()
            },
            speaker: RouteConfig {
                capture_device: if cfg!(target_os = "linux") {
                    "babel_speaker.monitor".into()
                } else {
                    String::new()
                },
                source_language: "en-US".into(),
                target_language: "pt-BR".into(),
                ..RouteConfig::default()
            },
            transcription: TranscriptionConfig::default(),
            recording: RecordingConfig::default(),
            history: HistoryConfig::default(),
            files: FileConfig::default(),
        }
    }
}

/// Upgrade only the legacy addresses that Babel itself generated as defaults.
/// Custom endpoints, models and explicit new configurations remain external.
fn migrate_managed_runtime(config: &mut AppConfig, document: &toml::Value) -> bool {
    let mut changed = false;
    let stored_local = document
        .get("providers")
        .and_then(|value| value.get("local"));
    let local = &mut config.providers.local;
    if stored_local.is_some_and(|value| value.get("whisper_model").is_none())
        && stored_local
            .and_then(|value| value.get("piper_voice"))
            .is_some_and(|value| value.as_str() == Some(""))
        && local.whisper_endpoint == "http://127.0.0.1:8080/inference"
        && local.ollama_endpoint == "http://127.0.0.1:11434/api/chat"
        && local.piper_endpoint == "http://127.0.0.1:5000/synthesize"
        && local.translation_model == "qwen3:4b"
    {
        local.whisper_endpoint = "auto".into();
        local.ollama_endpoint = "auto".into();
        local.piper_endpoint = "auto".into();
        local.translation_model = "qwen3-0.6b".into();
        changed = true;
    }
    let whisper = &mut config.transcription.providers.whisper;
    let stored_whisper = document
        .get("transcription")
        .and_then(|value| value.get("providers"))
        .and_then(|value| value.get("whisper"));
    if whisper.endpoint.is_empty()
        || (stored_whisper.is_none_or(|value| value.get("model").is_none())
            && whisper.endpoint == "http://127.0.0.1:8080/inference"
            && whisper.api_key_env.is_empty())
    {
        whisper.endpoint = "auto".into();
        changed = true;
    }
    changed
}

/// Compatibility markers deliberately retain no legacy data. Detect their
/// original presence separately so loading an existing file removes obsolete
/// settings from disk without modifying unrelated configuration.
fn has_removed_voice_settings(document: &toml::Value) -> bool {
    let route_settings = ["microphone", "speaker"].into_iter().any(|route| {
        document
            .get(route)
            .is_some_and(|route| route.get("voice").is_some())
    });
    let provider_settings = document.get("providers").is_some_and(|providers| {
        providers.get("elevenlabs").is_some()
            || ["gemini", "openai"].into_iter().any(|provider| {
                providers.get(provider).is_some_and(|profile| {
                    profile.get("voice").is_some() || profile.get("tts_model").is_some()
                })
            })
            || providers
                .get("local")
                .is_some_and(|local| local.get("piper_voice").is_some())
    });
    route_settings || provider_settings
}

impl AppConfig {
    pub fn profile(&self, kind: &str) -> &CloudProviderConfig {
        match kind {
            "openai" => &self.providers.openai,
            _ => &self.providers.gemini,
        }
    }
    pub fn continuous_translation(&self, route: &RouteConfig) -> bool {
        (route.provider == "gemini"
            && self.providers.gemini.model.trim_start_matches("models/") == TRANSLATE_MODEL)
            || (route.provider == "openai"
                && (self.providers.openai.model == "gpt-realtime-translate"
                    || self
                        .providers
                        .openai
                        .model
                        .starts_with("gpt-realtime-translate-")))
    }
    pub fn capture_frame_ms(&self, route: &RouteConfig) -> u32 {
        if route.enabled && self.continuous_translation(route) {
            100
        } else {
            self.audio.quality.frame_ms()
        }
    }
    pub fn load(path: &Path) -> Result<Self> {
        ensure!(
            fs::metadata(path)?.len() <= 65_536,
            "Configuration exceeds 64 KiB"
        );
        let text = fs::read_to_string(path).context("Could not read the configuration")?;
        let document: toml::Value = toml::from_str(&text).context("Invalid TOML configuration")?;
        let mut cfg: Self = document
            .clone()
            .try_into()
            .context("Invalid TOML configuration")?;
        let stored_base = document
            .get("files")
            .and_then(|files| files.get("base_path"));
        // Only on-disk legacy loading accepts relative bases. New API values
        // go directly through validate(), which always requires an absolute path.
        let migrate_base = stored_base.is_none() || !Path::new(&cfg.files.base_path).is_absolute();
        if migrate_base {
            let legacy_base = if stored_base.is_none() {
                "."
            } else {
                &cfg.files.base_path
            };
            let absolute_config = std::path::absolute(path)
                .context("Could not resolve the configuration path to migrate the base folder")?;
            let directory = absolute_config
                .parent()
                .context("The configuration has no valid parent folder")?;
            cfg.files.base_path = crate::storage::resolve_directory(directory, legacy_base)?
                .to_str()
                .context("The migrated base folder must be representable in UTF-8")?
                .to_owned();
        }
        // A removed local diagnostic provider must never silently become cloud
        // translation or recognition. Preserve its original-audio route, but
        // require the user to explicitly enable either AI feature again.
        let mut migrate_provider = false;
        for (route, transcribe) in [
            (&mut cfg.microphone, &mut cfg.transcription.microphone),
            (&mut cfg.speaker, &mut cfg.transcription.speaker),
        ] {
            if route.provider == "loopback" {
                route.provider = RouteConfig::default().provider;
                route.enabled = false;
                *transcribe = false;
                migrate_provider = true;
            }
        }
        if migrate_provider && !cfg.transcription.microphone && !cfg.transcription.speaker {
            cfg.transcription.enabled = false;
        }
        let migrate_stt = stt::migrate(&mut cfg, &document)?;
        let migrate_runtime = migrate_managed_runtime(&mut cfg, &document);
        let migrate_voices = has_removed_voice_settings(&document);
        // Legacy local translation could bypass Piper through custom TTS.
        // Restore the managed native synthesizer when that removed path left
        // its endpoint empty, preserving every explicit external endpoint.
        if cfg.providers.local.piper_endpoint.is_empty()
            && [("microphone", &cfg.microphone), ("speaker", &cfg.speaker)]
                .into_iter()
                .any(|(name, route)| {
                    route.provider == "local"
                        && document
                            .get(name)
                            .is_some_and(|route| route.get("voice").is_some())
                })
        {
            cfg.providers.local.piper_endpoint = "auto".into();
        }
        cfg.validate()?;
        if migrate_base || migrate_provider || migrate_stt || migrate_runtime || migrate_voices {
            cfg.save(path)
                .context("Could not persist the configuration migration")?;
        }
        Ok(cfg)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let serialized = toml::to_string_pretty(self)?;
        ensure!(serialized.len() <= 65_536, "Configuration exceeds 64 KiB");
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(serialized.as_bytes())?;
        file.as_file().sync_all()?;
        file.persist(path)
            .context("Could not save the configuration atomically")?;
        Ok(())
    }
    pub fn validate(&self) -> Result<()> {
        self.interface.validate()?;
        self.agent.validate()?;
        self.local_runtime.validate()?;
        self.history.validate()?;
        self.recording.mix.validate()?;
        ensure!(self.version == 1, "Unsupported configuration version");
        crate::storage::resolve_base(&self.files.base_path)?;
        crate::session::validate_pattern(&self.files.name_pattern)?;
        if self.recording.enabled {
            ensure!(
                self.recording.microphone || self.recording.speaker,
                "Select at least one source to record audio"
            );
            crate::storage::validate_path(&self.recording.directory, "audio recording folder")?;
        }
        for (name, p) in [
            ("gemini", &self.providers.gemini),
            ("openai", &self.providers.openai),
        ] {
            let model = p.model.trim_start_matches("models/");
            ensure!(
                !model.is_empty()
                    && model.len() <= 128
                    && model
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b)),
                "Invalid model identifier for provider {name}"
            );
            ensure!(
                model != "gemini-3.8-flash" || name != "gemini",
                "gemini-3.8-flash is not a Live voice output model"
            );
            let env = &p.api_key_env;
            ensure!(
                !env.is_empty()
                    && env.len() <= 128
                    && !env.as_bytes()[0].is_ascii_digit()
                    && env.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
                "Invalid environment variable name for {name}"
            );
            ensure!(
                (1..=120).contains(&p.connect_timeout_secs),
                "Timeout for {name}: 1 to 120 seconds"
            );
            ensure!(
                p.max_reconnect_attempts <= 20,
                "At most 20 reconnections for {name}"
            );
            ensure!(
                p.transcription_model.len() <= 128,
                "Transcription model settings exceed the limit for {name}"
            );
            ensure!(
                p.endpoint.len() <= 2048,
                "Endpoint for {name} exceeds 2048 bytes"
            );
        }
        ensure!(
            self.providers.gemini.endpoint == GEMINI_ENDPOINT,
            "The Gemini adapter uses the fixed official endpoint"
        );
        // Validate only transports needed by selected features. An original-only
        // session must not require an unused translator, synthesizer or ASR setup.
        for route in [&self.microphone, &self.speaker] {
            if route.enabled {
                crate::provider::create_configured_provider(
                    &route.provider,
                    self.profile(&route.provider),
                    &self.providers.local,
                )?;
            }
        }
        for (recognition, selected) in [
            (
                &self.transcription.microphone_recognition,
                self.transcription.microphone,
            ),
            (
                &self.transcription.speaker_recognition,
                self.transcription.speaker,
            ),
        ] {
            recognition.validate()?;
            if self.transcription.enabled && selected {
                self.transcription
                    .providers
                    .validate_selected(recognition)?;
                crate::provider::stt::create(recognition, &self.transcription.providers)?;
            }
        }
        ensure!(
            (100..=1000).contains(&self.audio.capture_queue_ms),
            "Capture queue: 100 to 1000 ms"
        );
        ensure!(
            (100..=5000).contains(&self.audio.playback_queue_ms),
            "Playback queue: 100 to 5000 ms"
        );
        ensure!(
            (100..=1000).contains(&self.audio.max_capture_age_ms),
            "Maximum capture age: 100 to 1000 ms"
        );
        ensure!(
            (5..=200).contains(&self.audio.device_latency_ms),
            "Device latency: 5 to 200 ms"
        );
        if self.transcription.enabled {
            ensure!(
                self.transcription.microphone || self.transcription.speaker,
                "Select at least one route to transcribe"
            );
            crate::storage::validate_path(&self.transcription.directory, "transcript folder")?;
        }
        for (name, route) in [("microphone", &self.microphone), ("speaker", &self.speaker)] {
            ensure!(
                matches!(route.provider.as_str(), "gemini" | "openai" | "local"),
                "Unknown translation provider for {name}"
            );
            for lang in [&route.source_language, &route.target_language] {
                ensure!(
                    !lang.is_empty()
                        && lang.len() <= 35
                        && lang.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
                    "Invalid language code for {name} (e.g. pt-BR)"
                );
            }
            if route.enabled && route.provider == "gemini" && self.continuous_translation(route) {
                crate::provider::gemini_translation_target_language(&route.target_language)?;
            }
            ensure!(
                route.gain.is_finite() && (0.0..=4.0).contains(&route.gain),
                "Gain for {name}: 0 to 4"
            );
            ensure!(
                route.prompt.len() <= 8192,
                "Prompt for {name} exceeds 8192 bytes"
            );
            ensure!(
                !route.enabled
                    || !self.continuous_translation(route)
                    || route.prompt.trim().is_empty(),
                "The dedicated continuous model does not accept prompts for {name}; choose a conversational model"
            );
            for device in [&route.capture_device, &route.playback_device] {
                ensure!(
                    device.len() <= 1024 && !device.contains(['\0', '\n', '\r']),
                    "Invalid device for {name}"
                );
            }
        }
        self.validate_routing()?;
        Ok(())
    }
    /// Validate routing independently of cloud credentials or feature switches.
    /// Incomplete routes are allowed while the user configures their devices.
    pub fn validate_routing(&self) -> Result<()> {
        for (name, route) in [("microphone", &self.microphone), ("speaker", &self.speaker)] {
            for device in [&route.capture_device, &route.playback_device] {
                ensure!(
                    device.len() <= 1024
                        && !device.contains(['\0', '\n', '\r'])
                        && !device.starts_with('@'),
                    "Use explicit, valid devices for {name}"
                );
            }
            if !route_configured(route) {
                continue;
            }
            ensure!(
                canonical_device(&route.capture_device) != canonical_device(&route.playback_device),
                "The {name} route feeds back into its own device; use independent cables"
            );
        }
        if route_configured(&self.microphone) && route_configured(&self.speaker) {
            ensure!(
                canonical_device(&self.microphone.playback_device)
                    != canonical_device(&self.speaker.capture_device),
                "The virtual microphone and speaker require two independent cables"
            );
        }
        Ok(())
    }
    pub fn validate_for_start(&self) -> Result<()> {
        self.validate()?;
        self.validate_routing()?;
        ensure!(
            self.microphone.enabled
                || self.speaker.enabled
                || self.transcription.enabled
                || self.recording.enabled,
            "Select translation, transcription or recording to start a session; original audio routing already works without a session"
        );
        for (name, route, transcribe, record) in [
            (
                "microphone",
                &self.microphone,
                self.transcription.microphone,
                self.recording.microphone,
            ),
            (
                "speaker",
                &self.speaker,
                self.transcription.speaker,
                self.recording.speaker,
            ),
        ] {
            let transcribe = self.transcription.enabled && transcribe;
            let record = self.recording.enabled && record;
            if route.enabled || transcribe || record {
                ensure!(
                    route_configured(route),
                    "Select capture and playback devices for {name}"
                );
            }
            if route.enabled && matches!(route.provider.as_str(), "gemini" | "openai") {
                crate::credentials::get(&self.profile(&route.provider).api_key_env)?;
            }
        }
        for (recognition, selected) in [
            (
                &self.transcription.microphone_recognition,
                self.transcription.microphone,
            ),
            (
                &self.transcription.speaker_recognition,
                self.transcription.speaker,
            ),
        ] {
            if self.transcription.enabled
                && selected
                && let Some(key) = self
                    .transcription
                    .providers
                    .api_key_env(&recognition.provider)
            {
                crate::credentials::get(key)?;
            }
        }
        Ok(())
    }
}

pub fn route_configured(route: &RouteConfig) -> bool {
    !route.capture_device.trim().is_empty() && !route.playback_device.trim().is_empty()
}

fn canonical_device(id: &str) -> String {
    let name = id.splitn(3, ':').nth(2).unwrap_or(id).to_lowercase();
    if matches!(name.as_str(), "babel_microphone" | "babel_mic_bus.monitor") {
        return "babel_mic_bus".into();
    }
    for prefix in ["cable-a", "cable-b", "cable-c", "cable-d", "cable"] {
        if name.starts_with(&format!("{prefix} input"))
            || name.starts_with(&format!("{prefix} output"))
        {
            return prefix.into();
        }
    }
    name.trim_end_matches(".monitor").into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removed_voice_settings_are_discarded_and_migrated_without_changing_session_preferences() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let mut expected = configured_routes();
        expected.files.base_path = directory.path().to_str().unwrap().into();
        expected.interface.language = "pt".into();
        expected.microphone.provider = "local".into();
        expected.speaker.provider = "local".into();
        expected.microphone.prompt = "Preserve names and numbers.".into();
        expected.speaker.prompt = "Use conversational phrasing.".into();
        expected.recording.enabled = true;
        expected.recording.directory = "original-session-audio".into();
        expected.recording.mix.microphone_gain_db = 12.0;
        expected.transcription.enabled = true;
        expected.transcription.microphone_recognition.provider = "whisper".into();
        expected.transcription.speaker_recognition.provider = "whisper".into();
        expected.validate_for_start().unwrap();
        let canonical = serde_json::to_value(&expected).unwrap();
        let mut legacy = toml::Value::try_from(&expected).unwrap();
        legacy["microphone"].as_table_mut().unwrap().insert(
            "voice".into(),
            toml::toml! {
                engine = "gemini"
                voice_id = "synthetic-clone-id"
                style = "Old custom voice design"
                chunk_ms = 400
                reference_base64 = "synthetic-private-reference"
            }
            .into(),
        );
        legacy["speaker"].as_table_mut().unwrap().insert(
            "voice".into(),
            toml::toml! {
                engine = "elevenlabs"
                voice_id = "synthetic-participant-clone"
                style = "Old participant voice"
                chunk_ms = 200
            }
            .into(),
        );
        for provider in ["gemini", "openai"] {
            let profile = legacy["providers"][provider].as_table_mut().unwrap();
            profile.insert("voice".into(), "synthetic-custom-voice".into());
            profile.insert("tts_model".into(), "synthetic-tts-model".into());
        }
        legacy["providers"]["local"]
            .as_table_mut()
            .unwrap()
            .insert("piper_voice".into(), "synthetic-old-piper-voice".into());
        legacy["providers"].as_table_mut().unwrap().insert(
            "elevenlabs".into(),
            toml::toml! {
                api_key_env = "invalid removed key reference"
                endpoint = "unsupported old synthesis endpoint"
                model = "obsolete model"
                voice = "obsolete voice"
            }
            .into(),
        );

        // The API also accepts old clients' fields without retaining their data.
        let api: AppConfig =
            serde_json::from_value(serde_json::to_value(&legacy).unwrap()).unwrap();
        api.validate_for_start().unwrap();
        assert_eq!(serde_json::to_value(&api).unwrap(), canonical);
        fs::write(&path, toml::to_string_pretty(&legacy).unwrap()).unwrap();
        let loaded = AppConfig::load(&path).unwrap();
        loaded.validate_for_start().unwrap();
        assert_eq!(serde_json::to_value(&loaded).unwrap(), canonical);
        assert!(loaded.microphone.resolved_voice.is_empty());
        assert!(loaded.speaker.resolved_voice.is_empty());
        let migrated = fs::read_to_string(&path).unwrap();
        let document: toml::Value = toml::from_str(&migrated).unwrap();
        assert!(!has_removed_voice_settings(&document));
        assert!(!migrated.contains("synthetic-private-reference"));
        assert!(!migrated.contains("synthetic-custom-voice"));
        let with_comment = format!("# Preserve after one completed migration.\n{migrated}");
        fs::write(&path, &with_comment).unwrap();
        AppConfig::load(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), with_comment);
    }

    #[test]
    fn native_voice_defaults_have_no_persisted_override_and_markers_retain_no_data() {
        assert_eq!(std::mem::size_of::<RemovedSetting>(), 0);
        let mut config: AppConfig = toml::from_str("").unwrap();
        config.validate().unwrap();
        assert!(config.microphone.resolved_voice.is_empty());
        config.microphone.resolved_voice = "synthetic-embedded-language-voice".into();
        let json = serde_json::to_value(&config).unwrap();
        for route in ["microphone", "speaker"] {
            assert!(json[route].get("voice").is_none());
            assert!(json[route].get("resolved_voice").is_none());
        }
        assert!(json["providers"].get("elevenlabs").is_none());
        for provider in ["gemini", "openai"] {
            assert!(json["providers"][provider].get("voice").is_none());
            assert!(json["providers"][provider].get("tts_model").is_none());
        }
        assert!(json["providers"]["local"].get("piper_voice").is_none());
        let encoded = toml::to_string(&config).unwrap();
        assert!(!encoded.contains("synthetic-embedded-language-voice"));
        let loaded: AppConfig = toml::from_str(&encoded).unwrap();
        assert!(loaded.microphone.resolved_voice.is_empty());
        assert_eq!(serde_json::to_value(loaded).unwrap(), json);
    }

    #[test]
    fn compatibility_does_not_accept_unrelated_unknown_fields_or_internal_voice_selection() {
        for document in [
            serde_json::json!({"microphone":{"voices":{"engine":"gemini"}}}),
            serde_json::json!({"microphone":{"resolved_voice":"not-user-configurable"}}),
            serde_json::json!({"providers":{"gemini":{"voices":"unknown"}}}),
            serde_json::json!({"providers":{"local":{"piper_voices":"unknown"}}}),
            serde_json::json!({"providers":{"unrelated":{"voice":"unknown"}}}),
        ] {
            assert!(serde_json::from_value::<AppConfig>(document).is_err());
        }
        // Only explicitly removed settings accept arbitrary legacy shapes.
        let mut legacy = serde_json::to_value(AppConfig::default()).unwrap();
        legacy["microphone"]["voice"] = serde_json::json!(["discarded", {"nested":true}]);
        legacy["speaker"]["voice"] = serde_json::Value::Null;
        legacy["providers"]["gemini"]["voice"] = 42.into();
        legacy["providers"]["gemini"]["tts_model"] = false.into();
        legacy["providers"]["elevenlabs"] = serde_json::json!([]);
        let config: AppConfig = serde_json::from_value(legacy).unwrap();
        config.validate().unwrap();
        assert_eq!(
            serde_json::to_value(config).unwrap(),
            serde_json::to_value(AppConfig::default()).unwrap()
        );
    }

    #[test]
    fn invalid_config_is_not_rewritten_when_removed_voice_settings_are_present() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let mut document = toml::Value::try_from(AppConfig::default()).unwrap();
        document["microphone"].as_table_mut().unwrap().insert(
            "voice".into(),
            toml::toml! { voice_id = "synthetic-old-clone" }.into(),
        );
        document["audio"]["playback_queue_ms"] = 0.into();
        let original = toml::to_string(&document).unwrap();
        fs::write(&path, &original).unwrap();
        assert!(AppConfig::load(&path).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn removed_external_voice_restores_missing_native_piper_but_preserves_explicit_endpoints() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        for endpoint in ["", "http://127.0.0.1:54321/synthesize"] {
            let mut config = configured_routes();
            config.microphone.provider = "local".into();
            config.speaker.enabled = false;
            config.providers.local.piper_endpoint = endpoint.into();
            let mut legacy = toml::Value::try_from(&config).unwrap();
            legacy["microphone"].as_table_mut().unwrap().insert(
                "voice".into(),
                toml::toml! {
                    engine = "elevenlabs"
                    voice_id = "synthetic-external-voice"
                }
                .into(),
            );
            fs::write(&path, toml::to_string(&legacy).unwrap()).unwrap();
            let restored = AppConfig::load(&path).unwrap();
            restored.validate_for_start().unwrap();
            assert_eq!(
                restored.providers.local.piper_endpoint,
                if endpoint.is_empty() {
                    "auto"
                } else {
                    endpoint
                }
            );
            assert_eq!(restored.microphone.provider, "local");
            assert!(!has_removed_voice_settings(
                &toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap()
            ));
        }
        // An unrelated invalid new configuration is not silently repaired.
        let mut config = configured_routes();
        config.microphone.provider = "local".into();
        config.speaker.enabled = false;
        config.providers.local.piper_endpoint.clear();
        let invalid = toml::to_string(&config).unwrap();
        fs::write(&path, &invalid).unwrap();
        assert!(AppConfig::load(&path).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), invalid);
    }

    #[test]
    fn original_history_defaults_and_bounds_are_backward_compatible() {
        let mut document = toml::Value::try_from(AppConfig::default()).unwrap();
        document.as_table_mut().unwrap().remove("history");
        let mut config: AppConfig = document.try_into().unwrap();
        assert!(config.history.enabled);
        assert_eq!(config.history.duration_secs, 600);
        config.validate().unwrap();
        for duration in [0, 3601, u32::MAX] {
            config.history.duration_secs = duration;
            assert!(config.validate().is_err());
        }
        for duration in [1, 600, 3600] {
            config.history.duration_secs = duration;
            config.validate().unwrap();
        }
        config.history.enabled = false;
        let serialized = toml::to_string(&config).unwrap();
        let restored: AppConfig = toml::from_str(&serialized).unwrap();
        assert!(!restored.history.enabled);
        assert_eq!(restored.history.duration_secs, 3600);
    }

    #[test]
    fn managed_local_defaults_validate_without_an_external_installation() {
        let mut config = configured_routes();
        config.microphone.provider = "local".into();
        config.speaker.enabled = false;
        config.transcription.enabled = true;
        config.transcription.speaker = false;
        config.transcription.microphone_recognition.provider = "whisper".into();
        config.validate_for_start().unwrap();
        assert_eq!(config.providers.local.whisper_endpoint, "auto");
        assert_eq!(config.providers.local.whisper_model, DEFAULT_WHISPER_MODEL);
        assert_eq!(config.providers.local.ollama_endpoint, "auto");
        assert_eq!(config.providers.local.piper_endpoint, "auto");
        assert_eq!(config.transcription.providers.whisper.endpoint, "auto");
        assert_eq!(
            config.transcription.providers.whisper.model,
            DEFAULT_WHISPER_MODEL
        );
        config.providers.local.whisper_model = "unknown".into();
        assert!(config.validate().is_err());
        config.providers.local.whisper_model = "small".into();
        config.validate().unwrap();
        config.local_runtime.threads = 0;
        assert!(config.validate().is_err());
        config.local_runtime.threads = 64;
        config.local_runtime.directory = "relative-models".into();
        assert!(config.validate().is_err());
        config.local_runtime.directory = tempfile::tempdir()
            .unwrap()
            .path()
            .to_string_lossy()
            .into_owned();
        config.validate().unwrap();
    }

    #[test]
    fn local_resource_defaults_are_bounded_and_idle_timeout_round_trips() {
        let mut config = LocalRuntimeConfig::default();
        assert!((1..=2).contains(&config.threads));
        assert_eq!(config.idle_unload_secs, 60);
        for invalid in [0, 3601, u32::MAX] {
            config.idle_unload_secs = invalid;
            assert!(config.validate().is_err());
        }
        config.idle_unload_secs = 120;
        config.validate().unwrap();
        let restored: LocalRuntimeConfig =
            toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
        assert_eq!(restored.idle_unload_secs, 120);
        let legacy: LocalRuntimeConfig = toml::from_str("threads = 4").unwrap();
        assert_eq!(legacy.threads, 4);
        assert_eq!(legacy.idle_unload_secs, 60);
    }

    #[test]
    fn saved_whisper_choices_round_trip_without_silent_model_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        for model in [
            "tiny",
            "base",
            "small",
            "tiny-q5_1",
            "base-q5_1",
            "small-q5_1",
        ] {
            let mut config = configured_routes();
            config.microphone.provider = "local".into();
            config.speaker.enabled = false;
            config.transcription.enabled = true;
            config.transcription.speaker = false;
            config.transcription.microphone_recognition.provider = "whisper".into();
            config.providers.local.whisper_model = model.into();
            config.transcription.providers.whisper.model = model.into();
            config.validate_for_start().unwrap();
            config.save(&path).unwrap();
            let restored = AppConfig::load(&path).unwrap();
            assert_eq!(restored.providers.local.whisper_model, model);
            assert_eq!(restored.transcription.providers.whisper.model, model);
        }
    }

    #[test]
    fn legacy_generated_ports_migrate_but_custom_endpoints_and_explicit_models_remain() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let mut config = AppConfig::default();
        config.providers.local.whisper_endpoint = "http://127.0.0.1:8080/inference".into();
        config.providers.local.ollama_endpoint = "http://127.0.0.1:11434/api/chat".into();
        config.providers.local.piper_endpoint = "http://127.0.0.1:5000/synthesize".into();
        config.providers.local.translation_model = "qwen3:4b".into();
        config.transcription.providers.whisper.endpoint =
            config.providers.local.whisper_endpoint.clone();
        let mut legacy = toml::Value::try_from(&config).unwrap();
        legacy["providers"]["local"]
            .as_table_mut()
            .unwrap()
            .remove("whisper_model");
        legacy["providers"]["local"]
            .as_table_mut()
            .unwrap()
            .insert("piper_voice".into(), "".into());
        legacy["transcription"]["providers"]["whisper"]
            .as_table_mut()
            .unwrap()
            .remove("model");
        fs::write(&path, toml::to_string(&legacy).unwrap()).unwrap();
        let migrated = AppConfig::load(&path).unwrap();
        assert_eq!(migrated.providers.local.whisper_endpoint, "auto");
        assert_eq!(migrated.providers.local.translation_model, "qwen3-0.6b");
        assert_eq!(migrated.transcription.providers.whisper.endpoint, "auto");
        let persisted = fs::read_to_string(&path).unwrap();
        AppConfig::load(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), persisted);

        legacy["providers"]["local"]["whisper_endpoint"] =
            "http://127.0.0.1:49152/inference".into();
        legacy["transcription"]["providers"]["whisper"]["endpoint"] =
            "https://example.test/recognize".into();
        fs::write(&path, toml::to_string(&legacy).unwrap()).unwrap();
        let external = AppConfig::load(&path).unwrap();
        assert_eq!(
            external.providers.local.whisper_endpoint,
            "http://127.0.0.1:49152/inference"
        );
        assert_eq!(
            external.providers.local.ollama_endpoint,
            "http://127.0.0.1:11434/api/chat"
        );
        assert_eq!(
            external.transcription.providers.whisper.endpoint,
            "https://example.test/recognize"
        );
        config.save(&path).unwrap();
        let explicit = AppConfig::load(&path).unwrap();
        assert_eq!(
            explicit.providers.local.whisper_endpoint,
            config.providers.local.whisper_endpoint
        );
        assert_eq!(
            explicit.transcription.providers.whisper.endpoint,
            config.transcription.providers.whisper.endpoint
        );
    }

    #[test]
    fn legacy_piper_voice_is_discarded_without_replacing_explicit_external_services() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let mut config = AppConfig::default();
        config.providers.local.whisper_endpoint = "http://127.0.0.1:8080/inference".into();
        config.providers.local.ollama_endpoint = "http://127.0.0.1:11434/api/chat".into();
        config.providers.local.piper_endpoint = "http://127.0.0.1:5000/synthesize".into();
        config.providers.local.translation_model = "qwen3:4b".into();
        for old_voice in [Some("synthetic-explicit-local-voice"), Some(""), None] {
            let mut legacy = toml::Value::try_from(&config).unwrap();
            let local = legacy["providers"]["local"].as_table_mut().unwrap();
            local.remove("whisper_model");
            if let Some(voice) = old_voice {
                local.insert("piper_voice".into(), voice.into());
            }
            fs::write(&path, toml::to_string(&legacy).unwrap()).unwrap();
            let migrated = AppConfig::load(&path).unwrap();
            if old_voice == Some("") {
                assert_eq!(migrated.providers.local.whisper_endpoint, "auto");
                assert_eq!(migrated.providers.local.ollama_endpoint, "auto");
                assert_eq!(migrated.providers.local.piper_endpoint, "auto");
                assert_eq!(migrated.providers.local.translation_model, "qwen3-0.6b");
            } else {
                assert_eq!(
                    serde_json::to_value(&migrated.providers.local).unwrap(),
                    serde_json::to_value(&config.providers.local).unwrap()
                );
            }
            let persisted = fs::read_to_string(&path).unwrap();
            assert!(!has_removed_voice_settings(
                &toml::from_str(&persisted).unwrap()
            ));
            AppConfig::load(&path).unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), persisted);
        }
    }

    #[test]
    fn local_translation_always_requires_native_synthesis_configuration() {
        let mut config = configured_routes();
        config.microphone.provider = "local".into();
        config.speaker.enabled = false;
        config.providers.local.piper_endpoint.clear();
        assert!(config.validate().is_err());
        config.providers.local.piper_endpoint = "auto".into();
        config.validate().unwrap();
    }

    pub(super) fn configured_routes() -> AppConfig {
        // Device discovery is deliberately not involved in configuration tests.
        // Native platforms start without selected virtual endpoints, so fixtures
        // must specify both complete routes instead of inheriting Linux defaults.
        let mut cfg = AppConfig::default();
        cfg.microphone.capture_device = "physical-mic".into();
        cfg.microphone.playback_device = "babel_mic_bus".into();
        cfg.speaker.capture_device = "babel_speaker.monitor".into();
        cfg.speaker.playback_device = "physical-speakers".into();
        cfg
    }

    #[test]
    fn removed_provider_is_rejected_in_new_configs_even_with_all_features_off() {
        for microphone in [true, false] {
            let mut cfg = configured_routes();
            cfg.microphone.enabled = false;
            cfg.speaker.enabled = false;
            cfg.transcription.enabled = false;
            cfg.recording.enabled = false;
            let route = if microphone {
                &mut cfg.microphone
            } else {
                &mut cfg.speaker
            };
            route.provider = "loopback".into();
            // API deserialization must not perform the legacy disk migration.
            let decoded: AppConfig =
                serde_json::from_value(serde_json::to_value(&cfg).unwrap()).unwrap();
            assert!(
                decoded
                    .validate()
                    .unwrap_err()
                    .to_string()
                    .contains("Unknown translation provider")
            );
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("new.toml");
            assert!(decoded.save(&path).is_err());
            assert!(!path.exists());
        }
    }

    #[test]
    fn loading_removed_provider_disables_its_ai_and_preserves_other_routes_and_recording() {
        for (microphone, speaker) in [(true, false), (false, true), (true, true)] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("legacy.toml");
            let mut cfg = configured_routes();
            cfg.files.base_path = directory.path().join("saved").to_str().unwrap().into();
            cfg.microphone.provider = "openai".into();
            cfg.speaker.provider = "openai".into();
            cfg.transcription.enabled = true;
            cfg.recording.enabled = true;
            cfg.recording.directory = "original-audio".into();
            cfg.files.name_pattern = "legacy_{session}_{id}".into();
            if microphone {
                cfg.microphone.provider = "loopback".into();
            }
            if speaker {
                cfg.speaker.provider = "loopback".into();
            }
            fs::write(&path, toml::to_string(&cfg).unwrap()).unwrap();
            let mut expected = cfg;
            if microphone {
                expected.microphone.provider = RouteConfig::default().provider;
                expected.microphone.enabled = false;
                expected.transcription.microphone = false;
            }
            if speaker {
                expected.speaker.provider = RouteConfig::default().provider;
                expected.speaker.enabled = false;
                expected.transcription.speaker = false;
            }
            if microphone && speaker {
                expected.transcription.enabled = false;
            }
            let loaded = AppConfig::load(&path).unwrap();
            assert_eq!(
                toml::to_string(&loaded).unwrap(),
                toml::to_string(&expected).unwrap()
            );
            if microphone && speaker {
                // Both old diagnostic routes remain usable for recording without
                // reading any cloud credentials or constructing a provider.
                loaded.validate_for_start().unwrap();
            }
            let persisted = fs::read_to_string(&path).unwrap();
            assert!(!persisted.contains("loopback"));
            assert_eq!(
                toml::to_string(&toml::from_str::<AppConfig>(&persisted).unwrap()).unwrap(),
                toml::to_string(&expected).unwrap()
            );
            let with_comment = format!("# Migration is complete.\n{persisted}");
            fs::write(&path, &with_comment).unwrap();
            AppConfig::load(&path).unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), with_comment);
            assert!(!directory.path().join("saved").exists());
        }
    }

    #[test]
    fn removed_provider_migration_does_not_rewrite_an_otherwise_invalid_config() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("invalid.toml");
        let mut cfg = configured_routes();
        cfg.microphone.provider = "loopback".into();
        cfg.recording.enabled = true;
        cfg.recording.directory.clear();
        let text = toml::to_string(&cfg).unwrap();
        fs::write(&path, &text).unwrap();
        assert!(AppConfig::load(&path).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn new_defaults_use_an_absolute_home_subdirectory_and_roundtrip_verbatim() {
        let mut cfg = AppConfig::default();
        let home = std::env::home_dir()
            .filter(|path| path.is_absolute())
            .unwrap();
        assert_eq!(Path::new(&cfg.files.base_path), home.join("Babel"));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        cfg.files.base_path = dir.path().join("Meus arquivos ").to_str().unwrap().into();
        cfg.save(&path).unwrap();
        assert_eq!(
            AppConfig::load(&path).unwrap().files.base_path,
            cfg.files.base_path
        );
        assert!(!Path::new(&cfg.files.base_path).exists());
    }

    #[test]
    fn loading_legacy_bases_migrates_once_relative_to_the_configuration_file() {
        for original_base in [None, Some("."), Some("Meus arquivos "), Some("../sessions")] {
            let directory = tempfile::tempdir().unwrap();
            let config_directory = directory.path().join("config");
            fs::create_dir(&config_directory).unwrap();
            let path = config_directory.join("babel.toml");
            let mut expected = AppConfig::default();
            expected.microphone.target_language = "fr-FR".into();
            expected.transcription.directory = "texto-original".into();
            expected.recording.directory = directory
                .path()
                .join("external-audio")
                .to_str()
                .unwrap()
                .into();
            let mut document = toml::Value::try_from(&expected).unwrap();
            let files = document.get_mut("files").unwrap().as_table_mut().unwrap();
            if let Some(base) = original_base {
                files.insert("base_path".into(), toml::Value::String(base.into()));
            } else {
                files.remove("base_path");
            }
            fs::write(&path, toml::to_string(&document).unwrap()).unwrap();
            expected.files.base_path =
                std::path::absolute(config_directory.join(original_base.unwrap_or(".")))
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .into();
            let loaded = AppConfig::load(&path).unwrap();
            assert_eq!(
                toml::to_string(&loaded).unwrap(),
                toml::to_string(&expected).unwrap()
            );
            let persisted = fs::read_to_string(&path).unwrap();
            assert_eq!(
                toml::from_str::<AppConfig>(&persisted)
                    .unwrap()
                    .files
                    .base_path,
                expected.files.base_path
            );
            // A second load must not save again; save would discard this comment.
            let with_comment = format!("# Already migrated; preserve this comment.\n{persisted}");
            fs::write(&path, &with_comment).unwrap();
            let again = AppConfig::load(&path).unwrap();
            assert_eq!(again.files.base_path, loaded.files.base_path);
            assert_eq!(fs::read_to_string(&path).unwrap(), with_comment);
            assert!(!directory.path().join("external-audio").exists());
        }
    }

    #[test]
    fn loading_a_legacy_file_without_files_table_uses_its_directory() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("old.toml");
        fs::write(&path, "version = 1").unwrap();
        let loaded = AppConfig::load(&path).unwrap();
        assert_eq!(Path::new(&loaded.files.base_path), directory.path());
        assert_eq!(loaded.transcription.directory, "transcripts");
        assert_eq!(loaded.recording.directory, "recordings");
        assert!(fs::read_to_string(&path).unwrap().contains("base_path"));
    }

    #[test]
    fn invalid_legacy_configuration_is_never_rewritten_or_partly_migrated() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("invalid.toml");
        for original in [
            "version = 2\n[files]\nbase_path = 'relative'",
            "version = 1\n[files]\nbase_path = ''",
            "version = 1\n[files]\nbase_path = '  '",
            "version = 1\n[files]\nbase_path = 12",
        ] {
            fs::write(&path, original).unwrap();
            assert!(AppConfig::load(&path).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
        }
        assert_eq!(directory.path().read_dir().unwrap().count(), 1);
    }

    #[test]
    fn deserializing_new_api_values_does_not_apply_legacy_migration() {
        for base in [".", "archive", "../archive", "~/Babel"] {
            let mut value = serde_json::to_value(AppConfig::default()).unwrap();
            value["files"]["base_path"] = serde_json::Value::String(base.into());
            let config: AppConfig = serde_json::from_value(value).unwrap();
            assert!(config.validate().is_err());
            assert_eq!(config.files.base_path, base);
        }
    }

    #[cfg(unix)]
    #[test]
    fn migration_preserves_symlink_parent_semantics() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("real/nested")).unwrap();
        std::os::unix::fs::symlink("real/nested", directory.path().join("alias")).unwrap();
        let path = directory.path().join("old.toml");
        fs::write(&path, "[files]\nbase_path = 'alias/../archive'").unwrap();
        let loaded = AppConfig::load(&path).unwrap();
        assert_eq!(
            Path::new(&loaded.files.base_path),
            directory.path().join("alias/../archive")
        );
        assert!(!directory.path().join("real/archive").exists());
    }

    // Linux permits these filenames; macOS APFS rejects mkdir with EILSEQ before
    // a configuration file could exist. Pure non-UTF-8 path validation is also
    // covered on every Unix platform by storage's non_utf8_home test.
    #[cfg(target_os = "linux")]
    #[test]
    fn migration_rejects_non_utf8_parent_without_rewriting_the_file() {
        use std::os::unix::ffi::OsStringExt;
        let directory = tempfile::tempdir().unwrap();
        let invalid_directory = directory
            .path()
            .join(std::ffi::OsString::from_vec(vec![0xff]));
        fs::create_dir(&invalid_directory).unwrap();
        let invalid_path = invalid_directory.join("old.toml");
        let original = "version = 1";
        fs::write(&invalid_path, original).unwrap();
        let error = AppConfig::load(&invalid_path).unwrap_err();
        assert!(error.to_string().contains("UTF-8"));
        assert_eq!(fs::read_to_string(invalid_path).unwrap(), original);
    }

    #[test]
    fn base_path_is_always_validated_but_disabled_destinations_can_be_empty() {
        let mut cfg = AppConfig::default();
        cfg.transcription.directory.clear();
        cfg.recording.directory.clear();
        cfg.validate().unwrap();
        for invalid in [
            String::new(),
            " \t".into(),
            "a\0b".into(),
            "x".repeat(4097),
            ".".into(),
            "relative".into(),
            "~/Babel".into(),
        ] {
            cfg.files.base_path = invalid;
            assert!(cfg.validate().is_err());
        }
    }
    #[test]
    fn default_is_continuous_and_keys_are_not_stored() {
        let cfg = AppConfig::default();
        cfg.validate().unwrap();
        assert!(cfg.continuous_translation(&cfg.microphone));
        assert_eq!(cfg.capture_frame_ms(&cfg.microphone), 100);
        assert!(!toml::to_string(&cfg).unwrap().contains("api_key ="));
    }
    #[test]
    fn unsupported_capabilities_rejected_instead_of_ignored() {
        let mut cfg = AppConfig::default();
        cfg.microphone.prompt = "Use glossário".into();
        assert!(cfg.validate().is_err());
        cfg.providers.gemini.model = "gemini-3.8-live".into();
        cfg.validate().unwrap();
        cfg.providers.gemini.model = "gemini-3.8-flash".into();
        assert!(cfg.validate().is_err());
    }
    #[test]
    fn gemini_translation_targets_are_checked_only_for_enabled_translate_routes() {
        let mut cfg = AppConfig::default();
        cfg.microphone.target_language = "xx-US".into();
        assert!(
            cfg.validate()
                .unwrap_err()
                .to_string()
                .contains("target language")
        );
        cfg.microphone.enabled = false;
        cfg.validate().unwrap();
        cfg.microphone.enabled = true;
        cfg.microphone.provider = "openai".into();
        cfg.validate().unwrap();
        cfg.microphone.provider = "gemini".into();
        cfg.providers.gemini.model = "gemini-3.8-live".into();
        cfg.validate().unwrap();
        cfg.providers.gemini.model = TRANSLATE_MODEL.into();
        cfg.microphone.target_language = "en-US".into();
        cfg.speaker.target_language = "pt-PT".into();
        cfg.validate().unwrap();
        assert_eq!(cfg.microphone.target_language, "en-US");
        cfg.speaker.target_language = "pt".into();
        assert!(
            cfg.validate()
                .unwrap_err()
                .to_string()
                .contains("pt-BR or pt-PT")
        );
    }
    #[test]
    fn dated_openai_translation_models_keep_continuous_restrictions() {
        let mut cfg = AppConfig::default();
        cfg.microphone.provider = "openai".into();
        cfg.providers.openai.model = "gpt-realtime-translate-2026-09-01".into();
        assert_eq!(cfg.capture_frame_ms(&cfg.microphone), 100);
        cfg.microphone.prompt = "Glossary".into();
        assert!(cfg.validate().is_err());
        cfg.microphone.prompt.clear();
        cfg.validate().unwrap();
    }
    #[test]
    fn invalid_gain_queue_and_typo_are_rejected() {
        let mut cfg = AppConfig::default();
        cfg.audio.playback_queue_ms = u32::MAX;
        assert!(cfg.validate().is_err());
        cfg.audio = AudioConfig::default();
        cfg.speaker.gain = f32::NAN;
        assert!(cfg.validate().is_err());
        assert!(toml::from_str::<AppConfig>("versoin = 1").is_err());
    }
    #[test]
    fn recording_mix_defaults_and_roundtrip_are_independent_of_translation() {
        let legacy: RecordingConfig = toml::from_str("enabled = true\n").unwrap();
        assert!(legacy.mix.microphone_priority);
        assert_eq!(legacy.mix.microphone_gain_db, 0.0);
        assert_eq!(legacy.mix.speaker_gain_db, 0.0);
        assert_eq!(legacy.mix.ducking_db, 12.0);
        let mut cfg = AppConfig::default();
        cfg.recording.mix.microphone_gain_db = 12.0;
        cfg.recording.mix.speaker_gain_db = -6.0;
        cfg.recording.mix.ducking_db = 18.0;
        cfg.recording.mix.microphone_threshold_db = -45.0;
        cfg.validate().unwrap();
        let saved = toml::to_string(&cfg).unwrap();
        let restored: AppConfig = toml::from_str(&saved).unwrap();
        assert_eq!(restored.recording.mix.microphone_gain_db, 12.0);
        assert_eq!(restored.recording.mix.speaker_gain_db, -6.0);
        assert_eq!(restored.recording.mix.ducking_db, 18.0);
        assert_eq!(restored.recording.mix.microphone_threshold_db, -45.0);
        assert_eq!(restored.microphone.gain, 1.0);
        assert_eq!(restored.speaker.gain, 1.0);
        assert!(toml::from_str::<RecordingConfig>("[mix]\nunknown_gain = 12\n").is_err());
    }
    #[test]
    fn recording_mix_rejects_non_finite_and_out_of_range_values_even_when_disabled() {
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -24.1, 24.1] {
            let mut cfg = AppConfig::default();
            cfg.recording.mix.microphone_gain_db = value;
            assert!(cfg.validate().is_err());
            cfg.recording.mix = RecordingMixConfig::default();
            cfg.recording.mix.speaker_gain_db = value;
            assert!(cfg.validate().is_err());
        }
        for value in [f32::NAN, f32::INFINITY, -1.0, 30.1] {
            let mut cfg = AppConfig::default();
            cfg.recording.mix.ducking_db = value;
            assert!(cfg.validate().is_err());
        }
        for value in [f32::NAN, f32::NEG_INFINITY, -60.1, -19.9] {
            let mut cfg = AppConfig::default();
            cfg.recording.mix.microphone_threshold_db = value;
            assert!(cfg.validate().is_err());
        }
        for (gain, reduction, threshold) in [(-24.0, 0.0, -60.0), (24.0, 30.0, -20.0)] {
            RecordingMixConfig {
                microphone_gain_db: gain,
                speaker_gain_db: gain,
                ducking_db: reduction,
                microphone_threshold_db: threshold,
                ..Default::default()
            }
            .validate()
            .unwrap();
        }
    }
    #[test]
    fn catches_feedback_across_native_endpoint_directions() {
        assert_eq!(
            canonical_device("input:0:BlackHole 2ch"),
            canonical_device("output:2:BlackHole 2ch")
        );
        assert_eq!(
            canonical_device("input:0:CABLE-A Output (VB-Audio)"),
            canonical_device("output:3:CABLE-A Input (VB-Audio)")
        );
        let mut cfg = configured_routes();
        cfg.microphone.enabled = false;
        cfg.speaker.enabled = false;
        cfg.recording.enabled = true;
        cfg.validate_for_start().unwrap();
        cfg.speaker.playback_device = "babel_speaker".into();
        assert!(
            cfg.validate_for_start()
                .unwrap_err()
                .to_string()
                .contains("feeds back into its own device")
        );
    }
    #[test]
    fn recording_only_needs_no_cloud_or_voice_credentials() {
        let mut cfg = configured_routes();
        cfg.microphone.enabled = false;
        cfg.speaker.enabled = false;
        cfg.recording.enabled = true;
        cfg.providers.gemini.api_key_env = "BABEL_TEST_UNUSED_ASR_KEY".into();
        cfg.validate_for_start().unwrap();
        assert_eq!(cfg.capture_frame_ms(&cfg.microphone), 20);
        cfg.recording.enabled = false;
        cfg.validate_routing().unwrap();
        assert!(cfg.validate_for_start().is_err());
    }
    #[test]
    fn transcription_without_translation_uses_only_selected_originals() {
        let mut cfg = configured_routes();
        cfg.microphone.enabled = false;
        cfg.speaker.enabled = false;
        cfg.transcription.enabled = true;
        cfg.transcription.speaker = false;
        cfg.transcription.microphone_recognition.provider = "whisper".into();
        cfg.transcription.providers.whisper.endpoint = "http://127.0.0.1:54321/inference".into();
        cfg.providers.local.ollama_endpoint.clear();
        cfg.providers.local.piper_endpoint.clear();
        cfg.providers.local.translation_model.clear();
        cfg.providers.gemini.api_key_env =
            format!("BABEL_UNUSED_STS_KEY_{}", rand::random::<u64>());
        cfg.validate_for_start().unwrap();
        cfg.transcription.microphone_recognition.provider = "unknown".into();
        assert!(cfg.validate_for_start().is_err());
    }
    #[test]
    fn feedback_is_rejected_even_when_every_feature_is_off() {
        let mut cfg = configured_routes();
        cfg.microphone.enabled = false;
        cfg.speaker.enabled = false;
        cfg.validate_routing().unwrap();
        cfg.microphone.playback_device = "babel_speaker".into();
        assert!(
            cfg.validate_routing()
                .unwrap_err()
                .to_string()
                .contains("two independent cables")
        );
        assert!(cfg.validate().is_err());
    }
    #[test]
    fn atomic_save_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("babel.toml");
        AppConfig::default().save(&path).unwrap();
        let mut cfg = AppConfig::load(&path).unwrap();
        cfg.audio.quality = Quality::LowLatency;
        cfg.save(&path).unwrap();
        assert_eq!(
            AppConfig::load(&path).unwrap().audio.quality,
            Quality::LowLatency
        );
    }
    #[test]
    fn interface_defaults_and_persistence_are_independent_of_speech_languages() {
        let old: AppConfig = toml::from_str("version = 1").unwrap();
        assert_eq!(old.interface.language, "system");
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        for language in ["system", "en", "pt"] {
            let mut config = old.clone();
            config.interface.language = language.into();
            config.save(&path).unwrap();
            let restored = AppConfig::load(&path).unwrap();
            assert_eq!(restored.interface.language, language);
            assert_eq!(
                restored.microphone.source_language,
                old.microphone.source_language
            );
            assert_eq!(
                restored.speaker.target_language,
                old.speaker.target_language
            );
        }
        let mut invalid = old;
        invalid.interface.language = "../../unknown".into();
        assert!(invalid.validate().is_err());
    }
}
