# Linux packages

This directory produces **amd64/x86-64 glibc** packages for the Babel application.
It includes no kernel driver: the Linux backend uses the user's PulseAudio or
PipeWire session with `pipewire-pulse`.

## Build and package

Packaging requirements: Python **3.11+**, `dpkg-deb` from `dpkg`, `readelf` from
`binutils`, `rpmbuild`/`rpm` from `rpm`, and `rpm2cpio` (RPM **4.17+**).
`cpio` is useful for manual inspection; builder validation reads the CPIO payload
directly without extracting unvalidated paths. The builder does not compile
Rust, install dependencies, or execute supplied binaries. On an Ubuntu 22.04 runner:

```sh
sudo apt install dpkg binutils rpm rpm2cpio cpio
cargo build --release --locked --bins
python3 scripts/build_local_runtime.py --output artifacts/local-runtime
python3 packaging/linux/build.py --bin-dir target/release --runtime-dir artifacts/local-runtime --output artifacts
python3 -m unittest discover -s packaging/linux -p 'test_*.py' -v
```

The script reads the version from `Cargo.toml`. `--version 1.2.3` overrides it;
prereleases such as `1.2.3-rc.1` become `1.2.3~rc.1` in DEB and RPM packages,
sorting before the final version. This local packager capability is broader than
the automated cross-platform release workflow, which accepts stable `vX.Y.Z`
tags. `SOURCE_DATE_EPOCH` controls file timestamps; without it, timestamps are
zero. For version `0.1.0`, outputs are:

- `babel-audio_0.1.0_amd64.deb`;
- `babel-audio-0.1.0-1.x86_64.rpm`;
- `babel-audio-0.1.0-linux-amd64.tar.gz`;
- `babel-audio-0.1.0-linux-amd64.manifest.json`, containing artifact SHA-256 hashes.

Generation inspects the three executables as amd64 ELF and extracts glibc
requirements. An unknown dynamic library stops the build until an explicit
dependency mapping is supplied. Building on Ubuntu 22.04 broadens compatibility;
a newer distribution can require a newer glibc. The measured minimum appears
in the manifest, DEB `Depends`, and RPM `Requires`.
The tarball has the same requirement: **it is not a static musl/Alpine binary**.

The builder validates `dpkg-deb --info`, `--contents`, RPM metadata and dependencies,
all hashes/permissions in the three formats, and the absence of installation/removal
hooks, scriptlets, and triggers. RPM uses format 4 with a gzip payload; the build
preserves supplied executables without stripping or post-processing. Two independent
generations in the test must produce identical artifacts with the same input files,
tool versions, and `SOURCE_DATE_EPOCH`. Tests use system executables as fixtures
without running them. The launcher test uses an argument-recording stub without
starting Babel or audio.

## Install the DEB on Debian/Ubuntu

```sh
sudo apt install ./babel-audio_0.1.0_amd64.deb
```

The package installs `babel`, `babel-tray`, `babel-feedback`, and `babel-launch`
into `/usr/bin`, plus a **Babel** application-menu entry and icon. Documentation
and the content manifest live in `/usr/share/doc/babel-audio`; Whisper, llama.cpp,
and Piper engines live in `/usr/share/babel/local-runtime/linux-x86_64` alongside
libraries, data, and licenses. The optional Needle helper is at
`/usr/share/babel/scripts/needle_bridge.py`.

Declared dependencies include build-compatible glibc, `libgcc-s1` when used by
the binaries, `libstdc++6` for native engines, `pulseaudio-utils`, `xdg-utils`, and
a session D-Bus provider. The native command overlay also declares `libx11-6`,
`libx11-xcb1`, `libxcursor1`, `libxi6`, `libxkbcommon0`, and `libxkbcommon-x11-0`.
These standard desktop libraries load at runtime and may not appear in `ldd` or
the executable's `NEEDED` entries, so the package declares them explicitly.
Xvfb is used only in CI tests and is not an application runtime dependency.
`pipewire-pulse` or `pulseaudio` appears only as **Suggests**: the Babel package
must not choose, replace, or start the audio server. Use the desktop's configured
server. The tray requires StatusNotifier/AppIndicator support, which can depend
on an extension in GNOME.

Installation does not run Babel, create devices, activate translation, or enable
autostart. There is no `postinst`, `prerm`, or `/etc/xdg/autostart` entry.
The systemd **user** unit is installed disabled at
`/usr/lib/systemd/user/org.babel.audio.service`. Creating devices and enabling
autostart remain separate user actions in the application.

## Install the RPM on Fedora and other RPM distributions

```sh
sudo dnf install ./babel-audio-0.1.0-1.x86_64.rpm
```

On openSUSE, use `sudo zypper install ./babel-audio-0.1.0-1.x86_64.rpm`.
The RPM installs the same files, launcher, and user unit as the DEB. It declares
compatible glibc, `libgcc_s.so.1` when used, `/bin/sh`, D-Bus, and tools through
the paths `/usr/bin/pactl`, `/usr/bin/parec`, `/usr/bin/pacat`, and
`/usr/bin/xdg-open`. The command overlay also requires the 64-bit library
capabilities `libX11.so.6`, `libX11-xcb.so.1`, `libXcursor.so.1`, `libXi.so.6`,
`libxkbcommon.so.0`, and `libxkbcommon-x11.so.0`. The package manager resolves
providers of those paths and libraries. A desktop with a PulseAudio/PipeWire-pulse
server and tray support must already be configured. The RPM neither chooses
nor starts the audio server and has no installation/removal scriptlets or triggers.

## User service and autostart

Only DEB/RPM installation places the unit in the system unit directory; it remains
a **user** service, never a root daemon. Do not use `sudo systemctl` or enable
linger for this graphical application. Babel's autostart setting can enable or
disable the packaged unit without starting or stopping the current instance.
The application's drop-in preserves the selected configuration's absolute path.

For manual control in the user's own graphical session:

```sh
systemctl --user daemon-reload
systemctl --user enable org.babel.audio.service   # future logins only
systemctl --user status org.babel.audio.service
systemctl --user disable org.babel.audio.service # does not stop the current instance
```

To start now, first quit any running Babel instance, then use
`systemctl --user start org.babel.audio.service`. Stop it with
`systemctl --user stop org.babel.audio.service` or **Quit** in the tray.
Diagnostics: `journalctl --user -u org.babel.audio.service`.

The unit binds to `graphical-session.target` and requires `DISPLAY` or
`WAYLAND_DISPLAY` in the user manager's environment. Desktops without that
integration should use the application's XDG autostart option. It does not start
in a root session or at boot without graphical login. Failures restart the
process with a retry limit; a normal exit does not restart. Stopping sends SIGINT
to finalize the session and files, with a 15-second limit.

## Use the tarball on other distributions

```sh
tar -xzf babel-audio-0.1.0-linux-amd64.tar.gz
./babel-audio-0.1.0-linux-amd64/bin/babel-launch
```

Install your distribution's PulseAudio tools (`pactl`, `parec`, `pacat`),
`xdg-open`, and session D-Bus. On Debian/Ubuntu, if using the tarball:

```sh
sudo apt install pulseaudio-utils xdg-utils dbus-user-session \
  libx11-6 libx11-xcb1 libxcursor1 libxi6 libxkbcommon0 libxkbcommon-x11-0
```

Check the manifest for minimum glibc and keep a PulseAudio or PipeWire-pulse
server active in your session. The tarball contains `bin/`, `share/`, and the
`lib/systemd/user/org.babel.audio.service` template. It can live in a user directory,
including paths with spaces or Unicode characters. It requires no root access
and contains no script that copies files into system directories. The supplied
`.desktop` entry uses `Exec=babel-launch`; manual application-menu installation
requires `bin/` in the graphical session's `PATH` and the icon in the user's theme.
Running `bin/babel-launch` directly needs none of that optional integration.
The service template is not installed or enabled automatically; if manually
copied to `~/.config/systemd/user`, its `ExecStart` must reference the extracted
launcher's correctly escaped absolute path. Application autostart uses packaged
integration only when installed and validated; prefer XDG autostart for a
portable extraction.

The native overlay uses X11 or XWayland. Without those libraries or an available
X11 display, the main process continues and tries to present command states
through desktop notifications. DEB/RPM packages supply the libraries; a Wayland
session without XWayland still uses that fallback.

## First use and configuration

The menu opens **the tray and local dashboard**, following the actual
`babel-tray` CLI. Select **Settings** in the tray to open the dashboard's current
address. The launcher passes `--port 0`; it neither hardcodes nor probes for a
port in advance. If the desktop has no tray, run
`babel --config ABSOLUTE_PATH serve --no-tray` in a terminal and open the printed
address.

The launcher uses `$XDG_CONFIG_HOME/babel/babel.toml` when `XDG_CONFIG_HOME` is
absolute, otherwise `$HOME/.config/babel/babel.toml`. It creates only the directory,
restricts permissions for new files, and preserves existing configuration. The
application saves configuration when settings are applied. Session file destinations
are configured separately; new configurations default to the absolute `Babel`
directory inside the user's home. Direct executables accept `--config` for another
configuration. With explicit arguments, `babel-launch` forwards them unchanged
to `babel-tray`, adding no configuration, port, or subcommand. For example:
`babel-launch --config /home/alex/config/babel.toml --port 0`.

After opening, configure physical devices in Routing and create virtual devices
through the dashboard. No translation, transcription, or recording session starts
automatically. Original routing follows configuration and virtual-device usage;
quitting the application stops it.

Local transcription/translation engines ship with the packages: Whisper, llama.cpp,
and Piper with ONNX Runtime and eSpeak data. Users do not separately install
Python, Ollama, compilers, or AI services. Models are prepared on first selection;
package installation neither downloads models nor starts inference. Needle voice
commands have their own setup described in `docs/voice-commands.md`. Cloud keys,
personal configuration, and recordings never enter the package.

## Remove

```sh
sudo apt remove babel-audio
# Fedora: sudo dnf remove babel-audio
# openSUSE: sudo zypper remove babel-audio
```

The package manager removes packaged files, leaving personal files, audio-server
devices, and autostart preferences unchanged. If autostart was enabled, disable
it in Babel before removal. For a manually enabled unit, first run
`systemctl --user disable --now org.babel.audio.service`, then
`systemctl --user daemon-reload` after removal. The installer does not delete
user-created drop-ins or links. Quit Babel and remove devices through the dashboard
if desired. For a tarball, quit and remove the extracted directory and any
manually created shortcuts.

## License and metadata

Application code and package integration files use the MIT license, included as
`copyright` in the documentation directory. Rust dependencies retain their own
licenses; `Cargo.lock` identifies versions. The separate Piper/eSpeak process
uses GPLv3: complete corresponding sources, changes, and build instructions are
in `local-runtime/linux-x86_64/sources/`; engine and library licenses are in that
runtime's `licenses/` directory. The manifest declares combined package licensing.
The interface's Manrope font retains its SIL Open Font License, included as
`licenses/Manrope-OFL.txt` in the same directory. The package maintainer contact
is a deliberately non-deliverable `.invalid` address until the project publishes
a distribution contact. Artifacts contain no credentials, personal configuration,
model weights, or third-party audio drivers.
