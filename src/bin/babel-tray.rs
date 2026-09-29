#![forbid(unsafe_code)]
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

//! GUI-subsystem entry point for login startup; never calls Controller::start.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "babel-tray",
    about = "Bandeja e painel do Babel; tradução inicia manualmente"
)]
struct Args {
    /// Configuration file; defaults to the current user's platform configuration folder.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Porta do painel; 0 deixa o sistema escolher uma porta livre.
    #[arg(long, default_value_t = 0)]
    port: u16,
    #[arg(long)]
    working_dir: Option<PathBuf>,
    #[arg(long, hide = true)]
    autostart_owner: Option<String>,
}
fn main() -> Result<()> {
    babel_audio::logging::init();
    let args = Args::parse();
    if let Some(owner) = args.autostart_owner {
        ensure!(
            owner == babel_audio::autostart::OWNER,
            "Identificador de inicialização inválido"
        );
    }
    if let Some(directory) = args.working_dir {
        std::env::set_current_dir(directory)
            .context("Diretório de trabalho da inicialização indisponível")?;
    }
    let path = match args.config {
        Some(path) => std::path::absolute(path)?,
        None => {
            let path = default_config_path()?;
            std::fs::create_dir_all(path.parent().context("Configuration has no parent")?)
                .context("Could not create the user's Babel configuration folder")?;
            path
        }
    };
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        babel_audio::tray::run_native(path, args.port)
    }
    #[cfg(target_os = "linux")]
    {
        use babel_audio::{config::AppConfig, dashboard, engine::Controller};
        use std::sync::Arc;
        use tokio_util::sync::CancellationToken;
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(async move {
                let instance = dashboard::InstanceGuard::acquire(&path)?;
                let config = if instance.config_path().exists() {
                    AppConfig::load(instance.config_path())?
                } else {
                    AppConfig::default()
                };
                let controller =
                    Arc::new(Controller::new(config, instance.config_path().to_owned())?);
                let cancel = CancellationToken::new();
                let signal = cancel.clone();
                tokio::spawn(async move {
                    if tokio::signal::ctrl_c().await.is_ok() {
                        signal.cancel();
                    }
                });
                let tray = match babel_audio::tray::start(controller.clone(), cancel.clone()).await
                {
                    Ok(tray) => Some(tray),
                    Err(error) => {
                        tracing::warn!(
                            "Bandeja indisponível; painel continua disponível: {error:#}"
                        );
                        None
                    }
                };
                let result = dashboard::serve(controller.clone(), args.port, cancel.clone()).await;
                cancel.cancel();
                let stopped = controller.shutdown().await;
                if let Some(tray) = tray {
                    let _ = tokio::task::spawn_blocking(move || tray.join()).await;
                }
                stopped?;
                result
            })
    }
}

fn default_config_path() -> Result<PathBuf> {
    let home = std::env::home_dir().context("User home directory is unavailable")?;
    ensure!(home.is_absolute(), "User home directory must be absolute");
    let base = user_config_directory(
        std::env::consts::OS,
        &home,
        std::env::var_os("APPDATA").map(PathBuf::from).as_deref(),
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .as_deref(),
    );
    Ok(base.join("babel.toml"))
}

fn user_config_directory(
    os: &str,
    home: &std::path::Path,
    appdata: Option<&std::path::Path>,
    xdg: Option<&std::path::Path>,
) -> PathBuf {
    match os {
        "macos" => home.join("Library/Application Support/Babel"),
        "windows" => appdata
            .filter(|path| path.is_absolute())
            .map_or_else(|| home.join("AppData/Roaming"), PathBuf::from)
            .join("Babel"),
        _ => xdg
            .filter(|path| path.is_absolute())
            .map_or_else(|| home.join(".config"), PathBuf::from)
            .join("babel"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_launcher_uses_a_dynamic_port_by_default() {
        assert_eq!(Args::try_parse_from(["babel-tray"]).unwrap().port, 0);
        assert_eq!(
            Args::try_parse_from(["babel-tray", "--port", "12345"])
                .unwrap()
                .port,
            12345
        );
    }

    #[test]
    fn installed_tray_uses_user_storage_and_keeps_explicit_configuration() {
        let home = std::env::temp_dir().join("babel-home");
        let custom = home.join("custom config");
        let relative = std::path::Path::new("relative");
        assert_eq!(
            user_config_directory("macos", &home, None, None),
            home.join("Library/Application Support/Babel")
        );
        assert_eq!(
            user_config_directory("windows", &home, Some(&custom), None),
            custom.join("Babel")
        );
        assert_eq!(
            user_config_directory("windows", &home, Some(relative), None),
            home.join("AppData/Roaming/Babel")
        );
        assert_eq!(
            user_config_directory("linux", &home, None, Some(&custom)),
            custom.join("babel")
        );
        assert_eq!(
            user_config_directory("linux", &home, None, Some(relative)),
            home.join(".config/babel")
        );
        assert!(
            Args::try_parse_from(["babel-tray"])
                .unwrap()
                .config
                .is_none()
        );
        assert_eq!(
            Args::try_parse_from(["babel-tray", "--config", "existing.toml"])
                .unwrap()
                .config,
            Some(PathBuf::from("existing.toml"))
        );
    }
}
