//! R1 side-effect protocol.
//!
//! Filesystem changes belong in [`super::SandboxTx`]. External effects are classified
//! here before execution. Compensable effects must describe their compensation and all
//! non-rollbackable effects must carry an idempotency key. Irreversible effects also
//! require a confirmation bound to the exact proposal.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectClass {
    Rollbackable,
    Compensable,
    Irreversible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectProposal {
    pub proposal_id: String,
    pub action: String,
    /// BLAKE3 digest of the exact serialized external-effect payload.
    pub payload_digest: String,
    pub class: EffectClass,
    pub idempotency_key: Option<String>,
    pub compensation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HitlConfirmation {
    pub proposal_id: String,
    pub action: String,
    pub confirmed_by: String,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectDecision {
    Allow,
    AllowWithCompensation { action: String },
    Deny(String),
    RequireHitl { proposal_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectState {
    Prepared,
    Executed { result_artifact: String },
    Failed { reason: String },
    Compensated { result_artifact: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectRecord {
    pub proposal: EffectProposal,
    pub state: EffectState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrepareOutcome {
    Prepared,
    AlreadyPrepared,
    AlreadyExecuted { result_artifact: String },
}

#[derive(Debug)]
pub enum EffectJournalError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Corrupt(String),
    Denied(EffectDecision),
    MissingIdempotencyKey,
    UnknownEffect(String),
    InvalidTransition(String),
}

impl std::fmt::Display for EffectJournalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "effect journal I/O error: {error}"),
            Self::Json(error) => write!(f, "effect journal JSON error: {error}"),
            Self::Corrupt(error) => write!(f, "corrupt effect journal: {error}"),
            Self::Denied(decision) => write!(f, "effect denied: {decision:?}"),
            Self::MissingIdempotencyKey => write!(f, "effect requires an idempotency key"),
            Self::UnknownEffect(key) => write!(f, "unknown effect: {key}"),
            Self::InvalidTransition(key) => write!(f, "invalid effect transition: {key}"),
        }
    }
}

impl std::error::Error for EffectJournalError {}

impl From<std::io::Error> for EffectJournalError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for EffectJournalError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum EffectEvent {
    Prepared(EffectProposal),
    Executed {
        idempotency_key: String,
        result_artifact: String,
    },
    Failed {
        idempotency_key: String,
        reason: String,
    },
    Compensated {
        idempotency_key: String,
        result_artifact: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct EffectWalRecord {
    seq: u64,
    event: EffectEvent,
}

/// Saga journal for external effects. Retrying `prepare` with the same idempotency key
/// never authorizes a second execution after an Executed record exists.
pub struct EffectJournal {
    path: PathBuf,
    next_seq: u64,
    records: BTreeMap<String, EffectRecord>,
}

impl EffectJournal {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, EffectJournalError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        super::storage::repair_jsonl_tail(&path)?;
        let wal = read_effect_wal(&path)?;
        let mut records = BTreeMap::new();
        let mut expected = 1;
        for record in wal {
            if record.seq != expected {
                return Err(EffectJournalError::Corrupt(format!(
                    "expected sequence {expected}, got {}",
                    record.seq
                )));
            }
            apply_effect_event(&mut records, &record.event)?;
            expected += 1;
        }
        Ok(Self {
            path,
            next_seq: expected,
            records,
        })
    }

    pub fn prepare(
        &mut self,
        proposal: &EffectProposal,
        confirmation: Option<&HitlConfirmation>,
    ) -> Result<PrepareOutcome, EffectJournalError> {
        let key = proposal
            .idempotency_key
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or(EffectJournalError::MissingIdempotencyKey)?;
        if let Some(record) = self.records.get(key) {
            if record.proposal != *proposal {
                return Err(EffectJournalError::InvalidTransition(format!(
                    "idempotency key {key} is already bound to another proposal"
                )));
            }
            return Ok(match &record.state {
                EffectState::Executed { result_artifact }
                | EffectState::Compensated { result_artifact } => PrepareOutcome::AlreadyExecuted {
                    result_artifact: result_artifact.clone(),
                },
                EffectState::Prepared | EffectState::Failed { .. } => {
                    PrepareOutcome::AlreadyPrepared
                }
            });
        }
        match gate_effect(proposal, confirmation) {
            EffectDecision::Allow | EffectDecision::AllowWithCompensation { .. } => {
                self.commit(EffectEvent::Prepared(proposal.clone()))?;
                Ok(PrepareOutcome::Prepared)
            }
            decision => Err(EffectJournalError::Denied(decision)),
        }
    }

    pub fn mark_executed(
        &mut self,
        idempotency_key: &str,
        result_artifact: &str,
    ) -> Result<(), EffectJournalError> {
        self.commit(EffectEvent::Executed {
            idempotency_key: idempotency_key.into(),
            result_artifact: result_artifact.into(),
        })
    }

    pub fn mark_failed(
        &mut self,
        idempotency_key: &str,
        reason: &str,
    ) -> Result<(), EffectJournalError> {
        self.commit(EffectEvent::Failed {
            idempotency_key: idempotency_key.into(),
            reason: reason.into(),
        })
    }

    pub fn mark_compensated(
        &mut self,
        idempotency_key: &str,
        result_artifact: &str,
    ) -> Result<(), EffectJournalError> {
        self.commit(EffectEvent::Compensated {
            idempotency_key: idempotency_key.into(),
            result_artifact: result_artifact.into(),
        })
    }

    pub fn pending_compensations(&self) -> Vec<&EffectRecord> {
        self.records
            .values()
            .filter(|record| {
                record.proposal.class == EffectClass::Compensable
                    && matches!(record.state, EffectState::Executed { .. })
            })
            .collect()
    }

    fn commit(&mut self, event: EffectEvent) -> Result<(), EffectJournalError> {
        let mut candidate = self.records.clone();
        apply_effect_event(&mut candidate, &event)?;
        let mut bytes = serde_json::to_vec(&EffectWalRecord {
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
        self.records = candidate;
        self.next_seq += 1;
        Ok(())
    }
}

fn apply_effect_event(
    records: &mut BTreeMap<String, EffectRecord>,
    event: &EffectEvent,
) -> Result<(), EffectJournalError> {
    match event {
        EffectEvent::Prepared(proposal) => {
            let key = proposal
                .idempotency_key
                .clone()
                .ok_or(EffectJournalError::MissingIdempotencyKey)?;
            if records.contains_key(&key) {
                return Err(EffectJournalError::InvalidTransition(key));
            }
            records.insert(
                key,
                EffectRecord {
                    proposal: proposal.clone(),
                    state: EffectState::Prepared,
                },
            );
        }
        EffectEvent::Executed {
            idempotency_key,
            result_artifact,
        } => {
            let record = records
                .get_mut(idempotency_key)
                .ok_or_else(|| EffectJournalError::UnknownEffect(idempotency_key.clone()))?;
            if !matches!(
                record.state,
                EffectState::Prepared | EffectState::Failed { .. }
            ) {
                return Err(EffectJournalError::InvalidTransition(
                    idempotency_key.clone(),
                ));
            }
            record.state = EffectState::Executed {
                result_artifact: result_artifact.clone(),
            };
        }
        EffectEvent::Failed {
            idempotency_key,
            reason,
        } => {
            let record = records
                .get_mut(idempotency_key)
                .ok_or_else(|| EffectJournalError::UnknownEffect(idempotency_key.clone()))?;
            if !matches!(record.state, EffectState::Prepared) {
                return Err(EffectJournalError::InvalidTransition(
                    idempotency_key.clone(),
                ));
            }
            record.state = EffectState::Failed {
                reason: reason.clone(),
            };
        }
        EffectEvent::Compensated {
            idempotency_key,
            result_artifact,
        } => {
            let record = records
                .get_mut(idempotency_key)
                .ok_or_else(|| EffectJournalError::UnknownEffect(idempotency_key.clone()))?;
            if record.proposal.class != EffectClass::Compensable
                || !matches!(record.state, EffectState::Executed { .. })
            {
                return Err(EffectJournalError::InvalidTransition(
                    idempotency_key.clone(),
                ));
            }
            record.state = EffectState::Compensated {
                result_artifact: result_artifact.clone(),
            };
        }
    }
    Ok(())
}

fn read_effect_wal(path: &Path) -> Result<Vec<EffectWalRecord>, EffectJournalError> {
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
                return Err(EffectJournalError::Corrupt(format!(
                    "line {}: {error}",
                    index + 1
                )));
            }
        }
    }
    Ok(records)
}

pub fn gate_effect(
    proposal: &EffectProposal,
    confirmation: Option<&HitlConfirmation>,
) -> EffectDecision {
    if proposal.proposal_id.trim().is_empty()
        || proposal.action.trim().is_empty()
        || proposal.payload_digest.len() != 64
        || !proposal
            .payload_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return EffectDecision::Deny(
            "effect proposal requires an id, action, and BLAKE3 payload digest".into(),
        );
    }

    match proposal.class {
        EffectClass::Rollbackable => EffectDecision::Allow,
        EffectClass::Compensable => {
            if proposal
                .idempotency_key
                .as_deref()
                .is_none_or(str::is_empty)
            {
                return EffectDecision::Deny(
                    "compensable effect requires a non-empty idempotency key".into(),
                );
            }
            match proposal
                .compensation
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                Some(action) => EffectDecision::AllowWithCompensation {
                    action: action.to_owned(),
                },
                None => EffectDecision::Deny(
                    "compensable effect requires an explicit compensation action".into(),
                ),
            }
        }
        EffectClass::Irreversible => {
            if proposal
                .idempotency_key
                .as_deref()
                .is_none_or(str::is_empty)
            {
                return EffectDecision::Deny(
                    "irreversible effect requires a non-empty idempotency key".into(),
                );
            }
            match confirmation {
                Some(value)
                    if value.proposal_id == proposal.proposal_id
                        && value.action == proposal.action
                        && !value.confirmed_by.trim().is_empty() =>
                {
                    EffectDecision::Allow
                }
                _ => EffectDecision::RequireHitl {
                    proposal_id: proposal.proposal_id.clone(),
                },
            }
        }
    }
}

pub fn effect_payload_digest(payload: &[u8]) -> String {
    blake3::hash(payload).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal(class: EffectClass) -> EffectProposal {
        EffectProposal {
            proposal_id: "effect-1".into(),
            action: "publish release".into(),
            payload_digest: effect_payload_digest(b"release-v1"),
            class,
            idempotency_key: Some("task-1:publish".into()),
            compensation: Some("withdraw release".into()),
        }
    }

    #[test]
    fn irreversible_confirmation_is_bound_to_exact_proposal() {
        let value = proposal(EffectClass::Irreversible);
        assert!(matches!(
            gate_effect(&value, None),
            EffectDecision::RequireHitl { .. }
        ));
        let wrong = HitlConfirmation {
            proposal_id: "effect-2".into(),
            action: value.action.clone(),
            confirmed_by: "operator".into(),
            note: String::new(),
        };
        assert!(matches!(
            gate_effect(&value, Some(&wrong)),
            EffectDecision::RequireHitl { .. }
        ));
        let approved = HitlConfirmation {
            proposal_id: value.proposal_id.clone(),
            action: value.action.clone(),
            confirmed_by: "operator".into(),
            note: "shadow validation passed".into(),
        };
        assert_eq!(gate_effect(&value, Some(&approved)), EffectDecision::Allow);
    }

    #[test]
    fn compensable_effect_requires_compensation_and_idempotency() {
        let mut value = proposal(EffectClass::Compensable);
        value.compensation = None;
        assert!(matches!(gate_effect(&value, None), EffectDecision::Deny(_)));
        value.compensation = Some("withdraw release".into());
        assert!(matches!(
            gate_effect(&value, None),
            EffectDecision::AllowWithCompensation { .. }
        ));
    }

    #[test]
    fn journal_deduplicates_execution_and_recovers_compensation() {
        let root = std::env::temp_dir().join(format!("lha_effect_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let path = root.join("effects.jsonl");
        let value = proposal(EffectClass::Compensable);
        let mut journal = EffectJournal::open(&path).unwrap();
        assert_eq!(
            journal.prepare(&value, None).unwrap(),
            PrepareOutcome::Prepared
        );
        journal
            .mark_executed("task-1:publish", "vault://effect/result")
            .unwrap();
        assert_eq!(journal.pending_compensations().len(), 1);
        drop(journal);
        let mut restored = EffectJournal::open(&path).unwrap();
        assert!(matches!(
            restored.prepare(&value, None).unwrap(),
            PrepareOutcome::AlreadyExecuted { .. }
        ));
        restored
            .mark_compensated("task-1:publish", "vault://effect/compensation")
            .unwrap();
        assert!(restored.pending_compensations().is_empty());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn journal_rejects_idempotency_key_reuse_for_another_effect() {
        let root = std::env::temp_dir().join(format!("lha_effect_key_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut journal = EffectJournal::open(root.join("effects.jsonl")).unwrap();
        let first = proposal(EffectClass::Compensable);
        journal.prepare(&first, None).unwrap();
        let mut conflicting = first.clone();
        conflicting.action = "delete release".into();
        assert!(matches!(
            journal.prepare(&conflicting, None),
            Err(EffectJournalError::InvalidTransition(_))
        ));
        fs::remove_dir_all(root).ok();
    }
}
