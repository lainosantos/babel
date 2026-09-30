//! Opt-in network smoke test: `cargo run --example gemini_connection_smoke`.
//!
//! Requires GEMINI_API_KEY in the environment and may incur provider usage.
//! Uses the application's default Gemini translation model with en-US as the
//! target. Sends only generated PCM silence; no configuration file, microphone,
//! speaker, or real speech is accessed. Does not verify translation quality.

use std::{process::ExitCode, time::Duration};

use anyhow::{Result, anyhow, ensure};
use babel_audio::{
    config::TRANSLATE_MODEL,
    provider::{ProviderEvent, SessionConfig, create_provider},
};
use tokio::{sync::mpsc, time::Instant};
use tokio_util::sync::CancellationToken;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // Gemini's public provider returns sanitized diagnostics. Never
            // print raw events, audio, transcription, environment, or requests.
            eprintln!("Gemini connection smoke failed: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    ensure!(
        std::env::var("GEMINI_API_KEY").is_ok_and(|key| !key.trim().is_empty()),
        "set GEMINI_API_KEY to a non-empty API key before running this opt-in test"
    );
    let provider = create_provider("gemini")?;
    let config = SessionConfig {
        model: TRANSLATE_MODEL.into(),
        api_key_env: "GEMINI_API_KEY".into(),
        voice: String::new(),
        source_language: "auto".into(),
        target_language: "en-US".into(),
        prompt: String::new(),
        vad_silence_ms: 180,
        connect_timeout_secs: 5,
        max_reconnect_attempts: 0,
        input_transcription: false,
        output_transcription: false,
    };
    let (audio, received_audio) = mpsc::channel(8);
    let (events, mut received_events) = mpsc::channel(16);
    let cancel = CancellationToken::new();
    let provider_cancel = cancel.clone();
    let mut worker = tokio::spawn(async move {
        provider
            .run(config, received_audio, events, provider_cancel)
            .await
    });

    println!("Connecting to Gemini Live Translate; target en-US; retries disabled.");
    let deadline = tokio::time::sleep(Duration::from_secs(35));
    tokio::pin!(deadline);
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut connected_at = None;
    let mut phase = 0;
    let mut events_open = true;
    let mut worker_finished = false;
    let observation = loop {
        tokio::select! {
            biased;
            completion = &mut worker => {
                worker_finished = true;
                break match completion {
                    Ok(Err(error)) => Err(error),
                    Ok(Ok(())) => Err(anyhow!("provider ended before the observation completed")),
                    Err(_) => Err(anyhow!("provider task ended unexpectedly")),
                };
            }
            _ = &mut deadline => break Err(anyhow!("connection test exceeded 35 seconds")),
            event = received_events.recv(), if events_open => {
                match event {
                    Some(ProviderEvent::Connected) => {
                        connected_at = Some(Instant::now());
                        println!("Setup acknowledged; sending two seconds of synthetic PCM silence.");
                    }
                    Some(ProviderEvent::Reconnecting { .. }) => {
                        break Err(anyhow!("provider attempted to reconnect despite retries being disabled"));
                    }
                    Some(ProviderEvent::Warning { .. }) => println!("Provider warning received."),
                    Some(_) => {} // Drain output without exposing any returned content.
                    None => {
                        // EOF may arrive just before the actual task error. Keep
                        // waiting for that result under the overall deadline.
                        events_open = false;
                    }
                }
            }
            _ = tick.tick(), if connected_at.is_some() => {
                let elapsed = connected_at.unwrap().elapsed();
                if elapsed >= Duration::from_secs(20) {
                    break Ok(());
                }
                if elapsed >= Duration::from_secs(2) && elapsed < Duration::from_secs(5) {
                    if phase == 0 {
                        phase = 1;
                        println!("Pausing input for three seconds to exercise audioStreamEnd.");
                    }
                } else {
                    if phase == 1 {
                        phase = 2;
                        println!("Resuming synthetic silence through the 15-second heartbeat window.");
                    }
                    if audio.try_send(vec![0i16; 1600]).is_err() {
                        break Err(anyhow!("synthetic audio queue is unavailable or congested"));
                    }
                }
            }
        }
    };

    // Preserve the source channel until cancellation so an intentional stop
    // cannot be mistaken for unexpected audio EOF. Total budget is at most
    // 35 seconds of observation plus three seconds for cooperative shutdown.
    cancel.cancel();
    let shutdown = if worker_finished {
        Ok(())
    } else {
        match tokio::time::timeout(Duration::from_secs(3), &mut worker).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(anyhow!("provider task failed during shutdown")),
            Err(_) => {
                worker.abort();
                Err(anyhow!("provider did not stop within three seconds"))
            }
        }
    };
    observation?;
    shutdown?;
    println!("Passed: connection remained active for 20 seconds and stopped cleanly.");
    Ok(())
}
