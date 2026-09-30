use super::*;
use crate::provider::TranscriptMetadata;
use futures_util::{FutureExt, future::BoxFuture};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, Notify};

type Handler =
    dyn Fn(String, mpsc::Sender<Vec<i16>>) -> BoxFuture<'static, Result<()>> + Send + Sync;
struct FakeSynthesizer(Arc<Handler>);
#[async_trait]
impl Synthesizer for FakeSynthesizer {
    async fn synthesize(
        &self,
        _: &SynthesisConfig,
        text: &str,
        audio: mpsc::Sender<Vec<i16>>,
        _: CancellationToken,
    ) -> Result<()> {
        (self.0)(text.to_owned(), audio).await
    }
}
struct ScriptProvider(Mutex<Option<mpsc::Receiver<ProviderEvent>>>);
struct ClosingProvider(Mutex<Option<tokio::sync::oneshot::Receiver<Result<()>>>>);
#[async_trait]
impl SpeechProvider for ClosingProvider {
    fn id(&self) -> &'static str {
        "closing-test"
    }
    async fn run(
        &self,
        _: SessionConfig,
        _: mpsc::Receiver<Vec<i16>>,
        events: mpsc::Sender<ProviderEvent>,
        cancel: CancellationToken,
    ) -> Result<()> {
        events.send(ProviderEvent::Connected).await?;
        drop(events);
        let finished = self.0.lock().await.take().unwrap();
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Ok(()),
            result = finished => result?,
        }
    }
}
#[async_trait]
impl SpeechProvider for ScriptProvider {
    fn id(&self) -> &'static str {
        "test"
    }
    async fn run(
        &self,
        config: SessionConfig,
        _: mpsc::Receiver<Vec<i16>>,
        events: mpsc::Sender<ProviderEvent>,
        cancel: CancellationToken,
    ) -> Result<()> {
        assert!(config.output_transcription);
        let mut source = self.0.lock().await.take().unwrap();
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Ok(()),
                event = source.recv() => {
                    let event = event.context("Script closed")?;
                    tokio::select! {
                        _ = cancel.cancelled() => return Ok(()),
                        result = events.send(event) => result?,
                    }
                }
            }
        }
    }
}
fn synthesis_config() -> SynthesisConfig {
    SynthesisConfig {
        provider: "test".into(),
        model: String::new(),
        api_key_env: String::new(),
        voice_id: String::new(),
        style: String::new(),
        language: "pt".into(),
    }
}
fn session_config() -> SessionConfig {
    SessionConfig {
        model: "test".into(),
        api_key_env: String::new(),
        voice: String::new(),
        source_language: "auto".into(),
        target_language: "pt".into(),
        prompt: String::new(),
        vad_silence_ms: 100,
        connect_timeout_secs: 1,
        max_reconnect_attempts: 0,
        input_transcription: true,
        output_transcription: false,
    }
}
fn segment(text: &str, generation: u64) -> Segment {
    Segment {
        text: text.into(),
        generation,
        created: Instant::now(),
    }
}

#[test]
fn sentence_split_preserves_unicode_and_bounds_even_long_punctuated_sentences() {
    let mut text = "Olá, esta é uma frase. Outra frase continua".to_owned();
    assert_eq!(
        take_phrase(&mut text, false).unwrap(),
        "Olá, esta é uma frase."
    );
    assert!(take_phrase(&mut text, false).is_none());
    assert_eq!(
        take_phrase(&mut text, true).unwrap(),
        "Outra frase continua"
    );
    let mut unicode = format!("{}.", "á".repeat(900));
    let mut result = String::new();
    while let Some(phrase) = take_phrase(&mut unicode, true) {
        assert!(phrase.chars().count() <= 240);
        result.push_str(&phrase);
    }
    assert_eq!(result, format!("{}.", "á".repeat(900)));
}

#[test]
fn flush_queues_all_remainders_and_backpressure_is_explicit() {
    let (tx, mut rx) = mpsc::channel(4);
    let mut text = "á".repeat(600);
    let created = Instant::now();
    let mut since = Some(created);
    enqueue_pending(&tx, &mut text, &mut since, 3, true).unwrap();
    assert!(text.is_empty() && since.is_none());
    let mut output = String::new();
    while let Ok(part) = rx.try_recv() {
        assert_eq!(part.generation, 3);
        assert!(part.created >= created);
        output.push_str(&part.text);
    }
    assert_eq!(output, "á".repeat(600));
    let (tx, _rx) = mpsc::channel(1);
    enqueue(&tx, "first".into(), 0, created).unwrap();
    assert!(enqueue(&tx, "second".into(), 0, created).is_err());
}

struct Dropped(Arc<AtomicBool>);
impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test(start_paused = true)]
async fn stop_cancels_request_that_closed_pcm_before_finishing() {
    let started = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let synthesizer = Arc::new(FakeSynthesizer(Arc::new({
        let started = started.clone();
        let dropped = dropped.clone();
        move |_, pcm| {
            let started = started.clone();
            let dropped = dropped.clone();
            async move {
                let _guard = Dropped(dropped);
                drop(pcm);
                started.notify_one();
                std::future::pending::<Result<()>>().await
            }
            .boxed()
        }
    })));
    let (tx, rx) = mpsc::channel(1);
    let (output, _output_rx) = mpsc::channel(1);
    let (_generation_tx, generation) = watch::channel(0);
    let cancel = CancellationToken::new();
    let worker = tokio::spawn(synthesis_worker(
        synthesizer,
        synthesis_config(),
        rx,
        output,
        generation,
        1000,
        cancel.clone(),
    ));
    tx.send(segment("first", 0)).await.unwrap();
    started.notified().await;
    tokio::task::yield_now().await;
    cancel.cancel();
    tokio::time::timeout(Duration::from_millis(100), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        dropped.load(Ordering::SeqCst),
        "request must not remain detached"
    );
}

#[tokio::test(start_paused = true)]
async fn request_error_after_pcm_eof_is_observed() {
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let synthesizer = Arc::new(FakeSynthesizer(Arc::new({
        let started = started.clone();
        let release = release.clone();
        move |_, pcm| {
            let started = started.clone();
            let release = release.clone();
            async move {
                drop(pcm);
                started.notify_one();
                release.notified().await;
                anyhow::bail!("synthetic completion failure")
            }
            .boxed()
        }
    })));
    let (tx, rx) = mpsc::channel(1);
    let (output, _output_rx) = mpsc::channel(1);
    let (_generation_tx, generation) = watch::channel(0);
    let worker = tokio::spawn(synthesis_worker(
        synthesizer,
        synthesis_config(),
        rx,
        output,
        generation,
        1000,
        CancellationToken::new(),
    ));
    tx.send(segment("first", 0)).await.unwrap();
    started.notified().await;
    tokio::task::yield_now().await;
    assert!(!worker.is_finished());
    release.notify_one();
    let error = tokio::time::timeout(Duration::from_millis(100), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("synthetic completion failure"));
}

#[tokio::test(start_paused = true)]
async fn provider_error_after_event_eof_is_observed_by_revoice_supervisor() {
    let (finish, finished) = tokio::sync::oneshot::channel();
    let (_audio, input) = mpsc::channel(1);
    let (events, mut received) = mpsc::channel(2);
    let cancel = CancellationToken::new();
    let worker = tokio::spawn(run_with_synthesizer(
        Arc::new(ClosingProvider(Mutex::new(Some(finished)))),
        session_config(),
        synthesis_config(),
        100,
        1000,
        input,
        events,
        cancel,
        Arc::new(FakeSynthesizer(Arc::new(|_, _| {
            async { anyhow::bail!("synthesis must not be requested") }.boxed()
        }))),
    ));
    assert_eq!(received.recv().await, Some(ProviderEvent::Connected));
    tokio::task::yield_now().await;
    assert!(
        !worker.is_finished(),
        "EOF must not replace the pending provider result"
    );
    finish
        .send(Err(anyhow!("synthetic translation completion failure")))
        .unwrap();
    let error = worker.await.unwrap().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("synthetic translation completion failure")
    );
}

#[tokio::test(start_paused = true)]
async fn revoice_eof_wait_is_bounded_and_cancellable() {
    for stopping in [false, true] {
        let (_finish, finished) = tokio::sync::oneshot::channel();
        let (_audio, input) = mpsc::channel(1);
        let (events, mut received) = mpsc::channel(2);
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(run_with_synthesizer(
            Arc::new(ClosingProvider(Mutex::new(Some(finished)))),
            session_config(),
            synthesis_config(),
            100,
            1000,
            input,
            events,
            cancel.clone(),
            Arc::new(FakeSynthesizer(Arc::new(|_, _| async { Ok(()) }.boxed()))),
        ));
        assert_eq!(received.recv().await, Some(ProviderEvent::Connected));
        tokio::task::yield_now().await;
        let started = Instant::now();
        if stopping {
            cancel.cancel();
            worker.await.unwrap().unwrap();
            assert_eq!(started.elapsed(), Duration::ZERO);
        } else {
            let error = worker.await.unwrap().unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("did not finish within 2 seconds")
            );
            assert!(started.elapsed() <= Duration::from_secs(2));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn completed_request_drains_pcm_and_paces_partial_frames_across_segments() {
    let synthesizer = Arc::new(FakeSynthesizer(Arc::new(move |text, pcm| {
        async move {
            pcm.send(vec![if text == "first" { 1 } else { 2 }; 240])
                .await?;
            Ok(())
        }
        .boxed()
    })));
    let (tx, rx) = mpsc::channel(2);
    let (output, mut output_rx) = mpsc::channel(2);
    let (_generation_tx, generation) = watch::channel(0);
    let cancel = CancellationToken::new();
    let worker = tokio::spawn(synthesis_worker(
        synthesizer,
        synthesis_config(),
        rx,
        output,
        generation,
        1000,
        cancel.clone(),
    ));
    tx.send(segment("first", 0)).await.unwrap();
    tx.send(segment("second", 0)).await.unwrap();
    assert_eq!(output_rx.recv().await.unwrap().samples, vec![1; 240]);
    let first_at = Instant::now();
    assert_eq!(output_rx.recv().await.unwrap().samples, vec![2; 240]);
    assert_eq!(first_at.elapsed(), Duration::from_millis(10));
    cancel.cancel();
    worker.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn interruption_unblocks_full_output_and_discards_old_request() {
    let (started_tx, mut started_rx) = mpsc::channel(4);
    let synthesizer = Arc::new(FakeSynthesizer(Arc::new(move |text, pcm| {
        let started = started_tx.clone();
        async move {
            started.send(text.clone()).await?;
            for _ in 0..4 {
                pcm.send(vec![if text == "old" { 1 } else { 2 }; 480])
                    .await?;
            }
            Ok(())
        }
        .boxed()
    })));
    let (tx, rx) = mpsc::channel(2);
    let (output, mut output_rx) = mpsc::channel(1);
    let (generation_tx, generation) = watch::channel(0);
    let cancel = CancellationToken::new();
    let worker = tokio::spawn(synthesis_worker(
        synthesizer,
        synthesis_config(),
        rx,
        output,
        generation,
        1000,
        cancel.clone(),
    ));
    tx.send(segment("old", 0)).await.unwrap();
    assert_eq!(started_rx.recv().await.unwrap(), "old");
    tokio::time::advance(Duration::from_millis(50)).await;
    tokio::task::yield_now().await;
    generation_tx.send_replace(1);
    tx.send(segment("new", 1)).await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_millis(100), started_rx.recv())
            .await
            .unwrap()
            .unwrap(),
        "new"
    );
    // A frame already delivered before interruption remains tagged as stale.
    assert_eq!(output_rx.recv().await.unwrap().generation, 0);
    let next = output_rx.recv().await.unwrap();
    assert_eq!(next.generation, 1);
    assert_eq!(next.samples, vec![2; 480]);
    cancel.cancel();
    worker.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn continuous_fragments_flush_on_oldest_text_and_only_original_transcripts_escape() {
    let (script_tx, script_rx) = mpsc::channel(8);
    let (calls_tx, mut calls_rx) = mpsc::channel(8);
    let synthesizer = Arc::new(FakeSynthesizer(Arc::new(move |text, pcm| {
        let calls = calls_tx.clone();
        async move {
            calls.send(text).await?;
            pcm.send(vec![42; 480]).await?;
            Ok(())
        }
        .boxed()
    })));
    let (_input_tx, input_rx) = mpsc::channel(1);
    let (events, mut event_rx) = mpsc::channel(16);
    let cancel = CancellationToken::new();
    let task = tokio::spawn(run_with_synthesizer(
        Arc::new(ScriptProvider(Mutex::new(Some(script_rx)))),
        session_config(),
        synthesis_config(),
        100,
        1000,
        input_rx,
        events,
        cancel.clone(),
        synthesizer,
    ));
    script_tx.send(ProviderEvent::Connected).await.unwrap();
    assert_eq!(event_rx.recv().await.unwrap(), ProviderEvent::Connected);
    let warning = ProviderEvent::Warning {
        message: "Synthetic overload recovery".into(),
    };
    script_tx.send(warning).await.unwrap();
    assert_eq!(
        event_rx.recv().await.unwrap(),
        ProviderEvent::Warning {
            message: "Synthetic overload recovery".into()
        }
    );
    script_tx
        .send(ProviderEvent::Transcript {
            input: true,
            text: "original".into(),
            metadata: TranscriptMetadata::default(),
        })
        .await
        .unwrap();
    assert!(matches!(
        event_rx.recv().await.unwrap(),
        ProviderEvent::Transcript { input: true, .. }
    ));
    script_tx
        .send(ProviderEvent::Audio {
            samples: vec![99; 480],
            sample_rate: 24000,
        })
        .await
        .unwrap();
    let initial = Instant::now();
    for text in ["texto", " contínuo", " sem pausa"] {
        script_tx
            .send(ProviderEvent::Transcript {
                input: false,
                text: text.into(),
                metadata: TranscriptMetadata::default(),
            })
            .await
            .unwrap();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(40)).await;
        tokio::task::yield_now().await;
    }
    let translated = tokio::time::timeout(Duration::from_millis(30), calls_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(translated, "texto contínuo sem pausa");
    assert!(
        initial.elapsed() <= Duration::from_millis(150),
        "continuous input must not postpone the flush deadline"
    );
    assert_eq!(
        event_rx.recv().await.unwrap(),
        ProviderEvent::Audio {
            samples: vec![42; 480],
            sample_rate: 24000
        }
    );
    assert!(
        event_rx.try_recv().is_err(),
        "source audio and translated text must not escape"
    );
    cancel.cancel();
    task.await.unwrap().unwrap();
}
