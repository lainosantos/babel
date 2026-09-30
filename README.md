# Babel

Virtual microphone and audio output for bidirectional speech translation. The
core, AI adapters, dashboard server and tray controls are written in Rust. Each
direction can use a different translation provider and voice. Transcription
also selects its own provider and language for the microphone and incoming
audio, independently of translation.
Translation, transcription and recording are independent. Outside a session,
Babel routes original audio between the configured devices. Turning off a
direction's translation preserves that routing during a session as well.

The command agent uses **only the original physical microphone**, with a
configurable wake name (default: “Babel”), local Whisper, **Needle 3** and
authenticated HTTP/stdio MCP integrations. It also works without a translation
or recording session. Output audio never feeds the agent. See
[setup and voice commands](docs/voice-commands.md) and
[MCP integrations and authentication](docs/mcp.md); both local services must be
prepared before use. The dashboard and notifications show processing and results
without blocking audio routing.

```text
Physical microphone → translation → Babel Microphone → calling app
Calling app → Babel Speaker → translation → physical headphones
```

Native voice mode streams audio over persistent connections. Gemini Live
Translate and OpenAI Realtime Translate are the continuous translation adapters.
Conversational models wait for end-of-speech detection; the local option works
in segments. Custom voices add streaming TTS after translation.
**Streaming does not mean zero latency or guarantee a hard real-time deadline.**

## Getting started

Requires Rust **1.90 or later**. Clone/open this directory and build:

```sh
cargo build --release --locked
cargo run --release -- init
```

On Linux, install `pulseaudio-utils` (Ubuntu/Debian), with PipeWire +
pipewire-pulse or PulseAudio running in the user session. Do not run Babel as root.

```sh
sudo apt install pulseaudio-utils
cargo run --release -- setup
cargo run --release -- serve
```

The OS chooses an available dashboard port on each run. The terminal prints the
actual address with a session token; open that complete link or **Settings**
from the tray, which tracks the current address. The tray offers **Settings**,
**Start session**, **Stop session** and **Quit**. Stopping a session finalizes
its files and returns to original audio; quitting Babel also stops local routing.
You can select the **physical microphone** and **physical output**, whether
translation is stopped or running, and refresh the device list. Switching keeps
the AI session and files open; a brief audio gap is possible. The dashboard also
works without a tray using `serve --no-tray`.

The dashboard and tray identify the OS running Babel. Audio instructions,
virtual-device controls and startup guidance follow that OS. On Linux, the
dashboard offers to create/remove Babel devices; on macOS and Windows, it
explains how to prepare/install Babel's own driver package and select its
endpoints. Help opens directly at the section for the current OS.

On Linux/macOS, `./scripts/run.sh` starts the built binary from this directory;
on the first run, it builds if the binary does not exist. It also accepts
subcommands, for example `./scripts/run.sh doctor`.

1. In **Translation & voices**, configure the profiles used for translation.
   Cloud providers receive a temporary key through the dashboard or the named
   environment variable. Recording or routing originals alone skips this step.
2. On the same page, select each direction's translator. One provider's settings
   do not overwrite another's. The default is Gemini Live Translate.
3. In **Routing**, select physical devices and your OS's virtual endpoints.
   On Linux, use the physical microphone as capture and `babel_mic_bus` as
   microphone playback; for incoming audio, use `babel_speaker.monitor` as
   capture and your headphones as playback. The [platform guide](docs/platforms.md)
   lists macOS/Windows endpoints.
4. In **Translation & voices**, choose each direction's languages and voices,
   enabling only the translations you want. AI profiles and the voice library
   are grouped on this page.
5. In **Transcription**, choose the originals to save and each source's STT
   provider and language: Gemini, OpenAI, Deepgram or Whisper. Transcription
   profiles and credentials live here, independently of translation profiles.
   In **Recording**, select the original audio to save. Both pages link to the
   base folder and filenames in **Settings**. You can record, transcribe or
   combine features.
6. Save settings, optionally enter a **Session name**, and click **Start session**.
   The session bar mirrors the current Translation, Recording and Transcription
   switches so you can adjust them before starting. In the calling app, select
   **Babel_Microphone** and **Babel_Speaker** on Linux, or **Babel Microphone**
   and **Babel Speaker** on macOS/Windows. Without an active session, these
   routes carry original audio.

To use original audio only, leave translation off in both directions. Routing
continues while an app uses the Babel devices; selecting Babel as the default
microphone also activates its microphone route. To save audio, enable
**Recording** and start a session; no special provider, AI key or transcription
is required.

Legacy `loopback` configurations migrate to the Gemini profile with route
translation and transcription disabled, without starting cloud calls. See
[configuration migration](docs/configuration.md#files-and-keys).

On macOS and Windows, the **native Babel driver** provides two independent
routes. Source code and build scripts are in `native/macos` and `native/windows`;
**distribution-package signing and native hardware validation are still pending**.
The app uses CoreAudio/WASAPI through CPAL, with persistent IDs and app-activity
monitoring. On macOS, that detection requires **macOS 14.2+**. BlackHole and
VB-CABLE remain optional alternatives, not dependencies of Babel's own driver.
See [native builds and packages](docs/native-drivers.md) and
[device maps by platform](docs/platforms.md).

On Windows, the executable is `target\release\babel.exe`; the same subcommands
work in PowerShell. On macOS, the tray event loop runs on the main thread. Grant
microphone capture permission to the app/terminal.

## Documentation

Browse the [documentation website](https://lainosantos.github.io/babel/) or the
source guides below.

- [Configuration and operation](docs/configuration.md): profiles, keys,
  languages, routes, quality, queues, transcription, recording and tray.
- [Interface languages](docs/localization.md): English, Portuguese, system
  selection and adding translation catalogs.
- [Original-audio transcription](docs/transcription.md): Gemini, OpenAI,
  Deepgram and Whisper, source-specific languages/providers, profiles and limits.
- [Original-audio recording](docs/recording.md): one WAV, mixing,
  synchronization and recorder limits.
- [Linux/macOS/Windows devices](docs/platforms.md): installation,
  routing, permissions, drivers and troubleshooting.
- [GitHub Actions installers](docs/ci-installers.md): platform packages,
  artifacts, checks, tagged releases and signing.
- [Native Babel drivers](docs/native-drivers.md): source, builds,
  package preparation and explicit installation requirements.
- [Gemini Live](docs/providers.md): protocols, models, transcriptions,
  capabilities and continuous translation restrictions.
- [OpenAI, Deepgram and open-source services](docs/other-providers.md): Realtime
  Translate, conversational Realtime, Deepgram STT and local inference.
- [Embedded local models](docs/local-inference.md): Whisper, Qwen and Piper,
  automatic preparation, storage, downloads and offline use.
- [Gemini and ElevenLabs voices](docs/voices.md): library, voice design,
  cloning, per-direction selection, formats, requirements, costs and limits.
- [Architecture, memory safety and performance](docs/architecture.md).
- [Diagnostics and validation](docs/testing.md).
- [Start the tray at login](docs/autostart.md): optional per-user activation.
- [Complete example configuration](examples/babel.example.toml).
- [Contributing](CONTRIBUTING.md): English repository content, Conventional
  Commits, checks and release policy.

## Implemented integrations

| Translation/voice integration | Audio translation | Custom voice |
|---|---|---|
| Gemini Live Translate | Continuous audio-to-audio | Approximate automatic preservation; optional TTS for a fixed voice |
| Gemini 3.8 Live | Speech-to-speech with turns/VAD | Native preset voice; optional TTS |
| OpenAI Realtime Translate | Continuous audio-to-audio | Model voice; optional TTS |
| OpenAI Realtime | Speech-to-speech with turns/VAD | Native preset voice; optional TTS |
| Embedded Whisper + Qwen + Piper | Local segmented pipeline | Catalog Piper voices; optional cloud TTS |
| Gemini 3.8 TTS | Synthesizes translated text; not a standalone translator | Preset voices, design and registered clones |
| ElevenLabs | Synthesizes translated text; not a standalone translator | Library voices, design and instant voice cloning |

The four recognizers below produce the original-audio TXT, with any translator
or without translation. Each source selects its own provider and language.

| STT recognizer | Default model/path | Speakers and timing |
|---|---|---|
| Gemini | `gemini-3.5-transcribe-live` | No streaming diarization or word timestamps |
| OpenAI | `gpt-live-transcribe` | No speaker identification or word timestamps in the default adapter |
| Deepgram | Nova-3 through Listen v1 | Configurable diarization and provider word timestamps |
| Whisper | Embedded engine, Base model; optional Tiny/Small | Captured-segment offsets, no speaker identification |

Recognition profiles use `transcription.providers.*`; selection and language
live in `transcription.microphone_recognition` and
`transcription.speaker_recognition`. STS input text is neither saved nor used
instead of dedicated recognition. See [configuration and examples](docs/transcription.md).

Embedded local providers are prepared when their selection is saved. Installers
include engines; missing weights are downloaded with hash verification and
reused offline. No separate Python, Ollama or service installation/startup is
needed. External endpoints, including Ollama, remain optional for custom
installations. The UI tracks preparation and lets you choose the absolute model
folder and recognition/translation thread limits.

The library supports creating multiple voices, listing account profiles and
selecting a voice per direction. Reference/consent audio is sent only when
cloning is explicitly requested. Keys entered in the dashboard remain in memory
until the process exits; TOML stores only the corresponding variable name.

## Session files, participants and voices

Saved transcripts contain **original audio only**: one TXT per session combines
microphone and incoming-audio segments, labeled `[microphone]` and
`[received output]`. Segments are ordered
by arrival, which may differ from the exact order of speech across independent
connections. Timestamps are optional; text distinguishes audio offsets,
approximate alignment and local receipt time.

Audio recording is a separate option, off by default. It mixes selected originals
into **one PCM16 mono WAV at 16 kHz**, before translation and output gain.
Transcription and recording have independent source/folder selections. In
**Settings → Session files**, `files.base_path` defines a shared base folder
that is **always absolute**; the dashboard shows complete TXT/WAV destinations.
New configurations use `Babel` inside the user's home directory, independently
of the launch directory. Transcript/recording destinations can be relative to
that base or absolute. Legacy configurations with a missing or relative base
are migrated once against the TOML's directory and saved with an absolute path;
existing files are not moved. Check the destination if you previously launched
from a directory other than the configuration folder.
The shared filename pattern is `{date}-{time}-{session}-{id}`: UTC date/time,
a filename-safe title and a required identifier. `.txt`/`.wav` extensions are
automatic. An enabled feature needs at least one selected source with configured
devices, even if its translation is off. Recording originals needs no AI provider;
transcription needs speech recognition and may use cloud services according to
the selected profile. See [filenames and recording options](docs/configuration.md#session-files).

An optional bounded memory history keeps both original directions on the same
timeline. Its default retention is ten minutes and is configurable. Including
recent history when starting a session is an explicit opt-in under the advanced
start options; it applies to whichever file-producing features are enabled.

Deepgram can assign speaker IDs to words, and Babel preserves those transcript
metadata. IDs are connection-local labels, can be wrong and can restart after
reconnection; they are not names or persistent identities across sources/sessions.
**Automatically enrolling clones from the first seconds and assigning them to
participants is not implemented.** Two audio directions are not treated as
identification of people in a meeting. Gemini may approximate original voice
characteristics without guaranteeing a distinct vocal identity per participant.
Fixed/designed/cloned voices are selected explicitly.

Gemini requires a 10–30-second reference sample and a consent recording from
the same person to register a clone. This differs from Live voice preservation.
Details and alternatives are in the voice guide; the dashboard does not offer
controls that pretend unsupported features are available.

## Commands

```text
babel                         Local dashboard + tray
babel serve --port 0           Dashboard on an OS-selected port (default)
babel serve --no-tray          Local dashboard only
babel init                    Create babel.toml without overwriting
babel devices                 List actual capture/playback IDs
babel setup                   Create Linux devices; show driver guidance elsewhere
babel uninstall               Remove Linux devices; show native removal guidance
babel doctor                  Local diagnostics without sending audio to the cloud
babel run                     Run configuration without dashboard; Ctrl+C stops
babel run --session "Meeting"  Start a named session without dashboard
babel --config other.toml …    Use another configuration
```

Stopping a session does not remove virtual devices, so the calling app retains
its selection. Linux modules must be recreated after the audio server restarts.
Babel does not change the global default output.

In the dashboard, **Startup → Start in the tray at login** registers only the
tray and local service. It is off by default. On startup, Babel waits for the
virtual devices' activation conditions and routes original audio to configured
physical devices only while those conditions hold, without starting translation,
transcription or recording. You can undo startup registration on the same page.

## Validation status

Tests cover simulated WebSocket/HTTP protocols, queues/cancellation,
configuration, transcription, resampling, dashboard authentication and real
PipeWire with synthetic audio. Cross-compilation checks Windows code. Babel's
drivers have core tests and build paths separate from the application. Loaded
driver tests on macOS/Windows hardware, signed distribution packages and calls
with real provider accounts/keys are still required before presenting the product
as validated for production. See [reproducing the tests](docs/testing.md).
