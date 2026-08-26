use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harness_capability::hook::{Hook, HookDecision, HookPayload};
use harness_core::error::Result;
use harness_core::types::UserInput;
use harness_core::AppContext;
use harness_llm::{Chunk, ChunkStream, LlmProvider, Message, ToolCall, ToolResult};
use harness_runtime::AgentLoop;
use harness_session::{SessionEvent, SessionLog};
use harness_tool::{DynTool, ToolRegistry};

struct AllowHook;
impl Hook for AllowHook {
    fn run(&self, _: &HookPayload) -> Result<HookDecision> {
        Ok(HookDecision::Allow)
    }
}

struct TwoStepLlm {
    calls: AtomicUsize,
    requests: Mutex<Vec<Vec<Message>>>,
}

#[async_trait]
impl LlmProvider for TwoStepLlm {
    fn name(&self) -> &'static str {
        "two-step-test"
    }
    fn tools(&self) -> Vec<harness_llm::ToolSchema> {
        vec![]
    }
    fn stream(&self, messages: Vec<Message>) -> ChunkStream {
        self.requests.lock().unwrap().push(messages);
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunk = if call == 0 {
            Chunk {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "call-1".into(),
                    name: "echo".into(),
                    args: serde_json::json!({"text":"hello"}),
                }],
                ..Default::default()
            }
        } else {
            Chunk {
                text: Some("工具执行完成".into()),
                tool_calls: vec![],
                ..Default::default()
            }
        };
        Box::pin(futures::stream::iter(vec![Ok(chunk)]))
    }
}

struct EchoTool;
#[async_trait]
impl DynTool for EchoTool {
    fn name(&self) -> &'static str {
        "echo"
    }
    async fn call(&self, call: &ToolCall) -> Result<ToolResult> {
        Ok(ToolResult {
            call_id: call.id.clone(),
            ok: true,
            content: call.args["text"].as_str().unwrap_or_default().to_string(),
            continuation_debt: 0,
        })
    }
}

struct EmptyThenTextLlm {
    calls: AtomicUsize,
    requests: Mutex<Vec<Vec<Message>>>,
}

/// 首次只给出“我会调查”的正文而没有工具调用。对于尚未验证的变更任务，
/// Runtime 必须把收敛提示送入下一次请求，不能在第一步直接结束回合。
struct TextThenTextLlm {
    calls: AtomicUsize,
    requests: Mutex<Vec<Vec<Message>>>,
}

#[async_trait]
impl LlmProvider for TextThenTextLlm {
    fn name(&self) -> &'static str {
        "text-then-text-test"
    }

    fn tools(&self) -> Vec<harness_llm::ToolSchema> {
        vec![]
    }

    fn stream(&self, messages: Vec<Message>) -> ChunkStream {
        self.requests.lock().unwrap().push(messages);
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(futures::stream::iter(vec![Ok(Chunk {
            text: Some(if call == 0 {
                "我先调查输入框自动换行的原因".into()
            } else {
                "收到收敛提示后继续处理".into()
            }),
            ..Default::default()
        })]))
    }
}

#[async_trait]
impl LlmProvider for EmptyThenTextLlm {
    fn name(&self) -> &'static str {
        "empty-then-text-test"
    }

    fn tools(&self) -> Vec<harness_llm::ToolSchema> {
        vec![]
    }

    fn stream(&self, messages: Vec<Message>) -> ChunkStream {
        self.requests.lock().unwrap().push(messages);
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunk = if call == 0 {
            Chunk {
                empty_response: true,
                finish_reason: Some("length".into()),
                ..Default::default()
            }
        } else {
            Chunk {
                text: Some("恢复后的完整答复".into()),
                ..Default::default()
            }
        };
        Box::pin(futures::stream::iter(vec![Ok(chunk)]))
    }
}

#[tokio::test]
async fn tool_result_is_sent_back_and_turn_finishes() {
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let llm = Arc::new(TwoStepLlm {
        calls: AtomicUsize::new(0),
        requests: Mutex::new(vec![]),
    });
    let tools = ToolRegistry::new();
    tools.register(Arc::new(EchoTool));
    let hook: Arc<dyn Hook> = Arc::new(AllowHook);
    let mut registrations = vec![];
    registrations.push(ctx.provide(log.clone()));
    let provider: Arc<dyn LlmProvider> = llm.clone();
    registrations.push(ctx.provide(provider));
    registrations.push(ctx.provide(tools));
    registrations.push(ctx.provide(hook));

    AgentLoop::new()
        .run_turn(
            &ctx,
            UserInput {
                text: "执行工具".into(),
                attachments: vec![],
            },
        )
        .await
        .unwrap();

    let requests = llm.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[1]
        .iter()
        .any(|m| m.tool_call_id.as_deref() == Some("call-1") && m.content.trim() == "hello"));
    assert!(log.replay().iter().any(|e| matches!(e, SessionEvent::Assistant { chunk, .. } if chunk.text.as_deref() == Some("工具执行完成"))));
    assert!(matches!(
        log.replay().last(),
        Some(SessionEvent::TurnEnd { .. })
    ));
}

#[tokio::test]
async fn empty_provider_response_is_retried_without_polluting_session_history() {
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let llm = Arc::new(EmptyThenTextLlm {
        calls: AtomicUsize::new(0),
        requests: Mutex::new(vec![]),
    });
    let hook: Arc<dyn Hook> = Arc::new(AllowHook);
    let mut registrations = vec![];
    registrations.push(ctx.provide(log.clone()));
    let provider: Arc<dyn LlmProvider> = llm.clone();
    registrations.push(ctx.provide(provider));
    registrations.push(ctx.provide(ToolRegistry::new()));
    registrations.push(ctx.provide(hook));

    AgentLoop::new()
        .run_turn(
            &ctx,
            UserInput {
                text: "继续完成任务".into(),
                attachments: vec![],
            },
        )
        .await
        .unwrap();

    let requests = llm.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[1]
        .iter()
        .any(|message| message.content.contains("[恢复请求]")));
    let events = log.replay();
    assert!(events.iter().any(|event| matches!(event, SessionEvent::Assistant { chunk, .. } if chunk.text.as_deref() == Some("恢复后的完整答复"))));
    assert!(!events.iter().any(|event| matches!(event, SessionEvent::Assistant { chunk, .. } if chunk.text.as_deref().is_some_and(|text| text.contains("返回了空内容")))));
}

#[tokio::test]
async fn unverified_text_only_step_gets_one_convergence_follow_up() {
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let llm = Arc::new(TextThenTextLlm {
        calls: AtomicUsize::new(0),
        requests: Mutex::new(vec![]),
    });
    let hook: Arc<dyn Hook> = Arc::new(AllowHook);
    let _a = ctx.provide(log);
    let provider: Arc<dyn LlmProvider> = llm.clone();
    let _b = ctx.provide(provider);
    let _c = ctx.provide(ToolRegistry::new());
    let _d = ctx.provide(hook);

    AgentLoop::new()
        .run_turn(
            &ctx,
            UserInput {
                text: "修复输入框自动换行".into(),
                attachments: vec![],
            },
        )
        .await
        .unwrap();

    let requests = llm.requests.lock().unwrap();
    assert_eq!(requests.len(), 2, "未验证的正文不能在首步结束回合");
    assert!(requests[1]
        .iter()
        .any(|message| message.content.contains("[系统提示]")));
}
