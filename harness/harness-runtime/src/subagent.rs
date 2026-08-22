use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use harness_capability::subagent::Subagent;
use harness_core::AppContext;
use harness_core::error::{Error, Result};
use harness_core::types::UserInput;
use harness_llm::{LlmProvider, Message};
use harness_session::{SessionEvent, SessionLog};
use harness_tool::ToolRegistry;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::AgentLoop;

/// 进程内子代理 Provider：共享只读 Provider 句柄，使用 fork 后的服务表与独立 SessionLog。
pub struct InProcessSubagent {
    parent: AppContext,
    slots: Arc<Semaphore>,
    timeout: Duration,
}

impl InProcessSubagent {
    pub fn new(parent: AppContext, max_parallel: usize, timeout: Duration) -> Arc<Self> {
        Arc::new(Self {
            parent,
            slots: Arc::new(Semaphore::new(max_parallel.max(1))),
            timeout,
        })
    }
}

#[async_trait]
impl Subagent for InProcessSubagent {
    async fn spawn(&self, task: &str) -> Result<String> {
        let _permit = self
            .slots
            .acquire()
            .await
            .map_err(|_| Error::Subagent("scheduler closed".into()))?;
        let child = self.parent.fork();
        let log = SessionLog::new();
        let _log_registration = child.provide(log.clone());
        // 子 Agent 不得再次 delegate 形成递归专家树；共享 PlanTool 绑定的是主日志，
        // 也必须排除，避免子任务状态污染主会话。fs/edit/shell 等实际执行工具仍保留。
        let child_tools = self
            .parent
            .get::<ToolRegistry>()
            .snapshot_excluding(&["delegate", "plan"]);
        let _tools_registration = child.provide(child_tools);
        let input = UserInput {
            text: task.to_string(),
            attachments: vec![],
        };
        // 超时结构化取消：仅 drop future 不足以终止子回合已派生的工作；
        // 走可取消入口，到期后通知取消并给短宽限等待其刷日志/释放资源，
        // 避免僵尸任务继续占用 LLM 连接与槽位。
        let cancel = CancellationToken::new();
        let agent = AgentLoop::new();
        let run = agent.run_turn_cancellable(&child, input, cancel.clone());
        tokio::pin!(run);
        let outcome = tokio::select! {
            r = &mut run => Some(r),
            _ = tokio::time::sleep(self.timeout) => None,
        };
        match outcome {
            Some(result) => result?,
            None => {
                cancel.cancel();
                let _ = tokio::time::timeout(Duration::from_secs(5), run).await;
                return Err(Error::Subagent(format!(
                    "child timed out after {} seconds",
                    self.timeout.as_secs()
                )));
            }
        }
        let answer = log
            .replay()
            .into_iter()
            .filter_map(|event| match event {
                SessionEvent::Assistant { chunk, .. } => chunk.text,
                _ => None,
            })
            .collect::<String>();
        if answer.trim().is_empty() {
            Err(Error::Subagent("child returned no assistant text".into()))
        } else {
            Ok(answer)
        }
    }

    async fn spawn_brief(&self, task: &str) -> Result<String> {
        let _permit = self
            .slots
            .acquire()
            .await
            .map_err(|_| Error::Subagent("scheduler closed".into()))?;
        let llm = self.parent.get::<dyn LlmProvider>();
        let mut stream = llm.stream(vec![
            Message::system(
                "你是专家团的轻量分析专家。直接分析并给出简洁、结构化、可交接的结论；不得调用工具，不展开冗长思考。",
            ),
            Message::user(task),
        ]);
        let collect = async {
            let mut answer = String::new();
            while let Some(chunk) = stream.next().await {
                if let Some(text) = chunk?.text {
                    answer.push_str(&text);
                }
            }
            if answer.trim().is_empty() {
                Err(Error::Subagent(
                    "brief child returned no assistant text".into(),
                ))
            } else {
                Ok(answer)
            }
        };
        tokio::time::timeout(self.timeout, collect)
            .await
            .map_err(|_| {
                Error::Subagent(format!(
                    "brief child timed out after {} seconds",
                    self.timeout.as_secs()
                ))
            })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_capability::hook::Hook;
    use harness_llm::{Chunk, LlmProvider, ReplayLlm};
    use harness_provider_hook::NullHook;
    use harness_tool::ToolRegistry;

    #[tokio::test]
    async fn child_uses_isolated_log_and_returns_text() {
        let ctx = AppContext::new();
        let parent_log = SessionLog::new();
        let _a = ctx.provide(parent_log.clone());
        let llm: Arc<dyn LlmProvider> = ReplayLlm::new(vec![Chunk {
            text: Some("child-ok".into()),
            tool_calls: vec![],
            reasoning: None,
            usage: None,
            ..Default::default()
        }]);
        let _b = ctx.provide(llm);
        let _c = ctx.provide(ToolRegistry::new());
        let hook: Arc<dyn Hook> = Arc::new(NullHook);
        let _d = ctx.provide(hook);
        let sub = InProcessSubagent::new(ctx, 2, Duration::from_secs(2));
        assert_eq!(sub.spawn("work").await.unwrap(), "child-ok");
        assert!(parent_log.replay().is_empty());
    }
}
