# OpenAI, Deepgram and local inference

Each route selects its translation provider independently. For example, the
microphone can use OpenAI while incoming audio uses the local pipeline.
Native voice defaults are described in [voices.md](voices.md); selectable
devices by OS are in [platforms.md](platforms.md).
Transcription has independent per-source provider/language selections and its
own `transcription.providers.*` profiles. It receives original audio even during
simultaneous translation and does not save STS input text. See
[Transcription: Gemini, OpenAI, Deepgram and Whisper](transcription.md).

## OpenAI: continuous translation or conversational models

The OpenAI profile defaults to `gpt-realtime-translate`. This dedicated
continuous-translation model receives audio while producing translated speech
and text. Its `/v1/realtime/translations` API differs from the conversational
API. For free-form instructions, choose `gpt-realtime-2.1`,
which operates through end-of-speech detection and responses. This distinction
comes from the [official translation documentation](https://developers.openai.com/api/docs/guides/realtime-translation).

In the dashboard, choose OpenAI for the route, set languages and configure its
session credential. You can also supply `OPENAI_API_KEY` to the process.
Use a project key with access to the model.

Merge this excerpt into your existing TOML:

```toml
[providers.openai]
api_key_env = "OPENAI_API_KEY"
model = "gpt-realtime-translate"
endpoint = ""
connect_timeout_secs = 15
max_reconnect_attempts = 5

[microphone]
provider = "openai"
source_language = "pt-BR"
target_language = "en"
prompt = ""

[speaker]
provider = "openai"
source_language = "en"
target_language = "pt"
prompt = ""
```

Preserve your device fields: the excerpts show only provider/language changes.
`endpoint = ""` chooses the correct official URL for the model. An explicit
endpoint retains exactly the selected host/path, with the model appended by
the adapter. It requires `wss`, except `ws` on loopback; URL credentials,
query strings and fragments are rejected. The key goes to the configured
endpoint in a Bearer header. An alternative server must implement the same GA
protocol; this field does not make arbitrary APIs compatible providers.

The dedicated model's session contract does not support custom prompts or native
voice selection. Babel also leaves the conversational model's voice at its
native default. Neither adapter promises vocal-identity preservation or
participant separation. The dedicated session supports output language and input
transcription under the [translation event contract](https://developers.openai.com/api/reference/resources/realtime/translation-client-events).
The source-language field does not force a language in that session; translation
uses received audio. In the conversational model, it becomes part of the instructions.

For translation with custom instructions:

```toml
[providers.openai]
model = "gpt-realtime-2.1"
endpoint = ""

[microphone]
provider = "openai"
source_language = "pt-BR"
target_language = "en-US"
prompt = "Preserve technical software development terms."
```

The adapter configures PCM16 mono at 24 kHz in both directions of the connection.
Babel's internal speech capture arrives at 16 kHz and is converted with a sinc
filter; this does not recover frequencies absent from the original signal.
The conversational version uses the GA `session.audio.input/output` contract.
Babel detects speech boundaries locally with an RMS threshold of 0.01, the
configured VAD silence duration and a maximum turn length of ten seconds. It
sends one explicit audio commit and response request at a time, awaiting that
response's completion before submitting the next turn. This preserves speech
order and prevents a previous response from acknowledging newer input. Speech
captured while a response runs remains queued, so a slow model increases delay.
The previous translation is not interrupted by new speech. See [Realtime conversations](https://developers.openai.com/api/docs/guides/realtime-conversations)
and [WebSockets](https://developers.openai.com/api/docs/guides/voice-websockets?voice-api=realtime).

The translation session does not request original recognition to generate the
TXT. Its native audio is played directly.
To save original speech, choose/configure STT on **Transcription**, independently
of the OpenAI translation model. Legacy `providers.*.transcription_model` fields
do not replace `transcription.providers.*.model` after migration. See the
[transcription configuration and migration guide](transcription.md).

The connection retains input during setup and releases it after confirmation.
Setup failures have bounded retries. Once translation
has consumed audio, a connection failure triggers automatic recovery from retained
originals. Translation resumes recent speech by default; explicitly opting in to
accelerated backlog replay preserves every pending window. See the
[recovery policy](recording.md#recover-an-incomplete-session). Reconnecting an empty
stream cannot claim earlier speech was processed.

Stop ends capture and closes the provider's input after queued audio drains.
For continuous translation, Babel flushes the resampler tail, sends
`session.close`, and receives remaining audio and text through `session.closed`.
For conversational translation, it commits the final partial utterance and waits
for its matching completed response and any requested input transcript. Playback
and processing can continue after capture stops. A missing final acknowledgement
or failed request reports an incomplete session and preserves retained originals.
Messages and active queues are bounded; output delivery waits for playback or
text-storage capacity without a consumer-delivery deadline. Network, request
and final-acknowledgement deadlines still apply. Older source audio remains
available through the session's encrypted journal. Remote errors expose neither keys nor
raw response content.
Actual model availability requires validation with your own account; the
[official model details](https://developers.openai.com/api/docs/models/gpt-realtime-translate)
do not guarantee access for every account.

## OpenAI: independent transcription

On **Transcription**, choose OpenAI for one or both sources. Use
`transcription.providers.openai`, whose default model is `gpt-live-transcribe`.
The adapter also accepts compatible `gpt-transcribe` and `gpt-realtime-whisper`
families, including validated dated snapshots. Key, model and endpoint do not
come from the translation profile. Original audio may continue forwarding or
be translated by another provider; STT produces original text only.

```toml
[transcription.microphone_recognition]
provider = "openai"
language = "pt-BR"

[transcription.providers.openai]
api_key_env = "OPENAI_STT_API_KEY"
model = "gpt-live-transcribe"
endpoint = ""
connect_timeout_secs = 15
max_reconnect_attempts = 5
```

Enable `transcription.enabled` and select the sources to save.
`transcription.speaker_recognition` configures incoming audio separately;
STT profiles may optionally share a key without sharing settings with translation.

An empty endpoint selects `/realtime?intent=transcription`. An explicit endpoint
ending in `/translations` is rejected in this mode. The adapter uses
`type=transcription` and PCM16 mono at 24 kHz, converted from AI-bound capture.
The source language is sent under the model's contract; target language, voice
and translation prompt are omitted. See the
[official Realtime transcription documentation](https://developers.openai.com/api/docs/guides/realtime-transcription).

The STT adapter detects pauses locally with an RMS threshold of 0.01 and 400 ms
silence, and also closes segments at ten seconds. It records only final results,
associated with commits by `item_id`, with at most 64 pending reorder items.
Per-direction ordering does not synchronize speech between connections. The
default model provides neither diarization nor word timing.

Stopping the session closes capture and flushes pending recognition, including
the final unfinished sentence. The provider waits for every submitted commit's
final transcript, and received results drain to TXT. A timeout or interrupted
result leaves the session incomplete with originals retained for recovery.
Enable WAV recording separately when you also want a permanent original-audio
file; temporary encrypted retention is independent of that setting.

## Deepgram: continuous original-audio transcription

Select `deepgram` for each desired source's recognition and configure
`transcription.providers.deepgram`. The default is Nova-3 on
`wss://api.deepgram.com/v1/listen`; the key defaults to `DEEPGRAM_API_KEY` and
travels in `Authorization: Token …`, never the URL.

Input is PCM16 mono at 16 kHz. The adapter records final results, not partial
hypotheses, and produces no translation or voice. `diarize` enables the v1
streaming diarizer (`diarize_model=v1`); consecutive words with the same speaker
ID are grouped, preserving Deepgram punctuation and timing. IDs are
connection-local labels, not names or persistent personal identities.
`punctuate` controls punctuation.

`language = "auto"` selects `multi` for general Nova-2/Nova-3 models. This covers
the model's multilingual set, not every individually supported language. Flux
uses another protocol and is not accepted by this adapter. Reconnection has a
bounded budget and may discard connection-local queued audio; speaker
labels/offsets may reset. Babel marks such an interruption incomplete and keeps
the session's originals for recovery instead of treating later results as a
complete transcript. Stop sends the remaining original PCM and `CloseStream`,
then waits for final results and terminal metadata before completing.
Keepalive maintains silent connections without inventing audio or advancing
timestamps. See [complete configuration](transcription.md),
[Listen v1](https://developers.deepgram.com/reference/speech-to-text/listen-streaming),
[diarization](https://developers.deepgram.com/docs/diarization) and
[multilingual mode](https://developers.deepgram.com/docs/multilingual-code-switching).

## Embedded local provider: Whisper → Qwen → Piper

Choose **Local** for translation and retain **Built into Babel** for recognition,
translation and voice components. Saving downloads/verifies missing models and
keeps their files on disk. Packaged engines load when a session uses enabled
features. The same flow works on Linux, macOS and Windows without separately
installing Python, CMake, Ollama or Piper. The dashboard tracks preparation/downloads.

The chain recognizes PCM16 mono/16 kHz WAV with Whisper, translates text with
Qwen through llama.cpp, and synthesizes with Piper. Babel converts the resulting
WAV to 24 kHz and delivers 20 ms frames. Existing endpoint field names remain
for compatibility; `ollama_endpoint = "auto"` starts llama.cpp and does not
require an Ollama service.

```toml
[providers.local]
whisper_endpoint = "auto"
whisper_model = "base-q5_1"
ollama_endpoint = "auto"
translation_model = "qwen3-0.6b"
piper_endpoint = "auto"
segment_ms = 2000
silence_ms = 300
vad_threshold = 0.01
request_timeout_secs = 30

[local_runtime]
directory = ""
threads = 2
idle_unload_secs = 60

[microphone]
provider = "local"
source_language = "pt-BR"
target_language = "en-US"
prompt = "Preserve proper names and technical terms."
```

Preserve existing devices and other fields when merging. Embedded Piper always
uses the catalog default for the target language; an external Piper service
uses its own default. Babel sends no voice override. The chain does not preserve
vocal identity or perform diarization. Intermediate recognition text does not feed the TXT:
choose independent STT on **Transcription**. STT Whisper and translation Whisper
have their own configurations.

Whisper Base Q5_1 is the new-configuration default, with 59.7 MB weights.
Tiny Q5_1 uses 32.2 MB; Small Q5_1 uses 190.1 MB. Original variants remain
selectable and saved choices are preserved. Qwen3 0.6B remains Q8 at 639 MB;
each medium Piper voice uses about 63–64 MB. These are download sizes, not RAM.
See limits and the point-in-time measurement in the [embedded catalog](local-inference.md).

Recognition is segmented; latency accumulates across recognition, translation
and synthesis. Small models may make more errors with ambiguous phrases,
underrepresented languages or technical context. Queues are bounded, and the
machine must keep up with audio; universal real-time CPU performance is not promised.

### Storage, voices and external servers

The [local-model guide](local-inference.md) describes the catalog, optional
absolute directory, threads, first preparation and offline operation. Initial
selection needs internet for weights. Engines are part of the installer;
a development build must generate the runtime package before embedded mode works.

The new default is up to two Whisper/Qwen threads according to available CPUs.
The configurable range is 1–64, further capped by the processing CPU budget.
Piper retains internal scheduling, with known compute-library limits supplied
to managed children. `idle_unload_secs` accepts 1–3600 seconds, default 60:
after the last session releases engines, this grace period permits reuse before
managed processes stop and release RAM. Weights remain cached on disk. Saving
a provider for a disabled feature prepares files but does not make a
recording-only session load AI.

Components can individually use **External server (advanced)**. Supply each
service's actual endpoint; Babel does not discover services by default ports.
Whisper accepts multipart WAV at `/inference`. Translation uses Ollama's chat
API for `translation_api = "ollama"` and compatible chat completions for
`"openai"`. External Piper must accept text and return WAV using its default
voice. Remote HTTPS endpoints receive the audio or text for their stage. Babel's idle policy does
not terminate external servers.

The Ollama API receives translation messages, `stream=false`, `think=false`,
zero temperature and a token limit. The server model must support that contract;
truncated output is not sent to synthesis. Consult the
[Ollama API](https://docs.ollama.com/api/chat),
[Whisper server](https://github.com/ggml-org/whisper.cpp/tree/master/examples/server)
and [Piper HTTP](https://github.com/OHF-Voice/piper1-gpl/blob/main/docs/API_HTTP.md)
for self-managed services.

### Performance and limits

`segment_ms` accepts 500–10000 ms; `silence_ms` accepts 100–2000 ms and must be
shorter than the segment. RMS `vad_threshold` ranges from 0.0001 to 0.5.
Increasing it may reject noise and also miss quiet speech. A 100 ms pre-roll
reduces clipped word beginnings. Offsets refer to segments, not word alignment.

Capture and inference progress in independent tasks, with at most two segments
waiting for inference and two audio segments waiting for playback per pipeline.
Two routes plus independent transcription increase CPU/memory load. When
inference falls behind, both local recognition and translation wait for queue
capacity and preserve every accepted speech segment in order. Recent originals
are available from RAM while encrypted journal writes proceed independently;
older retained originals can be read back from disk. Disk synchronization is
not on the live input path to inference. Backlog increases processing delay
instead of dropping older speech.

Stop ends capture, flushes the final partial segment, and lets queued recognition,
translation, synthesis and playback finish. Original audio routing remains
isolated from this processing. Service or request failures report an incomplete
session and retain its originals for recovery. Calls have timeouts and byte
limits and do not follow redirects. Ambiguous requests are not automatically
repeated, avoiding duplicate speech.

Babel's Rust core forbids its own `unsafe`. Inference engines use separate native
libraries with their own safety properties; integration does not make those
libraries memory-safe. Engine and weight licenses are independent and accompany
the package's distribution/documentation.

### Available validation

```sh
cargo test --lib provider::openai
cargo test --lib provider::deepgram
cargo test --lib provider::local
```

Tests use a real local WebSocket and HTTP servers as mocks: setup confirmation
before audio, both OpenAI protocols, PCM, text, alignment, multipart WAV,
translation/synthesis chaining, bounds, cancellation and saturation. Deepgram
mocks additionally cover authentication, PCM frames, finals, diarization,
timestamps, keepalive, old-queue disposal and reconnection. These tests do not
measure model quality, send voice to the cloud or use real keys. Evaluate your
audio/languages after configuring credentials or preparing local models.
