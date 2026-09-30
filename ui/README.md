# Local dashboard

The Rust binary embeds `index.html`, `style.css` and `app.js` directly. The
dashboard uses plain JavaScript and needs no Node.js, npm, CDN or package
installation at runtime.

## Development tests

With Node.js 22.12 or later, from the `ui` directory:

```sh
npm ci
npm test
```

`jsdom` is a test-only dependency. HTTP responses are simulated in memory:
tests do not access providers, use the user's audio or create real voices.

Tests cover independent profiles, continuous-model restrictions, voice
selection, escaping remote content, temporary keys, session names, explicit
login startup, independent text and recording, filename patterns and tray
changes synchronized without overwriting drafts. Saving and starting use the
configuration revision to reject concurrent changes. Regressions cover original
audio without a session, recording or transcription with both translations off,
and safely presented routing errors. Rust tests in `src/dashboard.rs` cover
authentication, local origins, upload limits and non-persistence of credentials.

## Interface language

The header selector offers **System default**, **English** and **Português**.
It works during a session and saves only `interface.language`, without starting,
stopping or reconfiguring audio. The preference uses the process/system locale
resolved by the backend at `/api/interface`, never the browser's
`navigator.language`. Speech languages, prompts, session/voice names,
transcripts, devices and files are user content and do not change with the
interface language.

Initial HTML is English. `i18n.js` loads the local catalogs embedded in the
binary at `locales/en.json` and `locales/pt.json`. Text uses stable keys with
named parameters, such as `session.identity` with `{name}`. A missing key in
the selected catalog falls back to English. Catalogs contain text only;
parameters are inserted through `textContent`, never as HTML. Numbers,
percentages and dates use `Intl` with the resolved language.

To add a language:

1. Copy `locales/en.json` to the language code, translate values and preserve
   keys and parameters. Keep product names, identifiers and units unchanged.
2. Register the language, native name, locale resolution and catalog in the
   Rust localization/backend module and expose `/locales/<code>.json`.
3. The selector adds languages reported by `/api/interface`; the new code
   needs no JavaScript change. Static text uses `data-i18n`,
   `data-i18n-placeholder`, `data-i18n-title` or `data-i18n-aria-label`.
   Dynamic text uses `t(key, parameters)`.
4. Add the tray menu translation to the Rust catalog and run interface and
   backend tests. Help documents use English regardless of interface language.

Language changes update text and attributes without rebuilding forms: drafts,
focus, cloning files and open dialogs are preserved. `If-Match` protects
concurrent changes; saving a preference updates the local revision without
saving audio drafts. Tests include English/Portuguese, fallback, switching during
a session or editing, conflicts, upload preservation and catalog coverage.

## Voice commands and MCP

Activation uses only the original physical microphone. Output audio, including
other participants' incoming speech, never feeds the agent.

`agent.js` controls a section separate from the audio form. `GET/PUT /api/agent`
use their own revision; saving these settings neither saves audio drafts nor
restarts the session. Status from `/api/agent/status` shows activation, local
recognition, Needle3 decisions, execution, results and failures. The floating
panel can be dismissed, does not move focus and renders responses as plain text.
Identical updates do not repeat screen-reader announcements.

Integrations support Streamable HTTP or stdio processes, with an optional tool
allowlist. Discovery uses `tools/list` without executing tools. The button stays
disabled until the server is saved, avoiding tests against an older configuration.
OAuth accounts have an explicit connection action, authorization link and
callback monitoring; they can also be disconnected. Bearer tokens, headers and
secret variables use references. Temporary values travel through a separate API
and never enter saved JSON.

`agent-tests.cjs` exercises these flows using in-memory responses, without MCP
servers, audio capture or real accounts. English and Portuguese catalogs cover
all agent states and fields.

## Operating-system controls

The dashboard queries authenticated `/api/platform` to identify **the computer
running Babel**. It does not inspect the browser's user agent or platform.
Installation text, permissions, endpoint names and startup methods follow Linux,
macOS or Windows. On Windows, the diagram uses generic labels to avoid confusing
a cable's Input/Output sides; instructions identify each side. On macOS, selected
virtual device names may appear in the diagram.

Device creation/removal is available only on Linux when the backend reports that
capability. This does not guarantee the audio server or its tools are installed.
macOS/Windows show the driver installation guide. `/help/platforms` is the guide
for the current OS; `/help/platforms/all` retains the complete documentation.
The startup guide is identified as the complete guide.

Failure or an unknown platform keeps generic labels and disables driver
management. Refreshing devices also retries platform detection, without changing
selections or drafts. Tests cover all three systems, a mismatched user agent,
failure/recovery and changing language while editing.

## Workspace organization

The dashboard has six views: routing, translation and voices, transcription,
recording, voice commands and host settings. The session bar stays available
while navigating. Fields remain in the same forms; changing view or language
does not rebuild inputs, discard drafts or start/stop audio.

Routing groups devices and meters. Translation and voices groups languages,
prompts, providers, credentials and the library. Transcription and recording
retain their own sources and destinations. The shared base folder and filename
pattern live in Settings, linked from both pages. `data-workspace-field`
reveals a target field, opens its details and moves focus, including shortcuts
to the same page. Old navigation names migrate without changing settings.

`files.base_path` requires an absolute path. New configurations use `Babel`
inside the user's home directory; the frontend does not calculate paths from
the browser or launch directory. Authenticated `POST /api/file-paths` resolves
transcript and recording destinations on the Babel host without creating
folders. Both destinations may be relative to the base or absolute. The backend
migrates legacy bases when loading the TOML before sending configuration to the
dashboard.

`workspace.js` controls navigation, focus and validation-error presentation only.
If a field in another view is invalid, its view, profile and details open before
validation is presented. Agent settings remain independent of the audio form
during a session.

The design direction and visual tokens are in [DESIGN.md](DESIGN.md). The
executable includes Manrope and its license at `fonts/OFL.txt`; opening the
dashboard does not fetch fonts or other external visual assets. Meters use real
levels from the backend, without animations that simulate activity.
