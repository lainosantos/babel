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
    for model in [TRANSLATE_MODEL, "gemini-3.8-live"] {
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
        pending(&mut task).await;
        send(
            &mut server,
            json!({"inputTranscription":{"text":"Original final."},"turnComplete":true}),
        )
        .await;
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
        for input in [false, true] {
            assert!(
                matches!(event(&mut events).await, ProviderEvent::Transcript { input: actual, .. } if actual == input)
            );
        }
        assert_eq!(event(&mut events).await, ProviderEvent::TurnComplete);
        assert!(events.recv().await.is_none());
    }
}

#[tokio::test]
async fn empty_and_silent_eof_finish_without_waiting_for_a_generation() {
    for model in [TRANSLATE_MODEL, "gemini-3.8-live"] {
        for input in [None, Some(Vec::new()), Some(vec![0; 480])] {
            let (socket, mut server) = sockets().await;
            let (audio, receiver) = mpsc::channel(1);
            if let Some(samples) = &input {
                audio.send(samples.clone()).await.unwrap();
            }
            drop(audio);
            let (task, mut events) = spawn(socket, receiver, config(model));
            if let Some(samples) = &input
                && !samples.is_empty()
            {
                assert_eq!(input_samples(&read(&mut server).await), *samples);
            }
            timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(events.recv().await.is_none());
        }
    }
}

#[tokio::test]
async fn inactivity_final_cannot_acknowledge_the_next_queued_stream() {
    let (socket, mut server) = sockets().await;
    let (audio, receiver) = mpsc::channel(1);
    let (mut task, mut events) = spawn(socket, receiver, config(TRANSLATE_MODEL));
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
    assert!(
        timeout(Duration::from_millis(30), server.next())
            .await
            .is_err()
    );
    send(&mut server, json!({"turnComplete":true})).await;
    assert_eq!(input_samples(&read(&mut server).await), vec![9000; 17]);
    assert_eq!(
        read(&mut server).await["realtimeInput"]["audioStreamEnd"],
        true
    );
    pending(&mut task).await;
    send(&mut server, output_audio(&[19, -20])).await;
    send(&mut server, json!({"generationComplete":true})).await;
    pending(&mut task).await;
    send(&mut server, json!({"turnComplete":true})).await;
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
async fn old_turn_complete_cannot_finish_audio_submitted_after_generation_complete() {
    let (socket, mut server) = sockets().await;
    let (audio, receiver) = mpsc::channel(1);
    let (mut task, mut events) = spawn(socket, receiver, config(TRANSLATE_MODEL));
    audio.send(vec![5000; 160]).await.unwrap();
    assert_eq!(input_samples(&read(&mut server).await), vec![5000; 160]);
    // Receiving the accompanying transcript proves that the generation marker
    // was processed before the later PCM is submitted to the socket.
    send(
        &mut server,
        json!({"generationComplete":true,"outputTranscription":{"text":"First generated."}}),
    )
    .await;
    assert!(matches!(
        event(&mut events).await,
        ProviderEvent::Transcript { input: false, .. }
    ));
    audio.send(vec![9000; 17]).await.unwrap();
    assert_eq!(input_samples(&read(&mut server).await), vec![9000; 17]);
    drop(audio);
    assert_eq!(
        read(&mut server).await["realtimeInput"]["audioStreamEnd"],
        true
    );
    send(&mut server, json!({"turnComplete":true})).await;
    assert_eq!(event(&mut events).await, ProviderEvent::TurnComplete);
    pending(&mut task).await;
    send(&mut server, output_audio(&[21, -22])).await;
    send(
        &mut server,
        json!({"generationComplete":true,"turnComplete":true}),
    )
    .await;
    timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        event(&mut events).await,
        ProviderEvent::Audio {
            samples: vec![21, -22],
            sample_rate: 24000
        }
    );
    assert_eq!(event(&mut events).await, ProviderEvent::TurnComplete);
    assert!(events.recv().await.is_none());
}

#[tokio::test]
async fn socket_close_before_turn_complete_fails_without_replaying_consumed_audio() {
    let (socket, mut server) = sockets().await;
    let (audio, receiver) = mpsc::channel(1);
    audio.send(vec![5000; 7]).await.unwrap();
    drop(audio);
    let (task, mut events) = spawn(socket, receiver, config(TRANSLATE_MODEL));
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
    assert!(!failure.retryable);
    assert!(events.recv().await.is_none());
}

#[tokio::test]
async fn missing_turn_complete_times_out_despite_nonfinal_audio_and_text() {
    let (socket, mut server) = sockets().await;
    let (audio, receiver) = mpsc::channel(1);
    audio.send(vec![5000; 7]).await.unwrap();
    drop(audio);
    let (task, mut events) = spawn(socket, receiver, config(TRANSLATE_MODEL));
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
