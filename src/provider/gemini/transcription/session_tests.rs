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
    input.send(vec![0; 1600]).await.unwrap();
    input.send(samples).await.unwrap();
    drop(input);
    let (events, mut output) = mpsc::channel(16);
    let collect = async {
        let mut received = Vec::new();
        while let Some(event) = output.recv().await {
            received.push(event);
        }
        received
    };
    let settings = config();
    timeout(Duration::from_secs(8), async {
        tokio::join!(
            session(&settings, "synthetic-key", endpoint, audio, events, cancel),
            collect
        )
    })
    .await
    .unwrap()
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
    assert_eq!(retryable_error(&error), Some(true));
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
    assert_eq!(retryable_error(&error), Some(false));
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
    assert_eq!(retryable_error(&error), Some(true));
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
