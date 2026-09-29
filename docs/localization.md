# Idiomas da interface / Interface languages

The dashboard and tray support English (`en`) and Portuguese (`pt`). The saved
preference is `interface.language`; `system` is the default, including for older
configuration files. System selection uses the Babel host's user locale through
[`sys-locale`](https://docs.rs/sys-locale/0.3.2/sys_locale/), not the browser's
language. Regional variants map to their base language. Unsupported, missing,
`C` and `POSIX` locales fall back to English.

## Change language

Use **Interface language / Idioma da interface** in the dashboard. The preference
is saved immediately. You can change it while a session is running without
restarting audio, AI connections or session files. It is separate from speech
source/target languages and does not translate user prompts, names, voice IDs,
transcripts or existing recordings. Help documents remain in Portuguese.

The authenticated `GET /api/interface` returns the saved preference, resolved
language, detected system locale, registered languages and configuration revision.
`PUT /api/interface` accepts `{"language":"en"}` (also `pt` or `system`) and the
same `If-Match` revision protection as configuration saves. This endpoint changes
only the interface preference; it does not stop or recreate audio routes. A stale
revision returns HTTP 412. Unsupported language codes are rejected.

## Add a language

1. Copy `ui/locales/en.json` to a catalog with the new base language code. Keep
   message keys and named interpolation placeholders unchanged; translate values.
   The dashboard uses `ui/i18n.js`, updates labels/attributes without replacing
   editable inputs, and falls back per missing key to English.
2. Copy `locales/tray/en.json` to the corresponding tray catalog. Keep action IDs,
   configuration values and device IDs unchanged. `session.default` is used only
   when creating a new session without a supplied name.
3. Add the language's code and native display name to `SUPPORTED_LANGUAGES` in
   `src/i18n.rs`, and embed its tray catalog in `CATALOGS`. Register the dashboard
   catalog route in `src/dashboard.rs`, following `/locales/en.json`. Static
   assets are embedded in the Rust binary; rebuild to ship changes. Runtime
   translation does not call AI services or download third-party assets.
4. Add the diagnostic catalog under `locales/messages` and register it in
   `src/interface_messages.rs`. Diagnostics are matched against known Babel
   messages; dynamic values and unknown OS/provider details stay verbatim.
5. Run `cargo test --locked --all-targets` and `npm test --prefix ui`. Check
   catalog parity/placeholders, fallback, system resolution, live switching,
   unsaved form preservation, screen-reader labels and narrow layouts. Add a
   locale test case before considering the new language supported.

English is the base catalog. Keep every English message available and use stable,
descriptive keys for new UI text. Do not use translated labels as action IDs or
persisted values. Do not localize provider model names, audio device IDs,
configuration keys, user text, transcripts, URLs or file paths. Interface
formatting may use the selected locale; technical timestamps and filenames retain
their existing formats.

## Scope and performance

Localization happens on control/UI paths. The real-time audio callbacks,
bounded audio queues and provider PCM formats do not change. Tray catalogs are
parsed once and cached. Locale selection does not mutate global process locale.
Native Windows/macOS tray behavior still requires verification on those systems;
cross-compilation cannot verify OS presentation.
