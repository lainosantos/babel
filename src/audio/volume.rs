//! Endpoint volume belongs to the control plane, never to PCM callbacks.
//!
//! A verified control-only virtual endpoint exposes the physical output's
//! master control. Other cables keep their existing gain behavior and expose
//! a separate physical control instead of multiplying two synchronized gains.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "linux")]
use linux as backend;
#[cfg(target_os = "macos")]
use macos as backend;
#[cfg(target_os = "windows")]
use windows as backend;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

use super::activity::EndpointUse;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct VolumeState {
    pub level: f32,
    pub muted: bool,
}

impl VolumeState {
    fn differs(self, other: Self) -> bool {
        self.muted != other.muted || (self.level - other.level).abs() > 0.001
    }
}

pub(super) struct Snapshot {
    pub virtual_state: VolumeState,
    pub physical_state: VolumeState,
    pub synchronized: bool,
    pub limitation: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    pub device: String,
    pub level: Option<f32>,
    pub muted: bool,
    pub synchronized: bool,
    pub limitation: Option<String>,
    pub error: Option<String>,
}

struct Request {
    device: String,
    value: VolumeState,
    reply: oneshot::Sender<Result<()>>,
}

struct Completion {
    reply: oneshot::Sender<Result<()>>,
    result: Result<()>,
}

impl Completion {
    fn revoked(self) {
        let _ = self.reply.send(Err(BindingChanged.into()));
    }
}

fn publish_status(sender: &watch::Sender<Status>, value: Status, completion: Option<Completion>) {
    // The dashboard reads status immediately after a successful POST. Publish
    // its new physical level before waking that request's response handler.
    sender.send_replace(value);
    if let Some(Completion { reply, result }) = completion {
        let _ = reply.send(result);
    }
}

pub(crate) struct Service {
    playback: watch::Sender<String>,
    requests: mpsc::Sender<Request>,
    status: watch::Receiver<Status>,
    _cancel: tokio_util::sync::DropGuard,
}

impl Service {
    pub fn start(capture: String, playback: String, usage: watch::Receiver<EndpointUse>) -> Self {
        let (playback, changes) = watch::channel(playback);
        let (requests, receiver) = mpsc::channel(8);
        let (status, observation) = watch::channel(Status::default());
        let cancel = CancellationToken::new();
        tokio::spawn(run(
            capture,
            changes,
            usage,
            receiver,
            status,
            cancel.clone(),
        ));
        Self {
            playback,
            requests,
            status: observation,
            _cancel: cancel.drop_guard(),
        }
    }

    pub fn select(&self, device: String) {
        self.playback.send_if_modified(|current| {
            if *current == device {
                false
            } else {
                *current = device;
                true
            }
        });
    }

    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }

    pub fn request(
        &self,
        device: String,
        value: VolumeState,
    ) -> Result<oneshot::Receiver<Result<()>>> {
        ensure!(
            value.level.is_finite() && (0.0..=1.0).contains(&value.level),
            "Output volume must be between 0 and 1"
        );
        ensure!(
            *self.playback.borrow() == device,
            "The selected output changed; refresh its volume before trying again"
        );
        let (reply, receiver) = oneshot::channel();
        self.requests
            .try_send(Request {
                device,
                value,
                reply,
            })
            .context("Output volume control is busy")?;
        Ok(receiver)
    }
}

#[derive(Default)]
struct Follow {
    previous: Option<(VolumeState, VolumeState)>,
}

#[derive(Debug, PartialEq, Eq)]
enum Change {
    Virtual,
    Physical,
    None,
}

impl Follow {
    fn next(&self, snapshot: &Snapshot) -> Change {
        let Some((virtual_before, physical_before)) = self.previous else {
            return Change::Virtual;
        };
        // A physical knob remains authoritative if both endpoints change in
        // the same observation. Never undo a hardware mute with a stale echo.
        if snapshot.physical_state.differs(physical_before) {
            Change::Virtual
        } else if snapshot.virtual_state.differs(virtual_before) {
            Change::Physical
        } else {
            Change::None
        }
    }
}

async fn run(
    capture: String,
    mut playback: watch::Receiver<String>,
    mut usage: watch::Receiver<EndpointUse>,
    mut requests: mpsc::Receiver<Request>,
    status: watch::Sender<Status>,
    cancel: CancellationToken,
) {
    #[cfg(target_os = "linux")]
    let mut events = backend::changes(cancel.child_token());
    #[cfg(target_os = "linux")]
    let refresh = std::time::Duration::from_secs(2);
    #[cfg(not(target_os = "linux"))]
    let refresh = std::time::Duration::from_millis(250);
    let mut interval = tokio::time::interval(refresh);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut follow = Follow::default();
    let mut identity = (String::new(), 0, false);
    let mut pending: Option<Request> = None;
    loop {
        let device = playback.borrow_and_update().clone();
        let selected = usage.borrow_and_update().clone();
        let current = (
            device.clone(),
            selected.speaker_selection_epoch,
            selected.speaker_selected,
        );
        if identity != current {
            follow = Follow::default();
            identity = current;
        }
        let requested = pending.take();
        if device.is_empty() {
            publish_status(
                &status,
                Status::default(),
                requested.map(|request| Completion {
                    reply: request.reply,
                    result: Err(anyhow::anyhow!("Choose an output device first")),
                }),
            );
        } else {
            let result = tokio::select! {
                _ = cancel.cancelled() => break,
                result = backend::inspect(&capture, &device) => result,
            };
            // A completed old query must not adjust a newly selected output.
            if playback.has_changed().unwrap_or(true) || usage.has_changed().unwrap_or(true) {
                pending = requested;
                continue;
            }
            match result {
                Ok(mut snapshot) => {
                    let mut failure = None;
                    let mut revoked = false;
                    let mut completion = None;
                    if let Some(request) = requested {
                        let result = if request.device != device {
                            Err(anyhow::anyhow!(
                                "The selected output changed; refresh its volume before trying again"
                            ))
                        } else {
                            guarded(&mut playback, &mut usage, &cancel, |token| {
                                backend::set_physical(&device, request.value, token)
                            })
                            .await
                        };
                        if let Err(error) = &result {
                            revoked = error.is::<BindingChanged>();
                            failure = Some(format!("{error:#}"));
                        } else {
                            snapshot.physical_state = request.value;
                            follow = Follow::default();
                        }
                        completion = Some(Completion {
                            reply: request.reply,
                            result,
                        });
                    }
                    if revoked
                        || !binding_matches(&playback, &usage, &device, &selected)
                        || cancel.is_cancelled()
                    {
                        follow = Follow::default();
                        if let Some(completion) = completion {
                            completion.revoked();
                        }
                        continue;
                    }
                    let active = selected.speaker_selected
                        && selected.error.is_none()
                        && selected.speaker_error.is_none();
                    if snapshot.synchronized
                        && active
                        && failure.is_none()
                        && !playback.has_changed().unwrap_or(true)
                        && !usage.has_changed().unwrap_or(true)
                    {
                        let action = follow.next(&snapshot);
                        let updated = match action {
                            Change::Virtual => {
                                guarded(&mut playback, &mut usage, &cancel, |token| {
                                    backend::set_virtual(&capture, snapshot.physical_state, token)
                                })
                                .await
                            }
                            Change::Physical => {
                                guarded(&mut playback, &mut usage, &cancel, |token| {
                                    backend::set_physical(&device, snapshot.virtual_state, token)
                                })
                                .await
                            }
                            Change::None => Ok(()),
                        };
                        if updated
                            .as_ref()
                            .err()
                            .is_some_and(|error| error.is::<BindingChanged>())
                            || !binding_matches(&playback, &usage, &device, &selected)
                            || cancel.is_cancelled()
                        {
                            follow = Follow::default();
                            if let Some(completion) = completion {
                                completion.revoked();
                            }
                            continue;
                        }
                        match updated {
                            Ok(()) => {
                                if action == Change::Virtual {
                                    snapshot.virtual_state = snapshot.physical_state;
                                }
                                if action == Change::Physical {
                                    snapshot.physical_state = snapshot.virtual_state;
                                }
                                follow.previous =
                                    Some((snapshot.virtual_state, snapshot.physical_state));
                            }
                            Err(error) => {
                                failure =
                                    Some(format!("Could not synchronize output volume: {error:#}"));
                                follow = Follow::default();
                                // Restore the truthful slider if the physical
                                // write failed, especially for a rejected mute.
                                if action == Change::Physical {
                                    let _ = guarded(&mut playback, &mut usage, &cancel, |token| {
                                        backend::set_virtual(
                                            &capture,
                                            snapshot.physical_state,
                                            token,
                                        )
                                    })
                                    .await;
                                }
                            }
                        }
                    } else {
                        follow = Follow::default();
                    }
                    if !binding_matches(&playback, &usage, &device, &selected)
                        || cancel.is_cancelled()
                    {
                        follow = Follow::default();
                        if let Some(completion) = completion {
                            completion.revoked();
                        }
                        continue;
                    }
                    publish_status(
                        &status,
                        Status {
                            device: device.clone(),
                            level: Some(snapshot.physical_state.level),
                            muted: snapshot.physical_state.muted,
                            synchronized: snapshot.synchronized && active && failure.is_none(),
                            limitation: snapshot.limitation,
                            error: failure,
                        },
                        completion,
                    );
                }
                Err(error) => {
                    follow = Follow::default();
                    let message = format!("Output volume control unavailable: {error:#}");
                    let completion = requested.map(|request| Completion {
                        reply: request.reply,
                        result: Err(anyhow::anyhow!(message.clone())),
                    });
                    publish_status(
                        &status,
                        Status {
                            device: device.clone(),
                            error: Some(message),
                            ..Default::default()
                        },
                        completion,
                    );
                }
            }
        }
        // Coalesce notifications generated by our own writes and slider drags.
        tokio::select! { _ = cancel.cancelled() => break, _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => () }
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = status.closed() => break,
            changed = playback.changed() => { if changed.is_err() { break; } },
            changed = usage.changed() => { if changed.is_err() { break; } },
            request = requests.recv() => { let Some(request) = request else { break; }; pending = Some(request); },
            _ = interval.tick() => (),
            _ = platform_changed(
                #[cfg(target_os = "linux")] &mut events,
            ) => (),
        }
    }
}

#[derive(Debug)]
struct BindingChanged;
impl std::fmt::Display for BindingChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("The selected output changed during volume adjustment")
    }
}
impl std::error::Error for BindingChanged {}

fn same_selection(left: &EndpointUse, right: &EndpointUse) -> bool {
    left.speaker_selection_epoch == right.speaker_selection_epoch
        && left.speaker_selected == right.speaker_selected
        && left.speaker_error == right.speaker_error
        && left.error == right.error
}

fn binding_matches(
    playback: &watch::Receiver<String>,
    usage: &watch::Receiver<EndpointUse>,
    device: &str,
    selection: &EndpointUse,
) -> bool {
    *playback.borrow() == device && same_selection(&usage.borrow(), selection)
}

/// A queued OS mutation loses authority when its endpoint binding changes.
/// Native blocking workers also check this token immediately before writing.
async fn guarded<F, Fut>(
    playback: &mut watch::Receiver<String>,
    usage: &mut watch::Receiver<EndpointUse>,
    cancel: &CancellationToken,
    operation: F,
) -> Result<()>
where
    F: FnOnce(CancellationToken) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    if playback.has_changed().unwrap_or(true) || usage.has_changed().unwrap_or(true) {
        return Err(BindingChanged.into());
    }
    let observation = usage.borrow().clone();
    let token = cancel.child_token();
    let _guard = token.clone().drop_guard();
    let future = operation(token);
    tokio::pin!(future);
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(BindingChanged.into()),
            _ = playback.changed() => return Err(BindingChanged.into()),
            changed = usage.changed() => {
                let current = usage.borrow().clone();
                if changed.is_err() || !same_selection(&current, &observation) {
                    return Err(BindingChanged.into());
                }
            }
            result = &mut future => return result,
        }
    }
}

async fn platform_changed(#[cfg(target_os = "linux")] events: &mut watch::Receiver<()>) {
    #[cfg(target_os = "linux")]
    if events.changed().await.is_ok() {
        return;
    }
    std::future::pending::<()>().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state(level: f32, muted: bool) -> VolumeState {
        VolumeState { level, muted }
    }
    fn snapshot(virtual_state: VolumeState, physical_state: VolumeState) -> Snapshot {
        Snapshot {
            virtual_state,
            physical_state,
            synchronized: true,
            limitation: None,
        }
    }
    #[test]
    fn binding_adopts_physical_level_without_normalizing_hardware() {
        assert_eq!(
            Follow::default().next(&snapshot(state(1.0, false), state(0.5, false))),
            Change::Virtual
        );
    }
    #[test]
    fn virtual_slider_controls_full_physical_range_once() {
        let follow = Follow {
            previous: Some((state(0.5, false), state(0.5, false))),
        };
        assert_eq!(
            follow.next(&snapshot(state(1.0, false), state(0.5, false))),
            Change::Physical
        );
        assert_eq!(
            follow.next(&snapshot(state(0.5, true), state(0.5, false))),
            Change::Physical
        );
    }
    #[test]
    fn external_physical_mute_wins_over_simultaneous_virtual_change() {
        let follow = Follow {
            previous: Some((state(0.5, false), state(0.5, false))),
        };
        assert_eq!(
            follow.next(&snapshot(state(1.0, false), state(0.5, true))),
            Change::Virtual
        );
    }
    #[test]
    fn rounding_and_own_writes_do_not_feed_back() {
        let follow = Follow {
            previous: Some((state(0.5, false), state(0.5, false))),
        };
        assert_eq!(
            follow.next(&snapshot(state(0.50001, false), state(0.50001, false))),
            Change::None
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn successful_request_acknowledges_only_after_new_status_is_visible() {
        let (status, observed) = watch::channel(Status {
            device: "speaker".into(),
            level: Some(0.5),
            ..Default::default()
        });
        let (reply, response) = oneshot::channel::<Result<()>>();
        let immediate_status_read = tokio::spawn(async move {
            response.await.unwrap().unwrap();
            // Equivalent to the dashboard's GET /status after its POST returns.
            let current = observed.borrow().clone();
            assert_eq!(current.level, Some(0.8));
            assert!(current.muted);
        });
        publish_status(
            &status,
            Status {
                device: "speaker".into(),
                level: Some(0.8),
                muted: true,
                ..Default::default()
            },
            Some(Completion {
                reply,
                result: Ok(()),
            }),
        );
        immediate_status_read.await.unwrap();
    }

    #[tokio::test]
    async fn failed_request_publishes_error_before_acknowledgment() {
        let (status, observed) = watch::channel(Status {
            level: Some(0.5),
            ..Default::default()
        });
        let (reply, response) = oneshot::channel();
        publish_status(
            &status,
            Status {
                error: Some("output unavailable".into()),
                ..Default::default()
            },
            Some(Completion {
                reply,
                result: Err(anyhow::anyhow!("output unavailable")),
            }),
        );
        assert_eq!(
            response.await.unwrap().unwrap_err().to_string(),
            "output unavailable"
        );
        assert_eq!(
            observed.borrow().error.as_deref(),
            Some("output unavailable")
        );
        assert_eq!(observed.borrow().level, None);
    }

    #[tokio::test]
    async fn revoked_binding_does_not_acknowledge_a_stale_success() {
        let (reply, response) = oneshot::channel();
        Completion {
            reply,
            result: Ok(()),
        }
        .revoked();
        assert!(response.await.unwrap().unwrap_err().is::<BindingChanged>());
    }

    #[tokio::test]
    async fn switching_devices_revokes_a_queued_native_write() {
        let (select, mut playback) = watch::channel("old-speaker".to_owned());
        let (_observer, mut usage) = watch::channel(EndpointUse::default());
        let (entered, started) = oneshot::channel();
        let (release, wait) = oneshot::channel();
        let wrote = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let attempted = wrote.clone();
        let task = tokio::spawn(async move {
            guarded(
                &mut playback,
                &mut usage,
                &CancellationToken::new(),
                |cancel| async move {
                    // Simulate a blocking platform query already queued when the
                    // user switches speakers. It outlives its dropped async handle.
                    tokio::spawn(async move {
                        let _ = entered.send(());
                        let _ = wait.await;
                        if !cancel.is_cancelled() {
                            attempted.store(true, std::sync::atomic::Ordering::SeqCst);
                        }
                    })
                    .await?;
                    Ok(())
                },
            )
            .await
        });
        started.await.unwrap();
        select.send_replace("new-speaker".to_owned());
        assert!(task.await.unwrap().is_err());
        release.send(()).unwrap();
        tokio::task::yield_now().await;
        assert!(!wrote.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn unrelated_microphone_activity_does_not_cancel_output_adjustments() {
        let (_select, mut playback) = watch::channel("speaker".to_owned());
        let (observer, mut usage) = watch::channel(EndpointUse::default());
        let (entered, started) = oneshot::channel();
        let (release, wait) = oneshot::channel();
        let task = tokio::spawn(async move {
            guarded(
                &mut playback,
                &mut usage,
                &CancellationToken::new(),
                |_| async move {
                    entered.send(()).unwrap();
                    wait.await?;
                    Ok(())
                },
            )
            .await
        });
        started.await.unwrap();
        observer.send_modify(|state| {
            state.microphone = true;
            state.microphone_epoch += 1;
        });
        release.send(()).unwrap();
        task.await.unwrap().unwrap();
    }
}
