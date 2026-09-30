//! Optional streaming voice replacement. Translation text is transient and never
//! saved by this adapter. It adds a TTS request and its associated latency/cost.
use crate::{
    provider::{ProviderEvent, SessionConfig, SpeechProvider},
    voices::{self, SynthesisConfig},
};
use anyhow::{Context, Result, anyhow, ensure};
use async_trait::async_trait;
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
    time::Instant,
};
use tokio_util::sync::CancellationToken;

#[async_trait]
trait Synthesizer: Send + Sync {
    async fn synthesize(
        &self,
        config: &SynthesisConfig,
        text: &str,
        audio: mpsc::Sender<Vec<i16>>,
        cancel: CancellationToken,
    ) -> Result<()>;
}

struct CloudSynthesizer;

#[async_trait]
impl Synthesizer for CloudSynthesizer {
    async fn synthesize(
        &self,
        config: &SynthesisConfig,
        text: &str,
        audio: mpsc::Sender<Vec<i16>>,
        cancel: CancellationToken,
    ) -> Result<()> {
        voices::synthesize(config, text, audio, cancel).await
    }
}

struct Segment {
    text: String,
    generation: u64,
    created: Instant,
}
struct Synthesized {
    samples: Vec<i16>,
    generation: u64,
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    provider: Arc<dyn SpeechProvider>,
    session: SessionConfig,
    synthesis: SynthesisConfig,
    chunk_ms: u32,
    queue_ms: u32,
    audio: mpsc::Receiver<Vec<i16>>,
    events: mpsc::Sender<ProviderEvent>,
    cancel: CancellationToken,
) -> Result<()> {
    run_with_synthesizer(
        provider,
        session,
        synthesis,
        chunk_ms,
        queue_ms,
        audio,
        events,
        cancel,
        Arc::new(CloudSynthesizer),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_with_synthesizer(
    provider: Arc<dyn SpeechProvider>,
    mut session: SessionConfig,
    synthesis: SynthesisConfig,
    chunk_ms: u32,
    queue_ms: u32,
    audio: mpsc::Receiver<Vec<i16>>,
    events: mpsc::Sender<ProviderEvent>,
    cancel: CancellationToken,
    synthesizer: Arc<dyn Synthesizer>,
) -> Result<()> {
    let _guard = cancel.clone().drop_guard();
    session.output_transcription = true;
    let (provider_tx, mut provider_rx) = mpsc::channel(16);
    let (text_tx, text_rx) = mpsc::channel(4);
    let (synth_tx, mut synth_rx) = mpsc::channel(4);
    let (generation_tx, generation_rx) = watch::channel(0_u64);
    let mut jobs = JoinSet::new();
    let provider_cancel = cancel.clone();
    jobs.spawn(async move {
        provider
            .run(session, audio, provider_tx, provider_cancel)
            .await
    });
    let synth_cancel = cancel.clone();
    jobs.spawn(synthesis_worker(
        synthesizer,
        synthesis,
        text_rx,
        synth_tx,
        generation_rx,
        queue_ms,
        synth_cancel,
    ));
    let mut pending = String::new();
    let mut generation = 0_u64;
    let mut pending_since = None;
    let mut tick = tokio::time::interval(Duration::from_millis(25));
    let result: Result<()> = async {
        loop {
            if cancel.is_cancelled() { break Ok(()); }
            tokio::select! {
                _ = cancel.cancelled() => break Ok(()),
                done = jobs.join_next() => { break match done { _ if cancel.is_cancelled() => Ok(()), Some(Ok(Err(e))) => Err(e), _ => Err(anyhow!("A voice synthesis stage ended unexpectedly")) }; }
                event = provider_rx.recv() => {
                    if cancel.is_cancelled() { break Ok(()); }
                    let Some(event) = event else {
                        break stage_result_after_eof(&mut jobs, &cancel, "Translation channel closed").await;
                    };
                    match event {
                        ProviderEvent::Audio { .. } => {} // The selected synthesizer supplies the final voice.
                        ProviderEvent::Transcript { input: false, text, .. } => {
                            ensure!(pending.len() + text.len() <= 8192, "Translated text exceeded the synthesis limit");
                            if !text.is_empty() { pending_since.get_or_insert_with(Instant::now); }
                            pending.push_str(&text);
                            enqueue_pending(&text_tx, &mut pending, &mut pending_since, generation, false)?;
                        }
                        ProviderEvent::TurnComplete => {
                            enqueue_pending(&text_tx, &mut pending, &mut pending_since, generation, true)?;
                            forward(&events, ProviderEvent::TurnComplete, &cancel).await?;
                        }
                        event @ (ProviderEvent::Interrupted | ProviderEvent::Reconnecting { .. }) => {
                            pending.clear(); pending_since = None; generation = generation.wrapping_add(1);
                            generation_tx.send_replace(generation);
                            forward(&events, event, &cancel).await?;
                        }
                        event => { forward(&events, event, &cancel).await?; }
                    }
                }
                packet = synth_rx.recv() => {
                    if cancel.is_cancelled() { break Ok(()); }
                    let Some(packet) = packet else {
                        break stage_result_after_eof(&mut jobs, &cancel, "Synthesized voice channel closed").await;
                    };
                    if packet.generation == generation { forward(&events, ProviderEvent::Audio { samples: packet.samples, sample_rate: 24_000 }, &cancel).await?; }
                }
                _ = tick.tick() => {
                    if pending_since.is_some_and(|start: Instant| start.elapsed() >= Duration::from_millis(u64::from(chunk_ms))) {
                        enqueue_pending(&text_tx, &mut pending, &mut pending_since, generation, true)?;
                    }
                }
            }
        }
    }.await;
    cancel.cancel();
    // Give providers a short opportunity to close their session, then abort any
    // unresponsive task. Dropping a synthesis worker also drops its HTTP future.
    let _ = tokio::time::timeout(Duration::from_secs(1), async {
        while jobs.join_next().await.is_some() {}
    })
    .await;
    jobs.shutdown().await;
    result
}

async fn stage_result_after_eof(
    jobs: &mut JoinSet<Result<()>>,
    cancel: &CancellationToken,
    channel: &'static str,
) -> Result<()> {
    // A stage can drop its sender before returning its error. Keep cancellation
    // and a deadline while retrieving the task result; EOF alone loses its cause.
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Ok(()),
        result = tokio::time::timeout(Duration::from_secs(2), jobs.join_next()) => {
            match result.with_context(|| format!("{channel}; stage did not finish within 2 seconds"))? {
                Some(result) => {
                    result.context("Voice synthesis task ended unexpectedly")??;
                    Err(anyhow!("A voice synthesis stage ended unexpectedly"))
                }
                None => Err(anyhow!("{channel}; no completion result is available")),
            }
        }
    }
}

fn enqueue_pending(
    sender: &mpsc::Sender<Segment>,
    pending: &mut String,
    pending_since: &mut Option<Instant>,
    generation: u64,
    flush: bool,
) -> Result<()> {
    while let Some(phrase) = take_phrase(pending, flush) {
        enqueue(sender, phrase, generation, Instant::now())?;
    }
    if pending.is_empty() {
        *pending_since = None;
    }
    Ok(())
}

fn enqueue(
    sender: &mpsc::Sender<Segment>,
    text: String,
    generation: u64,
    created: Instant,
) -> Result<()> {
    if text.trim().is_empty() {
        return Ok(());
    }
    sender.try_send(Segment { text, generation, created }).map_err(|_| anyhow!("Voice synthesis cannot keep up with speech; text queue full. Use a native voice or a faster TTS model"))
}
async fn forward(
    events: &mpsc::Sender<ProviderEvent>,
    event: ProviderEvent,
    cancel: &CancellationToken,
) -> Result<()> {
    tokio::select! {
        _ = cancel.cancelled() => Ok(()),
        result = tokio::time::timeout(Duration::from_secs(2), events.send(event)) => { result.context("Audio consumer is slow")?.context("Audio consumer disconnected") }
    }
}

/// Prefer sentence boundaries; split long unpunctuated speech at a word boundary.
fn take_phrase(text: &mut String, flush: bool) -> Option<String> {
    if text.trim().is_empty() {
        text.clear();
        return None;
    }
    let boundary = text
        .char_indices()
        .take(240)
        .enumerate()
        .find_map(|(chars, (i, c))| {
            (chars >= 12 && matches!(c, '.' | '!' | '?' | '\n' | ';')).then_some(i + c.len_utf8())
        });
    let end = if let Some(end) = boundary {
        end
    } else if text.chars().count() >= 240 {
        let max = text.char_indices().nth(240).map_or(text.len(), |(i, _)| i);
        text[..max]
            .rfind(char::is_whitespace)
            .filter(|&i| i > 0)
            .unwrap_or(max)
    } else if flush {
        text.len()
    } else {
        return None;
    };
    Some(text.drain(..end).collect::<String>().trim().to_owned())
}

async fn synthesis_worker(
    synthesizer: Arc<dyn Synthesizer>,
    config: SynthesisConfig,
    mut text: mpsc::Receiver<Segment>,
    output: mpsc::Sender<Synthesized>,
    mut generation: watch::Receiver<u64>,
    queue_ms: u32,
    cancel: CancellationToken,
) -> Result<()> {
    // Keep the clock across text segments, including final partial PCM frames.
    let mut next_sample = Instant::now();
    loop {
        let segment = tokio::select! { biased; _ = cancel.cancelled() => return Ok(()), segment = text.recv() => segment.context("Text queue closed")? };
        if segment.generation != *generation.borrow() {
            continue;
        }
        ensure!(
            segment.created.elapsed() <= Duration::from_millis(u64::from(queue_ms)),
            "Voice synthesis is delayed; queue expired. Use a faster TTS model or the native voice"
        );
        let (pcm_tx, mut pcm_rx) = mpsc::channel(2);
        let request_cancel = cancel.child_token();
        // Poll the network future together with its output instead of spawning a
        // detached task. Cancellation or interruption drops this exact request.
        let request =
            synthesizer.synthesize(&config, &segment.text, pcm_tx, request_cancel.clone());
        tokio::pin!(request);
        let mut finished = false;
        let mut pcm_closed = false;
        let result: Result<()> = async {
            loop {
                if finished && pcm_closed { break Ok(()); }
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break Ok(()),
                    changed = generation.changed() => { changed.context("Audio generation ended")?; if segment.generation != *generation.borrow() { break Ok(()); } }
                    result = &mut request, if !finished => { finished = true; result?; }
                    packet = pcm_rx.recv(), if !pcm_closed => {
                        let Some(samples) = packet else {
                            // The sender may be dropped before the request reports
                            // its final status. Keep polling it WITH cancellation.
                            pcm_closed = true;
                            continue;
                        };
                        ensure!(!samples.is_empty() && samples.len() <= 480, "Synthesizer returned an empty PCM chunk or one exceeding 20 ms");
                        tokio::select! {
                            biased;
                            _ = cancel.cancelled() => break Ok(()),
                            changed = generation.changed() => { changed.context("Audio generation ended")?; if segment.generation != *generation.borrow() { break Ok(()); } }
                            _ = tokio::time::sleep_until(next_sample) => {}
                        }
                        if segment.generation != *generation.borrow() { break Ok(()); }
                        let frame_duration = Duration::from_secs_f64(samples.len() as f64 / 24000.0);
                        tokio::select! {
                            biased;
                            _ = cancel.cancelled() => break Ok(()),
                            changed = generation.changed() => { changed.context("Audio generation ended")?; if segment.generation != *generation.borrow() { break Ok(()); } }
                            result = output.send(Synthesized { samples, generation: segment.generation }) => result.context("Synthesis consumer closed")?,
                        }
                        // Never catch up by bursting old packets after a slow
                        // consumer. Time the next frame from this actual send.
                        next_sample = Instant::now() + frame_duration;
                    }
                }
            }
        }.await;
        request_cancel.cancel();
        result?;
        if cancel.is_cancelled() {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests;
