//! harness-tool：模型可见工具（Consumer），仅依赖 capability trait（完成文档 §11.3）。
//!
//! `BashTool` / `FsTool` / `EditTool` 只依赖 `Arc<dyn Shell>` / `Arc<dyn Fs>` / `Arc<dyn Editor>`，
//! 换 Provider（`LocalBash` → `WasmShell`）源码零改动（不变量 2）。

mod bash;
mod delegate;
mod edit;
mod fs;
mod memory;
mod plan;
pub mod registry;
mod search;

pub use bash::BashTool;
pub use delegate::DelegateTool;
pub use edit::EditTool;
pub use fs::FsTool;
pub use memory::MemoryTool;
pub use plan::PlanTool;
pub use registry::{DynTool, ToolRegistry};
pub use search::SearchTool;
