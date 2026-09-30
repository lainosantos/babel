use super::*;
use tokio::net::TcpListener;
use tokio_tungstenite::{
    accept_async,
    tungstenite::protocol::{CloseFrame, frame::coding::CloseCode},
};

const PRIVATE_REASON: &str = "fake-api-key / private source speech / server payload";

#[test]
fn close_codes_keep_safe_categories_and_retry_classification() {
    for (code, category, retryable) in [
        (1000, "session ended normally", true),
        (1001, "server going away", true),
        (1002, "protocol violation", false),
        (1003, "unsupported message type", false),
        (1007, "invalid message payload", false),
        (1008, "policy or authentication rejected", false),
        (1009, "message exceeds server size limit", false),
        (1011, "temporary server failure", true),
        (1012, "server restart", true),
        (1013, "server busy", true),
        (4000, "unexpected closure", true),
    ] {
        let failure = close_failure(Some(code), CloseStage::Streaming);
        assert_eq!(failure.retryable, retryable, "code {code}");
        assert!(failure.message.contains(&format!("code {code}:")));
        assert!(failure.message.contains(category));
        assert!(failure.message.contains("live streaming"));
    }
    let absent = close_failure(None, CloseStage::History);
    assert!(absent.retryable);
    assert!(
        absent
            .message
            .contains("historical transcription (no close code)")
    );
}

async fn rejected_socket(
    code: u16,
    acknowledge: bool,
    retries: u32,
) -> (String, Vec<ProviderEvent>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for _ in 0..=retries {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let setup = socket.next().await.unwrap().unwrap();
            let setup: Value = serde_json::from_str(setup.to_text().unwrap()).unwrap();
            assert!(setup.get("setup").is_some());
            if acknowledge {
                socket
                    .send(Message::Text(r#"{"setupComplete":{}}"#.into()))
                    .await
                    .unwrap();
            }
            socket
                .close(Some(CloseFrame {
                    code: CloseCode::from(code),
                    reason: PRIVATE_REASON.into(),
                }))
                .await
                .unwrap();
        }
    });
    let config = SessionConfig {
        model: TRANSLATE_MODEL.into(),
        api_key_env: "BABEL_TEST_KEY_NOT_READ".into(),
        voice: String::new(),
        source_language: "pt-BR".into(),
        target_language: "en".into(),
        prompt: String::new(),
        vad_silence_ms: 180,
        connect_timeout_secs: 2,
        max_reconnect_attempts: retries,
        input_transcription: false,
        output_transcription: false,
    };
    let (_audio, audio_rx) = mpsc::channel(1);
    let (events, mut received) = mpsc::channel(16);
    let result = timeout(
        Duration::from_secs(3),
        run_sessions(
            &config,
            "fake-api-key",
            &endpoint,
            audio_rx,
            events,
            CancellationToken::new(),
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap();
    let mut delivered = Vec::new();
    while let Ok(event) = received.try_recv() {
        delivered.push(event);
    }
    let error = format!("{result:#}");
    for private in ["fake-api-key", "private source speech", "server payload"] {
        assert!(!error.contains(private));
    }
    (error, delivered)
}

#[tokio::test]
async fn rejected_setup_retains_close_code_without_private_reason_or_retry() {
    for code in [1002, 1003, 1007, 1008, 1009] {
        let (error, events) = rejected_socket(code, false, 0).await;
        assert!(error.contains(&format!("code {code}:")), "{error}");
        assert!(error.contains("setup acknowledgement"), "{error}");
        assert!(!error.contains("reconnect budget"));
        assert!(events.is_empty());
    }
}

#[tokio::test]
async fn rejected_live_stream_retains_close_code_without_private_reason_or_retry() {
    for code in [1002, 1003, 1007, 1008, 1009] {
        let (error, events) = rejected_socket(code, true, 0).await;
        assert!(error.contains(&format!("code {code}:")), "{error}");
        assert!(error.contains("live streaming"), "{error}");
        assert!(!error.contains("reconnect budget"));
        assert_eq!(events, vec![ProviderEvent::Connected]);
    }
}

#[tokio::test]
async fn transient_close_retains_code_after_bounded_reconnect() {
    let (error, events) = rejected_socket(1013, true, 1).await;
    assert!(error.contains("code 1013: server busy"), "{error}");
    assert!(error.contains("reconnect budget exhausted"), "{error}");
    assert_eq!(
        events,
        vec![
            ProviderEvent::Connected,
            ProviderEvent::Interrupted,
            ProviderEvent::Reconnecting { attempt: 1 },
            ProviderEvent::Connected,
        ]
    );
}
