//! harness-runtime：tokio 编排 + Agent 循环 + 工具管线 + 多任务调度（原 §5.6 / §7）。

pub mod agent_loop;
pub mod builtin_profile;
pub mod case_file;
pub mod concept_registry;
pub mod controller;
pub mod council;
pub mod events;
pub mod execution;
pub mod facts;
pub mod goal_execution;
pub mod governor;
pub mod intent;
pub mod lha;
pub mod long_horizon;
pub mod scheduler;
pub mod solve_sketch;
pub mod subagent;
pub mod target_extract;
pub mod task;
pub mod task_ledger;
pub mod workspace_grounder;
pub mod workspace_index;

pub use agent_loop::{AgentLoop, DeterministicCompaction, GovernorMode, parse_governor_mode};
pub use case_file::{CaseFile, TriedEntry, extract_anchors, normalize_question};
pub use controller::SessionController;
pub use events::{PreStep, TurnStopping};
pub use execution::{
    ActionGate, ActionProposal, Budget, BudgetManager, Completion, CompletionJudge, DomainPolicy,
    ExecutionState, GeneralDomainPolicy, SolveMode, SolvePlan, StrategyKind, TaskContract,
};
pub use goal_execution::{
    ActionContract, ActionSpec, ExpectedValue, GoalCompletion, GoalContract, GoalExecution,
    Hypothesis, HypothesisState, PhaseAttempts, PhaseBudget, SolveGraph, SolvePhase, WorkItem,
    WorkItemState,
};
pub use governor::{
    CANDIDATE_MARKERS, Decision, PROMPT_CAP, Strategy, StrategyStack, Termination, TurnGovernor,
    WINDOW_STEPS, WindowDelta, artifact_text, delta_between, has_candidates,
    is_continuation_request,
};
pub use intent::{
    Clarification, ClarificationKind, InspectVerdict, IntentKind, IntentProfile, ObservedBehavior,
    inspect_diff,
};
pub use lha::{
    Admission, ArtifactRef, ArtifactVault, ArtifactVerifier, ArtifactVersion, AuthorityRecord,
    BUDGET_DEFAULT, Blackboard, BlackboardError, BlackboardEvent, BudgetDecision, CapabilityRouter,
    CheckReport, CheckpointKind, CheckpointState, ContractDiff, ContractEntry, ContractError,
    ContractLanguage, ContractLock, ContractSnapshot, DagError, DagEvent, DecisionCheckpoint,
    DecisionLog, DurableDag, EffectClass, EffectDecision, EffectJournal, EffectJournalError,
    EffectProposal, EffectRecord, EffectState, EnergyDecision, EnergyInput, EnergyLedger,
    EvidenceRef, FactError, FactMatrix, Finding, GateFailure, GateResult, GlobalBudgetController,
    HardFact, HitlConfirmation, HitlError, IndependentVerifier, LeaseWatchdog, LongHorizonRuntime,
    MergeDecision, ModelClass, OrchestratorError, PartialDeliveryReport, PrepareOutcome,
    ProviderLimit, QualityGate, RateLimitError, RecoveryBudget, RoutingDecision,
    RoutingRequirements, SandboxError, SandboxTx, TaskRecord, TaskSpec, TaskStatus, TrackKind,
    VaultError, VerificationKind, VerificationReport, VerificationSpec, VerifierError,
    VerifierSnapshot, WalRecord, WatchdogEvent, WorkerDescriptor, WorkerRole,
    effect_payload_digest,
};
pub use long_horizon::{LongHorizonManager, run_durable_agent_turn, run_durable_council_turn};
pub use scheduler::Scheduler;
pub use subagent::InProcessSubagent;
pub use task::{SessionId, Task};
pub use task_ledger::{LedgerStatus, TaskLedger};
pub use workspace_grounder::{GroundingStatus, WorkspaceGrounder, WorkspaceGrounding};
pub use workspace_index::{AnchorGrade, WorkspaceIndex};
