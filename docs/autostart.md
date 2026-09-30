# Open Babel at login

**Start in the tray at login** opens Babel's tray and dashboard when the user signs
in. It is **off by default**. On launch, Babel routes original audio between
configured devices. The microphone activates when Babel is the system default
microphone or an app explicitly uses its virtual microphone. Output activates
only while an app sends audio to Babel. Enabled voice commands can listen even
without a calling app, including immediately after login. Translation,
transcription and recording wait for **Start session**; original routing sends
no audio to providers and creates no files. Autostart does not install virtual
devices either.

Check the option and click **Apply autostart**. To disable, uncheck and apply again.
Inspecting the option or saving other settings does not change login startup.
Changes apply to the next login and neither stop nor start another instance
immediately.

The dashboard identifies the Babel process's OS and shows its login integration:
a user systemd service or XDG Autostart on Linux, LaunchAgent on macOS, or the
user Run entry on Windows. Browser platform/interface language do not change
that identification. The table below documents all three systems for reference;
the app displays settings for the current system.

## Files and program location

Build both executables:

```sh
cargo build --release --bins
```

On Windows, distribute `babel.exe` and `babel-tray.exe` in the same directory.
`babel-tray.exe` uses the Windows GUI subsystem, so startup opens no console.
`babel` remains available for terminal commands. Linux's XDG fallback and macOS
register the executable currently in use; both can also run `babel-tray`
directly. The Linux service uses the packaged `/usr/bin/babel-launch` launcher.

Enable startup only after putting the program in its final location. The entry
stores absolute executable/configuration paths, `--port 0` and the current
working directory. The OS chooses an available port at every login; the entry
never stores the current run's temporary port. The working directory keeps
configuration-relative paths consistent after login. If you move the program,
configuration or working directory, launch Babel from its final location and
apply startup again. A `target/debug` copy depends on that directory remaining.

The login entry contains no API keys, dashboard token, transcripts or audio.
Dashboard tokens are regenerated each run. Credentials supplied only in process
memory must be entered again; environment variables must be available in the
graphical session, which may not load your terminal profile.

## Platform integrations

| OS | Per-user registration | Behavior |
| --- | --- | --- |
| Linux, DEB/RPM package and compatible session | Unit `/usr/lib/systemd/user/org.babel.audio.service`; user configuration at `$XDG_CONFIG_HOME/systemd/user/org.babel.audio.service.d/50-babel.conf` | User systemd service bound to `graphical-session.target`. |
| Linux fallback | `$XDG_CONFIG_HOME/autostart/org.babel.audio.desktop`, or `~/.config/autostart/org.babel.audio.desktop` | Graphical-session XDG Autostart; `Terminal=false`. |
| macOS | `~/Library/LaunchAgents/org.babel.audio.plist` | Aqua-session LaunchAgent with `RunAtLoad`; no `KeepAlive` or continuous restart. |
| Windows | `BabelAudio` value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` | Opens `babel-tray.exe`, with working directory explicitly passed to the launcher. |

Enabling/disabling login in the app needs neither administrator access nor
networking. DEB/RPM installation is a separate operation requiring package-manager
permissions. Disabling removes only entries marked as Babel-owned; unrelated
same-name entries and symlinks are rejected instead of overwritten. The app
rechecks ownership before changes and writes files through atomic replacement.

### Linux user service

DEB/RPM packages include the systemd unit but **do not enable or start it during
installation**. There is no system-wide systemd service, root execution or audio
configuration change by the installer.

When applying the option, Babel prefers the service if the packaged unit is
present, intact and loaded by the user manager, `graphical-session.target` is
active, and that manager's environment contains `DISPLAY` or `WAYLAND_DISPLAY`.
Availability is checked in the Linux process; having these variables only in
a terminal is insufficient. Desktops without this integration, portable installs
without the unit and sessions without systemd use XDG. An already-enabled service
remains visible and can be disabled even if the graphical target is currently inactive.

**Startup method** identifies the actual registered method, and the path shows
the personal entry managed by Babel. An existing XDG entry stays marked enabled
until you apply: when the service is available, Babel creates the personal drop-in,
enables only that unit, then removes its own XDG entry to avoid duplication.

The drop-in passes the current configuration's absolute path, `--port 0` and
working directory to the launcher. Arguments follow systemd escaping; variable
expansion is disabled and percent signs remain literal. No intermediate shell
interprets them. Without absolute `XDG_CONFIG_HOME`, user configuration lives
under `~/.config/systemd/user/`.

Enabling creates the personal link at
`graphical-session.target.wants/org.babel.audio.service`; disabling removes
enablement and retains the drop-in for diagnostics/configuration. Babel uses
`enable`/`disable` without `--now`: **it does not start, stop or restart the current
process**. `daemon-reload` only updates manager configuration. After a login
starts the service, process failure permits bounded restart through
`Restart=on-failure`; normal quitting does not restart. When the graphical
session ends, the unit uses `SIGINT` to allow session files to close.

Modified units, globally managed enablement and overrides not owned by Babel
produce diagnostics rather than being overwritten. If existing systemd
enablement cannot be queried, no additional XDG entry is created. Inspect from
a working graphical session and apply again. Babel checks package identity/content
but does not manage other services or import terminal environment into systemd.

Read-only diagnostic commands:

```sh
systemctl --user status org.babel.audio.service
systemctl --user is-enabled org.babel.audio.service
systemctl --user show graphical-session.target --property=ActiveState
```

### Other integrations and XDG fallback

Linux ignores relative `XDG_CONFIG_HOME` under the XDG convention. Argument paths
are escaped without running a shell. The `.desktop` standard does not allow `=`
in executable paths; move the program in that case. For executable names containing
`%`, the launcher uses `/usr/bin/env` to avoid GLib's early name resolution,
without interpreting arguments as code.

Windows Run keys have a documented 260-character command limit. If combined
paths exceed it, Babel rejects the change and suggests shorter paths rather
than truncating the command. Windows may delay/block startup apps under user or
organization preferences; check **Settings → Apps → Startup**. Babel's displayed
state confirms its managed registration, not external system authorization.

On macOS, the app writes the LaunchAgent for the next login; it does not run
`launchctl bootstrap` mid-session. Disabling does not kill the current process.
macOS permissions and execution policies still apply to the binary.

The tray depends on the desktop environment. If unavailable on Linux, the local
dashboard remains accessible through the terminal-printed address. **Settings**
in the tray always opens the current address/token. Bookmarks with a previous
run's port become invalid.

Babel keeps the socket open from port selection onward; it does not find an
available port and reserve it later. `babel serve --port NUMBER` requests a
specific port: if occupied, Babel logs that fact and opens the dashboard on an
OS-selected free port, without directing users to the other program. New login
entries always use `--port 0`; reapply to update an entry created by an older version.

An OS lock prevents multiple instances using the same configuration, including
`babel run`. It lives in `<configuration>.babel-instance.lock` beside the TOML
and stays held until the controller and streams close. A second launch directs
you to the existing instance without starting another router. The file contains
no keys/token and must not be deleted while Babel is open. On exit, the OS
releases the lock; the empty file remains reusable by the next run. The
configuration directory must allow its creation.

## Verification

Creation, update, removal, ownership and symlink tests use only temporary
directories. Tests cover XML/plist escaping, Windows arguments and length limits.
None enables actual developer login startup.

Systemd tests use a simulated manager and temporary directories to validate
unit identity, fallback without a graphical/display environment, XDG migration,
custom configuration, rollback and override refusal. They also verify that
`start`, `stop`, `restart` and `--now` are never sent. Output collection is tested
with temporary processes without calling systemd; memory/time are bounded.
Queried environment data is neither logged, persisted nor returned to the dashboard.

Dashboard tests open loopback sockets only, checking dynamic ports, collisions,
tokens, Host and Origin without capture/routing. Lock tests use temporary
configurations and verify that atomic TOML replacement does not allow a second instance.

```sh
cargo test --lib autostart::
cargo test --lib dashboard::
```

On Linux with `gio` and `desktop-file-validate`, this additional test validates
a temporary `.desktop` and launches only a temporary argument recorder:

```sh
cargo test --lib autostart::tests::desktop_launcher_preserves_actual_arguments_through_gio -- --ignored --nocapture
```

It checks real argument transport with spaces, Unicode, percent signs, quotes,
slashes, dollar signs and backticks. Actual login testing requires the respective
Linux, macOS or Windows desktop session; mocks and cross-compilation do not
replace that execution.

Primary references: [XDG Autostart](https://specifications.freedesktop.org/autostart/latest/),
[Exec escaping](https://specifications.freedesktop.org/desktop-entry/latest/exec-variables.html),
[desktop/systemd integration](https://systemd.io/DESKTOP_ENVIRONMENTS/),
[systemctl enable/disable](https://www.freedesktop.org/software/systemd/man/latest/systemctl.html),
[systemd ExecStart syntax](https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html),
[Apple LaunchAgents](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html),
[Windows Run/RunOnce](https://learn.microsoft.com/en-us/windows/win32/setupapi/run-and-runonce-registry-keys)
and [Windows arguments](https://learn.microsoft.com/en-us/cpp/c-language/parsing-c-command-line-arguments).
