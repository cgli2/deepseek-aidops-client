use std::any::Any;
use std::path::Path;

use async_trait::async_trait;

use harness_core::error::Result;

/// Editor 能力定义（Definition）。Provider：`LocalEditor`。
#[async_trait]
pub trait Editor: Any + Send + Sync {
    /// 对 `path` 应用统一 diff/patch（如 search/replace 或 unified diff）。
    async fn apply(&self, path: &Path, patch: &str) -> Result<()>;
}
