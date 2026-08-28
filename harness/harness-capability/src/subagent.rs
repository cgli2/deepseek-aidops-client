use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use harness_core::error::Result;

/// 子代理的过程反馈。编排器把它投影到任务卡，避免只能等待最终消息而误判为卡死。
pub type SubagentProgressReporter = Arc<dyn Fn(String) + Send + Sync>;

/// 子代理能力定义（Definition）。支持进程内 / fork / ACP / 外部产品桥接（原 §6 / 完成文档 §7）。
#[async_trait]
pub trait Subagent: Any + Send + Sync {
    /// 扇出一个子代理执行 `task`，返回其最终消息。
    async fn spawn(&self, task: &str) -> Result<String>;

    /// 在调用方给定的交付时限内执行子代理。专家团用它把短任务的等待预算
    /// 传到 Provider，而不是只在编排器外层丢弃 Future 后让子回合继续占用槽位。
    /// 外部 Provider 未实现专门取消时仍保持兼容，并由编排器外层时限兜底。
    async fn spawn_with_timeout(&self, task: &str, _timeout: Duration) -> Result<String> {
        self.spawn(task).await
    }

    /// 在执行期间汇报真实的模型/工具活动。`first_output_timeout` 用于阻止模型只
    /// 思考不行动；一旦已产生文本或工具动作，`idle_timeout` 才开始计时。
    /// 默认实现保持外部 Provider 兼容，由其既有超时入口执行。
    async fn spawn_observed(
        &self,
        task: &str,
        _first_output_timeout: Duration,
        idle_timeout: Duration,
        reporter: SubagentProgressReporter,
    ) -> Result<String> {
        reporter("专家已开始执行，正在等待模型首个可交付动作".into());
        self.spawn_with_timeout(task, idle_timeout).await
    }

    /// 轻量专家通道：适合需求、风险、设计和汇总，不进入工具循环。
    /// 外部 Provider 可覆写；默认回退完整子 Agent 以保持兼容。
    async fn spawn_brief(&self, task: &str) -> Result<String> {
        self.spawn(task).await
    }

    /// 轻量通道的带时限版本，语义与 [`Self::spawn_with_timeout`] 相同。
    async fn spawn_brief_with_timeout(&self, task: &str, _timeout: Duration) -> Result<String> {
        self.spawn_brief(task).await
    }
}
