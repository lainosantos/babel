use super::*;
use crate::provider::TranscriptMetadata;
use axum::{Json, Router, routing::post};
use serde_json::json;

#[tokio::test]
async fn recoverable_recognition_warning_records_a_gap_without_interrupting_audio() {
    let metrics = RouteMetrics::default();
    metrics.state("transcribing");
    let (tx, mut rx) = mpsc::channel(8);
    let transcript = Some(TranscriptSink {
        sender: tx,
        origin: TranscriptOrigin::Microphone,
    });
    record_recognition_event(
        ProviderEvent::Warning {
            message: "Local transcription skipped old audio to keep up".into(),
        },
        &transcript,
        &metrics,
    )
    .unwrap();
    assert!(matches!(rx.recv().await,
        Some(TranscriptRecord::Routed { origin: TranscriptOrigin::Microphone, record })
        if matches!(*record, TranscriptRecord::Gap)));
    assert_eq!(metrics.snapshot().state, "transcribing");
    assert!(metrics.snapshot().processing_error.is_some());
    assert_eq!(metrics.audio.playback_generation.load(Ordering::Relaxed), 0);
    record_recognition_event(
        ProviderEvent::Transcript {
            input: true,
            text: "The next original segment".into(),
            metadata: TranscriptMetadata::default(),
        },
        &transcript,
        &metrics,
    )
    .unwrap();
    assert!(matches!(rx.recv().await,
        Some(TranscriptRecord::Routed { record, .. })
        if matches!(*record, TranscriptRecord::Text { input: true, .. })));
}

#[tokio::test]
async fn full_or_disconnected_stt_queue_does_not_block_translation_and_vice_versa() {
    let metrics = RouteMetrics::default();
    let (translation, mut translated) = mpsc::channel(1);
    let (recognition, mut recognized) = mpsc::channel(1);
    recognition.try_send(vec![1]).unwrap();
    fanout_original_audio(vec![2], Some(&translation), Some(&recognition), &metrics);
    assert_eq!(translated.recv().await, Some(vec![2]));
    assert_eq!(recognized.recv().await, Some(vec![1]));
    translation.try_send(vec![3]).unwrap();
    fanout_original_audio(vec![4], Some(&translation), Some(&recognition), &metrics);
    assert_eq!(translated.recv().await, Some(vec![3]));
    assert_eq!(recognized.recv().await, Some(vec![4]));
    drop(recognized);
    fanout_original_audio(vec![5], Some(&translation), Some(&recognition), &metrics);
    assert_eq!(translated.recv().await, Some(vec![5]));
    assert_eq!(
        metrics
            .audio
            .processing_dropped_frames
            .load(Ordering::Relaxed),
        3
    );
    assert_eq!(metrics.audio.dropped_frames.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn stt_never_records_generated_text_or_audio() {
    let metrics = RouteMetrics::default();
    let (tx, mut rx) = mpsc::channel(8);
    let transcript = Some(TranscriptSink {
        sender: tx,
        origin: TranscriptOrigin::Speaker,
    });
    record_recognition_event(
        ProviderEvent::Audio {
            samples: vec![99],
            sample_rate: 24000,
        },
        &transcript,
        &metrics,
    )
    .unwrap();
    record_recognition_event(
        ProviderEvent::Transcript {
            input: false,
            text: "translated".into(),
            metadata: TranscriptMetadata::default(),
        },
        &transcript,
        &metrics,
    )
    .unwrap();
    assert!(rx.try_recv().is_err());
    record_recognition_event(
        ProviderEvent::Transcript {
            input: true,
            text: "original".into(),
            metadata: TranscriptMetadata {
                speaker: Some("speaker-2".into()),
                start_ms: Some(100),
                end_ms: Some(500),
                alignment_ms: None,
            },
        },
        &transcript,
        &metrics,
    )
    .unwrap();
    let TranscriptRecord::Routed { origin, record } = rx.recv().await.unwrap() else {
        panic!("missing origin")
    };
    assert_eq!(origin, TranscriptOrigin::Speaker);
    assert!(
        matches!(*record, TranscriptRecord::Text { input: true, ref text, ref metadata, .. } if text == "original" && metadata.speaker.as_deref() == Some("speaker-2") && metadata.start_ms == Some(100))
    );
    record_recognition_event(
        ProviderEvent::Reconnecting { attempt: 1 },
        &transcript,
        &metrics,
    )
    .unwrap();
    assert!(
        matches!(rx.recv().await, Some(TranscriptRecord::Routed { record, .. }) if matches!(*record, TranscriptRecord::Gap))
    );
    assert_eq!(metrics.audio.playback_generation.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn separate_stt_and_translation_receive_original_audio_concurrently() {
    // Distinct recognizers return deliberately different texts. The translator's
    // internal ASR output must not become the dedicated recognizer's transcript.
    let app = Router::new()
        .route(
            "/translation-asr",
            post(|| async { Json(json!({"text":"private translation input"})) }),
        )
        .route(
            "/translate",
            post(|| async {
                Json(json!({"done":true,"message":{"content":"generated translation"}}))
            }),
        )
        .route(
            "/native-speech",
            post(|Json(request): Json<serde_json::Value>| async move {
                assert_eq!(request["text"], "generated translation");
                assert!(request.get("voice").is_none());
                let mut bytes = std::io::Cursor::new(Vec::new());
                let mut writer = hound::WavWriter::new(
                    &mut bytes,
                    hound::WavSpec {
                        channels: 1,
                        sample_rate: 16_000,
                        bits_per_sample: 16,
                        sample_format: hound::SampleFormat::Int,
                    },
                )
                .unwrap();
                for _ in 0..320 {
                    writer.write_sample(1000i16).unwrap();
                }
                writer.finalize().unwrap();
                bytes.into_inner()
            }),
        )
        .route(
            "/dedicated-stt",
            post(
                |headers: axum::http::HeaderMap, body: axum::body::Bytes| async move {
                    assert_eq!(
                        headers.get("authorization").unwrap(),
                        "Bearer independent-test-key"
                    );
                    let form = String::from_utf8_lossy(&body);
                    assert!(form.contains("name=\"translate\"\r\n\r\nfalse"));
                    assert!(form.contains("name=\"language\"\r\n\r\npt"));
                    Json(json!({"text":"original reconhecido pelo STT"}))
                },
            ),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut cfg = AppConfig::default();
    cfg.providers.local.whisper_endpoint = format!("{base}/translation-asr");
    cfg.providers.local.ollama_endpoint = format!("{base}/translate");
    cfg.providers.local.piper_endpoint = format!("{base}/native-speech");
    cfg.providers.local.segment_ms = 500;
    cfg.providers.local.silence_ms = 100;
    cfg.transcription.microphone_recognition.provider = "whisper".into();
    cfg.transcription.microphone_recognition.language = "pt-BR".into();
    cfg.transcription.providers.whisper.endpoint = format!("{base}/dedicated-stt");
    cfg.transcription.providers.whisper.api_key_env = "BABEL_TEST_INDEPENDENT_STT_18641".into();
    cfg.transcription.providers.whisper.segment_ms = 500;
    cfg.transcription.providers.whisper.silence_ms = 100;
    crate::credentials::set(
        "BABEL_TEST_INDEPENDENT_STT_18641",
        "independent-test-key".into(),
    )
    .unwrap();
    let translator =
        provider::create_configured_provider("local", &cfg.providers.gemini, &cfg.providers.local)
            .unwrap();
    let stt = provider::stt::create(
        &cfg.transcription.microphone_recognition,
        &cfg.transcription.providers,
    )
    .unwrap();
    let stt_config = provider::stt::session_config(
        &cfg.transcription.microphone_recognition,
        &cfg.transcription.providers,
    )
    .unwrap();
    let translation_config = SessionConfig {
        model: "unused".into(),
        api_key_env: "BABEL_UNUSED_STS_KEY".into(),
        voice: String::new(),
        source_language: "en".into(),
        target_language: "fr".into(),
        prompt: "translation only".into(),
        vad_silence_ms: 100,
        connect_timeout_secs: 1,
        max_reconnect_attempts: 0,
        input_transcription: false,
        output_transcription: true,
    };
    let cancel = CancellationToken::new();
    let (translation_audio, translation_rx) = mpsc::channel(4);
    let (recognition_audio, recognition_rx) = mpsc::channel(4);
    let (translation_events, mut translation_output) = mpsc::channel(8);
    let (recognition_events, mut recognition_output) = mpsc::channel(8);
    let token = cancel.clone();
    let translation = tokio::spawn(async move {
        translator
            .run(
                translation_config,
                translation_rx,
                translation_events,
                token,
            )
            .await
    });
    let token = cancel.clone();
    let recognition = tokio::spawn(async move {
        stt.run(stt_config, recognition_rx, recognition_events, token)
            .await
    });
    let metrics = RouteMetrics::default();
    tokio::time::timeout(Duration::from_secs(3), async {
        assert_eq!(translation_output.recv().await, Some(ProviderEvent::Connected));
        assert_eq!(recognition_output.recv().await, Some(ProviderEvent::Connected));
        fanout_original_audio(vec![5000; 3200], Some(&translation_audio), Some(&recognition_audio), &metrics);
        fanout_original_audio(vec![0; 1600], Some(&translation_audio), Some(&recognition_audio), &metrics);
        assert!(matches!(translation_output.recv().await, Some(ProviderEvent::Transcript { input: false, ref text, .. }) if text == "generated translation"));
        assert!(matches!(recognition_output.recv().await, Some(ProviderEvent::Transcript { input: true, ref text, .. }) if text == "original reconhecido pelo STT"));
        assert!(matches!(translation_output.recv().await, Some(ProviderEvent::Audio { ref samples, .. }) if !samples.is_empty()));
        assert_eq!(translation_output.recv().await, Some(ProviderEvent::TurnComplete));
        assert_eq!(recognition_output.recv().await, Some(ProviderEvent::TurnComplete));
    }).await.unwrap();
    cancel.cancel();
    translation.await.unwrap().unwrap();
    recognition.await.unwrap().unwrap();
    crate::credentials::clear("BABEL_TEST_INDEPENDENT_STT_18641").unwrap();
    server.abort();
}
