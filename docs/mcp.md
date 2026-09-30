# MCP integrations for voice commands

Babel can connect to multiple MCP servers registered in the agent settings. The local recognizer identifies the wake name and instruction; the decision model receives only the catalog of enabled integrations. Before execution, Babel checks the tool and its JSON Schema against the server again. Text returned by tools is content, not authorization to execute additional instructions.

No integration is added automatically. Choose servers and credentials that match the actions you want to allow the agent to perform. Disabling an integration prevents new calls. The `allowed_tools` list restricts a server's tools; an empty list allows the entire catalog advertised by that server. Your selection can affect external services: a file server, for example, may expose write tools as well as read tools.

Whisper and Needle are the local recognition and decision services, separate
from the MCP servers in this guide. Their default endpoints are `auto`: after
explicit installation, Babel starts the helpers on ports chosen by the operating
system and shows the effective endpoints in the **Commands** status. The services
folder is configured on that page; when empty, it uses the absolute directory of
the loaded TOML file.
See [installing and operating voice commands](voice-commands.md).
This management does not install, authenticate, or select MCP integrations.

## Transports

| Transport | Configuration | Authentication | Notes |
|---|---|---|---|
| `stdio` | Executable, separate arguments, optional directory | Environment variables, including secret references | Babel starts the process directly, without building a shell command |
| `http` | Full Streamable HTTP endpoint URL, usually `/mcp` | No authentication, Bearer, custom headers, or OAuth | HTTPS required; HTTP accepted for loopback (`localhost`/`127.0.0.1`) |

The legacy SSE transport, usually exposed at `/sse` with a separate message endpoint, is not implemented. Use the Streamable HTTP endpoint. SSE responses from Streamable HTTP itself are supported.

The client uses the official Rust SDK `rmcp` 3.5.0 for negotiation, `initialize`/`notifications/initialized`, sessions, JSON-RPC correlation, and tool discovery. The preferred protocol version is `2025-11-25`, maintaining compatibility with the widely used initialization lifecycle. Features exclusive to the lifecycle without `initialize` in newer revisions are not promised.

## Adding an integration

1. Install and configure the MCP server according to its documentation, or obtain its remote URL.
2. Add an integration in the agent settings. The `id` is unique and stable; the name is only for display.
3. Choose the transport and fill in its parameters.
4. Configure the required credential references. Enter their values in the credentials section or provide environment variables to the Babel process.
5. For OAuth, save the integration and use the connect/authenticate action to open browser login.
6. Test the connection. The test initializes MCP and lists tools; it does not execute a business tool.
7. Restrict `allowed_tools` when you want to expose only part of the catalog to the agent.

A misconfigured integration returns a visible failure. Transport errors do not reproduce HTTP bodies, tokens, authorization codes, or subprocess error output.

## Local stdio example

In the interface, open **Commands → MCP integrations → Add integration** and
select **Connection → Local process · stdio**. Fill in:

- **Executable path or command:** only the server executable or its runtime,
  such as `node`, `python3`, or the full path to `node.exe` on Windows.
- **Arguments:** one argument per line. A path with spaces occupies a single
  line; do not surround it with shell quotes.
- **Working directory:** optional; used as the process's execution folder.
- **Public environment values:** one `NAME=value` entry per line. For keys and
  tokens, use the map under **Secret environment references (NAME=REFERENCE)**.

Save the agent settings and click **Discover tools** to test the connection and
list tools. Babel starts the local server automatically when using it; stdio
does not require a URL or port. This option is available on Linux, macOS, and Windows.

Excerpt from `babel.toml`; adapt the paths to the installed MCP package. Executable and arguments are separate fields; do not write a shell command line in `command`.

```toml
[[agent.integrations]]
id = "files"
name = "Work files"
enabled = true
transport = "stdio"
command = "/usr/bin/node"
args = ["/path/to/mcp-server/dist/index.js", "/path/to/allowed-folder"]
cwd = "/path/to/mcp-server"
auth = "none"
timeout_secs = 30
allowed_tools = ["list_directory", "read_text_file"]

[agent.integrations.env]
LOG_LEVEL = "error"

[agent.integrations.secret_env]
SERVICE_API_KEY = "BABEL_FILES_SERVICE_KEY"
```

`secret_env` maps the name received by the server to the credential name in Babel. This example injects the `BABEL_FILES_SERVICE_KEY` secret as `SERVICE_API_KEY` in the child process. `env` contains ordinary values, persisted as text in TOML; place secrets in `secret_env`.

On Windows, use a real executable, such as the full path to `node.exe` or `python.exe`. A package manager's `.cmd` shim may not be directly executable; prefer the runtime plus the installed JavaScript/Python file. Using `cmd.exe` or PowerShell is unnecessary.

The process receives a small set of system variables, including runtime paths, the home directory, and temporary directories, plus explicitly configured variables. Babel's entire environment, potentially containing other providers' keys, is not inherited. The server runs with the user's permissions; the transport is not a sandbox. `stderr` is discarded to avoid exposing secrets in the dashboard/log. To investigate errors in the server itself, run it separately with the configuration specified by its vendor.

Each operation opens a connection and terminates the subprocess on completion. Servers that keep state exclusively in process memory do not retain that state between commands. Babel does not install packages or download executables when adding an integration; if you configure a command that does so, that is the command's behavior.

## HTTP with Bearer authentication

```toml
[[agent.integrations]]
id = "service"
name = "Remote service"
enabled = true
transport = "http"
url = "https://mcp.your-service.example/mcp"
auth = "bearer"
token_env = "BABEL_MCP_SERVICE_TOKEN"
timeout_secs = 30
allowed_tools = []

[agent.integrations.headers]
x-tenant-id = "my-organization"

[agent.integrations.secret_headers]
x-api-key = "BABEL_MCP_SERVICE_API_KEY"
```

The `token_env` field stores the credential name, not its contents. The value is sent as `Authorization: Bearer …` in every request on the connection. Do not include the `Bearer` prefix in the secret.

Ordinary headers are persisted in `headers`. `secret_headers` maps a header name to a Babel credential. An API that uses only `x-api-key` can use `auth = "none"` and populate `secret_headers`.

Transport headers (`Host`, `Content-Length`, `Content-Type`, `Accept`, `Mcp-*`, among others), `Authorization`, `Cookie`, and `Proxy-Authorization` are reserved and cannot be overridden by these maps. Use the dedicated field for Bearer authorization. Basic authentication, browser cookies, client certificates/mTLS, and authenticated proxies have no dedicated fields in this version.

The MCP URL does not accept embedded usernames/passwords, fragments, or query strings; provide tokens through credentials. The MCP connection does not follow HTTP redirects, preventing headers from being forwarded to another URL. Configure the correct final endpoint.

Credentials entered in the dashboard remain only in the Babel process's memory; restarting the application requires entering them again. Alternatively, configure the user's environment variables before opening Babel. Never put the secret value in a field that asks for the credential name.

## OAuth: browser login

OAuth is implemented for HTTP servers that publish authorization metadata. It includes protected resource discovery, RFC 8414/OpenID Connect discovery, PKCE S256, authorization code flow, `state`/`iss` validation, resource binding, and token renewal when the server supplies a refresh token.

```toml
[[agent.integrations]]
id = "account"
name = "My account"
enabled = true
transport = "http"
url = "https://mcp.your-service.example/mcp"
auth = "oauth"
timeout_secs = 60
allowed_tools = []

[agent.integrations.oauth]
client_id = "registered-client-id"
client_secret_env = ""
scopes = ["tools:read"]
```

Scopes vary by provider; `tools:read` is illustrative. Copy the names required by the service. If you leave the list empty, the SDK selects the scopes published by the server. The SDK may also request `offline_access` when advertised, to allow renewal.

- **Previously registered client:** enter `client_id`. Use the callback `http://127.0.0.1:<actual-dashboard-port>/api/agent/oauth/callback`, replacing the placeholder with the current instance's port reported by Babel. Do not assume a default port. The provider registration must accept the callback used during authorization; a dynamic port may require dynamic registration or provider support for variable loopback ports.
- **Confidential client:** if registration requires a secret, use `client_secret_env` to reference an existing credential. The secret is not placed in the authorization URL.
- **Dynamic registration:** leave `client_id` empty. This works only if the server advertises and allows Dynamic Client Registration. Otherwise, register a client with the provider and enter its ID.
- **Consent:** connecting opens the provider's flow; the user signs in and grants permissions in the browser. Adding an integration does not silently authenticate an account.

Login must finish within ten minutes. The callback is single-use and bound to the integration configuration. Changing the URL, scopes, credentials, allowed tools, or another field invalidates the local authorization; connect again. Disconnecting removes tokens and pending logins from memory. Revocation at the provider itself is a separate operation.

Babel does not write access tokens, refresh tokens, or PKCE material to TOML or files. OAuth is valid during the current application run. After restarting, connect again. The SDK keeps its authentication material in memory; cryptographic erasure of every internal copy held by these dependencies is not promised.

Renewal occurs before opening a new MCP connection, when needed. If the server revokes or rejects the token before expiry, the operation fails and reconnection is required; Babel does not automatically repeat a tool after an authentication error. Additional permissions also require reconnecting with adjusted scopes.

OAuth requirements and limits:

- Published metadata and PKCE `S256` are required; Babel does not guess `/authorize` or `/token` endpoints.
- OAuth endpoints use HTTPS. HTTP is allowed only for local integrations with loopback endpoints, which is useful for local servers and tests.
- The official client limits OAuth response sizes and controls discovery redirects. MCP tokens and headers are not sent during metadata discovery.
- The callback must reach the same computer running Babel. Opening authorization on another computer does not complete the local callback.
- This interface does not support Client ID Metadata Documents, device-code flow, client-credentials grants, enterprise EMA/XAA authentication, DPoP, or mTLS.
- Services requiring application or organization approval, special scopes, or manual registration still require those provider-side steps. Babel does not bypass account requirements.

## Execution limits

| Limit | Value |
|---|---:|
| Registered integrations | 32 |
| Simultaneous MCP/OAuth operations per application | 4; new attempts receive “busy”, without an unbounded queue |
| Simultaneous pending OAuth logins | 16 |
| Login completion deadline | 10 minutes |
| Operation timeout, including initialization and discovery | 1–300 seconds; default 30 |
| Tools advertised per server | 256 |
| `tools/list` pages | 16 |
| Individual tool JSON Schema | 64 KiB |
| Total schemas per server | 512 KiB |
| Call arguments | 64 KiB |
| stdio line, HTTP JSON body, or SSE body for an operation | 1 MiB |
| Tool description delivered to the decision model | Up to 4,096 characters |

The decision model may impose lower limits on the combined catalog from multiple servers. A server exceeding these limits must offer a narrower catalog. The `allowed_tools` list filters the catalog exposed to the agent, but the server's raw response must still respect transport limits.

Arguments must be valid JSON objects matching the advertised schema. Schema references to files or external URLs are rejected; local fragment references (`#/$defs/...`) work. This prevents argument validation from accessing files or networks specified by a server.

Tool calls are not automatically repeated after a timeout, cancellation, session expiry, or HTTP failure. A network failure after sending a request may occur after the service has performed the action. Check the result in the service before repeating an operation that creates, sends, deletes, or changes data.

`isError: true` is treated as a tool failure even when the transport succeeds. Flows requiring elicitation/sampling, additional interactive input, or background MCP tasks are refused in this version; Babel neither invents responses nor resends the call to try to complete these flows.

Queues and messages are bounded; no MCP I/O occurs in the audio callback. Each operation closes its session, and the next opens a new one. The HTTP connection does not recover sessions or automatically retry tool POSTs.

## Troubleshooting

- **Executable does not start:** check the path, execution permission, runtime, arguments, and directory. Use the real runtime on Windows when a `.cmd` wrapper is involved.
- **Missing credential:** the field's name must point to a registered credential or an environment variable available to the Babel process.
- **HTTP initialization failure:** check the Streamable HTTP endpoint, TLS, and authentication. A web page or legacy SSE URL is not an MCP endpoint.
- **OAuth does not prepare login:** confirm published metadata, PKCE S256 support, and client registration. An empty ID requires dynamic registration allowed by the provider.
- **Callback rejected:** use the same computer, check the registered URI/port, and start a new login after ten minutes, configuration changes, or an already-used callback.
- **Tool unavailable:** review `allowed_tools`, the enabled integration, and the current catalog. Tools not advertised by the server cannot be called merely because the model suggested a name.
- **Arguments rejected:** check the schema advertised by the server. The decision model must generate the required types and fields.
- **Command failed after sending:** do not assume nothing happened; check the service before repeating it.

## Validation of this implementation

Tests use controlled local servers. They cover initialization and discovery, Bearer and secret-header authentication, stdio subprocesses with mapped secrets, JSON/SSE responses, pagination and size limits, rejection of invalid schemas/arguments, the `isError` flag, cancellation, and no repeated calls after session expiry. The OAuth fixture exercises metadata, PKCE S256, state, callback, renewal, and configuration invalidation.

These tests do not authenticate user accounts or certify every commercial MCP server. Service compatibility depends on the protocol, authentication, permissions, and schemas it publishes.

Primary sources: [official Rust SDK](https://github.com/modelcontextprotocol/rust-sdk), [SDK authorization](https://github.com/modelcontextprotocol/rust-sdk/blob/main/docs/OAUTH_SUPPORT.md), [MCP 2025-11-25 transport](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports), [MCP 2025-11-25 authorization](https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization).
