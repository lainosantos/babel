//! A private HTTP boundary around bundled Piper processes. Each voice owns one
//! actor so dropped HTTP requests can never desynchronize its stdin/stdout.
use std::{
    collections::BTreeMap,
    io::Cursor,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use serde::Deserialize;
use serde_json::json;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::timeout,
};
use tokio_util::sync::CancellationToken;

const READY_PREFIX: &[u8] = b"BABEL_PIPER_READY ";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
const INFERENCE_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_TEXT_BYTES: usize = 16 * 1024;
const MAX_WAV_BYTES: u64 = 6 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 8192;

pub(super) struct PiperGateway {
    pub endpoint: String,
    cancel: CancellationToken,
    server: Option<JoinHandle<()>>,
    workers: Vec<JoinHandle<()>>,
    healthy: Arc<AtomicBool>,
}
impl PiperGateway {
    pub fn healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
            && self
                .server
                .as_ref()
                .is_some_and(|server| !server.is_finished())
            && self.workers.iter().all(|worker| !worker.is_finished())
    }
}
impl Drop for PiperGateway {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(server) = &self.server {
            server.abort();
        }
        // Each actor owns a kill_on_drop Child and a private temporary folder.
        // Aborting releases them even when the HTTP caller already disappeared.
        for worker in &self.workers {
            worker.abort();
        }
    }
}

#[derive(Clone)]
struct GatewayState {
    default_voice: String,
    voices: BTreeMap<String, mpsc::Sender<Job>>,
    healthy: Arc<AtomicBool>,
}
struct Job {
    text: String,
    result: oneshot::Sender<Result<Vec<u8>>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SynthesisRequest {
    text: String,
    #[serde(default)]
    voice: Option<String>,
}
#[derive(Deserialize)]
struct Ready {
    sample_rate: u32,
}

pub(super) async fn start(
    executable: &Path,
    data: &Path,
    voices: Vec<(String, PathBuf, PathBuf)>,
    threads: u32,
) -> Result<PiperGateway> {
    ensure!(
        !voices.is_empty() && voices.len() <= 8,
        "Managed Piper requires 1..8 voices"
    );
    ensure!(
        (1..=64).contains(&threads),
        "Local inference threads must be 1..64"
    );
    // Piper's public C API does not expose ONNX thread settings. Do not invent
    // an environment variable or flag that its runtime would silently ignore.
    let mut commands = Vec::with_capacity(voices.len());
    for (id, model, config) in voices {
        ensure!(
            !id.is_empty() && id.len() <= 200,
            "Invalid managed Piper voice"
        );
        let mut command = crate::execution::background_command(executable);
        command
            .arg("-m")
            .arg(model)
            .arg("-c")
            .arg(config)
            .arg("--espeak-data")
            .arg(data)
            .arg("--json-input");
        commands.push((id, command));
    }
    start_commands(commands, STARTUP_TIMEOUT, INFERENCE_TIMEOUT).await
}

async fn start_commands(
    commands: Vec<(String, Command)>,
    startup_timeout: Duration,
    inference_timeout: Duration,
) -> Result<PiperGateway> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .context("Could not bind managed Piper gateway")?;
    let endpoint = format!("http://{}/synthesize", listener.local_addr()?);
    let mut gateway = PiperGateway {
        endpoint,
        cancel: CancellationToken::new(),
        server: None,
        workers: Vec::new(),
        healthy: Arc::new(AtomicBool::new(true)),
    };
    let default_voice = commands
        .first()
        .context("Managed Piper needs a voice")?
        .0
        .clone();
    let mut voices = BTreeMap::new();
    for (id, command) in commands {
        ensure!(!voices.contains_key(&id), "Duplicate managed Piper voice");
        let process = VoiceProcess::start(command, startup_timeout).await?;
        let (sender, receiver) = mpsc::channel(2);
        let cancel = gateway.cancel.child_token();
        let healthy = gateway.healthy.clone();
        gateway.workers.push(tokio::spawn(process.run(
            receiver,
            cancel,
            healthy,
            inference_timeout,
        )));
        voices.insert(id, sender);
    }
    let app = Router::new()
        .route("/synthesize", post(synthesize))
        .layer(DefaultBodyLimit::max(32 * 1024))
        .with_state(GatewayState {
            default_voice,
            voices,
            healthy: gateway.healthy.clone(),
        });
    let shutdown = gateway.cancel.clone();
    let healthy = gateway.healthy.clone();
    gateway.server = Some(tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await;
        healthy.store(false, Ordering::Release);
    }));
    Ok(gateway)
}

async fn synthesize(
    State(state): State<GatewayState>,
    Json(request): Json<SynthesisRequest>,
) -> Response {
    if request.text.trim().is_empty()
        || request.text.len() > MAX_TEXT_BYTES
        || request.text.contains('\0')
    {
        return (StatusCode::BAD_REQUEST, "Invalid speech text length").into_response();
    }
    if !state.healthy.load(Ordering::Acquire) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Managed Piper is restarting",
        )
            .into_response();
    }
    let voice = request
        .voice
        .as_deref()
        .filter(|voice| !voice.is_empty())
        .unwrap_or(&state.default_voice);
    let Some(sender) = state.voices.get(voice) else {
        return (
            StatusCode::BAD_REQUEST,
            "Managed Piper voice is unavailable",
        )
            .into_response();
    };
    let (result, receiver) = oneshot::channel();
    if sender
        .try_send(Job {
            text: request.text,
            result,
        })
        .is_err()
    {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Managed Piper speech queue is full",
        )
            .into_response();
    }
    match receiver.await {
        Ok(Ok(wav)) => ([(header::CONTENT_TYPE, "audio/wav")], wav).into_response(),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Managed Piper could not synthesize speech",
        )
            .into_response(),
    }
}

struct VoiceProcess {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    directory: tempfile::TempDir,
    sample_rate: u32,
}
impl VoiceProcess {
    async fn start(mut command: Command, startup_timeout: Duration) -> Result<Self> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        crate::execution::configure_background_process(&mut command);
        let mut child = command.spawn().context("Could not start bundled Piper")?;
        let input = child.stdin.take().context("Piper stdin unavailable")?;
        let mut output =
            BufReader::new(child.stdout.take().context("Piper readiness unavailable")?);
        let sample_rate = timeout(startup_timeout, async {
            let mut received = 0usize;
            while let Some(line) = bounded_line(&mut output).await? {
                received += line.len();
                ensure!(
                    received <= 1024 * 1024,
                    "Piper startup output exceeds the limit"
                );
                if let Some(json) = line.strip_prefix(READY_PREFIX) {
                    let ready: Ready =
                        serde_json::from_slice(json).context("Invalid Piper readiness")?;
                    ensure!(
                        (8000..=192_000).contains(&ready.sample_rate),
                        "Invalid Piper sample rate"
                    );
                    return Ok(ready.sample_rate);
                }
            }
            bail!("Piper exited before loading its voice")
        })
        .await
        .context("Piper voice loading timed out")??;
        ensure!(
            child.try_wait()?.is_none(),
            "Piper exited after loading its voice"
        );
        let directory = tempfile::Builder::new()
            .prefix("babel-piper-")
            .tempdir()
            .context("Could not create private Piper audio directory")?;
        Ok(Self {
            child,
            input,
            output,
            directory,
            sample_rate,
        })
    }

    async fn run(
        mut self,
        mut jobs: mpsc::Receiver<Job>,
        cancel: CancellationToken,
        healthy: Arc<AtomicBool>,
        inference_timeout: Duration,
    ) {
        loop {
            let job = tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                status = self.child.wait() => { let _ = status; healthy.store(false, Ordering::Release); break; }
                job = jobs.recv() => { let Some(job) = job else { break; }; job }
            };
            let result = tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                result = timeout(inference_timeout, self.synthesize(&job.text)) => {
                    result.context("Piper synthesis timed out").and_then(|result| result)
                }
            };
            let failed = result.is_err();
            // A closed receiver only means an HTTP caller stopped waiting. The
            // actor has still consumed the matching reply and cleaned its WAV.
            let _ = job.result.send(result);
            if failed {
                healthy.store(false, Ordering::Release);
                break;
            }
        }
        let _ = self.child.start_kill();
        let _ = timeout(Duration::from_secs(1), self.child.wait()).await;
    }

    async fn synthesize(&mut self, text: &str) -> Result<Vec<u8>> {
        let path = tempfile::NamedTempFile::new_in(self.directory.path())?.into_temp_path();
        let output_file = path.to_str().context("Piper output path must be UTF-8")?;
        let mut request = serde_json::to_vec(&json!({"text":text,"output_file":output_file}))?;
        request.push(b'\n');
        self.input
            .write_all(&request)
            .await
            .context("Piper input failed")?;
        self.input.flush().await.context("Piper input failed")?;
        let complete = bounded_line(&mut self.output)
            .await?
            .context("Piper exited during synthesis")?;
        ensure!(
            complete == output_file.as_bytes(),
            "Piper returned an unexpected synthesis response"
        );
        let metadata = tokio::fs::metadata(&path)
            .await
            .context("Piper output is unavailable")?;
        ensure!(
            metadata.is_file() && metadata.len() <= MAX_WAV_BYTES,
            "Piper WAV exceeds the size limit"
        );
        let file = tokio::fs::File::open(&path)
            .await
            .context("Piper output is unavailable")?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take(MAX_WAV_BYTES + 1)
            .read_to_end(&mut bytes)
            .await
            .context("Piper output is unavailable")?;
        ensure!(
            bytes.len() as u64 <= MAX_WAV_BYTES,
            "Piper WAV exceeds the size limit"
        );
        let sample_rate = self.sample_rate;
        tokio::task::spawn_blocking(move || normalize_wav(&bytes, sample_rate))
            .await
            .context("Piper WAV conversion failed")?
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
        let length = newline.unwrap_or(chunk.len());
        ensure!(
            line.len() + length <= MAX_LINE_BYTES,
            "Piper response line exceeds the limit"
        );
        line.extend_from_slice(&chunk[..length]);
        reader.consume(length + usize::from(newline.is_some()));
        if newline.is_some() {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some(line));
        }
    }
}

fn normalize_wav(bytes: &[u8], sample_rate: u32) -> Result<Vec<u8>> {
    ensure!(
        bytes.len() as u64 <= MAX_WAV_BYTES,
        "Piper WAV exceeds the size limit"
    );
    let bytes = finalize_streaming_header(bytes, sample_rate)?;
    let mut reader = hound::WavReader::new(Cursor::new(bytes.as_ref()))
        .context("Piper returned an invalid WAV")?;
    let spec = reader.spec();
    ensure!(
        spec.channels == 1 && spec.sample_rate == sample_rate,
        "Piper WAV format does not match the loaded voice"
    );
    ensure!(
        reader.duration() > 0 && reader.duration() <= sample_rate * 30,
        "Piper speech exceeds 30 seconds"
    );
    ensure!(
        u64::from(reader.duration()) * 2 + 44 <= MAX_WAV_BYTES,
        "Piper WAV exceeds the size limit"
    );
    let mut output = Cursor::new(Vec::with_capacity(reader.duration() as usize * 2 + 44));
    let mut writer = hound::WavWriter::new(
        &mut output,
        hound::WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )?;
    match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Float, 32) => {
            for sample in reader.samples::<f32>() {
                let sample = sample.context("Invalid Piper float sample")?;
                ensure!(sample.is_finite(), "Invalid Piper float sample");
                writer.write_sample(
                    (sample.clamp(-1.0, 1.0) * 32768.0)
                        .round()
                        .clamp(-32768.0, 32767.0) as i16,
                )?;
            }
        }
        (hound::SampleFormat::Int, 16) => {
            for sample in reader.samples::<i16>() {
                writer.write_sample(sample?)?;
            }
        }
        _ => bail!("Unsupported Piper WAV encoding"),
    }
    writer.finalize()?;
    let output = output.into_inner();
    ensure!(
        output.len() as u64 <= MAX_WAV_BYTES,
        "Piper WAV exceeds the size limit"
    );
    Ok(output)
}

/// The pinned Piper CLI closes each WAV but retains its streaming size markers.
/// Repair only its exact float32 mono header, after the owning actor receives
/// the completion line. Other truncated or malformed WAVs are never repaired.
fn finalize_streaming_header(bytes: &[u8], sample_rate: u32) -> Result<std::borrow::Cow<'_, [u8]>> {
    const DATA_MARKER: u32 = 0x7fff_f000;
    const RIFF_MARKER: u32 = DATA_MARKER + 36;
    let riff_marker = bytes.get(4..8) == Some(RIFF_MARKER.to_le_bytes().as_slice());
    let data_marker = bytes.get(40..44) == Some(DATA_MARKER.to_le_bytes().as_slice());
    if !riff_marker && !data_marker {
        return Ok(std::borrow::Cow::Borrowed(bytes));
    }
    let byte_rate = sample_rate
        .checked_mul(4)
        .context("Invalid Piper sample rate")?;
    ensure!(
        riff_marker
            && data_marker
            && bytes.len() > 44
            && bytes.len() as u64 <= MAX_WAV_BYTES
            && (bytes.len() - 44).is_multiple_of(4)
            && bytes.get(..4) == Some(b"RIFF")
            && bytes.get(8..16) == Some(b"WAVEfmt ")
            && bytes.get(16..24) == Some([16, 0, 0, 0, 3, 0, 1, 0].as_slice())
            && bytes.get(24..28) == Some(sample_rate.to_le_bytes().as_slice())
            && bytes.get(28..32) == Some(byte_rate.to_le_bytes().as_slice())
            && bytes.get(32..40) == Some([4, 0, 32, 0, b'd', b'a', b't', b'a'].as_slice()),
        "Invalid Piper streaming WAV header"
    );
    let mut finalized = bytes.to_vec();
    finalized[4..8].copy_from_slice(&((bytes.len() - 8) as u32).to_le_bytes());
    finalized[40..44].copy_from_slice(&((bytes.len() - 44) as u32).to_le_bytes());
    Ok(std::borrow::Cow::Owned(finalized))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::io::Write;

    const FIXTURE_MODE: &str = "BABEL_TEST_PIPER_MODE";

    // A real persistent subprocess exercises pipe framing, dropped requests,
    // temporary outputs and process ownership without installing an AI model.
    #[test]
    #[ignore = "private Piper subprocess fixture"]
    fn helper_process() {
        use std::io::BufRead;
        let Ok(mode) = std::env::var(FIXTURE_MODE) else {
            return;
        };
        if mode == "bad-ready" {
            println!("BABEL_PIPER_READY {{\"sample_rate\":0}}");
            std::io::stdout().flush().unwrap();
            std::thread::sleep(Duration::from_secs(10));
            return;
        }
        println!("BABEL_PIPER_READY {{\"sample_rate\":16000}}");
        std::io::stdout().flush().unwrap();
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else {
                break;
            };
            let request: Value = serde_json::from_str(&line).unwrap();
            let text = request["text"].as_str().unwrap();
            let path = request["output_file"].as_str().unwrap();
            if text == "slow" {
                std::thread::sleep(Duration::from_millis(300));
            }
            if mode == "hang" {
                std::thread::sleep(Duration::from_secs(30));
            }
            if mode == "wrong-line" {
                println!("unexpected");
                std::io::stdout().flush().unwrap();
                continue;
            }
            let sample: f32 = if text == "second" { 0.5 } else { 0.25 };
            let mut writer = hound::WavWriter::create(
                path,
                hound::WavSpec {
                    channels: 1,
                    sample_rate: 16000,
                    bits_per_sample: 32,
                    sample_format: hound::SampleFormat::Float,
                },
            )
            .unwrap();
            for _ in 0..1600 {
                writer.write_sample(sample).unwrap();
            }
            writer.finalize().unwrap();
            println!("{path}");
            std::io::stdout().flush().unwrap();
        }
    }

    fn command(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--ignored",
                "--exact",
                "local_runtime::piper::tests::helper_process",
                "--nocapture",
            ])
            .env(FIXTURE_MODE, mode);
        command
    }
    async fn gateway(mode: &str, timeout: Duration) -> PiperGateway {
        start_commands(
            vec![("voice-a".into(), command(mode))],
            Duration::from_secs(5),
            timeout,
        )
        .await
        .unwrap()
    }
    fn client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    #[tokio::test]
    async fn persistent_gateway_returns_pcm16_and_rejects_invalid_input() {
        let gateway = gateway("normal", Duration::from_secs(3)).await;
        assert!(gateway.healthy());
        let client = client();
        for (text, amplitude) in [("first", 8192i16), ("second", 16384i16)] {
            let response = client
                .post(&gateway.endpoint)
                .json(&json!({"text":text}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[header::CONTENT_TYPE], "audio/wav");
            let wav = response.bytes().await.unwrap();
            let mut reader = hound::WavReader::new(Cursor::new(wav)).unwrap();
            assert_eq!(reader.spec().sample_format, hound::SampleFormat::Int);
            assert_eq!(reader.spec().bits_per_sample, 16);
            assert_eq!(reader.spec().sample_rate, 16000);
            assert_eq!(reader.duration(), 1600);
            assert!(
                reader
                    .samples::<i16>()
                    .all(|sample| sample.unwrap() == amplitude)
            );
        }
        for request in [
            json!({"text":""}),
            json!({"text":"ok","voice":"missing"}),
            json!({"text":"x".repeat(MAX_TEXT_BYTES+1)}),
        ] {
            assert_eq!(
                client
                    .post(&gateway.endpoint)
                    .json(&request)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::BAD_REQUEST
            );
        }
        assert_eq!(
            client
                .post(&gateway.endpoint)
                .header(header::CONTENT_TYPE, "application/json")
                .body("x".repeat(32 * 1024 + 1))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        let endpoint = gateway.endpoint.clone();
        drop(gateway);
        for _ in 0..30 {
            if client
                .post(&endpoint)
                .json(&json!({"text":"after stop"}))
                .send()
                .await
                .is_err()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("gateway socket stayed open after its owner was dropped");
    }

    #[tokio::test]
    async fn dropped_http_request_does_not_steal_the_next_reply() {
        let gateway = gateway("normal", Duration::from_secs(3)).await;
        let client = client();
        let first = client
            .post(&gateway.endpoint)
            .json(&json!({"text":"slow"}))
            .timeout(Duration::from_millis(100))
            .send()
            .await;
        assert!(first.is_err());
        let response = client
            .post(&gateway.endpoint)
            .json(&json!({"text":"second","voice":"voice-a"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let wav = response.bytes().await.unwrap();
        let mut reader = hound::WavReader::new(Cursor::new(wav)).unwrap();
        assert!(
            reader
                .samples::<i16>()
                .all(|sample| sample.unwrap() == 16384)
        );
        assert!(gateway.healthy());
    }

    #[tokio::test]
    async fn failed_or_timed_out_process_is_unhealthy_and_errors_do_not_contain_speech() {
        for mode in ["wrong-line", "hang"] {
            let gateway = gateway(mode, Duration::from_millis(100)).await;
            let response = client()
                .post(&gateway.endpoint)
                .json(&json!({"text":"private source text"}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert!(
                !response
                    .text()
                    .await
                    .unwrap()
                    .contains("private source text")
            );
            // The actor publishes failure before/after replying within one turn.
            tokio::task::yield_now().await;
            assert!(!gateway.healthy());
        }
        assert!(
            start_commands(
                vec![("voice".into(), command("bad-ready"))],
                Duration::from_secs(1),
                Duration::from_secs(1)
            )
            .await
            .is_err()
        );
    }

    #[test]
    fn wav_conversion_checks_duration_channels_rate_and_non_finite_values() {
        fn wav(samples: &[f32], channels: u16, sample_rate: u32) -> Vec<u8> {
            let mut buffer = Cursor::new(Vec::new());
            let mut writer = hound::WavWriter::new(
                &mut buffer,
                hound::WavSpec {
                    channels,
                    sample_rate,
                    bits_per_sample: 32,
                    sample_format: hound::SampleFormat::Float,
                },
            )
            .unwrap();
            for sample in samples {
                writer.write_sample(*sample).unwrap();
            }
            writer.finalize().unwrap();
            buffer.into_inner()
        }
        assert!(normalize_wav(&wav(&[0.5, -0.5], 1, 16000), 16000).is_ok());
        assert!(normalize_wav(&wav(&[0.5, -0.5], 2, 16000), 16000).is_err());
        assert!(normalize_wav(&wav(&[0.5], 1, 22050), 16000).is_err());
        assert!(normalize_wav(&wav(&[f32::NAN], 1, 16000), 16000).is_err());
        assert!(normalize_wav(&wav(&vec![0.5; 16000 * 30 + 1], 1, 16000), 16000).is_err());
    }

    #[test]
    fn pinned_piper_streaming_header_is_finalized_without_accepting_other_truncated_wavs() {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&0x7fff_f024u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&[16, 0, 0, 0, 3, 0, 1, 0]);
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.extend_from_slice(&64000u32.to_le_bytes());
        wav.extend_from_slice(&[4, 0, 32, 0]);
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&0x7fff_f000u32.to_le_bytes());
        for value in [0.25f32, -0.5, 1.0] {
            wav.extend_from_slice(&value.to_le_bytes());
        }
        let converted = normalize_wav(&wav, 16000).unwrap();
        let mut reader = hound::WavReader::new(Cursor::new(converted)).unwrap();
        assert_eq!(reader.duration(), 3);
        assert_eq!(
            reader
                .samples::<i16>()
                .map(Result::unwrap)
                .collect::<Vec<_>>(),
            [8192, -16384, 32767]
        );
        assert!(normalize_wav(&wav[..wav.len() - 1], 16000).is_err());
        assert!(normalize_wav(&wav, 22050).is_err());
        let mut invalid = wav.clone();
        invalid[22] = 2; // A stereo header must not be treated as this CLI contract.
        assert!(normalize_wav(&invalid, 16000).is_err());
        let mut wrong_marker = wav.clone();
        wrong_marker[40..44].copy_from_slice(&12u32.to_le_bytes());
        assert!(normalize_wav(&wrong_marker, 16000).is_err());
        let mut normal = finalize_streaming_header(&wav, 16000).unwrap().into_owned();
        normal.truncate(normal.len() - 4);
        assert!(
            normalize_wav(&normal, 16000).is_err(),
            "ordinary truncated files must still fail"
        );
    }
}
