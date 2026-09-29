//! Explicit, authenticated voice-library operations and bounded streaming TTS.
//! Library creation is invoked by a user action; translation never enrolls voices.
mod stream;

use anyhow::{Result, anyhow, bail, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::{
    Client, Method, RequestBuilder, Response,
    header::{CONTENT_TYPE, HeaderValue},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashSet, sync::OnceLock, time::Duration};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

const GEMINI_BASE: &str = "https://generativelanguage.googleapis.com";
const ELEVEN_BASE: &str = "https://api.elevenlabs.io";
const MAX_JSON: usize = 8 * 1024 * 1024;
const MAX_UPLOAD: usize = 16 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoiceProfile {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub kind: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoiceDesignRequest {
    pub provider: String,
    pub api_key_env: String,
    pub name: String,
    pub description: String,
    pub language: String,
}

// Deliberately not Debug: these requests contain reference/consent recordings.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoiceCloneRequest {
    pub provider: String,
    pub api_key_env: String,
    pub name: String,
    pub reference_base64: String,
    #[serde(default)]
    pub consent_base64: String,
}

#[derive(Clone)]
pub struct SynthesisConfig {
    pub provider: String,
    pub model: String,
    pub api_key_env: String,
    pub voice_id: String,
    pub style: String,
    pub language: String,
}

fn shared_client() -> Result<&'static Client> {
    static CLIENT: OnceLock<std::result::Result<Client, ()>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(5))
                .timeout(REQUEST_TIMEOUT)
                .pool_max_idle_per_host(4)
                .build()
                .map_err(|_| ())
        })
        .as_ref()
        .map_err(|_| anyhow!("could not initialize the voice HTTP client"))
}

struct Api<'a> {
    provider: &'a str,
    base: &'a str,
    key: Zeroizing<String>,
}

impl<'a> Api<'a> {
    fn new(provider: &'a str, api_key_env: &str) -> Result<Self> {
        let base = match provider {
            "gemini" => GEMINI_BASE,
            "elevenlabs" => ELEVEN_BASE,
            _ => bail!("voice provider must be gemini or elevenlabs"),
        };
        ensure!(
            !api_key_env.is_empty()
                && api_key_env.len() <= 128
                && api_key_env
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_'),
            "invalid API-key environment variable name"
        );
        let key = crate::credentials::get(api_key_env)?;
        ensure!(!key.trim().is_empty(), "voice provider API key is empty");
        Ok(Self {
            provider,
            base,
            key,
        })
    }

    fn request(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        let mut value = HeaderValue::from_str(self.key.trim())
            .map_err(|_| anyhow!("invalid API-key header characters"))?;
        value.set_sensitive(true);
        Ok(shared_client()?
            .request(method, format!("{}{path}", self.base))
            .header(
                if self.provider == "gemini" {
                    "x-goog-api-key"
                } else {
                    "xi-api-key"
                },
                value,
            ))
    }
}

async fn send(request: RequestBuilder) -> Result<Response> {
    let response = request
        .send()
        .await
        .map_err(|_| anyhow!("voice provider request failed or timed out"))?;
    ensure!(
        response.status().is_success(),
        "voice provider returned HTTP {}; verify credentials, account access, quota and request settings",
        response.status().as_u16()
    );
    Ok(response)
}

async fn read_json(mut response: Response) -> Result<Value> {
    let mime = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    ensure!(
        mime.split(';')
            .next()
            .is_some_and(|m| m.trim() == "application/json"),
        "voice provider returned a non-JSON response"
    );
    if let Some(length) = response.content_length() {
        ensure!(
            length <= MAX_JSON as u64,
            "voice provider response exceeds memory limit"
        );
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow!("voice provider response was interrupted"))?
    {
        ensure!(
            bytes.len() + chunk.len() <= MAX_JSON,
            "voice provider response exceeds memory limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow!("voice provider returned invalid JSON"))?;
    ensure!(
        value.is_object() && value.get("error").is_none(),
        "voice provider rejected the request"
    );
    Ok(value)
}

fn validate_text(value: &str, max: usize, label: &'static str) -> Result<()> {
    ensure!(
        !value.trim().is_empty()
            && value.len() <= max
            && !value.chars().any(|c| c.is_control() && c != '\n'),
        "invalid {label}"
    );
    Ok(())
}

fn validate_id(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 256
            && value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c)),
        "invalid voice identifier"
    );
    Ok(())
}

fn profile(
    value: &Value,
    provider: &str,
    fallback_name: Option<&str>,
    fallback_kind: &str,
) -> Result<VoiceProfile> {
    let id = value
        .get(if provider == "gemini" {
            "id"
        } else {
            "voice_id"
        })
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("voice provider response has no voice identifier"))?;
    validate_id(id)?;
    let name = value
        .get(if provider == "gemini" {
            "display_name"
        } else {
            "name"
        })
        .and_then(Value::as_str)
        .or(fallback_name)
        .unwrap_or(id);
    validate_text(name, 256, "voice name")?;
    let kind = value
        .get(if provider == "gemini" {
            "type"
        } else {
            "category"
        })
        .and_then(Value::as_str)
        .unwrap_or(fallback_kind);
    validate_text(kind, 64, "voice category")?;
    // A verified-created profile can still require the provider's enrollment check.
    let kind = if value.get("requires_verification").and_then(Value::as_bool) == Some(true) {
        "verification_required"
    } else {
        kind
    };
    Ok(VoiceProfile {
        id: id.into(),
        name: name.into(),
        provider: provider.into(),
        kind: kind.into(),
    })
}

pub async fn list(provider: &str, api_key_env: &str) -> Result<Vec<VoiceProfile>> {
    list_with_api(&Api::new(provider, api_key_env)?).await
}

async fn list_with_api(api: &Api<'_>) -> Result<Vec<VoiceProfile>> {
    let mut voices = Vec::new();
    let mut token = String::new();
    let mut seen = HashSet::new();
    for _ in 0..100 {
        let mut request = api
            .request(
                Method::GET,
                if api.provider == "gemini" {
                    "/v1beta/voices"
                } else {
                    "/v2/voices"
                },
            )?
            .query(&[("page_size", "100")]);
        if !token.is_empty() {
            request = request.query(&[(
                if api.provider == "gemini" {
                    "page_token"
                } else {
                    "next_page_token"
                },
                &token,
            )]);
        }
        let value = read_json(send(request).await?).await?;
        let entries = value
            .get("voices")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("voice list response has no voices array"))?;
        ensure!(
            voices.len() + entries.len() <= 10_000,
            "voice library exceeds the supported pagination limit"
        );
        for entry in entries {
            voices.push(profile(entry, api.provider, None, "unknown")?);
        }
        let next = value
            .get("next_page_token")
            .and_then(Value::as_str)
            .unwrap_or("");
        let has_more = api.provider == "gemini" && !next.is_empty()
            || value.get("has_more").and_then(Value::as_bool) == Some(true);
        if !has_more {
            return Ok(voices);
        }
        ensure!(
            !next.is_empty() && next.len() <= 4096 && seen.insert(next.to_owned()),
            "voice provider returned invalid pagination"
        );
        token = next.into();
    }
    bail!("voice library exceeds the supported pagination limit")
}

pub async fn design(request: VoiceDesignRequest) -> Result<VoiceProfile> {
    validate_text(&request.name, 100, "voice name")?;
    validate_text(&request.description, 1000, "voice description")?;
    validate_text(&request.language, 32, "voice language")?;
    design_with_api(
        &Api::new(&request.provider, &request.api_key_env)?,
        &request,
    )
    .await
}

async fn design_with_api(api: &Api<'_>, request: &VoiceDesignRequest) -> Result<VoiceProfile> {
    let response = if api.provider == "gemini" {
        send(api.request(Method::POST, "/v1beta/voices")?.json(&json!({
            "store": true,
            "voice": {"model": "gemini-3.8-flash-tts", "type": "prompted", "display_name": request.name,
                "language_code": request.language, "prompted": {"input": request.description}}
        }))).await?
    } else {
        // Language is part of the description; the design endpoint has no
        // dedicated language field. Do not fabricate one.
        let description = format!("{}. Language: {}.", request.description, request.language);
        ensure!(
            (20..=1000).contains(&description.chars().count()),
            "ElevenLabs design description plus language must be 20..1000 characters"
        );
        let previews = read_json(send(api.request(Method::POST, "/v1/text-to-voice/design")?.json(&json!({
            "voice_description": description, "auto_generate_text": true, "stream_previews": true,
            "model_id": "eleven_multilingual_ttv_v2"
        }))).await?).await?;
        let generated = previews
            .pointer("/previews/0/generated_voice_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("ElevenLabs returned no generated voice preview"))?;
        validate_id(generated)?;
        send(api.request(Method::POST, "/v1/text-to-voice")?.json(&json!({
            "voice_name": request.name, "voice_description": description, "generated_voice_id": generated
        }))).await?
    };
    profile(
        &read_json(response).await?,
        api.provider,
        Some(&request.name),
        "prompted",
    )
}

pub async fn clone_voice(request: VoiceCloneRequest) -> Result<VoiceProfile> {
    validate_text(&request.name, 100, "voice name")?;
    let reference = decode_wav(&request.reference_base64)?;
    let duration = wav_duration_ms(&reference)?;
    let consent = if request.provider == "gemini" {
        ensure!(
            (10_000..=30_000).contains(&duration),
            "Gemini reference WAV must contain 10..30 seconds of clean speech"
        );
        let consent = decode_wav(&request.consent_base64)?;
        ensure!(
            (1_000..=60_000).contains(&wav_duration_ms(&consent)?),
            "Gemini consent WAV must contain 1..60 seconds with the provider's mandatory phrase"
        );
        Some(consent)
    } else {
        ensure!(
            (1_000..=300_000).contains(&duration),
            "ElevenLabs reference WAV must contain 1..300 seconds"
        );
        None
    };
    clone_with_api(
        &Api::new(&request.provider, &request.api_key_env)?,
        &request.name,
        reference,
        consent,
    )
    .await
}

async fn clone_with_api(
    api: &Api<'_>,
    name: &str,
    reference: Vec<u8>,
    consent: Option<Vec<u8>>,
) -> Result<VoiceProfile> {
    let request = if api.provider == "gemini" {
        let consent =
            consent.ok_or_else(|| anyhow!("Gemini requires a separate consent recording"))?;
        api.request(Method::POST, "/v1beta/voices")?.json(&json!({"store": true, "voice": {
            "model": "gemini-3.8-flash-tts", "type": "replicated", "display_name": name,
            "replicated": {"source_audio": {"mime_type": "audio/wav", "data": STANDARD.encode(reference)},
                "consent_audio": {"mime_type": "audio/wav", "data": STANDARD.encode(consent)}}
        }}))
    } else {
        let file = reqwest::multipart::Part::bytes(reference)
            .file_name("reference.wav")
            .mime_str("audio/wav")
            .map_err(|_| anyhow!("could not build voice upload"))?;
        api.request(Method::POST, "/v1/voices/add")?.multipart(
            reqwest::multipart::Form::new()
                .text("name", name.to_owned())
                .part("files", file),
        )
    };
    profile(
        &read_json(send(request).await?).await?,
        api.provider,
        Some(name),
        "replicated",
    )
}

fn decode_wav(encoded: &str) -> Result<Vec<u8>> {
    ensure!(
        encoded.len() <= MAX_UPLOAD.div_ceil(3) * 4,
        "voice recording exceeds the 16 MiB limit"
    );
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| anyhow!("voice recording is not valid Base64"))?;
    ensure!(
        bytes.len() <= MAX_UPLOAD,
        "voice recording exceeds the 16 MiB limit"
    );
    wav_duration_ms(&bytes)?;
    Ok(bytes)
}

/// Strict PCM16 mono WAV validation, without decoding or resampling untrusted
/// compressed media. The UI documents the accepted format before upload.
fn wav_duration_ms(bytes: &[u8]) -> Result<u64> {
    ensure!(
        bytes.len() >= 44 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
        "voice recordings must be RIFF WAV files"
    );
    let read_u32 =
        |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().expect("bounds checked"));
    ensure!(
        (read_u32(4) as usize).checked_add(8) == Some(bytes.len()),
        "WAV container length is invalid"
    );
    let mut cursor = 12usize;
    let mut rate = None;
    let mut data_len = None;
    while cursor + 8 <= bytes.len() {
        let len = read_u32(cursor + 4) as usize;
        let start = cursor + 8;
        let end = start
            .checked_add(len)
            .ok_or_else(|| anyhow!("invalid WAV chunk size"))?;
        ensure!(end <= bytes.len(), "WAV chunk is truncated");
        match &bytes[cursor..cursor + 4] {
            b"fmt " => {
                ensure!(rate.is_none() && len >= 16, "invalid WAV format chunk");
                ensure!(
                    bytes[start..start + 2] == [1, 0]
                        && bytes[start + 2..start + 4] == [1, 0]
                        && bytes[start + 14..start + 16] == [16, 0],
                    "voice WAV must be uncompressed PCM16 mono"
                );
                let sample_rate = read_u32(start + 4);
                ensure!(
                    (8_000..=48_000).contains(&sample_rate)
                        && read_u32(start + 8) == sample_rate * 2
                        && bytes[start + 12..start + 14] == [2, 0],
                    "invalid WAV sample rate or PCM alignment"
                );
                rate = Some(sample_rate);
            }
            b"data" => {
                ensure!(
                    data_len.is_none() && len > 0 && len.is_multiple_of(2),
                    "invalid WAV PCM data"
                );
                data_len = Some(len);
            }
            _ => {}
        }
        cursor = end + (len % 2);
    }
    ensure!(cursor == bytes.len(), "invalid WAV chunk alignment");
    Ok(
        data_len.ok_or_else(|| anyhow!("WAV has no audio data"))? as u64 * 1_000
            / (u64::from(rate.ok_or_else(|| anyhow!("WAV has no format"))?) * 2),
    )
}

/// Stream 24 kHz mono PCM16 in frames of at most 480 samples. Backpressure and
/// cancellation reach the HTTP reader; no full utterance audio is accumulated.
pub async fn synthesize(
    config: &SynthesisConfig,
    text: &str,
    audio: mpsc::Sender<Vec<i16>>,
    cancel: CancellationToken,
) -> Result<()> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Ok(()),
        result = async {
            validate_text(text, 8192, "synthesis text")?;
            validate_id(&config.voice_id)?;
            validate_text(&config.model, 100, "synthesis model")?;
            ensure!(config.style.len() <= 2000, "synthesis style exceeds 2000 bytes");
            let api = Api::new(&config.provider, &config.api_key_env)?;
            stream::synthesize_with_api(&api, config, text, audio).await
        } => result,
    }
}

#[cfg(test)]
mod tests;
