use super::*;
use serde_json::json;
use tokio::net::TcpListener;
use tokio_tungstenite::{
    accept_async, accept_hdr_async,
    tungstenite::http::{Response, StatusCode},
};

fn session() -> SessionConfig {
    SessionConfig {
        model: "must-not-use-translation-model".into(),
        api_key_env: "MUST_NOT_READ_TRANSLATION_KEY".into(),
        voice: "must-not-send-a-voice".into(),
        source_language: "auto".into(),
        target_language: "fr".into(),
        prompt: "must-not-send-a-prompt".into(),
        vad_silence_ms: 300,
        connect_timeout_secs: 1,
        max_reconnect_attempts: 0,
        input_transcription: false,
        output_transcription: true,
    }
}
fn provider(port: u16, reference: &str, attempts: u32) -> DeepgramProvider {
    crate::credentials::set(reference, "synthetic-deepgram-key".into()).unwrap();
    DeepgramProvider::new(DeepgramSttConfig {
        endpoint: format!("ws://127.0.0.1:{port}/v1/listen"),
        api_key_env: reference.into(),
        connect_timeout_secs: 1,
        max_reconnect_attempts: attempts,
        ..Default::default()
    })
    .unwrap()
}
fn final_result(start: f64, text: &str, speech_final: bool) -> Value {
    json!({"type":"Results","channel_index":[0,1],"start":start,"duration":1.0,
        "is_final":true,"speech_final":speech_final,
        "channel":{"alternatives":[{"transcript":text,"words":[]}]}})
}
async fn next_event(events: &mut mpsc::Receiver<ProviderEvent>) -> ProviderEvent {
    timeout(Duration::from_secs(3), events.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn history_preserves_queued_pcm_and_waits_for_close_stream_metadata() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/v1/listen", listener.local_addr().unwrap());
    let (release, released) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        let mut samples = 0;
        loop {
            match socket.next().await.unwrap().unwrap() {
                Message::Binary(bytes) => samples += bytes.len() / 2,
                Message::Text(text) => {
                    assert_eq!(
                        serde_json::from_str::<Value>(&text).unwrap()["type"],
                        "CloseStream"
                    );
                    break;
                }
                other => panic!("unexpected history message {other:?}"),
            }
        }
        assert_eq!(samples, 16321);
        socket
            .send(Message::Text(
                final_result(0.9, "Last original.", false)
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        released.await.unwrap();
        socket
            .send(Message::Text(
                json!({"type":"Metadata","duration":1.0200625,"channels":1})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
    });
    let provider = DeepgramProvider::new(DeepgramSttConfig::default()).unwrap();
    let (tx, mut rx) = mpsc::channel(2);
    tx.send(vec![0; 16000]).await.unwrap();
    tx.send(vec![4000; 321]).await.unwrap();
    drop(tx);
    let (events, mut received) = mpsc::channel(8);
    let worker = tokio::spawn(async move {
        provider
            .connection_mode(&url, "synthetic-key", &mut rx, &events, true)
            .await
    });
    assert_eq!(next_event(&mut received).await, ProviderEvent::Connected);
    assert!(matches!(
        next_event(&mut received).await,
        ProviderEvent::Transcript { input: true, .. }
    ));
    assert!(
        !worker.is_finished(),
        "a final utterance is not end-of-history acknowledgement"
    );
    release.send(()).unwrap();
    timeout(Duration::from_secs(3), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(next_event(&mut received).await, ProviderEvent::TurnComplete);
    server.await.unwrap();
}

#[tokio::test]
async fn history_socket_close_without_summary_is_an_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/v1/listen", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        let value = socket.next().await.unwrap().unwrap().into_text().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&value).unwrap()["type"],
            "CloseStream"
        );
        socket.close(None).await.unwrap();
    });
    let provider = DeepgramProvider::new(DeepgramSttConfig::default()).unwrap();
    let (tx, mut rx) = mpsc::channel(1);
    drop(tx);
    let (events, _received) = mpsc::channel(8);
    assert!(
        provider
            .connection_mode(&url, "synthetic-key", &mut rx, &events, true)
            .await
            .is_err()
    );
    server.await.unwrap();
}

#[tokio::test]
async fn history_missing_completion_times_out_as_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/v1/listen", listener.local_addr().unwrap());
    let (flushed, flushed_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        socket.next().await.unwrap().unwrap(); // CloseStream for empty input.
        flushed.send(()).unwrap();
        std::future::pending::<()>().await;
        drop(socket);
    });
    let provider = DeepgramProvider::new(DeepgramSttConfig::default()).unwrap();
    let (tx, mut rx) = mpsc::channel(1);
    drop(tx);
    let (events, _received) = mpsc::channel(8);
    let worker = tokio::spawn(async move {
        provider
            .connection_mode(&url, "synthetic-key", &mut rx, &events, true)
            .await
    });
    flushed_rx.await.unwrap();
    tokio::task::yield_now().await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(121)).await;
    let error = worker.await.unwrap().unwrap_err();
    assert!(error.message.contains("acknowledgement timed out"));
    server.abort();
}

#[test]
fn endpoint_and_language_configuration_are_explicit_and_do_not_leak_settings() {
    for invalid in [
        "http://api.deepgram.com/v1/listen",
        "ws://remote.example/v1/listen",
        "wss://key@api.deepgram.com/v1/listen",
        "wss://api.deepgram.com/v1/listen?token=secret",
        "wss://api.deepgram.com/v1/listen#fragment",
        "wss://api.deepgram.com/v2/listen",
    ] {
        let config = DeepgramSttConfig {
            endpoint: invalid.into(),
            ..Default::default()
        };
        assert!(DeepgramProvider::new(config).is_err());
    }
    let provider = DeepgramProvider::new(DeepgramSttConfig::default()).unwrap();
    let url = Url::parse(&provider.url("auto").unwrap()).unwrap();
    let query: std::collections::BTreeMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["language"], "multi");
    assert_eq!(query["diarize_model"], "v1");
    assert!(!query.contains_key("diarize"));
    assert!(provider.url("Português brasileiro").is_err());
    assert!(provider.url("pt-BR").unwrap().contains("language=pt-BR"));
    let config = DeepgramSttConfig {
        diarize: false,
        ..Default::default()
    };
    assert!(
        !DeepgramProvider::new(config)
            .unwrap()
            .url("en")
            .unwrap()
            .contains("diarize")
    );
    let config = DeepgramSttConfig {
        model: "flux-general-en".into(),
        ..Default::default()
    };
    assert!(DeepgramProvider::new(config).is_err());
    let config = DeepgramSttConfig {
        model: "nova-2-phonecall".into(),
        ..Default::default()
    };
    assert!(DeepgramProvider::new(config).unwrap().url("auto").is_err());
}

#[test]
fn final_words_keep_actual_speaker_ids_punctuation_and_timestamps() {
    let mut message = final_result(12.0, "Hello there. Olá!", true);
    message["channel"]["alternatives"][0]["words"] = json!([
        {"word":"hello","punctuated_word":"Hello","start":12.15,"end":12.35,"speaker":0},
        {"word":"there","punctuated_word":"there.","start":12.4,"end":12.55,"speaker":0},
        {"word":"olá","punctuated_word":"Olá!","start":12.7,"end":12.9,"speaker":2}
    ]);
    let mut finals = Finals::default();
    assert_eq!(
        finals.decode(&message, true).unwrap(),
        vec![
            transcript("Hello there.".into(), Some("0".into()), 12150, 12550),
            transcript("Olá!".into(), Some("2".into()), 12700, 12900),
            ProviderEvent::TurnComplete
        ]
    );
    assert!(finals.decode(&message, true).unwrap().is_empty());
    assert_eq!(
        Finals::default().decode(&message, false).unwrap(),
        vec![
            transcript("Hello there. Olá!".into(), None, 12150, 12900),
            ProviderEvent::TurnComplete
        ]
    );
}

#[test]
fn interim_results_and_repeated_speech_final_do_not_duplicate_original_text() {
    let mut message = final_result(0.0, "original", false);
    let mut finals = Finals::default();
    message["is_final"] = json!(false);
    assert!(finals.decode(&message, true).unwrap().is_empty());
    message["is_final"] = json!(true);
    assert_eq!(
        finals.decode(&message, true).unwrap(),
        vec![transcript("original".into(), None, 0, 1000)]
    );
    message["speech_final"] = json!(true);
    assert_eq!(
        finals.decode(&message, true).unwrap(),
        vec![ProviderEvent::TurnComplete]
    );
    assert!(finals.decode(&message, true).unwrap().is_empty());
    for index in 1..100 {
        finals
            .decode(&final_result(index as f64, "next", true), true)
            .unwrap();
    }
    assert_eq!(finals.recent.len(), MAX_FINALS);
}

#[test]
fn absent_speaker_metadata_preserves_original_text_without_inventing_names() {
    let mut message = final_result(1.0, "你好，世界。", true);
    message["channel"]["alternatives"][0]["words"] = json!([
        {"word":"你好","start":1.1,"end":1.4}, {"word":"世界","start":1.5,"end":1.9}
    ]);
    assert_eq!(
        Finals::default().decode(&message, true).unwrap(),
        vec![
            transcript("你好，世界。".into(), None, 1100, 1900),
            ProviderEvent::TurnComplete
        ]
    );
}

#[test]
fn parser_bounds_and_error_messages_never_include_provider_payloads() {
    assert!(parse(&vec![b' '; MAX_MESSAGE + 1]).is_err());
    assert!(parse(b"[]").is_err());
    assert!(parse(b"not-json").is_err());
    let error = parse(br#"{"type":"Error","variant":"AUTH","description":"synthetic-secret"}"#)
        .unwrap_err();
    assert!(!error.retryable && !error.message.contains("synthetic-secret"));
    for bad in [json!(-1.0), json!("1"), json!(700000.0), Value::Null] {
        let mut message = final_result(0.0, "text", true);
        message["start"] = bad;
        assert!(Finals::default().decode(&message, true).is_err());
    }
    let mut message = final_result(0.0, "text", true);
    message["channel"]["alternatives"][0]["words"] =
        json!([{"word":"x","start":0.8,"end":0.2,"speaker":1}]);
    assert!(Finals::default().decode(&message, true).is_err());
    message["channel"]["alternatives"][0]["words"] = json!(vec![json!({}); MAX_WORDS + 1]);
    assert!(Finals::default().decode(&message, true).is_err());
    assert!(
        Finals::default()
            .decode(&final_result(0.0, &"x".repeat(MAX_TEXT + 1), true), true)
            .is_err()
    );
}

#[tokio::test]
#[allow(clippy::result_large_err)] // Tungstenite fixes the handshake callback error type.
async fn websocket_auth_query_pcm_and_final_only_contract_work_without_setup_ack() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider = provider(
        listener.local_addr().unwrap().port(),
        "BABEL_TEST_DEEPGRAM_WIRE",
        0,
    );
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket = accept_hdr_async(
            tcp,
            |request: &tungstenite::handshake::server::Request, response| {
                assert_eq!(
                    request.headers()["Authorization"],
                    "Token synthetic-deepgram-key"
                );
                let url = Url::parse(&format!("http://local{}", request.uri())).unwrap();
                let query: std::collections::BTreeMap<_, _> =
                    url.query_pairs().into_owned().collect();
                assert_eq!(query.len(), 8);
                for (key, value) in [
                    ("model", "nova-3"),
                    ("language", "multi"),
                    ("encoding", "linear16"),
                    ("sample_rate", "16000"),
                    ("channels", "1"),
                    ("diarize_model", "v1"),
                    ("punctuate", "true"),
                    ("interim_results", "false"),
                ] {
                    assert_eq!(query[key], value);
                }
                Ok(response)
            },
        )
        .await
        .unwrap();
        assert_eq!(
            socket.next().await.unwrap().unwrap(),
            Message::Binary(vec![0, 128, 255, 255, 0, 0, 255, 127].into())
        );
        let mut interim = final_result(0.0, "wrong partial", false);
        interim["is_final"] = json!(false);
        socket
            .send(Message::Text(interim.to_string().into()))
            .await
            .unwrap();
        let original = final_result(0.0, "Original português.", true).to_string();
        socket
            .send(Message::Text(original.clone().into()))
            .await
            .unwrap();
        socket.send(Message::Text(original.into())).await.unwrap();
        while socket.next().await.is_some_and(|message| message.is_ok()) {}
    });
    let (audio_tx, audio_rx) = mpsc::channel(2);
    let (events_tx, mut events_rx) = mpsc::channel(8);
    let cancel = CancellationToken::new();
    let worker = tokio::spawn({
        let token = cancel.clone();
        async move { provider.run(session(), audio_rx, events_tx, token).await }
    });
    assert_eq!(next_event(&mut events_rx).await, ProviderEvent::Connected);
    audio_tx
        .send(vec![i16::MIN, -1, 0, i16::MAX])
        .await
        .unwrap();
    assert_eq!(
        next_event(&mut events_rx).await,
        transcript("Original português.".into(), None, 0, 1000)
    );
    assert_eq!(
        next_event(&mut events_rx).await,
        ProviderEvent::TurnComplete
    );
    assert!(
        timeout(Duration::from_millis(80), events_rx.recv())
            .await
            .is_err()
    );
    cancel.cancel();
    timeout(Duration::from_secs(2), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    server.await.unwrap();
    crate::credentials::clear("BABEL_TEST_DEEPGRAM_WIRE").unwrap();
}

#[tokio::test]
#[allow(clippy::result_large_err)] // Tungstenite fixes the handshake callback error type.
async fn authentication_errors_are_fatal_and_do_not_retry_or_expose_response_body() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider = provider(
        listener.local_addr().unwrap().port(),
        "BABEL_TEST_DEEPGRAM_AUTH",
        3,
    );
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let _ = accept_hdr_async(
            tcp,
            |_request: &tungstenite::handshake::server::Request, _response| {
                Err(Response::builder()
                    .status(StatusCode::UNAUTHORIZED)
                    .body(Some("synthetic-secret-response".into()))
                    .unwrap())
            },
        )
        .await;
        assert!(
            timeout(Duration::from_millis(400), listener.accept())
                .await
                .is_err()
        );
    });
    let (_audio_tx, audio_rx) = mpsc::channel(1);
    let (events_tx, mut events_rx) = mpsc::channel(1);
    let error = provider
        .run(session(), audio_rx, events_tx, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("authentication rejected"));
    assert!(!format!("{error:?}").contains("synthetic-secret-response"));
    assert!(events_rx.recv().await.is_none());
    server.await.unwrap();
    crate::credentials::clear("BABEL_TEST_DEEPGRAM_AUTH").unwrap();
}

#[tokio::test]
async fn transient_closures_obey_budget_and_oversized_audio_is_not_retried() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider = provider(
        listener.local_addr().unwrap().port(),
        "BABEL_TEST_DEEPGRAM_RETRY",
        1,
    );
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(tcp).await.unwrap();
            socket.close(None).await.unwrap();
        }
    });
    let (_audio_tx, audio_rx) = mpsc::channel(1);
    let (events_tx, mut events_rx) = mpsc::channel(8);
    let error = provider
        .run(session(), audio_rx, events_tx, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("reconnect budget exhausted"));
    assert_eq!(next_event(&mut events_rx).await, ProviderEvent::Connected);
    assert_eq!(
        next_event(&mut events_rx).await,
        ProviderEvent::Reconnecting { attempt: 1 }
    );
    assert_eq!(next_event(&mut events_rx).await, ProviderEvent::Connected);
    assert!(events_rx.recv().await.is_none());
    server.await.unwrap();
    crate::credentials::clear("BABEL_TEST_DEEPGRAM_RETRY").unwrap();
}

#[tokio::test]
async fn oversized_audio_and_binary_output_fail_within_bounds() {
    for (suffix, server_binary) in [("INPUT", false), ("OUTPUT", true)] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let reference = format!("BABEL_TEST_DEEPGRAM_LIMIT_{suffix}");
        let provider = provider(listener.local_addr().unwrap().port(), &reference, 0);
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(tcp).await.unwrap();
            if server_binary {
                socket
                    .send(Message::Binary(vec![0u8; 16].into()))
                    .await
                    .unwrap();
            }
            while socket.next().await.is_some_and(|message| message.is_ok()) {}
        });
        let (audio_tx, audio_rx) = mpsc::channel(1);
        let (events_tx, mut events_rx) = mpsc::channel(8);
        let worker = tokio::spawn(async move {
            provider
                .run(session(), audio_rx, events_tx, CancellationToken::new())
                .await
        });
        assert_eq!(next_event(&mut events_rx).await, ProviderEvent::Connected);
        if !server_binary {
            audio_tx.send(vec![0; 16001]).await.unwrap();
        }
        let error = timeout(Duration::from_secs(2), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains(if server_binary {
            "binary content"
        } else {
            "exceeds one second"
        }));
        server.await.unwrap();
        crate::credentials::clear(&reference).unwrap();
    }
}

#[tokio::test]
async fn silence_uses_text_keepalive_and_cancellation_works_with_full_event_queue() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider = provider(
        listener.local_addr().unwrap().port(),
        "BABEL_TEST_DEEPGRAM_IDLE",
        0,
    );
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(tcp).await.unwrap();
        assert_eq!(
            timeout(Duration::from_secs(5), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            Message::Text(r#"{"type":"KeepAlive"}"#.into())
        );
        socket
            .send(Message::Text(
                final_result(0.0, "first", true).to_string().into(),
            ))
            .await
            .unwrap();
        socket
            .send(Message::Text(
                final_result(1.0, "blocked", true).to_string().into(),
            ))
            .await
            .unwrap();
        while socket.next().await.is_some_and(|message| message.is_ok()) {}
    });
    let (_audio_tx, audio_rx) = mpsc::channel(1);
    let (events_tx, mut events_rx) = mpsc::channel(1);
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let worker =
        tokio::spawn(async move { provider.run(session(), audio_rx, events_tx, token).await });
    assert_eq!(next_event(&mut events_rx).await, ProviderEvent::Connected);
    assert_eq!(
        timeout(Duration::from_secs(5), events_rx.recv())
            .await
            .unwrap()
            .unwrap(),
        transcript("first".into(), None, 0, 1000)
    );
    // TurnComplete occupies the only slot; cancellation must interrupt the
    // pending second transcript instead of waiting for its slow-consumer limit.
    cancel.cancel();
    timeout(Duration::from_secs(1), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    server.await.unwrap();
    crate::credentials::clear("BABEL_TEST_DEEPGRAM_IDLE").unwrap();
}

#[tokio::test]
async fn reconnect_discards_old_audio_before_accepting_new_frames() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider = provider(
        listener.local_addr().unwrap().port(),
        "BABEL_TEST_DEEPGRAM_BACKLOG",
        1,
    );
    let (received_tx, received_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut first = accept_async(tcp).await.unwrap();
        first.close(None).await.unwrap();
        drop(first);
        let (tcp, _) = listener.accept().await.unwrap();
        let mut second = accept_async(tcp).await.unwrap();
        let message = timeout(Duration::from_secs(3), second.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        received_tx.send(message).unwrap();
        while second.next().await.is_some_and(|message| message.is_ok()) {}
    });
    let (audio_tx, audio_rx) = mpsc::channel(2);
    let (events_tx, mut events_rx) = mpsc::channel(4);
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let worker =
        tokio::spawn(async move { provider.run(session(), audio_rx, events_tx, token).await });
    assert_eq!(next_event(&mut events_rx).await, ProviderEvent::Connected);
    assert_eq!(
        next_event(&mut events_rx).await,
        ProviderEvent::Reconnecting { attempt: 1 }
    );
    audio_tx.send(vec![12345; 100]).await.unwrap();
    assert_eq!(next_event(&mut events_rx).await, ProviderEvent::Connected);
    audio_tx.send(vec![12, -34]).await.unwrap();
    assert_eq!(
        received_rx.await.unwrap(),
        Message::Binary(vec![12, 0, 222, 255].into())
    );
    cancel.cancel();
    worker.await.unwrap().unwrap();
    server.await.unwrap();
    crate::credentials::clear("BABEL_TEST_DEEPGRAM_BACKLOG").unwrap();
}

#[test]
fn a_repeated_old_boundary_does_not_end_the_current_utterance() {
    let mut finals = Finals::default();
    let old = final_result(0.0, "first", true);
    assert_eq!(finals.decode(&old, true).unwrap().len(), 2);
    let mut current = final_result(1.0, "second", false);
    assert_eq!(finals.decode(&current, true).unwrap().len(), 1);
    assert!(finals.decode(&old, true).unwrap().is_empty());
    current["speech_final"] = json!(true);
    assert_eq!(
        finals.decode(&current, true).unwrap(),
        vec![ProviderEvent::TurnComplete]
    );
}
