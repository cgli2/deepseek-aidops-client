use std::any::Any;

use async_trait::async_trait;

use harness_core::error::Result;

/// 子代理能力定义（Definition）。支持进程内 / fork / ACP / 外部产品桥接（原 §6 / 完成文档 §7）。
#[async_trait]
pub trait Subagent: Any + Send + Sync {
    /// 扇出一个子代理执行 `task`，返回其最终消息。
    async fn spawn(&self, task: &str) -> Result<String>;

    /// 轻量专家通道：适合需求、风险、设计和汇总，不进入工具循环。
    /// 外部 Provider 可覆写；默认回退完整子 Agent 以保持兼容。
    async fn spawn_brief(&self, task: &str) -> Result<String> {
        self.spawn(task).await
    }
}
