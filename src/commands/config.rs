use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

/// Only local inference endpoints are accepted. MCP integrations have their own
/// transport/authentication configuration; no credential values are saved here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentConfig {
    pub enabled: bool,
    pub desktop_notifications: bool,
    pub wake_name: String,
    pub whisper_endpoint: String,
    pub whisper_language: String,
    pub whisper_api_key_env: String,
    pub needle_endpoint: String,
    pub needle_api_key_env: String,
    /// Empty uses the configuration file's directory, never a guessed port.
    pub services_directory: String,
    pub max_calls: usize,
    pub integrations: Vec<crate::mcp_client::McpIntegration>,
    pub min_confidence: f64,
    pub silence_ms: u32,
    pub max_utterance_ms: u32,
    pub command_window_secs: u32,
    pub timeout_secs: u32,
    pub vad_threshold: f32,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            desktop_notifications: true,
            wake_name: "Babel".into(),
            whisper_endpoint: "auto".into(),
            whisper_language: "auto".into(),
            whisper_api_key_env: String::new(),
            needle_endpoint: "auto".into(),
            needle_api_key_env: String::new(),
            services_directory: String::new(),
            max_calls: 4,
            integrations: Vec::new(),
            min_confidence: 0.85,
            silence_ms: 600,
            max_utterance_ms: 10_000,
            command_window_secs: 8,
            timeout_secs: 20,
            vad_threshold: 0.012,
        }
    }
}

impl AgentConfig {
    pub fn validate(&self) -> Result<()> {
        crate::mcp_client::validate_integrations(&self.integrations)?;
        ensure!(
            serde_json::to_vec(self)?.len() <= 32 * 1024,
            "voice agent configuration exceeds 32 KiB"
        );
        ensure!(
            !self.wake_name.trim().is_empty()
                && self.wake_name.len() <= 80
                && self.wake_name.chars().any(char::is_alphanumeric)
                && !self.wake_name.chars().any(char::is_control),
            "invalid voice command wake name"
        );
        for endpoint in [&self.whisper_endpoint, &self.needle_endpoint] {
            if endpoint != "auto" {
                super::inference::validate_local_endpoint(endpoint)?;
            }
        }
        if !self.services_directory.is_empty() {
            crate::storage::resolve_base(&self.services_directory)?;
        }
        ensure!(
            self.whisper_endpoint != "auto" || self.whisper_api_key_env.is_empty(),
            "Managed Whisper does not use an API key; use an explicit local endpoint for authenticated servers"
        );
        ensure!(
            self.whisper_language == "auto"
                || (self.whisper_language.len() <= 12
                    && self
                        .whisper_language
                        .bytes()
                        .all(|b| b.is_ascii_alphabetic() || b == b'-' || b == b'_')),
            "invalid command transcription language"
        );
        for credential in [&self.whisper_api_key_env, &self.needle_api_key_env] {
            ensure!(
                credential.len() <= 128
                    && credential
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
                "invalid local inference credential name"
            );
        }
        ensure!(
            (1..=8).contains(&self.max_calls),
            "voice command call limit must be 1..8"
        );
        ensure!(
            self.min_confidence.is_finite() && (0.0..=1.0).contains(&self.min_confidence),
            "command confidence must be 0..1"
        );
        ensure!(
            (200..=2_000).contains(&self.silence_ms),
            "command silence must be 200..2000 ms"
        );
        ensure!(
            (1_000..=15_000).contains(&self.max_utterance_ms)
                && self.silence_ms < self.max_utterance_ms,
            "command utterance limit must be 1000..15000 ms and longer than silence"
        );
        ensure!(
            (2..=30).contains(&self.command_window_secs),
            "command window must be 2..30 seconds"
        );
        ensure!(
            (1..=120).contains(&self.timeout_secs),
            "command timeout must be 1..120 seconds"
        );
        ensure!(
            self.vad_threshold.is_finite() && (0.0001..=0.5).contains(&self.vad_threshold),
            "command VAD threshold must be 0.0001..0.5"
        );
        Ok(())
    }
}
