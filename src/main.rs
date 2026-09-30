#![forbid(unsafe_code)]

use anyhow::{Context, Result, ensure};
use babel_audio::{audio, config::AppConfig, dashboard, engine::Controller};
use clap::{Parser, Subcommand};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(
    name = "babel",
    version,
    about = "Real-time bidirectional voice translation with virtual devices"
)]
struct Cli {
    #[arg(long, global = true, default_value = "babel.toml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Subcommand)]
enum Command {
    /// Local settings dashboard (default).
    Serve {
        /// Dashboard port; 0 lets the operating system choose a free port.
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Run the dashboard and original audio routing without a tray icon.
        #[arg(long)]
        no_tray: bool,
    },
    /// Start the configured streams without a graphical interface.
    Run {
        /// Optional session name (also identifies transcript files).
        #[arg(long)]
        session: Option<String>,
    },
    /// Create initial settings without overwriting existing files.
    Init,
    /// List exact device identifiers.
    Devices,
    /// Check settings, executables and device availability.
    Doctor,
    /// Create virtual devices on Linux; provide driver guidance on other operating systems.
    Setup,
    /// Remove only virtual devices created by Babel.
    Uninstall,
}

fn main() -> Result<()> {
    babel_audio::logging::init();
    let mut cli = Cli::parse();
    let command = cli.command.take().unwrap_or(Command::Serve {
        port: 0,
        no_tray: false,
    });
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    if let Command::Serve {
        port,
        no_tray: false,
    } = &command
    {
        return babel_audio::tray::run_native(cli.config, *port);
    }
    babel_audio::execution::control_runtime()?.block_on(run(cli, command))
}

async fn run(cli: Cli, command: Command) -> Result<()> {
    match command {
        Command::Init => {
            ensure!(
                !cli.config.exists(),
                "{} already exists; edit the file or use --config with another path",
                cli.config.display()
            );
            AppConfig::default().save(&cli.config)?;
            println!("Configuration created at {}", cli.config.display());
        }
        Command::Devices => {
            println!(
                "{}",
                serde_json::to_string_pretty(&audio::devices().await?)?
            );
        }
        Command::Setup => {
            println!("{}", audio::install_virtual_devices().await?);
        }
        Command::Uninstall => {
            println!("{}", audio::uninstall_virtual_devices().await?);
        }
        Command::Doctor => {
            println!(
                "System: {}",
                babel_audio::platform::PlatformInfo::current().name
            );
            let cfg = load_or_default(&cli.config)?;
            cfg.validate()?;
            println!(
                "Configuration valid. Microphone: {}; speaker: {}",
                cfg.microphone.provider, cfg.speaker.provider
            );
            for (name, profile) in [
                ("Gemini", &cfg.providers.gemini),
                ("OpenAI", &cfg.providers.openai),
                ("ElevenLabs", &cfg.providers.elevenlabs),
            ] {
                println!(
                    "{name}: model={}, credential {}: {}",
                    profile.model,
                    profile.api_key_env,
                    if babel_audio::credentials::configured(&profile.api_key_env) {
                        "present (not validated remotely)"
                    } else {
                        "missing"
                    }
                );
            }
            match audio::devices().await {
                Ok(devices) => println!(
                    "{} devices available. Run `babel devices` to list them.",
                    devices.len()
                ),
                Err(error) => {
                    println!("Audio unavailable: {error:#}");
                    return Err(error);
                }
            }
            if let Err(error) = cfg.validate_for_start() {
                println!("Before starting: {error:#}");
            }
            println!("See docs/platforms.md. No audio was sent to the cloud.");
        }
        Command::Serve { port, no_tray } => {
            let instance = dashboard::InstanceGuard::acquire(&cli.config)?;
            let cfg = load_or_default(instance.config_path())?;
            let controller = Arc::new(Controller::new(cfg, instance.config_path().to_owned())?);
            let cancel = CancellationToken::new();
            let signal = cancel.clone();
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    signal.cancel();
                }
            });
            #[cfg(target_os = "linux")]
            let tray = if no_tray {
                None
            } else {
                match babel_audio::tray::start(controller.clone(), cancel.clone()).await {
                    Ok(tray) => Some(tray),
                    Err(error) => {
                        tracing::warn!("Tray unavailable; dashboard remains available: {error:#}");
                        None
                    }
                }
            };
            #[cfg(not(target_os = "linux"))]
            let _ = no_tray;
            let result = dashboard::serve(controller.clone(), port, cancel.clone()).await;
            cancel.cancel();
            controller.shutdown().await?;
            #[cfg(target_os = "linux")]
            if let Some(tray) = tray {
                let _ = tokio::task::spawn_blocking(move || tray.join()).await;
            }
            result?;
        }
        Command::Run { session } => {
            let instance = dashboard::InstanceGuard::acquire(&cli.config)?;
            let cfg = AppConfig::load(instance.config_path())
                .context("Run `babel init` and configure devices first")?;
            let controller = Controller::new(cfg, instance.config_path().to_owned())?;
            controller.start_named(session).await?;
            println!(
                "Session: {}. Press Ctrl+C to stop.",
                controller.status().await.session_name.unwrap_or_default()
            );
            loop {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => { controller.shutdown().await?; break; }
                    _ = tokio::time::sleep(Duration::from_millis(250)) => {
                        let status = controller.status().await;
                        if !status.running {
                            if let Some(error) = status.last_error { anyhow::bail!("{error}"); }
                            break;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn load_or_default(path: &std::path::Path) -> Result<AppConfig> {
    if path.exists() {
        AppConfig::load(path)
    } else {
        Ok(AppConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serve_asks_the_os_for_a_free_port_unless_explicitly_overridden() {
        let cli = Cli::try_parse_from(["babel", "serve"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Serve { port: 0, .. })));
        let cli = Cli::try_parse_from(["babel", "serve", "--port", "12345"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Serve { port: 12345, .. })
        ));
    }
}
