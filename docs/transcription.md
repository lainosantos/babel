# Original audio transcription (STT)

The **Transcription** menu has its own providers, models, credentials, and languages.
The STT recognizer does not depend on the speech-to-speech (STS) translator or
the voice synthesizer (TTS). For example, you can translate with Gemini and
transcribe with Deepgram, or use only whisper.cpp to transcribe without
translation. The microphone and incoming output can use different recognizers.

## Configuring the dashboard

1. Open **Transcription** and enable original transcription.
2. Select the sources: **Original microphone**, **Original output**, or both.
3. For each source, choose the STT provider and original language. `auto` depends
   on the model's capabilities; an explicit language may improve recognition.
4. In **Transcription providers**, configure each selected profile:
   model, API key reference, endpoint, and available options.
5. For a cloud service, add the key in the STT profile itself or provide the
   corresponding environment variable when starting Babel. For whisper.cpp,
   keep **Built into Babel**. Whisper Base Q5_1 is the compact default;
   Tiny Q5_1 and Small Q5_1 are also available. Saving prepares the files
   automatically. An external server is optional.
6. Configure the **absolute** base folder, destination folder, filename pattern,
   and optional segment timestamps. Save and start a named session.

Profiles are shared by both sources within the STT section. To use different
languages, configure each source; using separate accounts with the same provider
per source would require additional profiles, which are not yet available.
Session settings take effect at startup; changing the physical device during
a session remains available through routing and the tray.

Keys entered in the dashboard remain only in process memory and are erased on
exit. The TOML file stores the **reference** (`api_key_env`), never the key.
By default, STT and translation may reference the same account variable. To
separate credentials, use different names, such as `GEMINI_STT_API_KEY` and
`GEMINI_TRANSLATION_API_KEY`. Changing the STT profile's reference does not change
the translation or voice profile.

## Transcribing recent history

After choosing sources and recognizers, open **Advanced start options** next to
**Start session**. Select **Include recent history** and enter the duration in
minutes. Without this choice, transcription starts with current audio. This is
also the default when starting from the tray, through `babel run` in the CLI,
or through the API without a positive `history_seconds`. Enabling retention
does not automatically include history. This choice applies only to that start
and returns to unchecked after a session starts successfully.

History uses the recognizers, languages, and sources currently selected in
**Transcription**, even if they differ from the configuration in effect when
the audio was captured. Only original audio is recognized. If transcription
is off, including history for a recording does not activate STT or send that
audio to a provider. With cloud STT enabled, including history sends the requested
interval to the selected service when the session starts.

History results precede live results in the TXT file. The dashboard indicates
when history transcription is pending; routing, playback, and translation
continue with live audio. Recognizing several minutes may take time and consume
provider quota. Stopping before completion may interrupt the historical prefix;
live finals still drain into the same file after that incomplete prefix is marked.

The default in-memory capacity is ten minutes, adjustable under **Settings →
Recent audio history**. The dashboard shows the audio available per source;
if less than requested is available, it includes only that interval. History
accumulates only while routing captures audio, is not persisted before explicit
inclusion, and disappears when Babel closes. The same controls are available
on Linux, macOS, and Windows. See [retention and inclusion in recordings](recording.md#include-audio-from-before-session-start).

History recognition uses a separate STT connection from live transcription.
Provider quotas, costs, and session limits still apply. If a connection expires
or the session ends before completion, Babel reports that history is incomplete;
it does not present the result as full recovery. Speaker identifiers are not
automatically matched between historical and live connections.

## Implemented providers

| STT provider | Transport and model | Speakers | Saved times | Requirements |
| --- | --- | --- | --- | --- |
| Gemini Live Transcribe | WebSocket; `gemini-3.5-transcribe-live` | No confirmed diarization in current streaming | Submitted-turn alignment, not word timestamps | Google key with access to the model |
| OpenAI Realtime Transcription | WebSocket; `gpt-live-transcribe` by default | No diarization in this adapter | Receipt time | OpenAI key with access to the model |
| Deepgram Listen | WebSocket v1; `nova-3` by default, also Nova-2 | Optional; IDs supplied by the API | Segment intervals derived from returned words | Deepgram key and compatible model/language |
| whisper.cpp | Embedded engine; multilingual Tiny/Base/Small, with compact Q5_1 variants. Optional external HTTP server | No diarization in this adapter | Boundaries of segments sent to the recognizer | Installer with runtimes; internet only to prepare missing weights |

Implemented support does not guarantee model availability for every account,
region, or language. Automated tests use local simulated servers and do not
measure API quality with real audio. The configuration and Rust adapters are
shared across Linux, macOS, and Windows; whisper.cpp server installation is
specific to the operating system.

### Gemini Live Transcribe

Select **Gemini** for the source and configure its profile in Transcription.
The official endpoint is fixed; the STT model is `gemini-3.5-transcribe-live`,
separate from the translation model. Input is mono PCM16 at 16 kHz. The session
requests text output and preserves the original speech, without a target language,
translation prompt, or TTS voice.

Babel saves final `inputTranscription` segments; speculative
`interimInputTranscription` hypotheses are not saved as final text.
Babel keeps one connection and uses manual activity boundaries, ending a turn
when input pauses, reaches the configured interval of exact digital silence,
or reaches five seconds of PCM (plus at most one input chunk). Quiet nonzero
samples are retained. Each explicit boundary waits up to five seconds for its
authoritative final result. This prevents continuous incoming speech from
remaining only an unsaved hypothesis until the session stops.
`auto` omits the language restriction; an explicit code is sent in
`languageCodes`. Current streaming does not guarantee diarization or word-level
timestamps. Features of the **file** API should not be confused with Live
Transcribe features. The documented maximum Live session duration is ten
minutes; reconnecting may create a gap and reset the provider's clock.

Sources: [Live Transcribe](https://ai.google.dev/gemini-api/docs/live-api/live-transcribe)
and [model capabilities](https://ai.google.dev/gemini-api/docs/models/gemini-3.5-transcribe).

### OpenAI Realtime Transcription

The STT profile uses `gpt-live-transcribe` by default. Additional families accepted
by the adapter are `gpt-transcribe` and `gpt-realtime-whisper`, including valid
dated snapshots. Audio conversation or translation models are not STT models.

Leave the endpoint empty to use the official Realtime transcription address.
An explicit address must implement the same WebSocket protocol; `/translations`
is rejected. Babel opens a `type: transcription` session, converts PCM from
16 to 24 kHz, and sends segments committed by local VAD (400 ms of silence).
It saves only final events, respecting the order of submitted segments.

An explicit language configures recognition; `auto` lets the model decide when
supported. `gpt-realtime-whisper` requires an explicit language. This adapter
does not request diarization or generate word-level timestamps; Babel's timestamp
option records receipt time when the service provides no alignment.
Fields such as translation prompt, voice, and target language are not sent.

Source: [Realtime transcription](https://developers.openai.com/api/docs/guides/realtime-transcription).

### Deepgram

The default endpoint is `wss://api.deepgram.com/v1/listen`, using model `nova-3`
and a key referenced by `DEEPGRAM_API_KEY`. The adapter accepts Nova-2/Nova-3
family models compatible with **Listen v1**; Flux uses another protocol and is
not implemented here. Input is mono PCM16 at 16 kHz, sent as binary data.

Language `auto` uses `language=multi` on compatible general models. This enables
multilingual recognition within the model's supported languages, not universal
detection of any language. For specialized models, select a compatible explicit
language. The punctuation option sends `punctuate` to the service.

With **Identify speakers** enabled, Babel sends `diarize_model=v1`, groups
consecutive words by the returned speaker ID, and records the actual start/end
times of those groups. With the option off, it does not request diarization.
Numeric IDs such as `0` identify API groupings, not verified people; they may
change after reconnection or between the two sources. This option does not
clone voices or change the translation voice.

Only `is_final` results are persisted. `speech_final` ends the turn without
duplicating text. During silence, the client sends a keepalive every three
seconds. Transient failures use the configured reconnection budget;
rejected authentication does not trigger endless retries.

Sources: [Listen v1](https://developers.deepgram.com/reference/speech-to-text/listen-streaming),
[diarization](https://developers.deepgram.com/docs/diarization),
[multilingual recognition](https://developers.deepgram.com/docs/multilingual-code-switching),
and [keepalive](https://developers.deepgram.com/docs/audio-keep-alive).

### Local whisper.cpp

Select `whisper.cpp` for the source's recognition and keep **Built into Babel**
in the profile. On save, Babel downloads and verifies the selected multilingual
weights if they are not already cached. The engine occupies RAM only when a
session with transcription enabled needs it. You do not need to install Python,
CMake, or Ollama, or start a separate server. The dashboard shows preparation,
download progress, availability, and failures.

Choose `tiny-q5_1` (32.2 MB), `base-q5_1` (default, 59.7 MB), or `small-q5_1`
(190.1 MB). These are weight sizes, not total RAM usage. The original options
`tiny`, `base`, and `small` remain valid, including in existing configurations.
Tiny prioritizes low resource use and may lose accuracy; Small uses more resources.
Results depend on hardware, language, accent, and noise.
The models folder, CPU threads, and memory release delay are under **Settings →
Local models**. The default uses up to two threads; after the last session releases
the engine, it shuts down after 60 seconds by default. Weights remain on disk.
The `idle_unload_secs` delay accepts 1–3600 seconds.
After preparation, this recognition works without internet. The engine uses
a dynamic local port: no default port is assumed or saved in the profile.

```toml
[transcription.microphone_recognition]
provider = "whisper"
language = "pt-BR"

[transcription.providers.whisper]
endpoint = "auto"
model = "base-q5_1"
api_key_env = ""
segment_ms = 2000
silence_ms = 300
vad_threshold = 0.01
request_timeout_secs = 30

[local_runtime]
directory = "" # Account cache, or an absolute path chosen by the user.
threads = 2
idle_unload_secs = 60
```

This profile recognizes only original audio. It does not call the translator
model, Piper, or a voice service. `auto` in the **language** field requests Whisper
language detection, and codes such as `pt-BR` are reduced to `pt`; `translate`
is always false. `endpoint = "auto"` selects the managed process, not the language.

Audio is segmented by energy VAD, with configurable maximum duration, silence,
RMS threshold, and timeout. Shorter segments reduce waiting but may reduce
context. Slow models increase latency; this is not continuous neural streaming.
Times correspond to the boundaries of the submitted audio, without word alignment
or speaker identification.

**External server (advanced)** remains available for your own installation.
Enter the full inference URL with its actual port. In this mode, your server loads
the model, and Babel's Whisper model selection does not change it. Babel's memory
release delay does not stop that external server.
Optional Bearer authentication is available only in external mode: enter a key
reference when required by the server/proxy. HTTP without TLS is accepted only
on loopback; remote addresses require HTTPS. Redirects and URLs containing
credentials, queries, or fragments are not accepted.

Read [Embedded local models](local-inference.md) for storage, preparation,
installation, and limits. External protocol reference:
[whisper.cpp server](https://github.com/ggml-org/whisper.cpp/tree/master/examples/server).

## Independent TOML example

This excerpt uses Gemini to recognize the microphone and Deepgram to recognize
incoming speech. Translators remain configured separately in `microphone`,
`speaker`, and `providers`. Omit unused profiles or keep their defaults;
no key is required for an inactive provider.

```toml
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
language = "en"

[transcription.providers.gemini]
api_key_env = "GEMINI_STT_API_KEY"
model = "gemini-3.5-transcribe-live"
connect_timeout_secs = 15
max_reconnect_attempts = 5

[transcription.providers.deepgram]
api_key_env = "DEEPGRAM_STT_API_KEY"
endpoint = "wss://api.deepgram.com/v1/listen"
model = "nova-3"
diarize = true
punctuate = true
connect_timeout_secs = 15
max_reconnect_attempts = 3
```

Also configure `files.base_path` with a valid absolute path for the system,
such as `/home/user/Babel`, `/Users/user/Babel`, or `C:\Users\user\Babel`.
See [configuration](configuration.md) for the filename pattern and folder
resolution; the destination does not depend on where the application started.
The path does not need to exist beforehand. When starting a session with
transcription, Babel recursively creates the destination folder and any missing
parent directories, including the base, on Linux, macOS, and Windows. This also
works with audio recording off. A destination access error occurs only when
Babel cannot create the folder or open the file; a missing folder alone does
not prevent the session.

## Files, routing, and limits

- A session produces **one TXT file** with the selected sources, identified as
  microphone or incoming output, and only in the original language. With both
  active, results enter the file as received; different provider delays do not
  guarantee exact global ordering of speech.
- The recognizer receives original audio, before translation and generated voice.
  Even when STS supplies auxiliary text, that text neither replaces nor duplicates
  transcription from the selected STT provider.
- Timestamps are optional. Provider offsets are mapped to the captured session
  timeline, preserving microphone/output pauses; unavailable metadata uses receipt
  time. Gemini provides submitted-turn alignment, not word boundaries. Network
  reconnections may lose audio and are marked in the TXT file.
- Transcription, translation, and recording are enabled separately. Recording
  audio alone does not open STT. If both directions are selected for transcription,
  there will be two independent connections/requests, even with the same provider.
  Simultaneous cloud translation and STT may also incur separate charges.
- The microphone is processed while Babel is the system's default microphone
  or an application uses its virtual microphone. Output requires an application
  sending audio to Babel. Deactivation immediately stops physical capture and
  playback. Already captured originals and pending STT results belong to the
  session and can still complete while the route is inactive. No further audio
  is captured from an unselected route. Translated playback never survives this
  boundary.
  Voice-command activation remains limited to the microphone and uses its own
  configuration, independently of file transcription STT.
- Each selected STT source opens its provider lazily on the first captured frame.
  Up to 20 seconds of original PCM are retained while it connects or catches up,
  with separate frame-count and sample-count bounds. This queue is independent
  of the optional ten-minute history buffer and is not saved before a session.
  Congestion beyond the bound reports a gap rather than blocking audio routing.
- **Stop** closes physical session routes first, then flushes the recognizers'
  unfinished speech and drains text/files for up to 15 seconds. Original routing
  can resume during this drain. A missing provider acknowledgement or timeout
  reports an incomplete transcript; closing a connection is not treated as proof
  that the last words were saved. Recognizer failures do not stop original audio.

## Older configurations

When loading TOML from before this separation, Babel migrates missing fields:
it copies each route's original provider and language, converts `local` to
`whisper`, and copies key references and ASR models into the new STT profiles.
Existing STT fields are preserved. Migration does not activate features that
were off. From then on, translation changes do not change STT.

The official OpenAI `/realtime/translations` endpoint is converted to `/realtime`
in the new recognition profile. A custom translation endpoint requires explicit
STT configuration, avoiding guesses about another destination.
Custom Whisper addresses are preserved. Empty local profiles or those with
recognized old defaults migrate to embedded preparation; old fixed ports are
not automatically reused. There is no `loopback` diagnostic provider: simply
turn translation off for original passthrough.
