use std::{
    process::Stdio,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
    sync::{Mutex, mpsc},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use super::{AudioOptions, AudioStats, Device, DeviceDirection, PcmFrame, PlaybackCommand};

const OWNER: &str = "babel.owner=org.babel.audio.v1";
static MODULE_LOCK: Mutex<()> = Mutex::const_new(());

#[derive(Clone, Copy)]
struct ModuleSpec {
    kind: &'static str,
    key: &'static str,
    endpoint: &'static str,
    arguments: &'static [&'static str],
}

const MODULES: [ModuleSpec; 3] = [
    ModuleSpec {
        kind: "module-null-sink",
        key: "sink_name",
        endpoint: "babel_mic_bus",
        arguments: &[
            "sink_name=babel_mic_bus",
            "rate=48000",
            "channels=1",
            "channel_map=mono",
            "sink_properties='device.description=Babel_Microphone_Bus babel.owner=org.babel.audio.v1'",
        ],
    },
    ModuleSpec {
        kind: "module-remap-source",
        key: "source_name",
        endpoint: "babel_microphone",
        arguments: &[
            "source_name=babel_microphone",
            "master=babel_mic_bus.monitor",
            "channels=1",
            "channel_map=mono",
            "master_channel_map=mono",
            "source_properties='device.description=Babel_Microphone babel.owner=org.babel.audio.v1'",
        ],
    },
    ModuleSpec {
        kind: "module-null-sink",
        key: "sink_name",
        endpoint: "babel_speaker",
        arguments: &[
            "sink_name=babel_speaker",
            "rate=48000",
            "channels=2",
            "channel_map=front-left,front-right",
            "sink_properties='device.description=Babel_Speaker babel.owner=org.babel.audio.v1'",
        ],
    },
];

#[derive(Debug, Deserialize)]
struct Module {
    index: u32,
    name: String,
    #[serde(default)]
    argument: String,
}

// Parse only the grammar we write. Exact tokens prevent matching another user's
// similarly named sink or unloading a module merely because its numeric ID was reused.
fn owned_by(module: &Module, spec: ModuleSpec) -> bool {
    let words: Vec<&str> = module
        .argument
        .split(|c: char| c.is_whitespace() || c == '\'' || c == '"')
        .collect();
    module.name == spec.kind
        && words.contains(&OWNER)
        && words.contains(&format!("{}={}", spec.key, spec.endpoint).as_str())
}

async fn pactl(args: &[&str]) -> Result<String> {
    let mut command = Command::new("pactl");
    command.args(args).kill_on_drop(true).stdin(Stdio::null());
    let output = tokio::time::timeout(Duration::from_secs(5), command.output())
        .await.context("pactl timed out; check your PulseAudio/pipewire-pulse session")?
        .context("could not execute pactl; install pulseaudio-utils (or your distribution's PulseAudio client tools)")?;
    ensure!(
        output.status.success(),
        "pactl failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    String::from_utf8(output.stdout).context("pactl returned non-UTF-8 output")
}

async fn modules() -> Result<Vec<Module>> {
    let json = pactl(&["--format=json", "list", "modules"]).await?;
    let records: Vec<Value> =
        serde_json::from_str(&json).context("invalid pactl JSON module listing")?;
    if records.iter().all(|record| record["index"].is_u64()) {
        return serde_json::from_str(&json).context("invalid pactl module indices");
    }
    // Some pactl 17 builds omit module indices from JSON (including with
    // --short). Short text output still exposes them. Only our single-line
    // arguments can pass ownership validation, so multiline native PipeWire
    // module configuration can never be mistaken for an owned module.
    Ok(parse_short_modules(
        &pactl(&["list", "short", "modules"]).await?,
    ))
}

fn parse_short_modules(text: &str) -> Vec<Module> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.splitn(4, '\t');
            let index = fields.next()?.parse().ok()?;
            let name = fields.next()?.to_owned();
            let argument = fields.next().unwrap_or_default().to_owned();
            Some(Module {
                index,
                name,
                argument,
            })
        })
        .collect()
}

fn parse_devices(text: &str, direction: DeviceDirection) -> Result<Vec<Device>> {
    let list: Vec<Value> =
        serde_json::from_str(text).context("invalid pactl JSON device listing")?;
    list.into_iter()
        .map(|item| {
            let id = item["name"]
                .as_str()
                .context("audio device has no name")?
                .to_owned();
            let name = item["description"].as_str().unwrap_or(&id).to_owned();
            let is_virtual = item["properties"]["device.class"]
                .as_str()
                .is_some_and(|class| class == "monitor" || class == "filter")
                || item["properties"]["babel.owner"].as_str() == Some("org.babel.audio.v1")
                || id.ends_with(".monitor")
                || item["driver"]
                    .as_str()
                    .is_some_and(|driver| driver.contains("null-sink"));
            Ok(Device {
                id,
                name,
                direction,
                is_virtual,
            })
        })
        .collect()
}

pub async fn devices() -> Result<Vec<Device>> {
    let (inputs, outputs) = tokio::try_join!(
        pactl(&["--format=json", "list", "sources"]),
        pactl(&["--format=json", "list", "sinks"]),
    )?;
    let mut found = parse_devices(&inputs, DeviceDirection::Input)?;
    found.extend(parse_devices(&outputs, DeviceDirection::Output)?);
    Ok(found)
}

async fn unload_owned(index: u32, spec: ModuleSpec) -> Result<()> {
    if modules()
        .await?
        .iter()
        .any(|module| module.index == index && owned_by(module, spec))
    {
        pactl(&["unload-module", &index.to_string()]).await?;
    }
    Ok(())
}

pub async fn install_virtual_devices() -> Result<String> {
    let _guard = MODULE_LOCK.lock().await;
    let found = devices().await?;
    let existing = modules().await?;
    // Validate all collisions before creating anything. Existing unrelated devices
    // are never renamed, replaced, connected, or made default.
    for spec in MODULES {
        if found.iter().any(|device| device.id == spec.endpoint) {
            ensure!(
                existing.iter().any(|module| owned_by(module, spec)),
                "device {} already exists and is not owned by Babel; choose another audio session or remove the conflict manually",
                spec.endpoint
            );
        }
    }
    let mut created = Vec::new();
    for spec in MODULES {
        if existing.iter().any(|module| owned_by(module, spec)) {
            continue;
        }
        let mut args = vec!["load-module", spec.kind];
        args.extend_from_slice(spec.arguments);
        let loaded = pactl(&args).await.and_then(|text| {
            text.trim()
                .parse::<u32>()
                .context("pactl did not return a module ID")
        });
        match loaded {
            Ok(index) => created.push((index, spec)),
            Err(error) => {
                for (index, created_spec) in created.into_iter().rev() {
                    if let Err(cleanup) = unload_owned(index, created_spec).await {
                        tracing::warn!(%cleanup, "failed to roll back a Babel audio module");
                    }
                }
                return Err(error.context(format!("creating virtual device {}", spec.endpoint)));
            }
        }
    }
    let actual = devices().await?;
    for spec in MODULES {
        ensure!(
            actual.iter().any(|device| device.id == spec.endpoint),
            "audio server loaded a module but did not expose {}",
            spec.endpoint
        );
    }
    Ok("Babel_Microphone and Babel_Speaker are available. Select them in your calling app; system defaults were not changed.".to_owned())
}

pub async fn uninstall_virtual_devices() -> Result<String> {
    let _guard = MODULE_LOCK.lock().await;
    let existing = modules().await?;
    // Source must be removed before its master sink.
    for spec in [MODULES[1], MODULES[2], MODULES[0]] {
        for module in existing.iter().filter(|module| owned_by(module, spec)) {
            unload_owned(module.index, spec).await?;
        }
    }
    Ok("Removed Babel-owned virtual audio modules. Other devices and system defaults were not changed.".to_owned())
}

async fn require_device(id: &str, direction: DeviceDirection) -> Result<()> {
    ensure!(
        !id.trim().is_empty() && !id.starts_with('@'),
        "select an explicit audio device ID; defaults can create feedback loops"
    );
    ensure!(
        devices()
            .await?
            .iter()
            .any(|device| device.id == id && device.direction == direction),
        "audio device {id:?} is unavailable in the requested direction"
    );
    Ok(())
}

async fn collect_stderr(mut stderr: impl AsyncRead + Unpin) -> String {
    let mut buffer = [0_u8; 1024];
    let mut captured = Vec::new();
    while let Ok(n) = stderr.read(&mut buffer).await {
        if n == 0 {
            break;
        }
        let remaining = 8192_usize.saturating_sub(captured.len());
        captured.extend_from_slice(&buffer[..n.min(remaining)]);
    }
    String::from_utf8_lossy(&captured).trim().to_owned()
}

struct AudioProcess {
    child: Child,
    stderr: JoinHandle<String>,
}

fn audio_command(program: &str, device: &str, options: AudioOptions, record: bool) -> Command {
    let mut command = Command::new(program);
    command
        .args([
            "--raw",
            "--format=s16le",
            "--channels=1",
            "--client-name=Babel",
            "--stream-name=Babel_translation",
            "--property=application.id=org.babel.audio",
            "--property=babel.owner=org.babel.audio.v1",
            // Explicit --device selects the initial endpoint but does not stop
            // the desktop session manager moving a stream when defaults change.
            // These PipeWire properties pin both directions and reject fallback
            // when the selected device vanishes. PulseAudio accepts the custom
            // properties too, but requires our route monitor for move detection.
            // pacat/parec do not expose the libpulse PA_STREAM_DONT_MOVE flag.
            "--property=node.dont-move=true",
            "--property=node.dont-reconnect=true",
            "--property=node.dont-fallback=true",
        ])
        .arg(format!("--device={device}"))
        .arg(format!("--property=babel.target={device}"))
        .arg(if record {
            "--property=babel.direction=capture"
        } else {
            "--property=babel.direction=playback"
        })
        .arg(format!("--rate={}", options.sample_rate))
        .arg(format!("--latency-msec={}", options.latency_ms))
        .arg(format!("--process-time-msec={}", options.frame_ms))
        .stdin(if record {
            Stdio::null()
        } else {
            Stdio::piped()
        })
        .stdout(if record {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

impl AudioProcess {
    fn spawn(program: &str, device: &str, options: AudioOptions, record: bool) -> Result<Self> {
        let mut child = audio_command(program, device, options, record)
            .spawn()
            .with_context(|| {
                format!("could not execute {program}; install the PulseAudio client tools")
            })?;
        let stderr = child.stderr.take().context("missing audio child stderr")?;
        Ok(Self {
            child,
            stderr: tokio::spawn(collect_stderr(stderr)),
        })
    }

    async fn stop(mut self) -> String {
        // kill() waits/reaps the process; kill_on_drop covers task abortion too.
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
        self.stderr.await.unwrap_or_default()
    }
}

pub async fn capture(
    device: &str,
    options: AudioOptions,
    sink: mpsc::Sender<PcmFrame>,
    cancel: CancellationToken,
    stats: Arc<AudioStats>,
) -> Result<()> {
    let options = options.validate()?;
    require_device(device, DeviceDirection::Input).await?;
    let mut process = AudioProcess::spawn("parec", device, options, true)?;
    let mut stdout = process
        .child
        .stdout
        .take()
        .context("missing parec stdout")?;
    let mut bytes = vec![0_u8; options.frame_samples() * 2];
    let result = loop {
        let received = tokio::select! {
            biased;
            _ = cancel.cancelled() => break Ok(()),
            status = process.child.wait() => break Err(anyhow::anyhow!("parec exited unexpectedly: {:?}", status?)),
            received = stdout.read_exact(&mut bytes) => received,
        };
        if let Err(error) = received {
            break Err(error.into());
        }
        let samples = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|s| i16::from_le_bytes([s[0], s[1]]))
            .collect();
        stats.captured_frames.fetch_add(1, Ordering::Relaxed);
        let frame = PcmFrame {
            samples,
            sample_rate: options.sample_rate,
            captured_at: Instant::now(),
        };
        match sink.try_send(frame) {
            Ok(()) => (),
            Err(mpsc::error::TrySendError::Full(_)) => {
                stats.dropped_frames.fetch_add(1, Ordering::Relaxed);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => break Ok(()),
        }
    };
    drop(stdout);
    let stderr = process.stop().await;
    result.with_context(|| format!("capture {device}: {stderr}"))
}

fn spawn_playback(device: &str, options: AudioOptions) -> Result<(AudioProcess, ChildStdin)> {
    let mut process = AudioProcess::spawn("pacat", device, options, false)?;
    let stdin = process.child.stdin.take().context("missing pacat stdin")?;
    Ok((process, stdin))
}

enum WriteOutcome {
    Written,
    Interrupted,
    Cancelled,
}

async fn write_audio(
    process: &mut AudioProcess,
    stdin: &mut ChildStdin,
    bytes: &[u8],
    cancel: &CancellationToken,
    stats: &AudioStats,
    generation: u64,
    stall_timeout: Duration,
) -> Result<WriteOutcome> {
    let writing = stdin.write_all(bytes);
    tokio::pin!(writing);
    let mut check = tokio::time::interval(Duration::from_millis(5));
    let stalled = tokio::time::sleep(stall_timeout);
    tokio::pin!(stalled);
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(WriteOutcome::Cancelled),
            _ = &mut stalled => bail!("audio output stalled for {} ms", stall_timeout.as_millis()),
            _ = check.tick() => {
                if stats.playback_generation.load(Ordering::Acquire) != generation {
                    return Ok(WriteOutcome::Interrupted);
                }
            }
            status = process.child.wait() => bail!("pacat exited unexpectedly: {}", status?),
            written = &mut writing => { written?; return Ok(WriteOutcome::Written); }
        }
    }
}

pub async fn playback(
    device: &str,
    options: AudioOptions,
    mut source: mpsc::Receiver<PlaybackCommand>,
    cancel: CancellationToken,
    stats: Arc<AudioStats>,
) -> Result<()> {
    let options = options.validate()?;
    require_device(device, DeviceDirection::Output).await?;
    let (mut process, mut stdin) = spawn_playback(device, options)?;
    let mut generation = stats.playback_generation.load(Ordering::Acquire);
    let mut bytes = Vec::with_capacity(options.frame_samples() * 2);
    let mut check = tokio::time::interval(Duration::from_millis(5));
    let result = 'playback: loop {
        let current = stats.playback_generation.load(Ordering::Acquire);
        if current != generation {
            drop(stdin);
            process.stop().await;
            (process, stdin) = spawn_playback(device, options)?;
            generation = current;
        }
        let next = tokio::select! {
            biased;
            _ = cancel.cancelled() => break Ok(()),
            _ = check.tick() => continue,
            status = process.child.wait() => break Err(anyhow::anyhow!("pacat exited unexpectedly: {:?}", status?)),
            command = source.recv() => command,
        };
        match next {
            None => break Ok(()),
            Some(PlaybackCommand::Flush) => {
                drop(stdin);
                process.stop().await;
                // Reopening removes server-side buffered audio as well as the pipe.
                (process, stdin) = spawn_playback(device, options)?;
            }
            Some(PlaybackCommand::Audio {
                samples,
                generation: queued_generation,
            }) => {
                if queued_generation != generation {
                    continue;
                }
                for chunk in samples.chunks(options.frame_samples()) {
                    bytes.clear();
                    bytes.extend(chunk.iter().flat_map(|sample| sample.to_le_bytes()));
                    match write_audio(
                        &mut process,
                        &mut stdin,
                        &bytes,
                        &cancel,
                        &stats,
                        generation,
                        Duration::from_millis(
                            u64::from(options.queue_ms.max(options.latency_ms)) + 500,
                        ),
                    )
                    .await
                    {
                        Ok(WriteOutcome::Written) => (),
                        Ok(WriteOutcome::Interrupted) => continue 'playback,
                        Ok(WriteOutcome::Cancelled) => break 'playback Ok(()),
                        Err(error) => break 'playback Err(error),
                    }
                }
            }
        }
    };
    drop(stdin);
    let stderr = process.stop().await;
    result.with_context(|| format!("playback {device}: {stderr}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_streams_keep_explicit_targets_and_stable_ownership_in_both_directions() {
        let options = AudioOptions {
            sample_rate: 48_000,
            frame_ms: 20,
            latency_ms: 40,
            queue_ms: 200,
        };
        for (program, record, direction, device) in [
            ("parec", true, "capture", "babel_speaker.monitor"),
            ("pacat", false, "playback", "USB Speaker (1)"),
        ] {
            let command = audio_command(program, device, options, record);
            let args: Vec<_> = command
                .as_std()
                .get_args()
                .map(|value| value.to_str().unwrap())
                .collect();
            assert!(args.contains(&format!("--device={device}").as_str()));
            assert!(args.contains(&format!("--property=babel.target={device}").as_str()));
            assert!(args.contains(&format!("--property=babel.direction={direction}").as_str()));
            for property in [
                "application.id=org.babel.audio",
                "babel.owner=org.babel.audio.v1",
                "node.dont-move=true",
                "node.dont-reconnect=true",
                "node.dont-fallback=true",
            ] {
                assert!(args.contains(&format!("--property={property}").as_str()));
            }
            // The libpulse flag has no corresponding pacat option. Accidentally
            // adding it would stop all audio on standard PulseAudio utilities.
            assert!(!args.contains(&"--dont-move"));
        }
    }

    #[test]
    fn ownership_requires_exact_name_endpoint_and_owner() {
        let owned = Module {
            index: 12,
            name: MODULES[0].kind.into(),
            argument: MODULES[0].arguments.join(" "),
        };
        assert!(owned_by(&owned, MODULES[0]));
        assert!(!owned_by(
            &Module {
                argument: "sink_name=babel_mic_bus".into(),
                ..owned
            },
            MODULES[0]
        ));
        let foreign = Module {
            index: 12,
            name: MODULES[0].kind.into(),
            argument: format!("sink_name=babel_mic_bus_other sink_properties='{OWNER}'"),
        };
        assert!(!owned_by(&foreign, MODULES[0]));
        let foreign = Module {
            argument: format!("sink_name=babel_mic_bus sink_properties='x{OWNER}'"),
            ..foreign
        };
        assert!(!owned_by(&foreign, MODULES[0]));
    }

    #[test]
    fn pulse_devices_preserve_ids_and_mark_monitors_virtual() {
        let devices = parse_devices(r#"[{"name":"physical","description":"USB mic","properties":{}},{"name":"babel_speaker.monitor","description":"Babel monitor","properties":{"device.class":"monitor"}}]"#, DeviceDirection::Input).unwrap();
        assert_eq!(devices[0].id, "physical");
        assert!(!devices[0].is_virtual);
        assert!(devices[1].is_virtual);
    }

    #[test]
    fn short_module_fallback_recovers_ids_and_ignores_multiline_native_configuration() {
        let text = format!(
            "1\tlibpipewire-module-rt\t{{\n    rt.prio = 88\n}}\t\n536870919\tmodule-null-sink\t{}\t\n",
            MODULES[0].arguments.join(" ")
        );
        let modules = parse_short_modules(&text);
        assert_eq!(modules.len(), 2);
        assert!(!owned_by(&modules[0], MODULES[0]));
        assert_eq!(modules[1].index, 536870919);
        assert!(owned_by(&modules[1], MODULES[0]));
    }

    async fn live_route(input_device: &'static str, output_device: &'static str) -> Result<()> {
        let stop = CancellationToken::new();
        let _cancel_on_drop = stop.clone().drop_guard();
        let stats = Arc::new(AudioStats::default());
        let (capture_tx, mut capture_rx) = mpsc::channel(300);
        let (playback_tx, playback_rx) = mpsc::channel(100);
        let capture_stop = stop.clone();
        let playback_stop = stop.clone();
        let capture_stats = stats.clone();
        let playback_stats = stats.clone();
        let options = AudioOptions {
            sample_rate: 16_000,
            frame_ms: 20,
            latency_ms: 40,
            queue_ms: 2_000,
        };
        let capture_task = tokio::spawn(async move {
            capture(
                input_device,
                options,
                capture_tx,
                capture_stop,
                capture_stats,
            )
            .await
        });
        let playback_task = tokio::spawn(async move {
            playback(
                output_device,
                AudioOptions {
                    sample_rate: 24_000,
                    ..options
                },
                playback_rx,
                playback_stop,
                playback_stats,
            )
            .await
        });
        let tone: Vec<i16> = (0..480)
            .map(|n| {
                ((2.0 * std::f64::consts::PI * 1_000.0 * n as f64 / 24_000.0).sin() * 10_000.0)
                    as i16
            })
            .collect();
        for _ in 0..50 {
            playback_tx
                .send(PlaybackCommand::Audio {
                    samples: tone.clone(),
                    generation: 0,
                })
                .await?;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Fill with two seconds of obsolete speech. An epoch change must bypass
        // those commands even if an explicit Flush cannot fit in the channel.
        for _ in 0..100 {
            playback_tx
                .send(PlaybackCommand::Audio {
                    samples: tone.clone(),
                    generation: 0,
                })
                .await?;
        }
        let interrupted_at = Instant::now();
        stats.playback_generation.fetch_add(1, Ordering::Release);
        for _ in 0..40 {
            playback_tx
                .send(PlaybackCommand::Audio {
                    samples: vec![0; 480],
                    generation: 1,
                })
                .await?;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        stop.cancel();
        capture_task.await??;
        playback_task.await??;
        let mut maximum_rms = 0.0_f64;
        let mut after_interrupt = Vec::new();
        while let Ok(frame) = capture_rx.try_recv() {
            let rms = (frame
                .samples
                .iter()
                .map(|sample| f64::from(*sample).powi(2))
                .sum::<f64>()
                / frame.samples.len() as f64)
                .sqrt();
            maximum_rms = maximum_rms.max(rms);
            if frame.captured_at > interrupted_at + Duration::from_millis(350) {
                after_interrupt.push(rms);
            }
        }
        ensure!(
            maximum_rms > 1_000.0,
            "no tone received through {output_device} → {input_device}"
        );
        ensure!(
            !after_interrupt.is_empty(),
            "no frames captured after interruption"
        );
        ensure!(
            after_interrupt.iter().all(|rms| *rms < 100.0),
            "obsolete audio remained after generation change"
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires a live PulseAudio/pipewire-pulse session and client tools; creates then removes Babel devices"]
    async fn live_virtual_routes_idempotence_interruption_and_cleanup() -> Result<()> {
        ensure!(
            !devices()
                .await?
                .iter()
                .any(|device| device.id.starts_with("babel_")),
            "Babel devices already exist; stop Babel and remove them before this isolated smoke test"
        );
        let before: Value = serde_json::from_str(&pactl(&["--format=json", "info"]).await?)?;
        let result = async {
            install_virtual_devices().await?;
            install_virtual_devices().await?;
            ensure!(
                devices()
                    .await?
                    .iter()
                    .filter(|device| device.id == "babel_microphone")
                    .count()
                    == 1,
                "duplicate microphone after repeated installation"
            );
            live_route("babel_microphone", "babel_mic_bus").await?;
            live_route("babel_speaker.monitor", "babel_speaker").await?;
            let after: Value = serde_json::from_str(&pactl(&["--format=json", "info"]).await?)?;
            ensure!(
                before["default_sink_name"] == after["default_sink_name"]
                    && before["default_source_name"] == after["default_source_name"],
                "system defaults changed"
            );
            Ok::<_, anyhow::Error>(())
        }
        .await;
        uninstall_virtual_devices().await?;
        ensure!(
            !devices()
                .await?
                .iter()
                .any(|device| device.id.starts_with("babel_")),
            "Babel devices remained after cleanup"
        );
        result
    }
}
