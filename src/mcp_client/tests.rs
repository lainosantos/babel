use super::*;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
struct Fixture {
    calls: AtomicUsize,
    mode: &'static str,
    authenticated: bool,
}
async fn rpc(
    State(state): State<Arc<Fixture>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    if state.authenticated
        && (headers.get("Authorization").and_then(|h| h.to_str().ok())
            != Some("Bearer local-test-token")
            || headers.get("x-api-key").and_then(|h| h.to_str().ok()) != Some("local-extra-token"))
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let result = match request["method"].as_str().unwrap_or_default() {
        "initialize" => json!({"protocolVersion":"2025-11-25", "capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}),
        "notifications/initialized" => return StatusCode::ACCEPTED.into_response(),
        "tools/list" => {
            let mut result = json!({"tools":[{"name":"echo","description":"Untrusted fixture description", "inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"],"additionalProperties":false}}]});
            if state.mode == "pages" { result["nextCursor"] = json!("repeat"); }
            if state.mode == "large" { return Json(json!({"padding":"a".repeat(transport::MAX_MESSAGE_BYTES + 1)})).into_response(); }
            result
        },
        "tools/call" => {
            state.calls.fetch_add(1, Ordering::Relaxed);
            if state.mode == "expired" { return StatusCode::NOT_FOUND.into_response(); }
            if state.mode == "slow" { tokio::time::sleep(Duration::from_secs(3)).await; }
            json!({"content":[{"type":"text","text":"done"}],"isError":state.mode == "error"})
        },
        "notifications/cancelled" => return StatusCode::ACCEPTED.into_response(),
        other => return Json(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("Unsupported {other}")}})).into_response(),
    };
    let body = json!({"jsonrpc":"2.0","id":id,"result":result});
    if state.mode == "sse" && request["method"] == "tools/list" {
        return (
            [("Content-Type", "text/event-stream")],
            format!("event: message\ndata: {body}\n\n"),
        )
            .into_response();
    }
    ([("Mcp-Session-Id", "fixture-session")], Json(body)).into_response()
}
async fn fixture(
    mode: &'static str,
    authenticated: bool,
) -> (McpIntegration, Arc<Fixture>, tokio::task::JoinHandle<()>) {
    let state = Arc::new(Fixture {
        mode,
        authenticated,
        ..Default::default()
    });
    let app = Router::new()
        .route(
            "/mcp",
            post(rpc).delete(|| async { StatusCode::NO_CONTENT }),
        )
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (
        McpIntegration {
            id: "fixture".into(),
            name: "Fixture".into(),
            transport: McpTransport::Http,
            url,
            ..Default::default()
        },
        state,
        task,
    )
}
#[test]
fn validate_registry_and_transports() {
    let good = McpIntegration {
        id: "local".into(),
        command: "program".into(),
        ..Default::default()
    };
    assert!(good.validate().is_ok());
    assert!(validate_integrations(&[good.clone(), good]).is_err());
    for url in [
        "http://example.org/mcp",
        "https://user:secret@example.org/mcp",
        "https://example.org/mcp?token=secret",
        "file:///tmp/mcp",
    ] {
        assert!(validate_http_url(url).is_err());
    }
    assert!(validate_http_url("https://example.org/mcp").is_ok());
    assert!(validate_http_url("http://127.0.0.1:9478/mcp").is_ok());
    assert!(transport::validate_header_name("Authorization").is_err());
    assert!(transport::validate_header_name("Mcp-Session-Id").is_err());
}
#[test]
fn schemas_validate_arguments_without_network_resolution() {
    let schema = json!({"type":"object","properties":{"x":{"type":"integer"}},"required":["x"],"additionalProperties":false});
    assert!(validate_arguments(&schema, &json!({"x":2})).is_ok());
    assert!(validate_arguments(&schema, &json!({"x":"2"})).is_err());
    assert!(
        validate_arguments(
            &json!({"$ref":"https://malicious.invalid/schema"}),
            &json!({})
        )
        .is_err()
    );
    assert!(validate_arguments(&json!({"$ref":"file:///etc/passwd"}), &json!({})).is_err());
    assert!(
        validate_arguments(
            &json!({"$defs":{"x":{"type":"integer"}},"$ref":"#/$defs/x"}),
            &json!(2)
        )
        .is_ok()
    );
}
#[tokio::test]
async fn http_handshake_bearer_and_secret_header_work() {
    let (mut config, state, task) = fixture("json", true).await;
    let first = "BABEL_MCP_TEST_TOKEN_9333";
    let second = "BABEL_MCP_TEST_HEADER_9333";
    credentials::set(first, "local-test-token".into()).unwrap();
    credentials::set(second, "local-extra-token".into()).unwrap();
    config.auth = McpAuth::Bearer;
    config.token_env = first.into();
    config
        .secret_headers
        .insert("x-api-key".into(), second.into());
    let client = McpClient::new();
    let tools = client
        .list_tools(&config, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "echo");
    let result = client
        .call_tool(
            &config,
            "echo",
            json!({"text":"hello"}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(!result.is_error);
    assert_eq!(state.calls.load(Ordering::Relaxed), 1);
    credentials::clear(first).unwrap();
    credentials::clear(second).unwrap();
    task.abort();
}
#[tokio::test]
async fn live_schema_and_allowlist_reject_calls_before_dispatch() {
    let (mut config, state, task) = fixture("json", false).await;
    let client = McpClient::new();
    assert!(
        client
            .call_tool(
                &config,
                "echo",
                json!({"text":17}),
                &CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert!(
        client
            .call_tool(&config, "invented", json!({}), &CancellationToken::new())
            .await
            .is_err()
    );
    config.allowed_tools = vec!["other".into()];
    assert!(
        client
            .call_tool(
                &config,
                "echo",
                json!({"text":"hello"}),
                &CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert_eq!(state.calls.load(Ordering::Relaxed), 0);
    task.abort();
}
#[tokio::test]
async fn sse_results_and_tool_error_flag_are_preserved() {
    let (config, _, task) = fixture("sse", false).await;
    assert_eq!(
        McpClient::new()
            .list_tools(&config, &CancellationToken::new())
            .await
            .unwrap()
            .len(),
        1
    );
    task.abort();
    let (config, _, task) = fixture("error", false).await;
    assert!(
        McpClient::new()
            .call_tool(
                &config,
                "echo",
                json!({"text":"x"}),
                &CancellationToken::new()
            )
            .await
            .unwrap()
            .is_error
    );
    task.abort();
}
#[tokio::test]
async fn pagination_and_response_size_are_bounded() {
    for mode in ["pages", "large"] {
        let (config, _, task) = fixture(mode, false).await;
        assert!(
            McpClient::new()
                .list_tools(&config, &CancellationToken::new())
                .await
                .is_err()
        );
        task.abort();
    }
}
#[tokio::test]
async fn expired_session_does_not_repeat_tool_side_effects() {
    let (config, state, task) = fixture("expired", false).await;
    assert!(
        McpClient::new()
            .call_tool(
                &config,
                "echo",
                json!({"text":"x"}),
                &CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert_eq!(state.calls.load(Ordering::Relaxed), 1);
    task.abort();
}
#[tokio::test]
async fn cancelled_call_returns_without_retry() {
    let (config, state, task) = fixture("slow", false).await;
    let cancel = CancellationToken::new();
    let cancel_child = cancel.clone();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        cancel_child.cancel();
    });
    let now = Instant::now();
    assert!(
        McpClient::new()
            .call_tool(&config, "echo", json!({"text":"x"}), &cancel)
            .await
            .is_err()
    );
    assert!(now.elapsed() < Duration::from_secs(1));
    assert_eq!(state.calls.load(Ordering::Relaxed), 1);
    canceller.await.unwrap();
    task.abort();
}
#[tokio::test]
async fn callback_without_valid_pending_state_is_rejected() {
    let client = McpClient::new();
    assert!(
        client
            .oauth_complete("invented", "test-code", None)
            .await
            .is_err()
    );
    assert!(!client.oauth_status("fixture").await);
}
#[cfg(unix)]
#[tokio::test]
async fn stdio_initializes_lists_and_calls_local_fixture() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("mcp.py");
    std::fs::write(&script, r#"import sys,json,os
for line in sys.stdin:
    q=json.loads(line)
    method=q.get('method','')
    if method=='initialize': r={'protocolVersion':'2025-11-25','capabilities':{'tools':{}},'serverInfo':{'name':'fixture','version':'1'}}
    elif method=='tools/list': r={'tools':[{'name':'echo','inputSchema':{'type':'object'}}]}
    elif method=='tools/call': r={'content':[{'type':'text','text':os.environ.get('TEST_SECRET','missing')}],'isError':False}
    else: continue
    print(json.dumps({'jsonrpc':'2.0','id':q['id'],'result':r}),flush=True)
"#).unwrap();
    let reference = "BABEL_MCP_TEST_STDIO_SECRET_8439";
    credentials::set(reference, "mapped-secret".into()).unwrap();
    let config = McpIntegration {
        id: "stdio-test".into(),
        command: "python3".into(),
        args: vec![script.display().to_string()],
        secret_env: BTreeMap::from([("TEST_SECRET".into(), reference.into())]),
        ..Default::default()
    };
    let result = McpClient::new()
        .call_tool(&config, "echo", json!({}), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result.content[0]["text"], "mapped-secret");
    credentials::clear(reference).unwrap();
}

struct OAuthFixture {
    origin: String,
    token_calls: AtomicUsize,
    refresh_calls: AtomicUsize,
}
async fn resource_metadata(State(state): State<Arc<OAuthFixture>>) -> Json<Value> {
    Json(
        json!({"resource":format!("{}/mcp",state.origin),"authorization_servers":[state.origin],"scopes_supported":["tools:read"]}),
    )
}
async fn authorization_metadata(State(state): State<Arc<OAuthFixture>>) -> Json<Value> {
    Json(
        json!({"issuer":state.origin,"authorization_endpoint":format!("{}/authorize",state.origin),"token_endpoint":format!("{}/token",state.origin),"response_types_supported":["code"],"grant_types_supported":["authorization_code","refresh_token"],"code_challenge_methods_supported":["S256"],"token_endpoint_auth_methods_supported":["none"],"scopes_supported":["tools:read"]}),
    )
}
async fn oauth_probe(State(state): State<Arc<OAuthFixture>>, headers: HeaderMap) -> Response {
    if headers.get("Authorization").and_then(|h| h.to_str().ok())
        == Some("Bearer fixture-access-refreshed")
    {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    (
        StatusCode::UNAUTHORIZED,
        [(
            "WWW-Authenticate",
            format!(
                "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource/mcp\"",
                state.origin
            ),
        )],
    )
        .into_response()
}
async fn oauth_token(
    State(state): State<Arc<OAuthFixture>>,
    axum::Form(form): axum::Form<BTreeMap<String, String>>,
) -> Response {
    if form.get("resource") != Some(&format!("{}/mcp", state.origin)) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if form.get("grant_type").is_some_and(|g| g == "refresh_token") {
        state.refresh_calls.fetch_add(1, Ordering::Relaxed);
        assert_eq!(
            form.get("refresh_token").map(String::as_str),
            Some("fixture-refresh")
        );
        return Json(json!({"access_token":"fixture-access-refreshed","token_type":"Bearer","expires_in":3600,"refresh_token":"fixture-refresh-rotated"})).into_response();
    }
    state.token_calls.fetch_add(1, Ordering::Relaxed);
    assert_eq!(form.get("code").map(String::as_str), Some("fixture-code"));
    assert!(form.get("code_verifier").is_some_and(|s| s.len() >= 43));
    Json(json!({"access_token":"fixture-access-expired","token_type":"Bearer","expires_in":0,"refresh_token":"fixture-refresh"})).into_response()
}
async fn oauth_rpc(
    State(_state): State<Arc<OAuthFixture>>,
    headers: HeaderMap,
    body: Json<Value>,
) -> Response {
    if headers.get("Authorization").and_then(|h| h.to_str().ok())
        != Some("Bearer fixture-access-refreshed")
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    rpc(State(Arc::new(Fixture::default())), headers, body).await
}
#[tokio::test]
async fn oauth_pkce_discovery_callback_refresh_and_invalidation_work() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let state = Arc::new(OAuthFixture {
        origin: origin.clone(),
        token_calls: AtomicUsize::new(0),
        refresh_calls: AtomicUsize::new(0),
    });
    let app = Router::new()
        .route(
            "/mcp",
            post(oauth_rpc)
                .get(oauth_probe)
                .delete(|| async { StatusCode::NO_CONTENT }),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            axum::routing::get(resource_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource",
            axum::routing::get(resource_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            axum::routing::get(authorization_metadata),
        )
        .route("/token", post(oauth_token))
        .with_state(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut config = McpIntegration {
        id: "oauth-fixture".into(),
        transport: McpTransport::Http,
        url: format!("{origin}/mcp"),
        auth: McpAuth::Oauth,
        oauth: OAuthConfig {
            client_id: "registered-babel".into(),
            scopes: vec!["tools:read".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let client = McpClient::new();
    let start = client
        .oauth_begin(&config, "http://127.0.0.1:9473/api/agent/oauth/callback")
        .await
        .unwrap();
    let url = reqwest_mcp::Url::parse(&start.authorization_url).unwrap();
    let params: BTreeMap<_, _> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(
        params.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    assert_eq!(params.get("resource"), Some(&config.url));
    assert_eq!(
        params.get("client_id").map(String::as_str),
        Some("registered-babel")
    );
    assert!(params.get("code_challenge").is_some_and(|v| v.len() == 43));
    let csrf = params.get("state").unwrap();
    assert!(
        client
            .oauth_complete("wrong-state", "fixture-code", Some(&origin))
            .await
            .is_err()
    );
    assert_eq!(
        client
            .oauth_complete(csrf, "fixture-code", Some(&origin))
            .await
            .unwrap(),
        config.id
    );
    assert!(client.oauth_status(&config.id).await);
    assert!(
        client
            .oauth_complete(csrf, "fixture-code", Some(&origin))
            .await
            .is_err()
    );
    assert_eq!(
        client
            .list_tools(&config, &CancellationToken::new())
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(state.token_calls.load(Ordering::Relaxed), 1);
    assert_eq!(state.refresh_calls.load(Ordering::Relaxed), 1);
    config.oauth.scopes.push("changed".into());
    assert!(
        client
            .list_tools(&config, &CancellationToken::new())
            .await
            .is_err()
    );
    client.reconcile(&[config.clone()]).await;
    assert!(!client.oauth_status(&config.id).await);
    let start = client
        .oauth_begin(&config, "http://127.0.0.1:9473/api/agent/oauth/callback")
        .await
        .unwrap();
    let url = reqwest_mcp::Url::parse(&start.authorization_url).unwrap();
    let csrf = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    client.oauth_disconnect(&config.id).await;
    assert!(
        client
            .oauth_complete(&csrf, "fixture-code", Some(&origin))
            .await
            .is_err()
    );
    server.abort();
}

#[tokio::test]
async fn simultaneous_operations_are_bounded_and_cancellation_releases_slots() {
    let (config, state, server) = fixture("slow", false).await;
    let client = Arc::new(McpClient::new());
    let cancel = CancellationToken::new();
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let client = client.clone();
        let config = config.clone();
        let cancel = cancel.clone();
        tasks.push(tokio::spawn(async move {
            client
                .call_tool(&config, "echo", json!({"text":"x"}), &cancel)
                .await
        }));
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        while state.calls.load(Ordering::Relaxed) != 4 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let now = Instant::now();
    let error = client
        .call_tool(
            &config,
            "echo",
            json!({"text":"fifth"}),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("MCP is busy"));
    assert!(now.elapsed() < Duration::from_millis(200));
    assert_eq!(state.calls.load(Ordering::Relaxed), 4);
    assert!(
        client
            .list_tools(&config, &CancellationToken::new())
            .await
            .unwrap_err()
            .to_string()
            .contains("MCP is busy")
    );
    cancel.cancel();
    for task in tasks {
        assert!(task.await.unwrap().is_err());
    }
    assert_eq!(
        client
            .list_tools(&config, &CancellationToken::new())
            .await
            .unwrap()
            .len(),
        1
    );
    server.abort();
}
