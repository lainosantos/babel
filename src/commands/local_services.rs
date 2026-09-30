//! Owned, loopback-only inference children. The child binds port 0 and reports
//! its already-bound endpoint; Babel never probes/reserves a guessed port.
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, BufReader},
    process::{Child, Command},
    task::JoinHandle,
};

use super::AgentConfig;

const READY_PREFIX: &[u8] = b"BABEL_SERVICE_READY ";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
struct Announcement {
    service: String,
    endpoint: String,
    port: u16,
    #[serde(default)]
    pid: Option<u32>,
}

struct ServiceProcess {
    child: Child,
    endpoint: String,
    drain: JoinHandle<()>,
}

impl Drop for ServiceProcess {
    fn drop(&mut self) {
        self.drain.abort();
        // kill_on_drop remains set as well; never leave a helper serving a
        // stale configuration after the owning Babel process exits normally.
        let _ = self.child.start_kill();
    }
}

impl ServiceProcess {
    async fn start(mut command: Command, service: &str, path: &str) -> Result<Self> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        crate::execution::configure_background_process(&mut command);
        let mut child = command
            .spawn()
            .context("Could not start an installed local voice service")?;
        let pid = child.id();
        let mut output = BufReader::new(
            child
                .stdout
                .take()
                .context("Local voice service has no readiness output")?,
        );
        let endpoint = tokio::time::timeout(STARTUP_TIMEOUT, async {
            let mut total = 0;
            while let Some(line) = bounded_line(&mut output).await? {
                total += line.len();
                ensure!(total <= 1024 * 1024, "Local voice service exceeded the startup output limit");
                if let Some(json) = line.strip_prefix(READY_PREFIX) {
                    return announced_endpoint(json, service, path, pid);
                }
            }
            bail!("Local voice service exited before announcing a bound port; verify its installation and model")
        }).await.context("Local voice service did not announce its bound port within 60 seconds")??;
        ensure!(
            child.try_wait()?.is_none(),
            "Local voice service exited during startup"
        );
        // Whisper/runtime output may contain transcripts. Drain it to a sink,
        // never to a persistent log or a UI error; memory stays bounded.
        let drain = tokio::spawn(async move {
            let _ = tokio::io::copy(&mut output, &mut tokio::io::sink()).await;
        });
        Ok(Self {
            child,
            endpoint,
            drain,
        })
    }

    async fn stop(mut self) {
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(1), self.child.wait()).await;
    }
}

async fn bounded_line(reader: &mut (impl AsyncBufRead + Unpin)) -> Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            return Ok((!line.is_empty()).then_some(line));
        }
        let newline = chunk.iter().position(|byte| *byte == b'\n');
        let len = newline.unwrap_or(chunk.len());
        ensure!(
            line.len() + len <= 8192,
            "Local voice service readiness line exceeds the limit"
        );
        line.extend_from_slice(&chunk[..len]);
        reader.consume(len + usize::from(newline.is_some()));
        if newline.is_some() {
            return Ok(Some(line));
        }
    }
}

fn announced_endpoint(json: &[u8], expected: &str, path: &str, pid: Option<u32>) -> Result<String> {
    let announcement: Announcement =
        serde_json::from_slice(json).context("Invalid local voice service readiness response")?;
    let url = reqwest::Url::parse(&announcement.endpoint)?;
    ensure!(
        announcement.service == expected
            && announcement.port != 0
            && url.scheme() == "http"
            && url.host_str() == Some("127.0.0.1")
            && url.port() == Some(announcement.port)
            && url.path() == path
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && announcement.pid.is_none_or(|value| Some(value) == pid),
        "Local voice service announced an invalid or non-loopback endpoint"
    );
    Ok(announcement.endpoint)
}

pub(super) struct ManagedServices {
    default_root: Option<PathBuf>,
    root: Option<PathBuf>,
    whisper: Option<ServiceProcess>,
    whisper_key: Option<(PathBuf, u32)>,
    needle: Option<ServiceProcess>,
    needle_key: Option<zeroize::Zeroizing<String>>,
    needle_limits: Option<(u32, u32)>,
}

impl ManagedServices {
    pub fn new(default_root: Option<PathBuf>) -> Self {
        Self {
            default_root,
            root: None,
            whisper: None,
            whisper_key: None,
            needle: None,
            needle_key: None,
            needle_limits: None,
        }
    }

    pub async fn stop(&mut self) {
        let whisper = self.whisper.take();
        let needle = self.needle.take();
        self.needle_key = None;
        self.whisper_key = None;
        self.needle_limits = None;
        tokio::join!(
            async {
                if let Some(process) = whisper {
                    process.stop().await;
                }
            },
            async {
                if let Some(process) = needle {
                    process.stop().await;
                }
            }
        );
    }

    pub async fn resolve(&mut self, config: &AgentConfig) -> Result<AgentConfig> {
        let mut effective = config.clone();
        let automatic = config.whisper_endpoint == "auto" || config.needle_endpoint == "auto";
        if !automatic {
            self.stop().await;
            return Ok(effective);
        }
        let root = if config.services_directory.is_empty() {
            self.default_root.clone().context(
                "Choose an absolute local services folder before using automatic voice services",
            )?
        } else {
            PathBuf::from(&config.services_directory)
        };
        ensure!(
            root.is_absolute(),
            "Local voice services folder must be absolute"
        );
        if self.root.as_ref() != Some(&root) {
            self.stop().await;
            self.root = Some(root.clone());
        }
        if self
            .whisper
            .as_mut()
            .is_some_and(|p| !p.child.try_wait().is_ok_and(|s| s.is_none()))
        {
            self.whisper = None;
        }
        if self
            .needle
            .as_mut()
            .is_some_and(|p| !p.child.try_wait().is_ok_and(|s| s.is_none()))
        {
            self.needle = None;
        }
        if config.whisper_endpoint == "auto" {
            let model = whisper_model(&root, &config.whisper_model)?;
            let threads = crate::execution::inference_threads(config.local_threads);
            let key = (model.clone(), threads);
            if self.whisper_key.as_ref() != Some(&key)
                && let Some(process) = self.whisper.take()
            {
                process.stop().await;
            }
            if self.whisper.is_none() {
                let executable = whisper_binary(&root).context("Whisper is not installed in the local services folder; run scripts/setup_whisper.py")?;
                let mut command = crate::execution::background_command(executable);
                command
                    .args(["--host", "127.0.0.1", "--port", "0", "-t"])
                    .arg(threads.to_string())
                    .arg("-m")
                    .arg(model);
                self.whisper =
                    Some(ServiceProcess::start(command, "babel-whisper", "/inference").await?);
                self.whisper_key = Some(key);
            }
            effective.whisper_endpoint = self.whisper.as_ref().unwrap().endpoint.clone();
        } else if let Some(process) = self.whisper.take() {
            process.stop().await;
        }
        if config.needle_endpoint == "auto" {
            let key = if config.needle_api_key_env.is_empty() {
                None
            } else {
                Some(crate::credentials::get(&config.needle_api_key_env)?)
            };
            let limits = (config.idle_unload_secs, config.timeout_secs);
            if (self.needle_key != key || self.needle_limits != Some(limits))
                && let Some(process) = self.needle.take()
            {
                process.stop().await;
            }
            if self.needle.is_none() {
                let python = if cfg!(windows) {
                    root.join(".tools/needle/Scripts/python.exe")
                } else {
                    root.join(".tools/needle/bin/python")
                };
                let script = root.join("scripts/needle_bridge.py");
                ensure!(
                    python.is_file() && script.is_file(),
                    "Needle is not installed in the local services folder; install cactus-needle in .tools/needle"
                );
                let mut command = crate::execution::background_command(python);
                command
                    .arg(script)
                    .args(["--port", "0", "--idle-unload-secs"])
                    .arg(config.idle_unload_secs.to_string())
                    .arg("--timeout-secs")
                    .arg(config.timeout_secs.to_string())
                    .env("NEEDLE_TELEMETRY", "0")
                    .env("DO_NOT_TRACK", "1")
                    .env("HF_HUB_OFFLINE", "1");
                if let Some(key) = &key {
                    command
                        .args(["--api-key-env", &config.needle_api_key_env])
                        .env(&config.needle_api_key_env, key.as_str());
                }
                self.needle =
                    Some(ServiceProcess::start(command, "babel-needle", "/complete").await?);
                self.needle_key = key;
                self.needle_limits = Some(limits);
            }
            effective.needle_endpoint = self.needle.as_ref().unwrap().endpoint.clone();
        } else {
            self.needle_key = None;
            if let Some(process) = self.needle.take() {
                process.stop().await;
            }
        }
        Ok(effective)
    }

    pub async fn wait_for_exit(&mut self) -> Result<()> {
        loop {
            for process in [&mut self.whisper, &mut self.needle].into_iter().flatten() {
                ensure!(
                    process.child.try_wait()?.is_none(),
                    "A managed voice service stopped; restarting on a new available port"
                );
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
}

fn whisper_model(root: &Path, model: &str) -> Result<PathBuf> {
    let directory = root.join(".tools/whisper.cpp/models");
    let path = directory.join(format!("ggml-{model}.bin"));
    if path.is_file() {
        return Ok(path);
    }
    // Preserve installations made before the compact default was introduced.
    // The next setup upgrades the model; an explicit tiny/small selection must
    // never silently load a different, larger model.
    let legacy = directory.join("ggml-base.bin");
    if model == crate::config::DEFAULT_WHISPER_MODEL && legacy.is_file() {
        return Ok(legacy);
    }
    bail!(
        "Selected command Whisper model is not installed; run scripts/setup_whisper.py or install its ggml model in the local services folder"
    )
}

fn whisper_binary(root: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "whisper-server.exe"
    } else {
        "whisper-server"
    };
    [
        root.join(".tools/whisper.cpp/build/bin").join(name),
        root.join(".tools/whisper.cpp/build/bin/Release").join(name),
    ]
    .into_iter()
    .find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const FIXTURE_MODE: &str = "BABEL_TEST_LOCAL_VOICE_HELPER_MODE";
    const FIXTURE_REPORT: &str = "BABEL_TEST_LOCAL_VOICE_HELPER_REPORT";

    #[test]
    fn compact_model_preferred_legacy_installation_survives_and_explicit_models_never_fall_back() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join(".tools/whisper.cpp/models");
        std::fs::create_dir_all(&directory).unwrap();
        assert!(whisper_model(root.path(), crate::config::DEFAULT_WHISPER_MODEL).is_err());
        let legacy = directory.join("ggml-base.bin");
        std::fs::write(&legacy, b"fixture").unwrap();
        assert_eq!(
            whisper_model(root.path(), crate::config::DEFAULT_WHISPER_MODEL).unwrap(),
            legacy
        );
        let compact = directory.join("ggml-base-q5_1.bin");
        std::fs::write(&compact, b"fixture").unwrap();
        assert_eq!(
            whisper_model(root.path(), crate::config::DEFAULT_WHISPER_MODEL).unwrap(),
            compact
        );
        assert_eq!(whisper_model(root.path(), "base").unwrap(), legacy);
        assert!(whisper_model(root.path(), "tiny-q5_1").is_err());
        assert!(whisper_model(root.path(), "small").is_err());
    }

    // Invoked only by this module's child-process tests. A normal --ignored test
    // run without its private environment variable performs no I/O or waiting.
    #[test]
    #[ignore = "synthetic child fixture; requires the private test environment"]
    fn helper_process() {
        use std::io::Write;

        let Ok(mode) = std::env::var(FIXTURE_MODE) else {
            return;
        };
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let ready = json!({
            "service": "babel-needle",
            "endpoint": format!("http://127.0.0.1:{port}/complete"),
            "port": port,
            "pid": std::process::id(),
        });
        if let Some(report) = std::env::var_os(FIXTURE_REPORT) {
            std::fs::write(report, serde_json::to_vec(&ready).unwrap()).unwrap();
        }
        match mode.as_str() {
            // libtest can print its test name without a final newline. Ensure
            // the real protocol always begins at the start of its own line.
            "ready" => println!("\nBABEL_SERVICE_READY {ready}"),
            "malformed" => println!("\nBABEL_SERVICE_READY {{broken-json"),
            "silent" => {}
            "exit" => return,
            _ => panic!("unknown synthetic helper mode"),
        }
        std::io::stdout().flush().unwrap();
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let _ = writeln!(stream, "{}", std::process::id());
        }
    }

    fn fixture_command(mode: &str, report: Option<&Path>) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args([
            "--ignored",
            "--exact",
            "commands::local_services::tests::helper_process",
            "--nocapture",
            "--test-threads=1",
        ]);
        command.env(FIXTURE_MODE, mode);
        if let Some(report) = report {
            command.env(FIXTURE_REPORT, report);
        } else {
            command.env_remove(FIXTURE_REPORT);
        }
        command
    }

    async fn fixture_identity(endpoint: &str) -> u32 {
        tokio::time::timeout(Duration::from_secs(3), async {
            let port = reqwest::Url::parse(endpoint).unwrap().port().unwrap();
            let stream = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
                .await
                .unwrap();
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line).await.unwrap();
            line.trim().parse().unwrap()
        })
        .await
        .expect("synthetic helper must respond from its announced socket")
    }

    async fn assert_port_closed(endpoint: &str) {
        let port = reqwest::Url::parse(endpoint).unwrap().port().unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
                    .await
                    .is_err()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("owned helper socket must close after stop or drop");
    }

    async fn fixture_report(path: &Path) -> serde_json::Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(bytes) = tokio::fs::read(path).await
                    && let Ok(report) = serde_json::from_slice(&bytes)
                {
                    return report;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("fixture must bind and write its test-only report")
    }

    #[tokio::test]
    async fn independent_children_announce_their_own_sockets_and_stop_or_drop_closes_only_the_owner()
     {
        let first = tokio::time::timeout(
            Duration::from_secs(5),
            ServiceProcess::start(fixture_command("ready", None), "babel-needle", "/complete"),
        )
        .await
        .unwrap()
        .unwrap();
        let second = tokio::time::timeout(
            Duration::from_secs(5),
            ServiceProcess::start(fixture_command("ready", None), "babel-needle", "/complete"),
        )
        .await
        .unwrap()
        .unwrap();
        let first_endpoint = first.endpoint.clone();
        let second_endpoint = second.endpoint.clone();
        assert_ne!(first_endpoint, second_endpoint);
        assert_eq!(
            fixture_identity(&first_endpoint).await,
            first.child.id().unwrap()
        );
        assert_eq!(
            fixture_identity(&second_endpoint).await,
            second.child.id().unwrap()
        );
        drop(first);
        assert_port_closed(&first_endpoint).await;
        assert_eq!(
            fixture_identity(&second_endpoint).await,
            second.child.id().unwrap()
        );
        second.stop().await;
        assert_port_closed(&second_endpoint).await;
    }

    #[tokio::test]
    async fn malformed_readiness_and_exit_before_readiness_reject_and_close_owned_sockets() {
        let directory = tempfile::tempdir().unwrap();
        for mode in ["malformed", "exit"] {
            let report_path = directory.path().join(format!("{mode}.json"));
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                ServiceProcess::start(
                    fixture_command(mode, Some(&report_path)),
                    "babel-needle",
                    "/complete",
                ),
            )
            .await
            .expect("malformed or closed readiness output must fail promptly");
            let error = result
                .err()
                .expect("invalid readiness must not create a service");
            assert!(error.to_string().contains(if mode == "malformed" {
                "readiness"
            } else {
                "before announcing"
            }));
            let report = fixture_report(&report_path).await;
            assert_port_closed(report["endpoint"].as_str().unwrap()).await;
        }
    }

    #[tokio::test]
    async fn missing_readiness_times_out_and_cancellation_kills_the_pending_child() {
        let directory = tempfile::tempdir().unwrap();
        for cancel in [false, true] {
            let report_path = directory.path().join(format!("silent-{cancel}.json"));
            let command = fixture_command("silent", Some(&report_path));
            let task = tokio::spawn(ServiceProcess::start(command, "babel-needle", "/complete"));
            let report = fixture_report(&report_path).await;
            let endpoint = report["endpoint"].as_str().unwrap();
            assert_eq!(
                fixture_identity(endpoint).await,
                report["pid"].as_u64().unwrap() as u32
            );
            if cancel {
                task.abort();
                assert!(task.await.is_err());
            } else {
                // Pause only after the real OS child has bound its socket.
                // Advance the production startup deadline without a 60s wait.
                tokio::time::pause();
                tokio::time::advance(STARTUP_TIMEOUT + Duration::from_secs(1)).await;
                tokio::task::yield_now().await;
                tokio::time::resume();
                let error = task
                    .await
                    .unwrap()
                    .err()
                    .expect("silent helper must time out");
                assert!(error.to_string().contains("within 60 seconds"));
            }
            assert_port_closed(endpoint).await;
        }
    }

    #[tokio::test]
    async fn switching_to_manual_or_removing_the_managed_key_retires_the_old_child() {
        let directory = tempfile::tempdir().unwrap();
        for manual in [false, true] {
            let process = tokio::time::timeout(
                Duration::from_secs(5),
                ServiceProcess::start(fixture_command("ready", None), "babel-needle", "/complete"),
            )
            .await
            .unwrap()
            .unwrap();
            let old_endpoint = process.endpoint.clone();
            let mut services = ManagedServices::new(Some(directory.path().into()));
            services.root = Some(directory.path().into());
            services.needle = Some(process);
            services.needle_key = Some(zeroize::Zeroizing::new("old-fixture-key".to_owned()));
            let config = AgentConfig {
                whisper_endpoint: "http://127.0.0.1:43001/inference".into(),
                needle_endpoint: if manual {
                    "http://127.0.0.1:43002/complete"
                } else {
                    "auto"
                }
                .into(),
                needle_api_key_env: String::new(),
                ..Default::default()
            };
            let before = serde_json::to_value(&config).unwrap();
            let result = services.resolve(&config).await;
            if manual {
                let effective = result.unwrap();
                assert_eq!(effective.whisper_endpoint, config.whisper_endpoint);
                assert_eq!(effective.needle_endpoint, config.needle_endpoint);
            } else {
                // With the old key removed, do not reuse an authenticated
                // listener. This empty fixture root deliberately cannot start
                // a replacement, so the request fails rather than using it.
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("Needle is not installed")
                );
            }
            assert_eq!(serde_json::to_value(&config).unwrap(), before);
            assert!(services.needle.is_none());
            assert_port_closed(&old_endpoint).await;
        }
    }

    #[test]
    fn readiness_requires_the_child_identity_and_an_actual_loopback_port() {
        let valid = json!({"service":"babel-needle","endpoint":"http://127.0.0.1:49215/complete","port":49215,"pid":42});
        assert!(
            announced_endpoint(
                &serde_json::to_vec(&valid).unwrap(),
                "babel-needle",
                "/complete",
                Some(42)
            )
            .is_ok()
        );
        for (key, value) in [
            ("service", json!("other")),
            ("pid", json!(99)),
            ("port", json!(0)),
            ("port", json!(1234)),
            ("endpoint", json!("http://localhost:49215/complete")),
            ("endpoint", json!("http://127.0.0.1:49215/wrong")),
            (
                "endpoint",
                json!("http://127.0.0.1:49215/complete?key=secret"),
            ),
        ] {
            let mut invalid = valid.clone();
            invalid[key] = value;
            assert!(
                announced_endpoint(
                    &serde_json::to_vec(&invalid).unwrap(),
                    "babel-needle",
                    "/complete",
                    Some(42)
                )
                .is_err()
            );
        }
    }

    #[tokio::test]
    async fn readiness_parser_is_bounded_and_preserves_following_output() {
        let mut reader = BufReader::with_capacity(3, &b"initial\nready\nnext"[..]);
        assert_eq!(
            bounded_line(&mut reader).await.unwrap().unwrap(),
            b"initial"
        );
        assert_eq!(bounded_line(&mut reader).await.unwrap().unwrap(), b"ready");
        assert_eq!(bounded_line(&mut reader).await.unwrap().unwrap(), b"next");
        assert!(bounded_line(&mut reader).await.unwrap().is_none());
        let oversized = vec![b'x'; 8193];
        assert!(
            bounded_line(&mut BufReader::new(oversized.as_slice()))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn explicit_endpoints_are_preserved_and_missing_auto_installations_do_not_probe_ports() {
        let temp = tempfile::tempdir().unwrap();
        let mut services = ManagedServices::new(Some(temp.path().into()));
        let mut config = AgentConfig {
            whisper_endpoint: "http://127.0.0.1:43001/inference".into(),
            needle_endpoint: "http://127.0.0.1:43002/complete".into(),
            ..Default::default()
        };
        assert_eq!(
            services.resolve(&config).await.unwrap().whisper_endpoint,
            config.whisper_endpoint
        );
        config.whisper_endpoint = "auto".into();
        assert!(
            services
                .resolve(&config)
                .await
                .unwrap_err()
                .to_string()
                .contains("not installed")
        );
        assert!(services.whisper.is_none());
    }
}
