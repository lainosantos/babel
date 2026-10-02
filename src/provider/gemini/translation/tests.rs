use super::*;
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_tungstenite::{accept_async, connect_async};

type Server = WebSocketStream<TcpStream>;

fn config(model: &str) -> SessionConfig {
    SessionConfig {
        model: model.into(),
        api_key_env: "BABEL_TEST_CREDENTIALS_NOT_READ".into(),
        voice: String::new(),
        source_language: "pt-BR".into(),
        target_language: "en".into(),
        prompt: String::new(),
        vad_silence_ms: 100,
        connect_timeout_secs: 2,
        max_reconnect_attempts: 0,
        input_transcription: true,
        output_transcription: true,
    }
}

async fn sockets() -> (Socket, Server) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let (client, server) = tokio::join!(connect_async(endpoint), async {
        accept_async(listener.accept().await.unwrap().0)
            .await
            .unwrap()
    });
    (client.unwrap().0, server)
}

fn spawn(
    socket: Socket,
    mut audio: mpsc::Receiver<Vec<i16>>,
    config: SessionConfig,
) -> (JoinHandle<SessionResult<()>>, mpsc::Receiver<ProviderEvent>) {
    let (events, receiver) = mpsc::channel(32);
    let task =
        tokio::spawn(async move { run(socket, &mut audio, &events, &config, &mut None).await });
    (task, receiver)
}

async fn read(server: &mut Server) -> Value {
    let message = timeout(Duration::from_secs(3), server.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(&message.into_text().unwrap()).unwrap()
}

async fn send(server: &mut Server, content: Value) {
    server
        .send(Message::Text(
            json!({"serverContent":content}).to_string().into(),
        ))
        .await
        .unwrap();
}

async fn event(events: &mut mpsc::Receiver<ProviderEvent>) -> ProviderEvent {
    timeout(Duration::from_secs(3), events.recv())
        .await
        .unwrap()
        .unwrap()
}

async fn pending(task: &mut JoinHandle<SessionResult<()>>) {
    assert!(
        timeout(Duration::from_millis(30), task).await.is_err(),
        "translation finished before its final acknowledgement"
    );
}

fn output_audio(samples: &[i16]) -> Value {
    let bytes: Vec<_> = samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect();
    json!({"modelTurn":{"parts":[{"inlineData":{
        "mimeType":"audio/pcm;rate=24000", "data":STANDARD.encode(bytes)
    }}]}})
}

fn input_samples(value: &Value) -> Vec<i16> {
    STANDARD
        .decode(value["realtimeInput"]["audio"]["data"].as_str().unwrap())
        .unwrap()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|bytes| i16::from_le_bytes(*bytes))
        .collect()
}

#[tokio::test]
async fn eof_sends_audio_stream_end_and_waits_for_delayed_tail_and_turn_complete() {
    let model = "gemini-3.8-live";
    let (socket, mut server) = sockets().await;
    let (audio, receiver) = mpsc::channel(1);
    audio.send(vec![7000; 37]).await.unwrap();
    drop(audio);
    let (mut task, mut events) = spawn(socket, receiver, config(model));
    assert_eq!(input_samples(&read(&mut server).await), vec![7000; 37]);
    assert_eq!(
        read(&mut server).await["realtimeInput"]["audioStreamEnd"],
        true
    );
    pending(&mut task).await;
    send(&mut server, output_audio(&[7, -8])).await;
    send(
        &mut server,
        json!({"outputTranscription":{"text":"Translated final."}}),
    )
    .await;
    send(&mut server, json!({"generationComplete":true})).await;
    if model != TRANSLATE_MODEL {
        pending(&mut task).await;
        send(
            &mut server,
            json!({"inputTranscription":{"text":"Original final."},"turnComplete":true}),
        )
        .await;
    }
    timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        event(&mut events).await,
        ProviderEvent::Audio {
            samples: vec![7, -8],
            sample_rate: 24000
        }
    );
    let expected_inputs: &[bool] = if model == TRANSLATE_MODEL {
        &[false]
    } else {
        &[false, true]
    };
    for &input in expected_inputs {
        assert!(
            matches!(event(&mut events).await, ProviderEvent::Transcript { input: actual, .. } if actual == input)
        );
    }
    assert_eq!(event(&mut events).await, ProviderEvent::TurnComplete);
    assert!(events.recv().await.is_none());
}

#[tokio::test]
async fn inactivity_final_cannot_acknowledge_the_next_queued_stream() {
    let model = "gemini-3.8-live";
    let (socket, mut server) = sockets().await;
    let (audio, receiver) = mpsc::channel(1);
    let (mut task, mut events) = spawn(socket, receiver, config(model));
    audio.send(vec![5000; 160]).await.unwrap();
    assert_eq!(input_samples(&read(&mut server).await), vec![5000; 160]);
    assert_eq!(
        read(&mut server).await["realtimeInput"]["audioStreamEnd"],
        true
    );
    audio.send(vec![9000; 17]).await.unwrap();
    drop(audio);
    assert!(
        timeout(Duration::from_millis(30), server.next())
            .await
            .is_err(),
        "a second stream must wait for the first stream's terminal acknowledgement"
    );
    send(&mut server, json!({"generationComplete":true})).await;
    if model != TRANSLATE_MODEL {
        assert!(
            timeout(Duration::from_millis(30), server.next())
                .await
                .is_err()
        );
        send(&mut server, json!({"turnComplete":true})).await;
    }
    assert_eq!(input_samples(&read(&mut server).await), vec![9000; 17]);
    assert_eq!(
        read(&mut server).await["realtimeInput"]["audioStreamEnd"],
        true
    );
    pending(&mut task).await;
    send(&mut server, output_audio(&[19, -20])).await;
    send(&mut server, json!({"generationComplete":true})).await;
    if model != TRANSLATE_MODEL {
        pending(&mut task).await;
        send(&mut server, json!({"turnComplete":true})).await;
    }
    timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(event(&mut events).await, ProviderEvent::TurnComplete);
    assert_eq!(
        event(&mut events).await,
        ProviderEvent::Audio {
            samples: vec![19, -20],
            sample_rate: 24000
        }
    );
    assert_eq!(event(&mut events).await, ProviderEvent::TurnComplete);
    assert!(events.recv().await.is_none());
}

#[tokio::test]
async fn socket_close_preserves_retryability_only_for_engine_owned_recovery() {
    for reconnect_budget in [0, 3] {
        let (socket, mut server) = sockets().await;
        let (audio, receiver) = mpsc::channel(1);
        audio.send(vec![5000; 7]).await.unwrap();
        drop(audio);
        let mut settings = config(TRANSLATE_MODEL);
        settings.max_reconnect_attempts = reconnect_budget;
        let (task, mut events) = spawn(socket, receiver, settings);
        assert_eq!(input_samples(&read(&mut server).await), vec![5000; 7]);
        assert_eq!(
            read(&mut server).await["realtimeInput"]["audioStreamEnd"],
            true
        );
        server.close(None).await.unwrap();
        let failure = timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(failure.retryable, reconnect_budget == 0);
        assert!(events.recv().await.is_none());
    }
}

#[tokio::test]
async fn missing_turn_complete_times_out_despite_nonfinal_audio_and_text() {
    let (socket, mut server) = sockets().await;
    let (audio, receiver) = mpsc::channel(1);
    audio.send(vec![5000; 7]).await.unwrap();
    drop(audio);
    let (task, mut events) = spawn(socket, receiver, config("gemini-3.8-live"));
    assert_eq!(input_samples(&read(&mut server).await), vec![5000; 7]);
    assert_eq!(
        read(&mut server).await["realtimeInput"]["audioStreamEnd"],
        true
    );

    // Keep actual socket I/O on the real clock; only the provider's remaining
    // acknowledgement deadline needs simulated time.
    tokio::time::pause();
    tokio::time::advance(FINAL_TIMEOUT / 2).await;
    tokio::time::resume();
    send(&mut server, output_audio(&[3, -4])).await;
    send(
        &mut server,
        json!({"generationComplete":true,"outputTranscription":{"text":"Still translating."}}),
    )
    .await;
    assert_eq!(
        event(&mut events).await,
        ProviderEvent::Audio {
            samples: vec![3, -4],
            sample_rate: 24000
        }
    );
    assert!(matches!(
        event(&mut events).await,
        ProviderEvent::Transcript { input: false, .. }
    ));

    tokio::time::pause();
    tokio::time::advance(FINAL_TIMEOUT / 2 + Duration::from_millis(1)).await;
    let failure = timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(
        failure.message,
        "Gemini translation final acknowledgement timed out"
    );
    assert!(!failure.retryable);
    assert!(events.recv().await.is_none());
}

#[tokio::test]
async fn residual_noise_after_generation_complete_does_not_create_an_unacknowledged_turn() {
    let (socket, mut server) = sockets().await;
    let (audio, receiver) = mpsc::channel(1);
    let (mut task, mut events) = spawn(socket, receiver, config("gemini-3.8-live"));
    audio.send(vec![5000; 160]).await.unwrap();
    assert_eq!(input_samples(&read(&mut server).await), vec![5000; 160]);
    send(
        &mut server,
        json!({"generationComplete":true,"outputTranscription":{"text":"Completed speech."}}),
    )
    .await;
    assert!(matches!(
        event(&mut events).await,
        ProviderEvent::Transcript { input: false, .. }
    ));
    let noise = [2, -2].repeat(240);
    audio.send(noise.clone()).await.unwrap();
    assert_eq!(input_samples(&read(&mut server).await), noise);
    drop(audio);
    assert_eq!(
        read(&mut server).await["realtimeInput"]["audioStreamEnd"],
        true
    );
    pending(&mut task).await;
    send(&mut server, json!({"turnComplete":true})).await;
    timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(event(&mut events).await, ProviderEvent::TurnComplete);
    assert!(events.recv().await.is_none());
}

// Finish real socket exchanges before advancing only the provider timers.
async fn advance(duration: Duration) {
    tokio::task::yield_now().await;
    tokio::time::pause();
    tokio::time::advance(duration).await;
    tokio::time::resume();
}

#[tokio::test]
async fn interpreter_drains_delayed_audio_without_terminal_model_markers() {
    let (socket, mut server) = sockets().await;
    let (audio, receiver) = mpsc::channel(1);
    audio.send(vec![7000; 37]).await.unwrap();
    drop(audio);
    let (mut task, mut events) = spawn(socket, receiver, config(TRANSLATE_MODEL));
    read(&mut server).await;
    assert_eq!(
        read(&mut server).await["realtimeInput"]["audioStreamEnd"],
        true
    );
    // The real service supplied neither generationComplete nor turnComplete.
    await_tail(&mut task, &mut server, &mut events).await;
}

async fn await_tail(
    task: &mut JoinHandle<SessionResult<()>>,
    server: &mut Server,
    events: &mut mpsc::Receiver<ProviderEvent>,
) {
    advance(interpreter::QUIET_TAIL - Duration::from_millis(100)).await;
    pending(task).await;
    send(server, output_audio(&[700, -800])).await;
    assert!(matches!(event(events).await, ProviderEvent::Audio { .. }));
    advance(interpreter::QUIET_TAIL - Duration::from_millis(100)).await;
    pending(task).await;
    // Continuous zero PCM does not count as unfinished speech or refill playback.
    send(server, output_audio(&vec![0; 6000])).await;
    send(
        server,
        json!({"inputTranscription":{"text":"Original tail."}}),
    )
    .await;
    assert!(matches!(
        event(events).await,
        ProviderEvent::Transcript { .. }
    ));
    advance(Duration::from_millis(200)).await;
    timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(event(events).await, ProviderEvent::TurnComplete);
    assert!(events.recv().await.is_none());
}

#[tokio::test]
async fn interpreter_pause_does_not_block_new_input_waiting_for_a_nonexistent_final() {
    let (socket, mut server) = sockets().await;
    let (audio, receiver) = mpsc::channel(1);
    let (mut task, mut events) = spawn(socket, receiver, config(TRANSLATE_MODEL));
    audio.send(vec![5000; 160]).await.unwrap();
    read(&mut server).await;
    assert_eq!(
        read(&mut server).await["realtimeInput"]["audioStreamEnd"],
        true
    );
    audio.send(vec![9000; 17]).await.unwrap();
    assert_eq!(input_samples(&read(&mut server).await), vec![9000; 17]);
    drop(audio);
    assert_eq!(
        read(&mut server).await["realtimeInput"]["audioStreamEnd"],
        true
    );
    await_tail(&mut task, &mut server, &mut events).await;
}

#[tokio::test]
async fn interpreter_silent_input_observes_a_tail_without_requesting_a_model_final() {
    for samples in [
        vec![],
        vec![0; 480],
        [2, -2].repeat(240),
        vec![17; 256 * 32],
    ] {
        let (socket, mut server) = sockets().await;
        let (audio, receiver) = mpsc::channel(1);
        audio.send(samples.clone()).await.unwrap();
        drop(audio);
        let (task, mut events) = spawn(socket, receiver, config(TRANSLATE_MODEL));
        if !samples.is_empty() {
            assert_eq!(input_samples(&read(&mut server).await), samples);
            assert_eq!(
                read(&mut server).await["realtimeInput"]["audioStreamEnd"],
                true
            );
            advance(interpreter::QUIET_TAIL + Duration::from_millis(1)).await;
        }
        timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if !samples.is_empty() {
            assert_eq!(event(&mut events).await, ProviderEvent::TurnComplete);
        }
        assert!(events.recv().await.is_none());
    }
}

#[tokio::test]
async fn interpreter_silence_and_translated_text_cannot_acknowledge_missing_audio() {
    let (socket, mut server) = sockets().await;
    let (audio, receiver) = mpsc::channel(1);
    audio.send(vec![7000; 37]).await.unwrap();
    drop(audio);
    let (mut task, mut events) = spawn(socket, receiver, config(TRANSLATE_MODEL));
    read(&mut server).await;
    read(&mut server).await;
    send(&mut server, output_audio(&vec![0; 6000])).await;
    send(
        &mut server,
        json!({"outputTranscription":{"text":"Text without delivered speech."}}),
    )
    .await;
    assert!(matches!(
        event(&mut events).await,
        ProviderEvent::Transcript { .. }
    ));
    advance(interpreter::QUIET_TAIL + Duration::from_millis(1)).await;
    pending(&mut task).await;
    advance(FINAL_TIMEOUT).await;
    let failure = timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(
        failure.message,
        "Gemini translation output drain timed out; original audio remains unconfirmed"
    );
    assert!(!failure.retryable);
    assert!(events.recv().await.is_none());
}

#[tokio::test]
async fn interpreter_old_output_does_not_cover_new_input_without_translated_audio() {
    let (socket, mut server) = sockets().await;
    let (audio, receiver) = mpsc::channel(1);
    let (mut task, mut events) = spawn(socket, receiver, config(TRANSLATE_MODEL));
    audio.send(vec![5000; 160]).await.unwrap();
    read(&mut server).await;
    send(&mut server, output_audio(&[700, -800])).await;
    assert!(matches!(
        event(&mut events).await,
        ProviderEvent::Audio { .. }
    ));
    // Prove the output was consumed before later speech becomes pending.
    tokio::task::yield_now().await;
    audio.send(vec![9000; 17]).await.unwrap();
    read(&mut server).await;
    drop(audio);
    read(&mut server).await;
    send(
        &mut server,
        json!({"generationComplete":true,"turnComplete":true}),
    )
    .await;
    assert_eq!(event(&mut events).await, ProviderEvent::TurnComplete);
    advance(interpreter::QUIET_TAIL + Duration::from_millis(1)).await;
    pending(&mut task).await;
    advance(FINAL_TIMEOUT).await;
    assert!(
        timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
}

#[tokio::test]
async fn interpreter_drain_has_a_fixed_bound_even_if_audible_output_never_stops() {
    let (socket, mut server) = sockets().await;
    let (audio, receiver) = mpsc::channel(1);
    audio.send(vec![7000; 37]).await.unwrap();
    drop(audio);
    let (task, mut events) = spawn(socket, receiver, config(TRANSLATE_MODEL));
    read(&mut server).await;
    read(&mut server).await;
    for _ in 0..10 {
        send(&mut server, output_audio(&[700, -800])).await;
        assert!(matches!(
            event(&mut events).await,
            ProviderEvent::Audio { .. }
        ));
        advance(Duration::from_secs(3)).await;
    }
    advance(Duration::from_millis(1)).await;
    assert!(
        timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
}

#[tokio::test]
async fn generic_old_turn_cannot_finish_new_audio_submitted_after_generation_complete() {
    let (socket, mut server) = sockets().await;
    let (audio, receiver) = mpsc::channel(1);
    let (mut task, mut events) = spawn(socket, receiver, config("gemini-3.8-live"));
    audio.send(vec![5000; 160]).await.unwrap();
    read(&mut server).await;
    send(
        &mut server,
        json!({"generationComplete":true,"outputTranscription":{"text":"First generated."}}),
    )
    .await;
    assert!(matches!(
        event(&mut events).await,
        ProviderEvent::Transcript { .. }
    ));
    tokio::task::yield_now().await;
    audio.send(vec![9000; 17]).await.unwrap();
    read(&mut server).await;
    send(&mut server, json!({"turnComplete":true})).await;
    assert_eq!(event(&mut events).await, ProviderEvent::TurnComplete);
    drop(audio);
    assert_eq!(
        read(&mut server).await["realtimeInput"]["audioStreamEnd"],
        true
    );
    pending(&mut task).await;
    send(
        &mut server,
        json!({"generationComplete":true,"turnComplete":true}),
    )
    .await;
    timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn interpreter_full_event_queue_cannot_extend_the_drain_deadline() {
    let (socket, mut server) = sockets().await;
    let (audio, mut receiver) = mpsc::channel(1);
    audio.send(vec![5000; 7]).await.unwrap();
    drop(audio);
    let (events, _unconsumed) = mpsc::channel(1);
    events.send(ProviderEvent::Connected).await.unwrap();
    let task = tokio::spawn(async move {
        run(
            socket,
            &mut receiver,
            &events,
            &config(TRANSLATE_MODEL),
            &mut None,
        )
        .await
    });
    read(&mut server).await;
    read(&mut server).await;
    send(&mut server, output_audio(&[700, -800])).await;
    // Real socket traffic must reach the blocked event delivery first.
    tokio::time::sleep(Duration::from_millis(30)).await;
    advance(FINAL_TIMEOUT + Duration::from_millis(1)).await;
    assert!(
        timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
}
