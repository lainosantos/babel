//! Pulse-compatible volume control runs on the controller's control worker.
//! Only a verified PipeWire monitor can separate the virtual slider from PCM.

use std::{
    collections::VecDeque,
    process::Stdio,
    sync::{Mutex, OnceLock},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use serde_json::Value;
use tokio::{io::AsyncBufReadExt, process::Command, sync::watch};
use tokio_util::sync::CancellationToken;

use super::{Snapshot, VolumeState};
use crate::audio::linux::{owned_speaker_module, pactl};

const NORMAL_VOLUME: u32 = 65_536;
const OWNER: &str = "org.babel.audio.v1";

#[derive(Default)]
struct BalanceMemory(VecDeque<(String, Vec<u32>)>);

impl BalanceMemory {
    fn channels(&mut self, key: String, current: &[u32]) -> Vec<u32> {
        let old = self
            .0
            .iter()
            .position(|(saved, _)| saved == &key)
            .and_then(|index| self.0.remove(index));
        let channels = if current.iter().any(|volume| *volume != 0) {
            current.to_vec()
        } else if let Some((_, channels)) = old {
            channels
        } else {
            return current.to_vec();
        };
        if self.0.len() == 32 {
            self.0.pop_front();
        }
        self.0.push_back((key, channels.clone()));
        channels
    }
}

fn remembered_channels(server: &Value, sink: &Value, current: &[u32]) -> Vec<u32> {
    static BALANCE: OnceLock<Mutex<BalanceMemory>> = OnceLock::new();
    // A server restart or replacement node must never inherit another device's
    // balance. Without a server identity, preserve only currently visible ratios.
    if server["cookie"].is_null() || numeric(&sink["index"]).is_none() {
        return current.to_vec();
    }
    let key = serde_json::json!([
        server["cookie"],
        sink["name"],
        sink["index"],
        sink["owner_module"],
        sink["properties"]["object.serial"],
        sink["channel_map"]
    ])
    .to_string();
    BALANCE
        .get_or_init(Mutex::default)
        .lock()
        .map(|mut balance| balance.channels(key, current))
        .unwrap_or_else(|_| current.to_vec())
}

struct Endpoints {
    server: Value,
    sinks: Vec<Value>,
    sources: Vec<Value>,
    owned_module: Option<u32>,
}

async fn endpoints() -> Result<Endpoints> {
    let (server, sinks, sources, owned_module) = tokio::try_join!(
        pactl(&["--format=json", "info"]),
        pactl(&["--format=json", "list", "sinks"]),
        pactl(&["--format=json", "list", "sources"]),
        owned_speaker_module(),
    )?;
    Ok(Endpoints {
        server: serde_json::from_str(&server).context("invalid audio server information")?,
        sinks: serde_json::from_str(&sinks).context("invalid audio output information")?,
        sources: serde_json::from_str(&sources).context("invalid audio input information")?,
        owned_module,
    })
}

fn endpoint<'a>(rows: &'a [Value], name: &str) -> Result<&'a Value> {
    rows.iter()
        .find(|row| row["name"].as_str() == Some(name))
        .context("the selected audio endpoint is unavailable")
}

fn numeric(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn channel_volumes(sink: &Value) -> Result<Vec<u32>> {
    let names: Vec<_> = sink["channel_map"]
        .as_str()
        .context("audio output has no channel map")?
        .split(',')
        .map(str::trim)
        .collect();
    let volumes = sink["volume"]
        .as_object()
        .context("audio output has no volume control")?;
    ensure!(
        (1..=32).contains(&names.len())
            && volumes.len() == names.len()
            && names
                .iter()
                .enumerate()
                .all(|(index, name)| !names[..index].contains(name)),
        "audio output volume channels do not match its channel map"
    );
    names
        .iter()
        .map(|name| {
            numeric(&sink["volume"][*name]["value"])
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| *value <= i32::MAX as u32)
                .context("audio output returned an invalid channel volume")
        })
        .collect()
}

fn state(sink: &Value) -> Result<VolumeState> {
    Ok(VolumeState {
        level: *channel_volumes(sink)?.iter().max().unwrap() as f32 / NORMAL_VOLUME as f32,
        muted: sink["mute"]
            .as_bool()
            .context("audio output has no mute control")?,
    })
}

fn is_false(value: &Value) -> bool {
    value.as_bool() == Some(false) || value.as_str() == Some("false")
}

fn limitation(reading: &Endpoints, capture: &str, sink: Option<&Value>) -> Option<String> {
    if !reading.server["server_name"]
        .as_str()
        .is_some_and(|name| name.contains("PipeWire"))
    {
        return Some("This PulseAudio server applies the virtual speaker volume to captured audio. Use the physical speaker volume control; automatic synchronization requires PipeWire.".into());
    }
    let Some(sink) = sink else {
        return Some("The selected capture endpoint is not a speaker monitor. Use the physical speaker volume control.".into());
    };
    let source = reading
        .sources
        .iter()
        .find(|source| source["name"].as_str() == Some(capture));
    let owned = capture == "babel_speaker.monitor"
        && sink["name"].as_str() == Some("babel_speaker")
        && sink["properties"]["babel.owner"].as_str() == Some(OWNER)
        && reading.owned_module.is_some_and(|module| {
            numeric(&sink["owner_module"]) == Some(u64::from(module))
                && source.is_some_and(|source| {
                    numeric(&source["owner_module"]) == Some(u64::from(module))
                        && source["properties"]["device.class"].as_str() == Some("monitor")
                })
        });
    if !owned {
        return Some("Automatic volume synchronization requires the Babel-owned speaker monitor. Use the physical speaker volume control for this endpoint.".into());
    }
    if !is_false(&sink["properties"]["monitor.channel-volumes"]) {
        return Some("Babel Speaker needs a volume-control upgrade. Finish sessions, close apps using Babel devices, and reinstall the virtual devices; use the physical speaker volume control until then.".into());
    }
    None
}

fn snapshot(reading: &Endpoints, capture: &str, playback: &str) -> Result<Snapshot> {
    let physical = endpoint(&reading.sinks, playback)?;
    let physical_state = state(physical)?;
    let virtual_sink = capture
        .strip_suffix(".monitor")
        .and_then(|name| reading.sinks.iter().find(|sink| sink["name"] == name));
    let virtual_state = virtual_sink
        .map(state)
        .transpose()?
        .unwrap_or(physical_state);
    remembered_channels(&reading.server, physical, &channel_volumes(physical)?);
    if let Some(sink) = virtual_sink {
        remembered_channels(&reading.server, sink, &channel_volumes(sink)?);
    }
    let mut limitation = limitation(reading, capture, virtual_sink);
    if limitation.is_none()
        && (physical["properties"]["babel.owner"].as_str() == Some(OWNER)
            || virtual_sink.is_some_and(|sink| sink["name"] == physical["name"]))
    {
        limitation = Some("Choose a physical speaker output before synchronizing volume.".into());
    }
    if limitation.is_none()
        && [Some(physical), virtual_sink]
            .into_iter()
            .flatten()
            .any(|sink| {
                channel_volumes(sink)
                    .is_ok_and(|channels| channels.iter().any(|volume| *volume > NORMAL_VOLUME))
            })
    {
        limitation = Some("Automatic volume synchronization supports levels up to 100%. Lower amplified endpoint volumes before enabling it.".into());
    }
    Ok(Snapshot {
        virtual_state,
        physical_state,
        synchronized: limitation.is_none(),
        limitation,
    })
}

pub(super) async fn inspect(capture: &str, playback: &str) -> Result<Snapshot> {
    snapshot(&endpoints().await?, capture, playback)
}

fn scaled_channels(channels: &[u32], level: f32) -> Result<Vec<String>> {
    ensure!(
        level.is_finite() && (0.0..=1.0).contains(&level),
        "speaker volume must be between 0% and 100%"
    );
    ensure!(!channels.is_empty(), "audio output has no volume channels");
    let target = (f64::from(level) * f64::from(NORMAL_VOLUME)).round() as u32;
    let previous = *channels.iter().max().unwrap();
    // This is the same proportional scaling of Pulse volume units used by
    // pa_cvolume_scale. Never treat the displayed percentage as PCM gain.
    Ok(channels
        .iter()
        .map(|value| {
            if previous == 0 {
                target
            } else {
                ((u64::from(*value) * u64::from(target) + u64::from(previous) / 2)
                    / u64::from(previous)) as u32
            }
            .to_string()
        })
        .collect())
}

async fn write(args: &[&str], cancel: &CancellationToken) -> Result<()> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => anyhow::bail!("Output volume selection changed before the update completed"),
        result = pactl(args) => result.map(|_| ()),
    }
}

async fn set(
    server: &Value,
    sink: &Value,
    desired: VolumeState,
    cancel: CancellationToken,
) -> Result<()> {
    let channels = channel_volumes(sink)?;
    let values = scaled_channels(&remembered_channels(server, sink, &channels), desired.level)?;
    // Use the inspected instance, not a name that could now resolve to a
    // replacement endpoint. The controller rechecks its binding generation.
    let index = numeric(&sink["index"])
        .context("audio output has no stable instance index")?
        .to_string();
    let muted = sink["mute"]
        .as_bool()
        .context("audio output has no mute control")?;
    if desired.muted && !muted {
        write(&["set-sink-mute", &index, "1"], &cancel).await?;
    }
    if channels.iter().map(u32::to_string).collect::<Vec<_>>() != values {
        let mut args = vec!["set-sink-volume", index.as_str()];
        args.extend(values.iter().map(String::as_str));
        write(&args, &cancel).await?;
    }
    if !desired.muted && muted {
        // Set level before unmuting so restoring audio never exposes stale gain.
        write(&["set-sink-mute", &index, "0"], &cancel).await?;
    }
    Ok(())
}

pub(super) async fn set_virtual(
    capture: &str,
    desired: VolumeState,
    cancel: CancellationToken,
) -> Result<()> {
    let reading = endpoints().await?;
    let sink = capture
        .strip_suffix(".monitor")
        .and_then(|name| reading.sinks.iter().find(|sink| sink["name"] == name));
    if let Some(reason) = limitation(&reading, capture, sink) {
        anyhow::bail!("{reason}");
    }
    set(
        &reading.server,
        sink.context("Babel speaker is unavailable")?,
        desired,
        cancel,
    )
    .await
}

pub(super) async fn set_physical(
    playback: &str,
    desired: VolumeState,
    cancel: CancellationToken,
) -> Result<()> {
    let (server, sinks) = tokio::try_join!(
        pactl(&["--format=json", "info"]),
        pactl(&["--format=json", "list", "sinks"]),
    )?;
    let server: Value = serde_json::from_str(&server)?;
    let sinks: Vec<Value> = serde_json::from_str(&sinks)?;
    set(&server, endpoint(&sinks, playback)?, desired, cancel).await
}

fn relevant_event(line: &str) -> bool {
    line.contains(" on sink #") || line.contains(" on server #")
}

pub(super) fn changes(cancel: CancellationToken) -> watch::Receiver<()> {
    let (sender, receiver) = watch::channel(());
    tokio::spawn(async move {
        while !cancel.is_cancelled() && !sender.is_closed() {
            let child = Command::new("pactl")
                .arg("subscribe")
                .env("LC_ALL", "C")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn();
            if let Ok(mut child) = child {
                let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
                loop {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => break,
                        _ = sender.closed() => break,
                        line = lines.next_line() => match line {
                            Ok(Some(line)) if relevant_event(&line) => {
                                sender.send_replace(());
                            }
                            Ok(Some(_)) => {}
                            _ => break,
                        }
                    }
                }
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = sender.closed() => break,
                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            }
        }
    });
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Endpoints {
        let sink = serde_json::json!({
            "index":7,"name":"babel_speaker","owner_module":42,"mute":false,
            "channel_map":"front-right,front-left",
            "volume":{"front-left":{"value":32768},"front-right":{"value":65536}},
            "properties":{"babel.owner":OWNER,"monitor.channel-volumes":"false"}
        });
        let mut physical = sink.clone();
        physical["name"] = "physical".into();
        physical["index"] = 8.into();
        physical["properties"] = serde_json::json!({});
        Endpoints {
            server: serde_json::json!({"server_name":"PulseAudio (on PipeWire 1.6.2)"}),
            sinks: vec![sink, physical],
            sources: vec![serde_json::json!({
                "name":"babel_speaker.monitor","owner_module":42,
                "properties":{"device.class":"monitor"}
            })],
            owned_module: Some(42),
        }
    }

    #[test]
    fn synchronization_requires_actual_pipewire_owned_control_only_monitor() {
        let mut reading = fixture();
        assert!(
            snapshot(&reading, "babel_speaker.monitor", "physical")
                .unwrap()
                .synchronized
        );
        for value in [
            serde_json::json!("true"),
            Value::Null,
            serde_json::json!(true),
        ] {
            reading.sinks[0]["properties"]["monitor.channel-volumes"] = value;
            let state = snapshot(&reading, "babel_speaker.monitor", "physical").unwrap();
            assert!(!state.synchronized);
            assert!(state.limitation.unwrap().contains("upgrade"));
        }
        reading.sinks[0]["properties"]["monitor.channel-volumes"] = false.into();
        assert!(
            snapshot(&reading, "babel_speaker.monitor", "physical")
                .unwrap()
                .synchronized
        );
        reading.server["server_name"] = "pulseaudio".into();
        assert!(
            !snapshot(&reading, "babel_speaker.monitor", "physical")
                .unwrap()
                .synchronized
        );
        reading = fixture();
        reading.owned_module = Some(99);
        assert!(
            !snapshot(&reading, "babel_speaker.monitor", "physical")
                .unwrap()
                .synchronized
        );
        reading = fixture();
        reading.sources[0]["owner_module"] = 99.into();
        assert!(
            !snapshot(&reading, "babel_speaker.monitor", "physical")
                .unwrap()
                .synchronized
        );
    }

    #[test]
    fn endpoint_selection_and_amplified_levels_fail_closed() {
        let mut reading = fixture();
        let missing_virtual = snapshot(&reading, "missing", "physical").unwrap();
        assert!(!missing_virtual.synchronized);
        assert_eq!(missing_virtual.physical_state.level, 1.0);
        assert!(
            missing_virtual
                .limitation
                .unwrap()
                .contains("physical speaker volume")
        );
        assert!(snapshot(&reading, "babel_speaker.monitor", "missing").is_err());
        assert!(
            !snapshot(&reading, "babel_speaker.monitor", "babel_speaker")
                .unwrap()
                .synchronized
        );
        reading.sinks[1]["volume"]["front-left"]["value"] = 80_000.into();
        let state = snapshot(&reading, "babel_speaker.monitor", "physical").unwrap();
        assert!(!state.synchronized);
        assert_eq!(state.physical_state.level, 80_000.0 / NORMAL_VOLUME as f32);
        assert!(state.limitation.unwrap().contains("100%"));
    }

    #[test]
    fn volume_scaling_preserves_channel_order_balance_and_zero_channels() {
        let reading = fixture();
        let channels = channel_volumes(&reading.sinks[0]).unwrap();
        assert_eq!(channels, [65_536, 32_768]);
        assert_eq!(scaled_channels(&channels, 0.5).unwrap(), ["32768", "16384"]);
        assert_eq!(scaled_channels(&[65_536, 0], 0.5).unwrap(), ["32768", "0"]);
        assert_eq!(scaled_channels(&[0, 0], 0.5).unwrap(), ["32768", "32768"]);
        assert_eq!(scaled_channels(&[NORMAL_VOLUME], 0.0).unwrap(), ["0"]);
        assert!(scaled_channels(&channels, f32::NAN).is_err());
        assert!(scaled_channels(&channels, 1.1).is_err());
    }

    #[test]
    fn malformed_channel_maps_never_write_reordered_or_partial_levels() {
        let mut sink = fixture().sinks.remove(0);
        sink["channel_map"] = "front-center,front-left".into();
        assert!(channel_volumes(&sink).is_err());
        sink["channel_map"] = "front-left".into();
        assert!(channel_volumes(&sink).is_err());
        sink["channel_map"] = "front-left,front-left".into();
        assert!(channel_volumes(&sink).is_err());
    }

    #[test]
    fn zero_then_raise_preserves_balance_without_crossing_endpoint_instances() {
        let mut memory = BalanceMemory::default();
        assert_eq!(
            memory.channels("first".into(), &[65_536, 32_768]),
            [65_536, 32_768]
        );
        let zero = memory.channels("first".into(), &[0, 0]);
        assert_eq!(scaled_channels(&zero, 0.5).unwrap(), ["32768", "16384"]);
        assert_eq!(memory.channels("replacement".into(), &[0, 0]), [0, 0]);
        for index in 0..40 {
            memory.channels(format!("device-{index}"), &[65_536, 0]);
        }
        assert_eq!(memory.0.len(), 32);
        assert_eq!(memory.channels("first".into(), &[0, 0]), [0, 0]);
    }

    #[test]
    fn subscription_ignores_stream_churn_but_includes_output_and_default_changes() {
        assert!(relevant_event("Event 'change' on sink #7"));
        assert!(relevant_event("Event 'remove' on sink #7"));
        assert!(relevant_event("Event 'change' on server #4294967295"));
        assert!(!relevant_event("Event 'change' on sink-input #8"));
        assert!(!relevant_event("Event 'new' on source-output #8"));
    }

    #[tokio::test]
    async fn canceled_binding_never_starts_a_volume_or_mute_command() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = set(
            &fixture().server,
            &fixture().sinks[0],
            VolumeState {
                level: 0.5,
                muted: true,
            },
            cancel,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("selection changed"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "run scripts/test_volume_pipewire.py; requires its private hardware-free PipeWire server"]
    async fn isolated_pipewire_service_tracks_real_endpoint_controls() -> Result<()> {
        let runtime = std::env::var("XDG_RUNTIME_DIR")?;
        ensure!(
            std::env::var("BABEL_PRIVATE_VOLUME_TEST").as_deref() == Ok("1")
                && std::path::Path::new(&runtime)
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("babel-volume-smoke-"))
                && std::env::var("PULSE_SERVER")? == format!("unix:{runtime}/pulse/native"),
            "This test only runs against the launcher's private PipeWire server"
        );
        let existing: Vec<Value> =
            serde_json::from_str(&pactl(&["--format=json", "list", "sinks"]).await?)?;
        ensure!(
            existing.is_empty(),
            "The private test server must initially have no sinks"
        );
        for (name, level) in [
            ("babel_test_output_a", "50%"),
            ("babel_test_output_b", "75%"),
        ] {
            pactl(&[
                "load-module",
                "module-null-sink",
                &format!("sink_name={name}"),
            ])
            .await?;
            pactl(&["set-sink-volume", name, level]).await?;
        }
        crate::audio::install_virtual_devices().await?;
        let (_sender, usage) = watch::channel(crate::audio::activity::EndpointUse {
            speaker_selected: true,
            ..Default::default()
        });
        let service = super::super::Service::start(
            "babel_speaker.monitor".into(),
            "babel_test_output_a".into(),
            usage.clone(),
        );
        wait_endpoint("babel_speaker", 32_768, false).await?;
        wait_endpoint("babel_test_output_a", 32_768, false).await?;
        pactl(&["set-sink-volume", "babel_speaker", "100%"]).await?;
        wait_endpoint("babel_test_output_a", NORMAL_VOLUME, false).await?;
        pactl(&["set-sink-volume", "babel_test_output_a", "25%"]).await?;
        wait_endpoint("babel_speaker", 16_384, false).await?;
        pactl(&["set-sink-mute", "babel_speaker", "1"]).await?;
        wait_endpoint("babel_test_output_a", 16_384, true).await?;
        pactl(&["set-sink-mute", "babel_speaker", "0"]).await?;
        wait_endpoint("babel_test_output_a", 16_384, false).await?;
        service.select("babel_test_output_b".into());
        wait_endpoint("babel_speaker", 49_152, false).await?;
        wait_endpoint("babel_test_output_a", 16_384, false).await?;
        wait_endpoint("babel_test_output_b", 49_152, false).await?;
        drop(service);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let owned = owned_speaker_module()
            .await?
            .context("missing isolated Babel module")?;
        pactl(&["unload-module", &owned.to_string()]).await?;
        pactl(&[
            "load-module",
            "module-null-sink",
            "sink_name=babel_speaker",
            "sink_properties='babel.owner=org.babel.audio.v1'",
        ])
        .await?;
        let legacy = super::super::Service::start(
            "babel_speaker.monitor".into(),
            "babel_test_output_b".into(),
            usage,
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
        pactl(&["set-sink-volume", "babel_speaker", "50%"]).await?;
        tokio::time::sleep(Duration::from_millis(2200)).await;
        ensure!(
            !legacy.status().synchronized && legacy.status().limitation.is_some(),
            "Legacy monitor unexpectedly synchronized"
        );
        wait_endpoint("babel_test_output_b", 49_152, false).await?;
        drop(legacy);
        // The launcher tears down the entire private server even on failure.
        Ok(())
    }

    async fn wait_endpoint(name: &str, expected: u32, muted: bool) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let sinks: Vec<Value> =
                    serde_json::from_str(&pactl(&["--format=json", "list", "sinks"]).await?)?;
                let sink = endpoint(&sinks, name)?;
                if channel_volumes(sink)?.iter().max() == Some(&expected)
                    && sink["mute"].as_bool() == Some(muted)
                {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .with_context(|| format!("Timed out waiting for isolated volume endpoint {name}"))?
    }
}
