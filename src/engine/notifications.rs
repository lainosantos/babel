//! Desktop feedback remains available when the browser dashboard is closed.
//! One bounded queue and one OS-notification thread, isolated from audio.
use super::*;
use crate::commands::{CommandErrorScope, CommandPhase, CommandStatus};

#[derive(Default)]
struct NotificationGate {
    last: Option<(u64, &'static str)>,
}
impl NotificationGate {
    fn next(&mut self, status: &CommandStatus) -> Option<&'static str> {
        // Service readiness is a settings diagnostic, not a spoken command.
        // The activation ID can refer to an older command, so inspect scope too.
        if status.activation_id == 0 || status.error_scope == Some(CommandErrorScope::Service) {
            return None;
        }
        let key = match status.phase {
            CommandPhase::Activated => "agent.activated",
            CommandPhase::Transcribing if status.activation_id > 0 => "agent.processing",
            CommandPhase::Deciding | CommandPhase::Executing => "agent.processing",
            CommandPhase::Succeeded => "agent.succeeded",
            CommandPhase::Failed => "agent.failed",
            _ => return None,
        };
        let next = (status.activation_id, key);
        if self.last == Some(next) {
            return None;
        }
        self.last = Some(next);
        Some(key)
    }
}

impl Controller {
    pub(super) fn start_command_notifications(&self) {
        if self.notifications_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let (sender, receiver) = std::sync::mpsc::sync_channel::<(String, String)>(1);
        // OS notification APIs can block. Never put them on a Tokio/audio
        // worker, and never accumulate unbounded threads or notification jobs.
        if std::thread::Builder::new()
            .name("babel-notifications".into())
            .spawn(move || {
                #[cfg(target_os = "linux")]
                let mut replacement = None;
                while let Ok((summary, body)) = receiver.recv() {
                    let mut notification = notify_rust::Notification::new();
                    notification
                        .appname("Babel")
                        .summary(&summary)
                        .body(&body)
                        .timeout(3500);
                    #[cfg(target_os = "linux")]
                    if let Some(id) = replacement {
                        notification.id(id);
                    }
                    #[cfg(target_os = "linux")]
                    {
                        replacement = notification.show().ok().map(|handle| handle.id());
                    }
                    #[cfg(not(target_os = "linux"))]
                    let _ = notification.show();
                }
            })
            .is_err()
        {
            return;
        }
        let state = Arc::downgrade(&self.state);
        let commands = Arc::downgrade(&self.commands);
        let cancel = self.monitor_cancel.clone();
        tokio::spawn(async move {
            let mut gate = NotificationGate::default();
            let mut tick = tokio::time::interval(Duration::from_millis(100));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! { biased; _ = cancel.cancelled() => break, _ = tick.tick() => {} }
                let (Some(state), Some(commands)) = (state.upgrade(), commands.upgrade()) else {
                    break;
                };
                let (enabled, language) = {
                    let state = tokio::select! { biased; _ = cancel.cancelled() => break, state = state.lock() => state };
                    (
                        state.config.agent.desktop_notifications,
                        crate::i18n::resolve_language(&state.config.interface.language),
                    )
                };
                let status = commands.status();
                let key = gate.next(&status);
                if enabled && let Some(key) = key {
                    let _ = sender.try_send((
                        format!("Babel · {}", crate::i18n::text(&language, key)),
                        crate::i18n::text(&language, "agent.details"),
                    ));
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn notifications_coalesce_processing_and_failures_without_exposing_speech() {
        let mut gate = NotificationGate::default();
        let mut status = CommandStatus {
            phase: CommandPhase::Listening,
            wake_name: "Babel".into(),
            microphone_active: true,
            sequence: 1,
            activation_id: 0,
            command: None,
            tool: None,
            result: None,
            error: None,
            error_scope: None,
            dropped_frames: 0,
            whisper_endpoint: None,
            needle_endpoint: None,
        };
        assert!(gate.next(&status).is_none());
        status.phase = CommandPhase::Failed;
        assert!(gate.next(&status).is_none());
        status.activation_id = 1;
        status.phase = CommandPhase::Activated;
        assert_eq!(gate.next(&status), Some("agent.activated"));
        status.phase = CommandPhase::Deciding;
        assert_eq!(gate.next(&status), Some("agent.processing"));
        status.phase = CommandPhase::Executing;
        assert!(gate.next(&status).is_none());
        status.phase = CommandPhase::Succeeded;
        assert_eq!(gate.next(&status), Some("agent.succeeded"));
        status.phase = CommandPhase::Failed;
        status.error_scope = Some(CommandErrorScope::Service);
        assert!(
            gate.next(&status).is_none(),
            "historical activation must not turn a readiness error into a command notification"
        );
        status.error_scope = Some(CommandErrorScope::Command);
        assert_eq!(gate.next(&status), Some("agent.failed"));
        assert!(gate.next(&status).is_none());
    }
}
