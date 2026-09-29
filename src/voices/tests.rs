use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

type Captured = tokio::task::JoinHandle<Vec<(String, Vec<u8>)>>;

async fn mock(responses: Vec<(&'static str, &'static str, Vec<u8>)>) -> (String, Captured) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut captured = Vec::new();
        for (status, mime, response) in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let end = loop {
                let mut buf = [0u8; 4096];
                let count = socket.read(&mut buf).await.unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buf[..count]);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    break end + 4;
                }
                assert!(bytes.len() < 32_768);
            };
            let headers = String::from_utf8(bytes[..end].to_vec()).unwrap();
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    line.split_once(':')
                        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map(|(_, value)| value.trim().parse().unwrap())
                })
                .unwrap_or(0);
            while bytes.len() - end < length {
                let mut buf = [0u8; 4096];
                let count = socket.read(&mut buf).await.unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buf[..count]);
            }
            captured.push((headers, bytes[end..end + length].to_vec()));
            let headers = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.len()
            );
            socket.write_all(headers.as_bytes()).await.unwrap();
            socket.write_all(&response).await.unwrap();
        }
        captured
    });
    (url, task)
}

fn api<'a>(provider: &'a str, base: &'a str) -> Api<'a> {
    Api {
        provider,
        base,
        key: Zeroizing::new("fake-voice-key".into()),
    }
}

fn config(provider: &str) -> SynthesisConfig {
    SynthesisConfig {
        provider: provider.into(),
        model: if provider == "gemini" {
            "gemini-3.8-flash-tts"
        } else {
            "eleven_flash_v2_5"
        }
        .into(),
        api_key_env: "UNUSED_TEST_KEY".into(),
        voice_id: "voice_selected".into(),
        style: String::new(),
        language: "pt-BR".into(),
    }
}

fn wav(seconds: u32) -> Vec<u8> {
    let data_len = seconds * 24_000 * 2;
    let mut result = b"RIFF".to_vec();
    result.extend_from_slice(&(36 + data_len).to_le_bytes());
    result.extend_from_slice(b"WAVEfmt ");
    result.extend_from_slice(&16u32.to_le_bytes());
    result.extend_from_slice(&1u16.to_le_bytes());
    result.extend_from_slice(&1u16.to_le_bytes());
    result.extend_from_slice(&24_000u32.to_le_bytes());
    result.extend_from_slice(&48_000u32.to_le_bytes());
    result.extend_from_slice(&2u16.to_le_bytes());
    result.extend_from_slice(&16u16.to_le_bytes());
    result.extend_from_slice(b"data");
    result.extend_from_slice(&data_len.to_le_bytes());
    result.resize(44 + data_len as usize, 0);
    result
}

#[tokio::test]
async fn voice_library_paginates_without_sending_key_in_url() {
    let (url, server) = mock(vec![
        ("200 OK", "application/json", br#"{"voices":[{"id":"voice_a","display_name":"A","type":"prompted"}],"next_page_token":"next"}"#.to_vec()),
        ("200 OK", "application/json", br#"{"voices":[{"id":"voice_b","display_name":"B","type":"replicated"}]}"#.to_vec()),
    ]).await;
    let voices = list_with_api(&api("gemini", &url)).await.unwrap();
    assert_eq!(voices.len(), 2);
    let requests = server.await.unwrap();
    assert!(requests[0].0.contains("x-goog-api-key: fake-voice-key"));
    assert!(
        !requests[0]
            .0
            .lines()
            .next()
            .unwrap()
            .contains("fake-voice-key")
    );
    assert!(
        requests[1]
            .0
            .lines()
            .next()
            .unwrap()
            .contains("page_token=next")
    );
}

#[tokio::test]
async fn eleven_design_saves_an_actual_returned_preview() {
    let (url, server) = mock(vec![
        (
            "200 OK",
            "application/json",
            br#"{"previews":[{"generated_voice_id":"generated_actual"}]}"#.to_vec(),
        ),
        (
            "200 OK",
            "application/json",
            br#"{"voice_id":"voice_saved","name":"Speaker","category":"generated"}"#.to_vec(),
        ),
    ])
    .await;
    let request = VoiceDesignRequest {
        provider: "elevenlabs".into(),
        api_key_env: "unused".into(),
        name: "Speaker".into(),
        description: "A calm and warm conversational voice".into(),
        language: "pt-BR".into(),
    };
    let voice = design_with_api(&api("elevenlabs", &url), &request)
        .await
        .unwrap();
    assert_eq!(voice.id, "voice_saved");
    let requests = server.await.unwrap();
    assert!(requests[0].0.starts_with("POST /v1/text-to-voice/design "));
    let body: Value = serde_json::from_slice(&requests[1].1).unwrap();
    assert_eq!(body["generated_voice_id"], "generated_actual");
}

#[tokio::test]
async fn clone_uploads_reference_and_dedicated_gemini_consent() {
    let (url, server) = mock(vec![(
        "200 OK",
        "application/json",
        br#"{"id":"voice_clone","display_name":"Clone","type":"replicated"}"#.to_vec(),
    )])
    .await;
    clone_with_api(&api("gemini", &url), "Clone", wav(10), Some(wav(3)))
        .await
        .unwrap();
    let requests = server.await.unwrap();
    let body: Value = serde_json::from_slice(&requests[0].1).unwrap();
    assert_eq!(body["voice"]["type"], "replicated");
    assert_eq!(body["store"], true);
    assert!(
        body["voice"]["replicated"]["consent_audio"]["data"]
            .as_str()
            .unwrap()
            .len()
            > 100
    );
}

#[tokio::test]
async fn eleven_clone_uses_multipart_and_reports_verification_requirement() {
    let (url, server) = mock(vec![(
        "200 OK",
        "application/json",
        br#"{"voice_id":"voice_clone","requires_verification":true}"#.to_vec(),
    )])
    .await;
    let result = clone_with_api(&api("elevenlabs", &url), "Clone", wav(1), None)
        .await
        .unwrap();
    assert_eq!(result.kind, "verification_required");
    let requests = server.await.unwrap();
    assert!(requests[0].0.contains("multipart/form-data"));
    assert!(requests[0].1.windows(12).any(|s| s == b"name=\"files\""));
}

#[tokio::test]
async fn gemini_tts_stream_uses_selected_voice_and_real_sse_audio() {
    let bytes: Vec<u8> = (0i16..1000).flat_map(|v| v.to_le_bytes()).collect();
    let body = format!(
        "event: step.delta\ndata: {}\n\nevent: interaction.completed\ndata: {{\"event_type\":\"interaction.completed\",\"interaction\":{{\"status\":\"completed\"}}}}\n\nevent: done\ndata: [DONE]\n\n",
        json!({
            "event_type":"step.delta", "delta":{"type":"audio","mime_type":"audio/l16","data":STANDARD.encode(bytes)}
        })
    );
    let (url, server) = mock(vec![("200 OK", "text/event-stream", body.into_bytes())]).await;
    let (tx, mut rx) = mpsc::channel(16);
    stream::synthesize_with_api(&api("gemini", &url), &config("gemini"), "Olá", tx)
        .await
        .unwrap();
    let mut samples = Vec::new();
    while let Some(chunk) = rx.recv().await {
        assert!(chunk.len() <= 480);
        samples.extend(chunk);
    }
    assert_eq!(samples, (0i16..1000).collect::<Vec<_>>());
    let requests = server.await.unwrap();
    let request: Value = serde_json::from_slice(&requests[0].1).unwrap();
    assert_eq!(
        request["generation_config"]["speech_config"][0]["voice"],
        "voice_selected"
    );
    assert_eq!(request["response_format"]["sample_rate"], 24_000);
    assert_eq!(request["input"][0]["content"][0]["text"], "Olá");
}

#[tokio::test]
async fn premature_sse_eof_and_wrong_raw_encoding_fail() {
    for (provider, mime, body) in [
        ("gemini", "text/event-stream", b"data: {}\n\n".to_vec()),
        ("elevenlabs", "audio/mpeg", b"fake mp3".to_vec()),
    ] {
        let (url, server) = mock(vec![("200 OK", mime, body)]).await;
        let (tx, _rx) = mpsc::channel(4);
        assert!(
            stream::synthesize_with_api(&api(provider, &url), &config(provider), "text", tx)
                .await
                .is_err()
        );
        server.await.unwrap();
    }
}

#[tokio::test]
async fn failures_do_not_echo_keys_and_cancel_does_not_require_credentials() {
    let (url, server) = mock(vec![(
        "401 Unauthorized",
        "application/json",
        br#"{"error":"fake-voice-key"}"#.to_vec(),
    )])
    .await;
    let error = list_with_api(&api("gemini", &url))
        .await
        .unwrap_err()
        .to_string();
    assert!(!error.contains("fake-voice-key"));
    assert!(error.contains("401"));
    server.await.unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let (tx, _rx) = mpsc::channel(1);
    synthesize(&config("gemini"), "text", tx, cancel)
        .await
        .unwrap();
}

#[test]
fn wav_validation_checks_format_lengths_and_duration() {
    assert_eq!(wav_duration_ms(&wav(10)).unwrap(), 10_000);
    let mut invalid = wav(1);
    invalid[22] = 2; // stereo not accepted for enrollment.
    assert!(wav_duration_ms(&invalid).is_err());
    assert!(wav_duration_ms(&wav(1)[..100]).is_err());
    assert!(decode_wav("invalid base64").is_err());
    assert!(validate_id("../../escape").is_err());
}

#[tokio::test]
async fn eleven_pcm_stream_preserves_selected_voice_and_language() {
    let bytes: Vec<u8> = [i16::MIN, -1, 0, 1, i16::MAX]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let (url, server) = mock(vec![("200 OK", "audio/pcm;rate=24000", bytes)]).await;
    let (tx, mut rx) = mpsc::channel(4);
    stream::synthesize_with_api(&api("elevenlabs", &url), &config("elevenlabs"), "Olá", tx)
        .await
        .unwrap();
    assert_eq!(rx.recv().await.unwrap(), [i16::MIN, -1, 0, 1, i16::MAX]);
    let requests = server.await.unwrap();
    assert!(
        requests[0]
            .0
            .starts_with("POST /v1/text-to-speech/voice_selected/stream?output_format=pcm_24000 ")
    );
    assert!(requests[0].0.contains("xi-api-key: fake-voice-key"));
    let request: Value = serde_json::from_slice(&requests[0].1).unwrap();
    assert_eq!(request["language_code"], "pt");
}

#[tokio::test]
async fn pagination_cycle_and_redirects_are_explicit_errors() {
    let (url, server) = mock(vec![("302 Found", "application/json", b"{}".to_vec())]).await;
    assert!(
        list_with_api(&api("gemini", &url))
            .await
            .unwrap_err()
            .to_string()
            .contains("302")
    );
    server.await.unwrap();
    let body = br#"{"voices":[],"has_more":true,"next_page_token":"repeat"}"#.to_vec();
    let (url, server) = mock(vec![
        ("200 OK", "application/json", body.clone()),
        ("200 OK", "application/json", body),
    ])
    .await;
    assert!(
        list_with_api(&api("elevenlabs", &url))
            .await
            .unwrap_err()
            .to_string()
            .contains("pagination")
    );
    server.await.unwrap();
}
