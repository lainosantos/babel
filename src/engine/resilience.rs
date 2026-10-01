//! Retry finite work without advancing its original-audio checkpoint. Results
//! stay private until a provider acknowledges the whole segment successfully.
use super::*;

const MAX_RESULT_BYTES: usize = 8 * 1024 * 1024;

pub(super) fn delay(attempt: u32) -> Duration {
    Duration::from_millis((250u64 << attempt.saturating_sub(1).min(5)).min(5000))
}

/// Limit failed attempts, not the duration of successful session finalization.
/// Providers without a reconnect setting still get three transient retries.
pub(super) fn retry(error: &anyhow::Error, attempt: u32, configured: u32) -> Result<()> {
    if !provider::retryable_error(error) || attempt > configured.max(3) {
        bail!(
            "{error:#}. Automatic recovery paused; original audio remains available for explicit recovery"
        );
    }
    Ok(())
}

pub(super) fn retry_live(
    error: &anyhow::Error,
    attempt: u32,
    configured: u32,
    closed: bool,
) -> Result<()> {
    if closed || !provider::retryable_error(error) {
        retry(error, attempt, configured)?;
    }
    Ok(())
}

pub(super) async fn backoff(originals: &retained::RetainedSession, attempt: u32, configured: u32) {
    let closed = originals.status().capture_closed;
    let delay = if !closed && attempt > configured.max(3) {
        Duration::from_secs(30)
    } else {
        delay(attempt)
    };
    tokio::select! {
        _ = tokio::time::sleep(delay) => {},
        _ = originals.wait_capture_closed(), if !closed => {},
    }
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
    let collect_cancel = cancel.clone();
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
        let result = async {
            let mut results = Vec::new();
            let mut bytes = 0usize;
            while let Some(event) = output.recv().await {
                match &event {
                    ProviderEvent::Warning { message } => {
                        bail!("{message}");
                    }
                    ProviderEvent::Interrupted | ProviderEvent::Reconnecting { .. } => {
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
        }
        .await;
        drop(output);
        if result.is_err() {
            collect_cancel.cancel();
        }
        result
    };
    // Let the provider report its sanitized failure instead of racing it with
    // a secondary closed-input-queue error. Failed collection cancels the work.
    let (work, feed, results) = tokio::join!(work, feed, collect);
    let results = results?;
    work?;
    feed?;
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct RejectedInput;
    #[async_trait]
    impl provider::SpeechProvider for RejectedInput {
        fn id(&self) -> &'static str {
            "rejected-input"
        }
        async fn run(
            &self,
            _: SessionConfig,
            _: mpsc::Receiver<Vec<i16>>,
            _: mpsc::Sender<ProviderEvent>,
            _: CancellationToken,
        ) -> Result<()> {
            bail!("Synthetic authentication rejection");
        }
    }

    struct WarningThenCancelled;
    #[async_trait]
    impl provider::SpeechProvider for WarningThenCancelled {
        fn id(&self) -> &'static str {
            "warning-then-cancelled"
        }
        async fn run(
            &self,
            _: SessionConfig,
            _: mpsc::Receiver<Vec<i16>>,
            events: mpsc::Sender<ProviderEvent>,
            cancel: CancellationToken,
        ) -> Result<()> {
            events
                .send(ProviderEvent::Warning {
                    message: "Synthetic actionable processing warning".into(),
                })
                .await?;
            cancel.cancelled().await;
            bail!("Synthetic secondary cancellation");
        }
    }

    #[tokio::test]
    async fn processing_warning_is_not_replaced_by_the_cancellation_it_triggers() {
        let error = segment(
            Arc::new(WarningThenCancelled),
            settings(),
            &[1; 1600],
            false,
        )
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "Synthetic actionable processing warning");
    }

    fn settings() -> SessionConfig {
        SessionConfig {
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
        }
    }

    #[tokio::test]
    async fn provider_failure_takes_precedence_over_secondary_closed_input_error() {
        let error = segment(
            Arc::new(RejectedInput),
            settings(),
            &vec![1; INPUT_RATE as usize * 20],
            false,
        )
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "Synthetic authentication rejection");
    }

    #[test]
    fn automatic_recovery_is_bounded_and_feature_errors_clear_independently() {
        let error = anyhow!("Synthetic transient failure");
        for attempt in 1..=3 {
            assert!(retry(&error, attempt, 0).is_ok());
        }
        assert!(
            retry(&error, 4, 0)
                .unwrap_err()
                .to_string()
                .contains("original audio remains available")
        );
        assert!(retry(&error, 5, 5).is_ok());
        assert!(retry(&error, 6, 5).is_err());
        let metrics = RouteMetrics::default();
        metrics.report_processing_error("Recording still pending");
        metrics.recovery_error("transcription", Some("Recognizer unavailable"));
        metrics.recovery_error("translation", Some("Translator unavailable"));
        metrics.recovery_error("transcription", None);
        let error = metrics.snapshot().processing_error.unwrap();
        assert!(error.contains("Recording still pending"));
        assert!(error.contains("Translator unavailable"));
        assert!(!error.contains("Recognizer unavailable"));
    }

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
