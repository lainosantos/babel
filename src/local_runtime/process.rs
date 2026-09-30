//! Owned, loopback-only inference children. The child binds port 0 and reports
//! its already-bound endpoint; Babel never probes/reserves a guessed port.
use std::{process::Stdio, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, BufReader},
    process::{Child, Command},
    task::JoinHandle,
};

const READY_PREFIX: &[u8] = b"BABEL_SERVICE_READY ";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Deserialize)]
struct Announcement {
    service: String,
    endpoint: String,
    port: u16,
    #[serde(default)]
    pid: Option<u32>,
}

pub(super) struct ServiceProcess {
    child: Child,
    pub endpoint: String,
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
    pub async fn start(mut command: Command, service: &str, path: &str) -> Result<Self> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW, via safe Tokio API.
        let mut child = command
            .spawn()
            .context("Could not start an installed local inference service")?;
        let pid = child.id();
        let mut output = BufReader::new(
            child
                .stdout
                .take()
                .context("Local inference service has no readiness output")?,
        );
        let endpoint = tokio::time::timeout(STARTUP_TIMEOUT, async {
            let mut total = 0;
            while let Some(line) = bounded_line(&mut output).await? {
                total += line.len();
                ensure!(total <= 1024 * 1024, "Local inference service exceeded the startup output limit");
                if let Some(json) = line.strip_prefix(READY_PREFIX) {
                    return announced_endpoint(json, service, path, pid);
                }
            }
            bail!("Local inference service exited before announcing a bound port; verify its installation and model")
        }).await.context("Local inference service did not announce its bound port within 180 seconds")??;
        ensure!(
            child.try_wait()?.is_none(),
            "Local inference service exited during startup"
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

    pub fn healthy(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub async fn stop(mut self) {
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
            "Local inference service readiness line exceeds the limit"
        );
        line.extend_from_slice(&chunk[..len]);
        reader.consume(len + usize::from(newline.is_some()));
        if newline.is_some() {
            return Ok(Some(line));
        }
    }
}

fn announced_endpoint(json: &[u8], expected: &str, path: &str, pid: Option<u32>) -> Result<String> {
    let announcement: Announcement = serde_json::from_slice(json)
        .context("Invalid local inference service readiness response")?;
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
        "Local inference service announced an invalid or non-loopback endpoint"
    );
    Ok(announcement.endpoint)
}

#[cfg(test)]
#[path = "process/tests.rs"]
mod tests;
