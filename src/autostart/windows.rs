use super::{LaunchSpec, path_text};
use anyhow::{Result, ensure};

fn windows_arg(value: &str) -> String {
    let mut output = String::from("\"");
    let mut slashes = 0;
    for ch in value.chars() {
        if ch == '\\' {
            slashes += 1;
            continue;
        }
        if ch == '"' {
            output.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
        } else {
            output.extend(std::iter::repeat_n('\\', slashes));
        }
        slashes = 0;
        output.push(ch);
    }
    output.extend(std::iter::repeat_n('\\', slashes * 2));
    output.push('"');
    output
}
fn task_arguments(spec: &LaunchSpec) -> Result<String> {
    ensure!(
        spec.tray_launcher,
        "Windows startup requires babel-tray.exe"
    );
    // Task Scheduler expands percent-delimited variables in all Exec fields.
    for path in [&spec.executable, &spec.config, &spec.working_dir] {
        ensure!(
            path_text(path)?.matches('%').count() < 2,
            "Task Scheduler may expand percent-delimited variables in startup paths; use paths without paired percent signs"
        );
    }
    Ok(spec.args()?[1..]
        .iter()
        .map(|arg| windows_arg(arg))
        .collect::<Vec<_>>()
        .join(" "))
}

#[cfg(target_os = "windows")]
use super::{AutostartStatus, OWNER};
#[cfg(target_os = "windows")]
use anyhow::{Context, bail};
#[cfg(target_os = "windows")]
use std::{
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[cfg(target_os = "windows")]
fn run_task_scheduler(operation: &str, spec: Option<&LaunchSpec>) -> Result<AutostartStatus> {
    use base64::{Engine, engine::general_purpose::STANDARD};

    let system_root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .context("Windows system directory unavailable")?;
    let powershell = system_root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    // Windows PowerShell expects UTF-16LE for EncodedCommand. Keep the script
    // fixed and pass all paths as environment data, never as PowerShell source.
    let script = include_str!("windows.ps1");
    let encoded = STANDARD.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    let mut command = Command::new(&powershell);
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
        ])
        .arg(encoded)
        .env("BABEL_STARTUP_OPERATION", operation)
        .env("BABEL_STARTUP_OWNER", OWNER)
        .current_dir(
            powershell
                .parent()
                .context("PowerShell directory unavailable")?,
        )
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(spec) = spec {
        command
            .env("BABEL_STARTUP_EXECUTABLE", &spec.executable)
            .env("BABEL_STARTUP_ARGUMENTS", task_arguments(spec)?)
            .env("BABEL_STARTUP_WORKING_DIR", &spec.working_dir);
    }
    let mut child = command
        .spawn()
        .context("Could not open Windows Task Scheduler through PowerShell")?;
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            result => {
                let _ = child.kill();
                let _ = child.wait();
                result.context("Could not query Windows Task Scheduler")?;
                bail!("Windows Task Scheduler timed out; check the startup status before retrying");
            }
        }
    }
    let output = child
        .wait_with_output()
        .context("Could not read Windows startup settings")?;
    let reply = String::from_utf8(output.stdout).context("Invalid Windows startup status")?;
    let reply = reply.trim();
    let (state, detail) = reply.split_once(' ').unwrap_or((reply, ""));
    if !output.status.success() {
        match state {
            "foreign-task" => bail!(
                "The existing Windows startup task does not belong to Babel; nothing was changed"
            ),
            "foreign-run" => {
                bail!("The Windows BabelAudio entry does not belong to Babel; nothing was changed")
            }
            "busy" => bail!("Another process is changing Windows login startup; try again"),
            "error" if detail.parse::<i32>().is_ok() => {
                bail!("Could not query or configure Windows Task Scheduler (error {detail})");
            }
            _ => bail!("Could not query or configure Windows Task Scheduler"),
        }
    }
    let (enabled, method, entry_path, description) = match state {
        "legacy" => (
            true,
            "registry_run",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run\BabelAudio".into(),
            "The legacy Run entry is registered. Apply startup again to move it to Task Scheduler.",
        ),
        "enabled" | "disabled" if detail.starts_with(r"\BabelAudio-S-") => (
            state == "enabled",
            "task_scheduler",
            detail.to_owned(),
            if state == "enabled" {
                "The Windows startup task is enabled for this user's next login."
            } else {
                "Login startup is disabled for this user."
            },
        ),
        _ => bail!("Invalid Windows startup status"),
    };
    Ok(AutostartStatus {
        enabled,
        supported: true,
        migration_available: method == "registry_run",
        method,
        entry_path: Some(entry_path),
        description: description.into(),
    })
}

#[cfg(target_os = "windows")]
pub(super) fn platform_status() -> Result<AutostartStatus> {
    run_task_scheduler("status", None)
}

#[cfg(target_os = "windows")]
pub(super) fn platform_set(enabled: bool, config: &Path) -> Result<AutostartStatus> {
    if enabled {
        run_task_scheduler("enable", Some(&LaunchSpec::current(config)?))
    } else {
        run_task_scheduler("disable", None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduler_arguments_preserve_unicode_and_allow_long_paths() {
        assert_eq!(
            windows_arg(r"C:\Program Files\Babel\"),
            "\"C:\\Program Files\\Babel\\\\\""
        );
        assert_eq!(windows_arg("a\\\"b"), "\"a\\\\\\\"b\"");
        let mut spec = LaunchSpec {
            executable: r"C:\Babel\babel-tray.exe".into(),
            config: format!(r"C:\{}João.toml", "long directory\\".repeat(20)).into(),
            working_dir: r"C:\Babel".into(),
            tray_launcher: true,
        };
        assert_eq!(
            task_arguments(&spec).unwrap(),
            format!(r#""--config" "{}" "--port" "0""#, spec.config.display())
        );
        spec.config = r"C:\%USERPROFILE%\config.toml".into();
        assert!(task_arguments(&spec).is_err());
    }
}
