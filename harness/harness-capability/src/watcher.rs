use std::any::Any;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use harness_core::error::Result;

/// 工作区文件监听能力。Provider 必须合并抖动事件，并在原生通知丢失时兜底扫描。
#[async_trait]
pub trait FileWatcher: Any + Send + Sync {
    async fn start(&self, root: &Path) -> Result<()>;
    async fn next_change(&self) -> Result<Vec<PathBuf>>;
}
