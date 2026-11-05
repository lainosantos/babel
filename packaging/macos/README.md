# macOS application and driver installer

`build.py` packages the prebuilt `babel` and `babel-tray`
binaries into `Babel.app` and a universal `.pkg`, with checksums and a manifest.
The default installer contains the application and local inference engines.
Follow [macOS routing](../../docs/platforms.md#macos) to install BlackHole.
Pass `--driver-dir native/macos/dist` to include the optional Babel HAL driver
and its original installer scripts.

Run packaging on macOS with Python 3.11+ and the Xcode command-line tools.
The application requires macOS 14.2 or later. Both Mach-O executables must
contain the requested architectures and use system libraries.

## Development and CI

From the repository root, build the application slices with a 14.2 deployment
target, combine them, then package the app:

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
python3 packaging/macos/build.py --unsigned --arch universal \
  --runtime-dir artifacts/local-runtime \
  --bin-dir packaging/macos/universal-bin \
  --output packaging/macos/dist --version 0.1.0
```

Build `scripts/build_local_runtime.py --output artifacts/local-runtime` on each
native Mac architecture, or extract the two runtime artifacts produced by CI.
The universal app contains both `macos-aarch64` and `macos-x86_64` directories;
they are separate native engines, not binaries requiring Rosetta. The builder
needs CMake 3.26+, Git and a C++17 compiler; users need none of those tools.

Omit `--version` to read the root Cargo.toml package version. Numeric `X.Y.Z`
versions are accepted. `--arch arm64` or `--arch x86_64` can package a single
architecture if the application binaries and any included driver use that
architecture. The packager uses prebuilt inputs.

The script inspects both slices using `lipo` and `otool`, stages the application,
ad-hoc signs it, verifies the code signature, builds the installer,
and uses `pkgutil --expand-full` to verify identities, destinations, and binaries.
The package is inspected without running its installer or starting the app.
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

The installer writes `/Applications/Babel.app` and uses the system
authorization prompt. An optional driver package also writes
`/Library/Audio/Plug-Ins/HAL/BabelAudio.driver` and requests a restart.
The installer asks the user to close a running app before upgrading.

`CFBundleExecutable` is the real `babel-tray` Mach-O, with `LSUIElement=true`.
There is no shell launcher or invented `--serve` option. Microphone-purpose
strings are included in English and Portuguese. Hardened-runtime signing uses
only the audio-input entitlement; the application is not App Sandbox enabled.
The app uses `~/Library/Application Support/Babel/babel.toml` by default and
requests a dynamic dashboard port. It does not write beside its executable.

The bundle includes:

- `Contents/MacOS/{babel,babel-tray}`;
- the repository's `docs/*.md` guides;
- driver package, removal helper, and licenses when `--driver-dir` is supplied;
- `Contents/Resources/local-runtime/macos-{aarch64,x86_64}` with Whisper,
  llama.cpp, Piper, ONNX Runtime, eSpeak data and their licenses. CPU backends
  and Metal are included; no Homebrew or separately installed AI daemon is used;
- complete corresponding Piper/eSpeak source archives, patches and build
  instructions in each runtime's `sources/` directory.

The translation and transcription engines start through Babel when selected.
Weights are verified and downloaded by the application on first use, outside
its immutable signed bundle. Native executables and libraries are signed before
the outer app, and payload hashes are regenerated after signing. Users can
install and run Babel without Python. The bundle excludes credentials, user
configuration and recordings.

Disable login startup and quit Babel before removal. If you installed the
optional Babel driver, its removal
command is explicit, checks Babel's bundle identity and preserves user files:

```sh
sudo /Applications/Babel.app/Contents/Resources/drivers/macos/uninstall.sh
```

Restart macOS, then move Babel.app to Trash. User configuration, credentials,
model cache and recordings are intentionally retained for user-directed cleanup.

## Signed distribution

Obtain Developer ID Application and Developer ID Installer certificates through
the Apple Developer Program and configure them in the build Mac's keychain.
Package the app with your signing identities:

```sh
python3 packaging/macos/build.py --arch universal \
  --runtime-dir artifacts/local-runtime \
  --bin-dir packaging/macos/universal-bin \
  --sign-identity 'Developer ID Application: YOUR ORGANIZATION (TEAMID)' \
  --installer-identity 'Developer ID Installer: YOUR ORGANIZATION (TEAMID)' \
  --notary-profile YOUR_EXISTING_KEYCHAIN_PROFILE
```

For a combined package, build and sign the optional driver separately with
`native/macos/build.py` and pass its output through `--driver-dir`.
The signed app build signs the auxiliary CLI before the app bundle, enables hardened runtime and
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
