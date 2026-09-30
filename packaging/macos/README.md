# macOS application and driver installer

`build.py` packages prebuilt executable `babel` and `babel-tray` binaries with
the BabelAudio driver package from `native/macos/build.py`. It creates
`Babel.app`, a combined `Babel-<version>-macos-<architecture>.pkg`,
`SHA256SUMS.txt` and `manifest.json`. Both components are mandatory in the
installer; it retains the driver package's original installation scripts.

Run packaging on macOS with Python 3.11+ and the Xcode command-line tools.
Install Rust and both target standard libraries beforehand when making a
universal build. The application requires macOS 14.2 or later. Its two Mach-O
executables must contain exactly the requested architectures and must not
depend on local Homebrew libraries or other unbundled libraries. The driver
package must match the supplied driver bundle and architecture set.

## Development and CI

From the repository root, build the application slices with a 14.2 deployment
target, combine them, and build the driver using its own SDK checks:

```sh
rustup target add aarch64-apple-darwin x86_64-apple-darwin
MACOSX_DEPLOYMENT_TARGET=14.2 cargo build --locked --release --bins --target aarch64-apple-darwin
MACOSX_DEPLOYMENT_TARGET=14.2 cargo build --locked --release --bins --target x86_64-apple-darwin
mkdir -p packaging/macos/universal-bin
for binary in babel babel-tray; do
  lipo -create "target/aarch64-apple-darwin/release/$binary" \
    "target/x86_64-apple-darwin/release/$binary" \
    -output "packaging/macos/universal-bin/$binary"
  chmod 755 "packaging/macos/universal-bin/$binary"
done
python3 native/macos/build.py --unsigned --arch universal --test --pkg
python3 packaging/macos/build.py --unsigned --arch universal \
  --runtime-dir artifacts/local-runtime \
  --bin-dir packaging/macos/universal-bin \
  --driver-dir native/macos/dist --output packaging/macos/dist --version 0.1.0
```

Build `scripts/build_local_runtime.py --output artifacts/local-runtime` on each
native Mac architecture, or extract the two runtime artifacts produced by CI.
The universal app contains both `macos-aarch64` and `macos-x86_64` directories;
they are separate native engines, not binaries requiring Rosetta. The builder
needs CMake 3.26+, Git and a C++17 compiler; users need none of those tools.

Omit `--version` to read the root Cargo.toml package version. Numeric `X.Y.Z`
versions are accepted. `--arch arm64` or `--arch x86_64` can package a single
architecture if both application binaries and the driver use that same
architecture. The packager does not build Rust code or download dependencies.

The script inspects both slices using `lipo` and `otool`, stages the application,
ad-hoc signs it, verifies the code signature, builds both-component distribution,
and uses `pkgutil --expand-full` to verify identities, destinations, binaries,
bundled driver installer and preserved driver scripts. No `installer`, `sudo`,
application launch, login-item registration or Core Audio restart is executed.
Its final files are published only after these checks pass. Existing output
symlinks and foreign application bundles are rejected.

An unsigned CI package is a development artifact. It is not a notarized public
release. Successful compilation/package inspection is not proof that audio
works on hardware, that permissions are granted or that Gatekeeper accepts a
public download. Test installation and audio on Intel and Apple Silicon Macs
before distribution, including upgrade, microphone permission, device changes,
login startup and removal. The native driver's conservative 4096-frame
loopback delay is approximately 85.33 ms at 48 kHz.

Portable script tests need only Python; they don't invoke Apple tools:

```sh
python3 -m unittest discover -s packaging/macos -p 'test_*.py' -v
python3 packaging/macos/build.py --help
sh -n packaging/macos/installer/preinstall
```

## Bundle and installed locations

The installer writes `/Applications/Babel.app` and
`/Library/Audio/Plug-Ins/HAL/BabelAudio.driver`, owned by root/wheel through
`pkgbuild --ownership recommended`. It refuses to replace symlink targets or
foreign bundles. Installer asks for administrator authorization and a restart;
it does not change the system's default audio devices. On an upgrade it asks
the user to close the running app through the Distribution `must-close` rule.

`CFBundleExecutable` is the real `babel-tray` Mach-O, with `LSUIElement=true`.
There is no shell launcher or invented `--serve` option. Microphone-purpose
strings are included in English and Portuguese. Hardened-runtime signing uses
only the audio-input entitlement; the application is not App Sandbox enabled.
The app uses `~/Library/Application Support/Babel/babel.toml` by default and
requests a dynamic dashboard port. It does not write beside its executable.

The bundle includes:

- `Contents/MacOS/{babel,babel-tray}`;
- `Contents/Resources/drivers/macos/BabelAudio.pkg` and `uninstall.sh`, matching
  the native-device discovery paths used by the application;
- driver licenses/instructions and the repository's `docs/*.md` guides;
- `Contents/Resources/local-runtime/macos-{aarch64,x86_64}` with Whisper,
  llama.cpp, Piper, ONNX Runtime, eSpeak data and their licenses. CPU backends
  and Metal are included; no Homebrew or separately installed AI daemon is used;
- complete corresponding Piper/eSpeak source archives, patches and build
  instructions in each runtime's `sources/` directory;
- `Contents/Resources/Support/scripts/` with Whisper setup, the Needle bridge
  and the pinned Whisper port-discovery patch.

The translation and transcription engines start through Babel when selected.
Weights are verified and downloaded by the application on first use, outside
its immutable signed bundle. Native executables and libraries are signed before
the outer app, and payload hashes are regenerated after signing. No Python,
API key, user TOML or recording is included. The separate Needle command agent
still has its own setup. To prepare those command services explicitly:

```sh
mkdir -p "$HOME/Library/Application Support/Babel"
cp -R /Applications/Babel.app/Contents/Resources/Support/scripts \
  "$HOME/Library/Application Support/Babel/"
cd "$HOME/Library/Application Support/Babel"
python3 scripts/setup_whisper.py --backend metal
python3 -m venv .tools/needle
.tools/needle/bin/python -m pip install cactus-needle==3.0.6
.tools/needle/bin/python -c "import os; os.environ['NEEDLE_TELEMETRY']='0'; os.environ['DO_NOT_TRACK']='1'; from needle import Needle; Needle(tools=[]).close()"
```

Use `--backend cpu` for Whisper without Metal. Read the bundled
`Documentation/voice-commands.md` for requirements and provider limitations.
With the default configuration location, leave the services directory empty;
otherwise set an absolute writable directory containing `.tools` and `scripts`.
Keep both managed endpoints `auto`. Babel starts already-installed helpers on
OS-assigned ports and preserves `auto` in the configuration. Never modify the
signed app bundle to install helpers. Copying the support directory above
should be done once; preserve any existing user-modified scripts on upgrades.

Disable login startup and quit Babel before removal. The driver removal
command is explicit, checks Babel's bundle identity and preserves user files:

```sh
sudo /Applications/Babel.app/Contents/Resources/drivers/macos/uninstall.sh
```

Restart macOS, then move Babel.app to Trash. User configuration, credentials,
model cache and recordings are intentionally retained for user-directed cleanup.

## Signed distribution

Obtain Developer ID Application and Developer ID Installer certificates through
the Apple Developer Program and configure them in the build Mac's keychain.
First rebuild/sign the driver with `native/macos/build.py --sign-identity ...
--installer-identity ... --arch universal --test --pkg`, then package the app:

```sh
python3 packaging/macos/build.py --arch universal \
  --runtime-dir artifacts/local-runtime \
  --bin-dir packaging/macos/universal-bin --driver-dir native/macos/dist \
  --sign-identity 'Developer ID Application: YOUR ORGANIZATION (TEAMID)' \
  --installer-identity 'Developer ID Installer: YOUR ORGANIZATION (TEAMID)' \
  --notary-profile YOUR_EXISTING_KEYCHAIN_PROFILE
```

The release path rejects an ad-hoc driver and requires a signed driver package.
It signs the auxiliary CLI before the app bundle, enables hardened runtime and
timestamps, signs the outer installer, and optionally submits only that final
installer to Apple's notary service and staples the ticket. There are no
embedded certificates, passwords, bypass entitlements or CI secrets. Without
`--notary-profile` the signed package is **not claimed to be notarized**. The
loose output app is not separately submitted/stapled; distribute the completed
notarized installer, not an arbitrary zip of that app.

Apple references:

- [Packaging Mac software for distribution](https://developer.apple.com/documentation/xcode/packaging-mac-software-for-distribution).
- [Distribution XML reference](https://developer.apple.com/library/archive/documentation/DeveloperTools/Reference/DistributionDefinitionRef/Chapters/Distribution_XML_Ref.html).
- [Notarizing macOS software](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution).
- [Audio input entitlement](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.security.device.audio-input).
