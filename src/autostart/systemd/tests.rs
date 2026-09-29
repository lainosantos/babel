use super::*;
use anyhow::bail;
use std::os::unix::fs::{PermissionsExt, symlink};

struct Fixture {
    _temp: tempfile::TempDir,
    paths: Paths,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let package = root.join("usr/lib/systemd/user").join(UNIT);
        std::fs::create_dir_all(package.parent().unwrap()).unwrap();
        std::fs::write(&package, PACKAGED).unwrap();
        std::fs::set_permissions(&package, std::fs::Permissions::from_mode(0o644)).unwrap();
        let paths = Paths {
            package_uid: std::fs::metadata(&package).unwrap().uid(),
            allow_user_service: true,
            package,
            desktop: root.join("home/Área de trabalho/autostart/org.babel.audio.desktop"),
            dropin: root
                .join("home/Área de trabalho/systemd/user/org.babel.audio.service.d/50-babel.conf"),
            wanted: root
                .join("home/Área de trabalho/systemd/user/graphical-session.target.wants")
                .join(UNIT),
        };
        Self { _temp: temp, paths }
    }
    fn spec(&self) -> LaunchSpec {
        LaunchSpec {
            executable: "/usr/bin/babel-tray".into(),
            config: "/home/João/Config $HOME%20\\\"'`x`.toml".into(),
            working_dir: "/home/João/Pasta $HOME%\\ final ".into(),
            tray_launcher: true,
        }
    }
}
struct Fake<'a> {
    paths: &'a Paths,
    calls: Vec<Vec<String>>,
    graphical: bool,
    graphical_environment: bool,
    offline: bool,
    fail_enable: bool,
    extra_dropin: bool,
    foreign_fragment: bool,
}
impl<'a> Fake<'a> {
    fn new(paths: &'a Paths) -> Self {
        Self {
            paths,
            calls: Vec::new(),
            graphical: true,
            graphical_environment: true,
            offline: false,
            fail_enable: false,
            extra_dropin: false,
            foreign_fragment: false,
        }
    }
}
impl Manager for Fake<'_> {
    fn run(&mut self, args: &[&str]) -> Result<String> {
        self.calls
            .push(args.iter().map(|s| s.to_string()).collect());
        if self.offline {
            bail!(MANAGER_ERROR);
        }
        match args[0] {
            "show-environment" => Ok(if self.graphical_environment {
                "DISPLAY=:1\nSECRET_FOR_TEST=not-returned\n"
            } else {
                "USER=ana\n"
            }
            .into()),
            "show" if args[1] == UNIT => {
                let fragment = if self.foreign_fragment {
                    self.paths.desktop.as_path()
                } else {
                    self.paths.package.as_path()
                };
                let mut dropins = if self.paths.dropin.exists() {
                    format!("\"{}\"", self.paths.dropin.display())
                } else {
                    String::new()
                };
                if self.extra_dropin {
                    dropins.push_str(" /etc/systemd/user/org.babel.audio.service.d/admin.conf");
                }
                let state = if self.paths.wanted.exists() {
                    "enabled"
                } else {
                    "disabled"
                };
                Ok(format!(
                    "FragmentPath={}\nDropInPaths={dropins}\nLoadState=loaded\nUnitFileState={state}\n",
                    fragment.display()
                ))
            }
            "show" => Ok(if self.graphical {
                "active\n"
            } else {
                "inactive\n"
            }
            .into()),
            "enable" => {
                std::fs::create_dir_all(self.paths.wanted.parent().unwrap())?;
                if !self.paths.wanted.exists() {
                    symlink(&self.paths.package, &self.paths.wanted)?;
                }
                if self.fail_enable {
                    self.fail_enable = false;
                    bail!(MANAGER_ERROR);
                }
                Ok(String::new())
            }
            "disable" => {
                if self.paths.wanted.exists() {
                    std::fs::remove_file(&self.paths.wanted)?;
                }
                Ok(String::new())
            }
            "daemon-reload" => Ok(String::new()),
            _ => panic!("Unexpected manager action: {args:?}"),
        }
    }
}

#[test]
fn packaged_identity_refuses_foreign_writable_and_symlink_units() {
    let f = Fixture::new();
    assert!(f.paths.package_present().unwrap());
    std::fs::write(
        &f.paths.package,
        PACKAGED.replace("/usr/bin/babel-launch", "/bin/other"),
    )
    .unwrap();
    assert!(f.paths.package_present().is_err());
    std::fs::write(&f.paths.package, PACKAGED).unwrap();
    std::fs::set_permissions(&f.paths.package, std::fs::Permissions::from_mode(0o666)).unwrap();
    assert!(f.paths.package_present().is_err());
    let original = f.paths.package.with_extension("original");
    std::fs::rename(&f.paths.package, &original).unwrap();
    symlink(&original, &f.paths.package).unwrap();
    assert!(f.paths.package_present().is_err());
}

#[test]
fn missing_package_and_manager_allow_fallback_without_mutating_any_entry() {
    let f = Fixture::new();
    let mut manager = Fake::new(&f.paths);
    manager.offline = true;
    assert!(snapshot(&f.paths, &mut manager).unwrap().is_none());
    assert!(!f.paths.dropin.exists() && !f.paths.desktop.exists());
    std::fs::remove_file(&f.paths.package).unwrap();
    manager.calls.clear();
    assert!(snapshot(&f.paths, &mut manager).unwrap().is_none());
    assert!(manager.calls.is_empty());
}

#[test]
fn inactive_graphical_target_is_detected_and_existing_service_is_not_hidden() {
    let f = Fixture::new();
    let mut manager = Fake::new(&f.paths);
    manager.graphical = false;
    let state = snapshot(&f.paths, &mut manager).unwrap().unwrap();
    assert!(!state.graphical && !state.enabled);
    manager.run(&["enable", UNIT]).unwrap();
    let state = snapshot(&f.paths, &mut manager).unwrap().unwrap();
    assert!(!state.graphical && state.enabled);
    manager.offline = true;
    assert!(snapshot(&f.paths, &mut manager).is_err());
}

#[test]
fn graphical_target_without_display_environment_uses_fallback() {
    let f = Fixture::new();
    let mut manager = Fake::new(&f.paths);
    manager.graphical_environment = false;
    let state = snapshot(&f.paths, &mut manager).unwrap().unwrap();
    assert!(!state.graphical && !state.enabled);
    assert!(!f.paths.dropin.exists());
}

#[test]
fn existing_xdg_registration_remains_visible_until_explicit_migration() {
    let f = Fixture::new();
    let mut manager = Fake::new(&f.paths);
    let desktop = AutostartStatus {
        enabled: true,
        supported: true,
        method: "xdg",
        entry_path: Some(f.paths.desktop.to_string_lossy().into()),
        description: "Existing registration".into(),
    };
    let status = status_for(&f.paths, &mut manager, desktop.clone()).unwrap();
    assert!(status.enabled);
    assert_eq!(status.method, "xdg");
    assert!(!f.paths.wanted.exists() && !f.paths.dropin.exists());
    manager.run(&["enable", UNIT]).unwrap();
    let status = status_for(&f.paths, &mut manager, desktop).unwrap();
    assert!(status.enabled);
    assert_eq!(status.method, "systemd_user");
}

#[test]
fn overrides_and_global_or_ambiguous_registration_are_not_modified() {
    let f = Fixture::new();
    let mut manager = Fake::new(&f.paths);
    manager.extra_dropin = true;
    assert!(snapshot(&f.paths, &mut manager).is_err());
    manager.extra_dropin = false;
    manager.foreign_fragment = true;
    std::fs::create_dir_all(f.paths.desktop.parent().unwrap()).unwrap();
    std::fs::write(&f.paths.desktop, PACKAGED).unwrap();
    assert!(snapshot(&f.paths, &mut manager).is_err());
    assert!(
        !manager
            .calls
            .iter()
            .any(|a| ["enable", "disable"].contains(&a[0].as_str()))
    );
}

#[test]
fn enable_migrates_only_owned_desktop_and_disable_never_stops_the_process() {
    let f = Fixture::new();
    let spec = f.spec();
    let mut manager = Fake::new(&f.paths);
    set_file(&f.paths.desktop, Some(&desktop_entry(&spec).unwrap())).unwrap();
    let before = snapshot(&f.paths, &mut manager).unwrap().unwrap();
    let result = change(&f.paths, &mut manager, true, Some(&spec), &before).unwrap();
    assert!(result.enabled);
    assert_eq!(result.method, "systemd_user");
    assert_eq!(result.entry_path.as_deref(), f.paths.dropin.to_str());
    assert!(!f.paths.desktop.exists());
    assert!(f.paths.wanted.exists());
    let content = read_owned_file(&f.paths.dropin).unwrap().unwrap();
    assert_eq!(content, dropin(&spec).unwrap());
    assert!(content.contains("ExecStart=\nExecStart=:/usr/bin/babel-launch"));
    assert!(content.contains("--port 0 --working-dir"));
    assert!(content.contains("$HOME%%20\\\\\\\"'`x`.toml"));
    assert!(!content.contains("WorkingDirectory=") && !content.contains("Environment="));
    let before = snapshot(&f.paths, &mut manager).unwrap().unwrap();
    let result = change(&f.paths, &mut manager, false, None, &before).unwrap();
    assert!(!result.enabled && !f.paths.wanted.exists());
    assert_eq!(
        read_owned_file(&f.paths.dropin).unwrap().as_deref(),
        Some(content.as_str())
    );
    for call in manager.calls {
        for arg in call {
            assert!(
                !["start", "stop", "restart", "--now", "--system", "--global"]
                    .contains(&arg.as_str())
            );
        }
    }
}

#[test]
fn failed_enable_rolls_back_registration_config_and_keeps_desktop() {
    let f = Fixture::new();
    let spec = f.spec();
    let mut manager = Fake::new(&f.paths);
    manager.fail_enable = true;
    let old = format!(
        "# {MARKER}\n[Service]\nExecStart=\nExecStart=/usr/bin/babel-launch --config /old.toml\n"
    );
    let desktop = desktop_entry(&spec).unwrap();
    set_file(&f.paths.desktop, Some(&desktop)).unwrap();
    set_file(&f.paths.dropin, Some(&old)).unwrap();
    let before = snapshot(&f.paths, &mut manager).unwrap().unwrap();
    assert!(change(&f.paths, &mut manager, true, Some(&spec), &before).is_err());
    assert!(!f.paths.wanted.exists());
    assert_eq!(
        read_owned_file(&f.paths.dropin).unwrap().as_deref(),
        Some(old.as_str())
    );
    assert_eq!(
        read_owned_file(&f.paths.desktop).unwrap().as_deref(),
        Some(desktop.as_str())
    );
}

#[test]
fn foreign_desktop_is_not_deleted_or_followed_when_enabling_service() {
    let f = Fixture::new();
    let mut manager = Fake::new(&f.paths);
    std::fs::create_dir_all(f.paths.desktop.parent().unwrap()).unwrap();
    std::fs::write(&f.paths.desktop, "Exec=other").unwrap();
    let before = snapshot(&f.paths, &mut manager).unwrap().unwrap();
    manager.calls.clear();
    assert!(change(&f.paths, &mut manager, true, Some(&f.spec()), &before).is_err());
    assert!(manager.calls.is_empty());
    assert_eq!(
        std::fs::read_to_string(&f.paths.desktop).unwrap(),
        "Exec=other"
    );
}

#[test]
fn manager_property_words_preserve_literal_paths_without_shell_expansion() {
    assert_eq!(
        property_words(r#""/home/João/a b/\$HOME\`x\`\\\".conf" /other.conf"#).unwrap(),
        vec!["/home/João/a b/$HOME`x`\\\".conf", "/other.conf"]
    );
    assert!(property_words("\"unterminated").is_err());
    assert!(property_words("/path\\").is_err());
    assert!(property_words("/path\nforeign").is_err());
    assert!(unit_arg("bad\nExecStart=anything").is_err());
}

#[test]
fn manager_output_drains_past_pipe_capacity_but_enforces_size_and_timeout() {
    let run_printf = |size| {
        Command::new("/usr/bin/printf")
            .arg("%s")
            .arg("x".repeat(size))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };
    let text = bounded_output(run_printf(32768), Duration::from_secs(2)).unwrap();
    assert_eq!(text.len(), 32768);
    assert!(bounded_output(run_printf(70000), Duration::from_secs(2)).is_err());
    let sleeper = Command::new("/bin/sleep")
        .arg("10")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    assert!(bounded_output(sleeper, Duration::from_millis(20)).is_err());
    assert!(started.elapsed() < Duration::from_secs(2));
}
