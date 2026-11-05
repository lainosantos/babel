# Open Babel at login

**Start in the tray at login** opens Babel's tray and dashboard when the user signs
in. It is **off by default**. On launch, Babel routes original audio between
configured devices. The microphone activates when Babel is the system default
microphone or an app explicitly uses its virtual microphone. Output activates
only while an app sends audio to Babel. Translation,
transcription and recording wait for **Start session**; original routing sends
no audio to providers and creates no files. Autostart does not install virtual
devices either.

Check the option and click **Apply autostart**. To disable, uncheck and apply again.
Inspecting the option or saving other settings does not change login startup.
Changes apply to the next login and neither stop nor start another instance
immediately.

The dashboard shows the integration for the operating system that runs Babel.
It uses a systemd user service or XDG Autostart on Linux, a user LaunchAgent on
macOS, and a per-user scheduled task on Windows.

The command line manages the same registration. Pass the configuration you want
the tray to load at login:

```sh
babel --config /path/to/babel.toml autostart enable
babel autostart status
babel autostart disable
```

## Files and program location

Build both executables:

```sh
cargo build --release --bins
```

On Windows, distribute `babel.exe` and `babel-tray.exe` in the same directory.
`babel-tray.exe` uses the Windows GUI subsystem, so startup opens no console.
`babel` remains available for terminal commands. Linux's XDG fallback and macOS
register the executable currently in use; both can also run `babel-tray`
directly. The Linux package service uses `/usr/bin/babel-launch`. A portable
Linux installation uses the `babel-tray` executable next to `babel`.

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

### Updating a local Linux build

If login startup points into this checkout's `target/release` directory,
keep the executable paths when updating. Build all three binaries in a separate
directory, then replace each installed file with a rename on the destination
filesystem. Run these commands from the repository root:

```sh
set -e
babel_build_dir=$(mktemp -d)
cargo build --release --locked --bins --target-dir "$babel_build_dir"
for binary in babel babel-tray; do
    staged=$(mktemp "target/release/.${binary}.XXXXXX")
    install -m 755 "$babel_build_dir/release/$binary" "$staged"
    mv -fT -- "$staged" "target/release/$binary"
done
rm -rf -- "$babel_build_dir"
```

Linux keeps the running process on its original executable. Finish any
active audio session before quitting Babel, then reopen it to load the update.
The next login uses the updated binaries at the registered paths.

## Platform integrations

| OS | Per-user registration | Behavior |
| --- | --- | --- |
| Linux, compatible session | Packaged unit at `/usr/lib/systemd/user/org.babel.audio.service` or portable unit at `$XDG_CONFIG_HOME/systemd/user/org.babel.audio.service`. Both use the personal drop-in `org.babel.audio.service.d/50-babel.conf`. | User systemd service bound to `graphical-session.target`. |
| Linux fallback | `$XDG_CONFIG_HOME/autostart/org.babel.audio.desktop`, or `~/.config/autostart/org.babel.audio.desktop` | Graphical-session XDG Autostart; `Terminal=false`. |
| macOS | `~/Library/LaunchAgents/org.babel.audio.plist` | Aqua-session LaunchAgent with `RunAtLoad`; no `KeepAlive` or continuous restart. |
| Windows | `BabelAudio-<user SID>` in Task Scheduler Library | Starts `babel-tray.exe` in this user's interactive session with standard privileges. |

Enabling/disabling login in the app needs neither administrator access nor
networking. DEB/RPM installation is a separate operation requiring package-manager
permissions. Disabling removes only entries marked as Babel-owned; unrelated
same-name entries and symlinks are rejected instead of overwritten. The app
rechecks ownership before changes and writes files through atomic replacement.

### Linux user service

DEB/RPM packages include the systemd unit but **do not enable or start it during
installation**. There is no system-wide systemd service, root execution or audio
configuration change by the installer.

When applying the option, Babel uses systemd if `graphical-session.target` is
active and the user manager's environment contains `DISPLAY` or `WAYLAND_DISPLAY`.
It uses the packaged unit when available, or installs a personal unit if
`babel-tray` is available alongside the running executable. Desktops without
this integration and portable installs with only the CLI use XDG. An
already-enabled service remains visible and can be disabled even when the
graphical target is inactive.

**Startup method** identifies the actual registered method, and the path shows
the personal entry managed by Babel. An existing XDG entry stays marked enabled
until you click **Apply autostart** again or run `babel autostart enable`.
When the service is available, Babel creates the personal drop-in, enables the
unit, then removes its own XDG entry.

The drop-in passes the current configuration's absolute path, `--port 0` and
working directory to the launcher. Arguments follow systemd escaping; variable
expansion is disabled and percent signs remain literal. No intermediate shell
interprets them. Without absolute `XDG_CONFIG_HOME`, user configuration lives
under `~/.config/systemd/user/`.

Enabling creates the personal link at
`graphical-session.target.wants/org.babel.audio.service`. Disabling removes
the link and retains the drop-in and any personal unit. Babel uses
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

### XDG fallback

Linux ignores relative `XDG_CONFIG_HOME` under the XDG convention. Argument paths
are escaped without running a shell. The `.desktop` standard does not allow `=`
in executable paths; move the program in that case. For executable names containing
`%`, the launcher uses `/usr/bin/env` to avoid GLib's early name resolution,
without interpreting arguments as code.

### Windows scheduled task

Babel registers the task through Windows PowerShell and Task Scheduler. The
trigger is specific to the current user's sign-in and waits ten seconds for the
desktop shell. The tray can run on battery power and has no task time limit.
Registration requires a policy that permits Windows PowerShell and creation of
tasks for the current user. Task Scheduler expands environment variables in the
program, arguments and working directory, so Babel rejects paths with paired
percent signs.

If an earlier version registered `BabelAudio` in the user's Run registry key,
Babel shows the legacy method until you apply startup again. Applying with the
box checked creates the task and removes the old entry. Applying with the box
unchecked removes the Babel registration. Inspect the task under **Task Scheduler
Library**.

### macOS LaunchAgent

On macOS, the app writes the LaunchAgent for the next login; it does not run
`launchctl bootstrap` mid-session. Disabling does not kill the current process.
macOS permissions and execution policies still apply to the binary.

## Launch behavior

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
directories. Tests cover XML/plist escaping and Windows arguments with spaces,
Unicode and long paths.
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

For an end-to-end check on the target system, enable startup and confirm the
reported method with `babel autostart status`. Sign out and back in, open
**Settings** from the tray, and verify that the session is stopped. Disable
startup and repeat the sign-in to check removal.

Primary references: [XDG Autostart](https://specifications.freedesktop.org/autostart/latest/),
[Exec escaping](https://specifications.freedesktop.org/desktop-entry/latest/exec-variables.html),
[desktop/systemd integration](https://systemd.io/DESKTOP_ENVIRONMENTS/),
[systemctl enable/disable](https://www.freedesktop.org/software/systemd/man/latest/systemctl.html),
[systemd ExecStart syntax](https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html),
[Apple LaunchAgents](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html),
[Task Scheduler security contexts](https://learn.microsoft.com/en-us/windows/win32/taskschd/security-contexts-for-running-tasks)
and [Windows arguments](https://learn.microsoft.com/en-us/cpp/c-language/parsing-c-command-line-arguments).
