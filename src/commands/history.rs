//! Bounded, process-local command diagnostics. Never persists speech, audio or
//! credentials, and never participates in the original-audio callback.
use std::collections::VecDeque;

use serde::Serialize;

use super::{CommandErrorScope, CommandPhase, CommandStatus};

const CAPACITY: usize = 100;
const MAX_TOOLS: usize = 8;
const TEXT_LIMIT: usize = 4096;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CommandToolSelection {
    pub integration: String,
    pub tool: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolPhase {
    Selected,
    Executing,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandToolHistory {
    #[serde(flatten)]
    pub selection: CommandToolSelection,
    pub phase: ToolPhase,
    pub result: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandHistoryEntry {
    pub id: u64,
    pub started_at_ms: i64,
    pub updated_at_ms: i64,
    pub command: Option<String>,
    pub phase: CommandPhase,
    pub confidence: Option<f64>,
    pub tools: Vec<CommandToolHistory>,
    pub result: Option<String>,
    pub error: Option<String>,
    pub recognition_ms: Option<u64>,
    pub decision_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandHistorySnapshot {
    pub revision: u64,
    pub capacity: usize,
    pub entries: Vec<CommandHistoryEntry>,
}

#[derive(Default)]
pub(super) struct CommandHistory {
    revision: u64,
    last_activation: Option<u64>,
    entries: VecDeque<CommandHistoryEntry>,
}

fn bounded(value: &str, limit: usize) -> String {
    let mut end = value.len().min(limit);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn terminal(phase: CommandPhase) -> bool {
    matches!(phase, CommandPhase::Succeeded | CommandPhase::Failed)
}

impl CommandHistory {
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn snapshot(&self) -> CommandHistorySnapshot {
        CommandHistorySnapshot {
            revision: self.revision,
            capacity: CAPACITY,
            entries: self.entries.iter().rev().cloned().collect(),
        }
    }

    pub fn clear(&mut self) -> CommandHistorySnapshot {
        self.entries.clear();
        self.revision = self.revision.wrapping_add(1);
        self.snapshot()
    }

    pub fn update(&mut self, status: &CommandStatus) {
        if status.activation_id == 0 || status.error_scope == Some(CommandErrorScope::Service) {
            return;
        }
        if status.phase == CommandPhase::Activated
            && self.last_activation != Some(status.activation_id)
        {
            self.last_activation = Some(status.activation_id);
            self.interrupt("A new wake name replaced the pending command");
            if self.entries.len() == CAPACITY {
                self.entries.pop_front();
            }
            let now = chrono::Utc::now().timestamp_millis();
            self.entries.push_back(CommandHistoryEntry {
                id: status.activation_id,
                started_at_ms: now,
                updated_at_ms: now,
                command: None,
                phase: status.phase,
                confidence: None,
                tools: Vec::new(),
                result: None,
                error: None,
                recognition_ms: None,
                decision_ms: None,
            });
        }
        if matches!(
            status.phase,
            CommandPhase::Disabled | CommandPhase::Inactive | CommandPhase::Listening
        ) {
            return;
        }
        self.change(status.activation_id, |entry| {
            if let Some(command) = &status.command {
                entry.command = Some(bounded(command, 8192));
            }
            entry.phase = status.phase;
            entry.result = status
                .result
                .as_deref()
                .map(|value| bounded(value, TEXT_LIMIT));
            entry.error = status
                .error
                .as_deref()
                .map(|value| bounded(value, TEXT_LIMIT));
            if status.phase == CommandPhase::Failed {
                for tool in &mut entry.tools {
                    if tool.phase == ToolPhase::Executing {
                        tool.phase = ToolPhase::Failed;
                        tool.error = entry.error.clone();
                    }
                }
            }
        });
    }

    // Only the current activation can change. Late completions and cleared
    // entries never overwrite an old command or resurrect cleared history.
    fn change(&mut self, id: u64, update: impl FnOnce(&mut CommandHistoryEntry)) {
        if let Some(entry) = self.entries.back_mut()
            && entry.id == id
            && !terminal(entry.phase)
        {
            update(entry);
            entry.updated_at_ms = chrono::Utc::now().timestamp_millis();
            self.revision = self.revision.wrapping_add(1);
        }
    }

    pub fn recognition(&mut self, id: u64, milliseconds: u64) {
        self.change(id, |entry| {
            entry.recognition_ms = Some(
                entry
                    .recognition_ms
                    .unwrap_or(0)
                    .saturating_add(milliseconds),
            );
        });
    }

    pub fn decision(
        &mut self,
        id: u64,
        confidence: Option<f64>,
        selections: &[CommandToolSelection],
        milliseconds: u64,
    ) {
        self.change(id, |entry| {
            entry.confidence =
                confidence.filter(|value| value.is_finite() && (0.0..=1.0).contains(value));
            entry.decision_ms = Some(milliseconds);
            entry.tools = selections
                .iter()
                .take(MAX_TOOLS)
                .map(|selection| CommandToolHistory {
                    selection: CommandToolSelection {
                        integration: bounded(&selection.integration, 256),
                        tool: bounded(&selection.tool, 512),
                    },
                    phase: ToolPhase::Selected,
                    result: None,
                    error: None,
                })
                .collect();
        });
    }

    pub fn tool_started(&mut self, id: u64, index: usize) {
        self.change(id, |entry| {
            if let Some(tool) = entry.tools.get_mut(index) {
                tool.phase = ToolPhase::Executing;
            }
        });
    }

    pub fn tool_finished(&mut self, id: u64, index: usize, outcome: Result<&str, &str>) {
        self.change(id, |entry| {
            if let Some(tool) = entry.tools.get_mut(index) {
                match outcome {
                    Ok(result) => {
                        tool.phase = ToolPhase::Succeeded;
                        tool.result = Some(bounded(result, 2048));
                    }
                    Err(error) => {
                        tool.phase = ToolPhase::Failed;
                        tool.error = Some(bounded(error, TEXT_LIMIT));
                    }
                }
            }
        });
    }

    pub fn interrupt(&mut self, reason: &str) {
        if let Some(entry) = self.entries.back_mut()
            && !terminal(entry.phase)
        {
            entry.phase = CommandPhase::Failed;
            entry.error = Some(bounded(reason, TEXT_LIMIT));
            for tool in &mut entry.tools {
                if tool.phase == ToolPhase::Executing {
                    tool.phase = ToolPhase::Failed;
                    tool.error = entry.error.clone();
                }
            }
            entry.updated_at_ms = chrono::Utc::now().timestamp_millis();
            self.revision = self.revision.wrapping_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::AgentConfig;

    fn activation(id: u64) -> CommandStatus {
        let mut status = CommandStatus::initial(&AgentConfig::default());
        status.activation_id = id;
        status.phase = CommandPhase::Activated;
        status.command = Some("original command".into());
        status
    }

    #[test]
    fn history_is_bounded_newest_first_and_clearing_does_not_replay_active_work() {
        let mut history = CommandHistory::default();
        for id in 1..=110 {
            history.update(&activation(id));
        }
        let snapshot = history.snapshot();
        assert_eq!(snapshot.entries.len(), 100);
        assert_eq!(snapshot.entries.first().unwrap().id, 110);
        assert_eq!(snapshot.entries.last().unwrap().id, 11);
        assert_eq!(snapshot.entries[1].phase, CommandPhase::Failed);
        assert_eq!(history.clear().entries.len(), 0);
        history.update(&activation(110));
        assert!(history.snapshot().entries.is_empty());
        let mut completion = activation(110);
        completion.phase = CommandPhase::Succeeded;
        history.update(&completion);
        assert!(history.snapshot().entries.is_empty());
        history.update(&activation(111));
        assert_eq!(history.snapshot().entries.len(), 1);
    }

    #[test]
    fn history_retains_rejected_confidence_and_partial_results_but_no_arguments() {
        let mut history = CommandHistory::default();
        let mut status = activation(1);
        history.update(&status);
        let tools = [CommandToolSelection {
            integration: "Home".into(),
            tool: "lights".into(),
        }; 1];
        history.decision(1, Some(0.3), &tools, 40);
        history.recognition(1, 60);
        history.tool_started(1, 0);
        status.phase = CommandPhase::Failed;
        status.error = Some("synthetic failure".into());
        history.update(&status);
        history.update(&activation(1));
        history.tool_finished(1, 0, Ok("late completion"));
        let entry = history.snapshot().entries.remove(0);
        assert_eq!(entry.confidence, Some(0.3));
        assert_eq!(entry.recognition_ms, Some(60));
        assert_eq!(entry.tools[0].phase, ToolPhase::Failed);
        assert!(entry.tools[0].result.is_none());
        assert!(!serde_json::to_string(&entry).unwrap().contains("arguments"));
        history.update(&activation(2));
        history.decision(2, Some(f64::NAN), &tools, 2);
        history.tool_started(2, 0);
        history.tool_finished(2, 0, Ok(&"🦀".repeat(10_000)));
        let snapshot = history.snapshot();
        assert!(snapshot.entries[0].confidence.is_none());
        assert!(snapshot.entries[0].tools[0].result.as_ref().unwrap().len() <= 2048);
    }
}
