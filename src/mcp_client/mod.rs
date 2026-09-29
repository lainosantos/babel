//! Registered MCP integrations. Server content is data, never an instruction source.
mod transport;

use crate::credentials;
use anyhow::{Context, Result, bail, ensure};
use rmcp::{
    RoleClient, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, ClientConfig, Implementation,
        PaginatedRequestParams, ProtocolVersion,
    },
    service::RunningService,
    transport::{
        StreamableHttpClientTransport,
        auth::{AuthorizationManager, AuthorizationRequest, AuthorizationSession, OAuthState},
        streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, Semaphore, SemaphorePermit};
use tokio_util::sync::CancellationToken;

const MAX_TOOLS: usize = 256;
const MAX_PAGES: usize = 16;
const MAX_ARGUMENT_BYTES: usize = 64 * 1024;
const MAX_SCHEMA_BYTES: usize = 64 * 1024;
const MAX_PENDING_AUTH: usize = 16;
const AUTH_TTL: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpTransport {
    #[default]
    Stdio,
    Http,
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpAuth {
    #[default]
    None,
    Bearer,
    Oauth,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret_env: String,
    pub scopes: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct McpIntegration {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub transport: McpTransport,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub secret_env: BTreeMap<String, String>,
    pub url: String,
    pub auth: McpAuth,
    pub token_env: String,
    pub headers: BTreeMap<String, String>,
    pub secret_headers: BTreeMap<String, String>,
    pub oauth: OAuthConfig,
    pub timeout_secs: u64,
    /// Empty means every tool exposed by this registered server.
    pub allowed_tools: Vec<String>,
}
impl Default for McpIntegration {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            enabled: true,
            transport: McpTransport::Stdio,
            command: String::new(),
            args: vec![],
            cwd: String::new(),
            env: BTreeMap::new(),
            secret_env: BTreeMap::new(),
            url: String::new(),
            auth: McpAuth::None,
            token_env: String::new(),
            headers: BTreeMap::new(),
            secret_headers: BTreeMap::new(),
            oauth: OAuthConfig::default(),
            timeout_secs: 30,
            allowed_tools: vec![],
        }
    }
}
impl McpIntegration {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.id.is_empty()
                && self.id.len() <= 64
                && self
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "MCP integration ID must contain 1–64 letters, digits, _ or -"
        );
        ensure!(
            self.name.len() <= 128 && !self.name.contains(['\0', '\n', '\r']),
            "Invalid MCP integration name"
        );
        ensure!(
            (1..=300).contains(&self.timeout_secs),
            "MCP timeout must be between 1 and 300 seconds"
        );
        ensure!(
            self.args.len() <= 128
                && self
                    .args
                    .iter()
                    .all(|s| s.len() <= 4096 && !s.contains('\0')),
            "Too many or invalid MCP arguments"
        );
        ensure!(
            self.allowed_tools.len() <= MAX_TOOLS
                && self.allowed_tools.iter().all(|s| valid_tool_name(s)),
            "Invalid MCP tool allowlist"
        );
        ensure!(
            self.env.len() + self.secret_env.len() <= 64
                && self.headers.len() + self.secret_headers.len() <= 32,
            "Too many MCP environment variables or headers"
        );
        for (key, value) in self.env.iter().chain(self.secret_env.iter()) {
            validate_env_name(key)?;
            ensure!(
                value.len() <= 4096 && !value.contains('\0'),
                "Invalid MCP environment value"
            );
        }
        for reference in self.secret_env.values().chain(self.secret_headers.values()) {
            validate_env_name(reference)?;
        }
        if !self.token_env.is_empty() {
            validate_env_name(&self.token_env)?;
        }
        if !self.oauth.client_secret_env.is_empty() {
            validate_env_name(&self.oauth.client_secret_env)?;
        }
        ensure!(
            self.oauth.client_id.len() <= 2048
                && self.oauth.scopes.len() <= 64
                && self.oauth.scopes.iter().all(|s| !s.is_empty()
                    && s.len() <= 256
                    && !s.chars().any(char::is_whitespace)),
            "Invalid OAuth client or scopes"
        );
        let mut seen_headers = HashSet::new();
        for (name, value) in self.headers.iter().chain(self.secret_headers.iter()) {
            let parsed = transport::validate_header_name(name)?;
            ensure!(
                seen_headers.insert(parsed.as_str().to_owned()),
                "Duplicate MCP header"
            );
            ensure!(
                value.len() <= 4096 && !value.contains(['\0', '\r', '\n']),
                "Invalid MCP header value"
            );
            if self.headers.contains_key(name) {
                reqwest_mcp::header::HeaderValue::from_str(value)
                    .map_err(|_| anyhow::anyhow!("Invalid MCP header value"))?;
            }
        }
        match self.transport {
            McpTransport::Stdio => {
                ensure!(
                    !self.command.trim().is_empty()
                        && self.command.len() <= 4096
                        && !self.command.contains('\0'),
                    "MCP executable is required"
                );
                ensure!(
                    self.cwd.len() <= 4096 && !self.cwd.contains('\0'),
                    "Invalid MCP working directory"
                );
                ensure!(
                    self.auth == McpAuth::None
                        && self.headers.is_empty()
                        && self.secret_headers.is_empty(),
                    "stdio integrations use environment authentication only"
                );
            }
            McpTransport::Http => {
                validate_http_url(&self.url)?;
                if self.auth == McpAuth::Bearer {
                    ensure!(
                        !self.token_env.is_empty(),
                        "Bearer authentication needs a credential reference"
                    );
                }
            }
        }
        Ok(())
    }
    fn permits(&self, name: &str) -> bool {
        self.allowed_tools.is_empty() || self.allowed_tools.iter().any(|allowed| allowed == name)
    }
}
pub fn validate_integrations(configs: &[McpIntegration]) -> Result<()> {
    ensure!(
        configs.len() <= 32,
        "At most 32 MCP integrations are supported"
    );
    let mut ids = HashSet::new();
    for config in configs {
        config.validate()?;
        ensure!(ids.insert(&config.id), "Duplicate MCP integration ID");
    }
    Ok(())
}
fn validate_env_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 128
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
        "Invalid credential/environment reference"
    );
    Ok(())
}
fn valid_tool_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
}
fn validate_http_url(value: &str) -> Result<reqwest_mcp::Url> {
    let url =
        reqwest_mcp::Url::parse(value).map_err(|_| anyhow::anyhow!("Invalid MCP HTTP URL"))?;
    let loopback = url.host_str().is_some_and(|h| {
        h == "localhost"
            || h.parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    ensure!(
        url.scheme() == "https" || (url.scheme() == "http" && loopback),
        "MCP requires HTTPS, except loopback HTTP"
    );
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
            && url.query().is_none(),
        "MCP URL must not contain credentials, query or fragment; use secret headers"
    );
    Ok(url)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredTool {
    pub server_id: String,
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub is_error: bool,
    /// Untrusted MCP content. Render as text; never execute or reinterpret as instructions.
    pub content: Value,
    pub structured_content: Option<Value>,
}
#[derive(Debug, Serialize)]
pub struct OAuthStart {
    pub authorization_url: String,
}
struct PendingOAuth {
    config: McpIntegration,
    state: OAuthState,
    created: Instant,
    generation: u64,
}
struct OAuthGrant {
    config: McpIntegration,
    state: Arc<Mutex<AuthorizationManager>>,
}
#[derive(Default)]
struct OAuthData {
    pending: HashMap<String, PendingOAuth>,
    grants: HashMap<String, OAuthGrant>,
    revisions: HashMap<String, (McpIntegration, u64)>,
    generation: u64,
}
pub struct McpClient {
    oauth: Mutex<OAuthData>,
    slots: Semaphore,
}
impl Default for McpClient {
    fn default() -> Self {
        // reqwest 0.13 with rustls-no-provider requires explicit installation.
        // Installation is process-wide and one-shot; another caller may win the race.
        let _ = rustls::crypto::ring::default_provider().install_default();
        Self {
            oauth: Mutex::default(),
            slots: Semaphore::new(4),
        }
    }
}
type Connection = RunningService<RoleClient, ClientConfig>;

impl McpClient {
    pub fn new() -> Self {
        Self::default()
    }
    fn acquire(&self) -> Result<SemaphorePermit<'_>> {
        self.slots.try_acquire().map_err(|_| anyhow::anyhow!("MCP is busy: at most four simultaneous operations are allowed; try again after an operation finishes"))
    }
    pub async fn oauth_status(&self, id: &str) -> bool {
        self.oauth.lock().await.grants.contains_key(id)
    }
    pub async fn oauth_disconnect(&self, id: &str) {
        let mut data = self.oauth.lock().await;
        data.grants.remove(id);
        data.pending.retain(|_, p| p.config.id != id);
        data.revisions.remove(id);
    }
    /// Call after config changes. Tokens and pending authorizations are bound to an exact config.
    pub async fn reconcile(&self, configs: &[McpIntegration]) {
        let mut data = self.oauth.lock().await;
        data.pending.retain(|_, pending| {
            configs.iter().any(|c| c == &pending.config) && pending.created.elapsed() < AUTH_TTL
        });
        data.grants
            .retain(|_, grant| configs.iter().any(|c| c == &grant.config));
        data.revisions
            .retain(|_, (config, _)| configs.contains(config));
    }
    pub async fn oauth_begin(
        &self,
        config: &McpIntegration,
        redirect_uri: &str,
    ) -> Result<OAuthStart> {
        config.validate()?;
        let _permit = self.acquire()?;
        ensure!(
            config.transport == McpTransport::Http && config.auth == McpAuth::Oauth,
            "Choose OAuth on an HTTP integration first"
        );
        let redirect = reqwest_mcp::Url::parse(redirect_uri).context("Invalid OAuth callback")?;
        ensure!(
            redirect.scheme() == "http"
                && redirect.host_str() == Some("127.0.0.1")
                && redirect.query().is_none()
                && redirect.fragment().is_none(),
            "OAuth callback must be the local Babel dashboard"
        );
        let generation = {
            let mut data = self.oauth.lock().await;
            data.pending
                .retain(|_, p| p.created.elapsed() < AUTH_TTL && p.config.id != config.id);
            ensure!(
                data.pending.len() < MAX_PENDING_AUTH,
                "Too many pending OAuth logins"
            );
            ensure!(
                data.revisions.contains_key(&config.id) || data.revisions.len() < 32,
                "Too many OAuth integrations"
            );
            data.generation = data.generation.wrapping_add(1);
            let generation = data.generation;
            data.revisions
                .insert(config.id.clone(), (config.clone(), generation));
            generation
        };
        let task = async {
            // SDK discovery/state/PKCE, wrapped with HTTPS enforcement for every OAuth request.
            let mut manager = AuthorizationManager::new_with_oauth_http_client(
                &config.url,
                Arc::new(transport::StrictOAuth::new(&config.url)?),
            )
            .await
            .map_err(|_| anyhow::anyhow!("OAuth initialization failed"))?;
            let resolution = manager.resolve_metadata().await.map_err(|_| {
                anyhow::anyhow!(
                    "OAuth discovery failed; check the server's published OAuth metadata"
                )
            })?;
            ensure!(
                resolution.source.is_discovered(),
                "MCP server must publish OAuth metadata; guessed authorization endpoints are not accepted"
            );
            ensure!(
                resolution
                    .metadata
                    .code_challenge_methods_supported
                    .as_ref()
                    .is_some_and(|methods| methods.iter().any(|m| m == "S256")),
                "OAuth server must advertise PKCE S256"
            );
            transport::validate_oauth_url(
                &resolution.metadata.authorization_endpoint,
                &config.url,
            )?;
            transport::validate_oauth_url(&resolution.metadata.token_endpoint, &config.url)?;
            if let Some(endpoint) = &resolution.metadata.registration_endpoint {
                transport::validate_oauth_url(endpoint, &config.url)?;
            }
            manager.set_metadata(resolution.metadata);
            let mut request = AuthorizationRequest::new(redirect_uri)
                .with_client_name("Babel")
                .with_scopes(config.oauth.scopes.clone());
            if !config.oauth.client_id.is_empty() {
                request = request.with_preregistered_client(&config.oauth.client_id);
            }
            if !config.oauth.client_secret_env.is_empty() {
                request = request.with_client_secret(
                    credentials::get(&config.oauth.client_secret_env)?.to_string(),
                );
            }
            let session = AuthorizationSession::new(manager, request).await.map_err(|_| anyhow::anyhow!("OAuth authorization setup failed; provide a registered client ID when the server does not support dynamic registration, and verify PKCE S256/scopes"))?;
            let state = OAuthState::Session(session);
            let authorization_url = state
                .get_authorization_url()
                .await
                .map_err(|_| anyhow::anyhow!("OAuth authorization URL unavailable"))?;
            let url = reqwest_mcp::Url::parse(&authorization_url)
                .context("Invalid OAuth authorization URL")?;
            ensure!(
                url.scheme() == "https"
                    || (url.scheme() == "http"
                        && url.host_str().is_some_and(|h| h == "localhost"
                            || h.parse::<std::net::IpAddr>()
                                .is_ok_and(|ip| ip.is_loopback()))),
                "OAuth authorization requires HTTPS"
            );
            let csrf = url
                .query_pairs()
                .find(|(name, _)| name == "state")
                .map(|(_, value)| value.into_owned())
                .context("OAuth state unavailable")?;
            let mut data = self.oauth.lock().await;
            ensure!(
                data.pending.len() < MAX_PENDING_AUTH,
                "Too many pending OAuth logins"
            );
            ensure!(
                data.revisions
                    .get(&config.id)
                    .is_some_and(|(_, revision)| *revision == generation),
                "OAuth login was cancelled or configuration changed"
            );
            data.pending.insert(
                csrf,
                PendingOAuth {
                    config: config.clone(),
                    state,
                    created: Instant::now(),
                    generation,
                },
            );
            Ok(OAuthStart { authorization_url })
        };
        tokio::time::timeout(Duration::from_secs(config.timeout_secs), task)
            .await
            .context("OAuth discovery timed out")?
    }
    pub async fn oauth_complete(
        &self,
        state: &str,
        code: &str,
        issuer: Option<&str>,
    ) -> Result<String> {
        ensure!(
            state.len() <= 4096 && code.len() <= 8192 && !code.is_empty(),
            "Invalid OAuth callback"
        );
        let _permit = self.acquire()?;
        let mut pending = self
            .oauth
            .lock()
            .await
            .pending
            .remove(state)
            .context("OAuth login expired or state is invalid; start login again")?;
        ensure!(
            pending.created.elapsed() < AUTH_TTL,
            "OAuth login expired; start login again"
        );
        tokio::time::timeout(
            Duration::from_secs(pending.config.timeout_secs),
            pending
                .state
                .handle_callback_with_issuer(code, state, issuer),
        )
        .await
        .context("OAuth token exchange timed out")?
        .map_err(|_| {
            anyhow::anyhow!("OAuth code rejected; check the callback URL and start login again")
        })?;
        let id = pending.config.id.clone();
        let manager = pending
            .state
            .into_authorization_manager()
            .context("OAuth credentials unavailable after callback")?;
        let mut data = self.oauth.lock().await;
        ensure!(
            data.revisions
                .get(&id)
                .is_some_and(|(_, revision)| *revision == pending.generation),
            "OAuth login was cancelled or configuration changed"
        );
        data.grants.insert(
            id.clone(),
            OAuthGrant {
                config: pending.config,
                state: Arc::new(Mutex::new(manager)),
            },
        );
        Ok(id)
    }
    pub async fn list_tools(
        &self,
        config: &McpIntegration,
        cancel: &CancellationToken,
    ) -> Result<Vec<DiscoveredTool>> {
        config.validate()?;
        ensure!(config.enabled, "MCP integration is disabled");
        let _permit = self.acquire()?;
        run_bounded(config, cancel, async {
            let mut conn = self.connect(config).await?;
            let result = discover(&conn, config).await;
            let _ = conn.close_with_timeout(Duration::from_secs(2)).await;
            result
        })
        .await
    }
    pub async fn test_connection(
        &self,
        config: &McpIntegration,
        cancel: &CancellationToken,
    ) -> Result<Vec<DiscoveredTool>> {
        self.list_tools(config, cancel).await
    }
    pub async fn call_tool(
        &self,
        config: &McpIntegration,
        name: &str,
        arguments: Value,
        cancel: &CancellationToken,
    ) -> Result<ToolResult> {
        config.validate()?;
        ensure!(
            config.enabled && config.permits(name) && valid_tool_name(name),
            "MCP tool is not permitted"
        );
        ensure!(arguments.is_object(), "MCP arguments must be a JSON object");
        ensure!(
            serde_json::to_vec(&arguments)?.len() <= MAX_ARGUMENT_BYTES,
            "MCP arguments exceed 64 KiB"
        );
        let _permit = self.acquire()?;
        run_bounded(config, cancel, async {
            let mut conn = self.connect(config).await?;
            let result = async {
                // Revalidate against this connection's live catalog; model-supplied identities alone are insufficient.
                let tools = discover(&conn, config).await?;
                let tool = tools.iter().find(|t| t.name == name).context("MCP tool is no longer advertised by the integration")?;
                validate_arguments(&tool.input_schema, &arguments)?;
                let params = CallToolRequestParams::new(name.to_owned()).with_arguments(arguments.as_object().expect("validated object").clone());
                let result = conn.call_tool_once(params).await.map_err(|_| anyhow::anyhow!("MCP tool request failed; it was not retried. Check the remote result before repeating the command"))?;
                match result {
                    CallToolResponse::Complete(result) => Ok(ToolResult { is_error: result.is_error.unwrap_or(false), content: serde_json::to_value(result.content)?, structured_content: result.structured_content }),
                    _ => bail!("MCP tool requires interactive input or a background task, which Babel does not support; it was not repeated"),
                }
            }.await;
            let _ = conn.close_with_timeout(Duration::from_secs(2)).await;
            result
        }).await
    }
    async fn connect(&self, config: &McpIntegration) -> Result<Connection> {
        let client = ClientConfig::new(
            Default::default(),
            Implementation::new("babel", env!("CARGO_PKG_VERSION")),
        )
        .with_protocol_version(ProtocolVersion::V_2025_11_25);
        match config.transport {
            McpTransport::Stdio => client.serve(transport::ChildTransport::spawn(config)?).await.map_err(|_| anyhow::anyhow!("MCP initialization failed; verify the executable, arguments and environment credentials")),
            McpTransport::Http => {
                let mut http_config = StreamableHttpClientTransportConfig::with_uri(config.url.clone())
                    .custom_headers(transport::headers(config)?).max_concurrent_requests(1)
                    .max_sse_event_size(transport::MAX_MESSAGE_BYTES).control_request_timeout(Duration::from_secs(2))
                    .reinit_on_expired_session(false);
                http_config.channel_buffer_capacity = 8;
                http_config.retry_config = Arc::new(transport::NoRetry);
                match config.auth {
                    McpAuth::None => {},
                    McpAuth::Bearer => http_config = http_config.auth_header(credentials::get(&config.token_env)?.to_string()),
                    McpAuth::Oauth => {
                        let state = {
                            let data = self.oauth.lock().await;
                            let grant = data.grants.get(&config.id).context("MCP OAuth login required")?;
                            ensure!(grant.config == *config, "MCP configuration changed; sign in again");
                            grant.state.clone()
                        };
                        // Refresh proactively, before sending any MCP operation; no reactive POST retries.
                        let token = state.lock().await.get_access_token().await.map_err(|_| anyhow::anyhow!("MCP OAuth token expired or could not refresh; sign in again"))?;
                        http_config = http_config.auth_header(token);
                    }
                }
                let transport = StreamableHttpClientTransport::with_client(transport::BoundedHttp::new(config.timeout_secs)?, http_config);
                client.serve(transport).await.map_err(|_| anyhow::anyhow!("MCP HTTP initialization failed; verify URL, authentication and server availability"))
            }
        }
    }
}
async fn run_bounded<T>(
    config: &McpIntegration,
    cancel: &CancellationToken,
    future: impl Future<Output = Result<T>>,
) -> Result<T> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => bail!("MCP request cancelled; a dispatched tool may already have completed remotely"),
        result = tokio::time::timeout(Duration::from_secs(config.timeout_secs), future) => result.context("MCP deadline exceeded; a dispatched tool may already have completed remotely; no automatic retry")?,
    }
}
async fn discover(conn: &Connection, config: &McpIntegration) -> Result<Vec<DiscoveredTool>> {
    let mut tools = Vec::new();
    let mut cursor = None;
    let mut cursors = HashSet::new();
    let mut names = HashSet::new();
    let mut total_schema_bytes = 0;
    for _ in 0..MAX_PAGES {
        let page = conn
            .list_tools(Some(PaginatedRequestParams::default().with_cursor(cursor)))
            .await
            .map_err(|_| anyhow::anyhow!("MCP tool listing failed"))?;
        for tool in page.tools {
            ensure!(
                names.len() < MAX_TOOLS,
                "MCP integration advertises more than 256 tools; narrow the server catalog"
            );
            ensure!(
                valid_tool_name(&tool.name) && names.insert(tool.name.to_string()),
                "MCP advertised invalid or duplicate tool names"
            );
            let input_schema = serde_json::to_value(&tool.input_schema)?;
            let size = serde_json::to_vec(&input_schema)?.len();
            total_schema_bytes += size;
            ensure!(
                size <= MAX_SCHEMA_BYTES && total_schema_bytes <= 512 * 1024,
                "MCP tool schemas are too large"
            );
            if config.permits(&tool.name) {
                tools.push(DiscoveredTool {
                    server_id: config.id.clone(),
                    name: tool.name.to_string(),
                    description: tool
                        .description
                        .unwrap_or_default()
                        .chars()
                        .take(4096)
                        .collect(),
                    input_schema,
                });
            }
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            return Ok(tools);
        }
        ensure!(
            cursor
                .as_ref()
                .is_some_and(|c| c.len() <= 4096 && cursors.insert(c.clone())),
            "MCP pagination repeats or has an invalid cursor"
        );
    }
    bail!("MCP tool listing exceeds 16 pages")
}
pub(crate) fn validate_arguments(schema: &Value, arguments: &Value) -> Result<()> {
    reject_external_references(schema)?;
    let validator = jsonschema::validator_for(schema)
        .map_err(|_| anyhow::anyhow!("MCP tool has an invalid or unsupported JSON schema"))?;
    ensure!(
        validator.is_valid(arguments),
        "Arguments do not match the MCP tool schema"
    );
    Ok(())
}
fn reject_external_references(value: &Value) -> Result<()> {
    match value {
        Value::Object(map) => {
            for (name, value) in map {
                if matches!(name.as_str(), "$ref" | "$dynamicRef" | "$recursiveRef") {
                    ensure!(
                        value.as_str().is_some_and(|s| s.starts_with('#')),
                        "MCP schemas may only reference local fragments"
                    );
                }
                if name == "$id" {
                    ensure!(
                        value
                            .as_str()
                            .is_some_and(|s| s.starts_with('#') || s.is_empty()),
                        "MCP schemas cannot declare remote base identifiers"
                    );
                }
                reject_external_references(value)?;
            }
        }
        Value::Array(items) => {
            for value in items {
                reject_external_references(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests;
