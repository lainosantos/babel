use super::*;
use crate::audio::PcmFrame;
use anyhow::{Result, bail};
use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, header::SERVER},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::json;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

#[test]
fn wake_requires_addressed_whole_words_and_preserves_command_case() {
    use super::service::addressed_command as extract;
    assert_eq!(
        extract("BAbel, acenda a Sala!", "Babel"),
        Some("acenda a Sala!".into())
    );
    assert_eq!(
        extract("Oi, Babel. Acenda a luz", "Babel"),
        Some("Acenda a luz".into())
    );
    assert_eq!(
        extract("Hey my Agent: lights on", "My Agent"),
        Some("lights on".into())
    );
    assert_eq!(extract("Babel!", "Babel"), Some(String::new()));
    assert_eq!(extract("Oi, ligar a luz", "Oi"), Some("ligar a luz".into()));
    assert_eq!(extract("eu uso Babel para áudio", "Babel"), None);
    assert_eq!(extract("Babelina ligue", "Babel"), None);
    assert_eq!(extract("desbabel ligue", "Babel"), None);
}

#[test]
fn only_loopback_inference_and_bounded_configuration_are_allowed() {
    let mut config = AgentConfig::default();
    assert_eq!(config.whisper_endpoint, "auto");
    assert_eq!(config.needle_endpoint, "auto");
    config.validate().unwrap();
    config.services_directory = "relative-services".into();
    assert!(config.validate().is_err());
    config.services_directory.clear();
    config.whisper_api_key_env = "CUSTOM_WHISPER_KEY".into();
    assert!(config.validate().is_err());
    config.whisper_api_key_env.clear();
    for address in [
        "https://cloud.example/inference",
        "http://localhost.evil/",
        "http://127.0.0.1/?secret=x",
        "http://user:key@localhost/",
        "file:///tmp/model",
        "http://192.168.1.2/",
    ] {
        config.whisper_endpoint = address.into();
        assert!(config.validate().is_err(), "{address}");
    }
    for address in [
        "http://localhost:8080/inference",
        "http://[::1]:8080/inference",
        "http://127.0.0.2:8080/inference",
    ] {
        config.whisper_endpoint = address.into();
        config.validate().unwrap();
    }
    for threshold in [0.0, 0.25, 0.5, 0.85, 1.0] {
        config.min_confidence = threshold;
        config.validate().unwrap();
    }
    for threshold in [-0.01, 1.01, f64::NAN, f64::INFINITY] {
        config.min_confidence = threshold;
        assert!(config.validate().is_err());
    }
    config.min_confidence = 0.85;
    config.max_calls = 0;
    assert!(config.validate().is_err());
    assert!(serde_json::from_value::<AgentConfig>(json!({"unknown":true})).is_err());
}

#[test]
fn vad_discards_silence_and_never_executes_truncated_speech() {
    let config = AgentConfig {
        silence_ms: 200,
        max_utterance_ms: 1000,
        ..Default::default()
    };
    let mut vad = super::service::Segmenter::new(&config);
    assert!(vad.push(&vec![0; 3200]).is_none());
    assert!(vad.push(&vec![2000; 1600]).is_none());
    let (samples, truncated) = vad.push(&vec![0; 3200]).unwrap();
    assert!(!truncated);
    assert!(samples.contains(&2000));
    assert!(vad.push(&vec![2000; 8000]).is_none());
    assert!(vad.push(&vec![2000; 8000]).unwrap().1);
    assert!(vad.push(&vec![2000; 8000]).is_none());
    assert!(vad.push(&vec![0; 3200]).is_none());
    assert!(vad.push(&vec![2000; 1600]).is_none());
    assert!(vad.push(&vec![0; 3200]).is_some());
}

fn tool() -> CommandTool {
    CommandTool {
        id: "home/set_lights".into(),
        name: "set_lights".into(),
        description: "Turn room lights on".into(),
        input_schema: json!({"type":"object","properties":{"room":{"type":"string"}},"required":["room"],"additionalProperties":false}),
    }
}

#[test]
fn needle_refusal_uncertainty_untrusted_identity_and_ungrounded_args_never_execute() {
    let tool = tool();
    let aliases = BTreeMap::from([("tool0_set_lights".into(), &tool)]);
    let base = json!({"success":true,"type":"call","confidence":0.99,"function_calls":[{"name":"tool0_set_lights","arguments":{"room":"kitchen"}}]});
    let parse = |value: &Value| super::inference::parse_plan(value, &aliases, 0.85, 4);
    let parsed = parse(&base).unwrap();
    let mut low = base.clone();
    low["confidence"] = json!(0.0);
    assert!(super::inference::parse_plan(&low, &aliases, 0.0, 4).is_ok());
    assert!(super::inference::parse_plan(&low, &aliases, 0.01, 4).is_err());
    low["confidence"] = json!(1.0);
    assert!(super::inference::parse_plan(&low, &aliases, 1.0, 4).is_ok());
    assert_eq!(parsed[0].id, "home/set_lights");
    for (pointer, value) in [
        ("/confidence", json!(0.5)),
        ("/confidence", Value::Null),
        ("/success", json!(false)),
        ("/type", json!("respond")),
        ("/function_calls", json!([])),
        ("/function_calls/0/name", json!("shell")),
        ("/function_calls/0/arguments", json!("{}")),
    ] {
        let mut response = base.clone();
        *response.pointer_mut(pointer).unwrap() = value;
        assert!(parse(&response).is_err(), "{pointer}");
    }
    let mut response = base.clone();
    response["suppressed_calls"] = json!([{"name":"shell"}]);
    assert!(parse(&response).is_err());
    let mut response = base.clone();
    response["validation"] = json!({"ungrounded":["set_lights.room"]});
    assert!(parse(&response).is_err());
    let mut response = base.clone();
    response["validation"] = json!({"negation":true});
    assert!(parse(&response).is_err());
    let mut response = base.clone();
    response["function_calls"]
        .as_array_mut()
        .unwrap()
        .push(base["function_calls"][0].clone());
    assert!(parse(&response).is_err());
}

#[derive(Default)]
struct MockTools {
    calls: AtomicUsize,
    validations: AtomicUsize,
    reject: AtomicUsize,
    hang: AtomicUsize,
}
#[async_trait]
impl CommandTools for MockTools {
    async fn list_tools(&self) -> Result<Vec<CommandTool>> {
        Ok(vec![tool()])
    }
    async fn validate_call(&self, id: &str, arguments: &Value) -> Result<()> {
        assert_eq!(id, "home/set_lights");
        self.validations.fetch_add(1, Ordering::SeqCst);
        if self.reject.load(Ordering::SeqCst) > 0 || arguments["room"] != "kitchen" {
            bail!("schema mismatch");
        }
        assert_eq!(arguments["room"], "kitchen");
        Ok(())
    }
    async fn call_tool(
        &self,
        id: &str,
        arguments: Value,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        assert_eq!(id, "home/set_lights");
        assert_eq!(arguments["room"], "kitchen");
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.hang.load(Ordering::SeqCst) > 0 {
            cancel.cancelled().await;
            bail!("cancelled mock tool");
        }
        Ok(json!({"lights":"on"}))
    }
}
#[derive(Clone)]
struct MockInference {
    transcripts: Arc<Mutex<VecDeque<String>>>,
    asr: Arc<AtomicUsize>,
    planner: Arc<AtomicUsize>,
    invalid: Arc<AtomicUsize>,
    asr_failure: Arc<AtomicUsize>,
}
async fn whisper_health() -> impl IntoResponse {
    ([(SERVER, "whisper.cpp")], Json(json!({"status":"ok"})))
}
async fn mock_whisper_health(State(state): State<MockInference>) -> Response {
    if state.asr_failure.load(Ordering::SeqCst) == 1 {
        return axum::response::Html("<html>Another local application</html>").into_response();
    }
    whisper_health().await.into_response()
}
async fn whisper(State(state): State<MockInference>, body: axum::body::Bytes) -> Response {
    let data = String::from_utf8_lossy(&body);
    if !data.contains("name=\"file\"") {
        assert!(!data.contains("RIFF"));
        return (
            StatusCode::BAD_REQUEST,
            [(SERVER, "whisper.cpp")],
            "Invalid request",
        )
            .into_response();
    }
    assert!(data.contains("name=\"translate\"\r\n\r\nfalse"));
    assert!(data.contains("RIFF"));
    state.asr.fetch_add(1, Ordering::SeqCst);
    let failure = state.asr_failure.load(Ordering::SeqCst);
    if failure == 2 || failure == 3 {
        if failure == 3 {
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(SERVER, "whisper.cpp")],
            Json(json!({"error":"mock model unavailable"})),
        )
            .into_response();
    }
    (
        [(SERVER, "whisper.cpp")],
        Json(json!({"text":state.transcripts.lock().unwrap().pop_front().unwrap_or_default()})),
    )
        .into_response()
}
async fn needle(State(state): State<MockInference>, Json(request): Json<Value>) -> Json<Value> {
    state.planner.fetch_add(1, Ordering::SeqCst);
    assert!(
        !request["text"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .starts_with("babel")
    );
    assert_eq!(request["tools"][0]["parameters"]["required"][0], "room");
    let mut response = json!({"success":true,"type":"call","confidence":0.99,"function_calls":[{"name":request["tools"][0]["name"],"arguments":{"room":"kitchen"}}]});
    if state.invalid.load(Ordering::SeqCst) == 1 {
        response["confidence"] = json!(0.3);
    } else if state.invalid.load(Ordering::SeqCst) == 2 {
        response["function_calls"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":request["tools"][0]["name"],"arguments":{"room":"invalid"}}));
    }
    Json(response)
}
struct Fixture {
    service: Arc<CommandService>,
    tools: Arc<MockTools>,
    inference: MockInference,
    server: tokio::task::JoinHandle<()>,
    config: AgentConfig,
}
impl Fixture {
    async fn new(texts: &[&str]) -> Self {
        let inference = MockInference {
            transcripts: Arc::new(Mutex::new(texts.iter().map(|s| s.to_string()).collect())),
            asr: Arc::new(AtomicUsize::new(0)),
            planner: Arc::new(AtomicUsize::new(0)),
            invalid: Arc::new(AtomicUsize::new(0)),
            asr_failure: Arc::new(AtomicUsize::new(0)),
        };
        let router = Router::new()
            .route("/health", get(mock_whisper_health))
            .route("/inference", post(whisper))
            .route("/complete", post(needle))
            .with_state(inference.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let config = AgentConfig {
            whisper_endpoint: format!("http://127.0.0.1:{port}/inference"),
            needle_endpoint: format!("http://127.0.0.1:{port}/complete"),
            silence_ms: 200,
            max_utterance_ms: 1000,
            command_window_secs: 2,
            timeout_secs: 2,
            ..Default::default()
        };
        let tools = Arc::new(MockTools::default());
        let service = CommandService::new(config.clone(), tools.clone()).unwrap();
        service.start().unwrap();
        service.set_microphone_active(true);
        tokio::time::sleep(Duration::from_millis(15)).await;
        Self {
            service,
            tools,
            inference,
            server,
            config,
        }
    }
    async fn speech(&self) {
        for samples in [vec![3000; 1600], vec![0; 3200]] {
            assert!(self.service.try_audio(&PcmFrame {
                samples,
                sample_rate: 16000,
                captured_at: Instant::now()
            }));
            tokio::task::yield_now().await;
        }
    }
    async fn wait(&self, phase: CommandPhase) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while self.service.status().phase != phase {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("status {:?}, expected {phase:?}", self.service.status()));
    }
    async fn close(self) {
        self.service.shutdown().await;
        self.server.abort();
    }
}

#[tokio::test]
async fn same_utterance_routes_only_explicit_wake_and_reports_original_result() {
    let f = Fixture::new(&["eu uso Babel para falar", "Babel, acenda a cozinha"]).await;
    f.speech().await;
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(f.inference.asr.load(Ordering::SeqCst), 1);
    assert_eq!(f.inference.planner.load(Ordering::SeqCst), 0);
    f.speech().await;
    f.wait(CommandPhase::Succeeded).await;
    assert_eq!(f.tools.calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.service.status().activation_id, 1);
    assert!(f.service.status().result.unwrap().contains("lights"));
    f.close().await;
}

#[tokio::test]
async fn wake_alone_accepts_following_utterance_and_cancel_does_not_call_tools() {
    let f = Fixture::new(&["Babel", "acenda a cozinha", "Babel cancelar"]).await;
    f.speech().await;
    f.wait(CommandPhase::Activated).await;
    f.speech().await;
    f.wait(CommandPhase::Succeeded).await;
    f.speech().await;
    f.wait(CommandPhase::Failed).await;
    assert_eq!(f.tools.calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.inference.planner.load(Ordering::SeqCst), 1);
    f.close().await;
}

#[tokio::test]
async fn changing_microphone_discards_wake_context_and_disabled_tap_ignores_audio() {
    let f = Fixture::new(&["Babel", "acenda a cozinha"]).await;
    f.speech().await;
    f.wait(CommandPhase::Activated).await;
    f.service.set_microphone_active(false);
    f.service.set_microphone_active(true);
    tokio::time::sleep(Duration::from_millis(15)).await;
    f.speech().await;
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(f.tools.calls.load(Ordering::SeqCst), 0);
    let mut config = f.config.clone();
    config.enabled = false;
    f.service.update_config(config).unwrap();
    tokio::time::sleep(Duration::from_millis(15)).await;
    assert!(!f.service.try_audio(&PcmFrame {
        samples: vec![3000; 1600],
        sample_rate: 16000,
        captured_at: Instant::now()
    }));
    assert_eq!(f.service.status().phase, CommandPhase::Disabled);
    f.close().await;
}

#[tokio::test]
async fn schema_error_and_low_confidence_fail_without_tool_execution() {
    let f = Fixture::new(&["Babel acenda a cozinha", "Babel acenda a cozinha"]).await;
    f.tools.reject.store(1, Ordering::SeqCst);
    f.speech().await;
    f.wait(CommandPhase::Failed).await;
    assert_eq!(f.tools.calls.load(Ordering::SeqCst), 0);
    f.tools.reject.store(0, Ordering::SeqCst);
    f.inference.invalid.store(1, Ordering::SeqCst);
    f.speech().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(f.tools.calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.inference.planner.load(Ordering::SeqCst), 2);
    f.close().await;
}

#[tokio::test]
async fn cancellation_stops_inflight_tool_and_never_replays_backlog() {
    let f = Fixture::new(&["Babel acenda a cozinha"]).await;
    f.tools.hang.store(1, Ordering::SeqCst);
    f.speech().await;
    f.wait(CommandPhase::Executing).await;
    assert!(!f.service.try_audio(&PcmFrame {
        samples: vec![3000; 1600],
        sample_rate: 16000,
        captured_at: Instant::now()
    }));
    f.service.cancel();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(f.tools.calls.load(Ordering::SeqCst), 1);
    f.close().await;
}

#[tokio::test]
async fn stale_audio_is_dropped_and_shutdown_is_bounded() {
    let f = Fixture::new(&[]).await;
    assert!(!f.service.try_audio(&PcmFrame {
        samples: vec![3000; 1600],
        sample_rate: 16000,
        captured_at: Instant::now() - Duration::from_secs(10)
    }));
    assert!(f.service.status().dropped_frames > 0);
    assert_eq!(f.inference.asr.load(Ordering::SeqCst), 0);
    tokio::time::timeout(Duration::from_secs(1), f.close())
        .await
        .unwrap();
}

#[tokio::test]
async fn every_call_is_preflighted_before_the_first_side_effect() {
    let f = Fixture::new(&["Babel acenda a cozinha e a sala"]).await;
    f.inference.invalid.store(2, Ordering::SeqCst);
    f.speech().await;
    f.wait(CommandPhase::Failed).await;
    assert_eq!(f.tools.validations.load(Ordering::SeqCst), 2);
    assert_eq!(f.tools.calls.load(Ordering::SeqCst), 0);
    f.close().await;
}

#[tokio::test]
async fn audio_discontinuity_clears_an_armed_wake_name() {
    let f = Fixture::new(&["Babel", "acenda a cozinha"]).await;
    f.speech().await;
    f.wait(CommandPhase::Activated).await;
    assert!(!f.service.try_audio(&PcmFrame {
        samples: vec![3000; 1600],
        sample_rate: 16000,
        captured_at: Instant::now() - Duration::from_secs(2)
    }));
    f.speech().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(f.tools.calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.inference.planner.load(Ordering::SeqCst), 0);
    f.close().await;
}

#[tokio::test]
async fn preflight_failure_before_wake_is_service_error_without_audio_upload_or_tool_call() {
    let f = Fixture::new(&["Babel acenda a cozinha"]).await;
    f.inference.asr_failure.store(1, Ordering::SeqCst);
    f.speech().await;
    f.wait(CommandPhase::Failed).await;
    let status = f.service.status();
    assert_eq!(status.error_scope, Some(CommandErrorScope::Service));
    assert_eq!(status.activation_id, 0);
    assert!(
        status
            .error
            .unwrap()
            .contains("not a verified whisper.cpp server")
    );
    assert!(status.command.is_none() && status.tool.is_none() && status.result.is_none());
    assert_eq!(f.inference.asr.load(Ordering::SeqCst), 0);
    assert_eq!(f.inference.planner.load(Ordering::SeqCst), 0);
    assert_eq!(f.tools.calls.load(Ordering::SeqCst), 0);
    f.service.set_microphone_active(false);
    let status = f.service.status();
    assert_eq!(status.phase, CommandPhase::Inactive);
    assert!(status.error.is_none() && status.error_scope.is_none());
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(f.service.status().phase, CommandPhase::Inactive);
    assert_eq!(f.tools.calls.load(Ordering::SeqCst), 0);
    f.close().await;
}

#[tokio::test]
async fn historical_activation_does_not_relabel_background_asr_failure_as_command_failure() {
    let f = Fixture::new(&["Babel acenda a cozinha"]).await;
    f.speech().await;
    f.wait(CommandPhase::Succeeded).await;
    assert_eq!(f.service.status().activation_id, 1);
    assert!(f.service.status().result.is_some());
    f.inference.asr_failure.store(2, Ordering::SeqCst);
    f.speech().await;
    f.wait(CommandPhase::Failed).await;
    let status = f.service.status();
    assert_eq!(status.activation_id, 1);
    assert_eq!(status.error_scope, Some(CommandErrorScope::Service));
    assert!(status.command.is_none() && status.tool.is_none() && status.result.is_none());
    assert_eq!(f.tools.calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.inference.planner.load(Ordering::SeqCst), 1);
    f.close().await;
}

#[tokio::test]
async fn asr_failure_after_recognized_wake_is_a_command_failure() {
    let f = Fixture::new(&["Babel"]).await;
    f.speech().await;
    f.wait(CommandPhase::Activated).await;
    assert!(f.service.status().error_scope.is_none());
    f.inference.asr_failure.store(2, Ordering::SeqCst);
    f.speech().await;
    f.wait(CommandPhase::Failed).await;
    assert_eq!(
        f.service.status().error_scope,
        Some(CommandErrorScope::Command)
    );
    assert_eq!(f.service.status().activation_id, 1);
    assert_eq!(f.tools.calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.inference.planner.load(Ordering::SeqCst), 0);
    f.close().await;
}

#[tokio::test]
async fn disabling_microphone_cancels_pending_asr_without_restoring_its_error() {
    let f = Fixture::new(&[]).await;
    f.inference.asr_failure.store(3, Ordering::SeqCst);
    f.speech().await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while f.inference.asr.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    f.service.set_microphone_active(false);
    assert_eq!(f.service.status().phase, CommandPhase::Inactive);
    assert!(f.service.status().error_scope.is_none());
    tokio::time::sleep(Duration::from_millis(350)).await;
    let status = f.service.status();
    assert_eq!(status.phase, CommandPhase::Inactive);
    assert!(!status.microphone_active);
    assert!(status.error.is_none() && status.error_scope.is_none());
    assert_eq!(f.tools.calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.inference.planner.load(Ordering::SeqCst), 0);
    f.close().await;
}

#[tokio::test]
async fn ordinary_long_speech_is_discarded_without_a_command_failure() {
    let f = Fixture::new(&[]).await;
    for _ in 0..5 {
        assert!(f.service.try_audio(&PcmFrame {
            samples: vec![3000; 3200],
            sample_rate: 16000,
            captured_at: Instant::now(),
        }));
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    tokio::time::timeout(Duration::from_secs(1), async {
        while f.service.status().dropped_frames == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let status = f.service.status();
    assert_eq!(status.phase, CommandPhase::Listening);
    assert_eq!(status.activation_id, 0);
    assert!(status.error.is_none() && status.error_scope.is_none());
    assert_eq!(f.inference.asr.load(Ordering::SeqCst), 0);
    assert_eq!(f.tools.calls.load(Ordering::SeqCst), 0);
    f.close().await;
}

#[test]
fn error_scope_has_stable_serialization_and_is_optional_for_old_statuses() {
    let mut status = CommandStatus::initial(&AgentConfig::default());
    let old = serde_json::to_value(&status).unwrap();
    assert!(old.get("error_scope").is_none());
    assert!(
        serde_json::from_value::<CommandStatus>(old)
            .unwrap()
            .error_scope
            .is_none()
    );
    status.error_scope = Some(CommandErrorScope::Service);
    assert_eq!(
        serde_json::to_value(&status).unwrap()["error_scope"],
        "service"
    );
    status.error_scope = Some(CommandErrorScope::Command);
    assert_eq!(
        serde_json::to_value(&status).unwrap()["error_scope"],
        "command"
    );
}

#[tokio::test]
async fn unknown_local_app_is_rejected_before_any_microphone_upload() {
    for mode in 0..3 {
        let post_requests = Arc::new(AtomicUsize::new(0));
        let audio_uploads = Arc::new(AtomicUsize::new(0));
        let posts = post_requests.clone();
        let audio = audio_uploads.clone();
        let router = Router::new()
            .route(
                "/asr/health",
                get(move || async move {
                    match mode {
                        0 => (
                            [(axum::http::header::CONTENT_TYPE, "text/html")],
                            "<html>Another local application</html>",
                        )
                            .into_response(),
                        1 => Json(json!({"status":"ok"})).into_response(),
                        _ => ([(SERVER, "whisper.cpp")], Json(json!({"status":"ok"})))
                            .into_response(),
                    }
                }),
            )
            .route(
                "/asr/inference",
                post(move |body: axum::body::Bytes| {
                    let posts = posts.clone();
                    let audio = audio.clone();
                    async move {
                        posts.fetch_add(1, Ordering::SeqCst);
                        if body.windows(4).any(|part| part == b"RIFF") {
                            audio.fetch_add(1, Ordering::SeqCst);
                        }
                        axum::response::Html("<html>Wrong endpoint</html>")
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/asr/inference", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let config = AgentConfig {
            whisper_endpoint: endpoint,
            ..Default::default()
        };
        let inference = super::inference::Inference::new(&config).unwrap();
        let error = inference
            .transcribe(&[1000; 1600])
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("not a verified whisper.cpp server"),
            "{error}"
        );
        assert_eq!(post_requests.load(Ordering::SeqCst), usize::from(mode == 2));
        assert_eq!(audio_uploads.load(Ordering::SeqCst), 0);
        server.abort();
    }
}

#[tokio::test]
async fn whisper_json_missing_file_contract_is_cached_without_repeated_probes() {
    let probes = Arc::new(AtomicUsize::new(0));
    let captured = Arc::new(AtomicUsize::new(0));
    let probe_count = probes.clone();
    let audio_count = captured.clone();
    let router = Router::new()
        .route("/prefix/health", get(whisper_health))
        .route(
            "/prefix/inference",
            post(move |body: axum::body::Bytes| {
                let probes = probe_count.clone();
                let captured = audio_count.clone();
                async move {
                    if body.windows(4).any(|part| part == b"RIFF") {
                        captured.fetch_add(1, Ordering::SeqCst);
                        ([(SERVER, "whisper.cpp")], Json(json!({"text":"Babel"}))).into_response()
                    } else {
                        probes.fetch_add(1, Ordering::SeqCst);
                        (
                            StatusCode::BAD_REQUEST,
                            [(SERVER, "whisper.cpp")],
                            Json(json!({"error":"no 'file' field in the request"})),
                        )
                            .into_response()
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/prefix/inference", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let config = AgentConfig {
        whisper_endpoint: endpoint,
        ..Default::default()
    };
    let inference = super::inference::Inference::new(&config).unwrap();
    assert_eq!(inference.transcribe(&[1000; 1600]).await.unwrap(), "Babel");
    assert_eq!(inference.transcribe(&[1000; 1600]).await.unwrap(), "Babel");
    assert_eq!(probes.load(Ordering::SeqCst), 1);
    assert_eq!(captured.load(Ordering::SeqCst), 2);
    server.abort();
}

#[tokio::test]
async fn localhost_inference_services_keep_their_separate_ports() {
    let asr_router = Router::new().route("/health", get(whisper_health)).route(
        "/inference",
        post(|body: axum::body::Bytes| async move {
            if body.windows(4).any(|part| part == b"RIFF") {
                (
                    [(SERVER, "whisper.cpp")],
                    Json(json!({"text":"Babel weather"})),
                )
                    .into_response()
            } else {
                (
                    StatusCode::BAD_REQUEST,
                    [(SERVER, "whisper.cpp")],
                    "Invalid request",
                )
                    .into_response()
            }
        }),
    );
    let needle_router=Router::new().route("/complete",post(|Json(request):Json<Value>|async move {
        Json(json!({"success":true,"type":"call","confidence":0.9,"function_calls":[{"name":request["tools"][0]["name"],"arguments":{"room":"kitchen"}}]}))
    }));
    let asr_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let needle_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = AgentConfig {
        whisper_endpoint: format!(
            "http://localhost:{}/inference",
            asr_listener.local_addr().unwrap().port()
        ),
        needle_endpoint: format!(
            "http://localhost:{}/complete",
            needle_listener.local_addr().unwrap().port()
        ),
        ..Default::default()
    };
    let asr_server = tokio::spawn(async move {
        axum::serve(asr_listener, asr_router).await.unwrap();
    });
    let needle_server = tokio::spawn(async move {
        axum::serve(needle_listener, needle_router).await.unwrap();
    });
    let inference = super::inference::Inference::new(&config).unwrap();
    assert_eq!(
        inference.transcribe(&[1000; 1600]).await.unwrap(),
        "Babel weather"
    );
    assert_eq!(
        inference.plan("lights on", &[tool()]).await.unwrap()[0].arguments["room"],
        "kitchen"
    );
    asr_server.abort();
    needle_server.abort();
}
