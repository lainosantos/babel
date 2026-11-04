# Installers in GitHub Actions

Normal commits and pull requests run validation: Rust tests, Clippy, formatting,
interface tests, CLI smoke checks and Conventional Commit checks. They do not
build installer payloads. New commits must follow Conventional Commits; existing
history is not rewritten to adopt that policy.

Pushing a stable release tag in the form `vX.Y.Z` triggers the release workflow.
Its version must match `Cargo.toml`, with each numeric component at most 65535.
Prerelease suffixes and build metadata are currently rejected before building
because the native installer version fields do not support them.
After its required checks pass, it builds application installers,
creates the corresponding GitHub Release and attaches the packages and
integrity files. The reusable native-driver/installer workflow also supports
manual package builds. Manual builds are useful for inspection and do not
replace the tagged-release path. See [contribution and release policy](../CONTRIBUTING.md).

For manual validation, select `all`, `linux`, `macos` or `windows` in the workflow's
`platform` input; tagged releases always build all platforms and require every
artifact before publication.
The optional `windows-architecture` selector limits manual Windows builds to
`x64` or `ARM64`; its default is `all`, it does not affect Linux or macOS, and
tagged releases always build both Windows architectures. Set
`include-native-driver` on a manual build to include the Babel development
driver. Tagged releases use application-only packages on macOS and Windows.

Publication waits for every required platform artifact and records all files
in `release-manifest.json` and `SHA256SUMS.txt`. A partial draft can be resumed
only when its existing assets belong to the prepared set; unexpected files
stop publication without deleting them. Published releases are never replaced
by a workflow rerun.

All jobs use GitHub-hosted runners. No `self-hosted` runner, manual runner WDK
installation or AI-provider API key is required. The repository must include
all sources, manifests, lockfiles, scripts and workflows; build directories
and personal configurations must not be committed.

## Downloads

For a tagged release, open the repository's **Releases** page and select the
matching version. Workflow artifacts also appear under **Actions → workflow
run → Artifacts**. After the relevant checks and builds pass, installer
artifacts are retained for 30 days:

| Artifact | Contents | Runner |
|---|---|---|
| `babel-installers-linux-amd64` | `.deb`, `.rpm`, `.tar.gz`, manifests and hashes | Ubuntu 22.04 x64 |
| `babel-installers-macos-universal-development` | `.pkg` with Babel.app, manifest and hashes | macOS 15 / Apple SDK |
| `babel-installers-windows-x64-development` | `.exe` installer, complete `.zip`, manifest and hashes | Windows Server 2022 / VS2022 |
| `babel-installers-windows-ARM64-development` | Same formats with ARM64 payload | Windows Server 2022 with cross-compilation |

Filenames include the version from `Cargo.toml`; the release tag must match
that version. The macOS `.pkg` contains Intel and Apple Silicon code. Windows
uses separate installers and checks the OS's native architecture; the x64 driver
is not offered through emulation on ARM64. On Linux, the minimum glibc version
is extracted from the actual built binaries. The `.deb` targets compatible
Debian/Ubuntu systems; the `.rpm` targets compatible RPM distributions such as
Fedora. Other distributions can use the portable archive and install the
dependencies listed in the Linux guide.

A build/package-test failure or missing required file prevents upload of that
installer. WDK failure logs are preserved separately. A GitHub Release and its
assets do not imply that development drivers have acquired production signing
or passed hardware validation.

## Included inference engines

Before installers, the reusable `native-runtime.yml` workflow builds whisper.cpp,
llama.cpp and Piper from archives pinned by commit, size and SHA-256 in
`scripts/local_runtime.lock.json`, across five targets grouped by platform:

| Payload | Native runner |
|---|---|
| Linux x64 | `ubuntu-22.04` |
| macOS ARM64 | `macos-15` |
| macOS Intel | `macos-15-intel` |
| Windows x64 | `windows-2022` |
| Windows ARM64 | `windows-11-arm` |

Windows builds discover the installed Visual Studio version with `vswhere` and
select a matching CMake generator. The ARM64 runner uses CMake 4.2.3 for Visual
Studio 2026 and its ClangCL toolset, as required by GGML on Windows ARM64; the
other runners retain CMake 3.31.10. These are build tools only, not dependencies
of the installed application.

Each payload includes native libraries, eSpeak data, licenses, corresponding
Piper/eSpeak sources and an integrity manifest. Windows runtimes privately
bundle the shared Visual C++ DLLs used by the inference engines and ONNX; users
do not need to install a separate redistributable. On macOS, Intel/ARM64 runtimes remain separate inside
the universal app; after signing their binaries, packaging refreshes hashes
before signing the outer bundle.

The job runs `scripts/test_bundled_inference.py` with pinned catalog weights:
Whisper receives synthetic silence, Qwen translates a fixed sentence, and the
same Piper process produces two WAV files. This checks real loading, dynamic
ports, JSON, Unicode paths and valid audio, without a microphone or credentials.
The synthetic smoke uses CPU inference. On macOS, its environment disables
Metal device probing because the hosted runner may lack a usable GPU; the
distributed libraries still include Metal acceleration and embedded shaders.
This smoke does not validate GPU execution on physical Macs.
Test models use a cache keyed by the hash of `src/local_runtime/models.json`
and are not included in installers. Each platform's packager waits only for its
own runtimes and portable checks; release publication still requires all platforms
to succeed.
Windows x64 and ARM64 each run their own runtime-to-installer pipeline, so a
failure or delay in one architecture does not block packaging the other.

Intermediate artifacts are named `babel-local-runtime-<system>-<architecture>`.
Each packager receives only its system's payloads and verifies architecture,
applicable permissions and hashes. Selecting a local provider in the installed
app therefore needs no Python, Ollama, CMake or manual server installation;
it only needs to download weights that are missing during initial preparation.

## What each build checks

### Linux

Builds release executables; runs `--help`, `--version` and `init` with temporary
configuration; creates and inspects `.deb`, `.rpm` and `.tar.gz` packages.
Checks layout, permissions and binary integrity. Includes launcher, menu entry,
icon, documentation and licenses. Installation does not start Babel, enable
autostart or change the audio server. Packages include `org.babel.audio.service`
for the user's systemd manager; Babel's startup option enables/disables it
when available and retains XDG as an alternative. This is not an audio daemon
running as root. PulseAudio/PipeWire-pulse remains the Linux backend;
`pulseaudio-utils` supplies `pactl` and the audio clients.

### macOS

Builds the app for Intel and Apple Silicon, combines slices with `lipo`,
and verifies the bundle and expanded `.pkg`. The app installs under
`/Applications` and declares microphone access in `Info.plist`. With
`include-native-driver`, the build also tests and packages the HAL plugin.

### Windows

Inno Setup builds the app installer and portable ZIP for x64 and ARM64.
The app uses the static C runtime. The x64 job installs the application in
a temporary runner directory, verifies the manifest, runs `babel --version`,
and uninstalls it. For builds with `include-native-driver`, CI also restores the
pinned WDK, inspects the driver payload and verifies that setup leaves the
Babel driver unloaded. ARM64 applications are inspected on the x64 runner.
ARM64 inference engines are exercised separately on a native ARM64 runner.

## Signing and validation scope

The application-only builds use a local ad-hoc signature on macOS and unsigned
executables on Windows. The build needs no publisher account. macOS can require
manual approval, and Windows SmartScreen, Smart App Control or a managed-device
policy can warn or block execution. See [platform setup](platforms.md) for the
free virtual audio dependencies and system-specific limits.

The optional Babel drivers have separate signing requirements. The development
packages are intended for driver testing. See [native drivers](native-drivers.md)
for package signing, installation and removal.

A green job proves the listed checks, not audio stability on hardware.
Microphone permissions, quality, latency, suspend/resume, Driver Verifier and
use in calls still require the [native test scenarios](testing.md). Check the
actual GitHub run; local YAML/script validation is not a successful hosted run.

## Installed application configuration

The tray uses a writable per-user configuration, independently of the current
directory or protected application folder:

- Linux: `$XDG_CONFIG_HOME/babel/babel.toml` if the prefix is absolute;
  otherwise `~/.config/babel/babel.toml`.
- macOS: `~/Library/Application Support/Babel/babel.toml`.
- Windows: `%APPDATA%\Babel\babel.toml`, falling back to the user's Roaming folder.

`--config` still selects a different file. The `babel` CLI keeps its explicit
development behavior with `babel.toml` in the current directory. Autostart is a
user choice in Settings. The dashboard still uses a dynamic port. Packages
include engines, but not model weights, API keys, recordings, transcripts or
the development machine's configuration. Weights are prepared automatically
when a local selection is saved; see [storage, downloads and offline use](local-inference.md).

## Reproducing locally

Instructions and arguments are in:

- [Linux](../packaging/linux/README.md).
- [macOS](../packaging/macos/README.md).
- [Windows](../packaging/windows/README.md).
- [Native drivers](native-drivers.md), including builds and signing.

Primary references: [hosted runners](https://docs.github.com/en/actions/reference/runners/github-hosted-runners),
[Windows 2022 image](https://github.com/actions/runner-images/blob/main/images/windows/Windows2022-Readme.md),
[WDK in CI](https://techcommunity.microsoft.com/blog/windowsdriverdev/building-windows-driver-projects-with-ci-and-cd/4379200)
and [driver-installer architectures](https://jrsoftware.org/ishelp/topic_setup_architecturesallowed.htm).
