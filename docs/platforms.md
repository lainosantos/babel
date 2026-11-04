# Virtual devices and routing by operating system

The translation process runs in Rust in user space. On Linux, Babel creates
selectable devices through the existing audio server. On macOS and Windows,
Babel connects to separately installed virtual devices through CoreAudio and
WASAPI. BlackHole on macOS and Virtual Cables on Windows x64 provide two paths
at no cost and without an evaluation period. Babel's own drivers remain
available for [native driver builds](native-drivers.md).

The dashboard and tray use the operating system running Babel, regardless of
where the browser is open. Use **Refresh devices** after installing an external
audio driver. Installers and device setup require separate user authorization.

The device help opened at `/help/platforms` contains only instructions for the
current system. The full version is available at `/help/platforms/all` and in
this document. The native driver build and packaging guide opens at
`/help/native-drivers`. This lets users consult another system explicitly without
mixing its setup steps into their current configuration.

The two paths must remain independent:

```text
Physical microphone → Babel capture → translation → cable M → call microphone
Call output → cable S → Babel capture → translation → physical headphones/speaker
```

Select explicit physical devices in Babel before starting. Do not use the same
cable in both directions: that can feed translated output back into translation.
Headphones reduce acoustic feedback between speakers and the real microphone.
Babel does not change the system's global default devices.

Voice commands have a status overlay that works even with settings closed. It
uses a discreet native window without taking focus on Windows, macOS, and Linux
with X11/XWayland. Wayland without XWayland and window creation failures fall
back to system notifications. **Show desktop notifications**, in Commands,
controls this feedback; notifications never activate capture or execute tools.
The graphical helper ships with the packages and starts on demand. See
[command visual feedback](voice-commands.md#visual-command-feedback) for states,
reduced motion, privacy, and desktop limitations.

## Linux: PipeWire with pipewire-pulse or PulseAudio

<a id="linux"></a>

Runtime dependencies: `pactl`, `parec`, and `pacat`, usually provided by
`pulseaudio-utils` on Debian/Ubuntu or the distribution's PulseAudio tools
package. The server must be accessible in the user's session; do not run Babel
with `sudo`. Plain ALSA without PulseAudio/pipewire-pulse is insufficient for
this backend.

The Linux tray uses StatusNotifier/AppIndicator. If the desktop service is
unavailable during login or screen lock, Babel keeps the dashboard and audio
independent of the tray and retries registration every three seconds. The icon
appears when desktop support becomes available, without restarting Babel. GNOME
requires installed and enabled AppIndicator support. Babel does not unlock the
screen or change extensions automatically.

The **create virtual devices** action loads modules into the running server:

| ID | Type | Use |
|---|---|---|
| `babel_microphone` | Selectable input, “Babel_Microphone” | Microphone in Zoom/Meet/Discord/etc. |
| `babel_mic_bus` | Internal output, “Babel_Microphone_Bus” | Playback destination for Babel's microphone route |
| `babel_speaker` | Selectable output, “Babel_Speaker” | Speaker in the call application |
| `babel_speaker.monitor` | Monitor input | Capture source for Babel's output route |

`babel_mic_bus` is a mono `module-null-sink`; `babel_microphone` is a
`module-remap-source` connected to that sink's monitor. `babel_speaker` is a
separate stereo `module-null-sink`. Original routing uses float PCM at the
selected route format. Audio copies sent for speech processing are converted
separately to the model's required format, including mono where needed.

Creation is idempotent and recovers partially created modules. An ownership
marker identifies Babel's modules; removal revalidates the marker, type, and
name before unloading each module. Third-party devices with the same names
cause an error instead of being replaced. Modules belong to the current audio
server session: recreate them after restarting the server. Ending translation
does not remove devices or break the call application's device selection.

If devices do not appear immediately in a browser or application, refresh its
device list or reopen its audio settings. Select the virtual output in the
specific application. Applications that only accept the default output require
the user to select it manually in the system.

Choosing **Babel_Microphone as the system's default microphone** opens the
microphone route and enables configured voice commands even when no application
is capturing audio. An explicit capture by another application on
`babel_microphone` also opens the route. To pause it, choose the physical
microphone as the default and stop any explicit Babel captures in applications.

The output opens only while an application plays to `babel_speaker`; selecting
it as the default without playing audio is insufficient. Switching the
application's output to the real speaker closes that route; the microphone
remains independent. Routes without these conditions show **Routing inactive**.
Reactivating them resumes audio without ending the session or creating new files.
Changing only the system default does not deactivate applications that explicitly
selected Babel.

On PipeWire, Babel streams use `node.dont-move`, `node.dont-reconnect`, and
`node.dont-fallback` to prevent a default-device change from redirecting them.
Selecting a physical device from the tray still works: Babel closes the previous
stream and opens another at the chosen destination. On plain PulseAudio, the
monitor detects destination drift and suspends the route to reopen it correctly;
PipeWire-specific properties provide no guarantee on PulseAudio. If the server
becomes unreachable, routes suspend and the dashboard displays the error.

Official references: [PulseAudio modules](https://wiki.freedesktop.org/www/Software/PulseAudio/Documentation/User/Modules/),
[PipeWire null sink](https://docs.pipewire.org/page_pulse_module_null_sink.html),
and [PipeWire remap source](https://docs.pipewire.org/page_pulse_module_remap_source.html).
Linking properties are described in the
[official WirePlumber policy](https://pipewire.pages.freedesktop.org/wireplumber/policies/linking.html).

## macOS: BlackHole or Babel virtual devices

<a id="macos"></a>

### BlackHole setup

1. Install both **BlackHole 2ch** and **BlackHole 16ch** from the
   [BlackHole project](https://github.com/ExistentialAudio/BlackHole#installation-instructions).
   Follow its restart instructions. Babel uses the devices exposed by CoreAudio.
2. Open Babel, grant microphone access, and click **Refresh devices**.
3. Set the virtual devices in Babel and in the calling app as follows.

| Field/application | Device |
|---|---|
| Babel microphone capture | Your physical microphone |
| Babel translated microphone playback | **BlackHole 2ch** output |
| Call application microphone | **BlackHole 2ch** input |
| Call application speaker | **BlackHole 16ch** output |
| Babel output capture for translation | **BlackHole 16ch** input |
| Babel translated output playback | Your physical headphones/speaker |

Select the two devices directly. Babel uses channels 1/2. An app that cannot
select BlackHole 16ch needs another independent compatible loopback for the
speaker path. Test original audio in both directions before enabling translation.

On macOS 14.2 and later, the app monitors the selected devices through CoreAudio.
When opening a trusted Babel download without notarization, macOS may offer
**Open Anyway** in **System Settings → Privacy & Security** after the first
launch or installation attempt. Managed Macs may restrict the option.
See [Apple's Gatekeeper instructions](https://support.apple.com/en-us/102445).

### Optional Babel Audio driver on macOS

**BabelAudio.pkg** installs Babel's AudioServerPlugIn with two separate paths:
**Babel Microphone** and **Babel Speaker**. Each device has an input and an output
side. Source and build instructions are in
[native/macos](../native/macos/README.md); the distribution places
`drivers/macos/BabelAudio.pkg` beside the application. Source and a build path
are available, but this repository does not currently provide a signed package
validated on hardware. See [driver preparation](native-drivers.md).

Babel's usage monitor requires **macOS 14.2 or later**. Install the package
explicitly and follow the system authorization and package restart instructions.
The dashboard does not run installers or request elevation. Devices appear in
Sound settings and Audio MIDI Setup, not as applications in the Applications
folder. Use **Refresh devices** after installation.

Authorize microphone access for the application or terminal running Babel under
System Settings → Privacy & Security → Microphone. Capture availability depends
on that permission. Reopen the application if macOS requests it.

| Field/application | Device |
|---|---|
| Babel microphone capture | Your physical microphone |
| Babel translated microphone playback | **Babel Microphone** output |
| Call application microphone | **Babel Microphone** input |
| Call application speaker | **Babel Speaker** output |
| Babel output capture for translation | **Babel Speaker** input |
| Babel translated output playback | Your physical headphones/speaker |

Fixed UIDs are `org.babel.audio.microphone.v1` and
`org.babel.audio.speaker.v1`. Selection uses device UID and direction; renaming
a visible description does not create another cable. Use the IDs listed by
Babel. The native devices offer stereo at 48 kHz; speech processing converts a
separate copy to the translation format. No Aggregate or Multi-Output device
is required.

The CoreAudio monitor queries external processes using the **selected UID**,
distinguishes capture from playback, and excludes Babel itself. Choosing Babel
Microphone as the default macOS input opens the physical microphone route and
enables configured voice commands even without another application's capture.
Explicit capture of Babel Microphone also opens the route. Selecting the
physical microphone as default closes it only if no application still uses Babel.

Playback to Babel Speaker opens the route to the headphones; merely selecting
it as default output is insufficient. When activity ends, the route shows
**Routing inactive**, closes its streams, and discards queued audio. The other
direction and session files continue.

Select devices directly in applications: the monitor does not automatically
expand Aggregate/Multi-Output devices. Systems older than 14.2, unavailable APIs,
and query errors keep affected routes closed with a diagnostic. Process queries
are periodic, so suspension is not instantaneous. Compilation and driver core
tests do not replace validation on a real Mac, including permissions and use
by call applications.

Babel driver removal uses the package's `uninstall.sh`, explicitly invoked with
the required authorization. `babel setup` and `babel uninstall` identify the
local package or helper when found; they neither execute those files nor claim
to have installed or removed devices.

Official references: [devices used by a CoreAudio process](https://developer.apple.com/documentation/coreaudio/kaudioprocesspropertydevices)
and [Apple's process API example for macOS 14.2+](https://developer.apple.com/documentation/coreaudio/capturing-system-audio-with-core-audio-taps).
Babel queries processes/devices without creating taps to capture global audio.

## Windows: Virtual Cables or Babel virtual devices

<a id="windows"></a>

### Virtual Cables setup on Windows x64

[Virtual Cables](https://github.com/tarekwasfy01/Virtual-Cables) is a free,
open-source application that presents independent USB audio loopbacks. It uses
the Windows USB audio class driver and the separately distributed USB/IP
transport. The upstream project labels the application experimental and
documents Windows 10/11 x64 support.

1. Install the x64 transport from the
   [official usbip-win2 releases](https://github.com/vadimgrn/usbip-win2/releases).
   Use a release with Microsoft-signed drivers. Version
   [0.9.7.5](https://github.com/vadimgrn/usbip-win2/releases/tag/v.0.9.7.5)
   explicitly documents Windows hardware certification for x64.
2. Install [Virtual Cables](https://github.com/tarekwasfy01/Virtual-Cables/releases).
   Open it, create two cables and leave it running. Use its default local
   USB/IP address. Refresh devices in Babel.
3. Configure the two routes and test original audio before enabling translation.

| Field/application | Device |
|---|---|
| Babel microphone capture | Your physical microphone |
| Babel translated microphone playback | **Virtual Cable 01** playback |
| Call application microphone | **Virtual Cable 01** recording |
| Call application speaker | **Virtual Cable 02** playback |
| Babel output capture for translation | **Virtual Cable 02** recording |
| Babel translated output playback | Your physical headphones/speaker |

Windows may prefix the cable names with the endpoint type. Each cable uses
48 kHz stereo PCM16. Allow desktop microphone access and keep **Listen to
this device** disabled on both cables. If Windows assigns new endpoint IDs
after the cables are recreated, refresh and reselect them in Babel.

Windows 11 on Arm can emulate x64 applications. The USB/IP kernel driver
still requires the signed ARM64 build. The Virtual Cables setup on Arm and
the behavior of its emulated application have yet to be validated.
See [Microsoft's Windows on Arm guide](https://learn.microsoft.com/en-us/windows/arm/faq).

Windows SmartScreen may warn about unsigned Babel downloads. Smart App Control
and managed device policies can block execution.
See [Microsoft's SmartScreen guidance](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation)
and [Smart App Control guidance](https://learn.microsoft.com/en-us/windows/apps/develop/smart-app-control/overview).

### Optional Babel Audio driver on Windows

The **BabelAudio** package supplies four WASAPI endpoints forming two independent
cables. WaveRT driver source and build scripts are in
[native/windows](../native/windows/README.md). The INF targets Windows 10 build
19041 or later. The package must match the system architecture.
**A signed distribution package and real Windows validation of installation,
streams, and removal are still required.** Source code is not a production-approved
installer.

The `drivers/windows` distribution directory beside the Babel executable contains
`BabelAudio.inf`, `BabelAudio.sys`, the catalog, and installation/removal helpers.
Follow the [driver guide](native-drivers.md) to obtain or build the package and
install it explicitly with administrator authorization. The dashboard only shows
instructions and hides the creation/removal actions used by the Linux backend.
`babel setup` and `babel uninstall` identify available files without executing
scripts, elevating permissions, or claiming successful installation.

| Field/application | Device |
|---|---|
| Babel microphone capture | Your physical microphone |
| Babel translated microphone playback | **Babel Microphone Feed** (playback) |
| Call application microphone | **Babel Microphone** (recording) |
| Call application speaker | **Babel Speaker** (playback) |
| Babel output capture for translation | **Babel Speaker Monitor** (recording) |
| Babel translated output playback | Your physical headphones/speaker |

Authorize microphone access for desktop applications in Windows privacy settings.
Babel uses the device's shared-mode configuration through WASAPI. Do not enable
“Listen to this device” on the virtual devices: it creates a second route outside
Babel's control. Use **Refresh devices** after installation and select the
endpoints listed above.

The monitor checks the Windows default microphone and external WASAPI sessions.
Choosing **Babel Microphone** as default opens the microphone route and enables
configured voice commands even without an application capturing audio. Explicit
capture of Babel Microphone also opens the route. Selecting the physical
microphone as default closes it only if no application still uses Babel.

Application playback to **Babel Speaker** opens the output route; choosing it
as default without playback is insufficient. Babel's own sessions are excluded.
Each direction shows **Routing inactive** when its activity condition is unmet,
even if the other direction or session is still active.

Pairing checks the **Babel Audio v1** interface and its endpoint descriptions,
or the numbered USB adapter identity for Virtual Cables. WASAPI endpoint IDs
remain opaque. Missing or ambiguous endpoints and query failures keep the
affected direction closed with a diagnostic. Selecting both sides of the same
cable for both routes blocks both.

The monitor combines periodic enumeration with events from known sessions.
Microsoft notes that enumeration may omit newly created sessions; Babel can
remain waiting until it observes them. This implementation covers shared WASAPI.
Exclusive mode, ASIO, and Kernel Streaming have not been validated. Cross-compiling
the application checks types and APIs but does not prove loaded-driver behavior
on Windows.

**Optional alternative:** two independent VB-CABLE or CABLE-A/B/C/D pairs remain
recognized. With CABLE-A and CABLE-B, replace the four Babel endpoints in the
table with CABLE-A Input, CABLE-A Output, CABLE-B Input, and CABLE-B Output,
respectively. One cable does not provide two independent paths. Obtain drivers
and licenses directly from [VB-Audio](https://vb-audio.com/Cable/) and consult
its [distribution terms](https://vb-audio.com/Services/licensing.htm).
Babel's own driver does not depend on those packages.

Official reference: [WASAPI session enumeration limitations](https://learn.microsoft.com/en-us/windows/win32/api/audiopolicy/nf-audiopolicy-iaudiosessionmanager2-getsessionenumerator).

## Performance and validation limits

Suspension is independent per direction on all three backends. The microphone
requires Babel as system default or a consuming application; output requires
an application playing to it. When activity ends, Babel closes route streams,
discards queues, and stops the corresponding processing. Resuming creates a new
path without replaying old responses. The session name/ID and writers stay the
same. macOS and Windows detection have the requirements and limits above.
Compiling the monitor does not prove behavior with every driver.

- Native callbacks process samples, operate fixed-capacity atomic queues, and
  update counters. They perform no networking, allocation, waiting, mutex
  locking, or logging. Resampling and frame creation run on workers.
- Native resampling uses a 64-tap, 512-phase sinc FIR anti-aliasing filter where
  conversion is required. Original routing preserves the configured device
  format; the translation layer uses mono PCM16 at the provider's configured rate.
- On Linux, two persistent clients per route (`parec`/`pacat`) transport raw PCM.
  No process is launched per frame. Buffers and networking are outside the audio
  server callback; this is a simple integration choice, not the lowest possible
  latency of a native PipeWire implementation.
- Queues are bounded. Saturated capture drops frames and increments a counter
  instead of indefinitely accumulating old speech. Playback uses the application's
  configured limit. Interruptions advance a generation: stale audio is ignored
  even with a full queue. On Linux, playback restarts to clear pending server
  audio; native playback also checks the generation in the callback.
- If output stops consuming samples, writing fails after the queue limit or
  latency, whichever is greater, plus 500 ms, ending the route with an error.
  This also detects a final blocked frame when the provider sends no further
  audio that would otherwise saturate the queue.
- `latency_ms` is a Babel buffer request/limit. Native callback size is negotiated
  by CPAL and the system, not guaranteed by this field. Linux passes it to the
  PulseAudio client. Perceived latency includes network, model, end-of-speech
  detection, translation, and hardware buffering. This repository makes no
  zero-delay simultaneous translation guarantee and supplies no production benchmark.
- Native IDs include direction and persistent device identity: UID on macOS and
  endpoint ID on Windows. Reordering the list does not change selection. Legacy
  index/name configurations are accepted only when the name uniquely identifies
  a device in the correct direction; the old index never selects another device.
- Physical devices can change during a session. If the selected device disconnects,
  Babel discards backlog and retries the same identity every three seconds without
  selecting the system default. Another physical device can be selected while
  waiting. Switching/recovery preserves the session, transcription, writer, and
  provider; pending frames from the previous physical route are discarded. Native
  capture with no callbacks for two seconds triggers recovery; silence with
  callbacks remains valid audio. Physical behavior still needs validation on each OS.
- Cancelling a worker requests native closure but cannot interrupt a call stuck
  inside the OS or driver. Registration by identity and direction reserves an
  endpoint until actual release, preventing another Babel stream from opening
  that endpoint while the previous one persists. Recovery can remain blocked and
  require driver/process recovery; task status does not prove physical closure.
- `underruns` counts native callbacks that inserted silence, including when AI has
  not yet produced speech. PulseAudio does not expose this counter through
  `pacat`, so it remains zero on that backend.

Test the path with translation and transcription disabled, using original audio
routing. Optionally enable recording alone during a session to inspect the mixed
WAV; this needs no AI provider. Cross-compilation confirms types and APIs but does
not replace device, permission, suspension/resumption, and disconnection tests
on real macOS and Windows machines.

### Speaker volume

Supported Babel speaker endpoints expose one output volume control: changing
Babel's volume in the operating system changes the selected physical speaker's
master volume and mute. Connecting or switching speakers adopts the new physical
device's current level. Babel does not force hardware volume to 100%. Changes
made with a physical volume knob are reflected back to the virtual control.
Original and translated playback therefore share the same physical master.
Microphone gain is independent and is not changed by this feature.

The controller observes volume outside the audio executor; PCM callbacks never
query or set system volume. The source PCM is not multiplied by this control,
so transcription and recording still receive original audio. Muting playback
does not pause original capture, transcription or recording.

- **Linux with PipeWire:** the Babel-owned speaker monitor uses
  `monitor.channel-volumes=false`. Older virtual devices require an explicit
  reinstall after sessions finish and apps release the Babel devices. Reinstall
  adopts the physical speaker's current level; check that level before resuming
  playback, because the old virtual attenuation is removed. Existing devices
  are not silently replaced while apps use them.
- **Native PulseAudio and third-party virtual cables:** their monitor can
  already contain virtual attenuation, so automatic mirroring is disabled to
  avoid applying volume twice. The **Speaker volume** control in Audio routing
  adjusts the physical device directly, including during a session. Set the
  virtual control to 100% if you want only the physical control to attenuate
  playback. PulseAudio amplification above 100% also disables automatic mirroring.
- **macOS:** requires the updated Babel Audio driver with control-only speaker
  volume and mute properties and a physical endpoint with writable controls.
  Read-only digital outputs and older drivers can require hardware volume keys.
- **Windows:** requires the updated Babel driver advertising the control-only
  volume capability. Older Babel drivers and VB-CABLE keep the separate physical
  control. This native path requires validation on Windows hardware; compiling
  it is not an end-to-end driver test.

The routing page shows synchronization failures and unsupported endpoint details.
A failed physical volume write does not stop original routing; Babel attempts to
restore the virtual display to the observed physical state instead of silently
claiming that a rejected mute succeeded.

A real Linux test is also available, ignored by the normal suite. It requires
PulseAudio clients on `PATH` and an accessible audio session, refuses to run if
Babel devices already exist, creates endpoints, tests tones in both routes,
interruption with a full queue, and preservation of defaults, then removes its
endpoints:

```sh
cargo test --lib live_virtual_routes_idempotence_interruption_and_cleanup -- --ignored
```
