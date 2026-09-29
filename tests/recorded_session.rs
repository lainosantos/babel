#![cfg(target_os = "linux")]
//! Explicit live test: synthetic tones, four isolated test-only null sinks.
//! Existing Babel devices and physical devices are never changed or captured.
use anyhow::{Context, Result, ensure};
use babel_audio::{
    audio::{self, AudioOptions, AudioStats, PlaybackCommand},
    config::AppConfig,
    engine::Controller,
};
use std::{
    collections::BTreeSet,
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    process::{Child, Command},
    sync::mpsc,
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

async fn pactl(args: &[String]) -> Result<String> {
    let mut command = Command::new("pactl");
    command.args(args).kill_on_drop(true);
    let result = tokio::time::timeout(Duration::from_secs(5), command.output())
        .await
        .context("pactl timed out")??;
    ensure!(
        result.status.success(),
        "pactl failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(String::from_utf8(result.stdout)?.trim().to_owned())
}
async fn babel_devices() -> Result<BTreeSet<String>> {
    Ok(audio::devices()
        .await?
        .into_iter()
        .filter(|d| d.id.starts_with("babel_"))
        .map(|d| d.id)
        .collect())
}

fn external_client(device: &str, capture: bool) -> Result<Child> {
    // These fixture clients select the virtual endpoints. Babel-owned synthetic
    // producers/observers alone must not activate the production activity gate.
    Ok(Command::new("pacat")
        .args([
            if capture { "--record" } else { "--playback" },
            "--raw",
            "--format=s16le",
            "--channels=1",
            "--rate=16000",
            "--client-name=Babel recording regression",
        ])
        .arg(format!("--device={device}"))
        .stdin(if capture {
            Stdio::null()
        } else {
            Stdio::from(std::fs::File::open("/dev/zero")?)
        })
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?)
}

async fn stop_external_clients(clients: &mut Vec<Child>) -> Result<()> {
    let mut failure = None;
    for client in clients {
        if let Err(error) = client.kill().await {
            failure = Some(error);
        }
    }
    if let Some(error) = failure {
        return Err(error.into());
    }
    Ok(())
}
async fn cleanup_modules(modules: &[(u32, String)], owner: &str) -> Result<()> {
    let mut failed = None;
    for (id, name) in modules.iter().rev() {
        let result: Result<()> = async {
            let list = pactl(&["list".into(), "short".into(), "modules".into()]).await?;
            let row = list
                .lines()
                .find(|line| line.split('\t').next() == Some(id.to_string().as_str()));
            let Some(row) = row else { return Ok(()) };
            let mut columns = row.split('\t');
            let _ = columns.next();
            ensure!(
                columns.next() == Some("module-null-sink"),
                "Test module ID was reused; refusing removal"
            );
            let arguments = columns.next().unwrap_or_default();
            ensure!(
                arguments
                    .split_whitespace()
                    .any(|arg| arg == format!("sink_name={name}"))
                    && arguments
                        .split_whitespace()
                        .any(|arg| arg == format!("sink_properties=babel.test={owner}")),
                "Test module ownership changed; refusing removal"
            );
            pactl(&["unload-module".into(), id.to_string()]).await?;
            Ok(())
        }
        .await;
        if let Err(error) = result {
            failed = Some(error);
        }
    }
    if let Some(error) = failed {
        Err(error)
    } else {
        Ok(())
    }
}
fn tone(frequency: f64, frame: usize) -> Vec<i16> {
    (0..320)
        .map(|i| {
            ((std::f64::consts::TAU * frequency * (frame * 320 + i) as f64 / 16000.0).sin()
                * 12000.0) as i16
        })
        .collect()
}
fn amplitude(samples: &[i16], frequency: f64) -> f64 {
    let (mut real, mut imaginary) = (0.0, 0.0);
    for (index, &sample) in samples.iter().enumerate() {
        let angle = std::f64::consts::TAU * frequency * index as f64 / 16000.0;
        real += f64::from(sample) * angle.cos();
        imaginary += f64::from(sample) * angle.sin();
    }
    2.0 * real.hypot(imaginary) / samples.len() as f64
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires live PipeWire/PulseAudio; creates/removes only four uniquely owned test null sinks; preserves existing Babel devices"]
async fn original_tones_share_one_recorded_timeline_without_changing_babel_devices() -> Result<()> {
    let before = babel_devices().await?;
    let directory = tempfile::tempdir()?;
    let owner = format!("{}_{}", std::process::id(), rand::random::<u64>());
    let names: [String; 4] = std::array::from_fn(|i| format!("babeltestrec_{owner}_{i}"));
    let mut modules = Vec::new();
    let mut cfg = AppConfig::default();
    cfg.microphone.enabled = false;
    cfg.speaker.enabled = false;
    cfg.microphone.capture_device = format!("{}.monitor", names[0]);
    cfg.microphone.playback_device = names[1].clone();
    cfg.speaker.capture_device = format!("{}.monitor", names[2]);
    cfg.speaker.playback_device = names[3].clone();
    cfg.transcription.enabled = false;
    cfg.recording.enabled = true;
    cfg.recording.microphone = true;
    cfg.recording.speaker = true;
    cfg.recording.directory = directory
        .path()
        .join("recordings")
        .to_string_lossy()
        .into_owned();
    cfg.files.name_pattern = "mix_{session}_{id}".into();
    let recordings = std::path::PathBuf::from(&cfg.recording.directory);
    let controller = Controller::new(cfg, directory.path().join("config.toml")).unwrap();
    let cancel = CancellationToken::new();
    let _cancel_on_drop = cancel.clone().drop_guard();
    let mut feeders: Vec<JoinHandle<Result<()>>> = Vec::new();
    let mut external_clients = Vec::new();
    let result:Result<()>=async {
        for name in &names {
            let id=pactl(&["load-module".into(),"module-null-sink".into(),format!("sink_name={name}"),format!("sink_properties=babel.test={owner}"),"rate=16000".into(),"channels=1".into(),"channel_map=mono".into()]).await?.parse::<u32>()?;
            modules.push((id,name.clone()));
        }
        external_clients.push(external_client(&format!("{}.monitor",names[1]),true)?);
        external_clients.push(external_client(&names[2],false)?);
        let started=Instant::now();
        controller.start_named(Some("Gravação simultânea / teste".into())).await?;
        let options=AudioOptions{sample_rate:16000,frame_ms:20,latency_ms:40,queue_ms:200};
        let mut senders=Vec::new();
        for device in [names[0].clone(),names[2].clone()] {
            let (tx,rx)=mpsc::channel(8);senders.push(tx);let cancelled=cancel.clone();
            feeders.push(tokio::spawn(async move {audio::playback(&device,options,rx,cancelled,Arc::new(AudioStats::default())).await}));
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut tick=tokio::time::interval(Duration::from_millis(20));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        for frame in 0..150 {
            tick.tick().await;
            for (sender,frequency) in senders.iter().zip([440.0,880.0]) {
                sender.send(PlaybackCommand::Audio{samples:tone(frequency,frame),generation:0}).await?;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
        controller.stop().await?;
        let elapsed=started.elapsed().as_secs_f64();
        let status=controller.status().await;
        ensure!(!status.running && status.last_error.is_none(),"Normal stop failed: {status:?}");
        ensure!(status.session_name.as_deref()==Some("Gravação simultânea / teste"),"Session name was not preserved");
        ensure!(status.microphone.captured_frames>20 && status.speaker.captured_frames>20,"Both original capture routes must feed the recorder");
        ensure!(status.microphone.translated_samples==0 && status.speaker.translated_samples==0,"Original recording must not invoke translation");
        let files=std::fs::read_dir(&recordings)?.collect::<std::io::Result<Vec<_>>>()?;
        ensure!(files.len()==1,"Expected one mixed WAV, found {} files",files.len());
        let path=files[0].path();let name=path.file_name().unwrap().to_string_lossy();
        ensure!(name.starts_with("mix_gravação-simultânea-teste_") && name.ends_with(".wav"),"Shared filename pattern or session slug was not applied");
        let mut wav=hound::WavReader::open(&path)?;
        ensure!(wav.spec().sample_rate==16000 && wav.spec().channels==1 && wav.spec().bits_per_sample==16,"Unexpected WAV format");
        let samples=wav.samples::<i16>().collect::<std::result::Result<Vec<_>,_>>()?;
        let duration=samples.len() as f64/16000.0;
        ensure!(duration>=2.5 && duration<=elapsed+0.5,"Audio was concatenated or has incorrect timeline: WAV {duration:.2}s versus elapsed {elapsed:.2}s");
        let overlapping=samples.windows(16000).step_by(1600).any(|window|amplitude(window,440.0)>3500.0 && amplitude(window,880.0)>3500.0);
        ensure!(overlapping,"The same one-second window must contain both original tone frequencies at mixed headroom");
        println!("Mixed WAV verified: {:.2}s, two simultaneous tones, one file, normal stop clean",duration);
        Ok(())
    }.await;
    // Perform every cleanup even if a verification or process failed. Only the
    // four exact, still-owned IDs above can be unloaded; no Babel install calls.
    let stopped = controller.shutdown().await;
    cancel.cancel();
    let external_stopped = stop_external_clients(&mut external_clients).await;
    let mut feeder_error = None;
    for mut feeder in feeders {
        let joined = tokio::time::timeout(Duration::from_secs(3), &mut feeder).await;
        match joined {
            Ok(Ok(Ok(()))) => (),
            Ok(Ok(Err(error))) => feeder_error = Some(error),
            Ok(Err(error)) => feeder_error = Some(error.into()),
            Err(error) => {
                feeder.abort();
                let _ = feeder.await;
                feeder_error = Some(error.into());
            }
        }
    }
    let removed = cleanup_modules(&modules, &owner).await;
    result?;
    stopped?;
    external_stopped?;
    if let Some(error) = feeder_error {
        return Err(error);
    };
    removed?;
    ensure!(
        babel_devices().await? == before,
        "Persistent Babel endpoints changed during the isolated test"
    );
    ensure!(
        !audio::devices()
            .await?
            .iter()
            .any(|d| d.id.contains(&owner)),
        "Test endpoints remained after cleanup"
    );
    let children = Command::new("ps")
        .args(["-C", "parec", "-C", "pacat", "-o", "pid=,args="])
        .output()
        .await?;
    ensure!(
        !String::from_utf8_lossy(&children.stdout).contains(&owner),
        "Test parec/pacat processes leaked after stop"
    );
    Ok(())
}

async fn observe_original_tone(
    receiver: &mut mpsc::Receiver<audio::PcmFrame>,
    frequency: f64,
) -> Result<()> {
    // Drop stale observation frames at a phase boundary, then inspect newly
    // captured PCM so the assertion proves the current routing state.
    while receiver.try_recv().is_ok() {}
    let mut best_amplitude = 0.0_f64;
    let mut received_samples = 0_usize;
    tokio::time::timeout(Duration::from_secs(4), async {
        let mut samples = Vec::with_capacity(3200);
        while let Some(frame) = receiver.recv().await {
            received_samples += frame.samples.len();
            samples.extend(frame.samples);
            if samples.len() >= 3200 {
                best_amplitude = best_amplitude.max(amplitude(&samples[..3200], frequency));
                if best_amplitude > 6000.0 {
                    return Ok(());
                }
                samples.clear();
            }
        }
        Err(anyhow::anyhow!(
            "Observation capture closed before original tone arrived"
        ))
    })
    .await
    .with_context(||format!("Original {frequency} Hz audio did not arrive: received {received_samples} samples; highest amplitude {best_amplitude:.1}"))?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires live PipeWire/PulseAudio; uses only four isolated synthetic null sinks and preserves all persistent Babel devices"]
async fn idle_routing_recording_and_transcription_are_independent_without_cloud() -> Result<()> {
    let before = babel_devices().await?;
    let directory = tempfile::tempdir()?;
    let owner = format!("idle_{}_{}", std::process::id(), rand::random::<u64>());
    let names: [String; 4] = std::array::from_fn(|i| format!("babeltestrec_{owner}_{i}"));
    let mut modules = Vec::new();
    let mut cfg = AppConfig::default();
    // Both translation routes are disabled. An unavailable random credential
    // would make any accidental cloud/provider startup fail before a request.
    cfg.microphone.enabled = false;
    cfg.speaker.enabled = false;
    cfg.providers.gemini.api_key_env = format!("BABEL_TEST_MISSING_KEY_{}", rand::random::<u64>());
    cfg.microphone.capture_device = format!("{}.monitor", names[0]);
    cfg.microphone.playback_device = names[1].clone();
    cfg.speaker.capture_device = format!("{}.monitor", names[2]);
    cfg.speaker.playback_device = names[3].clone();
    cfg.transcription.enabled = false;
    cfg.transcription.directory = directory
        .path()
        .join("transcripts")
        .to_string_lossy()
        .into_owned();
    cfg.recording.enabled = true;
    cfg.recording.microphone = true;
    cfg.recording.speaker = true;
    cfg.recording.directory = directory
        .path()
        .join("recordings")
        .to_string_lossy()
        .into_owned();
    cfg.files.name_pattern = "noai_{session}_{id}".into();
    let recordings = std::path::PathBuf::from(&cfg.recording.directory);
    let controller = Controller::new(cfg, directory.path().join("config.toml")).unwrap();
    let cancel = CancellationToken::new();
    let _cancel_on_drop = cancel.clone().drop_guard();
    let mut workers: Vec<JoinHandle<Result<()>>> = Vec::new();
    let mut external_clients = Vec::new();
    let result:Result<()>=async {
        for name in &names {
            let id=pactl(&["load-module".into(),"module-null-sink".into(),format!("sink_name={name}"),format!("sink_properties=babel.test={owner}"),"rate=16000".into(),"channels=1".into(),"channel_map=mono".into()]).await?.parse::<u32>()?;
            modules.push((id,name.clone()));
        }
        external_clients.push(external_client(&format!("{}.monitor",names[1]),true)?);
        external_clients.push(external_client(&names[2],false)?);
        controller.enable_routing().await?;
        let options=AudioOptions{sample_rate:16000,frame_ms:20,latency_ms:40,queue_ms:200};
        let mut senders=Vec::new();
        for device in [names[0].clone(),names[2].clone()] {
            let (tx,rx)=mpsc::channel(8);senders.push(tx);let cancelled=cancel.clone();
            workers.push(tokio::spawn(async move {audio::playback(&device,options,rx,cancelled,Arc::new(AudioStats::default())).await}));
        }
        let (mic_tx,mut mic_rx)=mpsc::channel(32);let (speaker_tx,mut speaker_rx)=mpsc::channel(32);
        for (device,tx) in [(format!("{}.monitor",names[1]),mic_tx),(format!("{}.monitor",names[3]),speaker_tx)] {
            let cancelled=cancel.clone();
            workers.push(tokio::spawn(async move {audio::capture(&device,options,tx,cancelled,Arc::new(AudioStats::default())).await}));
        }
        let producer_cancel=cancel.clone();
        workers.push(tokio::spawn(async move {
            let mut tick=tokio::time::interval(Duration::from_millis(20));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut frame=0;
            loop {
                tokio::select!{biased;_=producer_cancel.cancelled()=>return Ok(()),_=tick.tick()=>{}}
                for (sender,frequency) in senders.iter().zip([440.0,880.0]) {
                    tokio::select!{biased;_=producer_cancel.cancelled()=>return Ok(()),sent=sender.send(PlaybackCommand::Audio{samples:tone(frequency,frame),generation:0})=>{sent?;}}
                }
                frame+=1;
            }
        }));
        let (mic,speaker)=tokio::join!(observe_original_tone(&mut mic_rx,440.0),observe_original_tone(&mut speaker_rx,880.0));
        if mic.is_err() || speaker.is_err() { anyhow::bail!("Mic={mic:?}, speaker={speaker:?}; status={:?}", controller.status().await); }
        let idle=controller.status().await;
        ensure!(!idle.running && idle.routing_active && idle.routing_error.is_none(),"Idle original routing failed: {idle:?}");
        ensure!(!recordings.exists(),"Idle routing must not create recording files before the user starts a session");
        controller.start_named(Some("Gravação sem IA".into())).await?;
        let (mic,speaker)=tokio::join!(observe_original_tone(&mut mic_rx,440.0),observe_original_tone(&mut speaker_rx,880.0));
        if mic.is_err() || speaker.is_err() { anyhow::bail!("Mic={mic:?}, speaker={speaker:?}; status={:?}", controller.status().await); }
        tokio::time::sleep(Duration::from_millis(1800)).await;
        let active=controller.status().await;
        ensure!(active.running && active.last_error.is_none(),"Recording-only session failed: {active:?}");
        ensure!(active.microphone.translated_samples==0 && active.speaker.translated_samples==0,"Recording-only mode must not count provider-translated audio");
        controller.stop().await?;
        let (mic,speaker)=tokio::join!(observe_original_tone(&mut mic_rx,440.0),observe_original_tone(&mut speaker_rx,880.0));
        if mic.is_err() || speaker.is_err() { anyhow::bail!("Mic={mic:?}, speaker={speaker:?}; status={:?}", controller.status().await); }
        let idle=controller.status().await;
        ensure!(!idle.running && idle.routing_active && idle.last_error.is_none() && idle.routing_error.is_none(),"Original routing did not resume cleanly after Stop: {idle:?}");
        let files=std::fs::read_dir(&recordings)?.collect::<std::io::Result<Vec<_>>>()?;
        ensure!(files.len()==1,"Recording-only session must produce exactly one mixed WAV");
        let path=files[0].path();ensure!(path.file_name().unwrap().to_string_lossy().starts_with("noai_gravação-sem-ia_"),"Recording-only name pattern was not applied");
        let mut wav=hound::WavReader::open(path)?;
        let samples=wav.samples::<i16>().collect::<std::result::Result<Vec<_>,_>>()?;
        ensure!(samples.windows(16000).step_by(1600).any(|window|amplitude(window,440.0)>3500.0 && amplitude(window,880.0)>3500.0),"Original mixed recording must contain both sources even with all translation disabled");
        println!("Idle original audio -> recording-only ({:.2}s mixed WAV) -> original audio verified without provider",samples.len() as f64/16000.0);

        // A transcription-only local session keeps the same original-audio
        // routing and calls only ASR. Translation and synthesis are instrumented
        // forbidden endpoints; no real local model or cloud key is needed.
        use axum::{Json, Router, body::Bytes, routing::post};
        use std::sync::atomic::{AtomicUsize,Ordering};
        let asr_calls=Arc::new(AtomicUsize::new(0));
        let translation_calls=Arc::new(AtomicUsize::new(0));
        let synthesis_calls=Arc::new(AtomicUsize::new(0));
        let asr=asr_calls.clone();let translation=translation_calls.clone();let synthesis=synthesis_calls.clone();
        let app=Router::new()
            .route("/inference",post(move |body:Bytes| {let asr=asr.clone();async move {
                if body.windows(4).any(|bytes|bytes==b"RIFF") {asr.fetch_add(1,Ordering::SeqCst);}
                Json(serde_json::json!({"text":"fala original simulada"}))
            }}))
            .route("/api/chat",post(move || {translation.fetch_add(1,Ordering::SeqCst);async {Json(serde_json::json!({"error":"translation must not run"}))}}))
            .route("/synthesize",post(move || {synthesis.fetch_add(1,Ordering::SeqCst);async {Json(serde_json::json!({"error":"synthesis must not run"}))}}));
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base=format!("http://{}",listener.local_addr()?);
        let server_cancel=cancel.clone();
        workers.push(tokio::spawn(async move {axum::serve(listener,app).with_graceful_shutdown(server_cancel.cancelled_owned()).await.context("ASR mock server failed")}));
        let mut cfg=controller.config().await;
        cfg.microphone.provider="local".into();cfg.speaker.provider="local".into();
        cfg.recording.enabled=false;
        cfg.transcription.enabled=true;cfg.transcription.microphone=true;cfg.transcription.speaker=true;
        cfg.providers.local.whisper_endpoint=format!("{base}/inference");
        cfg.providers.local.ollama_endpoint=format!("{base}/api/chat");
        cfg.providers.local.piper_endpoint=format!("{base}/synthesize");
        cfg.providers.local.segment_ms=500;cfg.providers.local.silence_ms=100;
        let transcripts=std::path::PathBuf::from(&cfg.transcription.directory);
        controller.set_config(cfg).await?;
        controller.start_named(Some("Texto original sem tradução".into())).await?;
        let (mic,speaker)=tokio::join!(observe_original_tone(&mut mic_rx,440.0),observe_original_tone(&mut speaker_rx,880.0));
        if mic.is_err() || speaker.is_err() { anyhow::bail!("Mic={mic:?}, speaker={speaker:?}; status={:?}", controller.status().await); }
        let texts=std::fs::read_dir(&transcripts)?.collect::<std::io::Result<Vec<_>>>()?;
        ensure!(texts.len()==1,"Transcription-only session must create one merged TXT");
        let text_path=texts[0].path();
        tokio::time::timeout(Duration::from_secs(5),async {
            loop {
                let text=tokio::fs::read_to_string(&text_path).await?;
                if text.contains("[microfone]") && text.contains("[saída recebida]") && text.contains("fala original simulada") {return Ok::<_,anyhow::Error>(());}
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }).await.context("Original ASR text from both routes did not reach the merged TXT")??;
        controller.stop().await?;
        ensure!(asr_calls.load(Ordering::SeqCst)>=2,"Both original streams should reach the ASR mock");
        ensure!(translation_calls.load(Ordering::SeqCst)==0 && synthesis_calls.load(Ordering::SeqCst)==0,"Transcription-only mode invoked translation or speech synthesis");
        ensure!(std::fs::read_dir(&recordings)?.count()==1,"Transcription-only mode unexpectedly created another WAV");
        let stopped=controller.status().await;
        ensure!(!stopped.running && stopped.last_error.is_none() && stopped.routing_active,"Transcription-only stop did not restore original routing: {stopped:?}");
        let (mic,speaker)=tokio::join!(observe_original_tone(&mut mic_rx,440.0),observe_original_tone(&mut speaker_rx,880.0));
        if mic.is_err() || speaker.is_err() { anyhow::bail!("Mic={mic:?}, speaker={speaker:?}; status={:?}", controller.status().await); }
        println!("Local transcription-only verified: one merged original TXT, {} ASR calls, zero translation/synthesis calls; original audio remained audible",asr_calls.load(Ordering::SeqCst));
        Ok(())
    }.await;
    // Shutdown, unlike Stop, must not restart idle capture while cleaning up.
    let stopped = controller.shutdown().await;
    cancel.cancel();
    let external_stopped = stop_external_clients(&mut external_clients).await;
    let mut worker_error = None;
    for mut worker in workers {
        match tokio::time::timeout(Duration::from_secs(3), &mut worker).await {
            Ok(Ok(Ok(()))) => (),
            Ok(Ok(Err(error))) => worker_error = Some(error),
            Ok(Err(error)) => worker_error = Some(error.into()),
            Err(error) => {
                worker.abort();
                let _ = worker.await;
                worker_error = Some(error.into());
            }
        }
    }
    let removed = cleanup_modules(&modules, &owner).await;
    result?;
    stopped?;
    external_stopped?;
    if let Some(error) = worker_error {
        return Err(error);
    };
    removed?;
    let final_status = controller.status().await;
    ensure!(
        !final_status.routing_active && !final_status.running,
        "Shutdown left original routing active"
    );
    ensure!(
        babel_devices().await? == before,
        "Persistent Babel devices changed during the isolated idle-routing test"
    );
    ensure!(
        !audio::devices()
            .await?
            .iter()
            .any(|d| d.id.contains(&owner)),
        "Idle-routing test endpoints leaked"
    );
    let children = Command::new("ps")
        .args(["-C", "parec", "-C", "pacat", "-o", "pid=,args="])
        .output()
        .await?;
    ensure!(
        !String::from_utf8_lossy(&children.stdout).contains(&owner),
        "Idle-routing test audio processes leaked"
    );
    Ok(())
}
