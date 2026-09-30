# Original session recording

Audio recording is optional and off by default. When enabled, it produces
**one WAV file per session**, mixing the selected original sources:

- **Microphone:** speech captured before translation.
- **Incoming audio:** audio captured from the virtual output before translation.

Both inputs share the same timeline. Simultaneous voices overlap in the file;
intervals without audio remain silent. Sources are not concatenated one after
another, and the file does not contain the translator's synthesized voice.

## Configuration

```toml
[recording]
enabled = false
microphone = true
speaker = true
directory = "recordings"

[recording.mix]
microphone_gain_db = 0.0
speaker_gain_db = 0.0
microphone_priority = true
ducking_db = 3.0
microphone_threshold_db = -50.0

[files]
# Choose and adapt an absolute base for your OS:
# base_path = '/home/ana/Babel'
# base_path = '/Users/ana/Babel'
# base_path = 'C:\Users\Ana\Babel'
name_pattern = "{date}-{time}-{session}-{id}"
```

`babel init` creates the base as `Babel` inside the user's home directory.
An existing TOML without `base_path`, including the example above if it is not
filled in, is treated as legacy: its base becomes the configuration file's directory.

Enable recording on **Recording** when you want to save audio. Select sources
and a folder before starting the session. Recording is not required for
translation. It also works alone: turn off both translation directions and
record the desired sources. `microphone.enabled` and `speaker.enabled` control
translation only and do not limit recorder sources. A recording-only session
passes original audio and uses no AI. **Stop session** finalizes the WAV while
original routing continues as long as Babel remains open. The session name
and filename pattern apply to both audio and text.

Audio is configured on **Recording** and text on **Transcription**, each with
its own sources/folder. Both pages link through **Base folder & filenames** to
shared settings. Device connections live in **Routing**; languages, providers
live in **Translation**; translated audio uses the model's native default voice.

In **Settings → Session files → Base folder**, set the shared file folder.
With `base_path = "/home/ana/Babel"` and `directory = "recordings"`, the WAV
lands in `/home/ana/Babel/recordings`. The base must be absolute; new configurations
use `Babel` inside the home directory (`HOME` or `USERPROFILE`), independently
of the launch directory. A relative `directory` uses that base; an absolute one
uses its own destination. The dashboard shows full paths before saving. Preview
neither creates folders nor checks permissions. At recording start, Babel
recursively creates missing destination/parent directories, including the base.
Manual creation is unnecessary: nonexistence is not an error; inability to create
the folder or open the file is. This applies to Linux, macOS and Windows.
Changing the base does not move existing files. Legacy relative bases are
resolved once against the TOML directory and saved as absolute. If the old
launch directory differed, check the destination before recording. See
[platform-specific rules and examples](configuration.md#base-folder-and-destinations).

Pattern tokens:

| Token | Contents |
| --- | --- |
| `{date}` | Session start date in UTC (`YYYYMMDD`). |
| `{time}` | Start time in UTC (`HHMMSS`). |
| `{session}` | Session name converted to an identifier of up to 64 bytes. |
| `{id}` | Unique session identifier. |

The same stem is used for the **single mixed WAV** and **single TXT containing
both sources' original transcripts**, when both are enabled. The app adds
extensions in each type's configured directory. `{id}` is required to reduce
session collisions. Existing files are never overwritten: a collision is an
explicit error.

## Include audio from before session start

The **Start session** button, tray startup and `babel run` CLI start with
current audio, excluding history by default. The API also excludes history
when `history_seconds` is omitted or zero. To include recent audio, open
**Advanced start** beside the button, check **Include recent history**
and choose how many minutes to include. The default is ten minutes, bounded
by configured capacity. The option starts unchecked and resets after every
successful start.

The dashboard shows the combined duration of original audio available in memory. Only sources
selected in **Recording** enter the WAV; **Transcription** selections remain
independent. If available history is shorter than requested, Babel includes
only what remains. With no history in any selected source, the option is
unavailable. History precedes live audio in the same WAV, preserving source
overlap. It is neither played through devices nor sent to translation.

In **Settings → Recent audio history**, control retention and capacity: ten
minutes by default, from one second to sixty minutes. **History retention settings** opens those controls. Save to apply. Reducing capacity drops the
oldest audio; disabling clears history. Increasing capacity cannot recover
already-discarded audio.

Retention follows active routes. Microphone history builds while Babel is the
system default microphone or an app uses its virtual microphone; output history
builds only while an app sends audio to Babel. Selecting the physical microphone
as default pauses microphone capture if no app still uses Babel. Retention
creates no files and does not send that history to transcription until you
explicitly include it in a session. Audio remains in memory across session
start/stop and device switches; capture gaps cannot be recovered. Quitting
Babel loses all history. This works on Linux, macOS and Windows.

```toml
[history]
enabled = true
duration_secs = 600
```

PCM uses up to 38.4 MB for ten minutes of both sources (mono PCM16 at 16 kHz),
plus metadata. During inclusion, selected history is shared with writers/recognizers
without copying all audio. If retention keeps advancing during processing,
those references can temporarily retain another PCM window. A selected prefix
also remains shared with session recovery until the selected files complete.

## Recover an incomplete session

Sessions with recording or transcription enabled retain the selected original
sources independently of live processing queues. If a writer, recognizer or
finalization fails, the **Session files need recovery** card offers **Recover session**, which
reprocesses the retained originals. Keep Babel open until recovery completes.
The optional pre-session prefix is retained too. Both sources keep their own
timestamps and enter recognition separately; mixing happens only in the WAV.

Recovery uses the session's original source selections, folders, mixing settings
and STT profiles. It creates new uniquely named files with a shared stem, leaving
the previous partial files untouched. Cloud recognition can incur additional
charges. A failed retry keeps the originals available for another attempt;
closing the dashboard does not cancel recovery. Original live routing continues.
Recognition uses bounded windows of about thirty seconds, preserving cumulative
source timestamps. Empty sources do not open a model connection; digital-silence
windows advance the clock without an inference request.
Provider-assigned speaker IDs may restart between requests; they are not
persistent identities across windows or sources.

Live retained PCM starts in memory. Above an 8 MiB working budget, a separate
processing worker spills bounded batches encrypted with **XChaCha20-Poly1305**
under a unique `.babel-retention-*` directory in the system temporary folder.
The platform resolves that folder, including its temporary-directory environment
overrides such as `TMPDIR` on Linux/macOS and `TMP`/`TEMP` on Windows. It is
independent of `files.base_path`. Audio and capture metadata are encrypted before
the first file write. Every session has a randomly generated key held only in
process memory; Babel never writes it to
configuration, files, logs or a credential store. Authenticated records detect
tampering and truncation. UNIX directories use `0700` and files `0600`; Windows
inherits the temporary parent folder's ACL. The final, explicitly requested
TXT/WAV files still use the configured destination folders and base path. They
are ordinary user files and are not encrypted by this temporary-storage scheme.

Pending originals are not evicted by age or by a failed provider. They are
released after all selected writers complete successfully, including their
final filesystem sync. The already allocated optional history prefix stays
shared in RAM. If spill fails, accepted live originals stay in memory; a 64 MiB
live-retention ceiling reports an explicit failure rather than silently
overwriting retained data. The ceiling excludes the separately configured
rolling/history prefix and small in-flight encryption/decryption buffers.

This is recovery while the process remains alive, **not crash recovery**.
Normal cleanup removes temporary ciphertext and erases the key. Forced exit or
power loss destroys the key; leftover encrypted files cannot be reopened. OS
swap and crash dumps are outside this application's key-storage guarantee.
Finite buffers cannot preserve audio that never reaches retention because of
a device/capture overrun or exhausted memory/storage. Such a gap is reported,
and replay of the surviving frames is never presented as complete recovery.
These storage and recovery paths are shared Rust code on Linux, macOS and Windows.

## Format and volume

The WAV uses **PCM16 mono at 16 kHz**, uncompressed. It contains original audio
from Babel's capture, converted in the separate speech-processing copy; it is
not a multichannel 48 kHz hardware archive. Original live routing retains its
negotiated float format independently. Recording precedes translation,
synthesis and output gain.

The two sources can have very different levels: a quiet microphone can become
inaudible under louder music even though both original signals are present.
**Recording → Recording balance** controls this balance only in the saved WAV:

- **Microphone level** and **Incoming audio level** independently adjust the original
  sources from −24 to +24 dB; both default to 0 dB. Increase the microphone gain
  when its original capture is too quiet. Positive gain also raises any noise
  already captured by that microphone.
- **Keep microphone audible** is on by default. While the original microphone
  exceeds the activity threshold, Babel smoothly reduces the incoming source in
  the recording. It never mutes or gates the microphone. Attack, hold and release
  prevent abrupt volume switches between syllables.
- **Incoming audio reduction** sets that reduction from 0 to 30 dB, default 3 dB.
  This subtle default retains about 71% of the incoming signal's amplitude while
  both sources overlap, before the shared mixing headroom. A larger value gives
  the microphone more space; the previous 12 dB default retained about 25%.
  Saved values remain unchanged when Babel is updated. Lower this control for a
  lighter effect in an existing configuration, or set it to 0 to disable reduction.
- **Microphone activity threshold** ranges from −60 to −20 dBFS, default −50.
  It is measured before microphone gain. A more negative value detects quieter
  activity; a less negative value rejects more background noise. This is level
  detection, not speech recognition: loud noise at the microphone can activate it.

These settings apply to the next session. They never change the physical device
volume, original live routing, translated voice gain, STT input, or in-memory
history. History included at session start uses the same recording mix exactly
once, followed by live originals without resetting the mix state. Speaker-only
recording never ducks itself, and microphone-only recording cannot attenuate an
unselected incoming source. No model, service or network request is involved.

The mixer retains its fixed summing headroom: each selected source contributes
0.5 when both are selected, or 1 with only one selected, before the recording
gain and priority adjustments. With priority off and both gains at 0 dB, the
previous fixed mix is preserved exactly. Added gains use linked peak protection
with five milliseconds of lookahead inside the file worker, preserving relative
levels without integer clipping. This introduces no latency into live routing.
There is no automatic normalization of quiet microphone noise toward a target
level. Turn priority off when you want fixed original-source levels.

The mixed file cannot perfectly separate the two voices afterward. Changing
these controls does not repair a previously saved mix or recover a microphone
that was not captured; separate-source recovery would require a different
recording format with independent channels.

Disk use is approximately **115 MB per hour**. RIFF WAV's container limit is
about 37 hours in this format. Reaching it reports a recording error instead
of silently truncating audio or opening another file. Original routing remains
independent of that recording failure.

## Timing, device switching and continuity

The recorder uses a session-wide monotonic clock. Each frame is positioned by
capture-completion time minus its sample duration. Scheduling variations up to
50 ms are absorbed by each source's continuity; larger discontinuities create
silent intervals.

A device switch keeps the session file. Time needed to close/reopen capture may
appear as silence when input resumes. Synchronization is estimated from pipeline
timestamps, not shared hardware clocks: device-specific latency can offset the
two sources.

The mixer keeps a bounded two-second window for both inputs before committing
their sum. It reserves at most three seconds of separate source samples (about
384 KiB), including room for one incoming frame, plus a small fixed lookahead
queue. Gains and priority are applied after timestamp alignment, independent of
frame size or source arrival order. Long gaps are written in bounded blocks. Its input queue
is also bounded. Excessively delayed input or write failure is reported to the
session supervisor rather than silently losing recording data. Such failures
are visible without canceling original routing.

## Finalization and privacy

On normal stop, Babel lets the recorder consume queued frames, writes the tail
and updates the WAV header before closing. Intermediate headers are periodically
updated for committed data. Power loss, forced termination or disk failure may
lose the last seconds; an interrupted recording does not have the guarantees
of a completed stop. Session shutdown has a bounded drain deadline and reports
potentially incomplete files if that deadline expires.

Files use exclusive creation and Unix mode `0600`: read/write by the user only.
On Windows, they inherit the chosen folder's permissions. Recording stays local
and performs no upload. This does not change audio transmission required by
translation when a remote provider is selected.

## Tests

```sh
cargo test --lib recording:: -- --nocapture
```

Tests use only synthetic PCM and temporary directories. They cover overlapping
mixes in one file, quiet-microphone/loud-music balance, original-only source gains,
noise and silence, peak protection, chunk/order invariance, headroom, one source,
jitter compensation, long gaps
without unbounded memory growth, excessive delay, error finalization, shutdown
draining, valid headers, privacy and overwrite refusal. No test captures
personal speech or enables recording in the actual user configuration.

Linux with PulseAudio/`pipewire-pulse` also has an optional full-path test:

```sh
cargo test --test recorded_session -- --ignored --nocapture
```

It creates four isolated temporary null sinks, sends two tones through the
Controller and verifies one WAV with both frequencies overlapping. A previous
real execution produced a valid 3.53-second file, stopped without error and
removed only test modules. Existing Babel devices remained intact. `pactl`,
`parec` and `pacat` must be on `PATH`; no physical device is used. Historical
results do not replace rerunning the test after transport changes.
