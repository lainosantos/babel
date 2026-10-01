//! End-to-end Live-to-finite recovery using synthetic PCM and loopback servers.
use super::*;
use axum::{Json, Router, http::StatusCode, routing::post};
use std::{
    io::Cursor,
    sync::{Arc, Mutex},
};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{WebSocketStream, accept_async};
use tokio_util::task::AbortOnDropHandle;

type Server = WebSocketStream<TcpStream>;
type Worker = AbortOnDropHandle<Result<()>>;

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
        max_reconnect_attempts: 0,
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
        setup["setup"]["generationConfig"]["responseModalities"],
        json!(["TEXT"])
    );
    assert_eq!(
        setup["setup"]["realtimeInputConfig"]["automaticActivityDetection"]["disabled"],
        true
    );
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

async fn close(socket: &mut Server) {
    socket
        .close(Some(tungstenite::protocol::CloseFrame {
            code: tungstenite::protocol::frame::coding::CloseCode::Restart,
            reason: "synthetic restart".into(),
        }))
        .await
        .unwrap();
}

fn completed(text: &str) -> Value {
    json!({"status":"completed","steps":[{"type":"model_output","content":[{"type":"text","text":text}]}]})
}

fn decode_wav(body: &Value) -> Vec<i16> {
    assert_eq!(body["model"], "gemini-3.5-transcribe");
    assert_eq!(body["store"], false);
    assert_eq!(body["input"][0]["mime_type"], "audio/wav");
    let bytes = STANDARD
        .decode(body["input"][0]["data"].as_str().unwrap())
        .unwrap();
    let mut wav = hound::WavReader::new(Cursor::new(bytes)).unwrap();
    assert_eq!(wav.spec().channels, 1);
    assert_eq!(wav.spec().sample_rate, 16000);
    wav.samples::<i16>()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
}

async fn http_app(app: Router) -> (String, AbortOnDropHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/interactions", listener.local_addr().unwrap());
    let worker = AbortOnDropHandle::new(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    (endpoint, worker)
}

struct HttpFixture {
    endpoint: String,
    calls: Arc<Mutex<Vec<Vec<i16>>>>,
    _worker: AbortOnDropHandle<()>,
}

async fn http(responses: Vec<(StatusCode, Value)>) -> HttpFixture {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let captured = calls.clone();
    let app = Router::new().route(
        "/interactions",
        post(move |Json(body): Json<Value>| {
            let pcm = decode_wav(&body);
            let response = {
                let mut calls = captured.lock().unwrap();
                let response = responses
                    .get(calls.len())
                    .expect("unexpected repeated recovery request")
                    .clone();
                calls.push(pcm);
                response
            };
            async move { (response.0, Json(response.1)) }
        }),
    );
    let (endpoint, worker) = http_app(app).await;
    HttpFixture {
        endpoint,
        calls,
        _worker: worker,
    }
}

fn spawn(
    config: SessionConfig,
    websocket: String,
    recovery: String,
) -> (
    Worker,
    mpsc::Sender<Vec<i16>>,
    mpsc::Receiver<ProviderEvent>,
    CancellationToken,
) {
    let (input, audio) = mpsc::channel(16);
    let (events, output) = mpsc::channel(32);
    let cancel = CancellationToken::new();
    let cancellation = cancel.clone();
    let task = AbortOnDropHandle::new(tokio::spawn(async move {
        session(
            &config,
            "synthetic-key",
            &websocket,
            &recovery,
            audio,
            events,
            cancellation,
        )
        .await
    }));
    (task, input, output, cancel)
}

async fn event(events: &mut mpsc::Receiver<ProviderEvent>) -> ProviderEvent {
    timeout(Duration::from_secs(3), events.recv())
        .await
        .unwrap()
        .unwrap()
}

async fn finish(
    worker: Worker,
    events: &mut mpsc::Receiver<ProviderEvent>,
) -> (Result<()>, Vec<ProviderEvent>) {
    let result = timeout(Duration::from_secs(3), worker)
        .await
        .unwrap()
        .unwrap();
    let mut received = Vec::new();
    while let Some(event) = events.recv().await {
        received.push(event);
    }
    (result, received)
}

fn assert_no_loss(events: &[ProviderEvent]) {
    assert!(
        !events.iter().any(|event| matches!(
            event,
            ProviderEvent::Interrupted
                | ProviderEvent::Reconnecting { .. }
                | ProviderEvent::Warning { .. }
        )),
        "successful retention/recovery must not report lost original audio"
    );
}

fn transcripts(events: &[ProviderEvent]) -> Vec<(&str, Option<u64>)> {
    events
        .iter()
        .filter_map(|event| match event {
            ProviderEvent::Transcript {
                input: true,
                text,
                metadata,
            } => Some((text.as_str(), metadata.alignment_ms)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn live_close_recovers_only_unacknowledged_eof_pcm_once_with_stable_alignment() {
    let (listener, endpoint) = bind().await;
    let server = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut socket = accept(&listener, true).await;
        assert_eq!(turn(&mut socket).await, vec![110; 1600]);
        socket
            .send(Message::Text(
                json!({"serverContent":{"inputTranscription":{"text":"Acknowledged original."}}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        assert_eq!(turn(&mut socket).await, vec![220; 1600]);
        close(&mut socket).await;
    }));
    let http = http(vec![(
        StatusCode::OK,
        completed("Recovered original tail."),
    )])
    .await;
    let (worker, input, mut events, _) = spawn(config(), endpoint, http.endpoint.clone());
    assert_eq!(event(&mut events).await, ProviderEvent::Connected);
    input.send(vec![0; 1600]).await.unwrap();
    input.send(vec![110; 1600]).await.unwrap();
    let acknowledged = event(&mut events).await;
    assert_eq!(
        transcripts(&[acknowledged]),
        vec![("Acknowledged original.", Some(100))]
    );
    assert_eq!(event(&mut events).await, ProviderEvent::TurnComplete);
    input.send(vec![0; 1600]).await.unwrap();
    input.send(vec![220; 1600]).await.unwrap();
    drop(input);
    let (result, remaining) = finish(worker, &mut events).await;
    result.unwrap();
    server.await.unwrap();
    assert_no_loss(&remaining);
    assert!(
        remaining
            .iter()
            .any(|event| matches!(event, ProviderEvent::RecoveringOriginal { attempt: 1 }))
    );
    assert_eq!(
        transcripts(&remaining),
        vec![("Recovered original tail.", Some(300))]
    );
    assert_eq!(*http.calls.lock().unwrap(), vec![vec![220; 1600]]);
}

#[tokio::test]
async fn interrupted_live_turn_recovers_original_pcm_without_a_false_gap() {
    let (listener, endpoint) = bind().await;
    let server = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut socket = accept(&listener, true).await;
        assert_eq!(turn(&mut socket).await, vec![260; 1600]);
        socket
            .send(Message::Text(
                json!({"serverContent":{"interrupted":true,"inputTranscription":{"text":"Uncommitted interrupted text"}}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        timeout(Duration::from_secs(3), async {
            while let Some(Ok(_)) = socket.next().await {}
        })
        .await
        .unwrap();
    }));
    let http = http(vec![(
        StatusCode::OK,
        completed("Recovered interrupted original."),
    )])
    .await;
    let (worker, input, mut events, _) = spawn(config(), endpoint, http.endpoint.clone());
    assert_eq!(event(&mut events).await, ProviderEvent::Connected);
    input.send(vec![0; 1600]).await.unwrap();
    input.send(vec![260; 1600]).await.unwrap();
    drop(input);
    let (result, remaining) = finish(worker, &mut events).await;
    result.unwrap();
    server.await.unwrap();
    assert_no_loss(&remaining);
    assert_eq!(
        transcripts(&remaining),
        vec![("Recovered interrupted original.", Some(100))]
    );
    assert_eq!(*http.calls.lock().unwrap(), vec![vec![260; 1600]]);
}

#[tokio::test]
async fn missing_live_noise_final_recovers_empty_then_keeps_processing_new_speech() {
    let (listener, endpoint) = bind().await;
    let server = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut socket = accept(&listener, true).await;
        assert_eq!(turn(&mut socket).await, vec![1; 1600]);
        timeout(Duration::from_secs(7), async {
            while let Some(Ok(_)) = socket.next().await {}
        })
        .await
        .unwrap();
    }));
    let http = http(vec![
        (
            StatusCode::OK,
            json!({"id":"synthetic-no-speech","model":"gemini-3.5-transcribe","status":"completed"}),
        ),
        (StatusCode::OK, completed("Speech after quiet input.")),
    ])
    .await;
    let (worker, input, mut events, _) = spawn(config(), endpoint, http.endpoint.clone());
    assert_eq!(event(&mut events).await, ProviderEvent::Connected);
    input.send(vec![0; 1600]).await.unwrap();
    input.send(vec![1; 1600]).await.unwrap();
    // Exercise the actual five-second Live deadline, without advancing mocked time.
    assert_eq!(
        timeout(Duration::from_secs(7), events.recv())
            .await
            .unwrap()
            .unwrap(),
        ProviderEvent::RecoveringOriginal { attempt: 1 }
    );
    assert_eq!(event(&mut events).await, ProviderEvent::TurnComplete);
    assert_eq!(event(&mut events).await, ProviderEvent::Connected);
    assert!(!worker.is_finished());
    input.send(vec![0; 1600]).await.unwrap();
    input.send(vec![310; 1600]).await.unwrap();
    drop(input);
    let (result, remaining) = finish(worker, &mut events).await;
    result.unwrap();
    server.await.unwrap();
    assert_no_loss(&remaining);
    assert!(
        !remaining
            .iter()
            .any(|event| matches!(event, ProviderEvent::RecoveringOriginal { .. }))
    );
    assert_eq!(
        transcripts(&remaining),
        vec![("Speech after quiet input.", Some(300))]
    );
    assert_eq!(
        *http.calls.lock().unwrap(),
        vec![vec![1; 1600], vec![310; 1600]]
    );
}

#[tokio::test]
async fn failed_live_setup_releases_the_startup_gate_and_consumes_queued_originals() {
    let (listener, endpoint) = bind().await;
    let server = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut socket = accept(&listener, false).await;
        close(&mut socket).await;
    }));
    let http = http(vec![(
        StatusCode::OK,
        completed("Original after setup failure."),
    )])
    .await;
    let (worker, input, mut events, _) = spawn(config(), endpoint, http.endpoint.clone());
    assert_eq!(
        event(&mut events).await,
        ProviderEvent::RecoveringOriginal { attempt: 1 }
    );
    assert_eq!(event(&mut events).await, ProviderEvent::Connected);
    // Session-owned recognition sends audio only after Connected.
    input.send(vec![0; 1600]).await.unwrap();
    input.send(vec![42; 1600]).await.unwrap();
    drop(input);
    let (result, remaining) = finish(worker, &mut events).await;
    result.unwrap();
    server.await.unwrap();
    assert_no_loss(&remaining);
    assert_eq!(
        transcripts(&remaining),
        vec![("Original after setup failure.", Some(100))]
    );
    assert_eq!(*http.calls.lock().unwrap(), vec![vec![42; 1600]]);
}

#[tokio::test]
async fn finite_retry_resends_identical_originals_without_duplicating_the_transcript() {
    let (listener, endpoint) = bind().await;
    let server = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut socket = accept(&listener, true).await;
        assert_eq!(turn(&mut socket).await, vec![500; 1600]);
        close(&mut socket).await;
    }));
    let http = http(vec![
        (
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error":"private server details"}),
        ),
        (StatusCode::OK, completed("Retried original.")),
    ])
    .await;
    let mut config = config();
    config.max_reconnect_attempts = 1;
    let (worker, input, mut events, _) = spawn(config, endpoint, http.endpoint.clone());
    assert_eq!(event(&mut events).await, ProviderEvent::Connected);
    input.send(vec![500; 1600]).await.unwrap();
    drop(input);
    let (result, remaining) = finish(worker, &mut events).await;
    result.unwrap();
    server.await.unwrap();
    assert_no_loss(&remaining);
    assert_eq!(
        transcripts(&remaining),
        vec![("Retried original.", Some(0))]
    );
    assert_eq!(
        *http.calls.lock().unwrap(),
        vec![vec![500; 1600], vec![500; 1600]]
    );
}

#[tokio::test]
async fn failed_finite_recovery_does_not_claim_original_audio_was_transcribed() {
    let (listener, endpoint) = bind().await;
    let server = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut socket = accept(&listener, true).await;
        assert_eq!(turn(&mut socket).await, vec![710; 1600]);
        close(&mut socket).await;
    }));
    let http = http(vec![(
        StatusCode::UNAUTHORIZED,
        json!({"error":"private speech synthetic-key"}),
    )])
    .await;
    let (worker, input, mut events, _) = spawn(config(), endpoint, http.endpoint.clone());
    assert_eq!(event(&mut events).await, ProviderEvent::Connected);
    input.send(vec![710; 1600]).await.unwrap();
    drop(input);
    let (result, remaining) = finish(worker, &mut events).await;
    let error = result.unwrap_err().to_string();
    assert!(error.contains("HTTP 401"));
    assert!(!error.contains("private speech"));
    assert!(!error.contains("synthetic-key"));
    assert!(transcripts(&remaining).is_empty());
    assert!(
        !remaining
            .iter()
            .any(|event| matches!(event, ProviderEvent::TurnComplete))
    );
    assert_eq!(*http.calls.lock().unwrap(), vec![vec![710; 1600]]);
    server.await.unwrap();
}

#[tokio::test]
async fn cancellation_interrupts_a_pending_finite_request_without_waiting_for_timeout() {
    let (listener, endpoint) = bind().await;
    let server = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut socket = accept(&listener, true).await;
        assert_eq!(turn(&mut socket).await, vec![900; 1600]);
        close(&mut socket).await;
    }));
    let entered = Arc::new(tokio::sync::Notify::new());
    let signal = entered.clone();
    let app = Router::new().route(
        "/interactions",
        post(move |Json(body): Json<Value>| {
            assert_eq!(decode_wav(&body), vec![900; 1600]);
            let signal = signal.clone();
            async move {
                signal.notify_one();
                std::future::pending::<Json<Value>>().await
            }
        }),
    );
    let (recovery, _http) = http_app(app).await;
    let (worker, input, mut events, cancel) = spawn(config(), endpoint, recovery);
    assert_eq!(event(&mut events).await, ProviderEvent::Connected);
    input.send(vec![900; 1600]).await.unwrap();
    drop(input);
    timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    cancel.cancel();
    timeout(Duration::from_millis(500), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    while let Some(event) = events.recv().await {
        assert!(!matches!(
            event,
            ProviderEvent::Transcript { .. } | ProviderEvent::TurnComplete
        ));
    }
    server.await.unwrap();
}
