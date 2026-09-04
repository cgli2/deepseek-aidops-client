//! Durable long-horizon orchestration primitives.
//!
//! This module is the P0 vertical slice from the DRLA plan: filesystem transactions,
//! evidence-backed facts, deterministic quality/energy gates, durable DAG state, and
//! explicit side-effect authorization.  It is deliberately independent from the
//! interactive [`crate::AgentLoop`] so callers can adopt it without changing existing
//! session semantics.

mod artifact_vault;
mod blackboard;
mod contract_lock;
mod dispatch;
mod effects;
mod energy;
mod fact_matrix;
mod hitl;
mod orchestrator;
mod quality;
mod rate_limit;
mod sandbox;
mod state_machine;
mod storage;
mod verifier;
mod watchdog;

pub use artifact_vault::{
    ArtifactRef, ArtifactVault, ArtifactVersion, AuthorityRecord, MergeDecision, VaultError,
    blake3_reader,
};
pub use blackboard::{Blackboard, BlackboardError, BlackboardEvent};
pub use contract_lock::{
    ContractDiff, ContractEntry, ContractError, ContractLanguage, ContractLock, ContractSnapshot,
};
pub use dispatch::{
    CapabilityRouter, ModelClass, RoutingDecision, RoutingRequirements, WorkerDescriptor,
    WorkerRole,
};
pub use effects::{
    EffectClass, EffectDecision, EffectJournal, EffectJournalError, EffectProposal, EffectRecord,
    EffectState, HitlConfirmation, PrepareOutcome, effect_payload_digest, gate_effect,
};
pub use energy::{
    BUDGET_DEFAULT, BudgetDecision, EnergyDecision, EnergyInput, EnergyLedger, Finding,
    RecoveryBudget, VerifierSnapshot,
};
pub use fact_matrix::{
    ArtifactVerifier, EvidenceRef, FactError, FactMatrix, HardFact, TrackKind, cross_check,
    sha256_file,
};
pub use hitl::{CheckpointKind, CheckpointState, DecisionCheckpoint, DecisionLog, HitlError};
pub use orchestrator::{LongHorizonRuntime, OrchestratorError, PartialDeliveryReport};
pub use quality::{GateFailure, GateResult, QualityGate};
pub use rate_limit::{Admission, GlobalBudgetController, ProviderLimit, RateLimitError};
pub use sandbox::{SandboxError, SandboxTx};
pub use state_machine::{
    DagError, DagEvent, DurableDag, TaskRecord, TaskSpec, TaskStatus, WalRecord,
};
pub use verifier::{
    CheckReport, IndependentVerifier, VerificationKind, VerificationReport, VerificationSpec,
    VerifierError,
};
pub use watchdog::{LeaseWatchdog, WatchdogEvent, now_ms};
