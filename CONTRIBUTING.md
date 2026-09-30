# Contributing to Babel

Babel targets Linux, macOS and Windows. Changes must preserve independent audio
routing, translation, transcription and recording, and keep expensive processing
out of audio callbacks. Read the [architecture](docs/architecture.md),
[testing guide](docs/testing.md) and relevant provider/platform documentation
before changing those paths.

## Repository language

Use English for new source comments, diagnostics, documentation, example
explanations, commit messages, pull requests and website content. Preserve
intentional localization catalogs, including Portuguese, and language-specific
test fixtures or speech examples when their language is part of the behavior
under test. Keep configuration keys, protocol fields, model IDs and public
identifiers stable unless a change explicitly includes compatibility/migration.
Do not rewrite third-party licenses, notices, vendored sources or user content
merely to change their language.

The dashboard and tray remain localizable. Add English base messages and keep
intentional translated catalogs synchronized; see [interface languages](docs/localization.md).
Documentation defaults to English. Update relative links and heading fragments
when moving or renaming sections.

## Commit messages

New commits use [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/):

```text
<type>[optional scope][!]: <short English description>
```

Examples:

```text
feat(transcription): add a provider-specific language selector
fix(audio): keep original routing active when recording fails
docs: explain native driver signing requirements
ci(release): publish installers for version tags
```

Use a type appropriate to the change, such as `feat`, `fix`, `docs`, `test`,
`refactor`, `perf`, `build`, `ci`, `style`, `chore` or `revert`. Mark a breaking
change with `!` and/or a `BREAKING CHANGE:` footer, and explain migration.
Write the subject in the imperative and keep it focused on the final change.
Existing history is not rewritten to retrofit this convention. Keep pull
request titles compatible with the convention, especially when squash merging.

## Development and validation

Install Rust 1.90 or later. UI tests require Node.js 22.12 or later; the running
application does not require Node.js. Start with the checks relevant to the
change, then run required repository checks:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
npm ci --prefix ui
npm test --prefix ui
```

Native drivers and packagers have additional workspaces/scripts documented in
the [testing guide](docs/testing.md) and [installer guide](docs/ci-installers.md).
Do not report hardware validation from cross-compilation or mocks. Use synthetic
audio and isolated test servers; do not capture a contributor's microphone,
change their default audio devices or call paid providers as part of ordinary
tests. Keep tokens, personal settings, session files and build outputs out of
version control.

For a pull request, describe the concrete problem, resulting behavior and
relevant validation. Record limitations or tests not run. Update documentation
for changed setup, capabilities, configuration and compatibility. Keep native
code limited to OS integration that the portable Rust core cannot provide.

## CI and releases

Ordinary pushes and pull requests run tests, lint/format checks and Conventional
Commit validation. They do not build installer payloads. Release builds are
triggered by stable version tags in the form `vX.Y.Z`, matching the package
version in `Cargo.toml`. Each numeric component must be at most 65535;
prerelease suffixes and build metadata are currently unsupported by the native
installers. A successful tagged run creates a GitHub Release and attaches
installers and integrity artifacts. Manual package workflows remain available
for development inspection. See [installer details](docs/ci-installers.md).

Creating a release tag is an explicit publishing action. Before tagging,
verify the version, required checks, release notes and package/signing status.
Do not describe development-signed or unsigned native drivers as approved for
production. macOS signing/notarization, Windows driver signing and native
hardware tests remain separate requirements from building release assets.

## Documentation website

The static documentation site is published at
[https://lainosantos.github.io/babel/](https://lainosantos.github.io/babel/).
Its build reads the repository's English Markdown guides; update the source
Markdown rather than generated `_site` files. Build and verify it locally:

```sh
python3 -m venv .site-venv
.site-venv/bin/pip install -r site/requirements.txt
.site-venv/bin/python scripts/build_site.py --output _site
.site-venv/bin/python scripts/test_site.py
```

On Windows, use `.site-venv\Scripts\python.exe` and
`.site-venv\Scripts\pip.exe` instead of the `bin` paths. The site builder checks
local files and heading fragments; fix broken links before publishing. Do not
commit the virtual environment or generated `_site` output. Website publication
is separate from building installers and does not imply that a release's native
drivers have production signing or hardware validation.
