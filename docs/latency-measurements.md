# Translation latency measurements

Measured on September 30, 2026, with Babel revision `d2a5410`, Linux and
PipeWire 1.6.2. These results describe this machine and these workloads, not
latency guarantees for other systems or cloud providers.

## Playback after translated audio becomes available

The optimized `audio_latency_probe` executable used Babel's production playback
and capture backends. It sent a 100 ms synthetic tone into an exclusively owned
null sink and detected the signal on that sink's monitor. Each trial started
after the monitor had settled to silence. Neither physical devices nor the
existing Babel endpoints were changed.

| Requested device latency | Playback queue capacity | Trials | Median | p95 | Minimum–maximum |
|---|---|---:|---:|---:|---:|
| 30 ms | 2,000 ms | 12 | 7.49 ms | 13.18 ms | 6.70–13.58 ms |
| 10 ms | 2,000 ms | 12 | 7.44 ms | 12.62 ms | 6.50–12.63 ms |
| 30 ms | 200 ms | 12 | 7.53 ms | 13.90 ms | 6.24–15.03 ms |

Times run from enqueueing the tone to receipt of the first monitor block
containing the signal. They include monitor capture, process scheduling and
the 10 ms observation frame. Percentiles use linear interpolation over the
sorted observations. The requested device latency is a backend request, not a
measured minimum delay.

All 36 trials completed without observation-capture loss. Owned temporary
endpoints were removed, and the system defaults and existing Babel endpoints
were verified unchanged.

**Finding:** the 2,000 ms queue capacity does not impose a two-second startup
delay. Neither reducing that capacity nor requesting 10 ms instead of 30 ms
produced a meaningful improvement in this small software-loopback experiment.
The differences are below the observation-frame duration.

This does **not** measure physical headphone/microphone latency, model response,
network travel, session setup, or accumulation of queued audio during a long
translation stream. In particular, it cannot establish which cloud provider is
fastest or attribute the reported translation delay to Gemini.

## Original audio through a complete Babel route

A second, isolated Babel instance ran three trials with translation,
transcription, recording, history and voice commands disabled. The dashboard
probe fed 6.304 seconds of locally synthesized Portuguese speech to its owned
input sink and observed both the input and output monitors. The normal user's
Babel instance and settings were not changed.

| Trial | Input onset to output onset | Input end to output end |
|---|---:|---:|
| 1 | 37.0 ms | 37.1 ms |
| 2 | 43.0 ms | 43.3 ms |
| 3 | 44.6 ms | 39.3 ms |

The onset median was **43.0 ms**. All three trials completed with zero reported
capture/processing drops, zero reconnections and zero translated samples.
Settings were restored and the temporary instance was closed.

These monitor-to-monitor observations include the original route and observation
clients. They are not acoustic measurements and do not include the additional
speech conversion and provider path used when translation is enabled. The small
sample supports a local baseline, not a general upper bound.

## Cloud translation comparison

The cloud experiment uses identical, locally synthesized Portuguese speech for
each provider and trial. It observes the original input monitor, the dashboard's
first increase in translated audio samples, and the translated output monitor.
The utterance is synthetic; user microphone audio and recordings are not used.

The Gemini comparison ran through the existing application with a temporary
API key held only in Babel's memory. No key was exported to the harness or its
audio child processes. The microphone route used only synthetic speech and
owned null sinks; the speaker route stayed isolated and silent. Transcription,
recording, history and voice commands were disabled for the trials. Original
configuration was restored after each model's run.

### Same-fixture Gemini comparison

Three trials per model used the same 6.304-second Portuguese fixture, English
target, empty operator prompt, `balanced` quality and a **5,000 ms playback
capacity**. The conversational models used Babel's interpreter instruction and
400 ms silence detection; the dedicated translation model used its native
translation configuration. These are the supported protocols, not an identical
prompt sent to incompatible APIs.

| Model | Input onset → first output sound, median (range) | Input onset → engine audio, median | Input end → output end, median |
|---|---:|---:|---:|
| `gemini-3.5-live-translate-preview` | **3.35 s** (3.33–3.44 s) | 3.33 s | 3.07 s |
| `gemini-3.8-live` | 7.49 s (7.33–7.71 s) | 7.49 s | 7.06 s |
| `gemini-3.1-flash-live-preview` | 7.29 s (7.28–7.48 s) | 7.24 s | 6.52 s |

All nine comparison trials reported zero capture drops, zero processing drops
and zero reconnections. Median status polling was 20.1 ms; the maximum observed
poll interval was 22.7 ms. Connection/readiness took approximately 0.69–0.76 s
and is reported separately: speech started after the connection was ready.
The audible input lasted approximately 6.17 s after excluding quiet edges.
After timing finished, a separate local Whisper base verifier inspected all
nine saved synthetic outputs with automatic language detection. It identified
English in every file and recovered the intended complete meaning: greeting,
scheduling a meeting for tomorrow morning, and discussing the project's
results. This automated content check rules out original-audio passthrough or
an unrelated first sound for these examples; it is not a general translation
quality or prosody evaluation. Verification did not run concurrently with the
latency trials.

The dedicated model began translating **while the original sentence was still
being spoken**. Both conversational models started after it ended. A model
optimized for quick conversational responses can therefore have a longer
simultaneous-translation onset than the dedicated translation model. This
experiment does not establish a meaningful ranking between the two
conversational models: there are only three trials and their ranges overlap.

The dedicated model was the fastest of these three for this sentence, but its
approximately 3.4-second onset did **not** deliver a subsecond experience. Most
of the observed onset interval elapsed before the engine received its first
audio event. This identifies the combined capture/submission/network/provider
path, not a model-internal inference measurement. It does not support reducing
the playback queue to eliminate a fixed local delay.

### Playback burst limitation at the normal capacity

At the existing **2,000 ms capacity**, Live Translate completed three trials
with onset times of 3.48, 3.30 and 3.47 s (median 3.47 s), without reported loss
or reconnection. Both conversational models failed at this capacity, twice
each, with Babel's translated-playback queue-full error after connecting.
These failed trials are excluded from the latency comparison.

The current route splits provider output into 20 ms frames and stops on the
first full playback queue. A conversational model can generate speech faster
than it plays, producing a normal burst that exceeds this limit. Increasing
capacity temporarily to 5,000 ms accommodated this fixture and allowed the
comparison; it is **not a permanent fix or a guarantee for longer utterances**.
The same capacity was applied to all three models. All user settings, including
the original 2,000 ms capacity, were restored afterward. No production audio
behavior was changed by these benchmark additions.

### What “Lite” supports

The official [Gemini 3.5 Flash-Lite model documentation](https://ai.google.dev/gemini-api/docs/models/gemini-3.5-flash-lite)
lists text output, no audio generation and no Live API support. The
[Gemini 3.8 Flash-Lite TTS model documentation](https://ai.google.dev/gemini-api/docs/models/gemini-3.8-flash-lite-tts)
describes low-latency **text-to-speech**, with text input and audio output,
also without Live API support. Neither is a drop-in speech-to-speech model for
Babel's Live WebSocket adapter.

Using Lite TTS requires a separate text-and-speech pipeline. Flash-Lite can
recognize and translate an audio segment directly into text, followed by a TTS
request; a distinct STT service is not mandatory. The next experiment measures
that option. Neither Lite model was sent an incompatible Live request. OpenAI
models were not tested because their key was not configured in the running
instance.

### Audio → Flash-Lite text → Flash-Lite TTS

A synthetic-only local benchmark bridge received the same fixture through
Babel's configurable Realtime transport. It sent each audio segment to
`gemini-3.5-flash-lite` with minimal thinking, collected the complete English
translation, then streamed `gemini-3.8-flash-lite-tts` output using the standard
Kore voice. The transport profile was named `openai`, but **both inference
stages used Google APIs**; no request went to OpenAI. This bridge is a test
utility, not a new installed provider.

The authenticated handoff supplied the existing temporary Gemini key to the
local bridge in memory. Fixed official Google endpoints, disabled redirects,
a dynamic loopback port and a private random path bounded the experiment.
HTTP preparation completed before the session-ready event; the same client
was reused with a 120-second keepalive. Setup took 0.49–0.65 s and is excluded
from onset, matching the Live benchmark's readiness boundary. Playback capacity
was 5,000 ms, with immediate first PCM followed by paced output.

| Segmentation | Trials | First output sound, median (range) | Translation request → complete first text, median (range) | TTS request → first PCM, median (range) |
|---|---:|---:|---:|---:|
| Whole 6.3 s utterance + 400 ms silence | 3 | **9.76 s** (9.30–18.64 s) | 2.23 s (1.80–11.11 s) | 0.75 s (0.67–0.79 s) |
| Independent segments capped at 2 s | 3 | **4.37 s** (4.16–5.19 s) | 1.39 s (1.35–2.31 s) | 0.72 s (0.64–0.74 s) |

The last column starts at the **TTS request**, after translated text exists;
it is not measured from the original audio or from the text-generation
request. Component medians do not necessarily add up to the onset median.
First PCM can contain leading silence: onset uses the output monitor's first
sound above −45 dBFS. The short-window cap includes a 100 ms onset prefix, so
its first segment closed approximately 1.89 s after detected speech began.

The 18.64-second whole-utterance result is retained in the range, not discarded:
the text stage alone took 11.11 s in that trial without a reported transport
error. This small sample demonstrates variability, not a provider latency
guarantee. Initial cold-HTTP smoke trials measured 9.77 s for a whole utterance
and 4.41 s for short segments; these are separate from the three-trial warmed
comparison.

All six warmed trials had zero capture/processing drops and zero reconnections.
Every captured segment completed (three whole utterances and twelve short
segments), with no queued or unfinished speech at disconnect. The bridge's
`client_disconnected`/1006 report describes the harness ending its WebSocket
after observation; it did not precede segment completion.

**Quality changes the conclusion.** Whole-utterance generated text preserved
the intended greeting, meeting time and project discussion. All three short
trials produced damaged fragments or unrelated suffixes, including `set`,
`exact` or `eto`. One middle segment was returned in Portuguese despite the
English target. These defects were present in the actual generated text, not
just in a later speech recognizer's interpretation. Successful transport and
complete requests do not mean a correct translation.

After timing finished, a separate local Whisper base verifier inspected all
six generated audio files. It recovered the intended meaning in the three
whole-utterance outputs and corroborated missing, mixed-language or malformed
fragments in the short-window outputs. This automated check is not a general
translation-quality evaluation; it ran after the timed trials so local
inference did not compete with the benchmark.

The short-window onset is therefore a **time-to-sound result, not a valid
replacement for accurate continuous translation**. Its median completion tail
was 13.52 s after the source ended, versus 9.67 s for the whole-utterance
cascade. The prototype serializes segment translation, synthesis and paced
playback; that choice contributes to its accumulating backlog. Overlapping
processing stages could reduce later waiting, but does not remove the initial
segment-capture and first text/TTS request costs measured here. These results
do not establish a lower bound for every possible cascade architecture.

For this fixture, the tested cascade did not improve on Live Translate's
3.35-second onset. The TTS stage was relatively short; capture, text generation
and segmentation quality were the main obstacles in this prototype. User
configuration was restored, temporary endpoints and the bridge were removed,
and the original Babel process and temporary key remained available.

### Measurement limits

The dashboard observation uses polling, so it bounds when Babel handled its
first provider audio event rather than recording the exact WebSocket arrival
time. Input-to-engine timing includes local capture and submission, network
travel and provider processing; these components cannot be separated by this
probe alone. Output monitors measure software delivery, not acoustic output.
Engine-to-output differences can be slightly negative within polling and
monitor scheduling error. The largest positive such difference in the
comparison was about 0.28 s; it can include model-generated leading silence,
not just local playback buffering.

The Live comparison inferred completion after three seconds below the
monitor's −45 dBFS threshold. The cascade used eight seconds and additionally
confirmed that all 15 captured segments completed. Live Translate continues
emitting silent PCM after speech, so its last engine sample event is not a
speech-end timestamp. A later phrase after a longer pause could fall outside
observation. First sound does not by itself
prove accurate or complete translation, and one short sentence does not
measure sustained semantic lag or long-session stability.

See the [testing guide](testing.md#translation-playback-latency-without-a-model-or-network)
for the reproducible local playback experiment.
