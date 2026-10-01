//! Configured-model sessions and retry boundaries use only loopback Live sockets.
use super::*;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{WebSocketStream, accept_async};
use tokio_util::task::AbortOnDropHandle;

type Server = WebSocketStream<TcpStream>;

fn config() -> SessionConfig {
    SessionConfig {
        model: TRANSCRIBE_MODEL.into(),
        api_key_env: String::new(),
        voice: String::new(),
        source_language: "pt-BR".into(),
        target_language: String::new(),
        prompt: String::new(),
        vad_silence_ms: 100,
        connect_timeout_secs: 2,
        max_reconnect_attempts: 3,
        input_transcription: true,
        output_transcription: false,
    }
}

async fn bind() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    (listener, endpoint)
}

async fn message(socket: &mut Server) -> Value {
    timeout(Duration::from_secs(3), async {
        loop {
            match socket.next().await.unwrap().unwrap() {
                Message::Text(text) => return serde_json::from_str(&text).unwrap(),
                Message::Ping(data) => socket.send(Message::Pong(data)).await.unwrap(),
                other => panic!("unexpected synthetic client frame {other:?}"),
            }
        }
    })
    .await
    .unwrap()
}

async fn accept(listener: &TcpListener, acknowledge: bool) -> Server {
    let (stream, _) = listener.accept().await.unwrap();
    let mut socket = accept_async(stream).await.unwrap();
    let setup = message(&mut socket).await;
    assert_eq!(
        setup["setup"]["model"],
        format!("models/{TRANSCRIBE_MODEL}")
    );
    assert_eq!(
        setup["setup"]["generationConfig"]["responseModalities"],
        json!(["TEXT"])
    );
    assert_eq!(
        setup["setup"]["inputAudioTranscription"]["languageCodes"],
        json!(["pt-BR"])
    );
    assert!(setup["setup"].get("systemInstruction").is_none());
    assert!(setup["setup"].get("outputAudioTranscription").is_none());
    if acknowledge {
        socket
            .send(Message::Text(
                json!({"setupComplete":{}}).to_string().into(),
            ))
            .await
            .unwrap();
    }
    socket
}

async fn turn(socket: &mut Server) -> Vec<i16> {
    assert!(
        message(socket).await["realtimeInput"]
            .get("activityStart")
            .is_some()
    );
    let mut pcm = Vec::new();
    loop {
        let value = message(socket).await;
        let input = &value["realtimeInput"];
        if input.get("activityEnd").is_some() {
            return pcm;
        }
        let bytes = STANDARD
            .decode(input["audio"]["data"].as_str().unwrap())
            .unwrap();
        assert!(bytes.len() <= STREAM_CHUNK_SAMPLES * 2);
        pcm.extend(
            bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| i16::from_le_bytes(*pair)),
        );
    }
}

async fn close(socket: &mut Server, code: u16) {
    socket
        .close(Some(tungstenite::protocol::CloseFrame {
            code: code.into(),
            reason: "synthetic-key private speech must not enter diagnostics".into(),
        }))
        .await
        .unwrap();
}

async fn window(
    endpoint: &str,
    samples: Vec<i16>,
    cancel: CancellationToken,
) -> (Result<()>, Vec<ProviderEvent>) {
    let (input, audio) = mpsc::channel(4);
    let feed = async move {
        input.send(vec![0; 1600]).await.unwrap();
        for chunk in samples.chunks(16000) {
            input.send(chunk.to_vec()).await.unwrap();
        }
        drop(input);
    };
    let (events, mut output) = mpsc::channel(16);
    let collect = async {
        let mut received = Vec::new();
        while let Some(event) = output.recv().await {
            received.push(event);
        }
        received
    };
    let settings = config();
    timeout(WINDOW_FINAL_TIMEOUT + Duration::from_secs(5), async {
        let (result, events, ()) = tokio::join!(
            session(&settings, "synthetic-key", endpoint, audio, events, cancel),
            collect,
            feed
        );
        (result, events)
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn retained_window_with_capture_overflow_has_one_complete_audio_boundary() {
    let (listener, endpoint) = bind().await;
    let mut originals = vec![420; 80000];
    // A 13 ms tail exceeds the engine's five-second collection target. It
    // must stay with the speech window instead of becoming an extra turn.
    originals.extend(vec![1; 208]);
    let expected = originals.clone();
    let server = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut socket = accept(&listener, true).await;
        let received = turn(&mut socket).await;
        assert_eq!(received.len(), expected.len());
        assert!(received == expected, "Original window PCM changed");
        socket.send(Message::Text(json!({"serverContent":{"inputTranscription":{"text":"Complete original window."}}}).to_string().into())).await.unwrap();
    }));
    let (result, events) = window(&endpoint, originals, CancellationToken::new()).await;
    result.unwrap();
    assert_eq!(events.iter().filter(|event| matches!(event, ProviderEvent::Transcript {text, ..} if text == "Complete original window.")).count(), 1);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ProviderEvent::TurnComplete))
            .count(),
        1
    );
    server.await.unwrap();
}

#[tokio::test]
async fn retained_window_accepts_a_final_later_than_the_old_five_second_deadline() {
    let (listener, endpoint) = bind().await;
    let server = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut socket = accept(&listener, true).await;
        assert_eq!(turn(&mut socket).await, vec![420; 1600]);
        tokio::time::sleep(Duration::from_secs(6)).await;
        socket
            .send(Message::Text(
                json!({"serverContent":{"inputTranscription":{"text":"Delayed original speech."}}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
    }));
    let (result, events) = window(&endpoint, vec![420; 1600], CancellationToken::new()).await;
    result.unwrap();
    assert!(events.iter().any(|event| matches!(event, ProviderEvent::Transcript {text, ..} if text == "Delayed original speech.")));
    server.await.unwrap();
}

#[tokio::test]
async fn pauses_and_digital_silence_inside_a_retained_window_do_not_split_it() {
    let (listener, endpoint) = bind().await;
    let (input, audio) = mpsc::channel(4);
    let (events, mut output) = mpsc::channel(16);
    let server = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut socket = accept(&listener, true).await;
        let received = turn(&mut socket).await;
        assert_eq!(received.len(), 6400);
        assert!(received[..1600].iter().all(|sample| *sample == 420));
        assert!(received[1600..4800].iter().all(|sample| *sample == 0));
        assert!(received[4800..].iter().all(|sample| *sample == 840));
        socket.send(Message::Text(json!({"serverContent":{"inputTranscription":{"text":"Speech on both sides of a pause."}}}).to_string().into())).await.unwrap();
    }));
    let feed = async move {
        input.send(vec![420; 1600]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(350)).await;
        input.send(vec![0; 3200]).await.unwrap();
        input.send(vec![840; 1600]).await.unwrap();
    };
    let collect = async {
        let mut finals = 0;
        while let Some(event) = output.recv().await {
            if matches!(event, ProviderEvent::TurnComplete) {
                finals += 1;
            }
        }
        finals
    };
    let settings = config();
    let (result, (), finals) = timeout(Duration::from_secs(3), async {
        tokio::join!(
            session(
                &settings,
                "synthetic-key",
                &endpoint,
                audio,
                events,
                CancellationToken::new()
            ),
            feed,
            collect
        )
    })
    .await
    .unwrap();
    result.unwrap();
    assert_eq!(finals, 1);
    server.await.unwrap();
}

#[tokio::test]
async fn failed_original_window_replays_to_the_same_configured_live_model() {
    let (listener, endpoint) = bind().await;
    let server = AbortOnDropHandle::new(tokio::spawn(async move {
        for attempt in 0..2 {
            let mut socket = accept(&listener, true).await;
            assert_eq!(turn(&mut socket).await, vec![42; 1600]);
            if attempt == 0 {
                socket.send(Message::Text(json!({"serverContent":{"interimInputTranscription":{"text":"Unconfirmed hypothesis"}}}).to_string().into())).await.unwrap();
                close(&mut socket, 1012).await;
            } else {
                socket.send(Message::Text(json!({"serverContent":{"inputTranscription":{"text":"Minha fala original."}}}).to_string().into())).await.unwrap();
            }
        }
    }));
    let (failed, events) = window(&endpoint, vec![42; 1600], CancellationToken::new()).await;
    let error = failed.unwrap_err();
    assert!(crate::provider::retryable_error(&error));
    assert!(error.to_string().contains("1012"));
    assert!(!error.to_string().contains("synthetic-key"));
    assert!(!events.iter().any(|event| matches!(
        event,
        ProviderEvent::Transcript { .. } | ProviderEvent::TurnComplete
    )));
    let (completed, events) = window(&endpoint, vec![42; 1600], CancellationToken::new()).await;
    completed.unwrap();
    assert_eq!(events.iter().filter(|event| matches!(event,
        ProviderEvent::Transcript { input: true, text, metadata } if text == "Minha fala original." && metadata.alignment_ms == Some(100)
    )).count(), 1);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ProviderEvent::TurnComplete))
            .count(),
        1
    );
    server.await.unwrap();
}

#[tokio::test]
async fn rejected_live_setup_returns_its_error_without_an_alternate_model() {
    let (listener, endpoint) = bind().await;
    let server = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut socket = accept(&listener, false).await;
        close(&mut socket, 1008).await;
    }));
    let (result, events) = window(&endpoint, vec![42; 1600], CancellationToken::new()).await;
    let error = result.unwrap_err();
    assert!(!crate::provider::retryable_error(&error));
    assert!(error.to_string().contains("1008"));
    assert!(!error.to_string().contains("private speech"));
    assert!(events.is_empty());
    server.await.unwrap();
}

#[tokio::test]
async fn a_missing_final_is_a_retryable_error_and_never_acknowledges_original_audio() {
    let (listener, endpoint) = bind().await;
    let server = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut socket = accept(&listener, true).await;
        assert_eq!(turn(&mut socket).await, vec![1; 1600]);
        while let Some(Ok(_)) = socket.next().await {}
    }));
    let (result, events) = window(&endpoint, vec![1; 1600], CancellationToken::new()).await;
    let error = result.unwrap_err();
    assert!(crate::provider::retryable_error(&error));
    assert!(
        error
            .to_string()
            .contains("final acknowledgement timed out")
    );
    assert!(!events.iter().any(|event| matches!(
        event,
        ProviderEvent::Transcript { .. } | ProviderEvent::TurnComplete
    )));
    server.await.unwrap();
}

#[tokio::test]
async fn cancellation_returns_incomplete_instead_of_successful_empty_transcription() {
    let (listener, endpoint) = bind().await;
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let server = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut socket = accept(&listener, true).await;
        assert_eq!(turn(&mut socket).await, vec![42; 1600]);
        stop.cancel();
        while let Some(Ok(_)) = socket.next().await {}
    }));
    let (result, events) = window(&endpoint, vec![42; 1600], cancel).await;
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("cancelled before completion")
    );
    assert!(!events.iter().any(|event| matches!(
        event,
        ProviderEvent::Transcript { .. } | ProviderEvent::TurnComplete
    )));
    server.await.unwrap();
}
