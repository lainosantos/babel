//! A route owns devices and provider connections while its virtual endpoint is
//! selected: the microphone may be the system default or used by an application;
//! the speaker requires application playback. Session/file writers outlive this scope.
use super::*;
use crate::audio::activity::EndpointUse;
use std::future::Future;

#[derive(Clone, Copy)]
enum Selection {
    Original(TranscriptOrigin),
    MirroredMicrophone,
}

fn selected(usage: &EndpointUse, selection: Selection) -> (bool, [u64; 2]) {
    match selection {
        Selection::Original(TranscriptOrigin::Microphone) => {
            (usage.microphone, [usage.microphone_epoch, 0])
        }
        Selection::Original(TranscriptOrigin::Speaker) => (usage.speaker, [usage.speaker_epoch, 0]),
        Selection::MirroredMicrophone => (
            usage.microphone && usage.speaker,
            [usage.microphone_epoch, usage.speaker_epoch],
        ),
    }
}

fn inspection_error(usage: &EndpointUse, selection: Selection) -> Option<String> {
    usage.error.clone().or_else(|| match selection {
        Selection::Original(TranscriptOrigin::Microphone) => usage.microphone_error.clone(),
        Selection::Original(TranscriptOrigin::Speaker) => usage.speaker_error.clone(),
        Selection::MirroredMicrophone => usage
            .microphone_error
            .clone()
            .or_else(|| usage.speaker_error.clone()),
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
    usage: watch::Receiver<EndpointUse>,
    origin: TranscriptOrigin,
    metrics: Arc<RouteMetrics>,
    cancel: CancellationToken,
    run: F,
) -> Result<()>
where
    F: FnMut(CancellationToken) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    while_selected_source(usage, Selection::Original(origin), metrics, cancel, run).await
}

pub(super) async fn while_mirrored<F, Fut>(
    usage: watch::Receiver<EndpointUse>,
    metrics: Arc<RouteMetrics>,
    cancel: CancellationToken,
    run: F,
) -> Result<()>
where
    F: FnMut(CancellationToken) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    while_selected_source(usage, Selection::MirroredMicrophone, metrics, cancel, run).await
}

async fn while_selected_source<F, Fut>(
    mut usage: watch::Receiver<EndpointUse>,
    selection: Selection,
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
        let (active, epoch) = selected(&current, selection);
        let error = inspection_error(&current, selection);
        if !active || error.is_some() {
            waiting(&metrics, error);
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Ok(()),
                changed = usage.changed() => changed.context("Virtual device usage monitor closed")?,
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
                        break Err(anyhow!("Virtual device usage monitor closed"));
                    }
                    let current = usage.borrow_and_update().clone();
                    let (active, current_epoch) = selected(&current, selection);
                    // Epochs retain even a false->true transition coalesced by watch.
                    // A change in the other direction never restarts this route.
                    if !active || current_epoch != epoch || inspection_error(&current, selection).is_some() {
                        break Ok(());
                    }
                }
                result = &mut worker => return result.and_then(|()| {
                    if cancel.is_cancelled() { Ok(()) }
                    else { Err(anyhow!("Virtual device stream ended unexpectedly")) }
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
            .context("Timed out while suspending the virtual device")??;
        waiting(&metrics, inspection_error(&usage.borrow(), selection));
        selection_changed?;
    }
}

#[cfg(test)]
mod tests;
