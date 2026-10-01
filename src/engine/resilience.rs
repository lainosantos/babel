//! Retry finite work without advancing its original-audio checkpoint. Results
//! stay private until a provider acknowledges the whole segment successfully.
use super::*;

const MAX_RESULT_BYTES: usize = 8 * 1024 * 1024;

pub(super) fn delay(attempt: u32) -> Duration {
    Duration::from_millis((250u64 << attempt.saturating_sub(1).min(5)).min(5000))
}

pub(super) async fn segment(
    provider: Arc<dyn provider::SpeechProvider>,
    settings: SessionConfig,
    samples: &[i16],
    historical: bool,
) -> Result<Vec<ProviderEvent>> {
    let (input, audio) = mpsc::channel(8);
    let (events, mut output) = mpsc::channel(16);
    let cancel = CancellationToken::new();
    let _guard = cancel.clone().drop_guard();
    let work = async {
        if historical {
            provider.run_history(settings, audio, events, cancel).await
        } else {
            provider.run(settings, audio, events, cancel).await
        }
    };
    let feed = async {
        for chunk in samples.chunks(INPUT_RATE as usize) {
            input
                .send(chunk.to_vec())
                .await
                .context("Finite processor closed before consuming originals")?;
        }
        drop(input);
        Ok::<_, anyhow::Error>(())
    };
    let collect = async {
        let mut results = Vec::new();
        let mut bytes = 0usize;
        while let Some(event) = output.recv().await {
            match &event {
                ProviderEvent::Interrupted
                | ProviderEvent::Reconnecting { .. }
                | ProviderEvent::Warning { .. } => {
                    bail!("Finite processing was interrupted before acknowledgement");
                }
                ProviderEvent::Transcript { text, .. } => {
                    bytes = bytes.saturating_add(text.len() + 256)
                }
                ProviderEvent::Audio { samples, .. } => {
                    bytes = bytes.saturating_add(samples.len() * 2 + 128)
                }
                _ => bytes = bytes.saturating_add(128),
            }
            ensure!(
                bytes <= MAX_RESULT_BYTES,
                "Finite processing result exceeds the memory limit"
            );
            results.push(event);
        }
        Ok::<_, anyhow::Error>(results)
    };
    let (_, _, results) = tokio::try_join!(work, feed, collect)?;
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    #[derive(Default)]
    struct PartialFailure {
        calls: AtomicU64,
        inputs: StdMutex<Vec<Vec<i16>>>,
    }
    #[async_trait]
    impl provider::SpeechProvider for PartialFailure {
        fn id(&self) -> &'static str {
            "partial-failure"
        }
        async fn run(
            &self,
            _: SessionConfig,
            mut input: mpsc::Receiver<Vec<i16>>,
            events: mpsc::Sender<ProviderEvent>,
            _: CancellationToken,
        ) -> Result<()> {
            let mut pcm = Vec::new();
            while let Some(samples) = input.recv().await {
                pcm.extend(samples);
            }
            self.inputs.lock().unwrap().push(pcm);
            let failed = self.calls.fetch_add(1, Ordering::Relaxed) == 0;
            events
                .send(ProviderEvent::Transcript {
                    input: true,
                    text: if failed {
                        "Unconfirmed partial text"
                    } else {
                        "Confirmed text"
                    }
                    .into(),
                    metadata: Default::default(),
                })
                .await?;
            events
                .send(ProviderEvent::Audio {
                    samples: vec![500; 2400],
                    sample_rate: OUTPUT_RATE,
                })
                .await?;
            if failed {
                bail!("Synthetic failure after partial output");
            }
            events.send(ProviderEvent::TurnComplete).await?;
            Ok(())
        }
    }
    #[tokio::test]
    async fn partial_provider_output_is_discarded_and_the_same_pcm_can_be_retried() {
        let processor = Arc::new(PartialFailure::default());
        let settings = SessionConfig {
            model: String::new(),
            api_key_env: String::new(),
            voice: String::new(),
            source_language: "auto".into(),
            target_language: "en".into(),
            prompt: String::new(),
            vad_silence_ms: 400,
            connect_timeout_secs: 1,
            max_reconnect_attempts: 0,
            input_transcription: true,
            output_transcription: false,
        };
        let pcm = vec![123; INPUT_RATE as usize * 6];
        assert!(
            segment(processor.clone(), settings.clone(), &pcm, false)
                .await
                .is_err()
        );
        let confirmed = segment(processor.clone(), settings, &pcm, false)
            .await
            .unwrap();
        assert!(
            matches!(&confirmed[0], ProviderEvent::Transcript { text, .. } if text == "Confirmed text")
        );
        assert!(matches!(
            confirmed.last(),
            Some(ProviderEvent::TurnComplete)
        ));
        assert_eq!(*processor.inputs.lock().unwrap(), vec![pcm.clone(), pcm]);
    }
}
