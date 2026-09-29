//! A route owns devices and provider connections only while an application uses
//! its virtual endpoint. Session identity and file writers live outside this scope.
use super::*;
use crate::audio::activity::EndpointUse;
use std::future::Future;

fn selected(usage: &EndpointUse, origin: TranscriptOrigin) -> (bool, u64) {
    match origin {
        TranscriptOrigin::Microphone => (usage.microphone, usage.microphone_epoch),
        TranscriptOrigin::Speaker => (usage.speaker, usage.speaker_epoch),
    }
}

fn inspection_error(usage: &EndpointUse, origin: TranscriptOrigin) -> Option<String> {
    usage.error.clone().or_else(|| match origin {
        TranscriptOrigin::Microphone => usage.microphone_error.clone(),
        TranscriptOrigin::Speaker => usage.speaker_error.clone(),
    })
}

fn reset_levels(metrics: &RouteMetrics) {
    metrics.input_level.store(0, Ordering::Relaxed);
    metrics.output_level.store(0, Ordering::Relaxed);
    metrics.audio.passthrough_level.store(0, Ordering::Relaxed);
}

fn waiting(metrics: &RouteMetrics, error: Option<String>) {
    reset_levels(metrics);
    *metrics
        .activity_error
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = error;
    // These failures describe handles that have now been closed. The next
    // activation rechecks the currently selected physical endpoints.
    *metrics
        .audio
        .capture_error
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    *metrics
        .audio
        .playback_error
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    metrics.state("waiting_for_app");
}

pub(super) async fn while_selected<F, Fut>(
    mut usage: watch::Receiver<EndpointUse>,
    origin: TranscriptOrigin,
    metrics: Arc<RouteMetrics>,
    cancel: CancellationToken,
    mut run: F,
) -> Result<()>
where
    F: FnMut(CancellationToken) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    loop {
        if cancel.is_cancelled() {
            reset_levels(&metrics);
            return Ok(());
        }
        let current = usage.borrow_and_update().clone();
        let (active, epoch) = selected(&current, origin);
        let error = inspection_error(&current, origin);
        if !active || error.is_some() {
            waiting(&metrics, error);
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Ok(()),
                changed = usage.changed() => changed.context("Monitor de uso dos dispositivos virtuais encerrado")?,
            }
            continue;
        }
        *metrics
            .activity_error
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        metrics.state("connecting");
        let active_cancel = cancel.child_token();
        let _cancel_on_drop = active_cancel.clone().drop_guard();
        let worker = run(active_cancel.clone());
        tokio::pin!(worker);
        let selection_changed = loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break Ok(()),
                changed = usage.changed() => {
                    if changed.is_err() {
                        break Err(anyhow!("Monitor de uso dos dispositivos virtuais encerrado"));
                    }
                    let current = usage.borrow_and_update().clone();
                    let (active, current_epoch) = selected(&current, origin);
                    // Epochs retain even a false->true transition coalesced by watch.
                    // A change in the other direction never restarts this route.
                    if !active || current_epoch != epoch || inspection_error(&current, origin).is_some() {
                        break Ok(());
                    }
                }
                result = &mut worker => return result.and_then(|()| {
                    if cancel.is_cancelled() { Ok(()) }
                    else { Err(anyhow!("O fluxo do dispositivo virtual encerrou inesperadamente")) }
                }),
            }
        };
        // Fence queued playback immediately, then await every old worker before
        // reopening. Dropping their channels also fences late provider events.
        metrics
            .audio
            .playback_generation
            .fetch_add(1, Ordering::AcqRel);
        active_cancel.cancel();
        tokio::time::timeout(Duration::from_secs(4), &mut worker)
            .await
            .context("Tempo limite ao suspender o dispositivo virtual")??;
        waiting(&metrics, inspection_error(&usage.borrow(), origin));
        selection_changed?;
    }
}

#[cfg(test)]
mod tests;
