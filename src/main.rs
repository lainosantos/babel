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
    about = "Tradução de voz bidirecional em tempo real com dispositivos virtuais"
)]
struct Cli {
    #[arg(long, global = true, default_value = "babel.toml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Subcommand)]
enum Command {
    /// Painel local de configuração (padrão).
    Serve {
        /// Porta do painel; 0 deixa o sistema escolher uma porta livre.
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Executa o painel e roteamento original, sem ícone na bandeja.
        #[arg(long)]
        no_tray: bool,
    },
    /// Inicia os fluxos configurados sem interface gráfica.
    Run {
        /// Nome opcional desta sessão (também identifica os arquivos de transcrição).
        #[arg(long)]
        session: Option<String>,
    },
    /// Cria uma configuração inicial; não sobrescreve arquivos existentes.
    Init,
    /// Lista os identificadores exatos de dispositivos.
    Devices,
    /// Verifica configuração, executáveis e disponibilidade dos dispositivos.
    Doctor,
    /// Cria os dispositivos virtuais no Linux; orienta os drivers nos outros SOs.
    Setup,
    /// Remove somente dispositivos virtuais criados pelo Babel.
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
                "{} já existe; edite o arquivo ou use --config com outro caminho",
                cli.config.display()
            );
            AppConfig::default().save(&cli.config)?;
            println!("Configuração criada em {}", cli.config.display());
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
                "Sistema: {}",
                babel_audio::platform::PlatformInfo::current().name
            );
            let cfg = load_or_default(&cli.config)?;
            cfg.validate()?;
            println!(
                "Configuração válida. Microfone: {}; saída: {}",
                cfg.microphone.provider, cfg.speaker.provider
            );
            for (name, profile) in [
                ("Gemini", &cfg.providers.gemini),
                ("OpenAI", &cfg.providers.openai),
                ("ElevenLabs", &cfg.providers.elevenlabs),
            ] {
                println!(
                    "{name}: modelo={}, credencial {}: {}",
                    profile.model,
                    profile.api_key_env,
                    if babel_audio::credentials::configured(&profile.api_key_env) {
                        "presente (não validada remotamente)"
                    } else {
                        "ausente"
                    }
                );
            }
            match audio::devices().await {
                Ok(devices) => println!(
                    "{} dispositivos disponíveis. Execute `babel devices` para listar.",
                    devices.len()
                ),
                Err(error) => {
                    println!("Áudio indisponível: {error:#}");
                    return Err(error);
                }
            }
            if let Err(error) = cfg.validate_for_start() {
                println!("Antes de iniciar: {error:#}");
            }
            println!("Consulte docs/platforms.md. Não foi enviado áudio à nuvem.");
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
                        tracing::warn!(
                            "Bandeja indisponível; painel continua disponível: {error:#}"
                        );
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
                .context("Execute `babel init` e configure os dispositivos primeiro")?;
            let controller = Controller::new(cfg, instance.config_path().to_owned())?;
            controller.start_named(session).await?;
            println!(
                "Sessão: {}. Ctrl+C para parar.",
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
