//! Desktop feedback remains available when the dashboard is closed. Command
//! transitions are delivered as bounded events, never sampled by a timer.
use super::*;
use crate::commands::{CommandFeedback, CommandFeedbackPhase};

fn current_feedback(
    mut event: CommandFeedback,
    latest: Option<CommandFeedback>,
) -> Option<CommandFeedback> {
    if let Some(latest) = latest {
        if latest.sequence > event.sequence
            && (latest.phase == CommandFeedbackPhase::Dismissed
                || latest.activation_id != event.activation_id)
        {
            return None;
        }
        let expired = match latest.phase {
            CommandFeedbackPhase::Succeeded => latest.age_ms >= 5_000,
            CommandFeedbackPhase::Failed => latest.age_ms >= 9_000,
            _ => false,
        };
        if latest.activation_id == event.activation_id && expired {
            return None;
        }
        if latest.sequence == event.sequence {
            event.age_ms = latest.age_ms;
        }
    }
    Some(event)
}

impl Controller {
    pub(super) fn start_command_notifications(&self) {
        if self.notifications_started.swap(true, Ordering::AcqRel) {
            return;
        }
        // Subscribe before spawning so even an immediate activation + success
        // remains visible. Subscription intentionally does not replay history.
        let mut events = self.commands.subscribe_feedback();
        let state = Arc::downgrade(&self.state);
        let commands = Arc::downgrade(&self.commands);
        let cancel = self.monitor_cancel.clone();
        tokio::spawn(async move {
            let dispatcher = crate::feedback::FeedbackDispatcher::new();
            loop {
                let received = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    event = events.recv() => event,
                };
                let (Some(state), Some(commands)) = (state.upgrade(), commands.upgrade()) else {
                    break;
                };
                let event = match received {
                    Ok(event) => Some(event),
                    // Feedback is bounded independently of audio and tools.
                    // Catch up directly to the latest state, without replaying
                    // a backlog of stale activations or losing the final result.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        events = commands.subscribe_feedback();
                        commands.feedback_snapshot()
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                let Some(event) = event else { continue };
                let (enabled, language) = {
                    let state = tokio::select! {
                        biased;
                        _ = cancel.cancelled() => break,
                        state = state.lock() => state,
                    };
                    (
                        state.config.agent.desktop_notifications,
                        crate::i18n::resolve_language(&state.config.interface.language),
                    )
                };
                if enabled {
                    if let Some(event) = current_feedback(event, commands.feedback_snapshot()) {
                        dispatcher.send(event, &language);
                    }
                } else {
                    dispatcher.dismiss();
                }
            }
            dispatcher.dismiss();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delayed_feedback_consumer_keeps_fresh_lifecycle_but_not_expired_or_dismissed_commands() {
        let activated = CommandFeedback {
            activation_id: 1,
            sequence: 1,
            phase: CommandFeedbackPhase::Activated,
            age_ms: 0,
        };
        let mut latest = CommandFeedback {
            phase: CommandFeedbackPhase::Succeeded,
            sequence: 3,
            age_ms: 200,
            ..activated
        };
        assert_eq!(current_feedback(activated, Some(latest)), Some(activated));
        assert_eq!(
            current_feedback(
                CommandFeedback {
                    age_ms: 0,
                    ..latest
                },
                Some(latest)
            ),
            Some(latest)
        );
        latest.age_ms = 5_000;
        assert!(current_feedback(activated, Some(latest)).is_none());
        assert!(current_feedback(latest, Some(latest)).is_none());
        latest.phase = CommandFeedbackPhase::Failed;
        assert!(current_feedback(latest, Some(latest)).is_some());
        latest.age_ms = 9_000;
        assert!(current_feedback(latest, Some(latest)).is_none());
        latest.phase = CommandFeedbackPhase::Dismissed;
        assert!(current_feedback(activated, Some(latest)).is_none());
        latest.phase = CommandFeedbackPhase::Activated;
        latest.activation_id = 2;
        assert!(current_feedback(activated, Some(latest)).is_none());
    }
}
