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
    let task = tokio::spawn(async move { run(socket, &mut audio, &events, &config).await });
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

async fn send(server: &mut Server, value: Value) {
    server
        .send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
}

fn pcm(value: &Value) -> Vec<i16> {
    STANDARD
        .decode(value["audio"].as_str().unwrap())
        .unwrap()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|bytes| i16::from_le_bytes(*bytes))
        .collect()
}

async fn pending(task: &mut JoinHandle<SessionResult<()>>) {
    assert!(
        timeout(Duration::from_millis(30), task).await.is_err(),
        "translation finished before its final acknowledgement"
    );
}

#[tokio::test]
async fn translation_eof_flushes_resampler_and_drains_delayed_audio_and_text() {
    let (socket, mut server) = sockets().await;
    let (audio, receiver) = mpsc::channel(2);
    audio.send(vec![9000; 480]).await.unwrap();
    audio.send(vec![16000; 17]).await.unwrap();
    drop(audio);
    let (mut task, mut events) = spawn(socket, receiver, config(TRANSLATION_MODEL));
    let mut submitted = Vec::new();
    loop {
        let value = read(&mut server).await;
        if value["type"] == "session.close" {
            break;
        }
        assert_eq!(value["type"], "session.input_audio_buffer.append");
        submitted.extend(pcm(&value));
    }
    assert_eq!(submitted.len(), 497 * 3 / 2);
    assert!(
        submitted.last().unwrap().abs() > 1000,
        "final speech samples were lost"
    );
    pending(&mut task).await;
    send(
        &mut server,
        json!({"type":"session.output_audio.delta","delta":STANDARD.encode([1, 0, 254, 255])}),
    )
    .await;
    send(
        &mut server,
        json!({"type":"session.input_transcript.delta","delta":"Original final."}),
    )
    .await;
    send(
        &mut server,
        json!({"type":"session.output_transcript.delta","delta":"Translated final."}),
    )
    .await;
    pending(&mut task).await;
    send(&mut server, json!({"type":"session.closed"})).await;
    timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        events.recv().await.unwrap(),
        ProviderEvent::Audio {
            samples: vec![1, -2],
            sample_rate: 24000
        }
    );
    for input in [true, false] {
        assert!(
            matches!(events.recv().await.unwrap(), ProviderEvent::Transcript { input: actual, .. } if actual == input)
        );
    }
    assert_eq!(events.recv().await.unwrap(), ProviderEvent::TurnComplete);
    assert!(events.recv().await.is_none());
}

#[tokio::test]
async fn translation_socket_close_without_session_closed_is_an_error() {
    let (socket, mut server) = sockets().await;
    let (audio, receiver) = mpsc::channel(1);
    audio.send(vec![5000; 7]).await.unwrap();
    drop(audio);
    let (task, mut events) = spawn(socket, receiver, config(TRANSLATION_MODEL));
    let value = read(&mut server).await;
    assert_eq!(value["type"], "session.input_audio_buffer.append");
    assert_eq!(pcm(&value).len(), 7 * 3 / 2);
    assert_eq!(read(&mut server).await["type"], "session.close");
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
async fn empty_translation_session_sends_close_without_padding_audio() {
    let (socket, mut server) = sockets().await;
    let (audio, receiver) = mpsc::channel(1);
    drop(audio);
    let (mut task, mut events) = spawn(socket, receiver, config(TRANSLATION_MODEL));
    assert_eq!(read(&mut server).await["type"], "session.close");
    pending(&mut task).await;
    send(&mut server, json!({"type":"session.closed"})).await;
    timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(events.recv().await.unwrap(), ProviderEvent::TurnComplete);
    assert!(events.recv().await.is_none());
}

#[tokio::test]
async fn realtime_drains_every_queued_turn_and_waits_for_late_input_transcription() {
    let (socket, mut server) = sockets().await;
    let (audio, receiver) = mpsc::channel(4);
    for _ in 0..3 {
        let mut segment = vec![6000; 160];
        segment.extend(vec![0; 1600]);
        audio.send(segment).await.unwrap();
    }
    audio.send(vec![12000; 37]).await.unwrap();
    drop(audio);
    let (mut task, mut events) = spawn(socket, receiver, config("gpt-realtime-2.1"));
    for index in 0..4 {
        let mut count = 0;
        loop {
            let value = read(&mut server).await;
            if value["type"] == "input_audio_buffer.commit" {
                break;
            }
            assert_eq!(value["type"], "input_audio_buffer.append");
            count += pcm(&value).len();
        }
        assert_eq!(count, if index == 3 { 2400 } else { 2640 });
        assert!(
            timeout(Duration::from_millis(20), server.next())
                .await
                .is_err(),
            "response creation must follow the matching commit acknowledgement"
        );
        let item = format!("item-{index}");
        let response = format!("response-{index}");
        send(
            &mut server,
            json!({"type":"input_audio_buffer.committed","item_id":item}),
        )
        .await;
        assert_eq!(read(&mut server).await["type"], "response.create");
        send(
            &mut server,
            json!({"type":"response.created","response":{"id":response}}),
        )
        .await;
        send(
            &mut server,
            json!({"type":"response.output_audio.delta","delta":STANDARD.encode([index as u8, 0])}),
        )
        .await;
        send(
            &mut server,
            json!({"type":"response.done","response":{"id":response,"status":"completed"}}),
        )
        .await;
        pending(&mut task).await;
        assert!(
            timeout(Duration::from_millis(20), server.next())
                .await
                .is_err(),
            "later queued audio must wait for the previous input transcript"
        );
        send(&mut server, json!({"type":"conversation.item.input_audio_transcription.completed","item_id":item,"transcript":format!("Original {index}.")})).await;
    }
    timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    for index in 0..4 {
        assert_eq!(
            events.recv().await.unwrap(),
            ProviderEvent::Audio {
                samples: vec![index],
                sample_rate: 24000
            }
        );
        assert!(
            matches!(events.recv().await.unwrap(), ProviderEvent::Transcript {
            input: true, text, metadata
        } if text == format!("Original {index}.") && metadata.alignment_ms == Some(index as u64 * 110))
        );
        assert_eq!(events.recv().await.unwrap(), ProviderEvent::TurnComplete);
    }
    assert!(events.recv().await.is_none());
}

#[tokio::test]
async fn empty_realtime_session_finishes_without_a_response() {
    let (socket, _server) = sockets().await;
    let (audio, receiver) = mpsc::channel(1);
    drop(audio);
    let (task, mut events) = spawn(socket, receiver, config("gpt-realtime-2.1"));
    timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(events.recv().await.is_none());
}

#[tokio::test]
async fn missing_session_closed_times_out_despite_nonfinal_audio_and_text() {
    let (socket, mut server) = sockets().await;
    let (audio, receiver) = mpsc::channel(1);
    audio.send(vec![5000; 7]).await.unwrap();
    drop(audio);
    let (task, mut events) = spawn(socket, receiver, config(TRANSLATION_MODEL));
    assert_eq!(
        read(&mut server).await["type"],
        "session.input_audio_buffer.append"
    );
    assert_eq!(read(&mut server).await["type"], "session.close");

    // Advance only after the real socket exchange, then resume while doing
    // network I/O so automatic clock advancement cannot race those reads.
    tokio::time::pause();
    tokio::time::advance(FINAL_TIMEOUT / 2).await;
    tokio::time::resume();
    send(
        &mut server,
        json!({"type":"session.output_audio.delta","delta":STANDARD.encode([3, 0])}),
    )
    .await;
    send(
        &mut server,
        json!({"type":"session.output_transcript.delta","delta":"Still translating."}),
    )
    .await;
    assert_eq!(
        timeout(Duration::from_secs(3), events.recv())
            .await
            .unwrap()
            .unwrap(),
        ProviderEvent::Audio {
            samples: vec![3],
            sample_rate: 24000
        }
    );
    assert!(matches!(
        timeout(Duration::from_secs(3), events.recv())
            .await
            .unwrap()
            .unwrap(),
        ProviderEvent::Transcript { input: false, .. }
    ));

    // Nonfinal output must neither acknowledge EOF nor extend its deadline.
    tokio::time::pause();
    tokio::time::advance(FINAL_TIMEOUT / 2 + Duration::from_millis(1)).await;
    let failure = timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(
        failure.message,
        "OpenAI translation final acknowledgement timed out"
    );
    assert!(!failure.retryable);
    assert!(events.recv().await.is_none());
}
