# Babel interface direction

The product is a live audio workspace: two independent routes, an optional
session that creates files, provider and voice configuration, and a microphone
command agent. The interface must make those separate lifecycles understandable.

## Design tokens

- Canvas: cool slate `#EEF2F7`.
- Surface: white `#FFFFFF`.
- Ink: deep blue `#192A42`.
- Microphone and primary action: blue `#345CF4`.
- Incoming audio: teal `#147D80`.
- Secondary text: slate `#66758A`.
- Typeface: locally served Manrope, with system sans-serif fallback. Use
  tabular numerals for audio levels and counts. Sentence-case labels.

## Layout decision

Use a stable navigation rail and six views organized by function. The session transport
stays available while changing views. All controls stay mounted to preserve
drafts, selected files, independent revisions and authentication state.

```text
Babel                 Current task                      Language  Audio status
Routing               --------------------------------------------------------
Translation & voices  Physical mic -> virtual mic     Virtual output -> speakers
Transcription         Devices and signal levels      Devices and signal levels
Recording
Commands              Each function has its own controls and independent switch
Settings              --------------------------------------------------------
Host OS               Session name           Unsaved state         Save / Start
```

The distinctive element is the pair of direction-colored audio strips. Meter
levels come from real backend samples; no simulated waveform or invented session
activity. Device diagrams represent the actual original/translated route.
Translation uses the same blue/teal direction identity for language, provider,
prompt and voice controls, followed by shared provider profiles and the voice
library. Routing contains devices, meters and audio tuning. Transcription and
recording each have their own destination and original-source selection. Shared
storage (base path and filename pattern) belongs to Settings, with direct field
shortcuts from both file-producing functions. Headings and fields align left.

The base folder is always absolute, defaulting to `Babel` inside the user's home
directory for new configurations. Do not present the process launch folder as a
storage option. Transcript and recording folders may be relative to this base
or absolute. Destination previews come from the Babel host, not browser path
calculations, and never create directories.

The session dock also exposes compact Translation, Recording and Transcription
switches, using the same switch style as their pages. These shortcuts mirror the
canonical form fields and save through the existing session-start flow; they do
not introduce separate settings. Translation preserves the current direction
selection when switched off and back on in the current draft. Its small scope
label shows which directions will be used. External configuration reloads remain
authoritative. All three shortcuts are read-only during a session or startup,
and recent history remains a separate, explicit opt-in.

## Review against the brief

The functional separation makes always-on routing distinct from optional
translation, transcription and recording. Keep one canonical control for every
setting, and use focused shortcuts where providers or storage are shared. Live
meters and the persistent session transport retain context across all six views.
Blue and teal distinguish the two audio paths; they do not imply enabled state.
Playback/routing/session state always has a text label as well.

## Interaction requirements

- Keyboard navigation, visible focus and responsive layouts through mobile.
- Never recreate form nodes when changing views or language.
- Reveal the appropriate view and disclosure when a field fails validation.
- Migrate stored view names (`audio`, `voices`, `files`) to the corresponding new views.
- Keep agent changes independent from audio-session settings.
- Match the actual host OS and retain both interface languages.
- Respect reduced motion and use no continuously animated decoration.
- Bundle typography with the app; no external font request at runtime.

The Manrope font and its OFL license are distributed in `ui/fonts` from the
[Google Fonts source](https://github.com/google/fonts/tree/main/ofl/manrope).

## Command feedback

Use the actual Babel B mark in a compact notification above the session dock.
The mark carries one restrained listening wave or processing orbit; the rest of
the workspace stays still. Success uses a small teal check, failure a muted coral
exclamation. Both retain a readable text state. Reduced-motion preferences disable
all logo motion. No notification opens a dialog, moves keyboard focus or plays a
sound.

Keep the spoken command, tool name, result and technical error inside a disclosure
that starts closed for every activation. The polite live region announces only
the phase and generic hint. The Commands page retains its independent current
listener state while a completed command remains briefly visible. Desktop feedback
and the dashboard share the same bounded backend visual events, including commands
that finish between dashboard polls. Opening settings does not replay past results.

Dashboard completion dismisses after five seconds; failure after nine. Hover,
keyboard focus and an expanded disclosure pause that timeout. A new activation
replaces the previous notification and cancels its timeout. Dismiss and Escape
hide the whole activation, including later updates. Local service setup errors
remain contextual on Commands rather than appearing as failed spoken commands.
