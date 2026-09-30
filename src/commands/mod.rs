//! Local wake-name commands, independent of translation and recording.
//! Audio arrives through a bounded tap of the original microphone: this module
//! never opens a device and never consumes translated/output audio.
mod config;
mod feedback;
mod inference;
mod local_services;
mod service;

pub use config::AgentConfig;
pub use feedback::{CommandFeedback, CommandFeedbackPhase};
pub use service::CommandService;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandTool {
    /// Stable identity including the integration, never a model-generated name.
    pub id: String,
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[async_trait]
pub trait CommandTools: Send + Sync {
    async fn list_tools(&self) -> Result<Vec<CommandTool>>;
    /// Preflight every call in a batch before any tool has side effects.
    async fn validate_call(&self, id: &str, arguments: &Value) -> Result<()>;
    /// Implementations MUST revalidate the exact identity, integration enabled
    /// state, and arguments against the current tool schema before dispatch.
    async fn call_tool(
        &self,
        id: &str,
        arguments: Value,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Value>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandPhase {
    Disabled,
    Inactive,
    Listening,
    Activated,
    Transcribing,
    Deciding,
    Executing,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandErrorScope {
    Service,
    Command,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandStatus {
    pub phase: CommandPhase,
    pub wake_name: String,
    pub microphone_active: bool,
    pub sequence: u64,
    pub activation_id: u64,
    pub command: Option<String>,
    pub tool: Option<String>,
    pub result: Option<String>,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_scope: Option<CommandErrorScope>,
    pub dropped_frames: u64,
    #[serde(default)]
    pub whisper_endpoint: Option<String>,
    #[serde(default)]
    pub needle_endpoint: Option<String>,
    /// Sanitized visual state retained across fast command transitions. Its
    /// monotonic age lets newly opened dashboards ignore an old result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<CommandFeedback>,
}

impl CommandStatus {
    fn initial(config: &AgentConfig) -> Self {
        Self {
            phase: if config.enabled {
                CommandPhase::Inactive
            } else {
                CommandPhase::Disabled
            },
            wake_name: config.wake_name.clone(),
            microphone_active: false,
            sequence: 0,
            activation_id: 0,
            command: None,
            tool: None,
            result: None,
            error: None,
            error_scope: None,
            dropped_frames: 0,
            whisper_endpoint: None,
            needle_endpoint: None,
            feedback: None,
        }
    }
}

#[cfg(test)]
mod tests;
