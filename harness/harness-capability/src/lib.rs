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
/// 资产索引器：把工作区静态资产（SKILL.md / *.md / 源码）自动沉淀进四类资产服务。
pub mod index;
pub mod compaction;
pub mod editor;
pub mod fs;
pub mod git;
pub mod hook;
pub mod lsp;
pub mod memory;
pub mod shell;
pub mod subagent;
pub mod watcher;
