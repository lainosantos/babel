//! Deepgram Listen v1 original-speech recognition. Never translates or generates audio.
use std::{collections::VecDeque, future::Future, net::IpAddr, time::Duration};

use anyhow::{Result, bail, ensure};
use async_trait::async_trait;
use futures_util::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use reqwest::Url;
use serde_json::Value;
use tokio::{
    net::TcpStream,
    sync::mpsc,
    time::{Instant, timeout},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{
        self, Message, client::IntoClientRequest, http::HeaderValue, protocol::WebSocketConfig,
    },
};
use tokio_util::sync::CancellationToken;

use super::{ProviderEvent, SessionConfig, SpeechProvider, TranscriptMetadata};
use crate::config::DeepgramSttConfig;

const MAX_MESSAGE: usize = 256 * 1024;
const MAX_TEXT: usize = 32 * 1024;
const MAX_WORDS: usize = 4096;
const MAX_FINALS: usize = 16;
const SEND_TIMEOUT: Duration = Duration::from_millis(500);
const EVENT_TIMEOUT: Duration = Duration::from_secs(2);
const KEEP_ALIVE: Duration = Duration::from_secs(3);
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type StreamResult<T> = std::result::Result<T, Failure>;

pub struct DeepgramProvider {
    config: DeepgramSttConfig,
}
impl DeepgramProvider {
    pub fn new(config: DeepgramSttConfig) -> Result<Self> {
        endpoint(&config.endpoint)?;
        ensure!(
            (1..=120).contains(&config.connect_timeout_secs) && config.max_reconnect_attempts <= 20,
            "invalid Deepgram timeout/reconnect limits"
        );
        ensure!(
            !config.api_key_env.is_empty()
                && config.api_key_env.len() <= 128
                && config
                    .api_key_env
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
            "invalid Deepgram credential reference"
        );
        ensure!(
            config.model.len() <= 200
                && (config.model == "nova-3"
                    || config.model == "nova-2"
                    || config.model.starts_with("nova-3-")
                    || config.model.starts_with("nova-2-"))
                && config
                    .model
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            "Deepgram ASR requires a Nova-2 or Nova-3 Listen v1 model"
        );
        Ok(Self { config })
    }

    pub(super) fn url(&self, source: &str) -> Result<String> {
        ensure!(
            !source.is_empty()
                && source.len() <= 35
                && source
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            "Deepgram ASR source must be a language code or auto"
        );
        let language = if source.eq_ignore_ascii_case("auto") || source == "multi" {
            ensure!(
                matches!(
                    self.config.model.as_str(),
                    "nova-3" | "nova-3-general" | "nova-2" | "nova-2-general"
                ),
                "Deepgram automatic multilingual recognition requires a general Nova-2 or Nova-3 model"
            );
            "multi"
        } else {
            source
        };
        let mut url = endpoint(&self.config.endpoint)?;
        {
            let mut query = url.query_pairs_mut();
            query.extend_pairs([
                ("encoding", "linear16"),
                ("sample_rate", "16000"),
                ("channels", "1"),
                ("model", &self.config.model),
                ("language", language),
                ("interim_results", "false"),
                (
                    "punctuate",
                    if self.config.punctuate {
                        "true"
                    } else {
                        "false"
                    },
                ),
            ]);
            // The public boolean controls the current documented streaming diarizer.
            // Never combine diarize_model with the deprecated diarize query flag.
            if self.config.diarize {
                query.append_pair("diarize_model", "v1");
            }
        }
        Ok(url.into())
    }

    async fn connection(
        &self,
        url: &str,
        key: &str,
        audio: &mut mpsc::Receiver<Vec<i16>>,
        events: &mpsc::Sender<ProviderEvent>,
    ) -> StreamResult<()> {
        let mut request = url
            .into_client_request()
            .map_err(|_| Failure::fatal("invalid Deepgram endpoint"))?;
        let mut header = HeaderValue::from_str(&format!("Token {}", key.trim()))
            .map_err(|_| Failure::fatal("Deepgram key contains invalid header characters"))?;
        header.set_sensitive(true);
        request.headers_mut().insert("Authorization", header);
        let limits = WebSocketConfig::default()
            .read_buffer_size(16 * 1024)
            .write_buffer_size(0)
            .max_write_buffer_size(64 * 1024)
            .max_message_size(Some(MAX_MESSAGE))
            .max_frame_size(Some(MAX_MESSAGE));
        let (socket, _) = timeout(
            Duration::from_secs(self.config.connect_timeout_secs),
            connect_async_with_config(request, Some(limits), true),
        )
        .await
        .map_err(|_| Failure::retry("Deepgram connection timed out"))?
        .map_err(socket_failure)?;
        discard_audio(audio);
        // Listen v1 has no separate session configuration/acknowledgement message.
        emit(events, ProviderEvent::Connected).await?;
        let (writer, reader) = socket.split();
        let (control_tx, control_rx) = mpsc::channel(8);
        tokio::select! {
            result = send_audio(writer, audio, control_rx) => result,
            result = receive(reader, events, control_tx, self.config.diarize) => result,
        }
    }
}

fn endpoint(value: &str) -> Result<Url> {
    ensure!(value.len() <= 2048, "Deepgram endpoint is too long");
    let url =
        Url::parse(value).map_err(|_| anyhow::anyhow!("invalid Deepgram WebSocket endpoint"))?;
    let host = url.host_str().unwrap_or("");
    let loopback = host == "localhost"
        || host
            .trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    ensure!(
        url.scheme() == "wss" || (url.scheme() == "ws" && loopback),
        "Deepgram endpoint requires wss, or ws on loopback only"
    );
    ensure!(
        !host.is_empty()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "Deepgram endpoint must have a host and no credentials, query or fragment"
    );
    ensure!(
        !url.path().starts_with("/v2/"),
        "Deepgram Nova ASR cannot use the Flux v2 endpoint"
    );
    Ok(url)
}

#[derive(Debug)]
struct Failure {
    message: &'static str,
    retryable: bool,
}
impl Failure {
    fn fatal(message: &'static str) -> Self {
        Self {
            message,
            retryable: false,
        }
    }
    fn retry(message: &'static str) -> Self {
        Self {
            message,
            retryable: true,
        }
    }
}

#[async_trait]
impl SpeechProvider for DeepgramProvider {
    fn id(&self) -> &'static str {
        "deepgram"
    }
    async fn run(
        &self,
        config: SessionConfig,
        mut audio: mpsc::Receiver<Vec<i16>>,
        events: mpsc::Sender<ProviderEvent>,
        cancel: CancellationToken,
    ) -> Result<()> {
        let url = self.url(&config.source_language)?;
        // Translation-specific SessionConfig fields and its credentials are never used.
        let key = crate::credentials::get(&self.config.api_key_env)?;
        ensure!(key.len() <= 4096, "Deepgram API key exceeds the limit");
        let mut attempts = 0u32;
        loop {
            let started = Instant::now();
            let failure = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Ok(()),
                result = self.connection(&url, &key, &mut audio, &events) => match result {
                    Ok(()) => return Ok(()), Err(failure) => failure,
                },
            };
            if !failure.retryable {
                bail!("{}", failure.message);
            }
            if started.elapsed() >= Duration::from_secs(60) {
                attempts = 0;
            }
            if attempts >= self.config.max_reconnect_attempts {
                bail!("{}; reconnect budget exhausted", failure.message);
            }
            attempts += 1;
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Ok(()),
                result = emit(&events, ProviderEvent::Reconnecting { attempt: attempts }) =>
                    result.map_err(|error| anyhow::anyhow!(error.message))?,
            }
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Ok(()),
                _ = tokio::time::sleep(Duration::from_millis((250u64 << (attempts - 1).min(5)).min(5000))) => (),
            }
            discard_audio(&mut audio);
        }
    }
}

fn discard_audio(audio: &mut mpsc::Receiver<Vec<i16>>) {
    for _ in 0..audio.len() {
        let _ = audio.try_recv();
    }
}
fn socket_failure(error: tungstenite::Error) -> Failure {
    if let tungstenite::Error::Http(response) = &error {
        return match response.status().as_u16() {
            401 | 403 => Failure::fatal(
                "Deepgram authentication rejected; check the API key and model access",
            ),
            400 | 404 | 405 | 422 => {
                Failure::fatal("Deepgram endpoint, model or language configuration rejected")
            }
            429 => Failure::retry("Deepgram rate limit exceeded"),
            _ => Failure::retry("Deepgram WebSocket handshake failed"),
        };
    }
    match error {
        tungstenite::Error::Capacity(_) => {
            Failure::fatal("Deepgram message exceeds the memory limit")
        }
        tungstenite::Error::Utf8(_) => Failure::fatal("Deepgram sent invalid UTF-8"),
        _ => Failure::retry("Deepgram WebSocket connection lost"),
    }
}
async fn send<T>(
    operation: impl Future<Output = std::result::Result<T, tungstenite::Error>>,
) -> StreamResult<T> {
    timeout(SEND_TIMEOUT, operation)
        .await
        .map_err(|_| Failure::retry("Deepgram WebSocket send timed out"))?
        .map_err(socket_failure)
}
async fn emit(events: &mpsc::Sender<ProviderEvent>, event: ProviderEvent) -> StreamResult<()> {
    timeout(EVENT_TIMEOUT, events.send(event))
        .await
        .map_err(|_| Failure::fatal("Deepgram transcript consumer is too slow"))?
        .map_err(|_| Failure::fatal("Deepgram transcript consumer closed"))
}
async fn send_audio(
    mut writer: SplitSink<Socket, Message>,
    audio: &mut mpsc::Receiver<Vec<i16>>,
    mut controls: mpsc::Receiver<Message>,
) -> StreamResult<()> {
    let mut keepalive = tokio::time::interval(KEEP_ALIVE);
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    keepalive.tick().await;
    let mut ping = tokio::time::interval(Duration::from_secs(15));
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping.tick().await;
    loop {
        tokio::select! {
            value = controls.recv() => {
                send(writer.send(value.ok_or_else(|| Failure::retry("Deepgram receive loop stopped"))?)).await?;
            }
            samples = audio.recv() => {
                let samples = samples.ok_or_else(|| Failure::fatal("audio source closed unexpectedly"))?;
                if samples.len() > 16000 { return Err(Failure::fatal("Deepgram input audio chunk exceeds one second")); }
                if !samples.is_empty() {
                    let bytes: Vec<u8> = samples.iter().flat_map(|sample| sample.to_le_bytes()).collect();
                    send(writer.send(Message::Binary(bytes.into()))).await?;
                }
            }
            _ = keepalive.tick() => { send(writer.send(Message::Text(r#"{"type":"KeepAlive"}"#.into()))).await?; }
            _ = ping.tick() => { send(writer.send(Message::Ping(Vec::new().into()))).await?; }
        }
    }
}
async fn receive(
    mut reader: SplitStream<Socket>,
    events: &mpsc::Sender<ProviderEvent>,
    controls: mpsc::Sender<Message>,
    diarize: bool,
) -> StreamResult<()> {
    let mut finals = Finals::default();
    loop {
        // JSON KeepAlive receives no response. RFC6455 ping/pong supplies liveness
        // without treating a silent user as a failed recognition connection.
        let message = timeout(Duration::from_secs(45), reader.next())
            .await
            .map_err(|_| Failure::retry("Deepgram connection stopped responding"))?
            .ok_or_else(|| Failure::retry("Deepgram WebSocket closed unexpectedly"))?
            .map_err(socket_failure)?;
        let value = match message {
            Message::Text(text) => parse(text.as_bytes())?,
            Message::Binary(_) => {
                return Err(Failure::fatal(
                    "Deepgram ASR unexpectedly returned binary content",
                ));
            }
            Message::Ping(data) => {
                controls
                    .try_send(Message::Pong(data))
                    .map_err(|_| Failure::retry("Deepgram control channel is congested"))?;
                continue;
            }
            Message::Pong(_) => continue,
            Message::Close(frame) => {
                return Err(match frame.map(|frame| u16::from(frame.code)) {
                    Some(1008) => {
                        Failure::fatal("Deepgram rejected session policy or authentication")
                    }
                    _ => Failure::retry("Deepgram WebSocket closed unexpectedly"),
                });
            }
            Message::Frame(_) => return Err(Failure::fatal("invalid raw Deepgram frame")),
        };
        for event in finals.decode(&value, diarize)? {
            emit(events, event).await?;
        }
    }
}
fn parse(bytes: &[u8]) -> StreamResult<Value> {
    if bytes.len() > MAX_MESSAGE {
        return Err(Failure::fatal("Deepgram message exceeds the memory limit"));
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| Failure::fatal("Deepgram sent invalid JSON"))?;
    if !value.is_object() {
        return Err(Failure::fatal("Deepgram event must be an object"));
    }
    if value["type"] == "Error" {
        return Err(match value["variant"].as_str() {
            Some("NET-0000" | "NET-0001" | "NET-0002" | "NET-0003") => {
                Failure::retry("Deepgram temporarily interrupted transcription")
            }
            _ => Failure::fatal("Deepgram rejected the recognition request"),
        });
    }
    Ok(value)
}

#[derive(Default)]
struct Finals {
    recent: VecDeque<(u64, u64, String, bool)>,
    turn_open: bool,
}
impl Finals {
    fn decode(&mut self, value: &Value, diarize: bool) -> StreamResult<Vec<ProviderEvent>> {
        if value["type"] != "Results" || value["is_final"] != true {
            return Ok(Vec::new());
        }
        if value
            .get("channel_index")
            .is_some_and(|channels| channels != &serde_json::json!([0, 1]))
        {
            return Err(Failure::fatal(
                "Deepgram ASR returned an unexpected audio channel",
            ));
        }
        let start = seconds(&value["start"])?;
        let duration = seconds(&value["duration"])?;
        let end = start
            .checked_add(duration)
            .ok_or_else(|| Failure::fatal("invalid Deepgram timestamp"))?;
        let alternative = value
            .pointer("/channel/alternatives/0")
            .ok_or_else(|| Failure::fatal("Deepgram final transcript is missing"))?;
        let text = alternative["transcript"]
            .as_str()
            .filter(|text| text.len() <= MAX_TEXT)
            .ok_or_else(|| Failure::fatal("Deepgram transcript is missing or exceeds the limit"))?;
        let boundary = value["speech_final"] == true;
        let previous =
            self.recent
                .iter_mut()
                .find(|(previous_start, previous_end, previous_text, _)| {
                    *previous_start == start && *previous_end == end && previous_text == text
                });
        let mut result = Vec::new();
        let new_boundary = if let Some((_, _, _, completed)) = previous {
            let new_boundary = boundary && !*completed;
            *completed |= boundary;
            new_boundary
        } else {
            if !text.trim().is_empty() {
                result = segments(alternative, text, start, end, diarize)?;
                self.turn_open = true;
            }
            if self.recent.len() == MAX_FINALS {
                self.recent.pop_front();
            }
            self.recent.push_back((start, end, text.into(), boundary));
            boundary
        };
        if new_boundary && self.turn_open {
            result.push(ProviderEvent::TurnComplete);
            self.turn_open = false;
        }
        Ok(result)
    }
}
fn seconds(value: &Value) -> StreamResult<u64> {
    value
        .as_f64()
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0 && *seconds <= 604800.0)
        .map(|seconds| (seconds * 1000.0).round() as u64)
        .ok_or_else(|| Failure::fatal("invalid Deepgram timestamp"))
}
fn transcript(text: String, speaker: Option<String>, start: u64, end: u64) -> ProviderEvent {
    ProviderEvent::Transcript {
        input: true,
        text,
        metadata: TranscriptMetadata {
            speaker,
            start_ms: Some(start),
            end_ms: Some(end),
            alignment_ms: None,
        },
    }
}
fn segments(
    alternative: &Value,
    text: &str,
    start: u64,
    end: u64,
    diarize: bool,
) -> StreamResult<Vec<ProviderEvent>> {
    let Some(words) = alternative.get("words").filter(|words| !words.is_null()) else {
        return Ok(vec![transcript(text.into(), None, start, end)]);
    };
    let words = words
        .as_array()
        .filter(|words| words.len() <= MAX_WORDS)
        .ok_or_else(|| Failure::fatal("Deepgram word metadata exceeds safe limits"))?;
    if words.is_empty() {
        return Ok(vec![transcript(text.into(), None, start, end)]);
    }
    let mut parsed = Vec::with_capacity(words.len());
    for word in words {
        let text = word
            .get("punctuated_word")
            .or_else(|| word.get("word"))
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty() && text.len() <= 1024)
            .ok_or_else(|| Failure::fatal("invalid Deepgram word text"))?;
        let start = seconds(&word["start"])?;
        let end = seconds(&word["end"])?;
        if end < start {
            return Err(Failure::fatal("invalid Deepgram word timing"));
        }
        let speaker = match word.get("speaker").filter(|value| !value.is_null()) {
            None => None,
            Some(value) => Some(
                value
                    .as_u64()
                    .ok_or_else(|| Failure::fatal("invalid Deepgram speaker identifier"))?
                    .to_string(),
            ),
        };
        parsed.push((text, speaker, start, end));
    }
    if !diarize || parsed.iter().all(|word| word.1.is_none()) {
        return Ok(vec![transcript(
            text.into(),
            None,
            parsed[0].2,
            parsed.last().unwrap().3,
        )]);
    }
    let mut result = Vec::new();
    let mut text = String::new();
    let mut speaker = parsed[0].1.clone();
    let mut start = parsed[0].2;
    let mut end = start;
    for (word, current, word_start, word_end) in parsed {
        if current != speaker && !text.is_empty() {
            result.push(transcript(std::mem::take(&mut text), speaker, start, end));
            start = word_start;
            speaker = current;
        }
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(word);
        if text.len() > MAX_TEXT {
            return Err(Failure::fatal("Deepgram transcript exceeds the limit"));
        }
        end = word_end;
    }
    if !text.is_empty() {
        result.push(transcript(text, speaker, start, end));
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
