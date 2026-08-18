use std::any::Any;

use async_trait::async_trait;

use harness_core::error::Result;

/// 子代理能力定义（Definition）。支持进程内 / fork / ACP / 外部产品桥接（原 §6 / 完成文档 §7）。
#[async_trait]
pub trait Subagent: Any + Send + Sync {
    /// 扇出一个子代理执行 `task`，返回其最终消息。
    async fn spawn(&self, task: &str) -> Result<String>;
}
