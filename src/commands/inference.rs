use std::{
    collections::{BTreeMap, BTreeSet},
    io::Cursor,
    net::{IpAddr, SocketAddr},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use anyhow::{Result, ensure};
use futures_util::StreamExt;
use reqwest::{Client, RequestBuilder, Url, multipart};
use serde_json::{Value, json};

use super::{AgentConfig, CommandTool};

const MAX_JSON: usize = 256 * 1024;
const MAX_TEXT: usize = 8_192;
const WHISPER_PREFLIGHT_ERROR: &str =
    "Whisper endpoint is not a verified whisper.cpp server; no microphone audio was sent";

pub(super) fn validate_local_endpoint(endpoint: &str) -> Result<()> {
    let url = Url::parse(endpoint)
        .map_err(|_| anyhow::anyhow!("invalid local voice inference endpoint"))?;
    let host = url.host_str().unwrap_or("").trim_matches(['[', ']']);
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && (host == "localhost" || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()))
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
            && url.query().is_none(),
        "voice inference endpoints must use HTTP(S) localhost or a literal loopback address, without credentials/query/fragment"
    );
    Ok(())
}

pub(super) struct Inference {
    client: Client,
    config: AgentConfig,
    whisper_verified: AtomicBool,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct PlannedCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

impl Inference {
    pub fn new(config: &AgentConfig) -> Result<Self> {
        config.validate()?;
        // Pin localhost to loopback instead of trusting system DNS/hosts overrides.
        let builder = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(config.timeout_secs.min(3).into()))
            .timeout(Duration::from_secs(config.timeout_secs.into()));
        // A zero port preserves each URL's port; otherwise two services using
        // localhost would overwrite one another's resolver entry.
        let builder = builder.resolve("localhost", SocketAddr::from(([127, 0, 0, 1], 0)));
        Ok(Self {
            client: builder
                .build()
                .map_err(|_| anyhow::anyhow!("could not create local voice inference client"))?,
            config: config.clone(),
            whisper_verified: AtomicBool::new(false),
        })
    }

    /// Verify the dedicated ASR protocol without sending any microphone data.
    /// The health JSON alone is too generic: other local applications expose it.
    async fn verify_whisper(&self) -> Result<()> {
        if self.whisper_verified.load(Ordering::Acquire) {
            return Ok(());
        }
        let health = Url::parse(&self.config.whisper_endpoint)?.join("health")?;
        let response = auth(self.client.get(health), &self.config.whisper_api_key_env)?
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Whisper local service is unavailable or timed out"))?;
        ensure!(
            whisper_server(&response) && json_content_type(&response),
            WHISPER_PREFLIGHT_ERROR
        );
        ensure!(
            response.status().is_success(),
            "Whisper model is not ready; no microphone audio was sent"
        );
        let body = limited_body(response, 8192, "Whisper health").await?;
        let health: Value =
            serde_json::from_slice(&body).map_err(|_| anyhow::anyhow!(WHISPER_PREFLIGHT_ERROR))?;
        ensure!(health["status"] == "ok", WHISPER_PREFLIGHT_ERROR);
        // Official whisper.cpp refuses multipart requests without the file
        // field. Current versions overwrite the JSON 400 with "Invalid request"
        // through their generic error handler; accept exactly these two forms.
        let response = auth(
            self.client
                .post(&self.config.whisper_endpoint)
                .multipart(multipart::Form::new().text("response_format", "json")),
            &self.config.whisper_api_key_env,
        )?
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("Whisper local service is unavailable or timed out"))?;
        ensure!(
            response.status() == reqwest::StatusCode::BAD_REQUEST && whisper_server(&response),
            WHISPER_PREFLIGHT_ERROR
        );
        let is_json = json_content_type(&response);
        let is_plain = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                v.split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .eq_ignore_ascii_case("text/plain")
            });
        ensure!(is_json || is_plain, WHISPER_PREFLIGHT_ERROR);
        let body = limited_body(response, 8192, "Whisper preflight").await?;
        let matches = if is_json {
            serde_json::from_slice::<Value>(&body)
                .is_ok_and(|value| value["error"] == "no 'file' field in the request")
        } else {
            body == b"Invalid request"
        };
        ensure!(matches, WHISPER_PREFLIGHT_ERROR);
        self.whisper_verified.store(true, Ordering::Release);
        Ok(())
    }

    pub async fn transcribe(&self, samples: &[i16]) -> Result<String> {
        self.verify_whisper().await?;
        let result = self.transcribe_verified(samples).await;
        if result.is_err() {
            self.whisper_verified.store(false, Ordering::Release);
        }
        result
    }

    async fn transcribe_verified(&self, samples: &[i16]) -> Result<String> {
        ensure!(
            !samples.is_empty() && samples.len() <= 16 * self.config.max_utterance_ms as usize,
            "invalid command audio length"
        );
        let mut cursor = Cursor::new(Vec::with_capacity(samples.len() * 2 + 44));
        {
            let mut writer = hound::WavWriter::new(
                &mut cursor,
                hound::WavSpec {
                    channels: 1,
                    sample_rate: 16000,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                },
            )?;
            for sample in samples {
                writer.write_sample(*sample)?;
            }
            writer.finalize()?;
        }
        let language = self
            .config
            .whisper_language
            .split(['-', '_'])
            .next()
            .unwrap_or("auto")
            .to_lowercase();
        let form = multipart::Form::new()
            .part(
                "file",
                multipart::Part::bytes(cursor.into_inner())
                    .file_name("command.wav")
                    .mime_str("audio/wav")?,
            )
            .text("response_format", "json")
            .text("temperature", "0.0")
            .text("translate", "false")
            .text("language", language);
        let result = bounded_json(
            auth(
                self.client
                    .post(&self.config.whisper_endpoint)
                    .multipart(form),
                &self.config.whisper_api_key_env,
            )?,
            "Whisper",
        )
        .await?;
        let text = result["text"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Whisper returned no original transcript"))?;
        ensure!(
            text.len() <= MAX_TEXT,
            "Whisper transcript exceeds the command limit"
        );
        Ok(text.trim().into())
    }

    pub async fn plan(&self, command: &str, tools: &[CommandTool]) -> Result<Vec<PlannedCall>> {
        ensure!(
            !command.is_empty() && command.len() <= MAX_TEXT,
            "invalid command text length"
        );
        ensure!(!tools.is_empty(), "no enabled MCP tools are available");
        ensure!(
            tools.len() <= 128,
            "too many MCP tools for the local command catalog (maximum 128)"
        );
        let mut identities = BTreeSet::new();
        let mut aliases = BTreeMap::new();
        let mut catalog = Vec::new();
        for (index, tool) in tools.iter().enumerate() {
            ensure!(
                !tool.id.is_empty() && tool.id.len() <= 512 && identities.insert(tool.id.clone()),
                "invalid or duplicate MCP tool identity"
            );
            ensure!(
                tool.input_schema.is_object(),
                "MCP tool has no object input schema"
            );
            // Preserve descriptive names for Needle retrieval, but never confuse
            // equal names from different integrations or accept generated IDs.
            let stem: String = tool
                .name
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                .take(48)
                .collect();
            let alias = format!("tool{index}_{stem}");
            aliases.insert(alias.clone(), tool);
            catalog.push(json!({ "name": alias, "description": tool.description.chars().take(1024).collect::<String>(), "parameters": tool.input_schema }));
        }
        let request = json!({"text": command, "tools": catalog, "max_new_tokens": 512,
            "system": format!("date: {}; locale: {}; assistant: {}", chrono::Local::now().format("%Y-%m-%d %a %H:%M %:z"), self.config.whisper_language, self.config.wake_name)});
        ensure!(
            serde_json::to_vec(&request)?.len() <= MAX_JSON,
            "MCP tool catalog exceeds the command request limit"
        );
        let result = bounded_json(
            auth(
                self.client
                    .post(&self.config.needle_endpoint)
                    .json(&request),
                &self.config.needle_api_key_env,
            )?,
            "Needle 3",
        )
        .await?;
        parse_plan(
            &result,
            &aliases,
            self.config.min_confidence,
            self.config.max_calls,
        )
    }
}

pub(super) fn parse_plan(
    result: &Value,
    tools: &BTreeMap<String, &CommandTool>,
    confidence_floor: f64,
    max_calls: usize,
) -> Result<Vec<PlannedCall>> {
    ensure!(
        result["success"].as_bool() == Some(true) && result["type"].as_str() == Some("call"),
        "Needle 3 did not produce an executable command"
    );
    ensure!(
        result.get("error").is_none_or(Value::is_null),
        "Needle 3 reported an inference error"
    );
    ensure!(
        result.get("error_code").is_none_or(Value::is_null),
        "Needle 3 reported an inference error code"
    );
    ensure!(
        result
            .pointer("/validation/negation")
            .is_none_or(|v| v.as_bool() == Some(false)),
        "Needle 3 identified a negated command"
    );
    ensure!(
        result
            .get("suppressed_calls")
            .is_none_or(|v| v.as_array().is_some_and(Vec::is_empty)),
        "Needle 3 withheld an uncertain command"
    );
    ensure!(
        result
            .pointer("/validation/ungrounded")
            .is_none_or(|v| v.as_array().is_some_and(Vec::is_empty)),
        "Needle 3 arguments were not grounded in the command"
    );
    let confidence = result["confidence"]
        .as_f64()
        .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
        .ok_or_else(|| anyhow::anyhow!("Needle 3 returned no calibrated confidence"))?;
    ensure!(
        confidence >= confidence_floor,
        "Needle 3 confidence is below the configured threshold"
    );
    let calls = result["function_calls"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Needle 3 returned invalid calls"))?;
    ensure!(
        !calls.is_empty(),
        "Needle 3 found no matching tool for this command"
    );
    ensure!(
        calls.len() <= max_calls,
        "Needle 3 command exceeds the configured tool-call limit"
    );
    let mut parsed = Vec::new();
    let mut duplicates = BTreeSet::new();
    for call in calls {
        let name = call["name"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Needle 3 returned an invalid tool name"))?;
        let tool = tools.get(name).ok_or_else(|| {
            anyhow::anyhow!("Needle 3 selected a tool outside this command catalog")
        })?;
        let arguments = call
            .get("arguments")
            .filter(|v| v.is_object())
            .ok_or_else(|| anyhow::anyhow!("Needle 3 returned non-object tool arguments"))?;
        ensure!(
            duplicates.insert((name, serde_json::to_string(arguments)?)),
            "Needle 3 repeated the same tool call"
        );
        parsed.push(PlannedCall {
            id: tool.id.clone(),
            name: tool.name.clone(),
            arguments: arguments.clone(),
        });
    }
    Ok(parsed)
}

fn auth(request: RequestBuilder, env: &str) -> Result<RequestBuilder> {
    if env.is_empty() {
        return Ok(request);
    }
    let secret = crate::credentials::get(env)?;
    Ok(request.bearer_auth(secret.as_str()))
}

fn whisper_server(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(reqwest::header::SERVER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("whisper.cpp"))
}

fn json_content_type(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
}

async fn bounded_json(request: RequestBuilder, service: &str) -> Result<Value> {
    let response = request
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("{service} local service is unavailable or timed out"))?;
    ensure!(
        response.status().is_success(),
        "{service} local service returned HTTP {}",
        response.status().as_u16()
    );
    ensure!(
        json_content_type(&response),
        "{service} local service did not return application/json"
    );
    let bytes = limited_body(response, MAX_JSON, service).await?;
    serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("{service} returned invalid JSON"))
}

async fn limited_body(response: reqwest::Response, limit: usize, service: &str) -> Result<Vec<u8>> {
    ensure!(
        response
            .content_length()
            .is_none_or(|len| len <= limit as u64),
        "{service} response exceeds the command memory limit"
    );
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| anyhow::anyhow!("{service} response failed or timed out"))?;
        ensure!(
            bytes.len().saturating_add(chunk.len()) <= limit,
            "{service} response exceeds the command memory limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
