//! Loopback-only control surface. A fresh bearer capability is generated on every run.
mod agent;
mod instance;
pub use instance::InstanceGuard;
use std::sync::Arc;

use anyhow::Context;
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Query, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroize;

use crate::{config::AppConfig, engine::Controller};

tokio::task_local! {
    // Scoped to one HTTP request; changing preferences never mutates process locale.
    static DISPLAY_LANGUAGE: String;
}

fn localized(message: impl ToString) -> String {
    let message = message.to_string();
    DISPLAY_LANGUAGE
        .try_with(|language| crate::interface_messages::localize(language, &message))
        .unwrap_or(message)
}

#[derive(Clone)]
struct DashboardState {
    controller: Arc<Controller>,
    token: Arc<str>,
    port: u16,
    mutations: Arc<tokio::sync::Mutex<()>>,
}
impl DashboardState {
    fn bound(
        controller: Arc<Controller>,
        listener: &tokio::net::TcpListener,
    ) -> anyhow::Result<Self> {
        let token = rand::random::<[u8; 32]>()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Ok(Self {
            controller,
            token: Arc::from(token.as_str()),
            port: listener.local_addr()?.port(),
            mutations: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/#token={}", self.port, self.token)
    }
}

pub async fn serve(
    controller: Arc<Controller>,
    port: u16,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let listener = bind_local(port).await?;
    // Local audio routing is independent of sessions, credentials, and the panel.
    if let Err(error) = controller.enable_routing().await {
        controller
            .report_error(format!("Roteamento original indisponível: {error:#}"))
            .await;
    }
    let state = DashboardState::bound(controller, &listener)?;
    let url = state.url();
    let app = router(state);
    crate::tray::set_dashboard_url(url.clone());
    let _link = DashboardLink(url.clone());

    // Deliberately printed once, never placed in URLs sent to the HTTP server.
    println!("Painel Babel: {url}");
    axum::serve(listener, app)
        .with_graceful_shutdown(cancel.cancelled_owned())
        .await
        .context("O servidor do painel local foi interrompido")
}

/// Bind and retain the real listener: checking a free port and releasing it
/// before use would let another local process take it between the two steps.
async fn bind_local(requested_port: u16) -> anyhow::Result<tokio::net::TcpListener> {
    let listener = match tokio::net::TcpListener::bind((
        std::net::Ipv4Addr::LOCALHOST,
        requested_port,
    ))
    .await
    {
        Ok(listener) => listener,
        Err(error) if requested_port != 0 && error.kind() == std::io::ErrorKind::AddrInUse => {
            let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .await
                .context("Não foi possível abrir o painel local em uma porta livre")?;
            tracing::warn!(
                "A porta solicitada {requested_port} está ocupada; o painel Babel usará a porta {} escolhida pelo sistema",
                listener.local_addr()?.port()
            );
            listener
        }
        Err(error) => return Err(error).context("Não foi possível abrir o painel local"),
    };
    Ok(listener)
}

// A closed listener's port can be reassigned to an unrelated app. The tray must
// never retain its URL after this server exits or its task is cancelled.
struct DashboardLink(String);
impl Drop for DashboardLink {
    fn drop(&mut self) {
        crate::tray::clear_dashboard_url(&self.0);
    }
}

fn router(state: DashboardState) -> Router {
    let api = Router::new()
        .route("/config", get(get_config).put(set_config))
        .route("/file-paths", post(file_paths))
        .route("/interface", get(get_interface).put(set_interface))
        .route(
            "/platform",
            get(|| async { Json(crate::platform::PlatformInfo::current()) }),
        )
        .route("/status", get(status))
        .route("/devices", get(devices))
        .route("/start", post(start))
        .route("/stop", post(stop))
        .route("/virtual/install", post(install))
        .route("/virtual/uninstall", post(uninstall))
        .route("/voices", get(list_voices))
        .route("/voices/design", post(design_voice))
        .route(
            "/voices/clone",
            post(clone_voice).layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route("/credentials", get(credential_status).post(set_credential))
        .route("/credentials/clear", post(clear_credential))
        .route("/autostart", get(autostart_status).post(set_autostart))
        .merge(agent::routes())
        .route_layer(middleware::from_fn_with_state(state.clone(), authorize))
        // OAuth redirects cannot carry the dashboard bearer. PKCE + one-use
        // state protect this single callback; host/origin checks still apply.
        .route("/agent/oauth/callback", get(agent::oauth_callback));

    Router::new()
        .route(
            "/",
            get(|| async { Html(include_str!("../ui/index.html")) }),
        )
        .route(
            "/brand.svg",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "image/svg+xml")],
                    include_str!("../assets/babel.svg"),
                )
            }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/app.js"),
                )
            }),
        )
        .route(
            "/i18n.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/i18n.js"),
                )
            }),
        )
        .route(
            "/agent.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/agent.js"),
                )
            }),
        )
        .route(
            "/help/voice-commands",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    include_str!("../docs/voice-commands.md"),
                )
            }),
        )
        .route(
            "/help/mcp",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    include_str!("../docs/mcp.md"),
                )
            }),
        )
        .route(
            "/locales/en.json",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
                    include_str!("../ui/locales/en.json"),
                )
            }),
        )
        .route(
            "/locales/pt.json",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
                    include_str!("../ui/locales/pt.json"),
                )
            }),
        )
        .route(
            "/workspace.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/workspace.js"),
                )
            }),
        )
        .route(
            "/fonts/manrope.ttf",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "font/ttf")],
                    include_bytes!("../ui/fonts/manrope.ttf").as_slice(),
                )
            }),
        )
        .route(
            "/fonts/OFL.txt",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    include_str!("../ui/fonts/OFL.txt"),
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../ui/style.css"),
                )
            }),
        )
        .route(
            "/help/providers",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    include_str!("../docs/providers.md"),
                )
            }),
        )
        .route(
            "/help/platforms",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    crate::platform::PlatformInfo::current().device_guide(),
                )
            }),
        )
        .route(
            "/help/platforms/all",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    include_str!("../docs/platforms.md"),
                )
            }),
        )
        .route(
            "/help/native-drivers",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    include_str!("../docs/native-drivers.md"),
                )
            }),
        )
        .route(
            "/help/voices",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    include_str!("../docs/voices.md"),
                )
            }),
        )
        .route(
            "/help/configuration",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    include_str!("../docs/configuration.md"),
                )
            }),
        )
        .route(
            "/help/transcription",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    include_str!("../docs/transcription.md"),
                )
            }),
        )
        .route(
            "/help/local-inference",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    include_str!("../docs/local-inference.md"),
                )
            }),
        )
        .route(
            "/help/recording",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    include_str!("../docs/recording.md"),
                )
            }),
        )
        .route(
            "/help/autostart",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    include_str!("../docs/autostart.md"),
                )
            }),
        )
        .route(
            "/help/other-providers",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    include_str!("../docs/other-providers.md"),
                )
            }),
        )
        .nest("/api", api)
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn(security_headers))
        .layer(middleware::from_fn_with_state(state.clone(), local_origin))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            localize_response,
        ))
        .with_state(state)
}

async fn localize_response(
    State(state): State<DashboardState>,
    request: Request,
    next: Next,
) -> Response {
    let (interface, _) = state.controller.interface_snapshot().await;
    let language = crate::i18n::resolve_language(&interface.language);
    let preference_change = request.method() == axum::http::Method::PUT
        && matches!(request.uri().path(), "/api/interface" | "/api/config");
    let resource_language = if request.uri().path().starts_with("/help/") {
        Some("pt".to_owned())
    } else {
        request
            .uri()
            .path()
            .strip_prefix("/locales/")
            .and_then(|path| path.strip_suffix(".json"))
            .filter(|code| crate::i18n::is_supported_language(code))
            .map(str::to_owned)
    };
    let mut response = DISPLAY_LANGUAGE
        .scope(language.clone(), next.run(request))
        .await;
    let language = if preference_change && response.status().is_success() {
        let (interface, _) = state.controller.interface_snapshot().await;
        crate::i18n::resolve_language(&interface.language)
    } else {
        resource_language.unwrap_or(language)
    };
    response
        .headers_mut()
        .insert(header::CONTENT_LANGUAGE, language.parse().unwrap());
    response
}

fn valid_origin(headers: &HeaderMap, port: u16) -> bool {
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let loopback = format!("127.0.0.1:{port}");
    let localhost = format!("localhost:{port}");
    if host != loopback && host != localhost {
        return false;
    }
    match headers.get(header::ORIGIN) {
        Some(origin) => origin
            .to_str()
            .is_ok_and(|origin| origin == format!("http://{host}")),
        None => true,
    }
}

fn valid_token(headers: &HeaderMap, expected: &str) -> bool {
    let Some(provided) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return false;
    };
    if provided.len() != expected.len() {
        return false;
    }
    // Compare the complete capability rather than short-circuiting on its prefix.
    provided
        .bytes()
        .zip(expected.bytes())
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

async fn local_origin(
    State(state): State<DashboardState>,
    request: Request,
    next: Next,
) -> Response {
    if !valid_origin(request.headers(), state.port) {
        return api_error(
            StatusCode::FORBIDDEN,
            "Abra o painel pelo endereço local exibido no terminal.",
        );
    }
    next.run(request).await
}

async fn authorize(State(state): State<DashboardState>, request: Request, next: Next) -> Response {
    if !valid_token(request.headers(), &state.token) {
        return api_error(
            StatusCode::UNAUTHORIZED,
            "Sessão inválida. Reabra o link completo do painel exibido no terminal.",
        );
    }
    next.run(request).await
}

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    headers.insert(header::CONTENT_SECURITY_POLICY, "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; object-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'".parse().unwrap());
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    headers.insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
    headers.insert(header::X_FRAME_OPTIONS, "DENY".parse().unwrap());
    response
}

fn api_error(status: StatusCode, message: impl ToString) -> Response {
    (status, Json(json!({ "error": localized(message) }))).into_response()
}

fn operation_result(result: anyhow::Result<()>) -> Response {
    match result {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(error) if error.is::<crate::engine::ConfigurationChanged>() => {
            api_error(StatusCode::PRECONDITION_FAILED, error)
        }
        Err(error) => api_error(StatusCode::BAD_REQUEST, format!("{error:#}")),
    }
}

fn expected_revision(headers: &HeaderMap) -> Result<Option<u64>, &'static str> {
    let mut values = headers.get_all(header::IF_MATCH).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    let revision = value
        .to_str()
        .ok()
        .and_then(|value| value.strip_prefix('"'))
        .and_then(|value| value.strip_suffix('"'))
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|value| value.parse::<u64>().ok());
    if values.next().is_some() || revision.is_none() {
        return Err("Revisão inválida. Recarregue os ajustes antes de salvar ou iniciar.");
    }
    Ok(revision)
}

async fn get_config(State(state): State<DashboardState>) -> impl IntoResponse {
    let (config, revision) = state.controller.config_snapshot().await;
    ([(header::ETAG, format!("\"{revision}\""))], Json(config))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FilePathsRequest {
    base_path: String,
    transcription_directory: String,
    recording_directory: String,
}

/// Resolve draft destinations on the Babel host, without saving configuration,
/// probing permissions or creating directories. This also works before either
/// recording feature is enabled and does not touch an active session.
async fn file_paths(
    State(state): State<DashboardState>,
    Json(request): Json<FilePathsRequest>,
) -> Response {
    match state.controller.file_paths(
        &request.base_path,
        &request.transcription_directory,
        &request.recording_directory,
    ) {
        Ok(paths) => Json(paths).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, format!("{error:#}")),
    }
}

fn interface_metadata(
    interface: crate::config::InterfaceConfig,
    revision: u64,
) -> serde_json::Value {
    let system_locale = crate::i18n::system_locale();
    json!({
        "resolved_language": crate::i18n::resolve_language_with_locale(&interface.language, system_locale.as_deref()),
        "language": interface.language,
        "system_locale": system_locale,
        "languages": crate::i18n::SUPPORTED_LANGUAGES.iter()
            .map(|(code, name)| json!({"code":code,"name":name})).collect::<Vec<_>>(),
        "config_revision": revision,
    })
}

async fn get_interface(State(state): State<DashboardState>) -> impl IntoResponse {
    let (interface, revision) = state.controller.interface_snapshot().await;
    (
        [(header::ETAG, format!("\"{revision}\""))],
        Json(interface_metadata(interface, revision)),
    )
}

async fn set_interface(
    State(state): State<DashboardState>,
    headers: HeaderMap,
    Json(interface): Json<crate::config::InterfaceConfig>,
) -> Response {
    let revision = match expected_revision(&headers) {
        Ok(revision) => revision,
        Err(message) => return api_error(StatusCode::BAD_REQUEST, message),
    };
    let _guard = state.mutations.lock().await;
    match state
        .controller
        .set_interface_language(interface.language, revision)
        .await
    {
        Ok((interface, revision)) => (
            [(header::ETAG, format!("\"{revision}\""))],
            Json(interface_metadata(interface, revision)),
        )
            .into_response(),
        Err(error) => operation_result(Err(error)),
    }
}

async fn set_config(
    State(state): State<DashboardState>,
    headers: HeaderMap,
    Json(config): Json<AppConfig>,
) -> Response {
    let revision = match expected_revision(&headers) {
        Ok(revision) => revision,
        Err(message) => return api_error(StatusCode::BAD_REQUEST, message),
    };
    let _guard = state.mutations.lock().await;
    let mut response = operation_result(
        state
            .controller
            .set_config_if_revision(config, revision)
            .await,
    );
    if response.status().is_success()
        && let Some(revision) = revision
    {
        // The controller increments exactly once under its lock. A later snapshot
        // could instead expose another tray mutation and must not label this save.
        response.headers_mut().insert(
            header::ETAG,
            format!("\"{}\"", revision.wrapping_add(1)).parse().unwrap(),
        );
    }
    response
}

async fn status(State(state): State<DashboardState>) -> impl IntoResponse {
    let mut status = state.controller.status().await;
    for message in [
        &mut status.last_error,
        &mut status.routing_error,
        &mut status.microphone.device_error,
        &mut status.speaker.device_error,
        &mut status.local_runtime.message,
    ]
    .into_iter()
    .flatten()
    {
        *message = localized(&*message);
    }
    Json(status)
}

async fn devices() -> Response {
    match crate::audio::devices().await {
        Ok(devices) => Json(devices).into_response(),
        Err(error) => api_error(StatusCode::SERVICE_UNAVAILABLE, format!("{error:#}")),
    }
}

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct StartRequest {
    name: Option<String>,
    #[serde(default)]
    history_seconds: u32,
}

fn parse_start_request(
    headers: &HeaderMap,
    body: &[u8],
) -> Result<StartRequest, (StatusCode, &'static str)> {
    if body.is_empty() {
        return Ok(StartRequest::default());
    }
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if !content_type
        .split(';')
        .next()
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"))
    {
        return Err((
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Envie o nome da sessão como JSON ou inicie sem corpo na requisição.",
        ));
    }
    serde_json::from_slice::<StartRequest>(body).map_err(|_| (StatusCode::BAD_REQUEST, "JSON inválido para iniciar a sessão. Use name como texto e history_seconds como número inteiro não negativo."))
}

async fn start(State(state): State<DashboardState>, headers: HeaderMap, body: Bytes) -> Response {
    let revision = match expected_revision(&headers) {
        Ok(revision) => revision,
        Err(message) => return api_error(StatusCode::BAD_REQUEST, message),
    };
    let request = match parse_start_request(&headers, &body) {
        Ok(request) => request,
        Err((status, message)) => return api_error(status, message),
    };
    // The controller checks the configuration revision again after preparation.
    // Holding the dashboard mutation lock here would prevent Stop and settings
    // from cancelling a model download/loading wait.
    operation_result(
        state
            .controller
            .start_with_history(request.name, request.history_seconds, revision)
            .await,
    )
}

async fn stop(State(state): State<DashboardState>) -> Response {
    let _guard = state.mutations.lock().await;
    operation_result(state.controller.stop().await)
}

async fn install(State(state): State<DashboardState>) -> Response {
    let _guard = state.mutations.lock().await;
    virtual_result(state.controller.install_virtual_devices().await)
}

async fn uninstall(State(state): State<DashboardState>) -> Response {
    let _guard = state.mutations.lock().await;
    virtual_result(state.controller.uninstall_virtual_devices().await)
}

fn virtual_result(result: anyhow::Result<String>) -> Response {
    match result {
        Ok(message) => Json(json!({ "ok": true, "message": localized(message) })).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, format!("{error:#}")),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VoiceQuery {
    provider: String,
    api_key_env: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialQuery {
    api_key_env: String,
}

// Never derive Debug or return this payload: it contains a secret.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialInput {
    api_key_env: String,
    key: String,
}

impl Drop for CredentialInput {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

async fn list_voices(Query(query): Query<VoiceQuery>) -> Response {
    match crate::voices::list(&query.provider, &query.api_key_env).await {
        Ok(voices) => Json(voices).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, format!("{error:#}")),
    }
}

async fn design_voice(
    State(state): State<DashboardState>,
    Json(request): Json<crate::voices::VoiceDesignRequest>,
) -> Response {
    let _guard = state.mutations.lock().await;
    if state.controller.status().await.running {
        return api_error(
            StatusCode::CONFLICT,
            "Encerre a sessão antes de criar uma voz.",
        );
    }
    match crate::voices::design(request).await {
        Ok(voice) => Json(voice).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, format!("{error:#}")),
    }
}

async fn clone_voice(
    State(state): State<DashboardState>,
    Json(request): Json<crate::voices::VoiceCloneRequest>,
) -> Response {
    let _guard = state.mutations.lock().await;
    if state.controller.status().await.running {
        return api_error(
            StatusCode::CONFLICT,
            "Encerre a sessão antes de clonar uma voz.",
        );
    }
    match crate::voices::clone_voice(request).await {
        Ok(voice) => Json(voice).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, format!("{error:#}")),
    }
}

async fn credential_status(Query(query): Query<CredentialQuery>) -> Json<serde_json::Value> {
    Json(json!({ "configured": crate::credentials::configured(&query.api_key_env) }))
}

async fn set_credential(
    State(state): State<DashboardState>,
    Json(mut request): Json<CredentialInput>,
) -> Response {
    let _guard = state.mutations.lock().await;
    if state.controller.status().await.running {
        return api_error(
            StatusCode::CONFLICT,
            "Encerre a sessão antes de alterar a chave de acesso.",
        );
    }
    operation_result(crate::credentials::set(
        &request.api_key_env,
        std::mem::take(&mut request.key),
    ))
}

async fn clear_credential(
    State(state): State<DashboardState>,
    Json(request): Json<CredentialQuery>,
) -> Response {
    let _guard = state.mutations.lock().await;
    if state.controller.status().await.running {
        return api_error(
            StatusCode::CONFLICT,
            "Encerre a sessão antes de remover a chave temporária.",
        );
    }
    operation_result(crate::credentials::clear(&request.api_key_env))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AutostartRequest {
    enabled: bool,
}

async fn autostart_status() -> Response {
    match crate::autostart::status().await {
        Ok(mut status) => {
            status.description = localized(status.description);
            Json(status).into_response()
        }
        Err(error) => api_error(StatusCode::BAD_REQUEST, format!("{error:#}")),
    }
}

async fn set_autostart(
    State(state): State<DashboardState>,
    Json(request): Json<AutostartRequest>,
) -> Response {
    let _guard = state.mutations.lock().await;
    match crate::autostart::set_enabled(request.enabled, state.controller.config_path()).await {
        Ok(mut status) => {
            status.description = localized(status.description);
            Json(status).into_response()
        }
        Err(error) => api_error(StatusCode::BAD_REQUEST, format!("{error:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use tower::ServiceExt;

    #[tokio::test]
    async fn dynamic_binding_owns_a_unique_loopback_port_and_preserves_an_occupied_port() {
        let occupied = bind_local(0).await.unwrap();
        let requested = occupied.local_addr().unwrap();
        assert!(requested.ip().is_loopback());
        assert_ne!(requested.port(), 0);
        let dynamic = bind_local(0).await.unwrap();
        assert_ne!(dynamic.local_addr().unwrap().port(), requested.port());
        let fallback = bind_local(requested.port()).await.unwrap();
        assert_ne!(fallback.local_addr().unwrap().port(), requested.port());
        assert!(fallback.local_addr().unwrap().ip().is_loopback());
        let client = tokio::net::TcpStream::connect(requested).await.unwrap();
        let accepted = tokio::time::timeout(std::time::Duration::from_secs(1), occupied.accept())
            .await
            .unwrap()
            .unwrap();
        drop((client, accepted));
    }

    #[tokio::test]
    async fn dynamic_port_is_used_for_the_url_token_host_and_origin_without_audio() {
        let directory = tempfile::tempdir().unwrap();
        let controller = Arc::new(
            Controller::new(AppConfig::default(), directory.path().join("config.toml")).unwrap(),
        );
        let listener = bind_local(0).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = DashboardState::bound(controller.clone(), &listener).unwrap();
        assert_eq!(state.port, port);
        assert_eq!(state.token.len(), 64);
        assert_eq!(
            state.url(),
            format!("http://127.0.0.1:{port}/#token={}", state.token)
        );
        let token = state.token.clone();
        let cancel = CancellationToken::new();
        let done = cancel.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, router(state))
                .with_graceful_shutdown(done.cancelled_owned())
                .await
                .unwrap();
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let url = format!("http://127.0.0.1:{port}/api/platform");
        let response = client
            .get(&url)
            .bearer_auth(token.as_ref())
            .header(header::ORIGIN, format!("http://127.0.0.1:{port}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        for (host, origin) in [
            ("127.0.0.1:0".to_owned(), format!("http://127.0.0.1:{port}")),
            (format!("127.0.0.1:{port}"), "http://127.0.0.1:0".to_owned()),
        ] {
            let response = client
                .get(&url)
                .bearer_auth(token.as_ref())
                .header(header::HOST, host)
                .header(header::ORIGIN, origin)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        assert_eq!(
            client.get(&url).send().await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
        let status = controller.status().await;
        assert!(!status.running && !status.routing_active);
        assert!(!directory.path().join("config.toml").exists());
    }

    fn headers(host: &str, origin: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, host.parse().unwrap());
        if let Some(origin) = origin {
            headers.insert(header::ORIGIN, origin.parse().unwrap());
        }
        headers
    }

    #[test]
    fn only_exact_loopback_hosts_and_same_origins_are_allowed() {
        assert!(valid_origin(&headers("127.0.0.1:8765", None), 8765));
        assert!(valid_origin(
            &headers("localhost:8765", Some("http://localhost:8765")),
            8765
        ));
        for (host, origin) in [
            ("attacker.example:8765", None),
            ("127.0.0.1.attacker.example:8765", None),
            ("127.0.0.1:8766", None),
            ("127.0.0.1:8765", Some("https://127.0.0.1:8765")),
            ("127.0.0.1:8765", Some("http://attacker.example")),
            ("127.0.0.1:8765", Some("null")),
            ("127.0.0.1:8765", Some("http://localhost:8765")),
        ] {
            assert!(
                !valid_origin(&headers(host, origin), 8765),
                "accepted {host} {origin:?}"
            );
        }
        assert!(!valid_origin(&HeaderMap::new(), 8765));
    }

    #[test]
    fn authorization_requires_complete_bearer_capability() {
        let mut headers = HeaderMap::new();
        assert!(!valid_token(&headers, "secret-capability"));
        for invalid in [
            "secret-capability",
            "Basic secret-capability",
            "Bearer secret",
            "Bearer secret-capabilitY",
            "Bearer secret-capability ",
        ] {
            headers.insert(header::AUTHORIZATION, invalid.parse().unwrap());
            assert!(!valid_token(&headers, "secret-capability"));
        }
        headers.insert(
            header::AUTHORIZATION,
            "Bearer secret-capability".parse().unwrap(),
        );
        assert!(valid_token(&headers, "secret-capability"));
    }

    #[tokio::test]
    async fn api_routes_require_token_and_reject_cross_origin_requests() {
        let directory = tempfile::tempdir().unwrap();
        let app = router(DashboardState {
            controller: Arc::new(
                Controller::new(AppConfig::default(), directory.path().join("config.toml"))
                    .unwrap(),
            ),
            token: Arc::from("test-capability"),
            port: 8765,
            mutations: Arc::new(tokio::sync::Mutex::new(())),
        });
        for (method, path) in [
            ("GET", "/api/config"),
            ("POST", "/api/file-paths"),
            ("GET", "/api/interface"),
            ("GET", "/api/platform"),
            ("PUT", "/api/interface"),
            ("GET", "/api/status"),
            ("GET", "/api/devices"),
            ("PUT", "/api/config"),
            ("POST", "/api/start"),
            ("POST", "/api/stop"),
            ("POST", "/api/virtual/install"),
            ("POST", "/api/virtual/uninstall"),
            ("GET", "/api/voices"),
            ("POST", "/api/voices/design"),
            ("POST", "/api/voices/clone"),
            ("GET", "/api/credentials"),
            ("POST", "/api/credentials"),
            ("POST", "/api/credentials/clear"),
            ("GET", "/api/autostart"),
            ("POST", "/api/autostart"),
        ] {
            let request = Request::builder()
                .method(method)
                .uri(path)
                .header(header::HOST, "127.0.0.1:8765")
                .body(Body::empty())
                .unwrap();
            assert_eq!(
                app.clone().oneshot(request).await.unwrap().status(),
                StatusCode::UNAUTHORIZED,
                "unprotected {method} {path}"
            );
        }
        let request = Request::builder()
            .uri("/api/config")
            .header(header::HOST, "127.0.0.1:8765")
            .header(header::ORIGIN, "https://attacker.example")
            .header(header::AUTHORIZATION, "Bearer test-capability")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        let request = Request::builder()
            .uri("/api/config")
            .header(header::HOST, "127.0.0.1:8765")
            .header(header::AUTHORIZATION, "Bearer test-capability")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()[header::X_FRAME_OPTIONS], "DENY");
        assert!(
            !response
                .headers()
                .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        );
    }

    #[tokio::test]
    async fn platform_and_setup_help_follow_the_process_not_the_browser() {
        let directory = tempfile::tempdir().unwrap();
        let app = router(DashboardState {
            controller: Arc::new(
                Controller::new(AppConfig::default(), directory.path().join("config.toml"))
                    .unwrap(),
            ),
            token: Arc::from("test-capability"),
            port: 8765,
            mutations: Arc::new(tokio::sync::Mutex::new(())),
        });
        let request = |path: &str, user_agent: &str| {
            Request::builder()
                .uri(path)
                .header(header::HOST, "127.0.0.1:8765")
                .header(header::AUTHORIZATION, "Bearer test-capability")
                .header(header::USER_AGENT, user_agent)
                .body(Body::empty())
                .unwrap()
        };
        let platform = crate::platform::PlatformInfo::current();
        for user_agent in [
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64)",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)",
            "Mozilla/5.0 (X11; Linux x86_64)",
        ] {
            let response = app
                .clone()
                .oneshot(request("/api/platform?os=windows", user_agent))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            let data: serde_json::Value = serde_json::from_slice(
                &axum::body::to_bytes(response.into_body(), 4096)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(data, serde_json::to_value(platform).unwrap());
            assert_eq!(data["os"], std::env::consts::OS);
        }
        let response = app
            .clone()
            .oneshot(request("/help/platforms", "Windows"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 65_536)
            .await
            .unwrap();
        assert_eq!(body.as_ref(), platform.device_guide().as_bytes());
        let response = app
            .oneshot(request("/help/platforms/all", "Windows"))
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), 65_536)
            .await
            .unwrap();
        assert_eq!(body.as_ref(), include_bytes!("../docs/platforms.md"));
    }

    #[tokio::test]
    async fn oversized_configuration_is_rejected_before_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        let app = router(DashboardState {
            controller: Arc::new(
                Controller::new(AppConfig::default(), config_path.clone()).unwrap(),
            ),
            token: Arc::from("test-capability"),
            port: 8765,
            mutations: Arc::new(tokio::sync::Mutex::new(())),
        });
        let request = Request::builder()
            .method("PUT")
            .uri("/api/config")
            .header(header::HOST, "127.0.0.1:8765")
            .header(header::AUTHORIZATION, "Bearer test-capability")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(format!("{}{{}}", " ".repeat(65_536))))
            .unwrap();
        assert_eq!(
            app.oneshot(request).await.unwrap().status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert!(!config_path.exists());
    }

    #[tokio::test]
    async fn language_preference_is_persisted_and_does_not_overwrite_audio_settings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let controller = Arc::new(Controller::new(AppConfig::default(), path.clone()).unwrap());
        let app = router(DashboardState {
            controller: controller.clone(),
            token: Arc::from("test-capability"),
            port: 8765,
            mutations: Arc::new(tokio::sync::Mutex::new(())),
        });
        let request = |method: &str, path: &str, revision: &str, body: &str| {
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::HOST, "127.0.0.1:8765")
                .header(header::AUTHORIZATION, "Bearer test-capability")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::IF_MATCH, revision)
                .body(Body::from(body.to_owned()))
                .unwrap()
        };
        let response = app
            .clone()
            .oneshot(request("GET", "/api/interface", "\"0\"", ""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let metadata: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(metadata["language"], "system");
        assert!(matches!(
            metadata["resolved_language"].as_str(),
            Some("pt" | "en")
        ));
        let response = app
            .clone()
            .oneshot(request(
                "PUT",
                "/api/interface",
                "\"0\"",
                r#"{"language":"en"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::ETAG], "\"1\"");
        let restored = AppConfig::load(&path).unwrap();
        assert_eq!(restored.interface.language, "en");
        assert_eq!(restored.microphone.source_language, "pt-BR");
        assert!(!controller.status().await.running);
        let response = app
            .clone()
            .oneshot(request(
                "PUT",
                "/api/interface",
                "\"0\"",
                r#"{"language":"pt"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED);
        let response = app
            .clone()
            .oneshot(request(
                "PUT",
                "/api/interface",
                "\"1\"",
                r#"{"language":"unknown"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers()[header::CONTENT_LANGUAGE], "en");
        let response = app
            .clone()
            .oneshot(request(
                "PUT",
                "/api/interface",
                "\"1\"",
                r#"{"language":"pt"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = app
            .oneshot(request("GET", "/api/interface", "\"2\"", ""))
            .await
            .unwrap();
        assert_eq!(response.headers()[header::CONTENT_LANGUAGE], "pt");
        assert_eq!(AppConfig::load(&path).unwrap().interface.language, "pt");
    }

    #[test]
    fn configuration_revision_requires_one_strong_numeric_etag() {
        let mut headers = HeaderMap::new();
        assert_eq!(expected_revision(&headers).unwrap(), None);
        for value in ["\"0\"", "\"18446744073709551615\""] {
            headers.insert(header::IF_MATCH, value.parse().unwrap());
            assert!(expected_revision(&headers).unwrap().is_some());
        }
        for value in [
            "0",
            "*",
            "W/\"0\"",
            "\"\"",
            "\"-1\"",
            "\"1\",\"2\"",
            "\"18446744073709551616\"",
        ] {
            headers.insert(header::IF_MATCH, value.parse().unwrap());
            assert!(expected_revision(&headers).is_err(), "accepted {value}");
        }
        headers.insert(header::IF_MATCH, "\"1\"".parse().unwrap());
        headers.append(header::IF_MATCH, "\"1\"".parse().unwrap());
        assert!(expected_revision(&headers).is_err());
    }

    #[tokio::test]
    async fn file_path_preview_resolves_drafts_without_writing_or_changing_config() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        let controller =
            Arc::new(Controller::new(AppConfig::default(), config_path.clone()).unwrap());
        let before = serde_json::to_value(controller.config_snapshot().await.0).unwrap();
        let app = router(DashboardState {
            controller: controller.clone(),
            token: Arc::from("test-capability"),
            port: 8765,
            mutations: Arc::new(tokio::sync::Mutex::new(())),
        });
        let base = directory.path().join("Arquivo de sessões");
        let outside = directory.path().join("WAV separados");
        let request = |body: serde_json::Value| {
            Request::builder()
                .method("POST")
                .uri("/api/file-paths")
                .header(header::HOST, "127.0.0.1:8765")
                .header(header::AUTHORIZATION, "Bearer test-capability")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap()
        };
        let response = app
            .clone()
            .oneshot(request(json!({
                "base_path": base,
                "transcription_directory": "texto/original",
                "recording_directory": outside,
            })))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let paths: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 65_536)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(paths["base_path"], json!(base));
        assert_eq!(
            std::path::Path::new(paths["transcription_directory"].as_str().unwrap()),
            base.join("texto").join("original")
        );
        assert_eq!(paths["recording_directory"], json!(outside));
        assert!(paths.get("working_directory").is_none());
        for invalid in [
            "".to_owned(),
            " ".to_owned(),
            ".".to_owned(),
            "archive".to_owned(),
            "../archive".to_owned(),
            "~/Babel".to_owned(),
            "bad\0path".to_owned(),
            "x".repeat(4097),
        ] {
            let response = app
                .clone()
                .oneshot(request(json!({
                    "base_path": invalid,
                    "transcription_directory": "transcripts",
                    "recording_directory": "recordings",
                })))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
        let (after, revision) = controller.config_snapshot().await;
        assert_eq!(before, serde_json::to_value(after).unwrap());
        assert_eq!(revision, 0);
        assert!(!base.exists());
        assert!(!outside.exists());
        assert!(!config_path.exists());
    }

    #[tokio::test]
    async fn configuration_etags_protect_save_and_start_against_concurrent_tray_changes() {
        let directory = tempfile::tempdir().unwrap();
        let controller = Arc::new(
            Controller::new(AppConfig::default(), directory.path().join("config.toml")).unwrap(),
        );
        let app = router(DashboardState {
            controller: controller.clone(),
            token: Arc::from("test-capability"),
            port: 8765,
            mutations: Arc::new(tokio::sync::Mutex::new(())),
        });
        let request = |method: &str, path: &str, revision: Option<&str>, body: String| {
            let mut request = Request::builder()
                .method(method)
                .uri(path)
                .header(header::HOST, "127.0.0.1:8765")
                .header(header::AUTHORIZATION, "Bearer test-capability")
                .header(header::CONTENT_TYPE, "application/json");
            if let Some(revision) = revision {
                request = request.header(header::IF_MATCH, revision);
            }
            request.body(Body::from(body)).unwrap()
        };
        let response = app
            .clone()
            .oneshot(request("GET", "/api/config", None, String::new()))
            .await
            .unwrap();
        assert_eq!(response.headers()[header::ETAG], "\"0\"");
        let original = controller.config().await;
        for relative in [".", "archive", "../archive", "~/Babel"] {
            let mut invalid = original.clone();
            invalid.files.base_path = relative.into();
            let response = app
                .clone()
                .oneshot(request(
                    "PUT",
                    "/api/config",
                    Some("\"0\""),
                    serde_json::to_string(&invalid).unwrap(),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let (unchanged, revision) = controller.config_snapshot().await;
            assert_eq!(revision, 0);
            assert_eq!(unchanged.files.base_path, original.files.base_path);
        }
        let mut saved = original.clone();
        saved.microphone.target_language = "de-DE".into();
        let response = app
            .clone()
            .oneshot(request(
                "PUT",
                "/api/config",
                Some("\"0\""),
                serde_json::to_string(&saved).unwrap(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::ETAG], "\"1\"");
        let mut external = saved.clone();
        external.microphone.capture_device = "tray-device-change".into();
        controller.set_config(external.clone()).await.unwrap();
        for (method, path, body) in [
            (
                "PUT",
                "/api/config",
                serde_json::to_string(&original).unwrap(),
            ),
            (
                "POST",
                "/api/start",
                json!({"name":"Named session"}).to_string(),
            ),
        ] {
            let response = app
                .clone()
                .oneshot(request(method, path, Some("\"1\""), body))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED);
        }
        assert!(!controller.status().await.running);
        assert_eq!(
            controller.config().await.microphone.capture_device,
            "tray-device-change"
        );
        let response = app
            .oneshot(request("GET", "/api/config", None, String::new()))
            .await
            .unwrap();
        assert_eq!(response.headers()[header::ETAG], "\"2\"");
        let body = axum::body::to_bytes(response.into_body(), 65_536)
            .await
            .unwrap();
        let config: AppConfig = serde_json::from_slice(&body).unwrap();
        assert_eq!(config.microphone.target_language, "de-DE");
        assert_eq!(config.microphone.capture_device, "tray-device-change");
    }

    #[test]
    fn session_start_accepts_optional_json_and_rejects_malformed_payloads() {
        let mut headers = HeaderMap::new();
        assert_eq!(
            parse_start_request(&headers, b"").unwrap(),
            StartRequest::default()
        );
        assert_eq!(
            parse_start_request(&headers, br#"{"name":"team"}"#)
                .unwrap_err()
                .0,
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
        headers.insert(
            header::CONTENT_TYPE,
            "application/json; charset=utf-8".parse().unwrap(),
        );
        assert_eq!(
            parse_start_request(&headers, br#"{}"#).unwrap(),
            StartRequest::default()
        );
        assert_eq!(
            parse_start_request(&headers, br#"{"name":"Team sync"}"#).unwrap(),
            StartRequest {
                name: Some("Team sync".into()),
                history_seconds: 0
            }
        );
        assert_eq!(
            parse_start_request(&headers, br#"{"history_seconds":600}"#)
                .unwrap()
                .history_seconds,
            600
        );
        for invalid in [
            "{",
            "[]",
            "null",
            "{\"name\":7}",
            "{\"unexpected\":true}",
            "{\"history_seconds\":-1}",
            "{\"history_seconds\":1.5}",
            "{\"history_seconds\":\"600\"}",
        ] {
            assert_eq!(
                parse_start_request(&headers, invalid.as_bytes())
                    .unwrap_err()
                    .0,
                StatusCode::BAD_REQUEST
            );
        }
    }

    #[tokio::test]
    async fn session_start_and_autostart_validate_requests_without_changing_system_state() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        let mut config = AppConfig::default();
        config.interface.language = "pt".into();
        let controller = Arc::new(Controller::new(config, config_path.clone()).unwrap());
        let app = router(DashboardState {
            controller: controller.clone(),
            token: Arc::from("test-capability"),
            port: 8765,
            mutations: Arc::new(tokio::sync::Mutex::new(())),
        });
        for (body, content_type, status, message) in [
            (String::new(), None, StatusCode::BAD_REQUEST, "Selecione"),
            (
                json!({"name":"Reunião"}).to_string(),
                Some("application/json"),
                StatusCode::BAD_REQUEST,
                "Selecione",
            ),
            (
                json!({"name":"a".repeat(101)}).to_string(),
                Some("application/json"),
                StatusCode::BAD_REQUEST,
                "100 caracteres",
            ),
            (
                json!({"name":"line\nbreak"}).to_string(),
                Some("application/json"),
                StatusCode::BAD_REQUEST,
                "controle",
            ),
            (
                "{".into(),
                Some("application/json"),
                StatusCode::BAD_REQUEST,
                "JSON inválido",
            ),
            (
                "{}".into(),
                Some("text/plain"),
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "JSON",
            ),
        ] {
            let mut request = Request::builder()
                .method("POST")
                .uri("/api/start")
                .header(header::HOST, "127.0.0.1:8765")
                .header(header::AUTHORIZATION, "Bearer test-capability");
            if let Some(content_type) = content_type {
                request = request.header(header::CONTENT_TYPE, content_type);
            }
            let response = app
                .clone()
                .oneshot(request.body(Body::from(body)).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), status);
            let body = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            assert!(String::from_utf8_lossy(&body).contains(message));
        }
        for body in [
            r#"{"enabled":"yes"}"#,
            "{}",
            r#"{"enabled":true,"unexpected":1}"#,
        ] {
            let request = Request::builder()
                .method("POST")
                .uri("/api/autostart")
                .header(header::HOST, "127.0.0.1:8765")
                .header(header::AUTHORIZATION, "Bearer test-capability")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .unwrap();
            assert_eq!(
                app.clone().oneshot(request).await.unwrap().status(),
                StatusCode::UNPROCESSABLE_ENTITY
            );
        }
        let request = Request::builder()
            .method("POST")
            .uri("/api/start")
            .header(header::HOST, "127.0.0.1:8765")
            .header(header::AUTHORIZATION, "Bearer test-capability")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(" ".repeat(65_537)))
            .unwrap();
        assert_eq!(
            app.oneshot(request).await.unwrap().status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert!(!controller.status().await.running);
        assert!(!config_path.exists());
    }

    #[tokio::test]
    async fn clone_upload_has_a_separate_bounded_limit_without_calling_a_provider() {
        let directory = tempfile::tempdir().unwrap();
        let app = router(DashboardState {
            controller: Arc::new(
                Controller::new(AppConfig::default(), directory.path().join("config.toml"))
                    .unwrap(),
            ),
            token: Arc::from("test-capability"),
            port: 8765,
            mutations: Arc::new(tokio::sync::Mutex::new(())),
        });
        for (size, expected) in [
            (70_000, StatusCode::BAD_REQUEST),
            (8 * 1024 * 1024 + 1, StatusCode::PAYLOAD_TOO_LARGE),
        ] {
            let payload = json!({"provider":"unsupported", "api_key_env":"BABEL_UNUSED_TEST_KEY", "name":"test", "reference_base64":"a".repeat(size), "consent_base64":""});
            let request = Request::builder()
                .method("POST")
                .uri("/api/voices/clone")
                .header(header::HOST, "127.0.0.1:8765")
                .header(header::AUTHORIZATION, "Bearer test-capability")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap();
            assert_eq!(
                app.clone().oneshot(request).await.unwrap().status(),
                expected
            );
        }
    }

    #[tokio::test]
    async fn temporary_credentials_return_only_presence_and_are_never_saved() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        let app = router(DashboardState {
            controller: Arc::new(
                Controller::new(AppConfig::default(), config_path.clone()).unwrap(),
            ),
            token: Arc::from("test-capability"),
            port: 8765,
            mutations: Arc::new(tokio::sync::Mutex::new(())),
        });
        let environment = format!("BABEL_DASHBOARD_TEST_{}", rand::random::<u64>());
        let request = Request::builder()
            .method("POST")
            .uri("/api/credentials")
            .header(header::HOST, "127.0.0.1:8765")
            .header(header::AUTHORIZATION, "Bearer test-capability")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"api_key_env":environment, "key":"test-only-ephemeral-value"}).to_string(),
            ))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            json!({"ok":true})
        );
        let request = Request::builder()
            .uri(format!("/api/credentials?api_key_env={environment}"))
            .header(header::HOST, "127.0.0.1:8765")
            .header(header::AUTHORIZATION, "Bearer test-capability")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            json!({"configured":true})
        );
        assert!(!config_path.exists());
        let request = Request::builder()
            .method("POST")
            .uri("/api/credentials/clear")
            .header(header::HOST, "127.0.0.1:8765")
            .header(header::AUTHORIZATION, "Bearer test-capability")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({"api_key_env":environment}).to_string()))
            .unwrap();
        assert_eq!(app.oneshot(request).await.unwrap().status(), StatusCode::OK);
        assert!(!crate::credentials::configured(&environment));
    }
}
