//! CoreAudio/WASAPI device I/O. Kernel/HAL loopback drivers remain separately
//! installed OS components; a userspace Rust process cannot register those.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use cpal::{
    FromSample, SampleFormat, SizedSample, Stream, StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use crossbeam_queue::ArrayQueue;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{
    AudioOptions, AudioStats, Device, DeviceDirection, PcmFrame, PlaybackCommand,
    native_ids::{self, Selection},
    native_lease::DeviceLease,
    resample::Resampler,
};

fn virtual_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        "blackhole",
        "vb-audio",
        "vb audio",
        "cable input",
        "cable output",
        "cable-a",
        "cable-b",
        "loopback",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn enumerate(direction: DeviceDirection) -> Result<Vec<(Device, cpal::Device)>> {
    let host = cpal::default_host();
    let available = match direction {
        DeviceDirection::Input => host.input_devices()?,
        DeviceDirection::Output => host.output_devices()?,
    };
    available
        .map(|device| {
            let description = device
                .description()
                .context("reading native audio device description")?;
            let name = description.name().to_owned();
            let native_id = device.id()?.to_string();
            let is_virtual = native_ids::babel_virtual_device(&native_id, description.driver())
                || description.device_type() == cpal::DeviceType::Virtual
                || description.interface_type() == cpal::InterfaceType::Virtual
                || description.driver().is_some_and(virtual_name)
                || virtual_name(&name);
            let id = native_ids::persistent_id(direction, &native_id);
            Ok((
                Device {
                    id,
                    is_virtual,
                    name,
                    direction,
                },
                device,
            ))
        })
        .collect()
}

pub(crate) fn resolve(id: &str, direction: DeviceDirection) -> Result<cpal::Device> {
    match native_ids::parse(id, direction)? {
        Selection::Stable(native) => {
            let host = cpal::default_host();
            let native: cpal::DeviceId = native.parse()?;
            ensure!(
                native.host() == host.id(),
                "device belongs to another operating system"
            );
            let device = host
                .device_by_id(&native)
                .with_context(|| format!("audio device {id:?} is disconnected"))?;
            ensure!(
                match direction {
                    DeviceDirection::Input => device.supports_input(),
                    DeviceDirection::Output => device.supports_output(),
                },
                "device does not support the selected direction"
            );
            Ok(device)
        }
        Selection::LegacyName(name) => {
            let mut candidates = enumerate(direction)?
                .into_iter()
                .filter(|(info, _)| info.name == name);
            let (_, device) = candidates.next().with_context(|| {
                format!("audio device {name:?} is unavailable; select it again after connecting")
            })?;
            ensure!(
                candidates.next().is_none(),
                "legacy device name {name:?} is ambiguous; select its persistent ID again"
            );
            Ok(device)
        }
    }
}

fn leased_device(id: &str, direction: DeviceDirection) -> Result<(DeviceLease, cpal::Device)> {
    match native_ids::parse(id, direction)? {
        Selection::Stable(native_id) => {
            // Own the identity before even asking the driver to resolve it: a
            // stuck lookup must not accumulate new workers on retries either.
            let lease = DeviceLease::acquire(direction, native_id)?;
            Ok((lease, resolve(id, direction)?))
        }
        Selection::LegacyName(_) => {
            let device = resolve(id, direction)?;
            let lease = DeviceLease::acquire(direction, &device.id()?.to_string())?;
            Ok((lease, device))
        }
    }
}

pub async fn devices() -> Result<Vec<Device>> {
    tokio::task::spawn_blocking(|| {
        let mut found: Vec<Device> = enumerate(DeviceDirection::Input)?
            .into_iter()
            .map(|(info, _)| info)
            .collect();
        found.extend(
            enumerate(DeviceDirection::Output)?
                .into_iter()
                .map(|(info, _)| info),
        );
        Ok(found)
    })
    .await
    .context("native audio enumeration task failed")?
}

pub async fn install_virtual_devices() -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        if let Some(directory) = driver_distribution(&["BabelAudio.pkg"])? {
            bail!(
                "The Babel Audio installer is available at {}. Open this package explicitly and follow macOS authorization. Babel has not installed any device.",
                directory.join("BabelAudio.pkg").display()
            );
        }
        bail!(
            "The Babel Audio macOS installer is not bundled with this application. Obtain the signed BabelAudio.pkg or build it using native/macos/build.py, then place it in drivers/macos next to the Babel executable. See native/macos/README.md. No device was installed."
        );
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(directory) = driver_distribution(&[
            "BabelAudio.inf",
            "BabelAudio.sys",
            "BabelAudio.cat",
            "install.ps1",
            "babel-driver-installer.exe",
        ])? {
            bail!(
                "The Babel Audio driver package is available at {}. Follow its install.ps1 instructions with administrator authorization. Babel has not run the installer or installed any device.",
                directory.display()
            );
        }
        bail!(
            "The complete Babel Audio Windows driver package is not bundled with this application. Obtain the signed package for this architecture and place its BabelAudio.inf, BabelAudio.sys, BabelAudio.cat and installation helpers in drivers/windows next to the Babel executable. See native/windows/README.md. No device was installed."
        );
    }
}

pub async fn uninstall_virtual_devices() -> Result<String> {
    #[cfg(target_os = "macos")]
    let helper = "uninstall.sh";
    #[cfg(target_os = "windows")]
    let helper = "uninstall.ps1";
    #[cfg(target_os = "macos")]
    let required = [helper];
    #[cfg(target_os = "windows")]
    let required = [
        helper,
        "babel-driver-installer.exe",
        "BabelAudio.inf",
        "BabelAudio.cat",
    ];
    if let Some(directory) = driver_distribution(&required)? {
        bail!(
            "The Babel Audio removal helper is available at {}. Review and run it explicitly with administrator authorization. Babel has not run it or removed any device. Optional third-party cables remain managed by their own installers.",
            directory.join(helper).display()
        );
    }
    bail!(
        "The Babel Audio removal helper is not bundled with this application. Obtain the matching Babel driver package and follow its removal instructions in native/macos/README.md or native/windows/README.md. Optional third-party cables remain managed by their own installers. No device was removed."
    )
}

fn driver_distribution(required: &[&str]) -> Result<Option<PathBuf>> {
    let executable = std::env::current_exe().context("locating the Babel application package")?;
    let directory = executable
        .parent()
        .context("the Babel executable has no parent directory")?;
    #[cfg(target_os = "macos")]
    let candidates = [
        PathBuf::from("drivers/macos"),
        PathBuf::from("../Resources/drivers/macos"),
        PathBuf::from("native/macos/dist"),
        PathBuf::from("native/macos"),
        PathBuf::from("."),
    ];
    #[cfg(target_os = "windows")]
    let candidates = [
        PathBuf::from("drivers/windows"),
        PathBuf::from("native/windows/dist").join(if cfg!(target_arch = "aarch64") {
            "ARM64"
        } else {
            "x64"
        }),
        PathBuf::from("native/windows"),
        PathBuf::from("."),
    ];
    // These paths are fixed relative to this binary, never the launch directory
    // or configuration data. Finding a helper only produces instructions; this
    // code does not execute it, auto-elevate, or claim a successful installation.
    find_distribution(directory, &candidates, required)
}

fn find_distribution(
    base: &Path,
    candidates: &[PathBuf],
    required: &[&str],
) -> Result<Option<PathBuf>> {
    for candidate in candidates {
        let directory = base.join(candidate);
        if required.iter().all(|name| directory.join(name).is_file()) {
            return directory
                .canonicalize()
                .context("resolving the Babel native driver package path")
                .map(Some);
        }
    }
    Ok(None)
}

#[cfg(test)]
mod installation_tests {
    use super::*;

    #[test]
    fn installation_guidance_requires_all_expected_files_from_one_package() {
        let temp = tempfile::tempdir().unwrap();
        let packaged = temp.path().join("drivers/test");
        let unrelated = temp.path().join("unrelated");
        std::fs::create_dir_all(&packaged).unwrap();
        std::fs::create_dir_all(&unrelated).unwrap();
        std::fs::write(packaged.join("driver.inf"), "fixture").unwrap();
        std::fs::write(unrelated.join("driver.sys"), "fixture").unwrap();
        let candidates = [PathBuf::from("drivers/test")];
        let required = ["driver.inf", "driver.sys"];
        assert!(
            find_distribution(temp.path(), &candidates, &required)
                .unwrap()
                .is_none()
        );
        std::fs::write(packaged.join("driver.sys"), "fixture").unwrap();
        assert_eq!(
            find_distribution(temp.path(), &candidates, &required).unwrap(),
            Some(packaged.canonicalize().unwrap())
        );
    }
}

fn input_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    queue: Arc<ArrayQueue<f32>>,
    failed: Arc<AtomicBool>,
    stats: Arc<AudioStats>,
) -> Result<Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels);
    Ok(device.build_input_stream(
        *config,
        move |data: &[T], _| {
            let frames = data.len() / channels;
            // Dropping a whole hardware callback avoids partially spliced frames.
            if queue.capacity() - queue.len() < frames {
                stats.dropped_frames.fetch_add(1, Ordering::Relaxed);
                return;
            }
            for frame in data.chunks_exact(channels) {
                // Use channels 1/2 only: BlackHole 16ch's unused channels are zero.
                let mono = frame
                    .iter()
                    .take(2)
                    .map(|sample| sample.to_sample::<f32>())
                    .sum::<f32>()
                    / channels.min(2) as f32;
                let _ = queue.push(mono);
            }
        },
        move |_| {
            failed.store(true, Ordering::Release);
        },
        None,
    )?)
}

fn build_input(
    device: &cpal::Device,
    config: &StreamConfig,
    format: SampleFormat,
    queue: Arc<ArrayQueue<f32>>,
    failed: Arc<AtomicBool>,
    stats: Arc<AudioStats>,
) -> Result<Stream> {
    match format {
        SampleFormat::I8 => input_stream::<i8>(device, config, queue, failed, stats),
        SampleFormat::I16 => input_stream::<i16>(device, config, queue, failed, stats),
        SampleFormat::I24 => input_stream::<cpal::I24>(device, config, queue, failed, stats),
        SampleFormat::I32 => input_stream::<i32>(device, config, queue, failed, stats),
        SampleFormat::I64 => input_stream::<i64>(device, config, queue, failed, stats),
        SampleFormat::U8 => input_stream::<u8>(device, config, queue, failed, stats),
        SampleFormat::U16 => input_stream::<u16>(device, config, queue, failed, stats),
        SampleFormat::U24 => input_stream::<cpal::U24>(device, config, queue, failed, stats),
        SampleFormat::U32 => input_stream::<u32>(device, config, queue, failed, stats),
        SampleFormat::U64 => input_stream::<u64>(device, config, queue, failed, stats),
        SampleFormat::F32 => input_stream::<f32>(device, config, queue, failed, stats),
        SampleFormat::F64 => input_stream::<f64>(device, config, queue, failed, stats),
        other => bail!("unsupported native input sample format: {other}"),
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
    // Aborting the async task must also stop its spawn_blocking device worker.
    let cancel = cancel.child_token();
    let _cancel_on_drop = cancel.clone().drop_guard();
    let id = device.to_owned();
    tokio::task::spawn_blocking(move || -> Result<()> {
        if cancel.is_cancelled() { return Ok(()); }
        let (_lease, device) = leased_device(&id, DeviceDirection::Input)?;
        let supported = device
            .default_input_config()
            .context("reading native input format")?;
        let config = supported.config();
        ensure!(
            config.channels > 0 && (8_000..=768_000).contains(&config.sample_rate),
            "invalid or unsupported native audio format"
        );
        let queue = Arc::new(ArrayQueue::new(
            (u64::from(config.sample_rate) * u64::from(options.queue_ms) / 1000) as usize,
        ));
        let failed = Arc::new(AtomicBool::new(false));
        let stream = build_input(
            &device,
            &config,
            supported.sample_format(),
            queue.clone(),
            failed.clone(),
            stats.clone(),
        )?;
        let mut resampler = Resampler::new(config.sample_rate, options.sample_rate);
        let mut input = Vec::with_capacity(4096);
        let mut output = Vec::with_capacity(8192);
        let mut frame = Vec::with_capacity(options.frame_samples());
        if cancel.is_cancelled() { return Ok(()); }
        stream
            .play()
            .context("starting native microphone capture; check OS microphone permission")?;
        let mut last_samples = Instant::now();
        while !cancel.is_cancelled() && !sink.is_closed() {
            ensure!(
                !failed.load(Ordering::Acquire),
                "native input stream failed or the device was disconnected"
            );
            input.clear();
            while input.len() < 4096 {
                let Some(sample) = queue.pop() else {
                    break;
                };
                input.push(sample);
            }
            if input.is_empty() {
                ensure!(last_samples.elapsed() < Duration::from_secs(2), "native input stopped delivering samples; check the device connection and microphone permission");
                thread::sleep(Duration::from_millis(2));
                continue;
            }
            last_samples = Instant::now();
            output.clear();
            resampler.process(&input, &mut output);
            for sample in &output {
                frame.push((sample.clamp(-1.0, 1.0) * 32767.0).round() as i16);
                if frame.len() == options.frame_samples() {
                    let samples =
                        std::mem::replace(&mut frame, Vec::with_capacity(options.frame_samples()));
                    stats.captured_frames.fetch_add(1, Ordering::Relaxed);
                    match sink.try_send(PcmFrame {
                        samples,
                        sample_rate: options.sample_rate,
                        captured_at: Instant::now(),
                    }) {
                        Ok(()) => (),
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            stats.dropped_frames.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => return Ok(()),
                    }
                }
            }
        }
        drop(stream);
        Ok(())
    })
    .await
    .context("native capture worker failed")?
}

#[derive(Clone, Copy)]
struct OutputSample {
    value: f32,
    generation: u64,
}

fn output_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    queue: Arc<ArrayQueue<OutputSample>>,
    failed: Arc<AtomicBool>,
    stats: Arc<AudioStats>,
) -> Result<Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels);
    Ok(device.build_output_stream(
        *config,
        move |data: &mut [T], _| {
            let mut underrun = false;
            for frame in data.chunks_exact_mut(channels) {
                let generation = stats.playback_generation.load(Ordering::Acquire);
                let sample = match queue.pop() {
                    Some(sample) if sample.generation == generation => sample.value,
                    _ => {
                        underrun = true;
                        0.0
                    }
                };
                for (channel, target) in frame.iter_mut().enumerate() {
                    *target = T::from_sample(if channel < 2 { sample } else { 0.0 });
                }
            }
            if underrun {
                stats.underruns.fetch_add(1, Ordering::Relaxed);
            }
        },
        move |_| {
            failed.store(true, Ordering::Release);
        },
        None,
    )?)
}

fn build_output(
    device: &cpal::Device,
    config: &StreamConfig,
    format: SampleFormat,
    queue: Arc<ArrayQueue<OutputSample>>,
    failed: Arc<AtomicBool>,
    stats: Arc<AudioStats>,
) -> Result<Stream> {
    match format {
        SampleFormat::I8 => output_stream::<i8>(device, config, queue, failed, stats),
        SampleFormat::I16 => output_stream::<i16>(device, config, queue, failed, stats),
        SampleFormat::I24 => output_stream::<cpal::I24>(device, config, queue, failed, stats),
        SampleFormat::I32 => output_stream::<i32>(device, config, queue, failed, stats),
        SampleFormat::I64 => output_stream::<i64>(device, config, queue, failed, stats),
        SampleFormat::U8 => output_stream::<u8>(device, config, queue, failed, stats),
        SampleFormat::U16 => output_stream::<u16>(device, config, queue, failed, stats),
        SampleFormat::U24 => output_stream::<cpal::U24>(device, config, queue, failed, stats),
        SampleFormat::U32 => output_stream::<u32>(device, config, queue, failed, stats),
        SampleFormat::U64 => output_stream::<u64>(device, config, queue, failed, stats),
        SampleFormat::F32 => output_stream::<f32>(device, config, queue, failed, stats),
        SampleFormat::F64 => output_stream::<f64>(device, config, queue, failed, stats),
        other => bail!("unsupported native output sample format: {other}"),
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
    // Aborting the async task must also stop its spawn_blocking device worker.
    let cancel = cancel.child_token();
    let _cancel_on_drop = cancel.clone().drop_guard();
    let id = device.to_owned();
    tokio::task::spawn_blocking(move || -> Result<()> {
        if cancel.is_cancelled() { return Ok(()); }
        let (_lease, device) = leased_device(&id, DeviceDirection::Output)?;
        let supported = device
            .default_output_config()
            .context("reading native output format")?;
        let config = supported.config();
        ensure!(
            config.channels > 0 && (8_000..=768_000).contains(&config.sample_rate),
            "invalid or unsupported native audio format"
        );
        // Keep the hardware bridge short; queue_ms bounds the engine's speech queue.
        let ring_ms = options.latency_ms.max(options.frame_ms * 2);
        let queue = Arc::new(ArrayQueue::new(
            (u64::from(config.sample_rate) * u64::from(ring_ms) / 1000) as usize,
        ));
        let failed = Arc::new(AtomicBool::new(false));
        let stream = build_output(
            &device,
            &config,
            supported.sample_format(),
            queue.clone(),
            failed.clone(),
            stats.clone(),
        )?;
        let mut resampler = Resampler::new(options.sample_rate, config.sample_rate);
        let mut input = Vec::with_capacity(options.frame_samples());
        let mut output = Vec::with_capacity((config.sample_rate / 5) as usize);
        let mut generation = stats.playback_generation.load(Ordering::Acquire);
        if cancel.is_cancelled() { return Ok(()); }
        stream.play().context("starting native playback")?;
        'playback: while !cancel.is_cancelled() {
            ensure!(
                !failed.load(Ordering::Acquire),
                "native output stream failed or the device was disconnected"
            );
            let current = stats.playback_generation.load(Ordering::Acquire);
            if generation != current {
                while queue.pop().is_some() {}
                resampler.reset();
                generation = current;
            }
            match source.try_recv() {
                Err(mpsc::error::TryRecvError::Disconnected) => break,
                Err(mpsc::error::TryRecvError::Empty) => {
                    thread::sleep(Duration::from_millis(2));
                }
                Ok(PlaybackCommand::Flush) => {
                    while queue.pop().is_some() {}
                    resampler.reset();
                }
                Ok(PlaybackCommand::Audio {
                    samples,
                    generation: queued_generation,
                }) => {
                    if queued_generation != generation {
                        continue;
                    }
                    for chunk in samples.chunks(options.frame_samples()) {
                        input.clear();
                        input.extend(chunk.iter().map(|sample| f32::from(*sample) / 32768.0));
                        output.clear();
                        resampler.process(&input, &mut output);
                        for sample in &output {
                            let mut sample = OutputSample {
                                value: *sample,
                                generation,
                            };
                            let mut stalled_since = None;
                            loop {
                                if cancel.is_cancelled() {
                                    break 'playback;
                                }
                                if stats.playback_generation.load(Ordering::Acquire) != generation {
                                    continue 'playback;
                                }
                                ensure!(
                                    !failed.load(Ordering::Acquire),
                                    "native output stream failed or the device was disconnected"
                                );
                                match queue.push(sample) {
                                    Ok(()) => break,
                                    Err(returned) => {
                                        let since = stalled_since.get_or_insert_with(Instant::now);
                                        ensure!(since.elapsed() < Duration::from_millis(u64::from(options.queue_ms.max(options.latency_ms)) + 500), "native audio output stalled; the device stopped consuming samples");
                                        sample = returned;
                                        thread::sleep(Duration::from_millis(2));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        drop(stream);
        Ok(())
    })
    .await
    .context("native playback worker failed")?
}
