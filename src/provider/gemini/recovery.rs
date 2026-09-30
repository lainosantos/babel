//! Finite recognition for original PCM that Live Transcribe did not acknowledge.
//! Uses the separate, documented gemini-3.5-transcribe Interactions model.
//! https://ai.google.dev/gemini-api/docs/transcribe
//! https://ai.google.dev/gemini-api/docs/audio#pass-audio-data-inline
use super::{Failure, SessionResult};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
use reqwest::{Client, header::HeaderValue};
use serde_json::{Value, json};
use std::{borrow::Cow, io::Cursor, time::Duration};
use zeroize::Zeroizing;

const MODEL: &str = "gemini-3.5-transcribe";
pub(super) const ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/interactions";
const MAX_SAMPLES: usize = 96_000;
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_TEXT_BYTES: usize = 32_768;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

/// The caller supplies a client with redirects disabled and a fixed production
/// endpoint. Only loopback fixtures may inject another endpoint. No raw HTTP
/// errors, response bodies, credentials, or speech enter diagnostic messages.
pub(super) async fn recover(
    client: &Client,
    endpoint: &str,
    api_key: &str,
    language: &str,
    samples: &[i16],
) -> SessionResult<String> {
    if api_key.trim().is_empty() {
        return Err(Failure::fatal("Gemini recovery API key is empty"));
    }
    let mut key = HeaderValue::from_str(api_key.trim())
        .map_err(|_| Failure::fatal("Gemini recovery API key has invalid header characters"))?;
    key.set_sensitive(true);
    let payload = request_body(language, samples)?;
    tokio::time::timeout(REQUEST_TIMEOUT, async {
        let response = client
            .post(endpoint)
            .header("x-goog-api-key", key)
            .header("Api-Revision", "2026-05-20")
            .json(&payload)
            .send()
            .await
            .map_err(|_| Failure::retry("Gemini recovery request failed"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(Failure {
                message: Cow::Owned(format!(
                    "Gemini recovery returned HTTP {}; original audio was not acknowledged",
                    status.as_u16()
                )),
                retryable: matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504),
            });
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(Failure::fatal(
                "Gemini recovery response exceeds the memory limit",
            ));
        }
        let mut bytes = Zeroizing::new(Vec::new());
        let mut chunks = response.bytes_stream();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|_| Failure::retry("Gemini recovery response failed"))?;
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(Failure::fatal(
                    "Gemini recovery response exceeds the memory limit",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| Failure::fatal("Gemini recovery returned invalid JSON"))?;
        completed_text(&value)
    })
    .await
    .map_err(|_| Failure::retry("Gemini recovery timed out; original audio was not acknowledged"))?
}

fn request_body(language: &str, samples: &[i16]) -> SessionResult<Value> {
    if samples.is_empty() || samples.len() > MAX_SAMPLES {
        return Err(Failure::fatal(
            "Gemini recovery requires at most six seconds of nonempty PCM",
        ));
    }
    if language.len() > 64
        || !language
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || value == '-')
    {
        return Err(Failure::fatal("Gemini recovery language is invalid"));
    }
    let languages: Vec<&str> = if language.is_empty() || language == "auto" {
        vec![]
    } else {
        vec![language]
    };
    let mut wav = Cursor::new(Vec::with_capacity(samples.len() * 2 + 44));
    {
        let mut writer = hound::WavWriter::new(
            &mut wav,
            hound::WavSpec {
                channels: 1,
                sample_rate: 16_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .map_err(|_| Failure::fatal("Could not initialize Gemini recovery audio"))?;
        for sample in samples {
            writer
                .write_sample(*sample)
                .map_err(|_| Failure::fatal("Could not encode Gemini recovery audio"))?;
        }
        writer
            .finalize()
            .map_err(|_| Failure::fatal("Could not finalize Gemini recovery audio"))?;
    }
    let wav = Zeroizing::new(wav.into_inner());
    Ok(json!({
        "model": MODEL,
        "store": false,
        "stream": false,
        "input": [{"type":"audio", "mime_type":"audio/wav", "data":STANDARD.encode(wav.as_slice())}],
        "generation_config": {"transcription_config": {"language_codes":languages, "mode":{"type":"verbatim"}}}
    }))
}

fn completed_text(value: &Value) -> SessionResult<String> {
    if !value.is_object() || value.get("error").is_some() {
        return Err(Failure::fatal(
            "Gemini recovery did not return a completed transcription",
        ));
    }
    if value.get("status").and_then(Value::as_str) != Some("completed") {
        return Err(Failure::fatal(
            "Gemini recovery transcription is incomplete",
        ));
    }
    // An explicit completed response with an empty result is authoritative
    // completion without recognized words. Absence of a valid result container
    // is not interpreted as silence. Legacy `outputs` is deliberately rejected.
    let steps = value
        .get("steps")
        .and_then(Value::as_array)
        .ok_or_else(|| Failure::fatal("Gemini recovery returned invalid transcription results"))?;
    let mut text = String::new();
    for step in steps {
        // Interactions can include the original request in its step timeline.
        // Echoed input is never recognition output and must not be persisted.
        if step.get("type").and_then(Value::as_str) == Some("user_input") {
            continue;
        }
        if step.get("type").and_then(Value::as_str) != Some("model_output") {
            return Err(Failure::fatal(
                "Gemini recovery returned unexpected processing content",
            ));
        }
        let Some(content) = step.get("content") else {
            // ModelOutputStep.content is optional in the Interactions schema.
            continue;
        };
        let content = content.as_array().ok_or_else(|| {
            Failure::fatal("Gemini recovery returned invalid transcription content")
        })?;
        for block in content {
            if block.get("type").and_then(Value::as_str) != Some("text") {
                return Err(Failure::fatal(
                    "Gemini recovery returned non-transcription content",
                ));
            }
            let fragment = block.get("text").and_then(Value::as_str).ok_or_else(|| {
                Failure::fatal("Gemini recovery returned invalid transcript text")
            })?;
            if text.len().saturating_add(fragment.len()) > MAX_TEXT_BYTES {
                return Err(Failure::fatal(
                    "Gemini recovery transcript exceeds the memory limit",
                ));
            }
            text.push_str(fragment);
        }
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        body::Body,
        http::{HeaderMap, Response, StatusCode},
        routing::post,
    };
    use tokio::net::TcpListener;
    use tokio_util::task::AbortOnDropHandle;

    async fn serve(app: Router) -> (String, AbortOnDropHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/interactions", listener.local_addr().unwrap());
        let server = AbortOnDropHandle::new(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }));
        (endpoint, server)
    }

    fn client() -> Client {
        Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn sends_original_inline_wav_without_persistence_or_translation_and_returns_only_text() {
        let samples = vec![i16::MIN, -1, 0, 1, i16::MAX];
        let expected = samples.clone();
        let app = Router::new().route("/interactions", post(move |headers: HeaderMap, Json(body): Json<Value>| {
            let expected = expected.clone();
            async move {
                assert_eq!(headers["x-goog-api-key"], "synthetic-key");
                assert_eq!(headers["api-revision"], "2026-05-20");
                assert_eq!(body["model"], MODEL);
                assert_eq!(body["store"], false);
                assert_eq!(body["stream"], false);
                assert!(body.get("system_instruction").is_none());
                assert_eq!(body["generation_config"]["transcription_config"], json!({"language_codes":["pt-BR"],"mode":{"type":"verbatim"}}));
                let inputs = body["input"].as_array().unwrap();
                assert_eq!(inputs.len(), 1);
                assert_eq!(inputs[0]["type"], "audio");
                assert_eq!(inputs[0]["mime_type"], "audio/wav");
                assert!(inputs[0].get("uri").is_none());
                let wav = STANDARD.decode(inputs[0]["data"].as_str().unwrap()).unwrap();
                let mut reader = hound::WavReader::new(Cursor::new(wav)).unwrap();
                assert_eq!(reader.spec().sample_rate, 16_000);
                assert_eq!(reader.spec().channels, 1);
                assert_eq!(reader.spec().bits_per_sample, 16);
                assert_eq!(reader.samples::<i16>().collect::<Result<Vec<_>, _>>().unwrap(), expected);
                Json(json!({"status":"completed","steps":[{"type":"model_output","content":[{"type":"text","text":"Original "},{"type":"text","text":"speech."}]}]}))
            }
        }));
        let (endpoint, _server) = serve(app).await;
        assert_eq!(
            recover(&client(), &endpoint, "synthetic-key", "pt-BR", &samples)
                .await
                .unwrap(),
            "Original speech."
        );
    }

    #[tokio::test]
    async fn completed_empty_results_are_finite_success_without_invented_text() {
        for steps in [
            json!([]),
            json!([{"type":"model_output"}]),
            json!([{"type":"model_output","content":[]}]),
            json!([{"type":"model_output","content":[{"type":"text","text":""}]}]),
        ] {
            let app = Router::new().route(
                "/interactions",
                post(move || {
                    let steps = steps.clone();
                    async move { Json(json!({"status":"completed","steps":steps})) }
                }),
            );
            let (endpoint, _server) = serve(app).await;
            assert_eq!(
                recover(&client(), &endpoint, "synthetic-key", "auto", &[1; 1600])
                    .await
                    .unwrap(),
                ""
            );
        }
    }

    #[test]
    fn echoed_user_input_never_becomes_a_recognized_transcript() {
        let value = json!({
            "status":"completed",
            "steps":[
                {"type":"user_input","content":[{"type":"text","text":"Unrecognized echoed input"}]},
                {"type":"model_output","content":[{"type":"text","text":"Recognized original."}]}
            ]
        });
        assert_eq!(completed_text(&value).unwrap(), "Recognized original.");
    }

    #[test]
    fn incomplete_malformed_and_non_text_results_cannot_acknowledge_audio() {
        for value in [
            json!({}),
            json!({"status":"completed"}),
            json!({"status":"incomplete","steps":[]}),
            json!({"status":"failed","steps":[]}),
            json!({"status":"in_progress","steps":[]}),
            json!({"status":"requires_action","steps":[]}),
            json!({"status":"completed","steps":[],"error":{"message":"private"}}),
            json!({"status":"completed","outputs":[{"type":"text","text":"legacy"}]}),
            json!({"status":"completed","steps":[{"type":"function_call"}]}),
            json!({"status":"completed","steps":[{"type":"model_output","content":[{"type":"audio","data":"private"}]}]}),
            json!({"status":"completed","steps":[{"type":"model_output","content":[{"type":"text","text":null}]}]}),
            json!({"status":"completed","steps":[{"type":"model_output","content":[{"type":"text","text":"x".repeat(MAX_TEXT_BYTES+1)}]}]}),
        ] {
            let error = completed_text(&value).unwrap_err();
            assert!(!error.message.contains("private"));
        }
        for samples in [vec![], vec![1; MAX_SAMPLES + 1]] {
            assert!(request_body("auto", &samples).is_err());
        }
        let value = request_body("", &[1; MAX_SAMPLES]).unwrap();
        assert_eq!(
            value["generation_config"]["transcription_config"]["language_codes"],
            json!([])
        );
    }

    #[tokio::test]
    async fn http_errors_preserve_safe_status_without_server_payload_or_key() {
        for (status, retryable) in [
            (StatusCode::UNAUTHORIZED, false),
            (StatusCode::TOO_MANY_REQUESTS, true),
            (StatusCode::SERVICE_UNAVAILABLE, true),
        ] {
            let app = Router::new().route(
                "/interactions",
                post(move || async move { (status, "private speech and synthetic-key") }),
            );
            let (endpoint, _server) = serve(app).await;
            let error = recover(&client(), &endpoint, "synthetic-key", "auto", &[1])
                .await
                .unwrap_err();
            assert_eq!(error.retryable, retryable);
            assert!(error.message.contains(&status.as_u16().to_string()));
            assert!(!error.message.contains("private"));
            assert!(!error.message.contains("synthetic-key"));
        }
    }

    #[tokio::test]
    async fn response_memory_limit_covers_declared_and_streamed_lengths() {
        for streamed in [false, true] {
            let app = Router::new().route(
                "/interactions",
                post(move || async move {
                    let data = vec![b'x'; MAX_RESPONSE_BYTES + 1];
                    let body = if streamed {
                        Body::from_stream(futures_util::stream::iter([Ok::<_, std::io::Error>(
                            data,
                        )]))
                    } else {
                        Body::from(data)
                    };
                    Response::new(body)
                }),
            );
            let (endpoint, _server) = serve(app).await;
            let error = recover(&client(), &endpoint, "synthetic-key", "auto", &[1])
                .await
                .unwrap_err();
            assert!(error.message.contains("memory limit"));
        }
    }

    #[tokio::test]
    async fn response_body_stalls_share_the_bounded_request_deadline() {
        let entered = std::sync::Arc::new(tokio::sync::Notify::new());
        let signal = entered.clone();
        let app = Router::new().route(
            "/interactions",
            post(move || {
                let signal = signal.clone();
                async move {
                    signal.notify_one();
                    Response::new(Body::from_stream(futures_util::stream::pending::<
                        Result<Vec<u8>, std::io::Error>,
                    >()))
                }
            }),
        );
        let (endpoint, _server) = serve(app).await;
        let task = tokio::spawn(async move {
            recover(&client(), &endpoint, "synthetic-key", "auto", &[1]).await
        });
        entered.notified().await;
        tokio::time::pause();
        tokio::time::advance(REQUEST_TIMEOUT + Duration::from_millis(1)).await;
        let error = task.await.unwrap().unwrap_err();
        assert!(error.message.contains("timed out"));
        assert!(error.retryable);
    }
}
