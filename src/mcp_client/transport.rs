//! Bounded adapters around the official MCP lifecycle implementation.
use super::{McpIntegration, credentials};
use anyhow::{Result, ensure};
use futures_util::{StreamExt, stream::BoxStream};
use reqwest_mcp::{
    Client, Response, StatusCode,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use rmcp::{
    RoleClient,
    model::{ClientJsonRpcMessage, ServerJsonRpcMessage},
    transport::{
        Transport,
        async_rw::AsyncRwTransport,
        common::client_side_sse::{BoxedSseResponse, SseRetryPolicy},
        streamable_http_client::{
            AuthRequiredError, InsufficientScopeError, StreamableHttpClient, StreamableHttpError,
            StreamableHttpPostResponse,
        },
    },
};
use std::{
    collections::HashMap,
    io,
    pin::Pin,
    process::Stdio,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, ReadBuf},
    process::{Child, ChildStdin, ChildStdout},
};

pub(super) const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
#[derive(Debug)]
pub(super) struct NoRetry;
impl SseRetryPolicy for NoRetry {
    fn retry(&self, _: usize) -> Option<Duration> {
        None
    }
}

pub(super) fn validate_header_name(name: &str) -> Result<HeaderName> {
    let header = HeaderName::from_bytes(name.as_bytes())
        .map_err(|_| anyhow::anyhow!("Invalid MCP header name"))?;
    ensure!(
        !matches!(
            header.as_str(),
            "authorization"
                | "host"
                | "connection"
                | "content-length"
                | "content-type"
                | "transfer-encoding"
                | "accept"
                | "cookie"
                | "set-cookie"
                | "proxy-authorization"
        ) && !header.as_str().starts_with("mcp-"),
        "Reserved MCP header; use the dedicated authentication field"
    );
    Ok(header)
}
pub(super) fn headers(config: &McpIntegration) -> Result<HashMap<HeaderName, HeaderValue>> {
    let mut result = HashMap::new();
    for (name, value) in &config.headers {
        result.insert(
            validate_header_name(name)?,
            HeaderValue::from_str(value)
                .map_err(|_| anyhow::anyhow!("Invalid MCP header value"))?,
        );
    }
    for (name, reference) in &config.secret_headers {
        let secret = credentials::get(reference)?;
        let mut value = HeaderValue::from_str(&secret)
            .map_err(|_| anyhow::anyhow!("Invalid secret MCP header value"))?;
        value.set_sensitive(true);
        result.insert(validate_header_name(name)?, value);
    }
    Ok(result)
}

struct BoundedLines<R> {
    inner: R,
    line_bytes: usize,
    failed: bool,
}
impl<R: AsyncRead + Unpin> AsyncRead for BoundedLines<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.failed {
            return Poll::Ready(Err(io::Error::other("MCP line exceeds 1 MiB")));
        }
        let previous = buf.filled().len();
        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                for &byte in &buf.filled()[previous..] {
                    if byte == b'\n' {
                        this.line_bytes = 0;
                    } else {
                        this.line_bytes += 1;
                    }
                    if this.line_bytes > MAX_MESSAGE_BYTES {
                        this.failed = true;
                        buf.set_filled(previous);
                        return Poll::Ready(Err(io::Error::other("MCP line exceeds 1 MiB")));
                    }
                }
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}
pub(super) struct ChildTransport {
    child: Child,
    io: AsyncRwTransport<RoleClient, BoundedLines<ChildStdout>, ChildStdin>,
}
impl ChildTransport {
    pub(super) fn spawn(config: &McpIntegration) -> Result<Self> {
        let mut command = crate::execution::background_command(&config.command);
        // Only a small OS environment and explicitly mapped credentials reach the child.
        command.env_clear();
        for name in [
            "PATH",
            "HOME",
            "USERPROFILE",
            "SystemRoot",
            "SYSTEMROOT",
            "WINDIR",
            "TEMP",
            "TMP",
            "TMPDIR",
            "APPDATA",
            "LOCALAPPDATA",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "LANG",
            "LC_ALL",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command
            .args(&config.args)
            .envs(&config.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        if !config.cwd.is_empty() {
            command.current_dir(&config.cwd);
        }
        for (name, reference) in &config.secret_env {
            command.env(name, credentials::get(reference)?.as_str());
        }
        crate::execution::configure_background_process(&mut command);
        let mut child = command
            .spawn()
            .map_err(|_| anyhow::anyhow!("Could not start registered MCP executable"))?;
        let read = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("MCP stdout unavailable"))?;
        let write = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("MCP stdin unavailable"))?;
        Ok(Self {
            child,
            io: AsyncRwTransport::new_client(
                BoundedLines {
                    inner: read,
                    line_bytes: 0,
                    failed: false,
                },
                write,
            ),
        })
    }
}
impl Drop for ChildTransport {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}
impl Transport<RoleClient> for ChildTransport {
    type Error = io::Error;
    fn send(
        &mut self,
        message: ClientJsonRpcMessage,
    ) -> impl Future<Output = io::Result<()>> + Send + 'static {
        self.io.send(message)
    }
    async fn receive(&mut self) -> Option<ServerJsonRpcMessage> {
        self.io.receive().await
    }
    async fn close(&mut self) -> io::Result<()> {
        let _ = self.io.close().await;
        if tokio::time::timeout(Duration::from_millis(500), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(1), self.child.wait()).await;
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(super) struct BoundedHttp {
    client: Client,
}
type HttpResult<T> = std::result::Result<T, StreamableHttpError<io::Error>>;
fn error(message: &'static str) -> StreamableHttpError<io::Error> {
    StreamableHttpError::UnexpectedServerResponse(message.into())
}
impl BoundedHttp {
    pub(super) fn new(timeout_secs: u64) -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .redirect(reqwest_mcp::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(timeout_secs))
                .retry(reqwest_mcp::retry::never())
                .build()
                .map_err(|_| anyhow::anyhow!("Could not create MCP HTTP client"))?,
        })
    }
    fn request(
        &self,
        method: reqwest_mcp::Method,
        uri: &str,
        session: Option<&str>,
        token: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> HttpResult<reqwest_mcp::RequestBuilder> {
        let mut request = self
            .client
            .request(method, uri)
            .header("Accept", "application/json, text/event-stream")
            .headers(HeaderMap::from_iter(headers));
        if let Some(session) = session {
            request = request.header("Mcp-Session-Id", session);
        }
        if let Some(token) = token {
            let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| error("Invalid MCP authorization token"))?;
            value.set_sensitive(true);
            request = request.header("Authorization", value);
        }
        Ok(request)
    }
}
fn check_auth(response: &Response) -> HttpResult<()> {
    let challenge = response
        .headers()
        .get("WWW-Authenticate")
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    match response.status() {
        StatusCode::UNAUTHORIZED => Err(StreamableHttpError::AuthRequired(AuthRequiredError::new(
            challenge.to_owned(),
        ))),
        StatusCode::FORBIDDEN => Err(StreamableHttpError::InsufficientScope(
            InsufficientScopeError::new(challenge.to_owned(), None),
        )),
        _ => Ok(()),
    }
}
async fn bounded_body(mut response: Response) -> HttpResult<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|n| n > MAX_MESSAGE_BYTES as u64)
    {
        return Err(error("MCP response exceeds 1 MiB"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| error("MCP response read failed"))?
    {
        if bytes.len() + chunk.len() > MAX_MESSAGE_BYTES {
            return Err(error("MCP response exceeds 1 MiB"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
fn bounded_sse(response: Response) -> BoxedSseResponse {
    // A command/list stream is short-lived: bounding its entire body also bounds every event.
    let mut remaining = MAX_MESSAGE_BYTES;
    let stream = response.bytes_stream().map(move |chunk| {
        let bytes = chunk.map_err(|_| io::Error::other("MCP SSE read failed"))?;
        remaining = remaining
            .checked_sub(bytes.len())
            .ok_or_else(|| io::Error::other("MCP SSE response exceeds 1 MiB"))?;
        Ok::<_, io::Error>(bytes)
    });
    sse_stream::SseStream::from_bytes_stream(stream).boxed()
}
impl StreamableHttpClient for BoundedHttp {
    type Error = io::Error;
    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session: Option<Arc<str>>,
        token: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> HttpResult<StreamableHttpPostResponse> {
        let response = self
            .request(
                reqwest_mcp::Method::POST,
                &uri,
                session.as_deref(),
                token,
                headers,
            )?
            .json(&message)
            .send()
            .await
            .map_err(|_| error("MCP HTTP request failed"))?;
        check_auth(&response)?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND && session.is_some() {
            return Err(StreamableHttpError::SessionExpired);
        }
        if matches!(status, StatusCode::ACCEPTED | StatusCode::NO_CONTENT) {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        let session = response
            .headers()
            .get("Mcp-Session-Id")
            .and_then(|h| h.to_str().ok())
            .map(str::to_owned);
        let content_type = response
            .headers()
            .get("Content-Type")
            .and_then(|h| h.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        if !status.is_success() {
            return Err(error(
                "MCP HTTP response rejected; verify authentication and server availability",
            ));
        }
        if response.content_length() == Some(0)
            && !matches!(message, ClientJsonRpcMessage::Request(_))
        {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        if content_type.starts_with("text/event-stream") {
            return Ok(StreamableHttpPostResponse::Sse(
                bounded_sse(response),
                session,
            ));
        }
        if content_type.starts_with("application/json") {
            let body = bounded_body(response).await?;
            let result =
                serde_json::from_slice(&body).map_err(|_| error("Invalid MCP JSON response"))?;
            return Ok(StreamableHttpPostResponse::Json(result, session));
        }
        Err(error(
            "MCP server must return application/json or text/event-stream",
        ))
    }
    async fn get_stream(
        &self,
        uri: Arc<str>,
        session: Option<Arc<str>>,
        last_event_id: Option<String>,
        token: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> HttpResult<BoxStream<'static, std::result::Result<sse_stream::Sse, sse_stream::Error>>>
    {
        let mut request = self.request(
            reqwest_mcp::Method::GET,
            &uri,
            session.as_deref(),
            token,
            headers,
        )?;
        if let Some(id) = last_event_id {
            request = request.header("Last-Event-ID", id);
        }
        let response = request
            .send()
            .await
            .map_err(|_| error("MCP event stream request failed"))?;
        check_auth(&response)?;
        if response.status() == StatusCode::METHOD_NOT_ALLOWED {
            return Err(StreamableHttpError::ServerDoesNotSupportSse);
        }
        if !response.status().is_success() {
            return Err(error("MCP event stream rejected"));
        }
        ensure_sse_content_type(&response)?;
        Ok(bounded_sse(response))
    }
    async fn delete_session(
        &self,
        uri: Arc<str>,
        session: Arc<str>,
        token: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> HttpResult<()> {
        let response = self
            .request(
                reqwest_mcp::Method::DELETE,
                &uri,
                Some(&session),
                token,
                headers,
            )?
            .send()
            .await
            .map_err(|_| error("MCP session close failed"))?;
        if response.status().is_success()
            || matches!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_FOUND
            )
        {
            Ok(())
        } else {
            Err(error("MCP session close rejected"))
        }
    }
}
fn ensure_sse_content_type(response: &Response) -> HttpResult<()> {
    if response
        .headers()
        .get("Content-Type")
        .and_then(|h| h.to_str().ok())
        .is_some_and(|s| s.starts_with("text/event-stream"))
    {
        Ok(())
    } else {
        Err(error("Invalid MCP SSE content type"))
    }
}

/// The official bounded OAuth HTTP client supplies PKCE/discovery request plumbing.
/// Enforce TLS before delegating, including URLs obtained from untrusted metadata.
pub(super) struct StrictOAuth {
    inner: Arc<dyn rmcp::transport::auth::OAuthHttpClient>,
    resource: String,
}
impl StrictOAuth {
    pub(super) fn new(resource: &str) -> Result<Self> {
        Ok(Self {
            inner: Arc::new(
                rmcp::transport::auth::default_oauth_http_client()
                    .map_err(|_| anyhow::anyhow!("Could not create OAuth HTTP client"))?,
            ),
            resource: resource.into(),
        })
    }
}
pub(super) fn validate_oauth_url(value: &str, resource: &str) -> Result<()> {
    let url = reqwest_mcp::Url::parse(value)
        .map_err(|_| anyhow::anyhow!("Invalid OAuth endpoint URL"))?;
    let local = |url: &reqwest_mcp::Url| {
        url.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        })
    };
    let local_resource = reqwest_mcp::Url::parse(resource).is_ok_and(|u| local(&u));
    ensure!(
        url.scheme() == "https" || (url.scheme() == "http" && local(&url) && local_resource),
        "OAuth endpoints require HTTPS; HTTP is accepted only for a local integration"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none() && url.fragment().is_none(),
        "OAuth endpoints cannot embed credentials or fragments"
    );
    Ok(())
}
impl rmcp::transport::auth::OAuthHttpClient for StrictOAuth {
    fn execute(
        &self,
        request: rmcp::transport::auth::OAuthHttpRequest,
    ) -> rmcp::transport::auth::OAuthHttpClientFuture<'_> {
        Box::pin(async move {
            validate_oauth_url(&request.request.uri().to_string(), &self.resource).map_err(
                |_| -> rmcp::transport::auth::OAuthHttpClientError {
                    Box::new(io::Error::other("OAuth endpoint blocked: HTTPS required"))
                },
            )?;
            self.inner.execute(request).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    #[tokio::test]
    async fn oversized_stdio_line_is_rejected_before_parsing() {
        let input = vec![b'x'; MAX_MESSAGE_BYTES + 1];
        let mut reader = BoundedLines {
            inner: input.as_slice(),
            line_bytes: 0,
            failed: false,
        };
        let mut bytes = Vec::new();
        assert!(reader.read_to_end(&mut bytes).await.is_err());
        assert!(bytes.len() <= MAX_MESSAGE_BYTES);
    }
    #[tokio::test]
    async fn separate_stdio_messages_do_not_accumulate_limit() {
        let mut input = vec![b'x'; MAX_MESSAGE_BYTES];
        input.push(b'\n');
        input.extend_from_slice(b"{}\n");
        let mut reader = BoundedLines {
            inner: input.as_slice(),
            line_bytes: 0,
            failed: false,
        };
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(input, bytes);
    }
}
