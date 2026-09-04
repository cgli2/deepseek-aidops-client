//! P2 durable shadow-HITL checkpoints for early irreversible decisions.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckpointKind {
    Architecture,
    IrreversibleEffect,
    VersionConvergence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckpointState {
    Pending,
    Approved { actor: String, note: String },
    Rejected { actor: String, note: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionCheckpoint {
    pub checkpoint_id: String,
    pub kind: CheckpointKind,
    pub subject: String,
    pub decision_digest: String,
    pub shadow_artifact: String,
    pub requested_at_ms: u64,
    pub state: CheckpointState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum DecisionEvent {
    Requested(DecisionCheckpoint),
    Approved {
        checkpoint_id: String,
        actor: String,
        note: String,
    },
    Rejected {
        checkpoint_id: String,
        actor: String,
        note: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DecisionRecord {
    seq: u64,
    event: DecisionEvent,
}

#[derive(Debug)]
pub enum HitlError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Corrupt(String),
    Invalid(String),
    Unknown(String),
    AlreadyDecided(String),
}

impl std::fmt::Display for HitlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "HITL I/O error: {error}"),
            Self::Json(error) => write!(f, "HITL JSON error: {error}"),
            Self::Corrupt(error) => write!(f, "corrupt HITL log: {error}"),
            Self::Invalid(error) => write!(f, "invalid HITL checkpoint: {error}"),
            Self::Unknown(id) => write!(f, "unknown HITL checkpoint: {id}"),
            Self::AlreadyDecided(id) => write!(f, "HITL checkpoint already decided: {id}"),
        }
    }
}

impl std::error::Error for HitlError {}

impl From<std::io::Error> for HitlError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for HitlError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

pub struct DecisionLog {
    path: PathBuf,
    next_seq: u64,
    checkpoints: BTreeMap<String, DecisionCheckpoint>,
}

impl DecisionLog {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, HitlError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        super::storage::repair_jsonl_tail(&path)?;
        let records = read_records(&path)?;
        let mut checkpoints = BTreeMap::new();
        let mut expected = 1;
        for record in records {
            if record.seq != expected {
                return Err(HitlError::Corrupt(format!(
                    "expected sequence {expected}, got {}",
                    record.seq
                )));
            }
            apply(&mut checkpoints, &record.event)?;
            expected += 1;
        }
        Ok(Self {
            path,
            next_seq: expected,
            checkpoints,
        })
    }

    pub fn request(
        &mut self,
        checkpoint_id: &str,
        kind: CheckpointKind,
        subject: &str,
        decision_payload: &[u8],
        shadow_artifact: &str,
        now_ms: u64,
    ) -> Result<DecisionCheckpoint, HitlError> {
        if checkpoint_id.trim().is_empty()
            || subject.trim().is_empty()
            || shadow_artifact.trim().is_empty()
        {
            return Err(HitlError::Invalid(
                "id, subject, and shadow artifact are required".into(),
            ));
        }
        let checkpoint = DecisionCheckpoint {
            checkpoint_id: checkpoint_id.into(),
            kind,
            subject: subject.into(),
            decision_digest: blake3::hash(decision_payload).to_hex().to_string(),
            shadow_artifact: shadow_artifact.into(),
            requested_at_ms: now_ms,
            state: CheckpointState::Pending,
        };
        self.commit(DecisionEvent::Requested(checkpoint.clone()))?;
        Ok(checkpoint)
    }

    pub fn approve(
        &mut self,
        checkpoint_id: &str,
        actor: &str,
        note: &str,
    ) -> Result<(), HitlError> {
        self.decide(checkpoint_id, actor, note, true)
    }

    pub fn reject(
        &mut self,
        checkpoint_id: &str,
        actor: &str,
        note: &str,
    ) -> Result<(), HitlError> {
        self.decide(checkpoint_id, actor, note, false)
    }

    pub fn checkpoint(&self, checkpoint_id: &str) -> Option<&DecisionCheckpoint> {
        self.checkpoints.get(checkpoint_id)
    }

    pub fn checkpoints(&self) -> impl Iterator<Item = &DecisionCheckpoint> {
        self.checkpoints.values()
    }

    pub fn is_approved(&self, checkpoint_id: &str, decision_payload: &[u8]) -> bool {
        let digest = blake3::hash(decision_payload).to_hex().to_string();
        self.checkpoint(checkpoint_id).is_some_and(|checkpoint| {
            checkpoint.decision_digest == digest
                && matches!(checkpoint.state, CheckpointState::Approved { .. })
        })
    }

    fn decide(
        &mut self,
        checkpoint_id: &str,
        actor: &str,
        note: &str,
        approved: bool,
    ) -> Result<(), HitlError> {
        if actor.trim().is_empty() {
            return Err(HitlError::Invalid("decision actor is required".into()));
        }
        let event = if approved {
            DecisionEvent::Approved {
                checkpoint_id: checkpoint_id.into(),
                actor: actor.into(),
                note: note.into(),
            }
        } else {
            DecisionEvent::Rejected {
                checkpoint_id: checkpoint_id.into(),
                actor: actor.into(),
                note: note.into(),
            }
        };
        self.commit(event)
    }

    fn commit(&mut self, event: DecisionEvent) -> Result<(), HitlError> {
        let mut candidate = self.checkpoints.clone();
        apply(&mut candidate, &event)?;
        let mut bytes = serde_json::to_vec(&DecisionRecord {
            seq: self.next_seq,
            event,
        })?;
        bytes.push(b'\n');
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        self.checkpoints = candidate;
        self.next_seq += 1;
        Ok(())
    }
}

fn apply(
    checkpoints: &mut BTreeMap<String, DecisionCheckpoint>,
    event: &DecisionEvent,
) -> Result<(), HitlError> {
    match event {
        DecisionEvent::Requested(checkpoint) => {
            if checkpoints.contains_key(&checkpoint.checkpoint_id) {
                return Err(HitlError::Invalid(format!(
                    "duplicate id {}",
                    checkpoint.checkpoint_id
                )));
            }
            checkpoints.insert(checkpoint.checkpoint_id.clone(), checkpoint.clone());
        }
        DecisionEvent::Approved {
            checkpoint_id,
            actor,
            note,
        }
        | DecisionEvent::Rejected {
            checkpoint_id,
            actor,
            note,
        } => {
            let checkpoint = checkpoints
                .get_mut(checkpoint_id)
                .ok_or_else(|| HitlError::Unknown(checkpoint_id.clone()))?;
            if checkpoint.state != CheckpointState::Pending {
                return Err(HitlError::AlreadyDecided(checkpoint_id.clone()));
            }
            checkpoint.state = if matches!(event, DecisionEvent::Approved { .. }) {
                CheckpointState::Approved {
                    actor: actor.clone(),
                    note: note.clone(),
                }
            } else {
                CheckpointState::Rejected {
                    actor: actor.clone(),
                    note: note.clone(),
                }
            };
        }
    }
    Ok(())
}

fn read_records(path: &Path) -> Result<Vec<DecisionRecord>, HitlError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = fs::read(path)?;
    let terminated = bytes.ends_with(b"\n");
    let lines: Vec<&[u8]> = bytes.split(|byte| *byte == b'\n').collect();
    let mut records = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if line.iter().all(|byte| byte.is_ascii_whitespace()) {
            continue;
        }
        match serde_json::from_slice(line) {
            Ok(record) => records.push(record),
            Err(_) if index + 1 == lines.len() && !terminated => {}
            Err(error) => {
                return Err(HitlError::Corrupt(format!("line {}: {error}", index + 1)));
            }
        }
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_is_durable_and_bound_to_exact_decision() {
        let root = std::env::temp_dir().join(format!("lha_hitl_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let path = root.join("decisions.jsonl");
        let mut log = DecisionLog::open(&path).unwrap();
        log.request(
            "architecture-1",
            CheckpointKind::Architecture,
            "database choice",
            b"postgres",
            "vault://shadow/benchmark",
            1,
        )
        .unwrap();
        log.approve("architecture-1", "operator", "benchmark accepted")
            .unwrap();
        assert!(log.is_approved("architecture-1", b"postgres"));
        assert!(!log.is_approved("architecture-1", b"sqlite"));
        drop(log);
        assert!(
            DecisionLog::open(path)
                .unwrap()
                .is_approved("architecture-1", b"postgres")
        );
        fs::remove_dir_all(root).ok();
    }
}
