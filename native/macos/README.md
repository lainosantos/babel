# Babel Audio — HAL driver for macOS

This package contains Babel's own `BabelAudio.driver`, a user-space Audio Server
Plug-in. It does not require BlackHole, a kernel extension or DriverKit.
It publishes two independent devices, each with input and output:

| Device | Stable UID | Babel use |
| --- | --- | --- |
| Babel Microphone | `org.babel.audio.microphone.v1` | Write to its output; applications capture its input |
| Babel Speaker | `org.babel.audio.speaker.v1` | Capture its input; applications play audio to its output |

The driver transports original audio or audio produced by the application; it
contains no AI, networking, hardware capture, recording, transcription or
physical-device selection. Without the Babel process routing audio, the virtual
cable remains enumerated but does not forward audio to a real microphone or
speaker on its own. Neither the package nor the uninstaller changes system
default devices.

## Implementation and limits

- Babel Speaker exposes output volume and mute controls in the macOS device
  interface. These controls carry state only: Babel mirrors them to the selected
  physical output, adopting its existing volume on connection. The driver never
  attenuates PCM, so the original recording/transcription capture remains intact
  and translated playback uses the same hardware level. Babel Microphone has no
  added gain controls. Older installed Babel drivers and third-party cables do
  not support this synchronization; upgrade the driver or use the physical
  output control in Babel. Hardware without writable volume/mute controls still
  requires its own hardware controls. Devices without a master volume control
  must expose volume for every output channel (up to 32); channel balance is
  preserved, including when raising the level after zero. Actual macOS hardware
  behavior remains to be validated; the SDK contract test checks property
  behavior in-process.
- Native 32-bit float PCM, interleaved stereo, fixed at 48,000 Hz. Rate/format
  conversion is handled by the HAL and clients; the driver rejects other
  physical or virtual stream configurations.
- Each cable holds 16,384 frames in fixed memory. One IO call is limited to
  **4,096 frames**. Larger requests return an error to the HAL before accessing
  the buffer; they are not partially processed.
- The advertised input latency is **4,096 frames, approximately 85.33 ms**.
  This conservative margin supports `ReadInput` preceding `WriteMix` within a
  cycle with the largest accepted block. It adds to application queues and
  model processing time. The driver does not assume the HAL renders blocks
  ahead of time. Reducing this margin requires demonstrating scheduling on
  macOS hardware; this code does not promise latency of just a few milliseconds.
- A shared clock uses `mach_absolute_time` and the rational conversion of its
  timebase, quantizing timestamps to 512 frames. There is no timer thread.
  The timestamp does not depend on how quickly the consumer reads.
- The buffer is addressed by sample time and has a generation for each usage
  cycle. Multiple readers receive the same frames without consuming a queue.
  Gaps, overwritten data and data from an old generation produce silence.
  Stopping the last client or changing a stream's activity changes the generation.
- `WriteMix` contains audio mixed by the HAL. The driver does not repeatedly
  add client buffers. A nonblocking atomic admission check rejects an
  unexpected concurrent writer; sample processing never waits for a mutex.
- Up to 256 clients per cable. Registration, removal, `StartIO` and `StopIO`
  use a short control mutex to prevent duplicate clients and inconsistent
  counts. Sample processing and the clock do not acquire this mutex.
- The `babel-hal-core` core forbids `unsafe`. The separate `babel-hal-driver`
  crate contains FFI pointers. `abi.c`, compiled with the Apple SDK, declares
  the real HAL vtable, constructs ASBD/layouts and extracts `IOCycleInfo`.
  No private CoreAudio structure layout is reimplemented in Rust.
- Audio callbacks do not allocate, log, access the network or access the
  filesystem. Their loops have fixed bounds. Production profiles abort on
  panic to avoid unwinding through C; IO paths validate sizes, IDs and null
  pointers, and contain no deliberately panicking operations. A dependency/ABI
  error can still crash the process hosting the plug-in. This does not claim
  that CoreAudio or the C bridge is memory safe.

The ABI follows the public contract and was checked against the official
[Creating an Audio Server Driver Plug-in](https://developer.apple.com/documentation/coreaudio/creating-an-audio-server-driver-plug-in)
example. The license notice for the consulted example is in
[LICENSE-Apple-example.txt](LICENSE-Apple-example.txt). This package's bridge
and core implement their own loopback; Apple's NullAudio example only produces
silence and discards output. Original Babel code uses [MIT](LICENSE-MIT.txt);
both notices accompany the generated bundle.

## Building on macOS

Requirements: Rust with Cargo, Python 3.9+, Xcode or Command Line Tools selected
through `xcode-select`, the macOS SDK, `codesign`, `lipo` and `pkgbuild`. The
bundle's minimum target is macOS 11.0. The Babel application may require a later
version for other features; this minimum does not lower application requirements.

For development/CI, including an in-process ABI test and package creation:

```bash
rustup target add aarch64-apple-darwin x86_64-apple-darwin
python3 native/macos/build.py --unsigned --arch universal --test --pkg
```

`--unsigned` is an explicit development choice: the bundle receives an ad-hoc
signature and the `.pkg` has no distribution certificate. This enables artifact
verification in CI, but does not guarantee that a user's macOS installation
will accept loading it as a driver. Do not disable SIP, Gatekeeper or other
system controls to distribute this build.

`--arch native`, `--arch arm64` and `--arch x86_64` are also supported. To run
with `--test`, the bundle must contain the current Mac's architecture. Both
slices are built separately with the SDK and combined by `lipo`; the test only
runs the slice matching the runner's hardware.

Default outputs:

```text
native/macos/dist/BabelAudio.driver
native/macos/dist/BabelAudio-0.1.0.pkg
native/macos/dist/BabelAudio.pkg
native/macos/dist/uninstall.sh
```

`BabelAudio.pkg` is a stable copy for distribution with the application, for
example as `drivers/macos/BabelAudio.pkg`. Also distribute the uninstaller as
`drivers/macos/uninstall.sh`. The build uses this directory's isolated Cargo
workspace; it does not change the application's manifest, `forbid(unsafe_code)`
policy or binaries. It does not automatically install targets/toolkits, use
`sudo`, install the package or restart CoreAudio.

## Signing, notarizing and distributing

For distribution, obtain **Developer ID Application** and **Developer ID
Installer** identities from your Apple Developer account and keep them in the
build machine's keychain. Credentials and certificates do not belong in the
repository. After configuring a `notarytool` credential profile:

```bash
python3 native/macos/build.py --arch universal --test --pkg \
  --sign-identity 'Developer ID Application: YOUR ORGANIZATION (TEAMID)' \
  --installer-identity 'Developer ID Installer: YOUR ORGANIZATION (TEAMID)' \
  --notary-profile 'babel-notary'
```

The script verifies the signature and exported factory symbol, signs the
package, explicitly submits it to Apple, waits for the result, and staples
and validates the ticket. No upload occurs without `--notary-profile`.
Without distribution credentials and successful completion of these steps,
the artifact is not declared ready for delivery. The HAL driver does not
request AudioDriverKit entitlements, which belong to a different driver model.
See Apple's official
[packaging](https://developer.apple.com/documentation/xcode/packaging-mac-software-for-distribution)
and [notarization](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution)
instructions.

## Installing and removing

Open the `.pkg` in the macOS Installer and authorize installation as an
administrator. The fixed destination is
`/Library/Audio/Plug-Ins/HAL/BabelAudio.driver`. The package prevents relocation,
checks the identity of a previous installation, rejects symlinks at expected
paths and assigns files to `root:wheel`. **Restart macOS** to load the driver
and check the devices in Audio MIDI Setup. The installer does not automatically
interrupt an ongoing call.

To remove it, close Babel and applications using its devices, then explicitly run:

```bash
sudo sh native/macos/uninstall.sh
```

The script requires privileges already granted, verifies the fixed path and
bundle ID, and removes only the Babel driver and its installation receipt.
Restart macOS to unload the copy CoreAudio still holds in memory. None of
these scripts restarts `coreaudiod`, changes default devices or removes other
audio plug-ins.

## Verification without installation or hardware

On Linux/macOS/Windows, portable workspace tests exercise the core, properties
and the private Rust/C contract:

```bash
cargo test --manifest-path native/macos/Cargo.toml --all-targets --locked
cargo clippy --manifest-path native/macos/Cargo.toml --all-targets --locked -- -D warnings
```

On macOS, `build.py --test` also compiles the shim against the SDK and uses
`CFPlugInCreate`/`CFPlugInInstanceCreate` to load the bundle in the test process,
without installing it. It exercises enumeration, UIDs, formats, the clock,
actual loopback through memory buffers, repeated readers, cable isolation,
rejection of oversized buffers and absence of replay after stopping. It does
not use the physical microphone or the host's audio defaults.

**Status of this delivery:** Rust tests passed on the Linux host. SDK compilation,
signing, installation and real audio on macOS hardware still need to be run;
CI files do not prove that those steps have already passed.

Before releasing an installer, validate on Intel and Apple Silicon Macs:

1. Install the signed/notarized package, restart and verify both UIDs and
   streams in both directions. Confirm that physical devices and previous
   defaults were preserved.
2. Play a known signal into Babel Speaker and capture it from the corresponding
   input with two simultaneous readers. Check channels, content, absence of
   data from the other cable and measured delay against 4,096 frames.
3. Repeat on the Babel Microphone cable with blocks of 64, 128, 256, 512, 1,024,
   2,048 and 4,096 frames; use a test client that varies IO size and order.
4. Repeatedly start/stop clients and change the destination to a physical device
   during the session. Confirm initial silence, absence of replay and the
   application's return to routing only virtual devices that are actually in use.
5. Test sustained load, suspend/resume, application shutdown and package removal.
   The contract test does not replace validation against the real HAL host.
