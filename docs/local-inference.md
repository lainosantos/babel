# Embedded local models

Babel manages local models and loads engines when an enabled session feature
needs them. Installers include inference engines for Linux, macOS and Windows.
Users do not need Python, Ollama, CMake, a compiler or a separate server.
Model weights download automatically on first selection and remain cached for
subsequent runs.

## Start from the interface

For transcription, open **Transcription**, choose **whisper.cpp** for one or
both sources and keep **Built into Babel** in its profile. New configurations
default to Whisper Base Q5_1. Saving downloads/verifies missing weights without
keeping the recognizer loaded. Enabling transcription remains a separate choice.

For translation, open **Translation**, choose **Local** for the desired
route and keep recognition, translation and voice components **Built into
Babel**. Saving prepares the selected Whisper and Qwen models plus the default
Piper voice for each target language. Voice selection is internal and automatic.

State appears on both pages and under **Settings → Local models**:

- **Idle:** no active preparation for the saved selection yet.
- **Preparing:** downloading weights or loading engines when a session starts;
  downloads show received size and percentage when the server supplies a total.
- **Weights cached (`cached`):** files are verified on disk; inference engines
  need not occupy RAM.
- **Ready (`ready`):** engines required by the session have loaded.
- **Needs attention:** preparation failed; details help diagnose networking,
  storage, model or installation issues. Saving again retries preparation.

Starting a session waits for models to become ready. **Cancel session start**
interrupts that wait without requiring original routing to stop. Requested
loading is canceled; downloaded files stay cached. The session loads only
engines required by enabled translation and/or transcription. A local provider
saved for a disabled feature does not make that session load AI. Original
routing and recording do not need these models.

After the last session releases engines, Babel retains them briefly so another
session can start without reloading. The default is 60 seconds; afterward it
terminates managed processes and releases their RAM while keeping weights on
disk. A new session within that interval cancels unloading. External services
remain controlled by whoever started them: Babel does not terminate them.

No session is needed to prepare models, but preparation does not automatically
record, transcribe or translate. Capture for these features follows session
options and the corresponding virtual device's use. With translation,
transcription and recording off, original audio follows the configured physical
route.

Voice commands have another lifecycle: Whisper must remain available while the
agent listens to an eligible Babel microphone, even without a session. Needle
loads only to interpret a command. See [voice commands](voice-commands.md) for
these components' CPU limits and idle unloading.

## Available models

| Stage | Embedded catalog | Default and considerations |
|---|---|---|
| Original recognition | Multilingual Whisper `tiny-q5_1`, `base-q5_1`, `small-q5_1`; original `tiny`, `base`, `small` variants remain available | `base-q5_1`; Tiny prioritizes cost, Small offers more capacity with higher resource use |
| Text translation | Qwen `qwen3-0.6b` | Included llama.cpp engine; compact model without a universal quality guarantee |
| Synthesis | Piper voices listed below | Catalog default for the target language |

Approximate download sizes in decimal MB (1 MB = 1,000,000 bytes):

| Model file | Download |
|---|---:|
| Whisper Tiny Q5_1 | 32.2 MB |
| Whisper Base Q5_1 — default | 59.7 MB |
| Whisper Small Q5_1 | 190.1 MB |
| Whisper Tiny | 78 MB |
| Whisper Base | 148 MB |
| Whisper Small | 488 MB |
| Qwen3 0.6B Q8 | 639 MB |
| Each Piper voice | 63–64 MB, plus configuration and license |

The interface shows progress in MiB (1 MiB = 1,048,576 bytes), so its numbers
differ from the table. Download size is not inference RAM usage. Local
translation with Whisper Base Q5_1, Qwen and two voices needs approximately
826 MB of weights; packaged engines and temporary files need additional space.
Already-present verified weights are reused without downloading each session.

Q5_1 reduces weights' numerical precision to save space. Multilingual Base
remains the default's underlying model; it is not replaced by Tiny.
[whisper.cpp documentation](https://github.com/ggml-org/whisper.cpp#quantization)
describes reduced memory/disk use and notes that speed improvements depend on
hardware. Versions and SHA-256 values come from Babel's pinned catalog.
A configuration already using `tiny`, `base` or `small` keeps that choice;
select/save the compact version to adopt it.

The translator remains Qwen3 0.6B Q8 and Piper voices remain medium quality.
Reducing their weights further without evaluating languages/audio could hurt
results. The selection uses [multilingual Qwen with thinking disabled](https://huggingface.co/Qwen/Qwen3-0.6B#switching-between-thinking-and-non-thinking-mode)
for segment translation and one Piper voice per required language. This does
not guarantee fidelity for every language, accent or vocabulary.

| Target language | Embedded Piper voice |
|---|---|
| English | `en_US-lessac-medium` |
| Portuguese | `pt_BR-faber-medium` |
| Spanish | `es_ES-davefx-medium` |
| French | `fr_FR-siwis-medium` |
| German | `de_DE-thorsten-medium` |
| Italian | `it_IT-paola-medium` |
| Chinese | `zh_CN-huayan-medium` |

Babel chooses these catalog defaults automatically; routes and profiles have
no voice override. If the catalog does not cover your target language, configure
a compatible external Piper service whose default voice supports that language.
Babel sends text without a voice override to that service. Embedded Piper does
not preserve the original speaker's vocal identity.

Translation Whisper and STT Whisper have independent model/segmentation
settings. Two sources and simultaneous translation may share engines, but each
stream still needs processing, increasing load. The TXT contains only the
selected STT's results, not intermediate translation text.

## Storage and processing

In **Settings → Local models**, configure:

- **Model storage folder:** empty uses the `models` subfolder of Babel’s user configuration folder. To choose another
  disk/folder, enter an absolute path, such as `/home/user/Babel-models` on Linux,
  `/Users/user/Babel-models` on macOS or `D:\Babel-models` on Windows.
  `~` and environment variables are not expanded.
- **Inference CPU threads:** 1–64, default up to two depending on available
  CPUs, for Whisper and Qwen. The effective value is capped by the processing
  CPU budget. Piper retains its own internal scheduling; managed children also
  receive limits for known compute libraries. A larger value does not guarantee
  lower latency and can increase resource contention.
- **Release idle models after (seconds):** `idle_unload_secs`, 1–3600 seconds, default
  60. Starts after the last session releases engines; it does not interrupt
  inference still owned by a session. Lower values release RAM earlier but may
  require reloading when another session starts.

When the folder field is empty, weights default to:

| Babel host OS | Default weights folder |
|---|---|
| Linux | `$XDG_CONFIG_HOME/babel/models` when set to an absolute path; otherwise `$HOME/.config/babel/models` |
| macOS | `$HOME/Library/Application Support/Babel/models` |
| Windows | `%APPDATA%\Babel\models` |

These names describe the Babel process's environment variables. The UI field
does not expand them: use a complete absolute path for a custom folder. Weights
live alongside the user configuration, not in browser downloads. If no valid account directory
exists, an explicit absolute folder is required.

The model folder is independent of the TXT/WAV session-file base. Changing it
does not move existing files: a model missing at the new location must be
prepared again. Reserve space for all selected models; model/voice sizes vary.

```toml
[local_runtime]
directory = "" # User configuration folder/models, or an absolute path on the Babel host.
threads = 2
idle_unload_secs = 60

[transcription.providers.whisper]
endpoint = "auto"
model = "base-q5_1"
api_key_env = ""
segment_ms = 2000
silence_ms = 300
vad_threshold = 0.01
request_timeout_secs = 30

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
```

Initial preparation needs internet access to published weights. Downloads are
verified against Babel's integrity catalog before use. Failure must not be
mistaken for readiness. After required models are prepared, embedded inference
works offline. Selecting another uncached model needs preparation with network
access again.

## Ports, credentials and external mode

`endpoint = "auto"` and component endpoints set to `"auto"` mean managed
processes. Babel dynamically chooses available loopback ports; it does not
trust a service already present on a well-known port. Temporary addresses do
not replace `auto` in saved configuration. The embedded recognizer needs no
user API key.

**External server (advanced)** supports your own installations or another
machine. Enter the full URL, including its actual port when applicable. In
this mode Babel does not install, start, update or download models for the
server, or stop it on inactivity. Whisper uses the model loaded there; the UI's
Whisper variant selection controls only the embedded engine.

For external Whisper STT, `api_key_env` is an optional Bearer credential
reference. The secret can come from the process environment, a saved key, or a temporary key
applied in the dashboard. Do not put the secret in TOML. Non-TLS HTTP is accepted
only on loopback; remote servers require HTTPS. Embedded URL credentials are
rejected.

For external translation, legacy `ollama_endpoint` selects the address and
`translation_api` selects the protocol: `ollama` for `/api/chat` or `openai` for
compatible chat completions. That legacy name does not imply installing Ollama
in embedded mode; the managed runtime uses llama.cpp. One component may be
managed while another uses an external endpoint, without coupling transcription STT.

## Installation, development and limits

Use a Babel installer/package containing runtimes for the OS and architecture.
Copying only the Rust executable from a development build does not automatically
include native engines/libraries. If the package is incomplete, the UI reports
preparation failure; updating the complete installation is preferable to
pointing at an arbitrary port.

Native engines are separate processes from the Rust core. Babel's Rust code
forbids its own `unsafe`, but that does not make whisper.cpp, llama.cpp, ONNX or
other native components memory-safe. They have their own contracts, updates
and licenses; voice weights also have separate licenses.

Embedded recognition operates in segments. Local translation accumulates
segmentation, recognition, translation and synthesis time. Tiny lowers cost but
can sacrifice quality; Small may need more resources. Fixed latency or universal
continuous-speech performance on every CPU is not promised. To reduce overhead,
start with one direction, a smaller model and a thread count that leaves CPU
capacity for audio. The dashboard retains preparation errors, and queue limits
prevent unbounded lag.

Live inference overload keeps the session active: old pending speech is skipped
with a visible processing warning so recent speech can continue. This can leave
gaps in translation or live transcription on slower hardware; transcription
marks those gaps. Retained-history transcription waits for all segments instead.
Service failures remain errors and include the failed processing stage.

Whisper transcription saves original text without diarization or word timestamps.
Timing refers to captured segments. See [transcription](transcription.md) and
[provider guidance](other-providers.md) for protocol-specific limits.

## Package verification in CI

### Point-in-time memory measurement on 2026-09-29

On this Linux development host, using two threads and the public JFK audio
included with whisper.cpp, original Base and Base Q5_1 produced identical
transcripts. Post-inference RSS decreased from 245,268 KiB to 156,728 KiB,
and duration from 2.19 s to 2.14 s. Weights decreased from 147,951,465 to
59,707,625 bytes. This confirms reduced memory in that scenario; it is not a
multilingual evaluation, latency promise or Windows/macOS measurement.

### Installation and inference contracts

In release/package builds, beyond checking hashes and `--help`, CI uses
`scripts/test_bundled_inference.py` to load all three real engines on the runner's
architecture. It downloads only pinned catalog weights, sends one second of
synthetic silence to Whisper Base Q5_1 and a fixed sentence to Qwen, and requests
two utterances from the same Piper process. It checks dynamic-port discovery,
JSON responses, Unicode output paths and valid WAV samples. No microphone,
audio device or cloud key is used. Ordinary commit/PR validation does not build
these installer runtime payloads; see [CI and releases](ci-installers.md).

This test verifies installation, loading and protocols, not linguistic quality,
real-call latency or driver operation. Python is used only by development/CI
testing; it is not an installed Babel dependency. The test cache accepts the same
model filenames/hashes as the app, allowing verified weights to be reused.
