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
