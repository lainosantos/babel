use super::*;
use crate::{commands::AgentConfig, mcp_client::McpAuth};

pub(super) fn routes() -> Router<DashboardState> {
    Router::new()
        .route("/agent", get(config).put(save))
        .route("/agent/status", get(status))
        .route("/agent/cancel", post(cancel))
        .route("/agent/integrations/test", post(test))
        .route("/agent/integrations/status", get(auth_status))
        .route("/agent/oauth/begin", post(oauth_begin))
        .route("/agent/oauth/disconnect", post(oauth_disconnect))
        .route("/agent/credentials", post(credential))
        .route("/agent/credentials/clear", post(clear_credential))
}
async fn config(State(state): State<DashboardState>) -> impl IntoResponse {
    let (config, revision) = state.controller.agent_snapshot().await;
    ([(header::ETAG, format!("\"{revision}\""))], Json(config))
}
async fn save(
    State(state): State<DashboardState>,
    headers: HeaderMap,
    Json(config): Json<AgentConfig>,
) -> Response {
    let revision = match expected_revision(&headers) {
        Ok(r) => r,
        Err(e) => return api_error(StatusCode::BAD_REQUEST, e),
    };
    let _guard = state.mutations.lock().await;
    match state.controller.set_agent(config, revision).await {
        Ok(revision) => (
            [(header::ETAG, format!("\"{revision}\""))],
            Json(json!({"ok":true})),
        )
            .into_response(),
        Err(error) => operation_result(Err(error)),
    }
}
async fn status(State(state): State<DashboardState>) -> Json<serde_json::Value> {
    let (mut status, revision) = state.controller.agent_status().await;
    if let Some(error) = &mut status.error {
        *error = localized(&*error);
    }
    let mut value = serde_json::to_value(status).expect("serializable command status");
    value["config_revision"] = revision.into();
    Json(value)
}
async fn cancel(State(state): State<DashboardState>) -> Response {
    state.controller.cancel_command();
    operation_result(Ok(()))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IntegrationId {
    id: String,
}
async fn test(State(state): State<DashboardState>, Json(request): Json<IntegrationId>) -> Response {
    let config = match state.controller.integration(&request.id).await {
        Ok(config) => config,
        Err(e) => return operation_result(Err(e)),
    };
    let cancel = CancellationToken::new();
    let _guard = cancel.clone().drop_guard();
    match state
        .controller
        .mcp()
        .test_connection(&config, &cancel)
        .await
    {
        Ok(tools) => Json(json!({"tools":tools})).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, error.to_string()),
    }
}
async fn auth_status(State(state): State<DashboardState>) -> Json<serde_json::Value> {
    let (config, _) = state.controller.agent_snapshot().await;
    let mut result = Vec::new();
    for integration in &config.integrations {
        let authenticated = match integration.auth {
            McpAuth::None => true,
            McpAuth::Bearer => crate::credentials::configured(&integration.token_env),
            McpAuth::Oauth => state.controller.mcp().oauth_status(&integration.id).await,
        };
        result.push(json!({"id":integration.id,"authenticated":authenticated}));
    }
    Json(json!(result))
}
async fn oauth_begin(
    State(state): State<DashboardState>,
    Json(request): Json<IntegrationId>,
) -> Response {
    let _guard = state.mutations.lock().await;
    let config = match state.controller.integration(&request.id).await {
        Ok(config) => config,
        Err(e) => return operation_result(Err(e)),
    };
    let redirect = format!("http://127.0.0.1:{}/api/agent/oauth/callback", state.port);
    match state.controller.mcp().oauth_begin(&config, &redirect).await {
        Ok(result) => Json(result).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, error.to_string()),
    }
}
async fn oauth_disconnect(
    State(state): State<DashboardState>,
    Json(request): Json<IntegrationId>,
) -> Response {
    let _guard = state.mutations.lock().await;
    if let Err(error) = state.controller.integration(&request.id).await {
        return operation_result(Err(error));
    }
    state.controller.cancel_command();
    state.controller.mcp().oauth_disconnect(&request.id).await;
    operation_result(Ok(()))
}
#[derive(Deserialize)]
pub(super) struct Callback {
    state: Option<String>,
    code: Option<String>,
    iss: Option<String>,
    error: Option<String>,
}
impl Drop for Callback {
    fn drop(&mut self) {
        if let Some(code) = &mut self.code {
            code.zeroize();
        }
    }
}
pub(super) async fn oauth_callback(
    State(state): State<DashboardState>,
    Query(query): Query<Callback>,
) -> Response {
    let _guard = state.mutations.lock().await;
    let success = if query.error.is_some() {
        false
    } else if let (Some(csrf), Some(code)) = (&query.state, &query.code) {
        if csrf.len() > 4096 || code.len() > 8192 {
            false
        } else {
            state
                .controller
                .mcp()
                .oauth_complete(csrf, code, query.iss.as_deref())
                .await
                .is_ok()
        }
    } else {
        false
    };
    let language =
        crate::i18n::resolve_language(&state.controller.interface_snapshot().await.0.language);
    let message = crate::interface_messages::localize(
        &language,
        if success {
            "Integration connected. Return to the Babel dashboard; you may close this window."
        } else {
            "Could not connect. Return to the Babel dashboard and restart authorization."
        },
    );
    // Static text only. Never reflect provider errors, codes or tokens into HTML.
    (if success { StatusCode::OK } else { StatusCode::BAD_REQUEST }, Html(format!("<!doctype html><html lang=\"{}\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>Babel · MCP</title><h1>Babel · MCP</h1><p>{message}</p></html>", language))).into_response()
}
async fn credential(
    State(state): State<DashboardState>,
    Json(mut request): Json<CredentialInput>,
) -> Response {
    let _guard = state.mutations.lock().await;
    operation_result(
        state
            .controller
            .set_agent_credential(&request.api_key_env, Some(std::mem::take(&mut request.key)))
            .await,
    )
}
async fn clear_credential(
    State(state): State<DashboardState>,
    Json(request): Json<CredentialQuery>,
) -> Response {
    let _guard = state.mutations.lock().await;
    operation_result(
        state
            .controller
            .set_agent_credential(&request.api_key_env, None)
            .await,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use tower::ServiceExt;

    fn request(method: &str, path: &str, body: serde_json::Value) -> Request {
        Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "127.0.0.1:8765")
            .header(header::AUTHORIZATION, "Bearer fixture-token")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }
    fn fixture(path: std::path::PathBuf) -> (Router, Arc<Controller>) {
        let controller = Arc::new(Controller::new(AppConfig::default(), path).unwrap());
        (
            super::super::router(DashboardState {
                controller: controller.clone(),
                token: Arc::from("fixture-token"),
                port: 8765,
                mutations: Arc::new(tokio::sync::Mutex::new(())),
            }),
            controller,
        )
    }
    #[tokio::test]
    async fn all_agent_operations_require_bearer_but_callback_only_requires_valid_oauth_state() {
        let dir = tempfile::tempdir().unwrap();
        let (app, _) = fixture(dir.path().join("config.toml"));
        for (method, path) in [
            ("GET", "/api/agent"),
            ("PUT", "/api/agent"),
            ("GET", "/api/agent/status"),
            ("POST", "/api/agent/cancel"),
            ("POST", "/api/agent/integrations/test"),
            ("GET", "/api/agent/integrations/status"),
            ("POST", "/api/agent/oauth/begin"),
            ("POST", "/api/agent/oauth/disconnect"),
            ("POST", "/api/agent/credentials"),
            ("POST", "/api/agent/credentials/clear"),
        ] {
            let mut req = request(method, path, json!({}));
            req.headers_mut().remove(header::AUTHORIZATION);
            assert_eq!(
                app.clone().oneshot(req).await.unwrap().status(),
                StatusCode::UNAUTHORIZED,
                "{path}"
            );
        }
        let mut callback = request(
            "GET",
            "/api/agent/oauth/callback?state=unknown&code=secret-code&error=untrusted",
            json!(null),
        );
        callback.headers_mut().remove(header::AUTHORIZATION);
        let response = app.clone().oneshot(callback).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let body = to_bytes(response.into_body(), 8192).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            !body.contains("secret-code")
                && !body.contains("untrusted")
                && !body.contains("unknown")
        );
        let mut cross = request(
            "GET",
            "/api/agent/oauth/callback?state=unknown&code=test",
            json!(null),
        );
        cross
            .headers_mut()
            .insert(header::ORIGIN, "https://attacker.invalid".parse().unwrap());
        assert_eq!(
            app.oneshot(cross).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
    #[tokio::test]
    async fn agent_save_uses_independent_etag_and_credentials_never_enter_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let (app, controller) = fixture(dir.path().join("config.toml"));
        let initial = app
            .clone()
            .oneshot(request("GET", "/api/agent", json!(null)))
            .await
            .unwrap();
        assert_eq!(initial.headers()[header::ETAG], "\"0\"");
        let config = AgentConfig {
            wake_name: "Atlas".into(),
            needle_api_key_env: "BABEL_AGENT_HTTP_TEST_37942".into(),
            ..Default::default()
        };
        let mut save = request("PUT", "/api/agent", json!(config));
        save.headers_mut()
            .insert(header::IF_MATCH, "\"0\"".parse().unwrap());
        let response = app.clone().oneshot(save).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::ETAG], "\"1\"");
        assert_eq!(controller.config_snapshot().await.1, 0);
        let mut stale = request("PUT", "/api/agent", json!(config));
        stale
            .headers_mut()
            .insert(header::IF_MATCH, "\"0\"".parse().unwrap());
        assert_eq!(
            app.clone().oneshot(stale).await.unwrap().status(),
            StatusCode::PRECONDITION_FAILED
        );
        assert_eq!(
            app.clone()
                .oneshot(request(
                    "POST",
                    "/api/agent/credentials",
                    json!({"api_key_env":config.needle_api_key_env,"key":"test-key-never-persist"})
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let saved = std::fs::read_to_string(controller.config_path()).unwrap();
        assert!(!saved.contains("test-key-never-persist"));
        assert!(crate::credentials::configured(&config.needle_api_key_env));
        assert_eq!(
            app.clone()
                .oneshot(request(
                    "POST",
                    "/api/agent/credentials/clear",
                    json!({"api_key_env":config.needle_api_key_env})
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert!(!crate::credentials::configured(&config.needle_api_key_env));
        let status = app
            .oneshot(request("GET", "/api/agent/status", json!(null)))
            .await
            .unwrap();
        let status: serde_json::Value =
            serde_json::from_slice(&to_bytes(status.into_body(), 8192).await.unwrap()).unwrap();
        assert_eq!(status["wake_name"], "Atlas");
        assert_eq!(status["config_revision"], 1);
        assert_eq!(status["microphone_active"], false);
    }
}
