//! Persistent blackboard event mesh. Workers exchange artifact references, not full context.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlackboardEvent {
    pub seq: u64,
    pub task_id: String,
    pub event_type: String,
    pub artifact_uri: Option<String>,
    pub summary_diff: String,
    pub published_by: String,
    pub published_at_ms: u64,
}

#[derive(Debug)]
pub enum BlackboardError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Corrupt(String),
    Invalid(String),
}

impl std::fmt::Display for BlackboardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "blackboard I/O error: {error}"),
            Self::Json(error) => write!(f, "blackboard JSON error: {error}"),
            Self::Corrupt(error) => write!(f, "corrupt blackboard: {error}"),
            Self::Invalid(error) => write!(f, "invalid blackboard event: {error}"),
        }
    }
}

impl std::error::Error for BlackboardError {}

impl From<std::io::Error> for BlackboardError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for BlackboardError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

pub struct Blackboard {
    path: PathBuf,
    events: Vec<BlackboardEvent>,
}

impl Blackboard {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, BlackboardError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        super::storage::repair_jsonl_tail(&path)?;
        let events = read_events(&path)?;
        for (index, event) in events.iter().enumerate() {
            let expected = index as u64 + 1;
            if event.seq != expected {
                return Err(BlackboardError::Corrupt(format!(
                    "expected sequence {expected}, got {}",
                    event.seq
                )));
            }
        }
        Ok(Self { path, events })
    }

    pub fn publish(
        &mut self,
        task_id: &str,
        event_type: &str,
        artifact_uri: Option<String>,
        summary_diff: &str,
        published_by: &str,
        published_at_ms: u64,
    ) -> Result<BlackboardEvent, BlackboardError> {
        if task_id.trim().is_empty()
            || event_type.trim().is_empty()
            || published_by.trim().is_empty()
        {
            return Err(BlackboardError::Invalid(
                "task, event type, and publisher are required".into(),
            ));
        }
        let event = BlackboardEvent {
            seq: self.events.len() as u64 + 1,
            task_id: task_id.into(),
            event_type: event_type.into(),
            artifact_uri,
            summary_diff: summary_diff.into(),
            published_by: published_by.into(),
            published_at_ms,
        };
        let mut bytes = serde_json::to_vec(&event)?;
        bytes.push(b'\n');
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        self.events.push(event.clone());
        Ok(event)
    }

    pub fn since(&self, after_seq: u64) -> &[BlackboardEvent] {
        let index = usize::try_from(after_seq)
            .unwrap_or(usize::MAX)
            .min(self.events.len());
        &self.events[index..]
    }
}

fn read_events(path: &Path) -> Result<Vec<BlackboardEvent>, BlackboardError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    fs::read_to_string(path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(BlackboardError::from))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribers_pull_only_lightweight_events_after_cursor() {
        let root = std::env::temp_dir().join(format!("lha_board_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let path = root.join("events.jsonl");
        let mut board = Blackboard::open(&path).unwrap();
        board
            .publish("t1", "TaskStarted", None, "started", "scheduler", 1)
            .unwrap();
        board
            .publish(
                "t1",
                "TaskCompleted",
                Some("vault://artifact".into()),
                "one interface added",
                "worker-1",
                2,
            )
            .unwrap();
        assert_eq!(board.since(1).len(), 1);
        drop(board);
        assert_eq!(Blackboard::open(path).unwrap().since(0).len(), 2);
        fs::remove_dir_all(root).ok();
    }
}
