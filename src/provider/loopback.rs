use anyhow::{Result, bail};
use async_trait::async_trait;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_util::sync::CancellationToken;

use super::{ProviderEvent, SessionConfig, SpeechProvider};

/// Diagnostic routing provider. No AI, translation, credentials, or network.
pub(super) struct LoopbackProvider;

#[async_trait]
impl SpeechProvider for LoopbackProvider {
    fn id(&self) -> &'static str {
        "loopback"
    }

    async fn run(
        &self,
        _config: SessionConfig,
        mut audio: Receiver<Vec<i16>>,
        events: Sender<ProviderEvent>,
        cancel: CancellationToken,
    ) -> Result<()> {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(()),
            result = events.send(ProviderEvent::Connected) => {
                result.map_err(|_| anyhow::anyhow!("provider event receiver closed"))?;
            }
        }
        let mut resampler = DiagnosticResampler::default();
        loop {
            let samples = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Ok(()),
                samples = audio.recv() => samples,
            };
            let Some(samples) = samples else {
                bail!("audio source closed unexpectedly");
            };
            if samples.len() > 16_000 {
                bail!("input audio chunk exceeds one second");
            }
            let output = resampler.process(&samples);
            if output.is_empty() {
                continue;
            }
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Ok(()),
                result = events.send(ProviderEvent::Audio { samples: output, sample_rate: 24_000 }) => {
                    result.map_err(|_| anyhow::anyhow!("provider event receiver closed"))?;
                }
            }
        }
    }
}

/// Stateful linear 3:2 interpolation, exclusively for the diagnostic provider.
/// Carries interpolation phase across arbitrarily split chunks.
#[derive(Default)]
struct DiagnosticResampler {
    previous: Option<i16>,
    phase: u8,
}

impl DiagnosticResampler {
    fn process(&mut self, samples: &[i16]) -> Vec<i16> {
        let mut output = Vec::with_capacity(samples.len() * 3 / 2 + 2);
        for &current in samples {
            if let Some(previous) = self.previous {
                while self.phase < 3 {
                    let left = i32::from(previous) * i32::from(3 - self.phase);
                    let right = i32::from(current) * i32::from(self.phase);
                    output.push(((left + right) / 3) as i16);
                    self.phase += 2;
                }
                self.phase -= 3;
            }
            self.previous = Some(current);
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampling_is_independent_of_chunk_boundaries() {
        let input: Vec<i16> = (0..100).map(|v| v * 100).collect();
        let expected = DiagnosticResampler::default().process(&input);
        let mut stream = DiagnosticResampler::default();
        let actual: Vec<i16> = input.chunks(7).flat_map(|v| stream.process(v)).collect();
        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 149);
        assert_eq!(&actual[..5], &[0, 66, 133, 200, 266]);
    }
}
