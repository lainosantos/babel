//! Inference-only verification of packaged components. Never opens an audio
//! device and never sends microphone/system audio or credentials to a server.
use anyhow::{Context, Result, ensure};
use babel_audio::{config::AppConfig, local_runtime::RuntimeManager};
use serde_json::json;
use std::{io::Cursor, time::Duration};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<()> {
    let mut cfg = AppConfig::default();
    cfg.microphone.provider = "local".into();
    cfg.microphone.target_language = "en-US".into();
    cfg.speaker.enabled = false;
    cfg.local_runtime.idle_unload_secs = 1;
    let manager = RuntimeManager::new();
    let operation = async {
        manager.reconcile(&cfg);
        loop {
            let status = manager.status();
            ensure!(
                status.services.is_empty(),
                "Selection loaded an idle engine"
            );
            if status.phase == "cached" {
                break;
            }
            ensure!(
                status.phase != "error",
                "Asset preparation: {:?}",
                status.message
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let resolved = manager.resolve(&cfg, CancellationToken::new());
        tokio::pin!(resolved);
        let (ready, runtime_lease) = loop {
            tokio::select! {
                result = &mut resolved => break result?,
                _ = tokio::time::sleep(Duration::from_secs(5)) => {
                    let status = manager.status();
                    if let Some(download) = status.download {
                        println!("{}: {} / {} bytes", download.name, download.received, download.total);
                    } else { println!("Local runtime: {}", status.phase); }
                }
            }
        };
        let local = ready.providers.local;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()?;
        let translation: serde_json::Value = client.post(&local.ollama_endpoint).json(&json!({
            "messages":[{"role":"system","content":"Translate the user text to English. Return only the translation."},{"role":"user","content":"Bom dia."}],
            "model":local.translation_model,"stream":false,"max_tokens":64,"chat_template_kwargs":{"enable_thinking":false}
        })).send().await?.error_for_status()?.json().await?;
        ensure!(
            translation["choices"][0]["message"]["content"]
                .as_str()
                .is_some_and(|s| !s.trim().is_empty()),
            "Empty local translation"
        );
        let speech = client
            .post(&local.piper_endpoint)
            .json(&json!({"text":"Good morning.","voice":ready.microphone.resolved_voice}))
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        let wav = hound::WavReader::new(Cursor::new(speech))?;
        ensure!(
            wav.duration() > 0 && wav.spec().bits_per_sample == 16,
            "Invalid Piper speech"
        );
        let mut silence = Cursor::new(Vec::new());
        {
            let mut wav = hound::WavWriter::new(
                &mut silence,
                hound::WavSpec {
                    channels: 1,
                    sample_rate: 16000,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                },
            )?;
            for _ in 0..16000 {
                wav.write_sample(0i16)?;
            }
            wav.finalize()?;
        }
        let form = reqwest::multipart::Form::new()
            .part(
                "file",
                reqwest::multipart::Part::bytes(silence.into_inner())
                    .file_name("silence.wav")
                    .mime_str("audio/wav")?,
            )
            .text("response_format", "json")
            .text("translate", "false");
        let response: serde_json::Value = client
            .post(&local.whisper_endpoint)
            .multipart(form)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        ensure!(
            response.get("text").is_some(),
            "Whisper returned an invalid response"
        );
        ensure!(
            local.whisper_endpoint != local.ollama_endpoint
                && local.ollama_endpoint != local.piper_endpoint,
            "Shared listening port"
        );
        println!(
            "Verified bundled Whisper, Qwen/llama.cpp and Piper; all endpoints use owned dynamic ports."
        );
        drop(runtime_lease);
        tokio::time::timeout(Duration::from_secs(5), async {
            while manager.status().phase != "cached" {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .context("Idle models did not unload")?;
        ensure!(
            manager.status().services.is_empty(),
            "Idle engines still published"
        );
        println!("Verified cache-only selection and automatic idle unload after use.");
        Ok::<_, anyhow::Error>(())
    };
    let result = tokio::time::timeout(Duration::from_secs(1800), operation)
        .await
        .context("Local runtime smoke timed out")?;
    manager.shutdown().await;
    result
}
