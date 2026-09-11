//! harness-capability：能力接缝三角色中的 **Definition**（纯 trait，零实现）。
//!
//! 每个可替换能力由三角色构成（原 §6 / 完成文档 §11.3）：
//! - **Definition**（本 crate）：声明接口与事件，零实现；
//! - **Provider**（harness-provider-*）：实现接口，可有多个；
//! - **Consumer**（harness-tool / harness-runtime）：仅依赖 Definition，永不直接依赖 Provider。
//!
//! 判定标准：把 `LocalBash` 换成 `WasmShell`，`BashTool` 零改动（完成文档 §8 不变量 2）。
//!
//! 所有 Definition trait 均带 `Any` 超 trait，使 `Arc<dyn Shell>` 等 trait 对象满足
//! `harness_core::Service`，可作为服务注册进 `AppContext`。

pub mod assets;
pub mod compaction;
/// 会话蒸馏器（Phase 2）：SessionEvent 流 → L1 会话摘要 / 候选事实队列 / 每日开发日志。
pub mod distill;
pub mod editor;
pub mod fs;
pub mod frontmatter;
pub mod git;
pub mod hook;
/// 资产索引器：把工作区静态资产（SKILL.md / *.md / 源码）自动沉淀进四类资产服务。
pub mod index;
pub mod lsp;
pub mod memory;
/// 审核晋升（Phase 3）：review/ 候选事实经质量门禁（来源/可独立理解/置信度/去重）晋升为 facts/ 的 L2 正式事实。
pub mod promotion;
pub mod search;
pub mod shell;
pub mod subagent;
pub mod watcher;
