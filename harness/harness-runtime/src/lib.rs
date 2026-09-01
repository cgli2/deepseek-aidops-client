//! harness-runtime：tokio 编排 + Agent 循环 + 工具管线 + 多任务调度（原 §5.6 / §7）。

pub mod agent_loop;
pub mod builtin_profile;
pub mod case_file;
pub mod controller;
pub mod council;
pub mod events;
pub mod execution;
pub mod facts;
pub mod concept_registry;
pub mod goal_execution;
pub mod governor;
pub mod solve_sketch;
pub mod intent;
pub mod scheduler;
pub mod subagent;
pub mod target_extract;
pub mod task;
pub mod task_ledger;
pub mod workspace_grounder;
pub mod workspace_index;

pub use agent_loop::{AgentLoop, DeterministicCompaction};
pub use case_file::{extract_anchors, normalize_question, CaseFile, TriedEntry};
pub use controller::SessionController;
pub use events::{PreStep, TurnStopping};
pub use goal_execution::{
    ActionContract, ActionSpec, ExpectedValue, GoalCompletion, GoalContract, GoalExecution,
    Hypothesis, HypothesisState, PhaseAttempts, PhaseBudget, SolveGraph, SolvePhase, WorkItem,
    WorkItemState,
};
pub use governor::sensors::{artifact_text, delta_between, WindowDelta};
pub use governor::strategy::{Strategy, StrategyStack, WINDOW_STEPS};
pub use intent::{
    Clarification, ClarificationKind, InspectVerdict, IntentKind, IntentProfile, ObservedBehavior,
    inspect_diff,
};
pub use execution::{
    ActionGate, ActionProposal, Budget, BudgetManager, Completion, CompletionJudge, DomainPolicy,
    ExecutionState, GeneralDomainPolicy, SolveMode, SolvePlan, StrategyKind, TaskContract,
};
pub use scheduler::Scheduler;
pub use subagent::InProcessSubagent;
pub use task::{SessionId, Task};
pub use task_ledger::{LedgerStatus, TaskLedger};
pub use workspace_grounder::{GroundingStatus, WorkspaceGrounder, WorkspaceGrounding};
pub use workspace_index::{AnchorGrade, WorkspaceIndex};
