# Babel

[Website](https://lainosantos.github.io/babel/) ·
[Documentation](https://lainosantos.github.io/babel/docs/index.html) ·
[Releases](https://github.com/lainosantos/babel/releases)

Virtual microphone and audio output for bidirectional speech translation. The
core, AI adapters, dashboard server and tray controls are written in Rust. Each
direction can use a different translation provider and target language.
Transcription also selects its own provider and language for the microphone
and incoming audio, independently of translation.
Translation, transcription and recording are independent. Outside a session,
Babel routes original audio between the configured devices. Turning off a
direction's translation preserves that routing during a session as well.

## Getting started

[Linux](#linux) · [macOS](#macos) · [Windows](#windows) ·
[First session](#first-session)

Download the Babel installer for your system from [Releases](https://github.com/lainosantos/babel/releases).
Open **Babel** and choose **Settings** from its tray or menu bar icon to open the
dashboard. The installer includes the local inference engines. You can install
and run Babel without Python.

### Linux

1. Install the `.deb` on Debian or Ubuntu, or the `.rpm` on Fedora and other
   compatible distributions. Use PipeWire with `pipewire-pulse` or PulseAudio
   in your desktop session.
2. Open Babel. In dashboard **Settings**, click **Create virtual devices**.
3. In **Routing**, send microphone audio to **Babel_Microphone_Bus**
   (`babel_mic_bus`) and capture received audio from
   `babel_speaker.monitor`.
4. In your calling app, select **Babel_Microphone** as the microphone and
   **Babel_Speaker** as the speaker.

If your desktop has no tray icon, run `babel serve --no-tray` and open the
dashboard link printed in the terminal.

[Linux setup and troubleshooting](docs/platforms.md#linux)

### macOS

1. On macOS 14.2 or later, install Babel's universal `.pkg`. Install both
   [BlackHole 2ch and BlackHole 16ch](https://github.com/ExistentialAudio/BlackHole#installation-instructions).
2. Open Babel from **Applications**, grant microphone access, and click
   **Refresh devices** in dashboard **Settings**.
3. In **Routing**, send microphone audio to **BlackHole 2ch** and capture
   received audio from **BlackHole 16ch**.
4. In your calling app, select **BlackHole 2ch** as the microphone and
   **BlackHole 16ch** as the speaker.

[macOS permissions and setup](docs/platforms.md#macos)

### Windows

1. Install Babel's `.exe` for your system architecture. On Windows x64,
   install the signed [usbip-win2 transport](https://github.com/vadimgrn/usbip-win2/releases)
   and [Virtual Cables](https://github.com/tarekwasfy01/Virtual-Cables/releases).
2. Open Virtual Cables, create two cables, and leave it running. Open Babel
   from the **Start** menu and click **Refresh devices** in dashboard **Settings**.
3. In **Routing**, send microphone audio to the playback side of
   **Virtual Cable 01** and capture received audio from the recording side of
   **Virtual Cable 02**.
4. In your calling app, select **Virtual Cable 01** as the microphone and
   **Virtual Cable 02** as the speaker.

The Virtual Cables setup on Windows on Arm still needs validation.

[Windows permissions, VB-CABLE and Windows on Arm](docs/platforms.md#windows)

### First session

1. In **Routing**, select your physical microphone as the microphone source
   and your headphones as the physical output. Click **Save settings** and
   test both directions in a call with the original audio.
2. In **Translation**, select a provider and the languages for each direction
   you want to translate. For a cloud provider, enter its API key in the
   profile. For local translation, choose **Local** and save to prepare the
   models.
3. To save the original audio or text, enable **Recording**, **Transcription**,
   or both. Transcription has its own provider and language settings.
4. Click **Save settings**, then **Start session**. **End session** stops
   translation and transcription. Audio and text already received finish saving
   in the background, and the call returns to original audio.

For a call using only original audio, save the routing settings and place the
call directly.

### Run from source

With Rust **1.90 or later**, run these commands from the repository:

```sh
cargo build --release --locked --bins
cargo run --release -- serve
```

Open the complete dashboard link printed in the terminal, or use **Settings**
from the tray. Use `serve --no-tray` for a dashboard-only launch. Follow the
[local model guide](docs/local-inference.md) for preparing the native engines
when building from source.

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
- [Gemini Live](docs/providers.md): protocols, models, transcriptions,
  capabilities and continuous translation restrictions.
- [OpenAI, Deepgram and open-source services](docs/other-providers.md): Realtime
  Translate, conversational Realtime, Deepgram STT and local inference.
- [Embedded local models](docs/local-inference.md): Whisper, Qwen and Piper,
  automatic preparation, storage, downloads and offline use.
- [Native translation voices](docs/voices.md): model defaults, local synthesis
  and migration of legacy voice settings.
- [Architecture, memory safety and performance](docs/architecture.md).
- [Diagnostics and validation](docs/testing.md).
- [Start the tray at login](docs/autostart.md): optional per-user activation.
- [Complete example configuration](examples/babel.example.toml).
- [Contributing](CONTRIBUTING.md): English repository content, Conventional
  Commits, checks and release policy.

## Implemented integrations

Gemini Live Translate and OpenAI Realtime Translate stream translated audio continuously.
Conversational models wait for end-of-speech detection, and local translation
works in segments. Output voices follow each model's defaults.

| Translation integration | Audio translation | Native output |
|---|---|---|
| Gemini Live Translate | Continuous audio-to-audio | Model default; best-effort preservation of vocal characteristics |
| Gemini 3.8 Live | Speech-to-speech with turns/VAD | Model default |
| OpenAI Realtime Translate | Continuous audio-to-audio | Model default |
| OpenAI Realtime | Speech-to-speech with turns/VAD | Model default |
| Embedded Whisper + Qwen + Piper | Local segmented pipeline | Catalog default for the target language |

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

Keys have separate temporary and saved dashboard fields. Temporary keys remain
in memory until Babel exits; saved keys persist alongside settings in the user
configuration file. Priority is temporary, saved, then environment. Each value
has its own remove button; APIs never return secrets. See [configuration](docs/configuration.md#files-and-keys)
for platform paths and file permissions. The session dock also shows elapsed
session time across views and dashboard reloads.

## Session files, participants and voices

Saved transcripts contain **original audio only**: one TXT per session combines
microphone and incoming-audio segments, labeled `[microphone]` and
`[received output]`. Segments are ordered
by arrival, which may differ from the exact order of speech across independent
connections. Timestamps are optional; text distinguishes audio offsets,
approximate alignment and local receipt time.

Audio recording is a separate option, off by default. It saves originals in
**one PCM16 WAV at 16 kHz**, with a stereo channel pair for each source.
Both layouts offer recording-only gain. The optional mono mix also offers microphone priority.
See [recording formats and balance](docs/recording.md#format-and-volume).
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

An optional encrypted rolling history keeps both original directions on the same
timeline. Its default retention is ten minutes and is configurable. Including
recent history when starting a session is an explicit opt-in under the advanced
start options; it applies to whichever file-producing features are enabled.

Incomplete file sessions keep originals available for recovery while Babel stays
open. Spill into the system temporary folder is encrypted with a per-session key
held only in RAM; successful completion releases it. Recovery writes a new file
pair and preserves earlier partial files. This does not survive process exit. See
[session recovery and limits](docs/recording.md#recover-an-incomplete-session).

Deepgram can assign speaker IDs to words, and Babel preserves those transcript
metadata. IDs are connection-local labels, can be wrong and can restart after
reconnection; they are not names or persistent identities across sources/sessions.
Two audio directions do not identify the people in a meeting. Gemini Live
Translate may approximate original voice characteristics without guaranteeing
a distinct vocal identity per participant. Other models have their own native
output behavior. Babel does not assign voices to participants or override the
model's default voice. See [native translation voices](docs/voices.md).

## CLI commands

```text
babel                         Local dashboard + tray
babel serve --port 0           Dashboard on an OS-selected port (default)
babel serve --no-tray          Local dashboard only
babel init                    Create the user config file without overwriting
babel devices                 List actual capture/playback IDs
babel setup                   Create Linux devices or show external device setup
babel uninstall               Remove Linux devices or show external device removal guidance
babel doctor                  Local diagnostics without sending audio to the cloud
babel run                     Run configuration without dashboard; Ctrl+C stops
babel run --session "Meeting"  Start a named session without dashboard
babel --config other.toml …    Use another configuration
```

Stopping a session does not remove virtual devices, so the calling app retains
its selection. Linux modules must be recreated after the audio server restarts.
Babel does not change the global default output.

In the dashboard, **Startup → Start in the tray at login** registers the tray
and local service for the current user. The same setting is available through
`babel autostart`. It is off by default. On startup, Babel waits for the
virtual devices' activation conditions and routes original audio to configured
physical devices only while those conditions hold, without starting translation,
transcription or recording. You can undo startup registration on the same page.

## Validation status

Tests cover simulated WebSocket and HTTP protocols, configuration, audio
processing, and routing through PipeWire with synthetic audio. Windows
cross-compilation and installer contract tests check the portable code and
packaging. Audio on macOS and Windows still requires validation on those
systems. See [reproducing the tests](docs/testing.md).
