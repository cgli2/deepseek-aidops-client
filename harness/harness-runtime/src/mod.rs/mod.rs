//! 自我监控（self-monitor）主进程侧：事件契约 + 非阻塞采集 + 在线确定性保护。
//!
//! 设计依据 docs/AGENT_SELF_MONITORING_AND_EVOLUTION_DESIGN.md（§3/§6/§11/§21/§23）：
//! - Event Tap：旁路 try_send，P99 主线程 < 1ms，失败开放（drop + 计数，绝不阻塞 Agent）；
//! - Hot Guard：确定性 detector（重复结论 / 无进展），在阈值内一次性收敛，禁止空转消耗回合；
//! - 重分析（归并 / 复盘 / 提案）在独立 harness-observer sidecar 中消费 WAL，不在本模块。

pub mod event;
pub mod hot_guard;
pub mod tap;
pub mod wal;

pub use event::{AgentEventEnvelope, EventClass, EventKind, Severity, SCHEMA_VERSION};
pub use hot_guard::{GuardAction, GuardVerdict, HotGuard};
pub use tap::{EventTap, TapStats};
pub use wal::{WalWriter, spawn_wal_writer};
