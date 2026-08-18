use std::any::Any;
use std::path::Path;

use async_trait::async_trait;
use serde_json::Value;

use harness_core::error::Result;

/// LSP 能力定义（Definition）。Provider：本地语言服务器（tokio::process + JSON-RPC，原 §12）。
#[async_trait]
pub trait Lsp: Any + Send + Sync {
    /// 启动语言服务器（root 为工作区根）。
    async fn start(&self, root: &Path) -> Result<()>;
    /// 发送 JSON-RPC 请求，返回结果。
    async fn request(&self, method: &str, params: Value) -> Result<Value>;
}
