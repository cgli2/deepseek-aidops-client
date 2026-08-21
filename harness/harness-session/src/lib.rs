//! harness-session：会话追加日志（真相源）+ 投影 / 标题 / telemetry（M1）。
//!
//! 运行时不变量（原 §5.5 / 完成文档 §8 不变量 1）：模型可见的一切必须从会话日志重建。
//! fork / resume / replay / transcript / telemetry 全部从这条事件流派生。
//! 存储后端：MVP 内存 `Vec`（骨架）；完成文档 §1 锁定 redb（纯 Rust 嵌入式、WAL 风格追加）。

mod log;
pub use log::{
    delete_session, list_sessions, prune_sessions, rename_session, CouncilEvent,
    CouncilGateResult, CouncilTaskSpec, CouncilTaskState, EventId, PlanItem, SessionEvent, SessionId,
    SessionLog, SessionMeta,
};
