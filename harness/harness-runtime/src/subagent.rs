use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use harness_capability::subagent::Subagent;
use harness_core::error::{Error, Result};
use harness_core::types::UserInput;
use harness_core::AppContext;
use harness_session::{SessionEvent, SessionLog};
use tokio::sync::Semaphore;

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
        let input = UserInput {
            text: task.to_string(),
            attachments: vec![],
        };
        tokio::time::timeout(self.timeout, AgentLoop::new().run_turn(&child, input))
            .await
            .map_err(|_| Error::Subagent("child timed out".into()))??;
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
