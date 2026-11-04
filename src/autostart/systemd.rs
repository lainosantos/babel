//! Linux user service. The manager is injectable; tests never call it.
use super::{
    AutostartStatus, LaunchSpec, MARKER, MAX_ENTRY_BYTES, desktop_entry, path_text,
    read_owned_file, set_file,
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeMap,
    io::{ErrorKind, Read},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const UNIT: &str = "org.babel.audio.service";
const PACKAGED: &str = include_str!("../../packaging/linux/org.babel.audio.service");
const MANAGER_ERROR: &str = "Could not query or configure the user systemd manager.";
const IDENTITY_ERROR: &str =
    "The existing systemd unit does not match the packaged Babel service; nothing was changed.";
const OVERRIDE_ERROR: &str =
    "The Babel service has an unmanaged systemd override; nothing was changed.";
const OFFLINE_ERROR: &str = "Babel already has a systemd entry, but the user manager is unavailable; try again in your graphical session.";

struct Paths {
    package: PathBuf,
    local: PathBuf,
    desktop: PathBuf,
    dropin: PathBuf,
    wanted: PathBuf,
    package_uid: u32,
    allow_user_service: bool,
}
impl Paths {
    fn current() -> Result<Self> {
        let desktop = super::location()?;
        let base = desktop
            .parent()
            .and_then(Path::parent)
            .context("Login entry directory unavailable")?;
        let user = base.join("systemd/user");
        // /lib may be a directory alias on merged-/usr distributions. The
        // final file must still be regular, root-owned and non-writable by users.
        let package = if Path::new("/usr/lib/systemd/user").join(UNIT).exists() {
            Path::new("/usr/lib/systemd/user").join(UNIT)
        } else {
            Path::new("/lib/systemd/user").join(UNIT)
        };
        Ok(Self {
            package,
            local: user.join(UNIT),
            desktop,
            dropin: user.join(format!("{UNIT}.d/50-babel.conf")),
            wanted: user.join("graphical-session.target.wants").join(UNIT),
            package_uid: 0,
            allow_user_service: std::fs::metadata("/proc/self").is_ok_and(|m| m.uid() != 0),
        })
    }
    fn package_present(&self) -> Result<bool> {
        let metadata = match std::fs::symlink_metadata(&self.package) {
            Ok(m) => m,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e).context(IDENTITY_ERROR),
        };
        ensure!(
            metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.uid() == self.package_uid
                && metadata.mode() & 0o022 == 0
                && metadata.len() <= MAX_ENTRY_BYTES,
            IDENTITY_ERROR
        );
        let mut text = String::new();
        std::fs::File::open(&self.package)?
            .take(MAX_ENTRY_BYTES + 1)
            .read_to_string(&mut text)?;
        ensure!(
            text.len() <= MAX_ENTRY_BYTES as usize && text == PACKAGED,
            IDENTITY_ERROR
        );
        Ok(true)
    }
    fn fragment(&self) -> Result<Option<&Path>> {
        if let Some(content) = read_owned_file(&self.local)? {
            ensure!(content == local_unit(), IDENTITY_ERROR);
            return Ok(Some(&self.local));
        }
        Ok(self.package_present()?.then_some(&self.package))
    }
    fn wanted(&self, fragment: &Path) -> Result<bool> {
        let metadata = match std::fs::symlink_metadata(&self.wanted) {
            Ok(m) => m,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e).context(OVERRIDE_ERROR),
        };
        ensure!(metadata.file_type().is_symlink(), OVERRIDE_ERROR);
        ensure!(
            self.wanted.canonicalize().context(OVERRIDE_ERROR)?
                == fragment.canonicalize().context(IDENTITY_ERROR)?,
            OVERRIDE_ERROR
        );
        Ok(true)
    }
}

fn local_unit() -> String {
    // The drop-in supplies the portable tray path. A base unit without the
    // drop-in must never launch an unrelated system-wide executable.
    PACKAGED.replace(
        "ExecStart=/usr/bin/babel-launch",
        "ExecStart=/usr/bin/false",
    )
}

trait Manager {
    fn run(&mut self, arguments: &[&str]) -> Result<String>;
}
struct Systemctl;
impl Manager for Systemctl {
    fn run(&mut self, arguments: &[&str]) -> Result<String> {
        let child = Command::new("/usr/bin/systemctl")
            .args(["--user", "--no-pager", "--no-ask-password"])
            .args(arguments)
            .env("LC_ALL", "C")
            .env("SYSTEMD_COLORS", "0")
            .env("SYSTEMD_PAGER", "cat")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context(MANAGER_ERROR)?;
        bounded_output(child, Duration::from_secs(3))
    }
}

fn bounded_output(mut child: Child, timeout: Duration) -> Result<String> {
    let stdout = child.stdout.take().context(MANAGER_ERROR)?;
    // Drain while the process runs: show-environment can exceed the OS pipe
    // capacity even when its total size is within our explicit memory bound.
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.take(MAX_ENTRY_BYTES + 1).read_to_end(&mut bytes)?;
        std::io::Result::Ok(bytes)
    });
    let deadline = Instant::now() + timeout;
    let status = (|| -> Result<_> {
        loop {
            if let Some(status) = child.try_wait().context(MANAGER_ERROR)? {
                return Ok(status);
            }
            ensure!(Instant::now() < deadline, MANAGER_ERROR);
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    if status.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let bytes = reader
        .join()
        .map_err(|_| anyhow::anyhow!(MANAGER_ERROR))?
        .context(MANAGER_ERROR)?;
    ensure!(
        status?.success() && bytes.len() <= MAX_ENTRY_BYTES as usize,
        MANAGER_ERROR
    );
    String::from_utf8(bytes).context(MANAGER_ERROR)
}

#[derive(Debug)]
struct Snapshot {
    enabled: bool,
    graphical: bool,
}

// systemctl show prints string arrays with shell_maybe_quote(..., 0). Parse
// quoted/backslash-escaped words as data; never invoke a shell or expand values.
fn property_words(value: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut started = false;
    for c in value.chars() {
        ensure!(!c.is_control(), OVERRIDE_ERROR);
        if escaped {
            word.push(c);
            escaped = false;
            started = true;
            continue;
        }
        if c == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                word.push(c);
            }
        } else if c == '"' || c == '\'' {
            quote = Some(c);
            started = true;
        } else if c == ' ' {
            if started {
                words.push(std::mem::take(&mut word));
                started = false;
            }
        } else {
            word.push(c);
            started = true;
        }
    }
    ensure!(!escaped && quote.is_none(), OVERRIDE_ERROR);
    if started {
        words.push(word);
    }
    Ok(words)
}

fn snapshot(paths: &Paths, manager: &mut impl Manager) -> Result<Option<Snapshot>> {
    let prior = read_owned_file(&paths.dropin)?;
    let Some(expected_fragment) = paths.fragment()? else {
        ensure!(
            std::fs::symlink_metadata(&paths.wanted)
                .is_err_and(|e| e.kind() == ErrorKind::NotFound),
            OFFLINE_ERROR
        );
        return Ok(None);
    };
    let wanted = paths.wanted(expected_fragment)?;
    let output = match manager.run(&[
        "show",
        UNIT,
        "--property=FragmentPath,DropInPaths,UnitFileState,LoadState",
    ]) {
        Ok(output) => output,
        Err(_) if !wanted && prior.is_none() => return Ok(None),
        Err(error) => return Err(error).context(OFFLINE_ERROR),
    };
    let mut properties = BTreeMap::new();
    for line in output.lines() {
        let (key, value) = line.split_once('=').context(IDENTITY_ERROR)?;
        ensure!(properties.insert(key, value).is_none(), IDENTITY_ERROR);
    }
    ensure!(
        properties.get("LoadState") == Some(&"loaded"),
        IDENTITY_ERROR
    );
    let fragment = properties.get("FragmentPath").context(IDENTITY_ERROR)?;
    ensure!(
        Path::new(fragment).canonicalize().ok() == expected_fragment.canonicalize().ok(),
        IDENTITY_ERROR
    );
    let dropins = property_words(properties.get("DropInPaths").context(OVERRIDE_ERROR)?)?;
    ensure!(
        dropins.iter().all(|p| Path::new(p) == paths.dropin) && dropins.len() <= 1,
        OVERRIDE_ERROR
    );
    if !dropins.is_empty() {
        ensure!(prior.is_some(), OVERRIDE_ERROR);
    }
    // Reject a foreign file waiting for daemon-reload too. Other load-path
    // overrides are checked by FragmentPath/DropInPaths after reload on writes.
    if let Some(parent) = paths.dropin.parent().filter(|p| p.exists()) {
        for entry in std::fs::read_dir(parent)? {
            let path = entry?.path();
            ensure!(
                path.extension().is_none_or(|e| e != "conf") || path == paths.dropin,
                OVERRIDE_ERROR
            );
        }
    }
    let state = properties.get("UnitFileState").context(IDENTITY_ERROR)?;
    ensure!(["enabled", "disabled"].contains(state), OVERRIDE_ERROR);
    let enabled = *state == "enabled";
    // Global/administrator activation is outside the scope of this checkbox.
    ensure!(enabled == wanted, OVERRIDE_ERROR);
    let graphical = graphical_session(paths, manager);
    Ok(Some(Snapshot { enabled, graphical }))
}

fn graphical_session(paths: &Paths, manager: &mut impl Manager) -> bool {
    paths.allow_user_service
        && manager
            .run(&[
                "show",
                "graphical-session.target",
                "--property=ActiveState",
                "--value",
            ])
            .is_ok_and(|v| v.trim() == "active")
        && manager.run(&["show-environment"]).is_ok_and(|environment| {
            // Only inspect the two graphical variable names. The complete
            // environment is never logged, persisted or returned to the UI.
            environment.lines().any(|line| {
                ["DISPLAY=", "WAYLAND_DISPLAY="].iter().any(|key| {
                    line.strip_prefix(key)
                        .is_some_and(|v| !["", "\"\"", "''"].contains(&v.trim()))
                })
            })
        })
}

fn service_status(paths: &Paths, enabled: bool) -> Result<AutostartStatus> {
    Ok(AutostartStatus {
        enabled,
        supported: true,
        migration_available: false,
        method: "systemd_user",
        entry_path: Some(path_text(&paths.dropin)?.to_owned()),
        description: if enabled {
            "Automatic startup through the systemd user service is enabled for the next graphical login.".into()
        } else {
            "Automatic startup through the systemd user service is disabled.".into()
        },
    })
}

fn unit_arg(value: &str) -> Result<String> {
    ensure!(
        !value.chars().any(char::is_control),
        "Startup path is empty or contains control characters"
    );
    let mut result = String::from("\"");
    for c in value.chars() {
        match c {
            '\\' | '"' => {
                result.push('\\');
                result.push(c);
            }
            '%' => result.push_str("%%"),
            _ => result.push(c),
        }
    }
    result.push('"');
    Ok(result)
}
fn dropin(spec: &LaunchSpec, local: bool) -> Result<String> {
    let config = path_text(&spec.config)?;
    let working = path_text(&spec.working_dir)?;
    ensure!(
        spec.config.is_absolute() && spec.working_dir.is_absolute(),
        "Invalid configuration path"
    );
    let launcher = if local {
        // Use a fixed executable word and pass the portable path as literal argv.
        format!(
            "/usr/bin/env -- {}",
            unit_arg(path_text(&spec.executable)?)?
        )
    } else {
        "/usr/bin/babel-launch".into()
    };
    // ':' disables systemd environment expansion; %% disables specifiers.
    // babel-tray receives argv directly and changes cwd.
    // Avoid WorkingDirectory= because that setting has different quote rules.
    Ok(format!(
        "# {MARKER}\n[Service]\nExecStart=\nExecStart=:{launcher} --config {} --port 0 --working-dir {} --autostart-owner {}\n",
        unit_arg(config)?,
        unit_arg(working)?,
        unit_arg(super::OWNER)?
    ))
}

fn change(
    paths: &Paths,
    manager: &mut impl Manager,
    enabled: bool,
    spec: Option<&LaunchSpec>,
    before: &Snapshot,
) -> Result<AutostartStatus> {
    let desktop = read_owned_file(&paths.desktop)?;
    let previous = read_owned_file(&paths.dropin)?;
    // Refresh before any enable/disable so pending administrator overrides are
    // validated too. Reload does not start/stop/restart any unit.
    manager.run(&["daemon-reload"])?;
    let refreshed = snapshot(paths, manager)?.context(IDENTITY_ERROR)?;
    ensure!(
        refreshed.enabled == before.enabled,
        "The login entry changed during the operation; try again"
    );
    if enabled {
        let local = paths.fragment()? == Some(paths.local.as_path());
        let content = dropin(spec.context("Invalid configuration path")?, local)?;
        ensure!(
            read_owned_file(&paths.dropin)? == previous,
            "The login entry changed during the operation; try again"
        );
        set_file(&paths.dropin, Some(&content))?;
        let apply = (|| -> Result<()> {
            manager.run(&["daemon-reload"])?;
            ensure!(snapshot(paths, manager)?.is_some(), IDENTITY_ERROR);
            manager.run(&["enable", UNIT])?;
            ensure!(
                snapshot(paths, manager)?.is_some_and(|s| s.enabled),
                MANAGER_ERROR
            );
            ensure!(
                read_owned_file(&paths.desktop)? == desktop,
                "The login entry changed during the operation; try again"
            );
            set_file(&paths.desktop, None)?;
            Ok(())
        })();
        if let Err(error) = apply {
            // Preserve the preexisting login behavior on failure. Never stop
            // the currently running service while rolling back registration.
            if !before.enabled {
                let _ = manager.run(&["disable", UNIT]);
            }
            ensure!(
                read_owned_file(&paths.dropin)?.as_deref() == Some(content.as_str()),
                "The login entry changed during the operation; try again"
            );
            set_file(&paths.dropin, previous.as_deref())
                .context("Could not restore the login entry after a systemd error.")?;
            let _ = manager.run(&["daemon-reload"]);
            return Err(error);
        }
    } else {
        manager.run(&["disable", UNIT])?;
        ensure!(
            snapshot(paths, manager)?.is_some_and(|s| !s.enabled),
            MANAGER_ERROR
        );
        ensure!(
            read_owned_file(&paths.desktop)? == desktop,
            "The login entry changed during the operation; try again"
        );
        set_file(&paths.desktop, None)?;
        // Keep the owned drop-in for diagnosis/custom-config preservation. It
        // cannot start anything without a wanted link. Re-enabling updates it.
    }
    service_status(paths, enabled)
}

pub(super) fn status() -> Result<AutostartStatus> {
    let paths = Paths::current()?;
    let desktop = super::file_status()?;
    let portable = std::env::current_exe().is_ok_and(|exe| tray_path(&exe).is_some());
    status_for(&paths, &mut Systemctl, desktop, portable)
}
fn status_for(
    paths: &Paths,
    manager: &mut impl Manager,
    mut desktop: AutostartStatus,
    portable: bool,
) -> Result<AutostartStatus> {
    match snapshot(paths, manager)? {
        Some(s) if !s.enabled && desktop.enabled => {
            desktop.migration_available = s.graphical && (!paths.local.exists() || portable);
            Ok(desktop)
        }
        Some(s) if s.enabled || s.graphical => service_status(paths, s.enabled),
        None if desktop.enabled && portable && paths.fragment()?.is_none() => {
            desktop.migration_available =
                graphical_session(paths, manager) && unit_slot_free(manager).unwrap_or(false);
            Ok(desktop)
        }
        _ => Ok(desktop),
    }
}
pub(super) fn set_enabled(enabled: bool, config: &Path) -> Result<AutostartStatus> {
    let paths = Paths::current()?;
    let directory = paths
        .local
        .parent()
        .context("User service directory unavailable")?;
    // The CLI and dashboard can apply the same user-wide preference from
    // separate processes. Keep rollback inside this registration lock.
    std::fs::create_dir_all(directory)?;
    let lock_path = directory.join(".babel-autostart.lock");
    match std::fs::symlink_metadata(&lock_path) {
        Ok(metadata) => ensure!(metadata.is_file(), "Invalid user service registration lock"),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).context("Could not inspect the user service registration lock");
        }
    }
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(lock_path)
        .context("Could not open the user service registration lock")?;
    ensure!(
        lock.metadata()?.is_file(),
        "Invalid user service registration lock"
    );
    lock.try_lock()
        .context("Could not lock user service registration; try again")?;
    let spec = enabled.then(|| LaunchSpec::current(config)).transpose()?;
    if let Some(status) = apply_service(&paths, &mut Systemctl, enabled, spec.as_ref())? {
        return Ok(status);
    }
    let content = spec.as_ref().map(desktop_entry).transpose()?;
    set_file(&paths.desktop, content.as_deref())?;
    super::file_status()
}

fn apply_service(
    paths: &Paths,
    manager: &mut impl Manager,
    enabled: bool,
    spec: Option<&LaunchSpec>,
) -> Result<Option<AutostartStatus>> {
    let fragment = paths.fragment()?;
    let before = snapshot(paths, manager)?;
    if let Some(before) = before.filter(|s| s.enabled || s.graphical) {
        if let Some(spec) = spec
            && fragment == Some(paths.local.as_path())
        {
            let tray =
                portable_tray(spec).context("Build or install babel-tray alongside Babel")?;
            return change(paths, manager, enabled, Some(&tray), &before).map(Some);
        }
        return change(paths, manager, enabled, spec, &before).map(Some);
    }

    let Some(spec) = spec.filter(|_| fragment.is_none()) else {
        return Ok(None);
    };
    let Some(tray) = portable_tray(spec).filter(|_| graphical_session(paths, manager)) else {
        return Ok(None);
    };
    manager.run(&["daemon-reload"])?;
    ensure!(unit_slot_free(manager)?, IDENTITY_ERROR);
    let unit = local_unit();
    set_file(&paths.local, Some(&unit))?;
    let result = change(
        paths,
        manager,
        true,
        Some(&tray),
        &Snapshot {
            enabled: false,
            graphical: true,
        },
    );
    if result.is_err()
        && !paths.wanted(&paths.local)?
        && read_owned_file(&paths.local)?.as_deref() == Some(unit.as_str())
    {
        set_file(&paths.local, None)?;
        let _ = manager.run(&["daemon-reload"]);
    }
    result.map(Some)
}

fn unit_slot_free(manager: &mut impl Manager) -> Result<bool> {
    Ok(manager
        .run(&["show", UNIT, "--property=LoadState", "--value"])?
        .trim()
        == "not-found")
}

fn tray_path(executable: &Path) -> Option<PathBuf> {
    let tray = executable.with_file_name("babel-tray");
    tray.is_file().then_some(tray)
}

fn portable_tray(spec: &LaunchSpec) -> Option<LaunchSpec> {
    Some(LaunchSpec {
        executable: tray_path(&spec.executable)?,
        tray_launcher: true,
        ..spec.clone()
    })
}

#[cfg(test)]
mod tests;
