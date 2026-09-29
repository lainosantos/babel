use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{fs, io::Write, path::Path};

pub const TRANSLATE_MODEL: &str = "gemini-3.5-live-translate-preview";
pub const GEMINI_ENDPOINT: &str = "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub version: u32,
    pub interface: InterfaceConfig,
    pub agent: crate::commands::AgentConfig,
    pub providers: ProviderProfiles,
    pub audio: AudioConfig,
    pub microphone: RouteConfig,
    pub speaker: RouteConfig,
    pub transcription: TranscriptionConfig,
    pub recording: RecordingConfig,
    pub files: FileConfig,
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
            "Idioma da interface não suportado"
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
}
impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            microphone: true,
            speaker: true,
            directory: "recordings".into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderProfiles {
    pub gemini: CloudProviderConfig,
    pub openai: CloudProviderConfig,
    pub elevenlabs: CloudProviderConfig,
    pub local: LocalProviderConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CloudProviderConfig {
    pub api_key_env: String,
    pub endpoint: String,
    pub model: String,
    pub voice: String,
    pub tts_model: String,
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
            voice: "Kore".into(),
            tts_model: "gemini-3.8-flash-tts".into(),
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
                voice: "marin".into(),
                tts_model: String::new(),
                ..CloudProviderConfig::default()
            },
            elevenlabs: CloudProviderConfig {
                api_key_env: "ELEVENLABS_API_KEY".into(),
                endpoint: "https://api.elevenlabs.io/v1".into(),
                model: "eleven_flash_v2_5".into(),
                tts_model: "eleven_flash_v2_5".into(),
                voice: String::new(),
                ..CloudProviderConfig::default()
            },
            local: LocalProviderConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LocalProviderConfig {
    pub whisper_endpoint: String,
    pub ollama_endpoint: String,
    pub translation_model: String,
    pub piper_endpoint: String,
    pub piper_voice: String,
    pub segment_ms: u32,
    pub silence_ms: u32,
    pub vad_threshold: f32,
    pub request_timeout_secs: u64,
}
impl Default for LocalProviderConfig {
    fn default() -> Self {
        Self {
            whisper_endpoint: "http://127.0.0.1:8080/inference".into(),
            ollama_endpoint: "http://127.0.0.1:11434/api/chat".into(),
            translation_model: "qwen3:4b".into(),
            piper_endpoint: "http://127.0.0.1:5000/synthesize".into(),
            piper_voice: String::new(),
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
}
impl Default for TranscriptionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            microphone: true,
            speaker: true,
            timestamps: true,
            directory: "transcripts".into(),
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RouteVoiceConfig {
    /// native uses audio from the translation model; other engines re-synthesize translated text.
    pub engine: String,
    pub voice_id: String,
    pub style: String,
    pub chunk_ms: u32,
}
impl Default for RouteVoiceConfig {
    fn default() -> Self {
        Self {
            engine: "native".into(),
            voice_id: String::new(),
            style: String::new(),
            chunk_ms: 400,
        }
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
    pub voice: RouteVoiceConfig,
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
            voice: RouteVoiceConfig::default(),
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
            files: FileConfig::default(),
        }
    }
}

impl AppConfig {
    pub fn profile(&self, kind: &str) -> &CloudProviderConfig {
        match kind {
            "openai" => &self.providers.openai,
            "elevenlabs" => &self.providers.elevenlabs,
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
            "Configuração maior que 64 KiB"
        );
        let text = fs::read_to_string(path).context("Não foi possível ler a configuração")?;
        let document: toml::Value = toml::from_str(&text).context("Configuração TOML inválida")?;
        let mut cfg: Self = document
            .clone()
            .try_into()
            .context("Configuração TOML inválida")?;
        let stored_base = document
            .get("files")
            .and_then(|files| files.get("base_path"));
        // Only on-disk legacy loading accepts relative bases. New API values
        // go directly through validate(), which always requires an absolute path.
        let migrate = stored_base.is_none() || !Path::new(&cfg.files.base_path).is_absolute();
        if migrate {
            let legacy_base = if stored_base.is_none() {
                "."
            } else {
                &cfg.files.base_path
            };
            let absolute_config = std::path::absolute(path).context(
                "Não foi possível resolver o caminho da configuração para migrar a pasta base",
            )?;
            let directory = absolute_config
                .parent()
                .context("A configuração não possui uma pasta válida")?;
            cfg.files.base_path = crate::storage::resolve_directory(directory, legacy_base)?
                .to_str()
                .context("A pasta base migrada precisa ser representável em UTF-8")?
                .to_owned();
        }
        cfg.validate()?;
        if migrate {
            cfg.save(path)
                .context("Não foi possível persistir a migração da pasta base")?;
        }
        Ok(cfg)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let serialized = toml::to_string_pretty(self)?;
        ensure!(serialized.len() <= 65_536, "Configuração maior que 64 KiB");
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
            .context("Não foi possível salvar a configuração atomicamente")?;
        Ok(())
    }
    pub fn validate(&self) -> Result<()> {
        self.interface.validate()?;
        self.agent.validate()?;
        ensure!(self.version == 1, "Versão de configuração não suportada");
        crate::storage::resolve_base(&self.files.base_path)?;
        crate::session::validate_pattern(&self.files.name_pattern)?;
        if self.recording.enabled {
            ensure!(
                self.recording.microphone || self.recording.speaker,
                "Escolha pelo menos uma origem para gravar áudio"
            );
            crate::storage::validate_path(&self.recording.directory, "Pasta de gravação de áudio")?;
        }
        for (name, p) in [
            ("gemini", &self.providers.gemini),
            ("openai", &self.providers.openai),
            ("elevenlabs", &self.providers.elevenlabs),
        ] {
            let model = p.model.trim_start_matches("models/");
            ensure!(
                !model.is_empty()
                    && model.len() <= 128
                    && model
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b)),
                "Identificador de modelo inválido no provider {name}"
            );
            ensure!(
                model != "gemini-3.8-flash" || name != "gemini",
                "gemini-3.8-flash não é um modelo Live de saída de voz"
            );
            let env = &p.api_key_env;
            ensure!(
                !env.is_empty()
                    && env.len() <= 128
                    && !env.as_bytes()[0].is_ascii_digit()
                    && env.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
                "Nome de variável de ambiente inválido para {name}"
            );
            ensure!(
                (1..=120).contains(&p.connect_timeout_secs),
                "Timeout de {name}: 1 a 120 segundos"
            );
            ensure!(
                p.max_reconnect_attempts <= 20,
                "Máximo de 20 reconexões para {name}"
            );
            ensure!(
                p.voice.len() <= 128
                    && p.tts_model.len() <= 128
                    && p.transcription_model.len() <= 128,
                "Configuração de voz/modelo excessiva em {name}"
            );
            ensure!(
                p.endpoint.len() <= 2048,
                "Endpoint de {name} excede 2048 bytes"
            );
        }
        ensure!(
            self.providers.gemini.endpoint == GEMINI_ENDPOINT,
            "O adaptador Gemini usa o endpoint oficial fixo"
        );
        ensure!(
            self.providers.elevenlabs.endpoint == "https://api.elevenlabs.io/v1",
            "O adaptador ElevenLabs usa o endpoint oficial fixo"
        );
        // Validate only transports needed by selected features. An original-only
        // session must not require an unused translator, synthesizer or ASR setup.
        for (route, transcribe) in [
            (&self.microphone, self.transcription.microphone),
            (&self.speaker, self.transcription.speaker),
        ] {
            if route.enabled {
                crate::provider::create_route_provider(
                    &route.provider,
                    self.profile(&route.provider),
                    &self.providers.local,
                    route.voice.engine == "native",
                )?;
            } else if self.transcription.enabled && transcribe {
                crate::provider::create_transcription_provider(
                    &route.provider,
                    self.profile(&route.provider),
                    &self.providers.local,
                )?;
            }
        }
        ensure!(
            (100..=1000).contains(&self.audio.capture_queue_ms),
            "Fila de captura: 100 a 1000 ms"
        );
        ensure!(
            (100..=5000).contains(&self.audio.playback_queue_ms),
            "Fila de reprodução: 100 a 5000 ms"
        );
        ensure!(
            (100..=1000).contains(&self.audio.max_capture_age_ms),
            "Idade máxima da captura: 100 a 1000 ms"
        );
        ensure!(
            (5..=200).contains(&self.audio.device_latency_ms),
            "Latência do dispositivo: 5 a 200 ms"
        );
        if self.transcription.enabled {
            ensure!(
                self.transcription.microphone || self.transcription.speaker,
                "Escolha pelo menos um fluxo para transcrever"
            );
            crate::storage::validate_path(&self.transcription.directory, "Pasta de transcrições")?;
        }
        for (name, route, transcribe) in [
            ("microfone", &self.microphone, self.transcription.microphone),
            ("saída", &self.speaker, self.transcription.speaker),
        ] {
            ensure!(
                matches!(
                    route.provider.as_str(),
                    "gemini" | "openai" | "local" | "loopback"
                ),
                "Provider de tradução desconhecido em {name}"
            );
            for lang in [&route.source_language, &route.target_language] {
                ensure!(
                    !lang.is_empty()
                        && lang.len() <= 35
                        && lang.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
                    "Código de idioma inválido em {name} (ex. pt-BR)"
                );
            }
            ensure!(
                route.gain.is_finite() && (0.0..=4.0).contains(&route.gain),
                "Ganho de {name}: 0 a 4"
            );
            ensure!(
                route.prompt.len() <= 8192,
                "Prompt de {name} excede 8192 bytes"
            );
            ensure!(
                !route.enabled
                    || !self.continuous_translation(route)
                    || route.prompt.trim().is_empty(),
                "Modelo dedicado contínuo não aceita prompts em {name}; escolha um modelo conversacional"
            );
            ensure!(
                matches!(
                    route.voice.engine.as_str(),
                    "native" | "gemini" | "elevenlabs"
                ),
                "Síntese de voz inválida em {name}"
            );
            ensure!(
                (100..=2000).contains(&route.voice.chunk_ms),
                "Intervalo de texto para síntese: 100 a 2000 ms"
            );
            ensure!(
                route.voice.voice_id.len() <= 256 && route.voice.style.len() <= 2000,
                "Voz/estilo excessivo em {name}"
            );
            if !route.enabled {
                // Unused voice settings cannot prevent original recording/ASR.
            } else if route.voice.engine == "native" {
                ensure!(
                    route.voice.style.is_empty(),
                    "Estilo TTS só é suportado pela síntese Gemini"
                );
                ensure!(
                    !self.continuous_translation(route) || route.voice.voice_id.is_empty(),
                    "Voz fixa/clonada em {name} exige síntese Gemini ou ElevenLabs; o modelo dedicado não aceita seleção de voz nativa"
                );
            } else {
                ensure!(
                    route.provider != "loopback",
                    "Loopback não produz texto traduzido para síntese"
                );
                ensure!(
                    !route.voice.voice_id.trim().is_empty(),
                    "Escolha uma voz para a síntese de {name}"
                );
                ensure!(
                    route.voice.engine != "elevenlabs" || route.voice.style.is_empty(),
                    "ElevenLabs não aceita este prompt livre de estilo; use uma voz de design"
                );
            }
            ensure!(
                !(transcribe && self.transcription.enabled && route.provider == "loopback"),
                "Loopback não produz transcrição"
            );
            for device in [&route.capture_device, &route.playback_device] {
                ensure!(
                    device.len() <= 1024 && !device.contains(['\0', '\n', '\r']),
                    "Dispositivo inválido em {name}"
                );
            }
        }
        self.validate_routing()?;
        Ok(())
    }
    /// Validate routing independently of cloud credentials or feature switches.
    /// Incomplete routes are allowed while the user configures their devices.
    pub fn validate_routing(&self) -> Result<()> {
        for (name, route) in [("microfone", &self.microphone), ("saída", &self.speaker)] {
            for device in [&route.capture_device, &route.playback_device] {
                ensure!(
                    device.len() <= 1024
                        && !device.contains(['\0', '\n', '\r'])
                        && !device.starts_with('@'),
                    "Use dispositivos explícitos e válidos em {name}"
                );
            }
            if !route_configured(route) {
                continue;
            }
            ensure!(
                canonical_device(&route.capture_device) != canonical_device(&route.playback_device),
                "O fluxo {name} retorna ao próprio dispositivo; use cabos independentes"
            );
        }
        if route_configured(&self.microphone) && route_configured(&self.speaker) {
            ensure!(
                canonical_device(&self.microphone.playback_device)
                    != canonical_device(&self.speaker.capture_device),
                "Microfone e saída virtuais precisam de dois cabos independentes"
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
            "Selecione tradução, transcrição ou gravação para iniciar uma sessão; o roteamento original já funciona sem sessão"
        );
        for (name, route, transcribe, record) in [
            (
                "microfone",
                &self.microphone,
                self.transcription.microphone,
                self.recording.microphone,
            ),
            (
                "saída",
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
                    "Selecione captura e reprodução de {name}"
                );
            }
            if (route.enabled || transcribe)
                && matches!(route.provider.as_str(), "gemini" | "openai")
            {
                crate::credentials::get(&self.profile(&route.provider).api_key_env)?;
            }
            if !route.enabled && transcribe {
                crate::provider::create_transcription_provider(
                    &route.provider,
                    self.profile(&route.provider),
                    &self.providers.local,
                )?;
            }
            if route.enabled && route.voice.engine != "native" {
                crate::credentials::get(&self.profile(&route.voice.engine).api_key_env)?;
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
    fn migration_preserves_symlink_parent_semantics_and_rejects_non_utf8_parent() {
        use std::os::unix::ffi::OsStringExt;
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
    fn dated_openai_translation_models_keep_continuous_restrictions() {
        let mut cfg = AppConfig::default();
        cfg.microphone.provider = "openai".into();
        cfg.providers.openai.model = "gpt-realtime-translate-2026-09-01".into();
        assert_eq!(cfg.capture_frame_ms(&cfg.microphone), 100);
        cfg.microphone.prompt = "Glossário".into();
        assert!(cfg.validate().is_err());
        cfg.microphone.prompt.clear();
        cfg.microphone.voice.voice_id = "marin".into();
        assert!(cfg.validate().is_err());
    }
    #[test]
    fn synthesis_style_limit_matches_transport_in_bytes() {
        let mut cfg = AppConfig::default();
        cfg.microphone.voice.engine = "gemini".into();
        cfg.microphone.voice.voice_id = "voice_test".into();
        cfg.microphone.voice.style = "a".repeat(2000);
        cfg.validate().unwrap();
        cfg.microphone.voice.style.push('a');
        assert!(cfg.validate().is_err());
        cfg.microphone.voice.style = "é".repeat(1000);
        cfg.validate().unwrap();
        cfg.microphone.voice.style.push('a');
        assert!(
            cfg.validate().is_err(),
            "UTF-8 style limits must count bytes"
        );
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
    fn catches_feedback_across_native_endpoint_directions() {
        assert_eq!(
            canonical_device("input:0:BlackHole 2ch"),
            canonical_device("output:2:BlackHole 2ch")
        );
        assert_eq!(
            canonical_device("input:0:CABLE-A Output (VB-Audio)"),
            canonical_device("output:3:CABLE-A Input (VB-Audio)")
        );
        let mut cfg = AppConfig::default();
        cfg.microphone.enabled = false;
        cfg.speaker.provider = "loopback".into();
        cfg.speaker.playback_device = "babel_speaker".into();
        assert!(cfg.validate_for_start().is_err());
    }
    #[test]
    fn recording_only_needs_no_cloud_or_voice_credentials() {
        let mut cfg = AppConfig::default();
        cfg.microphone.enabled = false;
        cfg.speaker.enabled = false;
        cfg.microphone.capture_device = "physical-mic".into();
        cfg.speaker.playback_device = "physical-speakers".into();
        cfg.recording.enabled = true;
        cfg.microphone.voice.engine = "elevenlabs".into();
        cfg.providers.gemini.api_key_env = "BABEL_TEST_UNUSED_ASR_KEY".into();
        cfg.providers.elevenlabs.api_key_env = "BABEL_TEST_UNUSED_TTS_KEY".into();
        cfg.validate_for_start().unwrap();
        assert_eq!(cfg.capture_frame_ms(&cfg.microphone), 20);
        cfg.recording.enabled = false;
        cfg.validate_routing().unwrap();
        assert!(cfg.validate_for_start().is_err());
    }
    #[test]
    fn transcription_without_translation_uses_only_selected_originals() {
        let mut cfg = AppConfig::default();
        cfg.microphone.enabled = false;
        cfg.speaker.enabled = false;
        cfg.microphone.capture_device = "physical-mic".into();
        cfg.microphone.provider = "local".into();
        cfg.transcription.enabled = true;
        cfg.transcription.speaker = false;
        cfg.providers.local.ollama_endpoint.clear();
        cfg.providers.local.piper_endpoint.clear();
        cfg.providers.local.translation_model.clear();
        cfg.validate_for_start().unwrap();
        cfg.microphone.provider = "loopback".into();
        assert!(cfg.validate_for_start().is_err());
    }
    #[test]
    fn feedback_is_rejected_even_when_every_feature_is_off() {
        let mut cfg = AppConfig::default();
        cfg.microphone.enabled = false;
        cfg.speaker.enabled = false;
        cfg.microphone.capture_device = "physical-mic".into();
        cfg.speaker.playback_device = "physical-speakers".into();
        cfg.validate_routing().unwrap();
        cfg.microphone.playback_device = "babel_speaker".into();
        assert!(cfg.validate_routing().is_err());
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
