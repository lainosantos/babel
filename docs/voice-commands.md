# Local voice commands with Needle 3 and MCP

The agent uses a copy of the **original physical microphone feeding Babel**. It works during original audio forwarding and during translation, transcription, or recording sessions. It does not open another microphone, listen to other participants' output, or depend on a recording session. `agent.enabled` is on by default. After installing the local services described below, Babel can start them and discover their ports automatically. Listening is active when Babel is the system's default microphone or an application explicitly uses Babel's virtual microphone; the output route never activates the agent.

To use commands without opening a calling or recording application, choose
**Babel as the default microphone in system audio settings** and keep **Listen
for voice commands** enabled under Commands. Babel captures the physical microphone
chosen in Routing. You do not need to start a session.

Choosing the physical microphone as the default pauses Babel's capture and listening,
unless an application still explicitly uses the virtual microphone. In that case,
also change that application's microphone or stop its capture. Turning off **Listen
for voice commands** pauses only the agent; audio may still be forwarded. Output
continues to depend on audio sent to Babel by an application and never activates
commands. This applies to all three systems.

Voice commands require a physical microphone device selected under **Routing →
Virtual microphone source**. The same device list also includes **Babel Speaker —
original audio** and **Babel Speaker — after translation**. Choosing either
speaker entry pauses command listening and does not capture the physical
microphone, even when Babel Microphone is selected. Neither original nor
translated mirrored audio activates the agent. Selecting a physical device in
that list restores physical capture and the saved command-listening preference.

Say **“Babel, turn on the kitchen light”**, or say **“Babel”**, wait for the activation indicator, and then give the command. The name is configurable. Matching is case-insensitive, requires whole words, and accepts an initial greeting such as “Hey, Babel” or the Portuguese “Oi, Babel”. Mentioning the name in the middle of a conversation (“I use Babel”) does not activate tools. After activation without a command, the default deadline for the next utterance is eight seconds. “Babel, cancel” cancels that activation; during a call in progress, use the dashboard's cancel button.

## Visual command feedback

Under **Commands**, enable **Show desktop notifications** to receive visual feedback
even with settings closed. Babel shows a compact panel with its logo and a state
indicator, without taking focus from your current application or playing a sound.
The panel follows the interface language, Portuguese or English. On Windows and
macOS, it follows the light or dark theme reported by the system; on X11/XWayland,
it uses the light version.

| State | Meaning | Duration |
| --- | --- | --- |
| Activation | The name was recognized; say the command if you have not already done so. | Activation remains visible for at least 300 ms, including fast commands. |
| Processing | Babel is recognizing the next utterance, preparing/loading the model, choosing the tool, or waiting for its response. | Remains while the command is active. |
| Success | The command's calls completed. | Disappears after five seconds. |
| Failure | The command was refused, canceled, timed out, or a stage failed. | Disappears after nine seconds. Check details under Commands. |

The native panel contains **only generic state messages**. It does not receive
recognized speech, arguments, tool names, or results. The Commands page shows
recognized commands, selected tools, and bounded result/error summaries. The
page indicator uses the same states and preserves results
from fast commands, even when they finish between two screen updates. Reopening
settings does not replay old results. Installation or availability problems that
precede activation remain diagnostic information on the page, without simulating
a command failure.

Turning off **Show desktop notifications** hides the current notice; turning it
back on applies to future events. Disabling the agent or losing the microphone
route also ends an active notice. None of these actions repeats MCP calls.
Closing a notice does not cancel the tool: use the cancellation control under
Commands, remembering that an action already sent may have completed.

### Systems, motion, and resource use

The panel uses native windows on **Windows and macOS** and on **Linux with X11 or
XWayland**. In a Wayland session without XWayland, or if the panel cannot start,
Babel uses system notifications. In that fallback mode, appearance, position,
duration, and visibility also follow desktop policies, including Do Not Disturb
and notification permissions. You do not need to install a browser or enable
browser notifications.

The logo effect is brief: at most 20 frames per second for 1.4 seconds per visual
change. The state then remains static until the next event; a long command does
not keep an animation running continuously. Babel respects Windows/macOS reduced
motion preferences and GNOME's disabled-animation preference. When it cannot read
the desktop preference, it uses the static version. `BABEL_REDUCED_MOTION=1` also
forces that version. The in-page indicator respects the browser's reduced-motion
preference.

The `babel-feedback` executable (`babel-feedback.exe` on Windows) ships with the
installers and starts only when there is a notice to show. Rendering is software
based, without a WebView or a GPU context created by the helper; the system
compositor may still use its own acceleration. The helper exits after 30 seconds
with no visible panel and returns on demand. While showing a processing command,
it stays available until the result or that state's termination. It does not open
audio devices or load AI models.

For a local build, use `cargo build --release --bins` and keep the helper alongside
Babel's other executables. Without it, system notification feedback is available,
but the custom panel is not. The feedback queue is bounded and independent of
capture, inference, and tools; a slow desktop neither blocks audio nor accumulates
a sequence of old notices.

## Recent command history

The **Commands** page keeps the most recent **100 command entries**, newest first.
This history exists only in Babel's process memory. **Clear history** removes
its entries; closing Babel also clears it. Reopening settings while Babel is
running can show the retained entries, without replaying notifications or tools.
No history file is created.

An entry begins when Babel recognizes an addressed wake name. It shows the
recognized command when available, the overall outcome, and the selected MCP
integration and tool for each call. Tool states distinguish **Selected**,
**Executing**, **Succeeded**, and **Failed**. Selection alone does not mean a
call was dispatched: confidence and argument validation can still reject the
plan, and a failed call prevents later calls from running. Result and error
summaries have bounded lengths.

Needle confidence appears when the model supplied a valid finite number from
**0 to 1**, including values below the configured execution threshold. An unknown
value means no valid confidence is available, for example because processing
failed before reaching Needle. It is not displayed as zero. Confidence is the
model's reported value, not proof that the requested action is correct.
Recognition and planning durations appear when measured; missing timings remain
unknown. They help distinguish speech-recognition delay from tool selection and
execution, without promising a fixed response time.

Ordinary background speech and service startup failures do not create fabricated
command entries. Readiness failures remain in the existing service notice on the
Commands page. Babel does not add audio, tool arguments or authentication
settings to these entries, and creates no history files. Entries contain the
recognized text and bounded summaries of tool responses. The separate
recent-audio buffer and optional session recording/transcription retain their
own settings and lifecycle.

## Listening and processing

While the microphone route is active, Babel continuously captures PCM from the
selected physical microphone. The agent reuses a copy of that same capture; it
does not open a second microphone. Selecting Babel as the default microphone
keeps this route active even without a calling application. Disabling voice commands
stops agent listening but does not deactivate a route still required by the system
or an application.

The current filter uses RMS energy to separate sound from silence. Non-silent
segments go to local Whisper for text recognition; Babel then looks for the wake
name. Speech without “Babel” may therefore also pass through Whisper, but does
not invoke Needle or tools. Needle is called only after the wake name and a command.
This energy filter is not a dedicated wake-word classifier; Babel does not yet
implement that type of detector.

Whisper remains loaded while listening is active, including during silence; the
filter avoids inference on silence but does not unload the model between sentences.
When the microphone is no longer eligible, listening pauses and Babel releases
its helpers after `agent.idle_unload_secs` (60 seconds by default). Needle has an
additional deadline based on the last command: it may release its weights while
Whisper continues listening. Disabling the agent immediately stops managed helpers.
External servers follow their own resource policies.

Command listening does not create WAV or TXT files. File recording and transcription
require a session with those features enabled separately. Optional recent audio
retention remains bounded in memory; it enters session files only when you select
**Include recent history** for that start. None of these choices is automatically
activated by the wake name.

## Flow

1. The audio worker copies frames to a bounded queue without waiting for AI.
2. A separate worker converts to mono PCM at 16 kHz and segments utterances using energy and silence.
3. The **local whisper.cpp server** transcribes original speech without translation. Babel looks for the wake name in the text.
4. Babel gathers tools from enabled MCP integrations and sends only the command and schemas to **local Needle 3**.
5. Needle chooses tools and fills in arguments. Before execution, Babel checks confidence, refusal, exact identification, fields not grounded in the utterance, and JSON Schemas.
6. The dashboard shows activation, recognition of the next utterance, decision, execution, success, or failure. Results are tool text/JSON, not an invented spoken response.

The agent executes at most four calls per command by default, sequentially. All are validated before the first execution; each integration is checked again before its call. A failure stops subsequent calls without undoing those already completed. Timeouts or cancellation **do not repeat a tool**: the server may already have completed the action. Changed settings, microphone switching, disabling the agent, and application shutdown cancel pending work and discard old speech.

While Needle/MCP processes a command, new speech for the agent is discarded to prevent a queue of delayed actions. Normal conversation audio continues to be forwarded. Commands also remain in forwarded audio; this feature does not mute the wake word in the call.

## Managed services and dynamic ports

Babel uses **two separate local processes**: whisper.cpp recognizes speech and
the wake name; Needle bridge chooses MCP tools. Gemini/OpenAI translation does
not replace these services. Both run on Babel's computer, even when the dashboard
is opened on another computer.

Under **Commands → Local speech and decision services**, keep both endpoints at
`auto`. Babel starts previously installed helpers with `--port 0`: the operating
system chooses a free port and keeps its socket reserved. Each process announces
its identity and actual endpoint in a `BABEL_SERVICE_READY` line followed by JSON.
Babel validates that announcement before using the service; it does not guess a
port or connect to a default number.

Effective endpoints appear in the **Commands page status**. They describe the
current run: do not copy those ports into managed configuration. TOML remains
`whisper_endpoint = "auto"` and `needle_endpoint = "auto"`, including when a helper
restarts and receives another port.

The **Local services folder** field corresponds to `agent.services_directory`.
An empty value uses `services` inside Babel’s default user configuration folder
(`~/.config/babel/services` on Linux). For installation
elsewhere, enter the absolute Babel folder containing `scripts` and `.tools`;
do not enter only the binary or model folder. Relative paths are not accepted
in this field. This folder is independent of `files.base_path`, which controls
transcript and recording files.

Expected structure inside that folder:

```text
scripts/needle_bridge.py
.tools/needle/bin/python                         # Linux/macOS
.tools/needle/Scripts/python.exe                 # Windows
.tools/whisper.cpp/models/ggml-base-q5_1.bin
.tools/whisper.cpp/build/bin/whisper-server       # Linux/macOS
.tools/whisper.cpp/build/bin/whisper-server.exe   # Windows, single configuration
.tools/whisper.cpp/build/bin/Release/whisper-server.exe  # Windows, Visual Studio
```

Helpers start when the enabled agent needs to serve an active virtual microphone,
either because it is the system default or because an application uses it.
When neither condition holds, Babel pauses listening, discards pending utterances,
and keeps the processes for the configurable idle delay (60 seconds by default).
If the microphone returns within that interval, they are reused; after expiry,
they terminate and release their memory.
Disabling the agent or exiting the application normally stops helpers it started.
Closing only the settings window does not exit the app. With the tray configured
to start at login, the same policy applies on the next run; a separate service is
not needed for managed helpers.

`agent.whisper_model` selects the `ggml-<model>.bin` file in this folder. The new
default is `base-q5_1`. For compatibility, if that file is missing and only
`ggml-base.bin` exists, the default reuses the already-installed original Base.
This fallback does not apply to an explicit Tiny or Small selection. Accepted
identifiers are `tiny-q5_1`, `base-q5_1`, `small-q5_1`, `tiny`, `base`, and `small`;
the corresponding file must be installed. The script below prepares Base Q5_1.
Existing configurations and files are not overwritten.

The app does not install packages, download Whisper, or compile tools when settings
open. Explicitly prepare binaries, the Python environment, and models before use
with the following instructions. A missing installation produces a diagnostic
under Commands and does not interrupt normal audio forwarding.

An explicit local HTTP(S) endpoint instead of `auto` selects a **service outside
Babel's management**. In that case, you manage its startup, port, and shutdown.
The two fields are independent: one helper can be managed while the other is external.
The Local provider's recognition endpoint under **Translation** is
also a separate setting and does not automatically follow the agent endpoint.

## Installing local Whisper

The [Babel installer script](../scripts/setup_whisper.py) uses the official
[whisper.cpp v1.9.4](https://github.com/ggml-org/whisper.cpp/releases/tag/v1.9.4)
HTTP server, pins commit `927cfce34f31707e17f2bff35c349632fb9e2c3a`, and downloads
the approximately 59.7 MB multilingual `base-q5_1` model. Its URL, size, and SHA-256
come from the same pinned catalog used by embedded transcription. It verifies
SHA-256 before use and applies the [port discovery patch](../scripts/patches/whisper-dynamic-port.patch).
Running it again reuses a compatible installation. A modified checkout or different
model is preserved and causes an error instead of being overwritten.

Run commands from the Babel repository folder. The installer uses only Python's
standard library; it requires **Python 3.9+, Git, CMake, and a C/C++ compiler**.
It neither starts the server nor changes Babel configuration.

### Linux

Have a C/C++ compiler and a build generator such as Make or Ninja installed.
On Debian/Ubuntu, requirements usually come from `python3`, `git`, `cmake`, and
`build-essential`; installation of those packages is managed by the user.

```bash
python3 scripts/setup_whisper.py --backend cpu --jobs 4
```

For NVIDIA with a CUDA toolkit and compatible compiler already installed:

```bash
python3 scripts/setup_whisper.py --backend cuda --jobs 4
```

The installer does not install CUDA. Having only the GPU driver is insufficient
to compile this backend.

### macOS

Have Python, Git, CMake, and Xcode build tools available. For CPU:

```bash
python3 scripts/setup_whisper.py --backend cpu --jobs 4
```

For Metal acceleration with compatible hardware and tools:

```bash
python3 scripts/setup_whisper.py --backend metal --jobs 4
```

The server receives WAV over local HTTP; microphone capture permission belongs
to Babel. These commands and inference have not been validated on macOS hardware
in this environment.

### Windows

Have Python, Git, CMake, and Visual Studio Build Tools with C/C++ support available.
Use a developer terminal matching the computer's architecture. In PowerShell:

```powershell
py -3 scripts/setup_whisper.py --backend cpu --jobs 4
```

With a CUDA toolkit and compatible compiler installed, use `--backend cuda`.
With the Visual Studio generator, the executable is usually under `build/bin/Release`;
single-configuration generators may use `build/bin`. The installer reports the
final path, and Babel checks both forms. Keep produced DLLs alongside the executable.
WSL is unnecessary. Native Windows builds and inference have not yet been validated
in this environment.

All builds use optimizations for the machine's CPU (`GGML_NATIVE=ON`). Do not move
this binary to another CPU without checking compatibility. `--jobs` limits build
parallelism, not inference response time. The managed helper uses
`agent.local_threads`, default 2, capped by available CPUs. This field accepts
1–32 and controls only command Whisper; it does not change Needle's internal
runtime or translation/transcription threads.

### Manual execution and verification without audio

Normally, install the helper and leave the endpoint at `auto`. To diagnose an
external installation, manually start the **binary with Babel's patch**:

```bash
.tools/whisper.cpp/build/bin/whisper-server -m .tools/whisper.cpp/models/ggml-base-q5_1.bin -t 2 -l auto --host 127.0.0.1 --port 0
```

On Windows, use the `.exe` reported by the installer. Read the
`BABEL_SERVICE_READY` line and obtain `endpoint` from its JSON. The `port` field
contains the OS-assigned number; do not use `:0` as a connection URL. The original
unpatched v1.9.4 server does not publish this discovery contract. Do not use
`--no-prints` or `--no-context`: that server version does not accept those options.

To validate without sending audio, set `WHISPER_ENDPOINT` to the actual announced
endpoint and run on Linux/macOS:

```bash
curl --max-time 5 -i "${WHISPER_ENDPOINT%/inference}/health"
curl --max-time 5 -i -F response_format=json "$WHISPER_ENDPOINT"
```

In PowerShell, assign the announced endpoint to `$WhisperEndpoint` and use:

```powershell
curl.exe --max-time 5 -i ($WhisperEndpoint -replace '/inference$', '/health')
curl.exe --max-time 5 -i -F response_format=json $WhisperEndpoint
```

The first request must return HTTP 200, `Server: whisper.cpp`, and JSON with
`status: ok`. The second includes no audio file: the expected result is HTTP 400
with `Invalid request` or official JSON indicating a missing `file`. A generic
`/health` returning 200 does not confirm service identity.

Babel performs both checks before transmitting an utterance. Confirmation is
reused under the same configuration/route and invalidated after transcription
failure. If using a local proxy at an explicit endpoint, preserve the header
and paths: `/asr/inference` requires `/asr/health`. Inference responses must have
`Content-Type: application/json`.

After verification, Babel sends mono PCM16/16 kHz WAV in multipart form,
`response_format=json`, `translate=false`, and the selected language. `auto`
detects the language; `pt` and `en` fix it. For Portuguese, use a multilingual
model: `.en` variants serve English.

Managed Whisper uses no API key; keep `whisper_api_key_env` empty. For an
authenticated external server, configure an explicit endpoint and enter the
**name** of a credential available to Babel in that field. The secret remains
in app memory or the environment, never TOML.

Whisper also recognizes the initial word. This implementation does not use a
small dedicated acoustic detector: every speech segment passes through local ASR.
Resource use and delay depend on CPU/GPU, model, duration, and noise. Without
Whisper, main audio continues working. Managed mode discards helper stdout and
stderr except the announcement needed for discovery; it does not save utterances
in those logs. During manual execution, do not enable `--print-realtime`,
`--print-progress`, or debugging if you do not want content exposed in the terminal.

### Interpreting Whisper diagnostics

| Diagnostic detail | Meaning and action |
| --- | --- |
| `Whisper is not installed in the local services folder` | Check the services folder and run the installer there. |
| `Whisper model is missing` | Run the installer again; it verifies the model before use. |
| `Local voice service did not announce its bound port` | Check the build with Babel's patch, model, and runtime requirements. An original server without the marker does not support managed mode. |
| `Whisper local service is unavailable or timed out` | Check process status and timeout. In external mode, use the endpoint actually announced by the current process. |
| `Whisper endpoint is not a verified whisper.cpp server` | The response does not identify the expected protocol: it may be another application, an incorrect path, or a proxy stripping headers. No audio is sent during this check. |
| `Whisper model is not ready` | The identified service has not yet passed its health check; check installation and the model file. |

After an installation is fixed, Babel retries starting/using helpers while the
agent is enabled and the virtual microphone is the system default or used by
an application. The settings page only queries status: it does not capture a
test recording or execute MCP tools on its own.

ASR failures before the wake name appear as diagnostics under **Commands**, without
a floating notice or command notification. Failures after activation retain the
corresponding visual feedback. The API distinguishes them with
`error_scope: "service"` or `"command"`.

## Local Needle 3

The correct model for this integration is [Cactus Compute Needle 3](https://huggingface.co/Cactus-Compute/needle3). It uses its own `.cact` runtime; it should not be treated as an Ollama/llama.cpp GGUF model. The [official Python API](https://github.com/cactus-compute/needle/blob/main/llms.txt) receives schemas through `Needle(tools=...)` and returns calls and arguments through `complete(...)`.

The project includes **`scripts/needle_bridge.py`**, a Babel HTTP adapter over that API. `/complete` is this adapter's protocol, not an official Needle HTTP API. The program runs in a separate process and uses only `complete`, never `run`: Babel's Rust client executes MCP.

Create the Python environment in the services folder. On Linux and macOS:

```bash
python3 -m venv .tools/needle
.tools/needle/bin/python -m pip install cactus-needle==3.0.6
.tools/needle/bin/python -c "import os; os.environ['NEEDLE_TELEMETRY']='0'; os.environ['DO_NOT_TRACK']='1'; from needle import Needle; Needle(tools=[]).close()"
```

The `cactus-needle==3.0.6` Python package selects native engine **3.0.2**. Required files are [published in the official repository](https://huggingface.co/Cactus-Compute/needle3/tree/main/python), and the [SDK platform selector](https://github.com/cactus-compute/needle/blob/main/needle/agent/fetch.py) includes native Windows:

| System / architecture | Provided engine | Babel verification |
| --- | --- | --- |
| Linux x86_64, glibc | `manylinux2014_x86_64`, `.so` library | Real inference tested on this host |
| Linux ARM64, glibc | `manylinux2014_aarch64`, `.so` library | Official file available; not executed here |
| Linux x86_64 / ARM64, musl | `musllinux_1_2_x86_64` / `musllinux_1_2_aarch64` | Official files available; not executed here |
| macOS 11+, Apple Silicon / Intel | `macosx_11_0_arm64` / `macosx_11_0_x86_64`, `.dylib` library | Official files available; not executed here |
| Windows x86_64 / ARM64 | `win_amd64` / `win_arm64`, `.dll` library | SDK and native files available; not executed here |

On Windows, use Python matching the architecture and these PowerShell commands; the published runtime does not require WSL:

```powershell
py -3 -m venv .tools\needle
.tools\needle\Scripts\python.exe -m pip install cactus-needle==3.0.6
.tools\needle\Scripts\python.exe -c "import os; os.environ['NEEDLE_TELEMETRY']='0'; os.environ['DO_NOT_TRACK']='1'; from needle import Needle; Needle(tools=[]).close()"
```

The last command prepares the native engine and official weights in the user's
cache through the Python API. It may download files during this explicit step;
it neither transcribes audio nor executes tools. Prepare using the same user
that starts Babel, before relying on commands during a call. Installing only the
`cactus-needle` package does not guarantee the engine and weights are available.
In managed mode, Babel starts Needle with `HF_HUB_OFFLINE=1`: if anything is missing
from the cache, the request fails without starting a download. Complete the
explicit preparation above and try again. The cache belongs to the process user;
preparing it under another account does not install weights for the account
running Babel.

With files prepared, leave `needle_endpoint = "auto"`: Babel starts
`needle_bridge.py --port 0`. The adapter binds only to `127.0.0.1` and announces
the actual endpoint through `BABEL_SERVICE_READY`, without a fixed port. `/health`
confirms bridge identity. It maintains a lightweight HTTP server; the model loads
in a separate inference process only on the first planning request, so that stage
may still take time.

To manage the helper separately, for example with your own weights:

```bash
.tools/needle/bin/python scripts/needle_bridge.py --port 0 --weights /path/to/needle3.cact --idle-unload-secs 60 --timeout-secs 20
```

Copy the announced `endpoint` into the Needle field only in this external mode.
On Windows, substitute Python from `.tools\needle\Scripts\python.exe`.
WSL2 can host an external Linux helper, subject to
[WSL localhost forwarding](https://learn.microsoft.com/en-us/windows/wsl/networking#accessing-linux-networking-apps-from-windows-localhost).
This arrangement has not been tested here and is not started by Babel's native
manager. Use the actual loopback endpoint accessible from Windows; VM private
addresses are rejected by the local inference policy.

The model can be reused between nearby commands. After 60 seconds without a command
by default, the bridge terminates and reaps the inference process, returning weight
memory to the system. Calling the SDK's `close()` alone did not guarantee this,
because the runtime retained global native state. The HTTP server and `/health`
remain available without loaded weights; the next request creates another process.
In managed mode, the delay follows `agent.idle_unload_secs` (1–3600). An in-progress
command is not interrupted by this idle release. The `timeout_secs` limit includes
loading and inference; request timeout or disconnection terminates the inference
process.

The adapter reuses the catalog while it is unchanged and resets history for each
command. Optional telemetry is disabled with `NEEDLE_TELEMETRY=0` and
`DO_NOT_TRACK=1`. The native engine remains outside the Rust process, isolating
its memory from audio routing without claiming native dependencies are written in Rust.

To authenticate the adapter, configure `needle_api_key_env` with a credential reference such as `BABEL_NEEDLE_TOKEN`. In managed mode, Babel passes that credential to the helper itself. In external mode, make the same secret available to both processes and start the bridge with `--api-key-env BABEL_NEEDLE_TOKEN`. The adapter rejects requests originating directly from browser pages; only the local backend makes calls.

Voice inference accepts only HTTP(S) at a literal loopback address or `localhost`, without proxies or redirects. MCP integrations have their own rules and may access authenticated remote services.

## Configuration

```toml
[agent]
enabled = true
desktop_notifications = true
wake_name = "Babel"
whisper_endpoint = "auto"
whisper_model = "base-q5_1"
whisper_language = "auto"
whisper_api_key_env = ""
needle_endpoint = "auto"
needle_api_key_env = ""
services_directory = ""
local_threads = 2
idle_unload_secs = 60
min_confidence = 0.85
max_calls = 4
silence_ms = 600
max_utterance_ms = 10000
command_window_secs = 8
timeout_secs = 20
vad_threshold = 0.012
```

- `whisper_endpoint` and `needle_endpoint`: `auto` for managed helpers and OS-selected ports; an explicit URL for a local external service. Discovered ports appear in status without replacing `auto` in configuration.
- `whisper_model`: managed Whisper model; default `base-q5_1`, with the legacy fallback described above. Does not change an external server's model.
- `local_threads`: 1–32, default 2, capped by available CPUs; only managed command Whisper.
- `idle_unload_secs`: 1–3600 seconds, default 60. Releases helpers when the microphone becomes ineligible and, independently, Needle weights after the last command. The silence filter does not make an eligible microphone inactive for this policy.
- `services_directory`: empty uses the `services` subfolder of the default user configuration folder; a custom value must be absolute and contain the installation structure above. It does not depend on the application's starting folder.
- `min_confidence`: execution threshold from 0 to 1. Needle without numeric confidence, with `suppressed_calls`, with `validation.ungrounded`, with a negation flag, or without calls results in refusal. The 0–1 interval adjusts Babel's policy; the runtime has its own internal refusal, documented for confidence below 0.1 and grounding failures. `complete()` exposes no parameter to disable that refusal, and calls in `suppressed_calls` are not promoted to execution. A threshold is an application choice, not a correctness guarantee.
- `max_calls`: one to eight calls per command. Default four.
- `silence_ms`: silence ending an utterance, from 200 to 2000 ms. A shorter pause improves responsiveness but may split natural sentences.
- `max_utterance_ms`: utterance limit, from 1000 to 15000 ms. Longer utterances are discarded instead of executing a truncated command.
- `command_window_secs`: deadline after saying only the name, from two to thirty seconds.
- `timeout_secs`: limit for each network request/stage, from one to 120 seconds.
- `vad_threshold`: minimum normalized energy, from 0.0001 to 0.5. Increasing it reduces noise but may miss quiet speech.
- `desktop_notifications`: enables the native state panel outside settings, with system notifications as fallback. Turning it off hides the current notice; re-enabling does not replay old commands. It changes neither listening, routing, nor details on the Commands page.

The interface stores credential references and integrations in agent configuration. Key values must be provided through the dashboard or environment. Interface text follows the application language; `whisper_language` controls only speech recognition.

## MCP integrations and authentication

Add multiple integrations in the agent settings dashboard, enabling only those whose tools you want to make available. The catalog is built from `tools/list`; the model does not invent names or execute shell commands directly. Two tools with the same name on different servers receive distinct identities. With no enabled integrations, no external action can be executed.

For a **stdio** server, enter the executable and arguments separately, a working directory if needed, and environment variables. Use credential references for secret values. Babel starts the executable directly without interpolating a shell command line.

For an **HTTP MCP** server, enter its Streamable HTTP endpoint and select no-key, Bearer, or OAuth authentication according to the server. Bearer uses a credential reference; OAuth uses the dashboard's connect/authorize flow with the scopes and client ID/client secret required by the server. Do not confuse MCP authentication with the Needle adapter key. Tool availability depends on the connected integration and permissions granted by the provider.

Tools and their results are data. Tool results do not become instructions for new autonomous actions: this version executes the initial utterance's plan and displays its result. Plans depending on the result of an earlier tool require a new user command.

## Limits and operation

- Recognition occurs after silence and local inference; instant wake-up is not promised. Quiet speech, overlapping speakers, accents, and noise may produce incorrect transcription. Activation neither identifies nor authenticates the speaker.
- There is no acoustic echo filter or voice authentication. Audio played near the microphone may be captured by it, although Babel never sends its output route directly to the agent.
- Schemas help constrain arguments, but no model guarantees correct intent. Check Needle's quality with your tools and languages. Custom weights without calibrated confidence are refused by the current policy.
- The catalog accepts up to 128 tools and requests up to 256 KiB. Needle has internal retrieval for large catalogs; clear descriptions and smaller catalogs make correct choices easier.
- The agent discards queued audio older than 500 ms and old segments, retains only one segment awaiting ASR, limits JSON responses to 256 KiB, and never blocks the audio route waiting for AI. A complete utterance already being recognized is not discarded merely because newer audio overflows a queue. An actual gap between a wake name and its following command still invalidates that activation.
- Enabling the agent does not enable transcript or recording storage. Recognized addressed commands and bounded summaries stay only in the in-memory history described above. Ordinary background recognition is not added to that history. The adapter does not log text or arguments.
- Closing the dashboard does not stop Babel or its agent. Normal exit stops routing, listening, and managed helpers. Servers configured through explicit endpoints remain externally managed.

## Development verification

```bash
cargo test --lib commands::
cargo test --lib feedback
cargo test --lib engine::notifications
python3 -m unittest discover -s scripts -p test_needle_bridge.py -v
python3 -m unittest discover -s scripts -p test_setup_whisper.py -v
```

The suite uses synthetic audio and simulated local servers to check whole-word matching/addressing, two-stage speech, refusal, invalid arguments, cancellation, microphone switching, queue limits, and the helper contract. These tests neither perform actions in real accounts nor measure a loaded model's accuracy.

Feedback tests check rapidly completed commands, result retention for the page,
discarding old events, disabling/microphone switching, the bounded helper protocol,
display durations, reduced motion, and rendering at different scales/themes.
These are deterministic tests without microphone capture; they do not replace
visual verification of windows, focus, and notification policies on each desktop.
Building and running tests in macOS/Windows CI does not itself establish that
visual validation on hardware.

### Real inference verified in this environment

Beyond simulated tests, the official `cactus-needle==3.0.6` package was loaded in an isolated environment, and the helper's `Planner.complete` was called with one fictional weather lookup tool. No tool was executed. The model correctly produced `city=Lisbon` for the English question and `city=Lisboa` for the Portuguese question. Confidence was 0.9417 in English and 0.5413 in Portuguese; therefore, **the default 0.85 threshold would refuse this Portuguese example**. This demonstrates real integration and a concrete accuracy/confidence limitation, not language certification.

The initial call took 13.751 seconds including preparation/download/loading, and the following Portuguese call took 4.355 seconds on this host. The runtime reported peak RAM of 153.6 MB. These values depend on the machine and catalog; they are not a latency promise. The ability to keep audio forwarded does not depend on that inference. Actions in external MCP services still depend on the user's integrations and credentials.

Whisper v1.9.4 with the patch was built and tested on this Linux host with an
Intel i7-12700H CPU, four threads, and the multilingual `base` model. The server
received an OS-assigned port, published the marker, and passed both identity
checks. It correctly transcribed the public JFK sample included in the project,
without microphone capture: a four-second interval took 4.81 s on first inference
and 3.46 s on the next; the eleven-second sample took 3.30 s. Observed RSS was
approximately 245 MiB. This demonstrates real execution and noticeable CPU delay;
it does not measure accuracy for every language or prove performance on macOS,
Windows, or GPU.

On 2026-09-29, a two-thread comparison on this Linux host transcribed the complete
JFK sample identically with original Base and Base Q5_1. Post-inference RSS fell
from 245,268 to 156,728 KiB, with times of 2.19 s and 2.14 s, respectively.
The quantized model occupies 59,707,625 bytes, compared with 147,951,465 for the
original. These are individual measurements of one English sample, without
microphone capture; they do not demonstrate equal Portuguese accuracy or performance
on other machines. The choice follows [whisper.cpp quantization support](https://github.com/ggml-org/whisper.cpp#quantization),
with versions and hashes pinned in Babel's catalog.
