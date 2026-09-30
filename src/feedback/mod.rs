//! Bounded desktop command feedback, independent of the audio callbacks.
//! The native panel receives only localized phase labels, never spoken content.
pub mod renderer;

use crate::commands::{CommandFeedback, CommandFeedbackPhase};
use renderer::{FeedbackMessage, FeedbackPhase};
use std::{
    collections::VecDeque,
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
    sync::Notify,
};

const READY: &[u8] = b"BABEL_FEEDBACK_READY\n";

struct Pending {
    state: Mutex<Queue>,
    changed: Notify,
}
#[derive(Default)]
struct Queue {
    events: VecDeque<Queued>,
    activation: u64,
    sequence: u64,
    closed: bool,
}
struct Queued {
    message: FeedbackMessage,
    created: Instant,
    initial_age: Duration,
}
impl Queued {
    fn expired(&self) -> bool {
        let lifetime = match self.message.phase {
            FeedbackPhase::Succeeded => 5,
            FeedbackPhase::Failed => 9,
            _ => return false,
        };
        self.initial_age + self.created.elapsed() >= Duration::from_secs(lifetime)
    }
}
impl Queue {
    fn push(&mut self, event: CommandFeedback, language: &str) {
        if self.closed || event.sequence <= self.sequence {
            return;
        }
        self.sequence = event.sequence;
        if event.activation_id != self.activation || event.phase == CommandFeedbackPhase::Dismissed
        {
            self.events.clear();
            self.activation = event.activation_id;
        }
        let message = localized(event, language);
        // Preserve activation, processing and the latest outcome for the current
        // command. An arbitrarily slow desktop service cannot build a backlog.
        self.events
            .retain(|queued| queued.message.phase != message.phase);
        if self.events.len() == 4 {
            self.events.pop_front();
        }
        self.events.push_back(Queued {
            message,
            created: Instant::now(),
            initial_age: Duration::from_millis(event.age_ms),
        });
    }
}

pub struct FeedbackDispatcher {
    pending: Arc<Pending>,
}
impl Default for FeedbackDispatcher {
    fn default() -> Self {
        Self::new()
    }
}
impl FeedbackDispatcher {
    pub fn new() -> Self {
        let pending = Arc::new(Pending {
            state: Mutex::new(Queue::default()),
            changed: Notify::new(),
        });
        tokio::spawn(run(pending.clone()));
        Self { pending }
    }
    pub fn send(&self, event: CommandFeedback, language: &str) {
        self.pending
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(event, language);
        self.pending.changed.notify_one();
    }
    pub fn dismiss(&self) {
        let mut state = self.pending.state.lock().unwrap_or_else(|e| e.into_inner());
        state.events.clear();
        let activation = state.activation;
        state.events.push_back(Queued {
            message: FeedbackMessage {
                activation_id: activation,
                phase: FeedbackPhase::Dismissed,
                title: String::new(),
                detail: String::new(),
                motion: false,
            },
            created: Instant::now(),
            initial_age: Duration::ZERO,
        });
        drop(state);
        self.pending.changed.notify_one();
    }
}
impl Drop for FeedbackDispatcher {
    fn drop(&mut self) {
        let mut state = self.pending.state.lock().unwrap_or_else(|e| e.into_inner());
        state.closed = true;
        state.events.clear();
        drop(state);
        self.pending.changed.notify_one();
    }
}
fn localized(event: CommandFeedback, language: &str) -> FeedbackMessage {
    let (phase, key) = match event.phase {
        CommandFeedbackPhase::Activated => (FeedbackPhase::Activated, "agent.activated"),
        CommandFeedbackPhase::Processing => (FeedbackPhase::Processing, "agent.processing"),
        CommandFeedbackPhase::Succeeded => (FeedbackPhase::Succeeded, "agent.succeeded"),
        CommandFeedbackPhase::Failed => (FeedbackPhase::Failed, "agent.failed"),
        CommandFeedbackPhase::Dismissed => (FeedbackPhase::Dismissed, ""),
    };
    FeedbackMessage {
        activation_id: event.activation_id,
        phase,
        title: if key.is_empty() {
            String::new()
        } else {
            crate::i18n::text(language, key)
        },
        detail: if key.is_empty() {
            String::new()
        } else {
            crate::i18n::text(language, &format!("{key}_hint"))
        },
        motion: true,
    }
}

struct Panel {
    child: Child,
    input: ChildStdin,
}
impl Panel {
    async fn start() -> anyhow::Result<Self> {
        let current = std::env::current_exe()?;
        let name = if cfg!(windows) {
            "babel-feedback.exe"
        } else {
            "babel-feedback"
        };
        let path = current
            .parent()
            .ok_or_else(|| anyhow::anyhow!("No application directory"))?
            .join(name);
        let mut command = Command::new(path);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000); // no console for the window helper
        let mut child = command.spawn()?;
        let mut output = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("No panel readiness pipe"))?;
        let mut ready = [0u8; READY.len()];
        tokio::time::timeout(Duration::from_secs(3), output.read_exact(&mut ready)).await??;
        anyhow::ensure!(
            ready == READY,
            "Native feedback readiness protocol mismatch"
        );
        let input = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("No panel input pipe"))?;
        Ok(Self { child, input })
    }
    async fn send(&mut self, message: &FeedbackMessage) -> anyhow::Result<()> {
        anyhow::ensure!(self.child.try_wait()?.is_none(), "Feedback panel exited");
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');
        anyhow::ensure!(line.len() <= 4096, "Feedback message exceeds the limit");
        tokio::time::timeout(Duration::from_millis(500), async {
            self.input.write_all(&line).await?;
            self.input.flush().await
        })
        .await??;
        Ok(())
    }
    async fn stop(mut self) {
        let _ = self.input.shutdown().await;
        drop(self.input);
        if tokio::time::timeout(Duration::from_millis(500), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
        }
    }
}
#[derive(Default)]
struct SystemNotice {
    #[cfg(target_os = "linux")]
    handle: Option<notify_rust::NotificationHandle>,
}
impl SystemNotice {
    fn show(self, message: FeedbackMessage) -> Self {
        if message.phase == FeedbackPhase::Dismissed {
            #[cfg(target_os = "linux")]
            if let Some(handle) = self.handle {
                handle.close();
            }
            return Self::default();
        }
        let mut notice = notify_rust::Notification::new();
        notice
            .appname("Babel")
            .summary(&format!("Babel · {}", message.title))
            .body(&message.detail)
            .timeout(if message.phase == FeedbackPhase::Failed {
                9000
            } else {
                5000
            });
        #[cfg(target_os = "linux")]
        {
            notice
                .icon("org.babel.audio")
                .hint(notify_rust::Hint::SuppressSound(true))
                .hint(notify_rust::Hint::Transient(true));
            if let Some(handle) = &self.handle {
                notice.id(handle.id());
            }
            Self {
                handle: notice.show().ok().or(self.handle),
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = notice.show();
            self
        }
    }
}
async fn run(pending: Arc<Pending>) {
    let mut panel: Option<Panel> = None;
    let mut retry_after = Instant::now();
    let mut fallback = SystemNotice::default();
    loop {
        let next = {
            let mut queue = pending.state.lock().unwrap_or_else(|e| e.into_inner());
            if queue.closed {
                break;
            }
            if queue.events.back().is_some_and(Queued::expired) {
                queue.events.clear();
            }
            queue.events.pop_front()
        };
        let Some(next) = next else {
            if let Some(ready) = &mut panel {
                tokio::select! {
                    _ = pending.changed.notified() => {},
                    _ = ready.child.wait() => { panel = None; },
                }
            } else {
                pending.changed.notified().await;
            }
            continue;
        };
        if next.expired() {
            continue;
        }
        let is_dismiss = next.message.phase == FeedbackPhase::Dismissed;
        if panel
            .as_mut()
            .is_some_and(|panel| !panel.child.try_wait().is_ok_and(|status| status.is_none()))
            && let Some(old) = panel.take()
        {
            old.stop().await;
        }
        if panel.is_none() && !is_dismiss && Instant::now() >= retry_after {
            match Panel::start().await {
                Ok(ready) => {
                    panel = Some(ready);
                    fallback = tokio::task::spawn_blocking(move || {
                        fallback.show(FeedbackMessage {
                            activation_id: 0,
                            phase: FeedbackPhase::Dismissed,
                            title: String::new(),
                            detail: String::new(),
                            motion: false,
                        })
                    })
                    .await
                    .unwrap_or_default();
                }
                Err(_) => {
                    retry_after = Instant::now() + Duration::from_secs(30);
                }
            }
        }
        // Disable/exit may arrive while the window initializes. Never display a
        // stale command after the user has switched feedback off.
        let obsolete = {
            let queue = pending.state.lock().unwrap_or_else(|e| e.into_inner());
            next.expired()
                || queue.closed
                || queue.activation > next.message.activation_id
                || queue.events.back().is_some_and(Queued::expired)
                || queue
                    .events
                    .iter()
                    .any(|event| event.message.phase == FeedbackPhase::Dismissed)
        };
        if obsolete {
            continue;
        }
        if let Some(ready) = &mut panel {
            if ready.send(&next.message).await.is_ok() {
                continue;
            }
            if let Some(old) = panel.take() {
                old.stop().await;
            }
            retry_after = Instant::now() + Duration::from_secs(30);
        }
        // One OS call at a time, outside the async/audio workers. Newer feedback
        // coalesces in the small mailbox while an OS API is busy.
        fallback = tokio::task::spawn_blocking(move || fallback.show(next.message))
            .await
            .unwrap_or_default();
    }
    if let Some(panel) = panel {
        panel.stop().await;
    }
    let _ = tokio::task::spawn_blocking(move || {
        fallback.show(FeedbackMessage {
            activation_id: 0,
            phase: FeedbackPhase::Dismissed,
            title: String::new(),
            detail: String::new(),
            motion: false,
        })
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(id: u64, sequence: u64, phase: CommandFeedbackPhase) -> CommandFeedback {
        CommandFeedback {
            activation_id: id,
            sequence,
            phase,
            age_ms: 0,
        }
    }
    #[test]
    fn backpressure_preserves_quick_lifecycle_but_never_accumulates_old_commands() {
        let mut queue = Queue::default();
        for id in 1..=100 {
            for (offset, phase) in [
                CommandFeedbackPhase::Activated,
                CommandFeedbackPhase::Processing,
                CommandFeedbackPhase::Processing,
                CommandFeedbackPhase::Succeeded,
            ]
            .into_iter()
            .enumerate()
            {
                queue.push(event(id, id * 4 + offset as u64, phase), "pt");
            }
        }
        assert_eq!(queue.events.len(), 3);
        assert!(queue.events.iter().all(|e| e.message.activation_id == 100));
        assert_eq!(
            queue.events.back().unwrap().message.phase,
            FeedbackPhase::Succeeded
        );
        queue.push(event(100, 500, CommandFeedbackPhase::Dismissed), "pt");
        assert_eq!(queue.events.len(), 1);
        assert_eq!(queue.events[0].message.phase, FeedbackPhase::Dismissed);
    }
    #[test]
    fn old_terminal_feedback_is_not_replayed_and_native_payload_is_phase_only() {
        let mut queue = Queue::default();
        let mut completed = event(1, 1, CommandFeedbackPhase::Succeeded);
        completed.age_ms = 6000;
        queue.push(completed, "en");
        assert!(queue.events[0].expired());
        let message = &queue.events[0].message;
        let json = serde_json::to_value(message).unwrap();
        assert!(
            json.get("command").is_none()
                && json.get("tool").is_none()
                && json.get("result").is_none()
        );
        assert!(!message.title.contains("agent."));
        assert!(!message.detail.contains("agent."));
        queue.push(event(1, 1, CommandFeedbackPhase::Failed), "en");
        assert_eq!(queue.events[0].message.phase, FeedbackPhase::Succeeded);
    }
}
