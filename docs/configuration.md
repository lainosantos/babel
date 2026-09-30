# Configuration and operation

## Dashboard navigation

The dashboard separates controls into six pages:

- **Routing:** choose the physical microphone and output and the virtual devices
  for each direction. Original audio forwarding works without starting a session.
  The microphone activates when Babel is the system default or an application
  uses it; output activates when an application sends audio to Babel.
- **Translation and voices:** enable translation for each direction, choose
  languages, providers, and voices, and configure shared AI profiles and credentials.
  The voice library is on this page.
- **Transcription:** choose each source's recognizer and language, configure its
  profiles and credentials, optional timestamps, and TXT folder, and follow the
  most recently received segments.
- **Recording:** choose original audio sources and the folder for the single WAV file.
- **Commands:** configure microphone activation, local services, and MCP integrations.
- **Settings:** configure the shared base folder and filename pattern, audio quality,
  virtual devices, and automatic startup.

**Transcription** and **Recording** have a **Base folder and filenames** shortcut
to **Settings → Session files**. Features remain independent: you can record or
transcribe without enabling translation. Transcription has its own providers,
models, languages, endpoints, and credentials, independently of **Translation
and voices**. Changing pages preserves unsaved settings.

## Interface language

The **Interface language** selector changes the dashboard
and tray menus. Options are **System default**, **English**, and **Português**.
The default is `system`: Babel reads the user's operating-system language,
recognizes variants such as `pt-BR`, `pt-PT`, and `en-US`, and uses English when
no translation exists for the detected language or detection is unavailable.
The browser language does not override the language of the system running Babel.

```toml
[interface]
language = "system" # system, en, or pt
```

The choice is saved immediately and can change during a session while preserving
capture, playback, AI connections, the session name, and open files. It does not
change speech/translation languages, prompts, voices, or drafts of other settings.
Older configuration files without an `interface` section use `system`.
User-provided text, device names, technical identifiers, transcripts, and external
provider/system messages are not translated. Technical documentation linked from
help is written in English.

To add translations to the project, see [Internationalization](localization.md).

## Files and keys

Babel loads `babel.toml` from the working directory or the path passed with
`--config`. Without a file, the dashboard opens with defaults; saving creates the
file. `init` refuses to overwrite an existing file. Unknown fields, out-of-range
values, and incompatible capability combinations are errors. Writes use a temporary
file and atomic replacement. On Unix, newly created configuration, transcript,
and recording files have permission 0600.

There are four translation/synthesis profiles: `providers.gemini`,
`providers.openai`, `providers.elevenlabs`, and `providers.local`. Each direction
selects its translator in `microphone.provider` or `speaker.provider`. For example,
you can use Gemini for the microphone and OpenAI for output. ElevenLabs is a
**synthesizer**; it is not listed as a speech-to-speech translator in this application.

Original recognition uses `transcription.providers.gemini`, `.openai`, `.deepgram`,
or `.whisper`. The selections are `transcription.microphone_recognition` and
`transcription.speaker_recognition`, each with `provider` and `language`. For
example, you can translate with Gemini and transcribe originals with Deepgram,
or transcribe with Whisper without cloud AI. Choosing another translator does
not change recognition settings.

The former `loopback` diagnostic provider was removed. When loading legacy TOML
that selects it, Babel replaces the selection with `gemini`, turns off route
translation (`microphone.enabled` or `speaker.enabled`), and deselects its source
in transcription (`transcription.microphone` or `transcription.speaker`). If no
transcription source remains selected, it also turns off `transcription.enabled`.
Migration is saved atomically and preserves devices, recording, and other settings.
It does not start cloud calls: using AI in that direction requires explicitly
enabling translation or transcription and configuring the profile. New
configurations submitted through the API do not accept `loopback`.

Each cloud profile has an `api_key_env`. Defaults are `GEMINI_API_KEY`,
`OPENAI_API_KEY`, `ELEVENLABS_API_KEY`, and `DEEPGRAM_API_KEY`. STT and translation
can use different names to keep accounts/keys separate. Whisper accepts an
optional variable name for HTTP services requiring authentication.
There are two ways to provide a key:

- **Dashboard, key for this run:** the key stays in a separate memory store,
  is erased when replaced/removed, and is not returned by dashboard APIs.
  It takes precedence over the environment variable with the same name.
  Restarting Babel requires entering it again.
- **Environment variable:** set it before opening Babel. Linux/macOS:
  `export GEMINI_API_KEY='your-key'`. PowerShell:
  `$env:GEMINI_API_KEY='your-key'`. The dashboard shows only presence/absence.

Removing the temporary key makes the program fall back to the environment variable,
if present. Do not write keys in prompts, endpoints, voice names, or configuration
files. Credentials already used in an active session are reapplied on the next
connection; stop/restart the stream when changing accounts. `doctor` checks
presence, not remote validity, credits, or model access.

The dashboard server listens only on `127.0.0.1`. The initial URL contains a random
capability in its fragment; JavaScript uses Bearer authentication for requests.
Do not expose this server through a proxy/network without your own authentication
layer. The dashboard checks Host/Origin and loads no third-party scripts/fonts.

## Forwarding and sessions

Babel has a local audio path and an optional processing session. While the program
is open, configured routes forward the physical microphone to the virtual microphone
and the virtual output to the physical output. Without an active session, forwarded
content is original audio, without activating translators or creating session files.

The microphone is active when **Babel is the system's default microphone**, even
without an application capturing audio, or when an application explicitly uses
Babel's virtual microphone. This also enables voice-command listening, if enabled.
To pause this route, choose the physical microphone as the system default and stop
using Babel in applications that selected it explicitly. Choosing a new default
does not disconnect those applications.

Output is independent: it opens only while an application sends audio to Babel's
virtual output. Selecting Babel only as the default output, without playback from
an application, does not open that route or activate commands. These rules are
the same on Linux, macOS, and Windows.

`microphone.enabled` and `speaker.enabled` control **only translation** for each
direction. They do not turn off capture, playback, transcription, or recording.
During a session, a direction with translation off continues passing original
audio. The `transcription.*` and `recording.*` options choose their own sources.

**Start session** activates the selected features. **Stop session** finalizes
processing and files and returns to original forwarding. To also stop forwarding
and release capture, use **Quit Babel**. No session is needed with no features
selected; the button is unavailable and the backend also rejects that start.

| Session selections | Audio heard at destinations | Files |
|---|---|---|
| Recording only | Original in both directions | One WAV of the selected originals. |
| Transcription only | Original in both directions | One TXT of the selected originals. |
| Transcription and recording, without translation | Original in both directions | One TXT and one WAV. |
| Translation in one or both directions | Translated in enabled directions; original elsewhere | TXT/WAV only if enabled separately. |

The dashboard shows **Original audio** when only forwarding is active and
**Session active** during processing or writing. Routing errors appear even
without a session. Configure both ends of every route you want to use; a device
selection does not change the system's global default output.

## Translators and voices are separate choices

A direction's `provider` chooses its translator. `voice.engine` chooses the source
of its final voice:

| Voice engine | Behavior |
|---|---|
| `native` | Uses the translator's audio. This path has the fewest stages. |
| `gemini` | Receives translated text in memory and synthesizes it with Gemini TTS. |
| `elevenlabs` | Receives translated text in memory and synthesizes it with ElevenLabs. |

`voice.voice_id` is the voice identifier in the synthesizer's library. A Gemini
voice is not interchangeable with an ElevenLabs ID. In native conversational mode,
this field may contain a preset voice from that model. For dedicated translation
models, leave it empty: those APIs do not accept native selection of a fixed voice.

`voice.style` is a style prompt for Gemini TTS, not a translation prompt. It is
rejected for the implemented ElevenLabs endpoint and native mode instead of being
ignored. `voice.chunk_ms` limits waiting from the first pending text fragment
before sending an incomplete segment to TTS, from 100 to 2000 ms. Punctuated
sentences may be sent earlier; long segments are split at word boundaries.
A small value may fragment prosody and increase request counts/costs.

The Live translator continues generating audio when a TTS voice is used. The
adapter discards that audio; its output transcription is used only in memory
for synthesis. This mode may therefore charge for **Live translation and TTS**
and increases latency. Only original text requested by the user is saved.
In the local pipeline, Piper is skipped when external TTS supplies the final voice.

To create a voice, open the library, choose Gemini or ElevenLabs, and use design
or cloning. Read [the dedicated guide](voices.md) before preparing files.
Babel accepts multiple profiles returned by the service and existing IDs. It does
not secretly enroll participants during capture.

Whenever transcription is enabled for a source, its STT recognizer receives
original audio, including when translation is active. Defaults are
`gemini-3.5-transcribe-live` for Gemini, `gpt-live-transcribe` for OpenAI, and
`nova-3` for Deepgram. Configure language in `*_recognition.language`: `auto`
requests automatic detection when the adapter/model supports it; codes such as
`pt-BR` select a specific language. The translation language does not replace this value.

Embedded Whisper uses `transcription.providers.whisper.endpoint = "auto"` and
`model = "base-q5_1"` by default. Selecting the provider and saving downloads and
verifies missing weights; the engine loads only when a session uses this STT.
It does not require Ollama, Piper, or manually started services. An external
endpoint is an advanced option using an actual URL, without assuming a port;
an optional Bearer key belongs only to that external server. Original passthrough
and recording without transcription or translation neither send audio to
recognizers nor require a key.

`[local_runtime]` configures `directory`, `threads`, and `idle_unload_secs`.
An empty folder uses the application's cache for the system account; an explicit
value must be absolute on Linux, macOS, or Windows. It is not resolved relative
to the startup directory. `threads` accepts 1–64, defaulting to up to 2 according
to available CPUs, for recognition and text translation; it does not control
Piper synthesis's internal threads. The models folder is independent of the
TXT/WAV file base. Changing the folder does not move existing models.

`idle_unload_secs` accepts 1–3600 seconds, default 60. After the last session
releases the engines, this delay allows reuse before terminating managed processes
and releasing RAM. Weights remain on disk (`cached`). A session loads only what
its enabled features require; a local profile saved for a disabled feature does
not load its model. This policy does not stop external servers and is independent
of voice-command listening.

For local translation, `whisper_endpoint`, `ollama_endpoint`, and `piper_endpoint`
each accept `"auto"`. Managed models are Whisper (`whisper_model`: `tiny-q5_1`,
`base-q5_1`, or `small-q5_1`; `tiny`, `base`, and `small` remain valid), Qwen
(`translation_model`: `qwen3-0.6b`), and Piper (`piper_voice = "auto"` follows the
target language). The legacy name `ollama_endpoint` remains compatible, but in
embedded mode the engine is llama.cpp included in the installer.
`translation_api = "ollama"` or `"openai"` defines the protocol only for an external
endpoint. See [Embedded local models](local-inference.md) for the catalog,
downloads, requirements, and offline operation.

The quantized default applies to new configurations or those without a model
selection. Previously saved models and CPU limits are preserved.

`providers.*.transcription_model` remains a translator compatibility field,
used where its protocol requires internal recognition. It no longer determines
the model writing the TXT. See [the transcription guide](transcription.md).

In independent recognition, the TXT receives only final results; partials are
not repeated in the file. Stopping a session cancels pending recognition, so
the last sentence still processing may not appear. When that segment matters,
wait for a short pause and the final result before stopping. The writer drains
final results already received; speech not yet finalized by the service is not
guaranteed. WAV recording is a separate path and retains the selected capture
regardless of that result.

## Languages and prompts

Languages use BCP-47 codes such as `pt-BR`, `en-US`, and `es-ES`. The adapter may
normalize them to the code required by the API. The exact language list depends
on the model; a valid text field does not prove account/API support.

Gemini Live Translate and OpenAI Realtime Translate detect the source language.
`source_language` remains configurable for other models but is not a hint in
these dedicated modes. `target_language` determines the final language.

`prompt` contains per-direction translation preferences. It is available for
conversational models and the local pipeline. Dedicated models do not accept
prompts; Babel rejects that combination. A prompt guides behavior but does not
guarantee terminological fidelity or error-free model output.

Conversational voice models receive instructions to translate questions and
commands, not execute them. The application grants the model no tools or system
commands. Spoken text is translation content.

## Devices and quality

Choose **explicit** devices. Default-device aliases could cause feedback when
the calling application starts using the virtual output. Through the tray, you
can switch the **physical microphone** and **physical output** during translation.
This preserves provider connections and session files but may cause an audio gap.
Other dashboard settings, such as profiles, languages, voices, virtual cables,
and recording options, require stopping the session. The original path stays
active during configuration. Native IDs include direction and persistent device
identity: the UID on macOS and endpoint ID on Windows. Reordering the device list
does not change selection. Legacy index/name configurations are accepted only
when the name uniquely identifies a device in the correct direction; the old
index never selects another device. Refresh the list after connecting or
disconnecting hardware to see available devices.

`gain` adjusts a direction's **translated audio** volume from 0 to 4. PCM16 limits
are enforced by saturation to prevent numeric overflow; high values may cause
audible clipping.

Quality profiles adjust transport/VAD, rather than providing a fictitious cloud
model fidelity control:

| Profile | Local frame in non-dedicated models | Conversational VAD silence |
|---|---:|---:|
| `low_latency` | 10 ms | 200 ms |
| `balanced` | 20 ms | 400 ms |
| `high_quality` | 40 ms | 700 ms |

For dedicated models, Babel sends 100 ms frames. Internal translated playback
uses mono PCM16 at 24 kHz in frames of up to 20 ms. The Linux server or Rust
resampler adapts the physical rate. AI capture is mono at 16 kHz; the OpenAI
adapter converts it to 24 kHz. This does not make translated output high-fidelity
stereo music audio: its focus is translated speech.

Original passthrough uses float32 PCM, preserving the negotiated source sample
rate and channel count rather than converting it to the speech format. This also
applies during sessions with translation off; copies destined for WAV or ASR
are converted separately to mono at 16 kHz. Virtual devices remain the same when
starting/stopping a session, although reopening streams may produce a short gap.
Babel must remain running to forward audio; automatic startup is optional.

- `capture_queue_ms`: 100–1000 ms, default 200. Bounds capture and sending queues.
- `max_capture_age_ms`: 100–1000 ms, default 200. Old local frames are discarded.
- `playback_queue_ms`: 100–5000 ms, default 2000. A ceiling, not a deliberate delay.
- `device_latency_ms`: 5–200 ms, default 30. A latency request to the backend;
  drivers and the system may choose a different buffer. CPAL uses the supported
  native configuration, so this number does not guarantee an exact physical buffer.

If the translator produces audio faster than it can be played, the queue does
not grow indefinitely: the stream stops with an error. Increasing queues absorbs
bursts but may increase maximum delay. Repeated drops indicate insufficient
network, device, queue, or hardware capacity. With local processing, the
STT/LLM/TTS models must process faster than speech to sustain a long session.

## Session files

Before starting, **Session name** accepts an optional title of up to 100 characters,
such as “Team meeting”. This name applies only to the new run; it does not change
provider profiles. Leaving it empty generates a title with the UTC date/time.
The dashboard retains the session name after stopping until another session starts.
From the CLI: `babel run --session "Team meeting"`. Starting from the tray uses
an automatic name; open the dashboard to enter your own.

The full title appears in the transcript header even with timestamps off.
`files.name_pattern` defines one shared basename for TXT and WAV. The default is
`{date}-{time}-{session}-{id}`. Available placeholders are:

| Placeholder | Contents |
|---|---|
| `{date}` | UTC start date, in `YYYYMMDD` format. |
| `{time}` | UTC start time, in `HHMMSS` format. |
| `{session}` | A safe version of the title, lowercased, with separators converted to hyphens and a 64-byte UTF-8 limit. |
| `{id}` | Random session identifier; required to distinguish runs. |

The pattern accepts 1–128 bytes. Unknown placeholders, control characters, path
separators, and reserved characters are rejected. The resulting basename is also
validated and limited to 240 bytes; Windows reserved names and names ending in
a dot or space are rejected for both outputs. Enter only the basename: the
program appends `.txt` and `.wav`. Repeating a title still produces distinct
names; a file collision causes an error, never an overwrite. For example:

```text
transcripts/20260929-150000-team-meeting-1a2b3c4d.txt
recordings/20260929-150000-team-meeting-1a2b3c4d.wav
```

Transcription and recording are independent: each has `enabled`, `microphone`,
`speaker`, and `directory`. An enabled feature must select at least one source
with capture configured; that source does not need translation enabled. Selecting
no sources for an active feature prevents startup. Folders may be relative to
`files.base_path` or absolute, and may differ between TXT and WAV. Files are
stored on the machine running Babel, not in the browser's downloads folder.
Both options are off by default.

```toml
[files]
# Choose an absolute path appropriate for your system:
# base_path = '/home/anna/Babel'
# base_path = '/Users/anna/Babel'
# base_path = 'C:\Users\Anna\Babel'
name_pattern = "{date}-{time}-{session}-{id}"

[transcription]
enabled = true
microphone = true
speaker = true
timestamps = true
directory = "transcripts"

[transcription.microphone_recognition]
provider = "gemini"
language = "pt-BR"

[transcription.speaker_recognition]
provider = "deepgram"
language = "auto"

[transcription.providers.gemini]
api_key_env = "GEMINI_STT_API_KEY"
endpoint = "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent"
model = "gemini-3.5-transcribe-live"
connect_timeout_secs = 15
max_reconnect_attempts = 5

[transcription.providers.deepgram]
api_key_env = "DEEPGRAM_API_KEY"
endpoint = "wss://api.deepgram.com/v1/listen"
model = "nova-3"
connect_timeout_secs = 15
max_reconnect_attempts = 3
diarize = true
punctuate = true

[recording]
enabled = false
microphone = true
speaker = true
directory = "recordings"
```

`babel init` populates `base_path` with the `Babel` folder inside the home directory.
When creating TOML manually, set this field explicitly: an existing file without
`base_path` is treated as legacy and migrated to the configuration file's own
folder according to the rules below.

### Audio history and advanced start

By default, Babel retains up to ten minutes of recent original audio in memory,
separately per source, while routing captures audio. Configure **Settings → Recent
audio history** or this section:

```toml
[history]
enabled = true
duration_secs = 600
```

`duration_secs` accepts integers from 1 to 3600. Capacity is a limit: starting Babel
recently, an inactive route, or a capture failure may leave less audio available.
History is discarded from oldest to newest as the window advances. Saving a lower
capacity removes the part exceeding the new limit; saving `enabled = false`
clears all retained memory. Starting/stopping sessions or switching devices does
not automatically erase history. Closing the application loses it.

To include an interval, use **Advanced start options → Include recent history**
next to **Start session**. Choose the duration in minutes, limited to capacity.
The **Audio available in memory** counter shows the total stored, updated every
second. Input and output are considered together: simultaneous intervals count
once, and gaps without capture do not increase the total. Configured capacity
appears separately. The option starts unchecked and is not saved in TOML: it is
a decision for each new session. Normal startup from the dashboard, tray, or
`babel run` in the CLI always starts without history. The API also defaults to
no history: inclusion requires a positive `history_seconds` in that request.
Keeping retention enabled does not enable inclusion in any session.

The interval goes only into enabled recording and/or transcription, using each
feature's selected sources and the current STT recognizer configuration. It does
not change those selections. It is not played back or translated. The TXT receives
history results before live results; the dashboard shows when that recognition
is still pending. Before explicit inclusion, retention creates no WAV/TXT and
sends no history for recognition. Controls are the same on Linux, macOS, and Windows.

In the API, `POST /api/start` accepts integer `history_seconds` from 0 to the saved
capacity (maximum 3600), together with the optional name and `If-Match` revision.
Omitting the field or using zero starts without history. A positive value requests
only the available portion of the window; it requires recording or transcription
to be enabled with a selected source containing available audio. Capacity is saved
through `/api/config`; the inclusion choice is not persisted. `/api/status` exposes
`history.enabled`, `capacity_secs`, `available_secs`, `combined_audio_secs`,
`microphone_secs`, and `speaker_secs`, plus `history_included_secs` and
`history_transcription_pending` for the session.

### Base folder and destinations

Configure **Settings → Session files → Base folder** in the dashboard. The field
remains available with recording and transcription off. The preview shows full
destinations calculated by the system running Babel, including unsaved changes.
Click **Save settings** before the next session. Folder changes do not apply to
a running session and do not move or delete existing files.

- `files.base_path` is the shared base and must be **an absolute path**. New
  configurations use the `Babel` folder in the user's home directory (`HOME`
  on Linux/macOS and `USERPROFILE` on Windows). The destination does not depend
  on the folder in which the terminal, shortcut, or automatic startup opens the program.
- The dashboard and API reject relative bases, including `"."`. There is no
  fallback to the working directory. If the home directory cannot be determined,
  configure an absolute path explicitly.
- Relative `transcription.directory` and `recording.directory` values are appended
  to the base. An absolute path in either field ignores the base for that file
  type. `"."` in these fields saves directly to the base folder.
- Paths follow the rules of Babel's operating system. Reading the home directory
  for the default does not mean expanding `~`, `$HOME`, or `%USERPROFILE%` in
  fields: enter the complete path. On Windows, the base requires a full drive
  path or UNC; ambiguous forms such as `C:folder` and `\folder` are rejected.
- The base and destination folders need not exist beforehand. When starting a
  session with recording or transcription, Babel recursively creates all missing
  directories in that feature's destination, including the base's parents. This
  also applies to separate absolute destinations on Linux, macOS, and Windows.
  Existing folders are reused.
- Previewing and saving configuration neither create folders nor test write
  permission. A missing destination alone is not an error: the session fails only
  if it cannot create the folder or open the file, for example because permission
  is missing or a file occupies a directory's place. Destinations for disabled
  features are not created. The filesystem follows symbolic links; the preview
  does not resolve them.

Examples of absolute bases in TOML (single quotes preserve Windows backslashes):

| System | Configuration | WAV destination with `directory = "recordings"` |
| --- | --- | --- |
| Linux | `base_path = '/home/anna/Babel'` | `/home/anna/Babel/recordings` |
| macOS | `base_path = '/Users/anna/Babel'` | `/Users/anna/Babel/recordings` |
| Windows | `base_path = 'C:\Users\Anna\Babel'` | `C:\Users\Anna\Babel\recordings` |

For example, for Linux user Anna, a new configuration uses `/home/anna/Babel`:
TXT files go to `/home/anna/Babel/transcripts` and WAV files to
`/home/anna/Babel/recordings`. Opening the program from another directory, through
a script, or through automatic startup does not change these destinations.

### Migrating older configurations

When loading an existing file without `files.base_path`, Babel fixes the
configuration file's folder as the absolute base. An old relative base is resolved
once against that same folder: for example, `base_path = "sessions"` in
`/home/anna/config/babel.toml` becomes `/home/anna/config/sessions`, and `"."` becomes
`/home/anna/config`. The absolute value is persisted by atomic TOML replacement.
Migration does not move or remove existing recordings or transcripts.

This rule preserves the old destination when configuration lived alongside the
folder used as the base. If Babel started from another directory, the previous
destination cannot be inferred from TOML: check the preview and enter the desired
absolute folder before the next session. If migration cannot be saved, fix the
configuration file's write error and try again.

### Transcribing originals

On **Transcription**, enable the feature, select sources, optional timestamps,
and the text folder. Choose each source's recognizer and language on that same
page, with its own AI profiles. Use **Base folder and filenames** to change shared
settings without enabling translation.

When loading older TOML without the new STT settings, Babel copies each direction's
previous selection and source language (`local` becomes `whisper`), its keys,
recognition models, and compatible endpoints. Explicit STT fields are preserved.
Migration does not enable translation or transcription and saves the result
atomically. An official OpenAI endpoint ending in `/v1/realtime/translations`
is adjusted to `/v1/realtime` on the same host. A custom translation endpoint
requires configuring an explicit STT endpoint; Babel does not redirect it to
another service. If the Whisper endpoint was absent from the old file, it stays
empty without introducing an assumed port.

`transcription.enabled` enables **a single TXT per session**, containing only
original text from the selected directions. The labels `[microphone]` and
`[received output]` identify where each segment came from, not participants.
Translated text used internally for voice synthesis is not saved in this file.

The two adapters may deliver fragments with different delays. The TXT follows
their arrival order at Babel; it does not reorder speech by timestamps or promise
to reconstruct the exact chronology of overlapping conversation.

The file is written incrementally in a separate worker. Write errors or a full
transcription queue are reported; text is not silently discarded. Segments
preserve spaces between provider deltas. The content is the model's transcript,
without a second translation.

With `timestamps = true`, markers have explicit meanings:

- `[audio +start–ends]`: audio offsets supplied/measured by the adapter.
- `[alignment +Xs]`: an approximate point supplied by the API, not a word boundary.
- `[received at ...]`: UTC arrival time of text at Babel; includes AI delay.

Receipt time should not be interpreted as the exact moment of speech. Reconnections
are marked and may reset provider offsets. With timestamps disabled, these speech
markers are omitted; the header still identifies the session. Actual speaker IDs,
if received, remain in the file. Missing IDs are not filled with invented names.
Persistent automatic identification of people/clones within a mixed call is a
current limitation.

### Recording originals

On **Recording**, enable the feature and choose sources and the audio folder.
The **Base folder and filenames** shortcut opens settings shared with transcription
under **Settings → Session files**.

`recording.enabled` enables **a single little-endian PCM16 WAV, mono at 16 kHz**.
It receives original physical microphone capture and original virtual output
capture according to the `microphone` and `speaker` selections. Sources are mixed
into one track. Capture occurs before translation and translated audio gain:
AI voices and changes to that gain do not enter the recording.

You can record without transcribing, transcribe without recording, or enable both.
Neither option depends on `microphone.enabled` or `speaker.enabled`: those fields
enable only translation. A recording session without transcription or translation
does not require an AI key. Switching physical devices through the tray retains
the same session file, with a possible gap during switching. For format, mixing,
disk failures, and limits, see [recording originals](recording.md).

## Tray and shutdown

`serve` opens a Linux tray service through StatusNotifier/KSNI, without GTK.
The desktop must support StatusNotifier; some GNOME installations require enabled
AppIndicator support. Without it, use the printed link or `serve --no-tray`.
Windows and macOS use native menus and their own event loop.

**Start session** uses features from saved configuration; **stop session** cancels
AI, finalizes files, and returns to original audio; **settings** opens the dashboard
in the browser; **quit** also stops capture, forwarding, and the server. Closing
the dashboard tab keeps audio running.

The **Physical microphone** and **Physical output** submenus show real devices and
mark the current selection. The first changes capture for the microphone direction;
the second changes where output plays the translation. Virtual cables remain the
same. Switching also works during an active session, preserving the provider,
transcription, and recording in progress. **Refresh devices** enumerates again
after connecting or disconnecting a headset. An invalid selection does not silently
fall back to another device.

If a device fails or is removed, the dashboard reports the error. Select another
physical device from the tray to recover audio in the same session. The unavailable
interval may create a gap; old audio does not accumulate in playback or translation
queues. Optional in-memory retention is separate from those queues and enters
recording/transcription only through explicit selection at startup. Changing
languages, models, voices, and other options still requires stopping the
processing session.

These selections are saved in the same TOML as the dashboard. The dashboard tracks
external changes; if local settings remain unsaved, it requests a reload before
saving or starting. Operations check the server's configuration revision, preventing
an old tab from overwriting a selection made through the tray.

In `run` mode, Ctrl+C stops the program. None of these paths removes virtual modules.
The explicit `uninstall` action cleans up Linux modules owned by Babel.

## Optional startup at login

Under **Startup**, select **Start in the tray at login** and apply. Registration belongs
to the current user and opens the tray/dashboard. Original audio is captured and
forwarded through configured devices; translation, transcription, and recording
wait for a session. Unchecking and applying removes registration. Simply opening
the dashboard does not change login configuration. A new instance generates a
different local URL/token; open the dashboard through the tray's **Open settings** menu.

Registration does not store API keys. Temporary keys entered during the previous
run must be entered again unless you configured variables in the user's session
environment. Read [startup by system](autostart.md) for the files used, path
portability, and desktop requirements.
