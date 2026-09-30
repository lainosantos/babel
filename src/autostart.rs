//! Per-user login startup. Never starts translation or stores authentication data.
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
};

pub const OWNER: &str = "org.babel.audio.autostart.v1";
#[cfg(any(target_os = "linux", target_os = "macos", test))]
const MARKER: &str = "Babel-Autostart-Owner: org.babel.audio.autostart.v1";
#[cfg(any(target_os = "linux", target_os = "macos", test))]
const MAX_ENTRY_BYTES: u64 = 65_536;
static MUTATION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
#[cfg(target_os = "linux")]
mod systemd;

#[derive(Debug, Clone, Serialize)]
pub struct AutostartStatus {
    pub enabled: bool,
    pub supported: bool,
    pub description: String,
    pub method: &'static str,
    pub entry_path: Option<String>,
}

/// Reads the user's startup entry without creating or changing anything.
pub async fn status() -> Result<AutostartStatus> {
    let _lock = MUTATION.lock().await;
    tokio::task::spawn_blocking(platform_status)
        .await
        .context("Failed to query login startup")?
}

/// Changes only Babel's owned login entry. Enabling does not start a process.
pub async fn set_enabled(enabled: bool, config_path: &Path) -> Result<AutostartStatus> {
    let _lock = MUTATION.lock().await;
    let config_path = config_path.to_owned();
    tokio::task::spawn_blocking(move || platform_set(enabled, &config_path))
        .await
        .context("Failed to configure login startup")?
}

struct LaunchSpec {
    executable: PathBuf,
    config: PathBuf,
    working_dir: PathBuf,
    tray_launcher: bool,
}
impl LaunchSpec {
    fn current(config: &Path) -> Result<Self> {
        let current = std::env::current_exe().context("Could not locate the Babel executable")?;
        #[cfg(target_os = "windows")]
        let executable = current
            .parent()
            .context("Executable directory unavailable")?
            .join("babel-tray.exe");
        #[cfg(not(target_os = "windows"))]
        let executable = current;
        ensure!(
            executable.is_file(),
            "Launcher unavailable; build/install babel-tray alongside Babel and restart the application"
        );
        let working_dir = std::env::current_dir().context("Working directory unavailable")?;
        let config = std::path::absolute(config).context("Invalid configuration path")?;
        for path in [&executable, &working_dir, &config] {
            path_text(path)?;
        }
        let tray_launcher = executable
            .file_stem()
            .is_some_and(|name| name == "babel-tray");
        Ok(Self {
            executable,
            config,
            working_dir,
            tray_launcher,
        })
    }
    fn args(&self) -> Result<Vec<String>> {
        let mut args = vec![
            path_text(&self.executable)?.to_owned(),
            "--config".into(),
            path_text(&self.config)?.to_owned(),
        ];
        if !self.tray_launcher {
            args.push("serve".into());
        }
        // Each login binds a fresh OS-selected port; never persist the current port.
        args.extend(["--port".into(), "0".into()]);
        Ok(args)
    }
}
fn path_text(path: &Path) -> Result<&str> {
    let value = path
        .to_str()
        .context("Login startup requires valid Unicode paths")?;
    ensure!(
        !value.is_empty() && !value.chars().any(char::is_control),
        "Startup path is empty or contains control characters"
    );
    Ok(value)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn location() -> Result<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .map(Ok)
            .unwrap_or_else(|| user_home().map(|p| p.join(".config")))?;
        Ok(base.join("autostart/org.babel.audio.desktop"))
    }
    #[cfg(target_os = "macos")]
    {
        Ok(user_home()?.join("Library/LaunchAgents/org.babel.audio.plist"))
    }
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn user_home() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("Home directory unavailable")?;
    ensure!(home.is_absolute(), "Home directory must be absolute");
    Ok(home)
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn read_owned_file(path: &Path) -> Result<Option<String>> {
    use std::io::Read;
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("Could not inspect the login entry"),
    };
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "The existing login entry is not a regular file; nothing was changed"
    );
    ensure!(
        metadata.len() <= MAX_ENTRY_BYTES,
        "The existing login entry exceeds the limit; nothing was changed"
    );
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_ENTRY_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= MAX_ENTRY_BYTES as usize,
        "The existing login entry exceeds the limit"
    );
    let text = String::from_utf8(bytes).context("The existing login entry is not UTF-8")?;
    ensure!(
        text.lines()
            .any(|line| line == format!("# {MARKER}") || line == format!("<!-- {MARKER} -->")),
        "The existing login entry does not belong to Babel; nothing was changed"
    );
    Ok(Some(text))
}
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn set_file(path: &Path, content: Option<&str>) -> Result<()> {
    use std::io::Write;
    let existing = read_owned_file(path)?;
    if let Some(content) = content {
        let parent = path.parent().context("Login entry directory unavailable")?;
        std::fs::create_dir_all(parent).context("Could not create the startup directory")?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(content.as_bytes())?;
        temporary.as_file().sync_all()?;
        // Recheck ownership immediately before replacement. We never follow an
        // existing symlink and an initial creation must not clobber a raced file.
        let current = read_owned_file(path)?;
        ensure!(
            current == existing,
            "The login entry changed during the operation; try again"
        );
        if existing.is_some() {
            temporary.persist(path).map_err(|e| e.error)?;
        } else {
            temporary.persist_noclobber(path).map_err(|e| e.error)?;
        }
    } else if existing.is_some() {
        ensure!(
            read_owned_file(path)? == existing,
            "The login entry changed during the operation; try again"
        );
        std::fs::remove_file(path).context("Could not remove the Babel login entry")?;
    }
    Ok(())
}

#[cfg(any(target_os = "linux", test))]
fn desktop_string(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}
#[cfg(any(target_os = "linux", test))]
fn desktop_arg(value: &str) -> String {
    // Exec quoting is decoded after desktop string escapes, before field codes.
    let mut quoted = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' | '"' | '`' | '$' => {
                quoted.push('\\');
                quoted.push(ch);
            }
            '%' => quoted.push_str("%%"),
            _ => quoted.push(ch),
        }
    }
    quoted.push('"');
    desktop_string(&quoted)
}
#[cfg(any(target_os = "linux", test))]
fn desktop_entry(spec: &LaunchSpec) -> Result<String> {
    ensure!(
        !path_text(&spec.executable)?.contains('='),
        "The XDG standard does not accept '=' in the executable path; move Babel to another directory"
    );
    let mut args = spec.args()?;
    // GLib resolves argv[0] before expanding %% and therefore cannot directly
    // launch a filename containing '%'. env execs that exact path as an argument
    // after GIO has expanded field codes; no shell or persistent helper is used.
    if path_text(&spec.executable)?.contains('%') {
        ensure!(
            Path::new("/usr/bin/env").is_file(),
            "The executable path contains '%' and requires /usr/bin/env for XDG startup"
        );
        args.splice(0..0, ["/usr/bin/env".into(), "--".into()]);
    }
    let command = args
        .iter()
        .map(|arg| desktop_arg(arg))
        .collect::<Vec<_>>()
        .join(" ");
    Ok(format!(
        "[Desktop Entry]\n# {MARKER}\nType=Application\nName=Babel\nComment=Voice translation dashboard\nExec={command}\nPath={}\nTerminal=false\nHidden=false\nX-GNOME-Autostart-enabled=true\n",
        desktop_string(path_text(&spec.working_dir)?).replace(' ', "\\s")
    ))
}
#[cfg(any(target_os = "macos", test))]
fn xml_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
#[cfg(any(target_os = "macos", test))]
fn launch_agent(spec: &LaunchSpec) -> Result<String> {
    let args = spec
        .args()?
        .iter()
        .map(|arg| format!("<string>{}</string>", xml_text(arg)))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!-- {MARKER} -->\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>org.babel.audio</string>\n<key>ProgramArguments</key><array>\n{args}\n</array>\n<key>WorkingDirectory</key><string>{}</string>\n<key>RunAtLoad</key><true/>\n<key>KeepAlive</key><false/>\n<key>LimitLoadToSessionType</key><string>Aqua</string>\n</dict></plist>\n",
        xml_text(path_text(&spec.working_dir)?)
    ))
}

#[cfg(target_os = "linux")]
fn platform_status() -> Result<AutostartStatus> {
    systemd::status()
}
#[cfg(target_os = "linux")]
fn platform_set(enabled: bool, config: &Path) -> Result<AutostartStatus> {
    systemd::set_enabled(enabled, config)
}
#[cfg(target_os = "macos")]
fn platform_status() -> Result<AutostartStatus> {
    file_status()
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn file_status() -> Result<AutostartStatus> {
    let path = location()?;
    let text = read_owned_file(&path)?;
    let enabled = text.as_ref().is_some_and(|value| {
        !value
            .lines()
            .any(|line| line == "Hidden=true" || line == "X-GNOME-Autostart-enabled=false")
    });
    Ok(AutostartStatus {
        enabled,
        supported: true,
        method: if cfg!(target_os = "linux") {
            "xdg"
        } else {
            "launch_agent"
        },
        entry_path: Some(path_text(&path)?.to_owned()),
        description: if enabled {
            "The tray and dashboard will start at next login. Translation will remain stopped."
                .into()
        } else {
            "Login startup is disabled for this user.".into()
        },
    })
}
#[cfg(target_os = "macos")]
fn platform_set(enabled: bool, config: &Path) -> Result<AutostartStatus> {
    let path = location()?;
    if enabled {
        let spec = LaunchSpec::current(config)?;
        let content = launch_agent(&spec)?;
        set_file(&path, Some(&content))?;
    } else {
        set_file(&path, None)?;
    }
    platform_status()
}

#[cfg(any(target_os = "windows", test))]
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
#[cfg(any(target_os = "windows", test))]
fn windows_command(spec: &LaunchSpec) -> Result<String> {
    ensure!(
        spec.tray_launcher,
        "Windows startup requires babel-tray.exe"
    );
    let mut args = spec.args()?;
    args.extend([
        "--working-dir".into(),
        path_text(&spec.working_dir)?.into(),
        "--autostart-owner".into(),
        OWNER.into(),
    ]);
    let command = args
        .iter()
        .map(|arg| windows_arg(arg))
        .collect::<Vec<_>>()
        .join(" ");
    ensure!(
        command.encode_utf16().count() <= 260,
        "The command exceeds the Windows Run limit of 260 characters; install Babel and its configuration at shorter paths"
    );
    Ok(command)
}
#[cfg(any(target_os = "windows", test))]
fn owned_windows_command(command: &str) -> bool {
    command.ends_with(&format!(
        "{} {}",
        windows_arg("--autostart-owner"),
        windows_arg(OWNER)
    )) && command.starts_with('"')
        && command.contains("babel-tray.exe\" \"--config\"")
}

#[cfg(target_os = "windows")]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(target_os = "windows")]
const RUN_VALUE: &str = "BabelAudio";
#[cfg(target_os = "windows")]
fn registry_entry() -> Result<Option<String>> {
    let key = match winreg::HKCU.open_subkey(RUN_KEY) {
        Ok(key) => key,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("Could not read Windows startup settings"),
    };
    match key.get_value::<String, _>(RUN_VALUE) {
        Ok(command) => {
            ensure!(
                owned_windows_command(&command),
                "The Windows BabelAudio entry does not belong to Babel; nothing was changed"
            );
            Ok(Some(command))
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("Could not read the Windows BabelAudio entry"),
    }
}
#[cfg(target_os = "windows")]
fn platform_status() -> Result<AutostartStatus> {
    let enabled = registry_entry()?.is_some();
    Ok(AutostartStatus {
        enabled,
        supported: true,
        method: "registry_run",
        entry_path: Some(format!("HKCU\\{RUN_KEY}\\{RUN_VALUE}")),
        description: if enabled {
            "The tray is registered for next login. Translation will remain stopped; Windows may block startup applications in Settings.".into()
        } else {
            "Login startup is disabled for this user.".into()
        },
    })
}
#[cfg(target_os = "windows")]
fn platform_set(enabled: bool, config: &Path) -> Result<AutostartStatus> {
    let existing = registry_entry()?;
    let command = if enabled {
        Some(windows_command(&LaunchSpec::current(config)?)?)
    } else {
        None
    };
    ensure!(
        registry_entry()? == existing,
        "The Windows entry changed during the operation; try again"
    );
    if let Some(command) = command {
        let (key, _) = winreg::HKCU
            .create_subkey(RUN_KEY)
            .context("Could not open Windows startup settings for writing")?;
        key.set_value(RUN_VALUE, &command)
            .context("Could not save Windows startup settings")?;
    } else if existing.is_some() {
        let key = winreg::HKCU.open_subkey_with_flags(RUN_KEY, winreg::enums::KEY_SET_VALUE)?;
        key.delete_value(RUN_VALUE)
            .context("Could not remove the Windows startup entry")?;
    }
    platform_status()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn spec() -> LaunchSpec {
        LaunchSpec {
            executable: "/opt/Babel voice/babel".into(),
            config: "/home/joão/Babel voice/settings.toml".into(),
            working_dir: "/home/joão/Babel voice".into(),
            tray_launcher: false,
        }
    }
    #[test]
    fn desktop_escaping_preserves_literal_metacharacters_without_shell() {
        assert_eq!(
            desktop_arg("/apps/a b/$voice`x`%20\\a\"b"),
            r#""/apps/a b/\\$voice\\`x\\`%%20\\\\a\\"b""#
        );
        let entry = desktop_entry(&spec()).unwrap();
        assert!(entry.contains(
            "\"--config\" \"/home/joão/Babel voice/settings.toml\" \"serve\" \"--port\" \"0\""
        ));
        assert!(entry.contains("Path=/home/joão/Babel\\svoice\nTerminal=false"));
        assert!(
            !entry.contains(" run ") && !entry.contains("api_key") && !entry.contains("token=")
        );
        let mut invalid = spec();
        invalid.executable = "/tmp/a=b/babel".into();
        assert!(desktop_entry(&invalid).is_err());
        assert!(path_text(Path::new("/tmp/config\nExec=anything")).is_err());
    }
    #[test]
    fn launch_agent_uses_xml_arguments_and_never_restarts_translation() {
        let mut input = spec();
        input.config = "/Users/João/A&B/<config>\"'.toml".into();
        let entry = launch_agent(&input).unwrap();
        assert!(entry.contains("/Users/João/A&amp;B/&lt;config&gt;&quot;&apos;.toml"));
        assert!(entry.contains("<string>serve</string>"));
        assert!(entry.contains("<string>--port</string>\n<string>0</string>"));
        assert!(
            entry.contains("<key>WorkingDirectory</key><string>/home/joão/Babel voice</string>")
        );
        assert!(entry.contains("<key>RunAtLoad</key><true/>"));
        assert!(entry.contains("<key>KeepAlive</key><false/>"));
        assert!(!entry.contains("<string>run</string>") && !entry.contains("EnvironmentVariables"));
    }
    #[test]
    fn windows_arguments_escape_quotes_and_trailing_backslashes() {
        assert_eq!(
            windows_arg(r"C:\Program Files\Babel\"),
            "\"C:\\Program Files\\Babel\\\\\""
        );
        assert_eq!(windows_arg("a\\\"b"), "\"a\\\\\\\"b\"");
        let input = LaunchSpec {
            executable: r"C:\Babel\babel-tray.exe".into(),
            config: r"C:\Babel\a b.toml".into(),
            working_dir: r"C:\Babel".into(),
            tray_launcher: true,
        };
        let command = windows_command(&input).unwrap();
        assert!(owned_windows_command(&command));
        assert!(!owned_windows_command(&format!("{command} extra")));
        assert!(!owned_windows_command(
            &command.replace("babel-tray.exe", "other.exe")
        ));
        assert!(command.contains("\"--working-dir\" \"C:\\Babel\""));
        assert!(command.contains("\"--port\" \"0\""));
        assert!(!command.contains("\"serve\"") && !command.contains("\"run\""));
        let mut too_long = input;
        too_long.config = format!("C:\\{}\\config.toml", "x".repeat(261)).into();
        assert!(windows_command(&too_long).is_err());
    }
    #[test]
    fn isolated_file_lifecycle_is_idempotent_and_refuses_foreign_entries() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("autostart/org.babel.audio.desktop");
        assert!(read_owned_file(&path).unwrap().is_none());
        assert!(
            !path.parent().unwrap().exists(),
            "read-only status must not create directories"
        );
        let entry = desktop_entry(&spec()).unwrap();
        set_file(&path, Some(&entry)).unwrap();
        set_file(&path, Some(&entry)).unwrap();
        assert_eq!(
            read_owned_file(&path).unwrap().as_deref(),
            Some(entry.as_str())
        );
        set_file(&path, None).unwrap();
        set_file(&path, None).unwrap();
        assert!(!path.exists());
        std::fs::write(&path, "[Desktop Entry]\nExec=someone-else\n").unwrap();
        assert!(set_file(&path, Some(&entry)).is_err());
        assert!(set_file(&path, None).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[Desktop Entry]\nExec=someone-else\n"
        );
    }
    #[cfg(unix)]
    #[test]
    fn startup_symlinks_are_not_followed_or_deleted() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        let link = directory.path().join("link.desktop");
        let content = desktop_entry(&spec()).unwrap();
        std::fs::write(&target, &content).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(set_file(&link, None).is_err());
        assert!(set_file(&link, Some(&content)).is_err());
        assert_eq!(std::fs::read_to_string(target).unwrap(), content);
        assert!(
            std::fs::symlink_metadata(link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires gio and desktop-file-validate; launches only a temporary argument recorder"]
    fn desktop_launcher_preserves_actual_arguments_through_gio() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let working_dir = directory.path().join("Área de trabalho ");
        std::fs::create_dir(&working_dir).unwrap();
        let script = working_dir.join("Babel $voice`x`%20\\\"'");
        std::fs::write(&script, "#!/bin/sh\nprintf '%s\\n' \"$@\" > received.txt\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let input = LaunchSpec {
            executable: script,
            config: working_dir.join("Config $name`x`%20\\\"'.toml"),
            working_dir: working_dir.clone(),
            tray_launcher: false,
        };
        let entry = directory.path().join("test.desktop");
        std::fs::write(&entry, desktop_entry(&input).unwrap()).unwrap();
        assert!(
            std::process::Command::new("desktop-file-validate")
                .arg(&entry)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            std::process::Command::new("gio")
                .arg("launch")
                .arg(&entry)
                .status()
                .unwrap()
                .success()
        );
        let output = working_dir.join("received.txt");
        for _ in 0..100 {
            if output.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            std::fs::read_to_string(output)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            input.args().unwrap()[1..]
        );
    }
}
