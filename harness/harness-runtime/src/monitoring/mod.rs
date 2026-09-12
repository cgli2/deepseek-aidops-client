//! Runtime metadata collection, deterministic protection and offline observation.
//! Bundle and patch APIs enforce filesystem boundaries. Automatic promotion and
//! binary self-repair remain unavailable until independently evaluated and approved.

pub mod bundle;
pub mod event;
pub mod graduation;
pub mod hot_guard;
pub mod incident;
pub mod proposal;
pub mod replay;
pub mod self_repair;
pub mod tap;
pub mod wal;
pub(crate) mod storage;
pub mod monitor_adapter;
pub mod observer;

pub use bundle::{BundleError, BundleManifest, ChangeKind, EvolutionLevel, PolicyBundle};
pub use event::{AgentEventEnvelope, EventClass, EventKind, Severity, SCHEMA_VERSION};
pub use graduation::{
    ActiveManifest, GateError, Governor, GovernorConfig, GradError, GraduationStage, Graduator,
    PromotionOutcome, PromotionRecord,
};
pub use hot_guard::{GuardAction, GuardVerdict, HotGuard};
pub use incident::{Incident, IncidentBook, QualityReport, quality_report, read_wal};
pub use proposal::{ImprovementProposal, ProposalKind, ProposalStatus, RiskTier};
pub use replay::{DeterministicRunner, FaultKind, MockedToolCall, ReplayOutcome, ReplaySpec, ReplayStep};
pub use self_repair::{
    BaselineComparison, FaultDrill, GoldenSetEntry, PatchFile, PatchSpec, ReleaseRecord,
    RepairCandidate, ReviewReport, SelfRepairError, SelfRepairPipeline,
};
pub use tap::{EventTap, TapStats};
pub use wal::{WalWriter, spawn_wal_writer};
