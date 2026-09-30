# Babel drivers for macOS and Windows

Babel's own drivers live in `native/macos` and `native/windows`. They create
selectable devices; the Rust application remains responsible for physical audio
capture, translation, transcription, recording, MCP, the dashboard and the tray.
The drivers' audio path contains no AI, networking, API keys or file recording.

**Status:** source code, build and installation are separate for each platform.
This does not mean a package has been signed, loaded and validated on hardware.
The Linux development host can check portable code and Rust cross-compilation
for Windows; SDK/WDK builds, signing and native execution require their own
validation. Do not distribute the driver as certified before completing these steps.

## Boundary between Rust and platform-specific code

| Component | Implementation | Reason |
|---|---|---|
| Application, AI, routing and settings | Rust with `forbid(unsafe_code)` | Does not need to run in the driver or be rewritten. |
| Both drivers' buffers | Rust; fixed storage and portable tests | Avoids allocation per callback and controls bounds, silence and drops. |
| macOS AudioServerPlugIn | Rust, with a C bridge compiled against the Apple SDK | The bridge defines the vtable, structures and CoreFoundation objects according to the SDK, without manually duplicating complex layouts. |
| Windows WaveRT/PortCls | C++ adaptation of Microsoft's SimpleAudioSample + Rust `no_std` transport | The WDK provides this integration through C++/COM interfaces, DMA and IRQL. C++ remains at this boundary. |
| Windows installer | Rust + an isolated SetupAPI module | Creates the ROOT instance, verifies the package and removes only the Babel installation. Does not depend on DevCon. |
| Packaging | Python, PowerShell and build shell scripts | Orchestrate Rust and the official SDK tools; do not process audio. |

FFI calls that use pointers are explicitly isolated and documented. The safe
core does not make the HAL, kernel or C/C++ code entirely memory safe. The
Windows driver, in particular, must pass Driver Verifier before distribution.
A defect at this boundary can compromise the audio session or the system;
a Rust check on Linux does not demonstrate WDK behavior.

## Devices and routing

| Use | macOS | Windows |
|---|---|---|
| Babel plays the processed microphone | **Babel Microphone** output | **Babel Microphone Feed** |
| Call application captures | **Babel Microphone** input | **Babel Microphone** |
| Call application plays audio | **Babel Speaker** output | **Babel Speaker** |
| Babel captures the call's original output | **Babel Speaker** input | **Babel Speaker Monitor** |

On macOS each device is duplex, with input and output. Its UIDs are
`org.babel.audio.microphone.v1` and `org.babel.audio.speaker.v1`. On Windows the
adapter is `ROOT\BabelAudio`, interface **Babel Audio v1**, with four endpoints.
Enumeration persists native IDs; Windows identification uses the driver and
interface descriptions, not the editable friendly name. Missing or ambiguous
pairs keep routing closed.

The transport format is stereo at 48 kHz: `f32` in the HAL and PCM16 in WaveRT.
The audio system may convert application formats. Babel's AI layer continues
working in each provider's format, outside the driver. The HAL uses a
conservative margin of 4,096 frames (85.33 ms) to tolerate reads before writes
within a cycle. This fixed latency adds to translation latency; reducing it
requires validating scheduling on macOS. WaveRT retains up to 4,096 queued
frames, without deliberately introducing the same fixed margin. The driver
transports each cable locally; it does not connect the cables to one another
or open physical devices. When the Babel process is closed, the virtual
devices remain installed, but no AI/routing service runs behind them.

## Build, package and installation

[GitHub CI](ci-installers.md) compiles the drivers on hosted runners and
produces complete application installers for each operating system, including
the corresponding driver files and content verification. macOS/Windows CI
packages are labeled as development builds without distribution signing.

See the complete commands and artifacts in:

- `native/macos/README.md`: Rust workspace, SDK-compiled bridge,
  `BabelAudio.driver` bundle, `BabelAudio.pkg` package, signing and removal.
- `native/windows/README.md`: Microsoft revision pinned and verified by hashes,
  Visual Studio/WDK, Rust transport, INF/CAT/SYS and Rust x64/ARM64 installer.

The build does not install drivers. The application does not download a
third-party driver, change default devices, install certificates or modify boot
policy. The driver package requires the operating system's normal administrative
authorization. On macOS, the HAL bundle goes in `/Library/Audio/Plug-Ins/HAL`.
On Windows, the installer uses SetupAPI to create the ROOT instance; merely
adding an INF with PnPUtil does not create that instance.

The Windows installer accepts `install --inf <absolute path to BabelAudio.inf>`,
`remove --inf <same path>` and `list`. It checks the MEDIA class, manufacturer,
service, architecture, hardware ID and catalog signature. An update preserves
a newer driver instead of forcing a downgrade. If installing a new instance
fails, the installer attempts to remove that instance. Removal verifies the
installed identity and uses the OEM name reported by Windows, never wildcards.
An in-use package may require a restart and another removal attempt with the
same INF.

To bundle the packages with the application, use `drivers/macos` or
`drivers/windows` beside the executable. A macOS app bundle can also use
`Contents/Resources/drivers/macos`. `babel setup` reports the package it found
or explains how to prepare it; it does not report installation success without
installing it. `babel uninstall` identifies the corresponding helper. The
interface provides the current operating system's installation guide and lets
you refresh the device list.

A public Windows release must comply with Microsoft's driver-signing policy.
The macOS distribution package requires Developer ID signing and the applicable
notarization process. Certificates, developer accounts and vendor approvals
are not generated by the project code.
Official sources: [Windows signing policy](https://learn.microsoft.com/en-us/windows-hardware/drivers/install/kernel-mode-code-signing-policy--windows-vista-and-later-),
[macOS signing](https://developer.apple.com/documentation/xcode/creating-distribution-signed-code-for-the-mac/),
[notarization](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow).

## Verifying the installed transport

The Rust example `examples/native_driver_smoke.rs` opens only explicit IDs,
requires original Babel names and the `--confirm-virtual-devices` option.
Close Babel and other audio applications before using it, to avoid mixing
external audio or activating physical routing. Do not select real microphones
or speakers.

1. Build on the target operating system with `cargo build --release --example native_driver_smoke --locked`.
2. Run `babel devices` and copy all four IDs, preserving `input:`/`output:`.
3. Run the test with the exact IDs in quotes:

```text
native_driver_smoke --confirm-virtual-devices \
  --microphone-render "output:<microphone playback endpoint ID>" \
  --microphone-capture "input:<microphone capture endpoint ID>" \
  --speaker-render "output:<speaker playback endpoint ID>" \
  --speaker-capture "input:<speaker capture endpoint ID>"
```

In PowerShell, use a single line or backticks for line continuation, and append
`.exe` to the executable. The test sends four distinct synthetic tones, one per
channel, for three seconds. It checks the expected signal, channel order,
separation between cables, callback errors and queue saturation. It prints a
JSON report and closes the streams. It does not record PCM or use AI providers.
A passing result covers transport and isolation for that run; latency,
permissions, interruptions, use by multiple applications and long-term stability
still require the scenarios in `docs/testing.md`.

## References and licenses

Original Babel code and its Rust cores use MIT. The Windows adaptation uses
the official SimpleAudioSample under **MS-PL**; its license and each file's
revision/hash are included in `native/windows`. It does not incorporate
VB-CABLE or BlackHole. Those drivers can still be installed separately as
alternatives under their own licenses. Linux continues to use
PulseAudio/PipeWire-pulse.

- [Apple: creating an AudioServerPlugIn](https://developer.apple.com/documentation/coreaudio/creating-an-audio-server-driver-plug-in).
- [Microsoft: SimpleAudioSample](https://github.com/microsoft/Windows-driver-samples/tree/main/audio/simpleaudiosample).
- [Microsoft: audio miniports](https://learn.microsoft.com/en-us/windows-hardware/drivers/audio/miniport-driver-types-by-operating-system).
- [SetupAPI: verifying an INF against its catalog](https://learn.microsoft.com/en-us/windows/win32/api/setupapi/nf-setupapi-setupverifyinffilew).
