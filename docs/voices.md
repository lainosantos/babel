# Native translation voices

Both translation directions use the selected model's native default voice.
The microphone and incoming audio can use different translators and target
languages, but neither route has a voice selector. Configure them on
**Translation**. Babel does not add a second cloud synthesis stage, create a
voice library, design voices, or enroll voice clones.

## Cloud translation

Gemini Live and OpenAI Realtime produce the audio played by Babel. Requests omit
fixed voice overrides: Gemini sends no `speechConfig`, and OpenAI sends no
`audio.output.voice`. The service chooses the model's native default behavior.

Gemini Live Translate can approximate characteristics of the original speaker.
This is model behavior, not a saved voice profile or a guarantee that every
participant retains a distinct identity. Voices can change after pauses or rapid
speaker changes. Other models have different behavior; Babel does not promise
identity preservation for OpenAI. Speaker labels from transcription do not
select translation voices.

See the [Gemini provider guide](providers.md),
[OpenAI provider guide](other-providers.md),
[Gemini Live capabilities](https://ai.google.dev/gemini-api/docs/live-api/capabilities)
and [OpenAI Realtime conversations](https://developers.openai.com/api/docs/guides/realtime-conversations).

## Local translation

The local translator runs Whisper → Qwen → Piper. Embedded Piper automatically
uses Babel's catalog default for the target language. Voice selection is internal;
there is no route or local-profile override. The catalog and supported target
languages are listed in [embedded local models](local-inference.md).

An external Piper service receives text without a voice override and must supply
its own default voice compatible with the target language. Babel does not manage
that service's model selection. Local translation is segmented and does not
preserve the original speaker's identity.

## Original audio and transcription

Turning translation off passes through the original audio. Recording and
transcription remain independent: they receive the selected originals, before
translation, and work without translated speech. Original transcripts never use
translated text as their source. See [recording](recording.md) and
[transcription](transcription.md).

## Legacy configuration migration

Older configuration files may contain settings from the removed voice controls.
Babel accepts these fields for migration, ignores their values, and omits them
from saved JSON and TOML:

- The route tables `[microphone.voice]` and `[speaker.voice]`.
- Translation-provider `voice` and `tts_model` fields.
- The `[providers.elevenlabs]` profile.
- The local-profile field `providers.local.piper_voice`.

Successful configuration loading saves the migrated settings automatically.
Translations then use the native defaults described above. This migration does
not call a voice service, delete remote account profiles, move session files, or
change original recording and transcription selections. Removed Gemini TTS and
ElevenLabs synthesis settings no longer enable audio processing or require keys.
