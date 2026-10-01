# Speech providers

`SpeechProvider` receives PCM16 mono at 16 kHz through a bounded channel and
returns text events and, for translators, PCM16 mono at 24 kHz. Each direction
maintains independent translation (STS) and transcription (STT) sessions when
those features are enabled. Original routing and recording without
transcription/translation do not open providers. Networking, JSON, Base64 and AI
stay outside audio callbacks. To add a provider, implement the trait and register
its factory.

## Profiles and per-direction selection

`providers.gemini`, `providers.openai` and `providers.local` hold independent
settings. `microphone.provider` and `speaker.provider` select each direction's
translator. Both directions play the translator's native audio with its default
voice, without a separate synthesizer or fixed voice override. Keys live in
process memory or the environment; TOML stores only the credential name. Recognizers
use separate profiles: `transcription.providers.gemini`, `.openai`, `.deepgram`
and `.whisper`. `transcription.microphone_recognition` and
`transcription.speaker_recognition` independently select the original-speech
provider/language for each source, without inheriting `microphone.provider`,
`speaker.provider` or their languages. The TXT receives only dedicated STT
results, even during simultaneous translation. Translator input text is not saved.

Read [independent transcription and its four providers](transcription.md),
[configuration and operation](configuration.md), [OpenAI and the local pipeline](other-providers.md)
and [native translation voices](voices.md).

## Gemini

The integration uses the official v1beta WebSocket, certificate-verified TLS and
the key named by `api_key_env`. The key travels in `x-goog-api-key` rather than
the URL; the header is marked sensitive. The client does not accept a custom
endpoint and never includes error bodies, close reasons or remote messages in
local errors. Credentials resolve first from temporary dashboard storage, then
from the environment, using `providers.gemini.api_key_env`. The resolved copy
uses `Zeroizing`; internal HTTP/TLS copies and the process environment have no
erasure guarantee.

Translation has two modes selected by model:

| Model | Behavior | Configuration |
|---|---|---|
| `gemini-3.5-live-translate-preview` | Continuous translation as audio arrives; the default model for real-time translation | Target BCP-47 code; automatic source language. Voice, prompts, VAD and reasoning are not sent. `echoTargetLanguage=true`: speech already in the target language remains audible. |
| `gemini-3.8-live` | Bidirectional audio, with generation subject to the model's activity/turn detection | Languages in the interpreting prompt and VAD silence duration; model-default voice. `NO_INTERRUPTION` allows capture to continue while translation plays. |

The client does not turn `gemini-3.8-flash` into a speech model or promise
continuous translation in generic Live mode. Availability/permissions depend
on the Google account. Models may change; their names remain configurable.
The `models/` prefix is optional. Translation-specific mode activates only for
the documented identifier, not a partial match.

Babel enables [target-language echo](https://ai.google.dev/gemini-api/docs/live-api/live-translate#configuration)
because translated routes replace original playback. Disabling it would silence
speech already in the destination language. For example, when both directions
target English, translated microphone speech can return as incoming English in
a two-account call. Original recording captures that incoming speech before
speaker translation, so an audible recording alone does not confirm playback
at the headphones or speakers.

Live Translate accepts a specific set of target codes. Babel maps regional
locales such as `en-US` and `en-GB` to the documented `en`, and `es-MX` to `es`,
when constructing the request. Saved settings retain the original locale;
the provider does not offer a separate regional target for those languages.
`pt-BR` and `pt-PT` remain distinct. Chinese `zh-CN`/`zh-SG` map to `zh-Hans`,
and `zh-TW`/`zh-HK`/`zh-MO` to `zh-Hant`; an explicit script takes precedence.
Choose a specific variant for bare `pt` or `zh`. Unsupported languages, scripts
and extensions fail validation before connecting. This mapping applies only
to the dedicated Gemini translator, not generic Live, STT or other providers.
See the [supported targets](https://ai.google.dev/gemini-api/docs/live-api/live-translate#supported-languages)
(checked 2026-09-30).

In continuous mode, nonempty prompts are configuration errors. The client sends
no `clientContent`, text or end-of-turn markers. In generic mode, the prompt
instructs the model to treat captured questions/commands as content to translate.
This is a model instruction, not a formal guarantee against instructions in speech.

The API receives Base64-encoded little-endian binary PCM in `realtimeInput.audio`.
Input begins only after `setupComplete`. The engine uses 100 ms frames in
continuous translation mode. A response may contain several audio fragments in
one event; all are processed. Output plays as it arrives, without waiting for
`turnComplete`. That event only signals generation completion to consumers.

Translation sessions disable `input_transcription`: STS input text does not
feed the TXT. Native translated audio plays directly, without another synthesis
stage. Original recognition uses a separate STT session fed the same original
audio from the selected source. Enabling or changing transcription does not
change the translation provider, model, language or native audio behavior.

## Gemini: independent transcription

On **Transcription**, select Gemini for the microphone, incoming audio or both.
`transcription.providers.gemini.model` uses `gemini-3.5-transcribe-live`; its key,
fixed endpoint and limits belong to this STT profile.
`transcription.microphone_recognition.language` and
`transcription.speaker_recognition.language` accept BCP-47 or `auto`.
The session requests `TEXT` and `VERBATIM` mode without a voice, target language
or translation prompt. It receives PCM16 mono at 16 kHz and returns final
original text. This path works with translation off or alongside any translator.
The legacy `providers.gemini.transcription_model` field does not control the new
running profile; see [STT configuration and migration](transcription.md).
See the [official Live Transcribe guide](https://ai.google.dev/gemini-api/docs/live-api/live-transcribe).

The adapter ignores partial hypotheses to avoid duplicating TXT content. It
bounds explicit Live turns to five seconds plus at most one input chunk and
waits up to five seconds for a final result, including a valid empty result.
If Live stalls, disconnects or requests rotation, Babel retains all unconfirmed
original PCM and retries the affected window through the same configured Live
Transcribe model. Normal transcription, retained history, and explicit session
recovery use `transcription.providers.gemini.model`; no other model is selected
implicitly. Microphone and output recover independently. No file-transcription
request or Files API upload is made by this adapter.

A successful recovery preserves timestamps and produces no gap marker. Only
explicitly completed requests commit recovered text; real recovery failure or
buffer exhaustion remains an incomplete-transcript error. Stopping capture
drains already captured originals while original routing can restart
independently. WAV recording remains a separate path. See
[recovery bounds and behavior](transcription.md#gemini-live-transcribe).

During active capture, transient window failures keep retrying with a 30-second
cooldown after the initial reconnect budget. After Stop, failed attempts are
bounded by the profile's reconnect budget (at least three retries). Permanent
Gemini authentication and protocol failures pause immediately. The dashboard
reports the sanitized cause, and incomplete originals
remain retained for explicit recovery rather than generating endless requests.
Translation checks output selection before retry inference. During capture it
waits locally for reselection; after Stop it pauses unselected work for explicit
recovery. Neither case makes paid requests while unselected. Catch-up only
advances its cursor.

## Participants, timestamps and voice identity

Live Translate attempts to reproduce vocal characteristics automatically. This
is a model capability, not an identity guarantee. Babel uses the model's native
defaults and does not enroll voices or configure a reference sample. Google
documents voice changes after pauses and confusion during rapid speaker changes.
[Live Translate limitations](https://ai.google.dev/gemini-api/docs/live-api/live-translate#limitations).

Call audio usually arrives already mixed. Identifying the “microphone” or
“output” direction does not identify every person in that signal. A manually
assigned channel name must be presented as a channel label, never automatic
speaker identification.

The presence of `diarization` and `wordTimestamp` in the shared
`AudioTranscriptionConfig` schema does not confirm model compatibility. The
`gemini-3.5-transcribe-live` documentation excludes streaming diarization and
word timestamps. The provider
therefore neither sends those options nor invents speakers/timing alignments.
[Live Transcribe guide](https://ai.google.dev/gemini-api/docs/live-api/live-transcribe#limitations).

The abstraction preserves authentic metadata when supplied: `speakerLabel`
and offsets of the first/last entries in `words` are exposed through
`TranscriptMetadata`. IDs/lists are bounded; invalid, negative or overflowing
durations are rejected. Missing fields remain missing. Offsets are relative to
the provider session's audio and may reset on reconnection. This compatibility
with the [official SDK schema](https://github.com/googleapis/python-genai/blob/main/google/genai/types.py#L2051)
does not enable diarization in current models.

`provider::capabilities(model)` describes translation models, advertising only
confirmed capabilities for known families. Gemini Live Translate advertises
continuous translation and best-effort automatic voice preservation; 3.8 Live
models support interpreting prompts. OpenAI `gpt-realtime-translate` advertises
continuous translation and `gpt-realtime-2.1` supports interpreting prompts,
including snapshots of those families with valid date suffixes. The catalog
does not attribute automatic voice preservation to OpenAI. Babel uses native
default voices for all of them. STT recognizers are separate flows: Deepgram
can supply actual speaker labels and word timing even if the translator cannot.
Unknown families and unrecognized suffixes advertise no assumed capabilities.

App timestamps derived from the local clock indicate when text arrived, including
network/AI delay. They are not individual word start times. Live documentation
mentions utterance timing, but the public WebSocket reference does not specify
those offsets for Live Translate; receiving such metadata is not guaranteed.

See [native translation voices](voices.md) for output behavior and migration
of legacy voice settings. Transcription speaker labels do not choose or change
translation voices.

## Gemini limits and recovery

The reconnect/playback rules below describe speech translation. Dedicated STT
uses the original-audio recovery path documented above.

- WebSocket messages: up to 512 KiB; output PCM fragment: up to 48,000 bytes
  (one second); input: up to 16,000 samples per send. MIME, rate, channels,
  Base64 and even PCM16 byte length are validated.
- Audio sends have a 500 ms deadline; playback events have two seconds. Queues
  and network buffers are bounded. Prolonged congestion ends the session
  instead of consuming unbounded memory.
- Ping every 15 seconds; a connection with no response for 45 seconds is
  reopened. One second without new frames sends `audioStreamEnd` without
  closing the connection.
- Configured retries limit consecutive failures; a healthy session of at least
  one minute restores the budget. Backoff grows from 250 ms to five seconds.
  Configuration, authentication and protocol errors do not retry indefinitely.
- Reconnection interrupts pending playback and discards audio captured during
  unavailability. Lossless continuity during network failures/session rotation
  is not promised.
- Generic Live uses `sessionResumptionUpdate` and `goAway` to resume from an
  explicitly resumable point. The client enables context compression. The
  translation model recreates sessions without those options, whose model-specific
  compatibility is undocumented.
- Cancellation interrupts connections, setup waits, networking and queues.
  Unexpected capture closure is an error, not successful completion.
- A remote close reports its numeric code and stage (setup acknowledgement,
  live streaming or historical transcription), without the remote reason.
  Codes 1002, 1003 and 1007 distinguish protocol, message type and payload
  rejection; 1009 means the server's message limit was exceeded. Code 1008
  indicates a policy rejection that may involve settings or account access.
  Codes 1011, 1012 and 1013 use the bounded recovery policy for server failures,
  restarts and overload. A close code alone does not identify the rejected field.

## Use without translation and validation

Turn off translation for a direction to forward its original audio. This works
without a session: the microphone activates when Babel is the system default or
an app uses its virtual microphone; output requires an app sending audio to
Babel. It also remains available during recording sessions. There is no diagnostic
provider to select; routing/recording with transcription and translation off
neither open AI connections nor require a key. Independent transcription uses
the `transcription.providers.*` profile selected for each source, even while
that direction translates. Gemini, OpenAI, Deepgram and Whisper are described
in the [transcription guide](transcription.md).

Tests use a local WebSocket server and dummy credentials: setup barrier,
little-endian PCM, multiple fragments, transcripts, cancellation, session
resumption, retry budgets, sanitized errors and capture EOF. They do not prove
account authorization or actual translation quality; those require a valid key
and real audio.

For an opt-in connection check, set `GEMINI_API_KEY` in your shell and run
`cargo run --locked --example gemini_connection_smoke`. It opens the official
service using the default translator and `en-US`, sends synthetic silence,
pauses input and checks that the connection survives the heartbeat window.
It never opens audio devices or reads your configuration. This check may incur
provider usage; it verifies connection/protocol behavior, not translation quality.

## Official references

Documentation consulted on 2026-09-29:

- [Live Translation](https://ai.google.dev/gemini-api/docs/live-api/live-translate):
  continuous mode, specialized model and configuration.
- [WebSocket reference](https://ai.google.dev/api/live): messages, transcription,
  setup and resumption.
- [Live capabilities](https://ai.google.dev/gemini-api/docs/live-api/capabilities):
  PCM formats and 3.8 Live model capabilities.
- [Official SDK headers](https://github.com/googleapis/python-genai/blob/main/google/genai/_api_client.py)
  and [Live connection](https://github.com/googleapis/python-genai/blob/main/google/genai/live.py):
  header authentication.
