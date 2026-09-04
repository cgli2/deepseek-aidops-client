//! Integrated durable control plane for P1/P2 components.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use harness_session::{DeliveryOutcome, DeliveryReport};
use serde::{Deserialize, Serialize};

use super::{
    Admission, ArtifactRef, ArtifactVault, ArtifactVersion, Blackboard, BlackboardError,
    BlackboardEvent, CheckpointKind, CheckpointState, ContractDiff, ContractError, ContractLock,
    DagError, DecisionCheckpoint, DecisionLog, DurableDag, EffectJournal, EffectJournalError,
    EffectProposal, FactMatrix, GlobalBudgetController, HitlConfirmation, HitlError, LeaseWatchdog,
    MergeDecision, PrepareOutcome, ProviderLimit, QualityGate, RateLimitError, TaskRecord,
    TaskSpec, TaskStatus, VaultError, WatchdogEvent,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialDeliveryReport {
    pub task_id: String,
    pub reason: String,
    pub checkpoint_id: Option<String>,
    pub progress_pct: u8,
}

#[derive(Debug)]
pub enum OrchestratorError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Dag(DagError),
    Vault(VaultError),
    RateLimit(RateLimitError),
    Hitl(HitlError),
    Effect(EffectJournalError),
    Blackboard(BlackboardError),
    Contract(ContractError),
    Poisoned(&'static str),
    Quality(Vec<String>),
    Delivery(String),
}

impl std::fmt::Display for OrchestratorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "orchestrator I/O error: {error}"),
            Self::Json(error) => write!(f, "orchestrator JSON error: {error}"),
            Self::Dag(error) => write!(f, "orchestrator DAG error: {error}"),
            Self::Vault(error) => write!(f, "orchestrator vault error: {error}"),
            Self::RateLimit(error) => write!(f, "orchestrator budget error: {error}"),
            Self::Hitl(error) => write!(f, "orchestrator HITL error: {error}"),
            Self::Effect(error) => write!(f, "orchestrator effect error: {error}"),
            Self::Blackboard(error) => write!(f, "orchestrator blackboard error: {error}"),
            Self::Contract(error) => write!(f, "orchestrator contract error: {error}"),
            Self::Poisoned(name) => write!(f, "orchestrator lock poisoned: {name}"),
            Self::Quality(failures) => write!(f, "quality gate failed: {}", failures.join("; ")),
            Self::Delivery(reason) => write!(f, "delivery gate failed: {reason}"),
        }
    }
}

impl std::error::Error for OrchestratorError {}

impl From<std::io::Error> for OrchestratorError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for OrchestratorError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

macro_rules! error_from {
    ($source:ty, $variant:ident) => {
        impl From<$source> for OrchestratorError {
            fn from(value: $source) -> Self {
                Self::$variant(value)
            }
        }
    };
}

error_from!(DagError, Dag);
error_from!(VaultError, Vault);
error_from!(RateLimitError, RateLimit);
error_from!(HitlError, Hitl);
error_from!(EffectJournalError, Effect);
error_from!(BlackboardError, Blackboard);
error_from!(ContractError, Contract);

pub struct LongHorizonRuntime {
    control_root: PathBuf,
    dag: Arc<Mutex<DurableDag>>,
    vault: ArtifactVault,
    budget: Mutex<GlobalBudgetController>,
    decisions: Mutex<DecisionLog>,
    effects: Mutex<EffectJournal>,
    blackboard: Mutex<Blackboard>,
}

impl LongHorizonRuntime {
    pub fn open(
        control_root: impl AsRef<Path>,
        total_token_budget: u64,
    ) -> Result<Self, OrchestratorError> {
        let control_root = control_root.as_ref().to_path_buf();
        fs::create_dir_all(&control_root)?;
        let budget_path = control_root.join("budget.json");
        let budget = if budget_path.exists() {
            GlobalBudgetController::load(&budget_path)?
        } else {
            GlobalBudgetController::new(total_token_budget)
        };
        Ok(Self {
            dag: Arc::new(Mutex::new(DurableDag::open(
                control_root.join("dag.jsonl"),
            )?)),
            vault: ArtifactVault::open(control_root.join("vault"))?,
            budget: Mutex::new(budget),
            decisions: Mutex::new(DecisionLog::open(control_root.join("decisions.jsonl"))?),
            effects: Mutex::new(EffectJournal::open(control_root.join("effects.jsonl"))?),
            blackboard: Mutex::new(Blackboard::open(control_root.join("blackboard.jsonl"))?),
            control_root,
        })
    }

    pub fn register_provider(
        &self,
        provider: &str,
        limit: ProviderLimit,
        now_ms: u64,
    ) -> Result<(), OrchestratorError> {
        let mut budget = self
            .budget
            .lock()
            .map_err(|_| OrchestratorError::Poisoned("budget"))?;
        budget.register_provider(provider, limit, now_ms)?;
        budget.save(self.control_root.join("budget.json"))?;
        Ok(())
    }

    pub fn submit(&self, spec: TaskSpec) -> Result<(), OrchestratorError> {
        let task_id = spec.task_id.clone();
        let mut dag = self
            .dag
            .lock()
            .map_err(|_| OrchestratorError::Poisoned("dag"))?;
        dag.create_task(spec)?;
        dag.refresh_ready()?;
        drop(dag);
        self.publish_event(
            &task_id,
            "TaskCreated",
            None,
            "task admitted",
            "planner",
            super::now_ms(),
        )?;
        Ok(())
    }

    pub fn claim_next(
        &self,
        worker_id: &str,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<Option<TaskRecord>, OrchestratorError> {
        let mut dag = self
            .dag
            .lock()
            .map_err(|_| OrchestratorError::Poisoned("dag"))?;
        dag.refresh_ready()?;
        let task_id = dag.tasks().values().find_map(|record| {
            (record.status == TaskStatus::Ready).then(|| record.spec.task_id.clone())
        });
        let Some(task_id) = task_id else {
            return Ok(None);
        };
        dag.schedule(&task_id, worker_id)?;
        dag.start(&task_id, worker_id, now_ms, ttl_ms)?;
        let record = dag.task(&task_id).cloned();
        drop(dag);
        self.publish_event(
            &task_id,
            "TaskStarted",
            None,
            "lease granted",
            worker_id,
            now_ms,
        )?;
        Ok(record)
    }

    pub fn claim_task(
        &self,
        task_id: &str,
        worker_id: &str,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<TaskRecord, OrchestratorError> {
        let mut dag = self
            .dag
            .lock()
            .map_err(|_| OrchestratorError::Poisoned("dag"))?;
        dag.refresh_ready()?;
        dag.schedule(task_id, worker_id)?;
        dag.start(task_id, worker_id, now_ms, ttl_ms)?;
        let record = dag
            .task(task_id)
            .cloned()
            .ok_or_else(|| DagError::UnknownTask(task_id.into()))?;
        drop(dag);
        self.publish_event(
            task_id,
            "TaskStarted",
            None,
            "lease granted",
            worker_id,
            now_ms,
        )?;
        Ok(record)
    }

    pub fn heartbeat(
        &self,
        task_id: &str,
        worker_id: &str,
        progress_pct: f32,
        note: Option<String>,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<(), OrchestratorError> {
        self.dag
            .lock()
            .map_err(|_| OrchestratorError::Poisoned("dag"))?
            .heartbeat(task_id, worker_id, progress_pct, note, now_ms, ttl_ms)?;
        Ok(())
    }

    pub fn checkpoint_task(
        &self,
        task_id: &str,
        checkpoint_id: &str,
    ) -> Result<(), OrchestratorError> {
        self.dag
            .lock()
            .map_err(|_| OrchestratorError::Poisoned("dag"))?
            .checkpoint(task_id, checkpoint_id)?;
        Ok(())
    }

    pub fn fail_task(
        &self,
        task_id: &str,
        reason: &str,
        worker_id: &str,
        now_ms: u64,
    ) -> Result<(), OrchestratorError> {
        self.dag
            .lock()
            .map_err(|_| OrchestratorError::Poisoned("dag"))?
            .fail(task_id, reason)?;
        self.publish_event(task_id, "TaskFailed", None, reason, worker_id, now_ms)
    }

    pub fn cancel_task(
        &self,
        task_id: &str,
        reason: &str,
        worker_id: &str,
        now_ms: u64,
    ) -> Result<(), OrchestratorError> {
        self.dag
            .lock()
            .map_err(|_| OrchestratorError::Poisoned("dag"))?
            .cancel(task_id, reason)?;
        self.publish_event(task_id, "TaskCancelled", None, reason, worker_id, now_ms)
    }

    pub fn admit_llm(
        &self,
        task_id: &str,
        provider: &str,
        estimated_tokens: u64,
        now_ms: u64,
    ) -> Result<Admission, OrchestratorError> {
        let admission = {
            let mut budget = self
                .budget
                .lock()
                .map_err(|_| OrchestratorError::Poisoned("budget"))?;
            let admission = budget.acquire(provider, estimated_tokens, now_ms)?;
            budget.save(self.control_root.join("budget.json"))?;
            admission
        };
        if admission == Admission::GracefulExhaustion {
            self.finish_budget_exhausted(task_id, "global token budget exhausted", now_ms)?;
        }
        Ok(admission)
    }

    pub fn record_429(&self, provider: &str, now_ms: u64) -> Result<u64, OrchestratorError> {
        let mut budget = self
            .budget
            .lock()
            .map_err(|_| OrchestratorError::Poisoned("budget"))?;
        let delay = budget.record_429(provider, now_ms)?;
        budget.save(self.control_root.join("budget.json"))?;
        Ok(delay)
    }

    pub fn finalize(
        &self,
        task_id: &str,
        logical_key: &str,
        artifact_bytes: &[u8],
        facts: &FactMatrix,
        producer: &str,
        now_ms: u64,
    ) -> Result<ArtifactVersion, OrchestratorError> {
        let invariants = {
            let dag = self
                .dag
                .lock()
                .map_err(|_| OrchestratorError::Poisoned("dag"))?;
            dag.task(task_id)
                .ok_or_else(|| DagError::UnknownTask(task_id.into()))?
                .spec
                .invariants
                .clone()
        };
        let gate = QualityGate::evaluate(facts, &invariants);
        if !gate.passed {
            let failures = gate
                .failures
                .into_iter()
                .map(|failure| format!("{}: {}", failure.key, failure.message))
                .collect();
            return Err(OrchestratorError::Quality(failures));
        }
        let artifact = self.vault.put_bytes(artifact_bytes)?;
        let generation = self
            .vault
            .versions(logical_key)?
            .last()
            .map_or(0, |version| version.generation);
        let version = self.vault.publish(
            logical_key,
            artifact,
            generation,
            vec![],
            producer,
            BTreeMap::new(),
            now_ms,
        )?;
        let mut dag = self
            .dag
            .lock()
            .map_err(|_| OrchestratorError::Poisoned("dag"))?;
        dag.begin_validation(task_id)?;
        dag.complete(
            task_id,
            &format!("vault://blake3/{}", version.artifact.blake3),
        )?;
        dag.refresh_ready()?;
        drop(dag);
        self.publish_event(
            task_id,
            "TaskCompleted",
            Some(format!("vault://blake3/{}", version.artifact.blake3)),
            "quality gates passed",
            producer,
            now_ms,
        )?;
        Ok(version)
    }

    /// Finalize an interactive turn using the runtime-generated delivery report as the
    /// gate. This is separate from compile/test `FactMatrix` finalization: a turn may be
    /// a read-only investigation, but it still must carry verified criterion evidence.
    pub fn finalize_delivery(
        &self,
        task_id: &str,
        logical_key: &str,
        artifact_bytes: &[u8],
        report: &DeliveryReport,
        producer: &str,
        now_ms: u64,
    ) -> Result<ArtifactVersion, OrchestratorError> {
        if report.outcome != DeliveryOutcome::Verified {
            return Err(OrchestratorError::Delivery(format!(
                "turn outcome is {:?}",
                report.outcome
            )));
        }
        if report.criteria.is_empty()
            || report.criteria.iter().any(|criterion| {
                !criterion.satisfied || criterion.evidence.iter().all(|item| item.trim().is_empty())
            })
            || report
                .verification
                .iter()
                .all(|item| item.trim().is_empty())
        {
            return Err(OrchestratorError::Delivery(
                "verified turn lacks criterion or verification evidence".into(),
            ));
        }
        let artifact = self.vault.put_bytes(artifact_bytes)?;
        let generation = self
            .vault
            .versions(logical_key)?
            .last()
            .map_or(0, |version| version.generation);
        let version = self.vault.publish(
            logical_key,
            artifact,
            generation,
            vec![],
            producer,
            BTreeMap::new(),
            now_ms,
        )?;
        let mut dag = self
            .dag
            .lock()
            .map_err(|_| OrchestratorError::Poisoned("dag"))?;
        dag.begin_validation(task_id)?;
        dag.complete(
            task_id,
            &format!("vault://blake3/{}", version.artifact.blake3),
        )?;
        drop(dag);
        self.publish_event(
            task_id,
            "TaskCompleted",
            Some(format!("vault://blake3/{}", version.artifact.blake3)),
            "runtime delivery gate passed",
            producer,
            now_ms,
        )?;
        Ok(version)
    }

    pub fn select_authoritative(
        &self,
        logical_key: &str,
        artifact_hash: &str,
        decision: MergeDecision,
        now_ms: u64,
    ) -> Result<(), OrchestratorError> {
        if let MergeDecision::Hitl {
            checkpoint_id,
            confirmed_by,
            ..
        } = &decision
        {
            let decisions = self
                .decisions
                .lock()
                .map_err(|_| OrchestratorError::Poisoned("decisions"))?;
            let checkpoint = decisions
                .checkpoint(checkpoint_id)
                .ok_or_else(|| HitlError::Unknown(checkpoint_id.clone()))?;
            let actor_matches = matches!(
                &checkpoint.state,
                CheckpointState::Approved { actor, .. } if actor == confirmed_by
            );
            if checkpoint.kind != CheckpointKind::VersionConvergence
                || checkpoint.subject != logical_key
                || !actor_matches
                || !decisions.is_approved(checkpoint_id, artifact_hash.as_bytes())
            {
                return Err(HitlError::Invalid(
                    "version decision is not bound to an approved convergence checkpoint".into(),
                )
                .into());
            }
        }
        self.vault
            .select_authoritative(logical_key, artifact_hash, decision, now_ms)?;
        Ok(())
    }

    pub fn request_decision(
        &self,
        checkpoint_id: &str,
        kind: CheckpointKind,
        subject: &str,
        payload: &[u8],
        shadow_artifact: &str,
        now_ms: u64,
    ) -> Result<(), OrchestratorError> {
        self.decisions
            .lock()
            .map_err(|_| OrchestratorError::Poisoned("decisions"))?
            .request(
                checkpoint_id,
                kind,
                subject,
                payload,
                shadow_artifact,
                now_ms,
            )?;
        Ok(())
    }

    pub fn approve_decision(
        &self,
        checkpoint_id: &str,
        actor: &str,
        note: &str,
    ) -> Result<(), OrchestratorError> {
        self.decisions
            .lock()
            .map_err(|_| OrchestratorError::Poisoned("decisions"))?
            .approve(checkpoint_id, actor, note)?;
        Ok(())
    }

    pub fn reject_decision(
        &self,
        checkpoint_id: &str,
        actor: &str,
        note: &str,
    ) -> Result<(), OrchestratorError> {
        self.decisions
            .lock()
            .map_err(|_| OrchestratorError::Poisoned("decisions"))?
            .reject(checkpoint_id, actor, note)?;
        Ok(())
    }

    pub fn prepare_effect(
        &self,
        proposal: &EffectProposal,
        confirmation: Option<&HitlConfirmation>,
    ) -> Result<PrepareOutcome, OrchestratorError> {
        Ok(self
            .effects
            .lock()
            .map_err(|_| OrchestratorError::Poisoned("effects"))?
            .prepare(proposal, confirmation)?)
    }

    /// Authorize an irreversible effect from the durable HITL log. The checkpoint must
    /// describe the same action and exact payload, preventing approval reuse.
    pub fn prepare_effect_from_checkpoint(
        &self,
        proposal: &EffectProposal,
        checkpoint_id: &str,
        decision_payload: &[u8],
    ) -> Result<PrepareOutcome, OrchestratorError> {
        let confirmation = {
            let decisions = self
                .decisions
                .lock()
                .map_err(|_| OrchestratorError::Poisoned("decisions"))?;
            let checkpoint = decisions
                .checkpoint(checkpoint_id)
                .ok_or_else(|| HitlError::Unknown(checkpoint_id.into()))?;
            let CheckpointState::Approved { actor, note } = &checkpoint.state else {
                return Err(HitlError::Invalid("checkpoint is not approved".into()).into());
            };
            if checkpoint.kind != CheckpointKind::IrreversibleEffect
                || checkpoint.subject != proposal.action
                || proposal.payload_digest != blake3::hash(decision_payload).to_hex().to_string()
                || !decisions.is_approved(checkpoint_id, decision_payload)
            {
                return Err(HitlError::Invalid(
                    "effect approval is not bound to the proposal action and payload".into(),
                )
                .into());
            }
            HitlConfirmation {
                proposal_id: proposal.proposal_id.clone(),
                action: proposal.action.clone(),
                confirmed_by: actor.clone(),
                note: note.clone(),
            }
        };
        self.prepare_effect(proposal, Some(&confirmation))
    }

    pub fn capture_contract(
        &self,
        task_id: &str,
        workspace: impl AsRef<Path>,
    ) -> Result<(), OrchestratorError> {
        let snapshot = ContractLock::capture(workspace)?;
        ContractLock::save(&snapshot, self.contract_path(task_id)?)?;
        Ok(())
    }

    pub fn check_contract(
        &self,
        task_id: &str,
        workspace: impl AsRef<Path>,
    ) -> Result<ContractDiff, OrchestratorError> {
        let baseline = ContractLock::load(self.contract_path(task_id)?)?;
        Ok(ContractLock::compare(
            &baseline,
            &ContractLock::capture(workspace)?,
        ))
    }

    pub fn task(&self, task_id: &str) -> Result<Option<TaskRecord>, OrchestratorError> {
        Ok(self
            .dag
            .lock()
            .map_err(|_| OrchestratorError::Poisoned("dag"))?
            .task(task_id)
            .cloned())
    }

    pub fn tasks(&self) -> Result<Vec<TaskRecord>, OrchestratorError> {
        Ok(self
            .dag
            .lock()
            .map_err(|_| OrchestratorError::Poisoned("dag"))?
            .tasks()
            .values()
            .cloned()
            .collect())
    }

    pub fn decisions(&self) -> Result<Vec<DecisionCheckpoint>, OrchestratorError> {
        Ok(self
            .decisions
            .lock()
            .map_err(|_| OrchestratorError::Poisoned("decisions"))?
            .checkpoints()
            .cloned()
            .collect())
    }

    pub fn blackboard_since(
        &self,
        after_seq: u64,
    ) -> Result<Vec<BlackboardEvent>, OrchestratorError> {
        Ok(self
            .blackboard
            .lock()
            .map_err(|_| OrchestratorError::Poisoned("blackboard"))?
            .since(after_seq)
            .to_vec())
    }

    pub fn start_watchdog(
        &self,
        interval: Duration,
    ) -> (
        LeaseWatchdog,
        tokio::sync::mpsc::UnboundedReceiver<WatchdogEvent>,
    ) {
        LeaseWatchdog::start(self.dag.clone(), interval)
    }

    pub fn vault(&self) -> &ArtifactVault {
        &self.vault
    }

    fn finish_budget_exhausted(
        &self,
        task_id: &str,
        reason: &str,
        now_ms: u64,
    ) -> Result<ArtifactRef, OrchestratorError> {
        let report = {
            let dag = self
                .dag
                .lock()
                .map_err(|_| OrchestratorError::Poisoned("dag"))?;
            let task = dag
                .task(task_id)
                .ok_or_else(|| DagError::UnknownTask(task_id.into()))?;
            PartialDeliveryReport {
                task_id: task_id.into(),
                reason: reason.into(),
                checkpoint_id: task.checkpoint_id.clone(),
                progress_pct: task.progress_pct.clamp(0.0, 100.0) as u8,
            }
        };
        let artifact = self.vault.put_bytes(&serde_json::to_vec_pretty(&report)?)?;
        let key = format!("reports/{task_id}");
        let generation = self
            .vault
            .versions(&key)?
            .last()
            .map_or(0, |version| version.generation);
        self.vault.publish(
            &key,
            artifact.clone(),
            generation,
            vec![],
            "budget-controller",
            BTreeMap::new(),
            now_ms,
        )?;
        self.dag
            .lock()
            .map_err(|_| OrchestratorError::Poisoned("dag"))?
            .exhaust_budget(task_id, &format!("vault://blake3/{}", artifact.blake3))?;
        self.publish_event(
            task_id,
            "TaskBudgetExhausted",
            Some(format!("vault://blake3/{}", artifact.blake3)),
            reason,
            "budget-controller",
            now_ms,
        )?;
        Ok(artifact)
    }

    fn publish_event(
        &self,
        task_id: &str,
        event_type: &str,
        artifact_uri: Option<String>,
        summary_diff: &str,
        published_by: &str,
        now_ms: u64,
    ) -> Result<(), OrchestratorError> {
        self.blackboard
            .lock()
            .map_err(|_| OrchestratorError::Poisoned("blackboard"))?
            .publish(
                task_id,
                event_type,
                artifact_uri,
                summary_diff,
                published_by,
                now_ms,
            )?;
        Ok(())
    }

    fn contract_path(&self, task_id: &str) -> Result<PathBuf, OrchestratorError> {
        let safe = !task_id.is_empty()
            && task_id != "."
            && task_id != ".."
            && task_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
        if !safe {
            return Err(DagError::InvalidTransition {
                task_id: task_id.into(),
                detail: "task_id cannot be used as a contract key".into(),
            }
            .into());
        }
        Ok(self
            .control_root
            .join("contracts")
            .join(format!("{task_id}.json")))
    }
}
