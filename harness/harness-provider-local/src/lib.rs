//! harness-provider-local：本地 Provider（bash / fs / editor），实现 capability Definition。
//!
//! 与 `harness-tool` 的 Consumer 解耦：换 Provider（`LocalBash` → `WasmShell`）不影响工具代码。

pub mod bash;
pub mod editor;
pub mod fs;
pub mod lsp;
pub mod search;
pub mod watcher;

pub use bash::LocalBash;
pub use editor::LocalEditor;
pub use fs::LocalFs;
pub use lsp::LocalLsp;
pub use search::LocalSearch;
pub use watcher::PollingFileWatcher;
