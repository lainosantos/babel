# Voice library and custom synthesis

Babel offers two output choices. The direct path uses audio translated by the
speech provider. The custom-voice path receives translated text internally and
synthesizes it with the selected voice. Transcript files still contain only
original speech; translated text used for synthesis must not be saved as an
original transcript.

Custom synthesis adds an AI step, requests and cost. Synthesized audio streams,
but needs some text before it can begin. A voice library does not make this path
equivalent to direct continuous translation. You can choose a different voice
for each direction; that identifies the direction, not each participant in a
call with mixed audio.

## Implemented features

| Feature | Gemini | ElevenLabs |
|---|---|---|
| List account library | Preset voices and custom profiles | Voices available to the key |
| Create a voice from a description | Persistent `prompted` profile | Generates previews and saves the first returned |
| Clone a user-uploaded reference | Reference and separate consent recording | Instant Voice Cloning with a reference |
| Synthesize using a selected voice | Gemini 3.8 Flash TTS or Flash-Lite TTS | Configured TTS model, such as `eleven_flash_v2_5` |
| Per-synthesis text style prompt | Yes, through `speech_metadata.style` | Not in this adapter; the field must be empty |
| Identify several people in the same audio | Not provided by the library | Not provided by the library |
| Automatically enroll a voice from its first seconds | Not implemented | Not implemented |

The client uses the ID actually returned by the API. It does not silently replace
the selected voice with a default. An enrollment response does not guarantee the
provider has released the voice for synthesis: ElevenLabs can return
`verification_required`.

## Configure credentials

1. Create a provider key and confirm access to the models and library operations.
2. Configure the key in Babel's credential panel or the environment of the
   process that launches the app.
3. Use separate names, such as `GEMINI_API_KEY` and `ELEVENLABS_API_KEY`.
   Profiles store the credential name, not the secret.
4. Choose the library provider and load voices. A Gemini voice cannot serve as
   an ElevenLabs ID, or vice versa.
5. Apply the profile to the desired route and restart translation to use it.

In Bash, enter a key without putting its literal value in shell history:

```bash
read -rsp 'Gemini API key: ' GEMINI_API_KEY
export GEMINI_API_KEY
```

The same pattern works with `ELEVENLABS_API_KEY`. On Windows, set the variable
for the process that opens Babel or use the dashboard. Dashboard keys live only
in process memory and take precedence over the environment. Restarting the app
removes the in-memory value. Configuration files do not persist keys.

Cloud connections use fixed official endpoints and TLS. HTTP redirects are
rejected. Babel errors report the HTTP code without response bodies or headers
containing secrets. The key is marked sensitive and the resolver's copy uses
`Zeroizing`; this does not guarantee erasure of all internal HTTP/TLS copies
or the process environment.

## Create a voice from a description

Choose a recognizable profile name and describe stable traits: timbre, vocal
range, accent and delivery style. For example: “adult, warm voice, clear
articulation and a neutral Brazilian accent.” Use the desired language/code,
such as `pt-BR`.

Gemini enrollment uses `gemini-3.8-flash-tts`, `type=prompted` and profile
storage enabled. The returned ID is reusable for synthesis. A per-utterance
style prompt controls delivery without redefining the profile.
[Voice design](https://ai.google.dev/gemini-api/docs/voice-design).

ElevenLabs uses two calls: generate previews, then save a generated ID. This
version of Babel saves the first returned preview, as stated in the feature
table. Description plus language must contain 20–1000 characters. Preview audio
is not downloaded: `stream_previews=true` requests IDs only, reducing the
response. This interface has no step for auditioning/comparing every preview.
[Design](https://elevenlabs.io/docs/api-reference/text-to-voice/design),
[save profile](https://elevenlabs.io/docs/api-reference/text-to-voice/create).

Creating a profile is an explicit external action that may incur charges and
consume voice quota. Babel does not automatically retry creation after network
errors, avoiding duplicate profiles. If a request times out after sending,
reload the library before retrying: it may already have completed remotely.

## Clone a voice from a reference

Uploads accept **RIFF WAV, uncompressed PCM16, mono, 8–48 kHz**. Renaming `.mp3`
to `.wav` is insufficient. The client checks headers, alignment, size and
duration. Use clean recordings without music or overlapping speakers.

For Gemini, prepare:

- A **10–30-second** reference of the person.
- Another WAV containing the provider's required consent sentence, spoken by
  the same person. Babel accepts 1–60 seconds for this file.
- Documentation recommends 24 kHz and similar acoustic conditions. The API
  verifies consent; local format validation does not replace that verification.

Record the exact consent sentence prescribed by Google's voice-replication
documentation for the recording language. Follow the provider's wording rather
than translating or substituting a consent statement yourself.

Persistent Gemini profiles have account/provider-defined limits and retention;
consult documentation before relying on an ID permanently.
[Replication requirements](https://ai.google.dev/gemini-api/docs/voice-replication).

For ElevenLabs, select the reference WAV. The adapter accepts 1–300 seconds,
subject to upload limits and service requirements. It uses Instant Voice
Cloning, not Professional Voice Cloning training. The separate consent file is
specific to Gemini and is not sent in the ElevenLabs call. This does not remove
account authorization, verification or access requirements.
[Instant Voice Cloning](https://elevenlabs.io/docs/api-reference/voices/ivc/create).

The dashboard form accepts up to **2 MiB per WAV file**, both reference and
consent. The local API limits the complete JSON request to **8 MiB**. Base64
adds roughly one third to the size; Gemini's two files share that budget. The
module also has an internal 16 MiB-per-decoded-file limit, but the smaller
dashboard limits prevail. Mono 24 kHz WAV saves space and follows Gemini's
recommendation.

Babel does not secretly capture participant samples or automatically associate
cloned profiles with people in mixed audio. Profiles come from explicitly
uploaded files and are selected per route.

## Selection and playback

For custom voices, supply matching synthesis provider, model, credential and
voice ID. This integration accepts these Gemini models:

```text
gemini-3.8-flash-tts
gemini-3.8-flash-lite-tts
```

For ElevenLabs, a low-latency model such as `eleven_flash_v2_5` is configurable.
Voice/model availability depends on the account. `pt-BR` maps to the TTS
endpoint's `pt` code; `eleven_multilingual_v2` detects language from text because
it does not accept a language parameter. The free-form style field must remain
empty for ElevenLabs.
[Streaming TTS](https://elevenlabs.io/docs/api-reference/text-to-speech/stream).

The module emits little-endian PCM16, mono, 24 kHz in frames of up to 480 samples,
without downloading an entire utterance first. The engine paces playback by
actual sample count and preserves that pace across text fragments.
`voice.chunk_ms` bounds waiting from the first unsynthesized fragment, even
while speech continues. Utterances are limited to 240 characters and the queue
to four segments; if synthesis falls behind its configured budget or the queue
fills, the flow ends with an explicit error. MIME, rate when supplied, Base64
and byte-pair continuity are checked. Audio with WAV/MP3/Ogg/FLAC headers is not
mistaken for raw PCM. Gemini must signal SSE completion; premature closure is
an error. [Gemini formats and streaming](https://ai.google.dev/gemini-api/docs/speech-generation#streaming-speech-generation).

## Endpoints

| Provider | Operation | Endpoint |
|---|---|---|
| Gemini | List/create profiles | `GET/POST https://generativelanguage.googleapis.com/v1beta/voices` |
| Gemini | SSE synthesis | `POST https://generativelanguage.googleapis.com/v1beta/interactions` |
| ElevenLabs | List profiles | `GET https://api.elevenlabs.io/v2/voices` |
| ElevenLabs | Generate design | `POST https://api.elevenlabs.io/v1/text-to-voice/design` |
| ElevenLabs | Save design | `POST https://api.elevenlabs.io/v1/text-to-voice` |
| ElevenLabs | Clone reference | `POST https://api.elevenlabs.io/v1/voices/add` |
| ElevenLabs | PCM synthesis | `POST https://api.elevenlabs.io/v1/text-to-speech/{voice_id}/stream?output_format=pcm_24000` |

Public code operations are `voices::list`, `voices::design`,
`voices::clone_voice` and `voices::synthesize`. The dashboard exposes the library,
design and cloning through authenticated routes; use the interface to benefit
from the app's tokens and origin protection.

Example design request to the local backend, with no plaintext key:

```json
{
  "provider": "gemini",
  "api_key_env": "GEMINI_API_KEY",
  "name": "Warm Brazilian Portuguese",
  "description": "Adult, warm voice, clear articulation and a neutral Brazilian accent.",
  "language": "pt-BR"
}
```

Cloning fields are `provider`, `api_key_env`, `name`, `reference_base64` and
`consent_base64` (required for Gemini). Send only the WAV's Base64 without a
`data:` prefix. The common response contains `id`, `name`, `provider` and `kind`.

## Costs, privacy and operational limits

Library access, enrollment and synthesis use the selected provider account and
quotas. Prices vary; Babel does not estimate charges or promise a free allowance.
If translation and synthesis use different providers, original audio goes to
the translator and translated text to the synthesizer. Voice references/consent
go to the enrollment service when the user creates a profile. Profiles may
persist in the cloud; removing a local selection does not delete remote profiles.

The synthesis client requests `store=false` for Gemini interactions; profile
enrollment uses `store=true`. Retention, logs and account policies remain
provider-defined. These parameters do not imply anonymization or a general
zero-retention guarantee.

Local stability limits are: five-second HTTP connection, up to 60 seconds per
request, JSON response up to 8 MiB, SSE event around 1 MiB, and up to 16 MiB
total audio per synthesis. An audio queue blocked for two seconds ends the
request. Libraries are paginated with limits of 100 pages and 10,000 profiles;
exceeding either produces an explicit error rather than presenting a truncated
list as complete.

## Troubleshooting

| Symptom | Action |
|---|---|
| Missing credential | Set the key in the dashboard or Babel's launch environment; check the profile's variable name. |
| HTTP 401/403 | Check the key, permissions, model, region and voice-operation access. |
| HTTP 429 | Check account quotas/limits; reduce sessions or request frequency. |
| HTTP 400/422 | Check model, ID, language, description and file formats; the API may impose additional conditions. |
| Profile requires verification | Complete the provider's process before selecting it for synthesis. |
| Invalid WAV | Export uncompressed mono PCM16 with a consistent RIFF header. |
| Synthesis queue blocked | Reduce load and segment duration; check that the physical output consumes audio. |
| Voice missing after enrollment timeout | Reload the library; do not immediately repeat creation. |
| Quality/voice varies across participants | The route receives mixed audio; selecting a profile does not perform diarization. |

Automated tests use dummy credentials and local HTTP servers. They validate
pagination, two-step design, multipart upload, Gemini consent, SSE, PCM and error
sanitization. They do not prove account access or voice fidelity; those require
a real call explicitly initiated with your key.
