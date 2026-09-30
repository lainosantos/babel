# Virtual devices and routing by operating system

The translation process runs in Rust in user space. On Linux, Babel creates
selectable devices through the existing audio server. On macOS and Windows,
the backend uses CoreAudio/WASAPI through CPAL and connects to the two paths
provided by the **native Babel driver**. Source and build scripts live in
`native/macos` and `native/windows`; BlackHole and VB-CABLE are not dependencies
of these drivers. **Signed distribution packages and driver validation on real
macOS/Windows hardware are still pending.** See the
[native driver guide](native-drivers.md) for building and explicit installation.

Babel identifies the operating system running its process. The dashboard and
tray use that host information regardless of the browser opening the dashboard.
Detection does not rely on the user agent or language preference. The dashboard
shows instructions, device names, and actions for that system: creating and
removing devices on Linux, or installing the Babel package and selecting its
devices on macOS and Windows. **Refresh devices** is available on all three.

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

## macOS: Babel driver with two duplex devices

<a id="macos"></a>

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

**Optional alternative:** two independent BlackHole installations, such as 2ch
and 16ch, remain compatible. Substitute BlackHole 2ch for Babel Microphone and
BlackHole 16ch for Babel Speaker in the table. Channels 1/2 are used; applications
that reject 16 channels need another compatible independent loopback. Obtain
and install packages from the
[BlackHole project](https://github.com/ExistentialAudio/BlackHole), respecting its
[license and integration terms](https://github.com/ExistentialAudio/BlackHole#can-i-integrate-blackhole-into-my-app).
Babel does not redistribute BlackHole, and its own driver does not require it.

Official references: [devices used by a CoreAudio process](https://developer.apple.com/documentation/coreaudio/kaudioprocesspropertydevices)
and [Apple's process API example for macOS 14.2+](https://developer.apple.com/documentation/coreaudio/capturing-system-audio-with-core-audio-taps).
Babel queries processes/devices without creating taps to capture global audio.

## Windows: Babel driver with two independent pairs

<a id="windows"></a>

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

Pairing checks each driver-supplied endpoint description and the **Babel Audio
v1** interface identity. WASAPI IDs remain opaque identities; a renameable friendly
name does not determine pairing. Missing or ambiguous endpoints and query failures
keep the affected direction closed with a diagnostic. Selecting both sides of
the same cable for both routes blocks both. There is no default-device fallback.

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

A real Linux test is also available, ignored by the normal suite. It requires
PulseAudio clients on `PATH` and an accessible audio session, refuses to run if
Babel devices already exist, creates endpoints, tests tones in both routes,
interruption with a full queue, and preservation of defaults, then removes its
endpoints:

```sh
cargo test --lib live_virtual_routes_idempotence_interruption_and_cleanup -- --ignored
```
