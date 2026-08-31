use std::any::Any;
use std::path::Path;

use async_trait::async_trait;

use harness_core::error::Result;

/// FS 能力定义（Definition）。Provider：`LocalFs` / `WasmFs`。bash 与 fs 共享同一沙箱根（原 §8）。
#[async_trait]
pub trait Fs: Any + Send + Sync {
    async fn read(&self, path: &Path) -> Result<String>;
    async fn write(&self, path: &Path, content: &str) -> Result<()>;
    async fn list(&self, path: &Path) -> Result<Vec<std::path::PathBuf>>;
}
