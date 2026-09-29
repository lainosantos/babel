//! Packaged Linux user service. The manager is injectable; tests never call it.
use super::{
    AutostartStatus, LaunchSpec, MARKER, MAX_ENTRY_BYTES, desktop_entry, path_text,
    read_owned_file, set_file,
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeMap,
    io::{ErrorKind, Read},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const UNIT: &str = "org.babel.audio.service";
const PACKAGED: &str = include_str!("../../packaging/linux/org.babel.audio.service");
const MANAGER_ERROR: &str = "Não foi possível consultar ou configurar o systemd de usuário.";
const IDENTITY_ERROR: &str = "A unidade systemd existente não corresponde ao serviço empacotado do Babel; nada foi alterado.";
const OVERRIDE_ERROR: &str =
    "O serviço Babel possui uma substituição systemd não gerenciada; nada foi alterado.";
const OFFLINE_ERROR: &str = "O serviço Babel já possui uma entrada systemd, mas o gerenciador de usuário está indisponível; tente novamente na sessão gráfica.";

struct Paths {
    package: PathBuf,
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
            .context("Diretório da entrada de login indisponível")?;
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
    fn wanted(&self) -> Result<bool> {
        let metadata = match std::fs::symlink_metadata(&self.wanted) {
            Ok(m) => m,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e).context(OVERRIDE_ERROR),
        };
        ensure!(metadata.file_type().is_symlink(), OVERRIDE_ERROR);
        ensure!(
            self.wanted.canonicalize().context(OVERRIDE_ERROR)?
                == self.package.canonicalize().context(IDENTITY_ERROR)?,
            OVERRIDE_ERROR
        );
        Ok(true)
    }
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
    if !paths.package_present()? {
        ensure!(
            std::fs::symlink_metadata(&paths.wanted)
                .is_err_and(|e| e.kind() == ErrorKind::NotFound),
            OFFLINE_ERROR
        );
        return Ok(None);
    }
    let wanted = paths.wanted()?;
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
        Path::new(fragment).canonicalize().ok() == paths.package.canonicalize().ok(),
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
    let graphical = paths.allow_user_service
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
        });
    Ok(Some(Snapshot { enabled, graphical }))
}

fn service_status(paths: &Paths, enabled: bool) -> Result<AutostartStatus> {
    Ok(AutostartStatus {
        enabled,
        supported: true,
        method: "systemd_user",
        entry_path: Some(path_text(&paths.dropin)?.to_owned()),
        description: if enabled {
            "Início automático pelo serviço de usuário systemd habilitado para o próximo login gráfico.".into()
        } else {
            "Início automático pelo serviço de usuário systemd desativado.".into()
        },
    })
}

fn unit_arg(value: &str) -> Result<String> {
    ensure!(
        !value.chars().any(char::is_control),
        "Caminho de inicialização vazio ou com caracteres de controle"
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
fn dropin(spec: &LaunchSpec) -> Result<String> {
    let config = path_text(&spec.config)?;
    let working = path_text(&spec.working_dir)?;
    ensure!(
        spec.config.is_absolute() && spec.working_dir.is_absolute(),
        "Caminho da configuração inválido"
    );
    // ':' disables systemd environment expansion; %% disables specifiers. The
    // packaged launcher passes argv directly to babel-tray, which changes cwd.
    // Avoid WorkingDirectory= because that setting has different quote rules.
    Ok(format!(
        "# {MARKER}\n[Service]\nExecStart=\nExecStart=:/usr/bin/babel-launch --config {} --port 0 --working-dir {} --autostart-owner {}\n",
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
        "A entrada de login mudou durante a operação; tente novamente"
    );
    if enabled {
        let content = dropin(spec.context("Caminho da configuração inválido")?)?;
        ensure!(
            read_owned_file(&paths.dropin)? == previous,
            "A entrada de login mudou durante a operação; tente novamente"
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
                "A entrada de login mudou durante a operação; tente novamente"
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
                "A entrada de login mudou durante a operação; tente novamente"
            );
            set_file(&paths.dropin, previous.as_deref())
                .context("Falha ao restaurar a entrada de login após erro do systemd.")?;
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
            "A entrada de login mudou durante a operação; tente novamente"
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
    status_for(&paths, &mut Systemctl, desktop)
}
fn status_for(
    paths: &Paths,
    manager: &mut impl Manager,
    desktop: AutostartStatus,
) -> Result<AutostartStatus> {
    match snapshot(paths, manager)? {
        Some(s) if !s.enabled && desktop.enabled => Ok(desktop),
        Some(s) if s.enabled || s.graphical => service_status(paths, s.enabled),
        _ => Ok(desktop),
    }
}
pub(super) fn set_enabled(enabled: bool, config: &Path) -> Result<AutostartStatus> {
    let paths = Paths::current()?;
    let mut manager = Systemctl;
    let spec = enabled.then(|| LaunchSpec::current(config)).transpose()?;
    if let Some(s) = snapshot(&paths, &mut manager)?.filter(|s| s.enabled || s.graphical) {
        return change(&paths, &mut manager, enabled, spec.as_ref(), &s);
    }
    let content = spec.as_ref().map(desktop_entry).transpose()?;
    set_file(&paths.desktop, content.as_deref())?;
    super::file_status()
}

#[cfg(test)]
mod tests;
