//! Exercise two real local speech pipelines without opening audio devices.
//! Uses managed dynamic ports and cached models; never reads application config.
use anyhow::{Context, Result, bail, ensure};
use babel_audio::{
    audio::{OriginalFrame, speech::SpeechTap},
    config::AppConfig,
    local_runtime::RuntimeManager,
    provider::{ProviderEvent, SessionConfig, create_configured_provider},
};
use serde_json::json;
use std::{
    io::Cursor,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{sync::mpsc, task::JoinSet, time::timeout};
use tokio_util::sync::CancellationToken;

const RATE: usize = 16_000;
const FRAME: usize = RATE / 50;

#[derive(Default)]
struct Metrics {
    audible_samples: AtomicU64,
    turns: AtomicU64,
    warnings: AtomicU64,
    alignment_ms: AtomicU64,
}

impl Metrics {
    fn snapshot(&self) -> [u64; 4] {
        [
            self.audible_samples.load(Ordering::Relaxed),
            self.turns.load(Ordering::Relaxed),
            self.warnings.load(Ordering::Relaxed),
            self.alignment_ms.load(Ordering::Relaxed),
        ]
    }
}

struct Route {
    input: mpsc::Sender<Vec<i16>>,
    speech: Vec<i16>,
    metrics: Arc<Metrics>,
    submitted: u64,
}

fn ensure_running(workers: &mut JoinSet<Result<()>>) -> Result<()> {
    if let Some(result) = workers.try_join_next() {
        result.context("Local route task failed")??;
        bail!("Local route stopped before cancellation");
    }
    Ok(())
}

async fn send(route: &mut Route, samples: Vec<i16>) -> Result<()> {
    let size = samples.len();
    timeout(Duration::from_secs(2), route.input.send(samples))
        .await
        .context("Synthetic input stopped draining")?
        .context("Local route closed its synthetic input channel")?;
    route.submitted += size as u64;
    Ok(())
}

async fn utterance(route: &mut Route) -> Result<()> {
    let mut clock = tokio::time::interval(Duration::from_millis(20));
    clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Include real-time silence while retaining the same open input channel.
    for _ in 0..50 {
        clock.tick().await;
        send(route, vec![0; FRAME]).await?;
    }
    for start in (0..route.speech.len()).step_by(FRAME) {
        clock.tick().await;
        let end = (start + FRAME).min(route.speech.len());
        send(route, route.speech[start..end].to_vec()).await?;
    }
    for _ in 0..30 {
        clock.tick().await;
        send(route, vec![0; FRAME]).await?;
    }
    Ok(())
}

async fn stress(route: &mut Route) -> Result<()> {
    // Deliver 64 seconds faster than real time. The live provider must shed
    // excess work with a warning rather than close either live audio channel.
    for second in 0..64 {
        let samples = (0..RATE)
            .map(|index| route.speech[(second * RATE + index) % route.speech.len()])
            .collect();
        send(route, samples).await?;
    }
    send(route, vec![0; RATE]).await?;
    Ok(())
}

async fn wait_audio(
    routes: &[Route; 2],
    previous: [[u64; 4]; 2],
    workers: &mut JoinSet<Result<()>>,
) -> Result<()> {
    timeout(Duration::from_secs(90), async {
        loop {
            ensure_running(workers)?;
            if routes.iter().zip(previous).all(|(route, before)| {
                let now = route.metrics.snapshot();
                now[0] > before[0] && now[1] > before[1]
            }) {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("Both local routes did not produce audible translated turns")?
}

fn report(phase: &str, routes: &[Route; 2], started: Instant) {
    let metrics: Vec<_> = routes
        .iter()
        .map(|route| route.metrics.snapshot())
        .collect();
    println!(
        "{phase}: {:.2}s; [audible samples, completed turns, warnings, source alignment ms] = {metrics:?}",
        started.elapsed().as_secs_f64()
    );
}

async fn synthetic_speech(
    client: &reqwest::Client,
    endpoint: &str,
    voice: &str,
    text: &str,
) -> Result<Vec<i16>> {
    let response = client
        .post(endpoint)
        .json(&json!({"voice":voice,"text":text}))
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    ensure!(
        response.len() <= 2 * 1024 * 1024,
        "Synthetic WAV exceeds its bound"
    );
    let mut wav = hound::WavReader::new(Cursor::new(response))?;
    let format = wav.spec();
    ensure!(
        format.bits_per_sample == 16 && format.channels == 1,
        "Unexpected synthetic WAV format"
    );
    let samples: Vec<f32> = wav
        .samples::<i16>()
        .map(|sample| sample.map(|sample| f32::from(sample) / 32768.0))
        .collect::<std::result::Result<_, _>>()?;
    let mut speech = SpeechTap::new()
        .convert(&OriginalFrame {
            samples: samples.into(),
            sample_rate: format.sample_rate,
            channels: format.channels,
            captured_at: Instant::now(),
        })
        .samples;
    let first = speech
        .iter()
        .position(|sample| sample.unsigned_abs() > 500)
        .context("Piper generated silent fixture")?;
    let last = speech
        .iter()
        .rposition(|sample| sample.unsigned_abs() > 500)
        .unwrap();
    speech = speech[first.saturating_sub(160)..(last + 161).min(speech.len())].to_vec();
    ensure!(
        !speech.is_empty() && speech.len() <= RATE * 10,
        "Invalid synthetic utterance length"
    );
    Ok(speech)
}

async fn exercise(cfg: &AppConfig, cancel: CancellationToken) -> Result<()> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()?;
    let local = &cfg.providers.local;
    let portuguese = synthetic_speech(
        &client,
        &local.piper_endpoint,
        "pt_BR-faber-medium",
        "Bom dia.",
    )
    .await?;
    let english = synthetic_speech(
        &client,
        &local.piper_endpoint,
        "en_US-lessac-medium",
        "Good morning.",
    )
    .await?;
    let mut workers = JoinSet::new();
    let mut routes = Vec::new();
    for (index, (settings, speech)) in [(&cfg.microphone, portuguese), (&cfg.speaker, english)]
        .into_iter()
        .enumerate()
    {
        let provider = create_configured_provider("local", &cfg.providers.gemini, local)?;
        let config = SessionConfig {
            model: "local".into(),
            api_key_env: String::new(),
            voice: settings.resolved_voice.clone(),
            source_language: settings.source_language.clone(),
            target_language: settings.target_language.clone(),
            prompt: String::new(),
            vad_silence_ms: local.silence_ms,
            connect_timeout_secs: 30,
            max_reconnect_attempts: 0,
            input_transcription: false,
            output_transcription: true,
        };
        let (input, audio) = mpsc::channel(8);
        let (events, mut received) = mpsc::channel(32);
        let metrics = Arc::new(Metrics::default());
        let counted = metrics.clone();
        let stop = cancel.clone();
        workers.spawn(async move {
            let running = provider.run(config, audio, events, stop.clone());
            tokio::pin!(running);
            loop {
                tokio::select! {
                    biased;
                    result = &mut running => {
                        result.with_context(|| format!("Local route {index} failed"))?;
                        ensure!(stop.is_cancelled(), "Local route {index} stopped unexpectedly");
                        return Ok(());
                    },
                    event = received.recv() => match event.context("Local events closed unexpectedly")? {
                        ProviderEvent::Audio { samples, sample_rate } => {
                            ensure!(sample_rate == 24_000, "Invalid translated sample rate");
                            counted.audible_samples.fetch_add(samples.iter().filter(|sample| sample.unsigned_abs() > 32).count() as u64, Ordering::Relaxed);
                        },
                        ProviderEvent::TurnComplete => { counted.turns.fetch_add(1, Ordering::Relaxed); },
                        ProviderEvent::Warning { .. } => { counted.warnings.fetch_add(1, Ordering::Relaxed); },
                        ProviderEvent::Transcript { input: false, metadata, .. } => {
                            counted.alignment_ms.fetch_max(metadata.alignment_ms.unwrap_or(0), Ordering::Relaxed);
                        },
                        _ => {},
                    }
                }
            }
        });
        routes.push(Route {
            input,
            speech,
            metrics,
            submitted: 0,
        });
    }
    let mut routes: [Route; 2] = routes
        .try_into()
        .map_err(|_| anyhow::anyhow!("Expected two routes"))?;
    for round in 1..=2 {
        let started = Instant::now();
        let previous = [routes[0].metrics.snapshot(), routes[1].metrics.snapshot()];
        let [first, second] = &mut routes;
        tokio::try_join!(utterance(first), utterance(second))?;
        wait_audio(&routes, previous, &mut workers).await?;
        report(
            &format!("PASS idle-to-speech round {round}"),
            &routes,
            started,
        );
    }
    let started = Instant::now();
    let previous = [routes[0].metrics.snapshot(), routes[1].metrics.snapshot()];
    let [first, second] = &mut routes;
    tokio::try_join!(stress(first), stress(second))?;
    wait_audio(&routes, previous, &mut workers).await?;
    ensure!(
        routes.iter().all(|route| route.metrics.snapshot()[2] > 0),
        "Stress did not exercise bounded overload warnings on both routes"
    );
    report("PASS accelerated continuous-input stress", &routes, started);

    let started = Instant::now();
    let boundaries = routes
        .each_ref()
        .map(|route| route.submitted * 1000 / RATE as u64);
    timeout(Duration::from_secs(90), async {
        loop {
            ensure_running(&mut workers)?;
            let previous = [routes[0].metrics.snapshot(), routes[1].metrics.snapshot()];
            let [first, second] = &mut routes;
            tokio::try_join!(utterance(first), utterance(second))?;
            wait_audio(&routes, previous, &mut workers).await?;
            if routes
                .iter()
                .zip(boundaries)
                .all(|(route, boundary)| route.metrics.snapshot()[3] >= boundary)
            {
                return Ok::<_, anyhow::Error>(());
            }
        }
    })
    .await
    .context("Fresh speech did not recover after overload")??;
    report("PASS new speech after overload", &routes, started);
    // Keep both input and event channels alive until cancellation completes.
    cancel.cancel();
    timeout(Duration::from_secs(3), async {
        while let Some(result) = workers.join_next().await {
            result??;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("Local routes did not stop promptly")??;
    println!("PASS both routes cancelled cleanly with channels still open");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut cfg = AppConfig::default();
    cfg.microphone.provider = "local".into();
    cfg.microphone.source_language = "pt-BR".into();
    cfg.microphone.target_language = "en-US".into();
    cfg.speaker.provider = "local".into();
    cfg.speaker.source_language = "en-US".into();
    cfg.speaker.target_language = "pt-BR".into();
    cfg.microphone.enabled = true;
    cfg.speaker.enabled = true;
    cfg.local_runtime.threads = 2;
    cfg.local_runtime.idle_unload_secs = 1;
    let manager = RuntimeManager::new();
    let cancel = CancellationToken::new();
    let cleanup = cancel.clone().drop_guard();
    let started = Instant::now();
    let result = timeout(Duration::from_secs(360), async {
        let (ready, _lease) = manager.resolve(&cfg, cancel.clone()).await?;
        println!(
            "PASS private runtime preparation: {:.2}s",
            started.elapsed().as_secs_f64()
        );
        exercise(&ready, cancel.clone()).await
    })
    .await
    .context("Local translation smoke exceeded six minutes")
    .and_then(|result| result);
    drop(cleanup);
    manager.shutdown().await;
    println!(
        "Local translation smoke finished after {:.2}s",
        started.elapsed().as_secs_f64()
    );
    result
}
