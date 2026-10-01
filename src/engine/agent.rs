use super::*;
use crate::{
    commands::{AgentConfig, CommandHistorySnapshot, CommandStatus},
    mcp_client::{McpClient, McpIntegration},
};

impl Controller {
    pub async fn agent_snapshot(&self) -> (AgentConfig, u64) {
        let state = self.state.lock().await;
        (state.config.agent.clone(), state.agent_revision)
    }
    pub async fn agent_status(&self) -> (CommandStatus, u64) {
        let state = self.state.lock().await;
        (self.commands.status(), state.agent_revision)
    }
    pub fn cancel_command(&self) {
        self.commands.cancel();
    }
    pub fn command_history(&self) -> CommandHistorySnapshot {
        self.commands.history()
    }
    pub fn clear_command_history(&self) -> CommandHistorySnapshot {
        self.commands.clear_history()
    }
    pub fn mcp(&self) -> &McpClient {
        &self.mcp
    }
    pub async fn integration(&self, id: &str) -> Result<McpIntegration> {
        self.state
            .lock()
            .await
            .config
            .agent
            .integrations
            .iter()
            .find(|c| c.id == id)
            .cloned()
            .context("MCP integration is not saved")
    }
    /// Does not replace audio devices, provider sessions or recording writers.
    pub async fn set_agent(&self, agent: AgentConfig, revision: Option<u64>) -> Result<u64> {
        agent.validate()?;
        let mut state = self.state.lock().await;
        if revision.is_some_and(|expected| expected != state.agent_revision) {
            return Err(ConfigurationChanged.into());
        }
        let mut config = state.config.clone();
        config.agent = agent.clone();
        config.save(&self.path)?;
        // Invalidate model-visible identities synchronously before publishing
        // the new configuration. Pending OAuth grants also bind exact configs.
        self.command_tools.update(agent.integrations.clone());
        self.commands.update_config(agent.clone())?;
        self.mcp.reconcile(&agent.integrations).await;
        state.config = config;
        state.agent_revision = state.agent_revision.wrapping_add(1);
        Ok(state.agent_revision)
    }
    pub async fn set_agent_credential(&self, name: &str, key: Option<String>) -> Result<()> {
        self.update_agent_credential(name, key, false).await
    }
    pub async fn set_agent_saved_credential(&self, name: &str, key: Option<String>) -> Result<()> {
        self.update_agent_credential(name, key, true).await
    }
    async fn update_agent_credential(
        &self,
        name: &str,
        key: Option<String>,
        permanent: bool,
    ) -> Result<()> {
        let key = key.map(zeroize::Zeroizing::new);
        let state = self.state.lock().await;
        let config = &state.config.agent;
        ensure!(
            !name.is_empty() && credential_refs(config).contains(&name),
            "Save this credential reference in the agent configuration first"
        );
        // Keys shared deliberately with audio providers retain their session
        // semantics; changing a command credential never restarts an audio task.
        if permanent {
            let config = state.config.clone();
            let path = self.path.clone();
            let name = name.to_owned();
            tokio::task::spawn_blocking(move || {
                crate::credentials::save(&config, &path, &name, key.map(|v| v.to_string()))
            })
            .await??;
        } else if let Some(key) = key {
            crate::credentials::set(name, key.to_string())?;
        } else {
            crate::credentials::clear(name)?;
        }
        self.command_tools.update(config.integrations.clone());
        self.commands.update_config(config.clone())?;
        for integration in &config.integrations {
            if integration_credential_refs(integration).contains(&name) {
                self.mcp.oauth_disconnect(&integration.id).await;
            }
        }
        Ok(())
    }
}

fn integration_credential_refs(config: &McpIntegration) -> Vec<&str> {
    std::iter::once(config.token_env.as_str())
        .chain(std::iter::once(config.oauth.client_secret_env.as_str()))
        .chain(config.secret_env.values().map(String::as_str))
        .chain(config.secret_headers.values().map(String::as_str))
        .collect()
}
fn credential_refs(config: &AgentConfig) -> Vec<&str> {
    [&config.whisper_api_key_env, &config.needle_api_key_env]
        .into_iter()
        .map(String::as_str)
        .chain(
            config
                .integrations
                .iter()
                .flat_map(integration_credential_refs),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn independent_revisions_preserve_agent_when_main_form_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        let controller =
            Controller::new(AppConfig::default(), dir.path().join("config.toml")).unwrap();
        let (old_main, main_revision) = controller.config_snapshot().await;
        let (mut agent, revision) = controller.agent_snapshot().await;
        agent.wake_name = "Jarvis".into();
        assert_eq!(
            controller
                .set_agent(agent.clone(), Some(revision))
                .await
                .unwrap(),
            1
        );
        assert_eq!(controller.config_snapshot().await.1, main_revision);
        controller
            .set_config_if_revision(old_main, Some(main_revision))
            .await
            .unwrap();
        assert_eq!(controller.agent_snapshot().await.0.wake_name, "Jarvis");
        assert!(controller.set_agent(agent, Some(revision)).await.is_err());
        assert_eq!(
            AppConfig::load(controller.config_path())
                .unwrap()
                .agent
                .wake_name,
            "Jarvis"
        );
    }
    #[tokio::test]
    async fn failed_save_does_not_publish_agent_or_replace_workers() {
        let dir = tempfile::tempdir().unwrap();
        let controller = Controller::new(AppConfig::default(), dir.path().to_owned()).unwrap();
        let agent = AgentConfig {
            wake_name: "Other".into(),
            ..Default::default()
        };
        assert!(controller.set_agent(agent, Some(0)).await.is_err());
        assert_eq!(controller.agent_snapshot().await.0.wake_name, "Babel");
        assert_eq!(controller.agent_snapshot().await.1, 0);
        assert_eq!(controller.agent_status().await.0.wake_name, "Babel");
        assert!(
            controller
                .set_agent_credential("UNREGISTERED", Some("secret".into()))
                .await
                .is_err()
        );
        assert!(!crate::credentials::configured("UNREGISTERED"));
    }
}
