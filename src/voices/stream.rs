use super::*;

const MAX_EVENT: usize = 1024 * 1024;
const MAX_AUDIO_DELTA: usize = 256 * 1024;
const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;

pub(super) async fn synthesize_with_api(
    api: &Api<'_>,
    config: &SynthesisConfig,
    text: &str,
    audio: mpsc::Sender<Vec<i16>>,
) -> Result<()> {
    let request = if api.provider == "gemini" {
        let model = config
            .model
            .strip_prefix("models/")
            .unwrap_or(&config.model);
        ensure!(
            matches!(model, "gemini-3.8-flash-tts" | "gemini-3.8-flash-lite-tts"),
            "Gemini custom-voice synthesis requires a Gemini 3.8 TTS model"
        );
        let mut content = json!({"type": "text", "text": text});
        if !config.style.trim().is_empty() {
            content["annotations"] = json!([{"type": "speech_metadata", "style": config.style}]);
        }
        api.request(Method::POST, "/v1beta/interactions")?.json(&json!({
            "model": model,
            "input": [{"type": "user_input", "content": [content]}],
            "response_format": {"type": "audio", "mime_type": "audio/l16", "sample_rate": 24000},
            "generation_config": {"speech_config": [{"voice": config.voice_id}]},
            "stream": true,
            "store": false
        }))
    } else {
        ensure!(
            config.style.trim().is_empty(),
            "ElevenLabs streaming TTS does not accept a free-text style prompt; leave style empty"
        );
        let mut body = json!({"text": text, "model_id": config.model});
        if !config.language.is_empty() && config.model != "eleven_multilingual_v2" {
            let language = config.language.split('-').next().unwrap_or("");
            ensure!(
                language.len() == 2 && language.bytes().all(|b| b.is_ascii_alphabetic()),
                "ElevenLabs TTS language must have an ISO 639-1 code"
            );
            body["language_code"] = json!(language.to_ascii_lowercase());
        }
        api.request(
            Method::POST,
            &format!("/v1/text-to-speech/{}/stream", config.voice_id),
        )?
        .query(&[("output_format", "pcm_24000")])
        .json(&body)
    };
    let response = send(request).await?;
    let mime = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    if api.provider == "gemini" {
        ensure!(
            mime.split(';')
                .next()
                .is_some_and(|m| m.trim() == "text/event-stream"),
            "Gemini synthesis did not return an SSE stream"
        );
        stream_gemini(response, audio).await
    } else {
        validate_pcm_mime(&mime, true)?;
        stream_raw(response, audio).await
    }
}

fn validate_pcm_mime(mime: &str, allow_octet: bool) -> Result<()> {
    let lower = mime.to_ascii_lowercase();
    let mut fields = lower.split(';');
    let media = fields.next().unwrap_or("").trim();
    ensure!(
        matches!(media, "audio/l16" | "audio/pcm" | "audio/x-raw")
            || allow_octet && media == "application/octet-stream",
        "synthesis returned an unsupported audio encoding"
    );
    for parameter in fields {
        let Some((name, value)) = parameter.trim().split_once('=') else {
            bail!("synthesis returned invalid PCM metadata");
        };
        ensure!(
            matches!(
                (name.trim(), value.trim()),
                ("rate", "24000") | ("channels", "1") | ("codec", "pcm")
            ),
            "synthesis returned incompatible PCM metadata"
        );
    }
    Ok(())
}

struct PcmEmitter {
    audio: mpsc::Sender<Vec<i16>>,
    prefix: Vec<u8>,
    header_checked: bool,
    odd: Option<u8>,
    received: usize,
    frame: Vec<i16>,
}

impl PcmEmitter {
    fn new(audio: mpsc::Sender<Vec<i16>>) -> Self {
        Self {
            audio,
            prefix: Vec::with_capacity(12),
            header_checked: false,
            odd: None,
            received: 0,
            frame: Vec::with_capacity(480),
        }
    }

    async fn push(&mut self, bytes: &[u8]) -> Result<()> {
        ensure!(
            self.received + bytes.len() <= MAX_STREAM_BYTES,
            "synthesis audio exceeds the request size limit"
        );
        self.received += bytes.len();
        if !self.header_checked {
            let take = (12 - self.prefix.len()).min(bytes.len());
            self.prefix.extend_from_slice(&bytes[..take]);
            if self.prefix.len() < 12 {
                return Ok(());
            }
            self.check_header()?;
            self.header_checked = true;
            let prefix = std::mem::take(&mut self.prefix);
            self.emit(&prefix).await?;
            self.emit(&bytes[take..]).await
        } else {
            self.emit(bytes).await
        }
    }

    fn check_header(&self) -> Result<()> {
        ensure!(
            !self.prefix.starts_with(b"RIFF")
                && !self.prefix.starts_with(b"ID3")
                && !self.prefix.starts_with(b"OggS")
                && !self.prefix.starts_with(b"fLaC"),
            "synthesis returned container audio instead of requested PCM"
        );
        Ok(())
    }

    async fn emit(&mut self, bytes: &[u8]) -> Result<()> {
        let mut cursor = 0;
        if let Some(previous) = self.odd.take() {
            if bytes.is_empty() {
                self.odd = Some(previous);
                return Ok(());
            }
            self.frame.push(i16::from_le_bytes([previous, bytes[0]]));
            cursor = 1;
        }
        if self.frame.len() == 480 {
            let frame = std::mem::replace(&mut self.frame, Vec::with_capacity(480));
            self.deliver(frame).await?;
        }
        while cursor + 1 < bytes.len() {
            self.frame
                .push(i16::from_le_bytes([bytes[cursor], bytes[cursor + 1]]));
            cursor += 2;
            if self.frame.len() == 480 {
                let frame = std::mem::replace(&mut self.frame, Vec::with_capacity(480));
                self.deliver(frame).await?;
            }
        }
        if cursor < bytes.len() {
            self.odd = Some(bytes[cursor]);
        }
        Ok(())
    }

    async fn deliver(&self, samples: Vec<i16>) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(2), self.audio.send(samples))
            .await
            .map_err(|_| anyhow!("synthesis playback queue stalled"))?
            .map_err(|_| anyhow!("synthesis audio receiver closed"))
    }

    async fn finish(mut self) -> Result<()> {
        ensure!(
            self.received > 0,
            "voice provider returned no synthesized audio"
        );
        if !self.header_checked {
            self.check_header()?;
            let prefix = std::mem::take(&mut self.prefix);
            self.emit(&prefix).await?;
        }
        ensure!(
            self.odd.is_none(),
            "synthesis returned truncated PCM16 audio"
        );
        if !self.frame.is_empty() {
            let frame = std::mem::take(&mut self.frame);
            self.deliver(frame).await?;
        }
        Ok(())
    }
}

async fn stream_raw(mut response: Response, audio: mpsc::Sender<Vec<i16>>) -> Result<()> {
    let mut emitter = PcmEmitter::new(audio);
    while let Some(bytes) = response
        .chunk()
        .await
        .map_err(|_| anyhow!("synthesis audio stream interrupted"))?
    {
        emitter.push(&bytes).await?;
    }
    emitter.finish().await
}

#[derive(Default)]
struct SseDecoder {
    line: Vec<u8>,
    data: Vec<u8>,
    event: String,
}

impl SseDecoder {
    fn byte(&mut self, byte: u8) -> Result<Option<(String, Vec<u8>)>> {
        if byte != b'\n' {
            ensure!(
                self.line.len() + self.data.len() < MAX_EVENT,
                "synthesis SSE event exceeds memory limit"
            );
            self.line.push(byte);
            return Ok(None);
        }
        let mut line = std::mem::take(&mut self.line);
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.is_empty() {
            let event = std::mem::take(&mut self.event);
            let mut data = std::mem::take(&mut self.data);
            if data.last() == Some(&b'\n') {
                data.pop();
            }
            return Ok(Some((event, data)));
        }
        if let Some(mut data) = line.strip_prefix(b"data:") {
            if data.first() == Some(&b' ') {
                data = &data[1..];
            }
            self.data.extend_from_slice(data);
            self.data.push(b'\n');
        } else if let Some(mut event) = line.strip_prefix(b"event:") {
            if event.first() == Some(&b' ') {
                event = &event[1..];
            }
            ensure!(event.len() <= 128, "synthesis SSE event name exceeds limit");
            self.event = std::str::from_utf8(event)
                .map_err(|_| anyhow!("synthesis SSE name is not UTF-8"))?
                .into();
        }
        Ok(None)
    }
}

async fn stream_gemini(mut response: Response, audio: mpsc::Sender<Vec<i16>>) -> Result<()> {
    let mut decoder = SseDecoder::default();
    let mut emitter = PcmEmitter::new(audio);
    let mut completed = false;
    let mut network_bytes = 0usize;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow!("Gemini synthesis stream interrupted"))?
    {
        network_bytes += chunk.len();
        ensure!(
            network_bytes <= MAX_STREAM_BYTES * 2,
            "Gemini synthesis stream exceeds the request size limit"
        );
        for byte in chunk {
            if let Some((event, data)) = decoder.byte(byte)? {
                if data.is_empty() || data == b"[DONE]" {
                    continue;
                }
                let value: Value = serde_json::from_slice(&data)
                    .map_err(|_| anyhow!("Gemini synthesis SSE contained invalid JSON"))?;
                let event = value
                    .get("event_type")
                    .and_then(Value::as_str)
                    .unwrap_or(&event);
                ensure!(
                    value.get("error").is_none()
                        && !matches!(
                            event,
                            "error" | "interaction.failed" | "interaction.requires_action"
                        ),
                    "Gemini synthesis failed or requested unsupported action"
                );
                match event {
                    "step.delta"
                        if value.pointer("/delta/type").and_then(Value::as_str)
                            == Some("audio") =>
                    {
                        ensure!(!completed, "Gemini sent audio after synthesis completed");
                        let delta = &value["delta"];
                        if let Some(mime) = delta.get("mime_type").and_then(Value::as_str) {
                            validate_pcm_mime(mime, false)?;
                        }
                        if let Some(rate) = delta.get("sample_rate") {
                            ensure!(
                                rate.as_u64() == Some(24_000),
                                "Gemini synthesis sample rate changed"
                            );
                        }
                        let encoded = delta
                            .get("data")
                            .and_then(Value::as_str)
                            .ok_or_else(|| anyhow!("Gemini synthesis audio has no data"))?;
                        ensure!(
                            encoded.len() <= MAX_AUDIO_DELTA.div_ceil(3) * 4,
                            "Gemini synthesis audio delta exceeds memory limit"
                        );
                        let bytes = STANDARD
                            .decode(encoded)
                            .map_err(|_| anyhow!("Gemini synthesis audio has invalid Base64"))?;
                        ensure!(
                            bytes.len() <= MAX_AUDIO_DELTA,
                            "Gemini synthesis audio delta exceeds memory limit"
                        );
                        emitter.push(&bytes).await?;
                    }
                    "interaction.completed" => {
                        ensure!(
                            value
                                .pointer("/interaction/status")
                                .and_then(Value::as_str)
                                .is_none_or(|s| s == "completed"),
                            "Gemini synthesis did not complete successfully"
                        );
                        completed = true;
                    }
                    _ => {}
                }
            }
        }
    }
    ensure!(
        completed && decoder.line.is_empty() && decoder.data.is_empty(),
        "Gemini synthesis ended before completion"
    );
    emitter.finish().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pcm_reassembles_odd_network_boundaries_and_emits_bounded_frames() {
        let (tx, mut rx) = mpsc::channel(32);
        let mut emitter = PcmEmitter::new(tx);
        let expected: Vec<i16> = (0..2000).map(|v| v - 1000).collect();
        let bytes: Vec<u8> = expected.iter().flat_map(|v| v.to_le_bytes()).collect();
        for chunk in bytes.chunks(511) {
            emitter.push(chunk).await.unwrap();
        }
        emitter.finish().await.unwrap();
        let mut actual = Vec::new();
        while let Some(chunk) = rx.recv().await {
            assert!(chunk.len() <= 480);
            actual.extend(chunk);
        }
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn odd_pcm_and_container_audio_fail() {
        for bytes in [&b"RIFFabcdWAVE"[..], &b"x"[..]] {
            let (tx, _rx) = mpsc::channel(2);
            let mut emitter = PcmEmitter::new(tx);
            let push = emitter.push(bytes).await;
            assert!(push.is_err() || emitter.finish().await.is_err());
        }
    }

    #[test]
    fn sse_reassembles_data_lines_and_crlf() {
        let mut parser = SseDecoder::default();
        let data = b": keepalive\r\nevent: step.delta\r\ndata: {\r\ndata: \"x\":1}\r\n\r\n";
        let events: Vec<_> = data
            .iter()
            .filter_map(|b| parser.byte(*b).unwrap())
            .collect();
        assert_eq!(events, vec![("step.delta".into(), b"{\n\"x\":1}".to_vec())]);
    }
}
