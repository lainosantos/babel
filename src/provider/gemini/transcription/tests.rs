use super::*;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{WebSocketStream, accept_async};

type Server = WebSocketStream<TcpStream>;

fn config() -> SessionConfig {
    SessionConfig {
        model: TRANSCRIBE_MODEL.into(),
        api_key_env: String::new(),
        voice: String::new(),
        source_language: "auto".into(),
        target_language: String::new(),
        prompt: String::new(),
        vad_silence_ms: 100,
        connect_timeout_secs: 2,
        max_reconnect_attempts: 2,
        input_transcription: true,
        output_transcription: false,
    }
}

async fn bind() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    (listener, endpoint)
}

async fn accept(listener: &TcpListener) -> Server {
    let (stream, _) = listener.accept().await.unwrap();
    let mut socket = accept_async(stream).await.unwrap();
    let setup = message(&mut socket).await;
    assert_eq!(
        setup["setup"]["realtimeInputConfig"]["automaticActivityDetection"]["disabled"],
        true
    );
    assert_eq!(
        setup["setup"]["generationConfig"]["responseModalities"],
        json!(["TEXT"])
    );
    socket
        .send(Message::Text(
            json!({"setupComplete":{}}).to_string().into(),
        ))
        .await
        .unwrap();
    socket
}

async fn message(socket: &mut Server) -> Value {
    timeout(Duration::from_secs(3), async {
        loop {
            match socket.next().await.unwrap().unwrap() {
                Message::Text(text) => return serde_json::from_str(&text).unwrap(),
                Message::Ping(data) => socket.send(Message::Pong(data)).await.unwrap(),
                other => panic!("unexpected client frame {other:?}"),
            }
        }
    })
    .await
    .unwrap()
}

async fn turn(socket: &mut Server) -> Vec<i16> {
    assert!(
        message(socket).await["realtimeInput"]
            .get("activityStart")
            .is_some()
    );
    let mut samples = Vec::new();
    loop {
        let input = message(socket).await;
        let input = &input["realtimeInput"];
        if input.get("activityEnd").is_some() {
            return samples;
        }
        let bytes = STANDARD
            .decode(input["audio"]["data"].as_str().unwrap())
            .unwrap();
        samples.extend(
            bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]])),
        );
    }
}

async fn final_text(socket: &mut Server, text: &str) {
    socket
        .send(Message::Text(
            json!({"serverContent":{"inputTranscription":{"text":text}}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
}

async fn wait_closed(socket: &mut Server) {
    timeout(Duration::from_secs(3), async {
        while let Some(Ok(_)) = socket.next().await {}
    })
    .await
    .unwrap();
}

fn spawn(
    endpoint: String,
) -> (
    tokio::task::JoinHandle<Result<()>>,
    mpsc::Sender<Vec<i16>>,
    mpsc::Receiver<ProviderEvent>,
) {
    let (input, audio) = mpsc::channel(16);
    let (events, received) = mpsc::channel(32);
    let worker = tokio::spawn(async move {
        run_sessions(
            &config(),
            "synthetic-key",
            &endpoint,
            audio,
            events,
            CancellationToken::new(),
        )
        .await
    });
    (worker, input, received)
}

async fn event(events: &mut mpsc::Receiver<ProviderEvent>) -> ProviderEvent {
    timeout(Duration::from_secs(3), events.recv())
        .await
        .unwrap()
        .unwrap()
}

async fn success(worker: tokio::task::JoinHandle<Result<()>>) {
    timeout(Duration::from_secs(3), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn startup_audio_and_eof_wait_for_delayed_final_without_saving_interim() {
    let (listener, endpoint) = bind().await;
    let (ready, release) = tokio::sync::oneshot::channel();
    let (flushed, flush) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        assert_eq!(turn(&mut socket).await, vec![1; 1600]);
        socket
            .send(Message::Text(
                json!({"serverContent":{"interimInputTranscription":{"text":"uncommitted"}}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        flushed.send(()).unwrap();
        release.await.unwrap();
        final_text(&mut socket, "Quiet original.").await;
        wait_closed(&mut socket).await;
    });
    let (worker, input, mut events) = spawn(endpoint);
    // Queued before Connected; exact-zero prefix must remain in source offsets.
    input.send(vec![0; 1600]).await.unwrap();
    input.send(vec![1; 1600]).await.unwrap();
    drop(input);
    assert_eq!(event(&mut events).await, ProviderEvent::Connected);
    flush.await.unwrap();
    assert!(!worker.is_finished());
    assert!(events.try_recv().is_err());
    ready.send(()).unwrap();
    assert_eq!(
        event(&mut events).await,
        ProviderEvent::Transcript {
            input: true,
            text: "Quiet original.".into(),
            metadata: TranscriptMetadata {
                alignment_ms: Some(100),
                ..Default::default()
            },
        }
    );
    assert_eq!(event(&mut events).await, ProviderEvent::TurnComplete);
    success(worker).await;
    server.await.unwrap();
}

#[tokio::test]
async fn paused_input_finalizes_and_resumes_on_the_same_socket() {
    let (listener, endpoint) = bind().await;
    let (pinged, pong) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        assert_eq!(turn(&mut socket).await, vec![120; 1600]);
        final_text(&mut socket, "First.").await;
        socket.send(Message::Ping(vec![9].into())).await.unwrap();
        let reply = timeout(Duration::from_secs(3), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(reply, Message::Pong(data) if data.as_ref() == [9]));
        pinged.send(()).unwrap();
        assert_eq!(turn(&mut socket).await, vec![90; 1600]);
        final_text(&mut socket, "Second.").await;
        wait_closed(&mut socket).await;
    });
    let (worker, input, mut events) = spawn(endpoint);
    assert_eq!(event(&mut events).await, ProviderEvent::Connected);
    input.send(vec![120; 1600]).await.unwrap();
    assert!(
        matches!(event(&mut events).await, ProviderEvent::Transcript { text, .. } if text == "First.")
    );
    assert_eq!(event(&mut events).await, ProviderEvent::TurnComplete);
    pong.await.unwrap();
    assert!(!worker.is_finished());
    input.send(vec![90; 1600]).await.unwrap();
    drop(input);
    assert!(
        matches!(event(&mut events).await, ProviderEvent::Transcript { text, metadata, .. } if text == "Second." && metadata.alignment_ms == Some(100))
    );
    assert_eq!(event(&mut events).await, ProviderEvent::TurnComplete);
    success(worker).await;
    server.await.unwrap();
}

#[tokio::test]
async fn continuous_audio_has_bounded_turns_and_each_sample_is_sent_once() {
    let (listener, endpoint) = bind().await;
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        let first = turn(&mut socket).await;
        assert_eq!(first, vec![2; MAX_TURN_SAMPLES]);
        final_text(&mut socket, "Continuous one.").await;
        assert_eq!(turn(&mut socket).await, vec![2; 16000]);
        final_text(&mut socket, "Continuous two.").await;
        wait_closed(&mut socket).await;
    });
    let (worker, input, mut events) = spawn(endpoint);
    for _ in 0..6 {
        input.send(vec![2; 16000]).await.unwrap();
    }
    drop(input);
    assert_eq!(event(&mut events).await, ProviderEvent::Connected);
    for alignment in [0, 5000] {
        assert!(
            matches!(event(&mut events).await, ProviderEvent::Transcript { metadata, .. } if metadata.alignment_ms == Some(alignment))
        );
        assert_eq!(event(&mut events).await, ProviderEvent::TurnComplete);
    }
    success(worker).await;
    server.await.unwrap();
}

#[tokio::test]
async fn empty_or_exact_silence_eof_finishes_without_an_unacknowledged_turn() {
    for silent in [false, true] {
        let (listener, endpoint) = bind().await;
        let server = tokio::spawn(async move {
            let mut socket = accept(&listener).await;
            timeout(Duration::from_secs(3), async {
                while let Some(Ok(message)) = socket.next().await {
                    assert!(
                        !matches!(message, Message::Text(_)),
                        "zero-only input opened an activity"
                    );
                }
            })
            .await
            .unwrap();
        });
        let (worker, input, mut events) = spawn(endpoint);
        if silent {
            input.send(vec![0; 1600]).await.unwrap();
        }
        drop(input);
        assert_eq!(event(&mut events).await, ProviderEvent::Connected);
        success(worker).await;
        server.await.unwrap();
        assert!(events.recv().await.is_none());
    }
}

#[tokio::test]
async fn missing_final_is_an_error_after_five_seconds_not_success_or_retry() {
    let (listener, endpoint) = bind().await;
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        assert_eq!(turn(&mut socket).await, vec![30; 1600]);
        timeout(Duration::from_secs(7), async {
            while let Some(Ok(_)) = socket.next().await {}
        })
        .await
        .unwrap();
    });
    let (worker, input, mut events) = spawn(endpoint);
    input.send(vec![30; 1600]).await.unwrap();
    drop(input);
    assert_eq!(event(&mut events).await, ProviderEvent::Connected);
    let error = timeout(Duration::from_secs(7), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("final acknowledgement timed out")
    );
    assert!(events.recv().await.is_none());
    server.await.unwrap();
}

#[tokio::test]
async fn source_closing_during_backoff_cannot_succeed_on_an_empty_replacement() {
    let (listener, endpoint) = bind().await;
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        socket
            .send(Message::Close(Some(tungstenite::protocol::CloseFrame {
                code: tungstenite::protocol::frame::coding::CloseCode::Restart,
                reason: "synthetic restart".into(),
            })))
            .await
            .unwrap();
        // An incorrect reconnect would consume the discarded source as empty
        // EOF and return success. Keep such a replacement available for the test.
        let mut replacement = accept(&listener).await;
        wait_closed(&mut replacement).await;
    });
    let (worker, input, mut events) = spawn(endpoint);
    assert_eq!(event(&mut events).await, ProviderEvent::Connected);
    assert_eq!(event(&mut events).await, ProviderEvent::Interrupted);
    assert_eq!(
        event(&mut events).await,
        ProviderEvent::Reconnecting { attempt: 1 }
    );
    input.send(vec![37; 1600]).await.unwrap();
    drop(input);
    let result = timeout(Duration::from_secs(3), worker).await;
    server.abort();
    let _ = server.await;
    let error = result.unwrap().unwrap().unwrap_err();
    assert!(error.to_string().contains("code 1012"));
}

#[tokio::test(start_paused = true)]
async fn final_event_delivery_cannot_extend_the_acknowledgement_deadline() {
    let (events, mut received) = mpsc::channel(1);
    let started = Instant::now();
    let deadline = started + Duration::from_millis(500);
    before_final_deadline(
        Some(deadline),
        emit(
            &events,
            ProviderEvent::Transcript {
                input: true,
                text: "Synthetic original.".into(),
                metadata: TranscriptMetadata::default(),
            },
        ),
    )
    .await
    .unwrap();
    let error = before_final_deadline(Some(deadline), emit(&events, ProviderEvent::TurnComplete))
        .await
        .unwrap_err();
    assert!(error.message.contains("final acknowledgement timed out"));
    assert_eq!(started.elapsed(), Duration::from_millis(500));
    assert!(matches!(
        received.try_recv().unwrap(),
        ProviderEvent::Transcript { .. }
    ));
    // Even with capacity restored, an already expired deadline cannot succeed.
    assert!(
        before_final_deadline(Some(deadline), emit(&events, ProviderEvent::TurnComplete))
            .await
            .is_err()
    );
    assert!(received.try_recv().is_err());
}
