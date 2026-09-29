//! Bind model-visible identities to an exact, saved MCP configuration.
use crate::{
    commands::{CommandTool, CommandTools},
    mcp_client::{McpClient, McpIntegration},
};
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct Entry {
    integration: McpIntegration,
    name: String,
    schema: Value,
}
struct Registry {
    generation: u64,
    configs: Vec<McpIntegration>,
    tools: HashMap<String, Entry>,
}
pub(super) struct AgentTools {
    mcp: Arc<McpClient>,
    registry: Mutex<Registry>,
}
impl AgentTools {
    pub fn new(mcp: Arc<McpClient>, configs: Vec<McpIntegration>) -> Self {
        Self {
            mcp,
            registry: Mutex::new(Registry {
                generation: 0,
                configs,
                tools: HashMap::new(),
            }),
        }
    }
    pub fn update(&self, configs: Vec<McpIntegration>) {
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        registry.generation = registry.generation.wrapping_add(1);
        registry.configs = configs;
        registry.tools.clear();
    }
    fn entry(&self, id: &str, arguments: &Value) -> Result<Entry> {
        let registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        let entry = registry
            .tools
            .get(id)
            .context("Unknown or expired command tool identity")?;
        ensure!(
            registry
                .configs
                .iter()
                .any(|c| c.enabled && c == &entry.integration),
            "MCP integration changed; say the command again"
        );
        ensure!(
            arguments.is_object() && serde_json::to_vec(arguments)?.len() <= 65_536,
            "Invalid or oversized command arguments"
        );
        crate::mcp_client::validate_arguments(&entry.schema, arguments)?;
        Ok(entry.clone())
    }
}
#[async_trait]
impl CommandTools for AgentTools {
    async fn list_tools(&self) -> Result<Vec<CommandTool>> {
        let (generation, configs) = {
            let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
            registry.generation = registry.generation.wrapping_add(1);
            registry.tools.clear();
            (registry.generation, registry.configs.clone())
        };
        let cancel = CancellationToken::new();
        let _cancel_on_drop = cancel.clone().drop_guard();
        let mut entries = HashMap::new();
        let mut tools = Vec::new();
        let mut bytes = 0;
        for integration in configs.iter().filter(|config| config.enabled) {
            for tool in self.mcp.list_tools(integration, &cancel).await? {
                bytes += serde_json::to_vec(&tool)?.len();
                ensure!(
                    tools.len() < 256 && bytes <= 512 * 1024,
                    "Enabled MCP tool catalog exceeds 256 tools or 512 KiB; restrict allowed tools"
                );
                let id = format!("babel_{generation}_{}", tools.len());
                entries.insert(
                    id.clone(),
                    Entry {
                        integration: integration.clone(),
                        name: tool.name.clone(),
                        schema: tool.input_schema.clone(),
                    },
                );
                tools.push(CommandTool {
                    id,
                    name: format!("{} / {}", integration.name, tool.name),
                    description: tool.description,
                    input_schema: tool.input_schema,
                });
            }
        }
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        ensure!(
            registry.generation == generation,
            "MCP configuration changed during tool discovery"
        );
        registry.tools = entries;
        Ok(tools)
    }
    async fn validate_call(&self, id: &str, arguments: &Value) -> Result<()> {
        self.entry(id, arguments)?;
        Ok(())
    }
    async fn call_tool(
        &self,
        id: &str,
        arguments: Value,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        ensure!(
            !cancel.is_cancelled(),
            "Voice command cancelled before dispatch"
        );
        let entry = self.entry(id, &arguments)?;
        let result = self
            .mcp
            .call_tool(&entry.integration, &entry.name, arguments, cancel)
            .await?;
        ensure!(
            !result.is_error,
            "MCP tool reported a failure; inspect its result before repeating the command"
        );
        Ok(serde_json::to_value(result)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn stale_id_and_invalid_arguments_never_dispatch() {
        let integration = McpIntegration {
            id: "test".into(),
            command: "unused".into(),
            ..Default::default()
        };
        let tools = AgentTools::new(Arc::new(McpClient::new()), vec![integration.clone()]);
        tools.registry.lock().unwrap().tools.insert("opaque".into(), Entry { integration: integration.clone(), name: "tool".into(), schema: serde_json::json!({"type":"object", "required":["text"], "properties":{"text":{"type":"string"}}, "additionalProperties":false}) });
        assert!(
            tools
                .validate_call("made_up", &serde_json::json!({}))
                .await
                .is_err()
        );
        assert!(
            tools
                .validate_call("opaque", &serde_json::json!({"text":2}))
                .await
                .is_err()
        );
        assert!(
            tools
                .validate_call("opaque", &serde_json::json!({"text":"hello"}))
                .await
                .is_ok()
        );
        tools.update(vec![integration]);
        assert!(
            tools
                .validate_call("opaque", &serde_json::json!({"text":"hello"}))
                .await
                .is_err()
        );
    }
}
