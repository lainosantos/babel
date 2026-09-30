//! Small, content-free command events. These are produced by control/inference
//! workers, never by the real-time audio callback or by tool execution itself.
use std::{sync::Mutex, time::Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use super::{CommandErrorScope, CommandPhase, CommandStatus};

const FEEDBACK_QUEUE: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandFeedbackPhase {
    Activated,
    Processing,
    Succeeded,
    Failed,
    Dismissed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandFeedback {
    pub activation_id: u64,
    pub sequence: u64,
    pub phase: CommandFeedbackPhase,
    /// Zero on publication; the status snapshot computes time since publication.
    /// This is monotonic elapsed time, independent of wall-clock adjustments.
    pub age_ms: u64,
}

struct LastFeedback {
    event: CommandFeedback,
    published: Instant,
}

pub(super) struct FeedbackPublisher {
    sender: broadcast::Sender<CommandFeedback>,
    last: Mutex<Option<LastFeedback>>,
}

impl FeedbackPublisher {
    pub(super) fn new() -> Self {
        Self {
            sender: broadcast::channel(FEEDBACK_QUEUE).0,
            last: Mutex::new(None),
        }
    }

    pub(super) fn subscribe(&self) -> broadcast::Receiver<CommandFeedback> {
        // Subscription deliberately has no replay: opening settings or enabling
        // notifications must not resurface a command that already completed.
        self.sender.subscribe()
    }

    pub(super) fn snapshot(&self) -> Option<CommandFeedback> {
        self.last
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|last| CommandFeedback {
                age_ms: last.published.elapsed().as_millis().min(u64::MAX.into()) as u64,
                ..last.event
            })
    }

    /// Caller holds the status lock so events keep the same order as mutations.
    pub(super) fn status_changed(&self, status: &CommandStatus) {
        if status.activation_id == 0 {
            return;
        }
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        let previous = last.as_ref().map(|last| last.event);
        let in_progress = previous.is_some_and(|event| {
            event.activation_id == status.activation_id
                && matches!(
                    event.phase,
                    CommandFeedbackPhase::Activated | CommandFeedbackPhase::Processing
                )
        });
        let phase = if status.error_scope == Some(CommandErrorScope::Service) {
            // A startup/readiness error is shown in settings, never as a spoken
            // command failure; clear any in-flight visual instead.
            if !in_progress {
                return;
            }
            CommandFeedbackPhase::Dismissed
        } else {
            match status.phase {
                CommandPhase::Activated => CommandFeedbackPhase::Activated,
                CommandPhase::Transcribing | CommandPhase::Deciding | CommandPhase::Executing
                    if in_progress =>
                {
                    CommandFeedbackPhase::Processing
                }
                CommandPhase::Succeeded if in_progress => CommandFeedbackPhase::Succeeded,
                CommandPhase::Failed if in_progress => CommandFeedbackPhase::Failed,
                CommandPhase::Disabled | CommandPhase::Inactive => CommandFeedbackPhase::Dismissed,
                CommandPhase::Listening if in_progress => CommandFeedbackPhase::Dismissed,
                _ => return,
            }
        };
        if previous.is_some_and(|event| {
            event.activation_id == status.activation_id && event.phase == phase
        }) {
            return;
        }
        let event = CommandFeedback {
            activation_id: status.activation_id,
            sequence: status.sequence,
            phase,
            age_ms: 0,
        };
        *last = Some(LastFeedback {
            event,
            published: Instant::now(),
        });
        // No consumer may block command processing, inference or audio routing.
        // A lagged consumer obtains the latest state from snapshot instead.
        let _ = self.sender.send(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::AgentConfig;

    fn transition(publisher: &FeedbackPublisher, status: &mut CommandStatus, phase: CommandPhase) {
        status.phase = phase;
        status.sequence += 1;
        status.error_scope = (phase == CommandPhase::Failed).then_some(CommandErrorScope::Command);
        publisher.status_changed(status);
    }

    #[test]
    fn fast_lifecycle_keeps_activation_and_terminal_and_coalesces_processing() {
        let publisher = FeedbackPublisher::new();
        let mut events = publisher.subscribe();
        let mut status = CommandStatus::initial(&AgentConfig::default());
        status.activation_id = 1;
        status.command = Some("private spoken words".into());
        status.tool = Some("private tool name".into());
        for phase in [
            CommandPhase::Activated,
            CommandPhase::Transcribing,
            CommandPhase::Deciding,
            CommandPhase::Executing,
            CommandPhase::Succeeded,
            CommandPhase::Listening,
        ] {
            transition(&publisher, &mut status, phase);
        }
        let mut received = Vec::new();
        while let Ok(event) = events.try_recv() {
            received.push(event);
        }
        assert_eq!(
            received.iter().map(|event| event.phase).collect::<Vec<_>>(),
            [
                CommandFeedbackPhase::Activated,
                CommandFeedbackPhase::Processing,
                CommandFeedbackPhase::Succeeded
            ]
        );
        assert!(
            received
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
        assert_eq!(
            publisher.snapshot().unwrap().phase,
            CommandFeedbackPhase::Succeeded
        );
        let serialized = serde_json::to_value(&received).unwrap();
        assert_eq!(serialized[0].as_object().unwrap().len(), 4);
        assert!(!serialized.to_string().contains("private"));
        assert_eq!(serialized[0]["phase"], "activated");
        assert!(matches!(
            publisher.subscribe().try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn unrelated_errors_and_settings_changes_do_not_replay_old_command_failure() {
        let publisher = FeedbackPublisher::new();
        let mut events = publisher.subscribe();
        let mut status = CommandStatus::initial(&AgentConfig::default());
        transition(&publisher, &mut status, CommandPhase::Failed);
        assert!(events.try_recv().is_err());
        status.activation_id = 1;
        transition(&publisher, &mut status, CommandPhase::Activated);
        transition(&publisher, &mut status, CommandPhase::Succeeded);
        while events.try_recv().is_ok() {}
        status.phase = CommandPhase::Failed;
        status.error_scope = Some(CommandErrorScope::Service);
        publisher.status_changed(&status);
        status.error_scope = Some(CommandErrorScope::Command);
        publisher.status_changed(&status);
        assert!(events.try_recv().is_err());
        transition(&publisher, &mut status, CommandPhase::Inactive);
        assert_eq!(
            events.try_recv().unwrap().phase,
            CommandFeedbackPhase::Dismissed
        );
        transition(&publisher, &mut status, CommandPhase::Disabled);
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn microphone_loss_invalidates_pending_success_and_next_wake_starts_fresh() {
        let publisher = FeedbackPublisher::new();
        let mut status = CommandStatus::initial(&AgentConfig::default());
        status.activation_id = 1;
        transition(&publisher, &mut status, CommandPhase::Activated);
        transition(&publisher, &mut status, CommandPhase::Inactive);
        transition(&publisher, &mut status, CommandPhase::Succeeded);
        assert_eq!(
            publisher.snapshot().unwrap().phase,
            CommandFeedbackPhase::Dismissed
        );
        status.activation_id = 2;
        transition(&publisher, &mut status, CommandPhase::Activated);
        transition(&publisher, &mut status, CommandPhase::Failed);
        assert_eq!(
            publisher.snapshot().unwrap().phase,
            CommandFeedbackPhase::Failed
        );
        assert_eq!(publisher.snapshot().unwrap().activation_id, 2);
    }

    #[test]
    fn slow_consumers_use_bounded_storage_and_can_recover_latest_terminal() {
        let publisher = FeedbackPublisher::new();
        let mut slow = publisher.subscribe();
        let mut status = CommandStatus::initial(&AgentConfig::default());
        for activation in 1..=64 {
            status.activation_id = activation;
            transition(&publisher, &mut status, CommandPhase::Activated);
            transition(&publisher, &mut status, CommandPhase::Succeeded);
        }
        assert_eq!(publisher.sender.len(), FEEDBACK_QUEUE);
        assert!(matches!(
            slow.try_recv(),
            Err(broadcast::error::TryRecvError::Lagged(_))
        ));
        let latest = publisher.snapshot().unwrap();
        assert_eq!(latest.activation_id, 64);
        assert_eq!(latest.phase, CommandFeedbackPhase::Succeeded);
    }
}
