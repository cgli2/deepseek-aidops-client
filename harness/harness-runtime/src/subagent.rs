use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use harness_capability::subagent::Subagent;
use harness_capability::subagent::SubagentProgressReporter;
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

    async fn run_agent(&self, task: &str, timeout: Duration) -> Result<String> {
        self.run_agent_observed(task, timeout, timeout, Arc::new(|_| {}))
            .await
    }

    async fn run_agent_observed(
        &self,
        task: &str,
        first_output_timeout: Duration,
        idle_timeout: Duration,
        reporter: SubagentProgressReporter,
    ) -> Result<String> {
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
        // 子代理日志是过程真相源。将其投影给编排器，既能让用户看到真实执行，
        // 也让“首个可交付动作”和“活动中空转”采用不同的超时策略。
        let cancel = CancellationToken::new();
        let agent = AgentLoop::new();
        let run = agent.run_turn_cancellable(&child, input, cancel.clone(), None);
        tokio::pin!(run);
        let started = std::time::Instant::now();
        let mut last_action = started;
        let mut saw_action = false;
        let mut cursor = 0;
        let outcome = loop {
            tokio::select! {
                r = &mut run => break Some(r),
                _ = tokio::time::sleep(Duration::from_millis(500)) => {
                    let (next, events) = log.replay_from(cursor);
                    cursor = next;
                    let mut latest = None;
                    let mut action_seen = false;
                    for event in events {
                        match event {
                            SessionEvent::StepStart { .. } => {
                                latest = Some("专家正在请求模型下一步".to_string());
                            }
                            SessionEvent::Thinking { .. } => {
                                latest = Some("模型正在分析；尚未产生文本或工具动作".to_string());
                            }
                            SessionEvent::Assistant { chunk, .. } if chunk.text.as_deref().is_some_and(|text| !text.trim().is_empty()) => {
                                action_seen = true;
                                latest = Some("专家已产生文本结果，正在继续收敛交付".to_string());
                            }
                            SessionEvent::ToolCall { call, .. } => {
                                action_seen = true;
                                latest = Some(format!("专家正在调用工具：{}", call.name));
                            }
                            SessionEvent::ToolResult { result, .. } => {
                                action_seen = true;
                                latest = Some(if result.ok {
                                    "专家已收到工具结果，正在执行下一步".to_string()
                                } else {
                                    "专家收到失败的工具结果，正在调整执行方案".to_string()
                                });
                            }
                            _ => {}
                        }
                    }
                    if action_seen {
                        saw_action = true;
                        last_action = std::time::Instant::now();
                    }
                    if let Some(detail) = latest {
                        reporter(detail);
                    }
                    let elapsed = started.elapsed();
                    let inactive = if saw_action {
                        last_action.elapsed() >= idle_timeout
                    } else {
                        elapsed >= first_output_timeout
                    };
                    if inactive {
                        cancel.cancel();
                        let _ = tokio::time::timeout(Duration::from_secs(5), &mut run).await;
                        let reason = if saw_action {
                            format!("child made no new delivery activity for {} seconds", idle_timeout.as_secs())
                        } else {
                            format!("child produced no text or tool action after {} seconds", first_output_timeout.as_secs())
                        };
                        break Some(Err(Error::Subagent(reason)));
                    }
                }
            }
        };
        match outcome {
            Some(result) => result?,
            None => unreachable!("the observed subagent loop always returns an outcome"),
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

    async fn run_brief(&self, task: &str, timeout: Duration) -> Result<String> {
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
        tokio::time::timeout(timeout, collect).await.map_err(|_| {
            Error::Subagent(format!(
                "brief child timed out after {} seconds",
                timeout.as_secs()
            ))
        })?
    }
}

#[async_trait]
impl Subagent for InProcessSubagent {
    async fn spawn(&self, task: &str) -> Result<String> {
        self.run_agent(task, self.timeout).await
    }

    async fn spawn_with_timeout(&self, task: &str, timeout: Duration) -> Result<String> {
        self.run_agent(task, timeout.min(self.timeout)).await
    }

    async fn spawn_observed(
        &self,
        task: &str,
        first_output_timeout: Duration,
        idle_timeout: Duration,
        reporter: SubagentProgressReporter,
    ) -> Result<String> {
        self.run_agent_observed(
            task,
            first_output_timeout.min(self.timeout),
            idle_timeout.min(self.timeout),
            reporter,
        )
        .await
    }

    async fn spawn_brief(&self, task: &str) -> Result<String> {
        self.run_brief(task, self.timeout).await
    }

    async fn spawn_brief_with_timeout(&self, task: &str, timeout: Duration) -> Result<String> {
        self.run_brief(task, timeout.min(self.timeout)).await
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
        // 子代理接收的是一个带代码符号（ModelForm）的具体任务：has_locatable_signal
        // 为真，Phase 1 定位门禁不会触发，由回放 LLM 产出文本。本测试验证的是
        // "子代理使用隔离日志、按时返回 LLM 文本"——受控交付校正可能对同一回放文本
        // 重复请求一次，故只校验文本来自 LLM 且父日志保持隔离，不纠结确切拼接次数。
        let output = sub.spawn("修复 ModelForm 的校验规则").await.unwrap();
        assert!(
            output.contains("child-ok"),
            "子代理应返回回放 LLM 的文本，实际返回：{output}"
        );
        assert!(
            parent_log.replay().is_empty(),
            "子代理的日志必须与父会话隔离"
        );
    }
}
