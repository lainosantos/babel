# Babel Virtual Audio — Windows

The driver source implements two independent render → capture cables. It is a
Babel driver, not a wrapper around VB-CABLE. The byte transport is Rust; the
Windows audio integration uses Microsoft's PortCls/WaveRT interfaces through a
pinned adaptation of the official Simple Audio Sample.

**Validation status:** portable Rust/ABI tests, deterministic source generation
and the C++ shim host harness pass on Linux. The `.sys` has **not** been compiled
with the WDK, signed, installed or exercised on Windows in this environment.
A source implementation and a reproducible build are provided; this directory
does not contain a ready-to-install signed production package. WDK/INF verification,
Windows playback/capture tests, Driver Verifier, and signing are release gates.

## Devices and application mapping

| Windows direction | Device description | Use |
| --- | --- | --- |
| Render / output | Babel Microphone Feed | Babel writes translated or original microphone audio |
| Capture / input | Babel Microphone | Call application reads its microphone |
| Render / output | Babel Speaker | Call application writes received audio |
| Capture / input | Babel Speaker Monitor | Babel reads received audio before routing/translation |

Both pairs use exactly **48,000 Hz, two channels, signed PCM16 little endian** at
the kernel boundary. In shared WASAPI mode the Windows audio engine converts
application formats and mixes multiple client sessions. Exclusive mode must use
this exact format; the miniport permits one hardware stream per endpoint.

The signed driver package owns one `ROOT\BabelAudio` device, service `BabelAudio`,
MEDIA class `{4d36e96c-e325-11ce-bfc1-08002be10318}`, provider `Babel`. It registers
four persistent KS reference names: `WaveBabelMicRender`, `WaveBabelMicCapture`,
`WaveBabelSpeakerRender`, `WaveBabelSpeakerCapture` and corresponding `Topology...`
references. These names and the root device are retained across updates.

The INF explicitly provides `PKEY_Device_DeviceDesc` with the descriptions above
and `PKEY_DeviceInterface_FriendlyName` = `Babel Audio v1`. The app matches that
pair of properties plus direction; the user's renameable friendly label is not
an identifier. Validate the resulting MMDevice properties on Windows before
release; missing/ambiguous metadata must fail closed. Custom diagnostic endpoint
properties `{76AD3C42-65B7-4DA0-A823-907A8E5AC6C1},1` and `,2` carry the route role
and `org.babel.audio.driver.v1`. The implementation does not invent or overwrite
Windows-generated endpoint GUIDs or parse opaque IMMDevice IDs. A device removal
and reinstallation may require selecting its new persistent OS ID again.

## Why the WDK portion is C++

PortCls exposes WDM/WaveRT COM-style miniport contracts. Microsoft's supported
sample supplies the topology descriptors, KS properties, buffer allocation,
PnP/power integration, notifications and clock positions. Rewriting these ABI
contracts in Rust would require a larger unsafe boundary and independent kernel
validation. Microsoft's `windows-drivers-rs` is still documented as experimental
and has no ready safe PortCls/WaveRT miniport implementation.

`transport/src/core.rs` forbids unsafe code and has no allocation, pointers,
floating-point operations, network, files or OS calls. Its two fixed queues each
hold 4,096 stereo frames (16 KiB, 85.33 ms maximum queued audio). They preserve
PCM bits, output silence on underrun, discard oldest frames on overflow and flush
on either endpoint's RUN/pause/stop transition. Audio is never retained for a
future consumer while one side is inactive. Repeated RUN does not flush audio.

`transport/src/lib.rs` contains the small audited C ABI boundary. It uses volatile
byte accesses to driver-owned DMA memory, never creates Rust references to a
shared user mapping, checks cable indices, frame alignment and chunk bounds, and
never unwinds into C++. `shim/BabelTransport.cpp` serializes each cable using a
WDK spin lock at DISPATCH_LEVEL. Calls are chunked to 1,024 frames, the ring uses
integer atomics, and no audio callback allocates or waits on a user-mode process.
This is bounded soft real time; it is not a promise of a fixed maximum OS scheduling
latency. The sample's simulated hardware clock uses a 1 ms timer while running.

The adaptation removes the tone-generator and disk-file audio sinks. It flushes
transport on state changes, cancels/drains timers before releasing stream memory,
protects notification lists against concurrent DPC access, rejects protected audio,
and limits DMA buffers to 100 ms and event periods to at least 1 ms. Rendering
and capture clocks can start at different times; an initial underrun is silence.
No voice processing or AI runs in the kernel.

## Reproducible build

Use **GitHub Actions `windows-2022`** or a Windows development machine with
Visual Studio 2022 C++ tools, the WDK Visual Studio extension, target architecture
and Spectre libraries, PowerShell 7+, Python 3.9+ and Rust 1.90+.
`build.ps1` discovers VS2022 and enters its developer environment itself.
The target package is x64 or ARM64, Windows 10 build 19041 or later; each supported
Windows version/architecture still requires native validation.

```powershell
.\native\windows\build.ps1 -Architecture x64
# or, from an appropriate ARM64 cross-development environment:
.\native\windows\build.ps1 -Architecture ARM64
```

The build script does not install anything. `prepare.py` downloads only pinned
Microsoft files and verifies every SHA256 in `upstream.json`, or accepts an offline
checkout with `-MicrosoftSourceRoot C:\src\Windows-driver-samples`. For a Git
repository, it reads the original blobs from the fixed commit, not modified
working-tree files. This avoids `.gitattributes` conversions to CRLF/UTF-16;
the hashes pin the UTF-8/LF bytes served by the raw commit URLs. An exported
directory must contain those exact original bytes. The exact revision
is `2dc3fd3a0cc84a2933f2194e7ec0871584979071`. It then applies asserted transformations,
builds the Rust static library, links it into the WDK driver, validates/generates
the INF/catalog, and builds the separate Rust SetupAPI installer. No dependency
on a mutable branch or unverified vendor binary is used.

`setup-wdk.ps1` restores official NuGet **SDK and WDK 10.0.26100.6584** archives
from `api.nuget.org`, validates every SHA-256 in `wdk-packages.json`, and extracts
them into a build cache. It does not resolve floating NuGet dependency ranges.
Generated `Directory.Build.props` imports their property files using the same
order as Microsoft's sample repository. The kit contents version is
`10.0.26100.0`; the target remains Windows 10 2004 with KMDF 1.31. The script
checks the actual VS toolset, compiler, Spectre libraries, WDK headers/libraries
and catalog tools, so a missing component fails before compiling.

The hosted runner supplies the VS2022 WDK extension; headers, libraries and tools
come from the pinned packages. This avoids a self-hosted runner and avoids
depending on the SDK/WDK content version preinstalled in the image. Microsoft
currently identifies 26100.6584 as the supported WDK series for VS2022. The VS
compiler and extension still follow the hosted image: their paths and kit version
are recorded in `toolchain.json`, and native execution remains the CI verification.

Example workflow commands, both using `shell: pwsh`:

```powershell
# Optional separate setup step; exports cache/tool locations through GITHUB_ENV.
.\.github\scripts\setup-wdk.ps1 -Architecture x64
# Also performs/validates setup, so it works without the separate step.
.\native\windows\build.ps1 -Architecture x64
```

Use `ARM64` in both commands to cross-compile from the x64 hosted runner. SDK and
WDK archives for both the host and target are restored in that case. A custom
cache can be configured with `native/windows/setup-wdk.ps1 -PackagesDirectory
<absolute-path>` before building. No machine-wide SDK/WDK installation is needed.
Do not cache incomplete extracted directories; the setup detects missing markers
and asks for a fresh cache rather than quietly trusting a partial extraction.

The output is `native/windows/dist/x64/` or `dist/ARM64/` containing:

- `BabelAudio.sys`, `BabelAudio.inf`, `BabelAudio.cat`;
- `babel-driver-installer.exe`, `install.ps1`, `uninstall.ps1`;
- license and documentation files.
- `toolchain.json` and `SHA256SUMS` for the complete unsigned package.

The GitHub build step exports `package_path` and `build_path` outputs. Preserve
`native/windows/build/**/wdk-build.binlog` even on failure; it includes compiler,
linker, stamping, INF validation and catalog tasks. The finished package is still
**unsigned**: the CI build must not advertise it as a signed production driver.

A new build uses a unique working directory and refuses to overwrite an existing
published `dist/<architecture>` package. Archive old output explicitly before
rebuilding. Distribute the **complete** signed package as `drivers/windows/` next
to Babel's executable; do not mix files from different versions/architectures.

## Signing and explicit installation

Production kernel drivers require the applicable Microsoft Hardware Dev Center
signing process. For development, use a properly prepared isolated test machine
with a trusted test certificate and the Windows test-signing policy required by
Microsoft. These scripts do not enable test signing, disable Secure Boot, install
certificates or bypass policy. Signing credentials belong to the publisher and
are not created, bundled or guessed here.

After signing and verifying the complete package, explicitly open an
**Administrator PowerShell** in its folder:

```powershell
.\install.ps1
# Read-only inventory (does not require elevation):
.\babel-driver-installer.exe list
# Read-only app-uninstaller guard; nonzero while any Babel devnode remains:
.\babel-driver-installer.exe check-absent
# Explicit removal, closing applications that use Babel audio first:
.\uninstall.ps1
```

The Rust helper validates the package identity/signature, creates the exact root
devnode through SetupAPI, and installs/updates it. `pnputil /add-driver /install`
alone cannot create a new root devnode, so the installer does not rely on that
shortcut or require DevCon. Removal is scoped to the Babel hardware ID, service,
provider and associated OEM package. Reboot requirements are reported rather than
acted on automatically. The main Babel application never elevates itself.

## Verification

Portable tests, without installing a device or touching real audio:

```sh
CARGO_INCREMENTAL=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --manifest-path native/windows/transport/Cargo.toml --lib --features kernel
python3 -m unittest discover -s native/windows/tests -v
# Include the generated-source checks using the pinned official checkout:
BABEL_WDK_SOURCE=/path/to/Windows-driver-samples python3 -m unittest discover -s native/windows/tests -v
python3 native/windows/tests/test_shim.py
```

The host harness compiles the **actual** Rust static library and C++ shim, with
host mutex stand-ins for the WDK spin-lock APIs. It verifies two-cable isolation,
bit-exact transfer across chunks, stale-audio flushing and concurrent write/read/
pause calls. It does not validate Windows ABI headers, IRQL, the scheduler or
kernel lifetime rules. Never treat it as a successful driver install.

On a disposable Windows test target, validate all four endpoint properties,
shared WASAPI full duplex, two simultaneous different test signals with no
cross-talk, channel order, PCM amplitude, underrun silence, pause/restart without
old audio, app-only speaker and mic use detection, suspend/resume, rapid
open/close, driver removal, and device renaming. Run Driver Verifier with the
WDK debugger and the relevant HLK audio tests. Measure loop latency, DPC duration,
CPU and memory at multiple periods under load. Confirm normal physical devices
and defaults are unchanged. Repeat on x64/ARM64 and every supported OS release.

## Sources and licensing

- [Microsoft Simple Audio Sample](https://github.com/microsoft/Windows-driver-samples/tree/2dc3fd3a0cc84a2933f2194e7ec0871584979071/audio/simpleaudiosample)
- [SYSVAD and WaveRT samples](https://learn.microsoft.com/en-us/windows-hardware/drivers/audio/sample-audio-drivers)
- [PortCls API](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/portcls/)
- [windows-drivers-rs status](https://github.com/microsoft/windows-drivers-rs)
- [Audio endpoint properties](https://learn.microsoft.com/en-us/windows/win32/coreaudio/audio-endpoint-properties)
- [Driver signing](https://learn.microsoft.com/en-us/windows-hardware/drivers/install/driver-signing)
- [Supported WDK/VS versions](https://learn.microsoft.com/en-us/windows-hardware/drivers/other-wdk-downloads)
- [WDK NuGet distribution](https://learn.microsoft.com/en-us/windows-hardware/drivers/install-the-wdk-using-nuget)
- [Hosted windows-2022 components](https://github.com/actions/runner-images/blob/main/images/windows/Windows2022-Readme.md)

The Microsoft sample and its generated modifications are **MS-PL**, including
`BabelAudio.inx`; the full license is `LICENSE-Microsoft.txt`. The new Rust transport,
WDK ABI shim, preparation/build scripts and host tests are MIT (see `LICENSE-MIT.txt`).
The original Microsoft notices remain in generated files. No VB-CABLE or
BlackHole source/binary is included.
