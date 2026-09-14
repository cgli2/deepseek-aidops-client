use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harness_capability::hook::{Hook, HookDecision, HookPayload};
use harness_core::error::Result;
use harness_core::types::UserInput;
use harness_core::{AppContext, Workspace};
use harness_llm::{
    Chunk, ChunkStream, LlmProvider, Message, RequestOptions, ToolCall, ToolResult, Usage,
};
use harness_runtime::AgentLoop;
use harness_session::{DeliveryOutcome, SessionEvent, SessionLog};
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
                reasoning: Some("先调用 echo 工具取得结果。".into()),
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

struct StaticTool {
    name: &'static str,
    output: &'static str,
}

#[async_trait]
impl DynTool for StaticTool {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn call(&self, call: &ToolCall) -> Result<ToolResult> {
        Ok(ToolResult {
            call_id: call.id.clone(),
            ok: true,
            content: self.output.into(),
            continuation_debt: 0,
        })
    }
}

struct ScriptedLlm {
    calls: AtomicUsize,
    initial_text_steps: usize,
    script: Vec<Option<ToolCall>>,
    options: Mutex<Vec<RequestOptions>>,
}

#[async_trait]
impl LlmProvider for ScriptedLlm {
    fn name(&self) -> &'static str {
        "v4-scripted-replay"
    }

    fn tools(&self) -> Vec<harness_llm::ToolSchema> {
        vec![]
    }

    fn stream(&self, messages: Vec<Message>) -> ChunkStream {
        self.stream_with_options(messages, RequestOptions::default())
    }

    fn stream_with_options(&self, _messages: Vec<Message>, options: RequestOptions) -> ChunkStream {
        self.options.lock().unwrap().push(options);
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        if index < self.initial_text_steps {
            return Box::pin(futures::stream::iter(vec![Ok(Chunk {
                text: Some("我先定位这个问题".into()),
                ..Default::default()
            })]));
        }
        let script_index = index - self.initial_text_steps;
        let chunk = match self.script.get(script_index).cloned().flatten() {
            Some(call) => Chunk {
                tool_calls: vec![call],
                ..Default::default()
            },
            None => Chunk {
                text: Some("已按证据完成并验证".into()),
                ..Default::default()
            },
        };
        Box::pin(futures::stream::iter(vec![Ok(chunk)]))
    }
}

fn scripted_call(id: &str, name: &str, args: serde_json::Value) -> Option<ToolCall> {
    Some(ToolCall {
        id: id.into(),
        name: name.into(),
        args,
    })
}

struct EmptyThenTextLlm {
    calls: AtomicUsize,
    requests: Mutex<Vec<Vec<Message>>>,
    options: Mutex<Vec<RequestOptions>>,
    finish_reason: &'static str,
    empty_responses: usize,
    tool_after_empty: bool,
}

/// 模拟真实故障：模型先输出一句工作说明，随后 `fs.arguments` 在字符串中间截断。
/// Provider 已把残缺调用转换成 empty_response；Runtime 仍应忽略前言并自动恢复。
struct PreambleThenIncompleteToolLlm {
    calls: AtomicUsize,
    requests: Mutex<Vec<Vec<Message>>>,
    options: Mutex<Vec<RequestOptions>>,
}

struct ToolCallsWithoutPayloadLlm {
    calls: AtomicUsize,
}

#[async_trait]
impl LlmProvider for ToolCallsWithoutPayloadLlm {
    fn name(&self) -> &'static str {
        "missing-tool-payload-test"
    }

    fn tools(&self) -> Vec<harness_llm::ToolSchema> {
        vec![]
    }

    fn stream(&self, _messages: Vec<Message>) -> ChunkStream {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(futures::stream::iter(vec![Ok(Chunk {
            empty_response: true,
            finish_reason: Some("tool_calls".into()),
            ..Default::default()
        })]))
    }
}

/// 捕获 Runtime 实际下发的阶段工具白名单，回放日志中的错误首步选择。
struct OptionsCaptureLlm {
    options: Mutex<Vec<RequestOptions>>,
}

#[async_trait]
impl LlmProvider for OptionsCaptureLlm {
    fn name(&self) -> &'static str {
        "options-capture-test"
    }

    fn tools(&self) -> Vec<harness_llm::ToolSchema> {
        vec![]
    }

    fn stream(&self, _messages: Vec<Message>) -> ChunkStream {
        Box::pin(futures::stream::iter(vec![Ok(Chunk {
            text: Some("等待运行时下一步".into()),
            ..Default::default()
        })]))
    }

    fn stream_with_options(&self, messages: Vec<Message>, options: RequestOptions) -> ChunkStream {
        self.options.lock().unwrap().push(options);
        self.stream(messages)
    }
}

/// 首次只给出“我会调查”的正文而没有工具调用。对于尚未验证的变更任务，
/// Runtime 必须把收敛提示送入下一次请求，不能在第一步直接结束回合。
struct TextThenTextLlm {
    calls: AtomicUsize,
    requests: Mutex<Vec<Vec<Message>>>,
}

/// 首回合用一次真实 Usage 把执行预算推到边界；后续响应不再计费，专门验证新回合
/// 不会继承历史熔断状态。对应实机日志中的 259607 + 57196 永久复读故障。
struct CapThenResumeLlm {
    calls: AtomicUsize,
    requests: Mutex<Vec<Vec<Message>>>,
}

/// 首次请求在产生真实工具证据的同时触达 prompt 窗口边界。Runtime 应在同一个
/// 用户请求内压缩断点并自动续跑，而不是要求用户再输入一次“继续”。
struct ProgressAtCapLlm {
    calls: AtomicUsize,
    requests: Mutex<Vec<Vec<Message>>>,
}

#[async_trait]
impl LlmProvider for ProgressAtCapLlm {
    fn name(&self) -> &'static str {
        "progress-at-cap-test"
    }

    fn tools(&self) -> Vec<harness_llm::ToolSchema> {
        vec![]
    }

    fn stream(&self, messages: Vec<Message>) -> ChunkStream {
        self.requests.lock().unwrap().push(messages);
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunk = if call == 0 {
            Chunk {
                tool_calls: vec![ToolCall {
                    id: "cap-call-1".into(),
                    name: "echo".into(),
                    args: serde_json::json!({"text":"new evidence"}),
                }],
                usage: Some(Usage {
                    prompt_tokens: harness_runtime::PROMPT_CAP,
                    completion_tokens: 1,
                    total_tokens: harness_runtime::PROMPT_CAP + 1,
                }),
                ..Default::default()
            }
        } else {
            Chunk {
                text: Some("工具执行完成".into()),
                ..Default::default()
            }
        };
        Box::pin(futures::stream::iter(vec![Ok(chunk)]))
    }
}

#[async_trait]
impl LlmProvider for CapThenResumeLlm {
    fn name(&self) -> &'static str {
        "cap-then-resume-test"
    }

    fn tools(&self) -> Vec<harness_llm::ToolSchema> {
        vec![]
    }

    fn stream(&self, messages: Vec<Message>) -> ChunkStream {
        self.requests.lock().unwrap().push(messages);
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunk = if call == 0 {
            Chunk {
                text: Some("已定位，仍需继续执行".into()),
                usage: Some(Usage {
                    prompt_tokens: harness_runtime::PROMPT_CAP,
                    completion_tokens: 1,
                    total_tokens: harness_runtime::PROMPT_CAP + 1,
                }),
                ..Default::default()
            }
        } else {
            Chunk {
                text: Some("已从断点恢复执行".into()),
                ..Default::default()
            }
        };
        Box::pin(futures::stream::iter(vec![Ok(chunk)]))
    }
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
        let chunk = if call < self.empty_responses {
            Chunk {
                empty_response: true,
                finish_reason: Some(self.finish_reason.into()),
                ..Default::default()
            }
        } else if self.tool_after_empty && call == self.empty_responses {
            Chunk {
                tool_calls: vec![ToolCall {
                    id: "recovered-call".into(),
                    name: "echo".into(),
                    args: serde_json::json!({"text":"recovered evidence"}),
                }],
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

    fn stream_with_options(&self, messages: Vec<Message>, options: RequestOptions) -> ChunkStream {
        self.options.lock().unwrap().push(options);
        self.stream(messages)
    }
}

#[async_trait]
impl LlmProvider for PreambleThenIncompleteToolLlm {
    fn name(&self) -> &'static str {
        "preamble-incomplete-tool-test"
    }

    fn tools(&self) -> Vec<harness_llm::ToolSchema> {
        vec![]
    }

    fn stream(&self, messages: Vec<Message>) -> ChunkStream {
        self.requests.lock().unwrap().push(messages);
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = match call {
            0 => vec![
                Ok(Chunk {
                    text: Some("我来写入这个文件。".into()),
                    ..Default::default()
                }),
                Ok(Chunk {
                    empty_response: true,
                    finish_reason: Some(
                        "incomplete_tool_arguments: EOF while parsing a string at line 1 column 16108"
                            .into(),
                    ),
                    ..Default::default()
                }),
            ],
            1 => vec![Ok(Chunk {
                tool_calls: vec![ToolCall {
                    id: "recovered-call".into(),
                    name: "echo".into(),
                    args: serde_json::json!({"text":"recovered evidence"}),
                }],
                ..Default::default()
            })],
            _ => vec![Ok(Chunk {
                text: Some("恢复后的完整答复".into()),
                ..Default::default()
            })],
        };
        Box::pin(futures::stream::iter(chunks))
    }

    fn stream_with_options(&self, messages: Vec<Message>, options: RequestOptions) -> ChunkStream {
        self.options.lock().unwrap().push(options);
        self.stream(messages)
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
    assert!(
        requests[1]
            .iter()
            .any(|m| m.tool_call_id.as_deref() == Some("call-1") && m.content.trim() == "hello")
    );
    assert!(requests[1].iter().any(|m| {
        m.tool_calls.iter().any(|call| call.id == "call-1")
            && m.reasoning_content.as_deref() == Some("先调用 echo 工具取得结果。")
    }));
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
        options: Mutex::new(vec![]),
        // 截图中的真实故障是 finish_reason=stop。它也必须换成紧凑检查点，
        // 不能只有 length 才压缩后重试。
        finish_reason: "stop",
        empty_responses: 1,
        tool_after_empty: false,
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
    assert!(
        requests[1]
            .iter()
            .any(|message| message.content.contains("[响应恢复 1/1·最小快照]"))
    );
    assert!(
        requests[1]
            .iter()
            .all(|message| message.role != harness_llm::Role::Tool)
    );
    let events = log.replay();
    assert!(events.iter().any(|event| matches!(event, SessionEvent::Assistant { chunk, .. } if chunk.text.as_deref() == Some("恢复后的完整答复"))));
    assert!(!events.iter().any(|event| matches!(event, SessionEvent::Assistant { chunk, .. } if chunk.text.as_deref().is_some_and(|text| text.contains("返回了空内容")))));
}

#[tokio::test]
async fn repeated_length_starvation_escalates_budget_and_recovers_in_the_same_turn() {
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let llm = Arc::new(EmptyThenTextLlm {
        calls: AtomicUsize::new(0),
        requests: Mutex::new(vec![]),
        options: Mutex::new(vec![]),
        finish_reason: "length",
        // 精确回放实机故障：正常请求和第一次恢复请求都只产生 reasoning。
        empty_responses: 2,
        tool_after_empty: true,
    });
    let _a = ctx.provide(log.clone());
    let provider: Arc<dyn LlmProvider> = llm.clone();
    let _b = ctx.provide(provider);
    let tools = ToolRegistry::new();
    tools.register(Arc::new(EchoTool));
    let _c = ctx.provide(tools);
    let hook: Arc<dyn Hook> = Arc::new(AllowHook);
    let _d = ctx.provide(hook);

    AgentLoop::new()
        .run_turn(
            &ctx,
            UserInput {
                text: "执行 echo 工具并返回结果".into(),
                attachments: vec![],
            },
        )
        .await
        .unwrap();

    assert_eq!(llm.calls.load(Ordering::SeqCst), 4);
    let options = llm.options.lock().unwrap();
    assert!(
        options[1].max_output_tokens.unwrap_or_default() >= 8_192,
        "首次 length 恢复必须扩容，不能降到 1024"
    );
    assert_eq!(
        options[2].max_output_tokens,
        options[1]
            .max_output_tokens
            .map(|cap| cap.saturating_mul(2).min(32_768))
    );
    assert_eq!(options[1].reasoning_effort.as_deref(), Some("none"));
    assert_eq!(options[2].reasoning_effort.as_deref(), Some("none"));
    assert!(
        options[3].max_output_tokens < options[2].max_output_tokens,
        "拿到有效工具调用后必须退出恢复 profile，避免永久使用高预算"
    );
    let events = log.replay();
    assert!(events.iter().any(|event| matches!(event,
        SessionEvent::Assistant { chunk, .. }
            if chunk.text.as_deref() == Some("恢复后的完整答复")
    )));
    assert!(!events.iter().any(|event| matches!(event,
        SessionEvent::Assistant { chunk, .. }
            if chunk.text.as_deref().is_some_and(|text| text.contains("模型连续"))
    )));
}

#[tokio::test]
async fn truncated_tool_arguments_recover_even_after_a_text_preamble() {
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let llm = Arc::new(PreambleThenIncompleteToolLlm {
        calls: AtomicUsize::new(0),
        requests: Mutex::new(vec![]),
        options: Mutex::new(vec![]),
    });
    let _a = ctx.provide(log.clone());
    let provider: Arc<dyn LlmProvider> = llm.clone();
    let _b = ctx.provide(provider);
    let tools = ToolRegistry::new();
    tools.register(Arc::new(EchoTool));
    let _c = ctx.provide(tools);
    let hook: Arc<dyn Hook> = Arc::new(AllowHook);
    let _d = ctx.provide(hook);

    AgentLoop::new()
        .run_turn(
            &ctx,
            UserInput {
                text: "执行 echo 工具并返回结果".into(),
                attachments: vec![],
            },
        )
        .await
        .unwrap();

    assert_eq!(llm.calls.load(Ordering::SeqCst), 3);
    let options = llm.options.lock().unwrap();
    assert!(options[1].max_output_tokens.unwrap_or_default() >= 8_192);
    assert_eq!(options[1].reasoning_effort.as_deref(), Some("none"));

    let requests = llm.requests.lock().unwrap();
    assert!(requests[1].iter().any(|message| {
        message.content.contains("不得重发整文件 fs write")
            && message.content.contains("残缺调用已丢弃且从未执行")
    }));
    assert!(
        requests[1]
            .iter()
            .all(|message| message.content != "我来写入这个文件。")
    );

    let events = log.replay();
    assert!(events.iter().any(|event| matches!(event,
        SessionEvent::Assistant { chunk, .. }
            if chunk.text.as_deref() == Some("恢复后的完整答复")
    )));
    assert!(!events.iter().any(|event| matches!(event,
        SessionEvent::Assistant { chunk, .. }
            if chunk.text.as_deref().is_some_and(|text| text.contains("llm provider error"))
    )));
}

#[tokio::test]
async fn prompt_cap_does_not_renew_a_repair_without_actual_progress() {
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let llm = Arc::new(CapThenResumeLlm {
        calls: AtomicUsize::new(0),
        requests: Mutex::new(vec![]),
    });
    let _a = ctx.provide(log.clone());
    let provider: Arc<dyn LlmProvider> = llm.clone();
    let _b = ctx.provide(provider);
    let _c = ctx.provide(ToolRegistry::new());
    let hook: Arc<dyn Hook> = Arc::new(AllowHook);
    let _d = ctx.provide(hook);

    AgentLoop::new()
        .run_turn(
            &ctx,
            UserInput {
                text: "修复 src/input.rs 的自动换行问题并验证".into(),
                attachments: vec![],
            },
        )
        .await
        .unwrap();

    let calls = llm.calls.load(Ordering::SeqCst);
    assert_eq!(calls, 1, "仅有下一步描述不能给零修改修复任务续期");
    let events = log.replay();
    assert!(!events.iter().any(|event| matches!(event,
        SessionEvent::Assistant { chunk, .. }
            if chunk.text.as_deref().is_some_and(|text| text.contains("下一条“继续”") || text.contains("是否按以下理解继续"))
    )));
    assert!(!llm.requests.lock().unwrap().iter().skip(1).any(|request| {
        request
            .iter()
            .any(|message| message.content.contains("[预算窗口续期 1/4·最小断点]"))
    }));
}

#[tokio::test]
async fn prompt_cap_auto_renews_after_tool_progress_without_user_continuation() {
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let llm = Arc::new(ProgressAtCapLlm {
        calls: AtomicUsize::new(0),
        requests: Mutex::new(vec![]),
    });
    let tools = ToolRegistry::new();
    tools.register(Arc::new(EchoTool));
    let _a = ctx.provide(log.clone());
    let provider: Arc<dyn LlmProvider> = llm.clone();
    let _b = ctx.provide(provider);
    let _c = ctx.provide(tools);
    let hook: Arc<dyn Hook> = Arc::new(AllowHook);
    let _d = ctx.provide(hook);

    AgentLoop::new()
        .run_turn(
            &ctx,
            UserInput {
                text: "执行 echo 工具并返回结果".into(),
                attachments: vec![],
            },
        )
        .await
        .unwrap();

    assert_eq!(
        llm.calls.load(Ordering::SeqCst),
        2,
        "有工具证据时应在同一用户请求内自动续跑"
    );
    let events = log.replay();
    assert!(!events.iter().any(|event| matches!(event,
        SessionEvent::Assistant { chunk, .. }
            if chunk.text.as_deref().is_some_and(|text| text.contains("【执行预算暂停】"))
    )));
    let requests = llm.requests.lock().unwrap();
    assert!(
        requests[1]
            .iter()
            .any(|message| message.content.contains("[预算窗口续期 1/4·最小断点]"))
    );
    assert!(
        requests[1]
            .iter()
            .all(|message| message.tool_call_id.as_deref() != Some("cap-call-1"))
    );
}

#[tokio::test]
async fn missing_tool_payload_stops_once_instead_of_three_empty_retries() {
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let llm = Arc::new(ToolCallsWithoutPayloadLlm {
        calls: AtomicUsize::new(0),
    });
    let _a = ctx.provide(log.clone());
    let provider: Arc<dyn LlmProvider> = llm.clone();
    let _b = ctx.provide(provider);
    let _c = ctx.provide(ToolRegistry::new());
    let hook: Arc<dyn Hook> = Arc::new(AllowHook);
    let _d = ctx.provide(hook);

    AgentLoop::new()
        .run_turn(
            &ctx,
            UserInput {
                text: "后台管理->多端拼装，这个菜单名称修改为智能体装配".into(),
                attachments: vec![],
            },
        )
        .await
        .unwrap();

    assert_eq!(llm.calls.load(Ordering::SeqCst), 1);
    assert!(log.replay().iter().any(|event| matches!(event,
        SessionEvent::Assistant { chunk, .. }
            if chunk.text.as_deref().is_some_and(|text| text.contains("没有返回可执行的工具名称或参数"))
    )));
    assert!(!log.replay().iter().any(|event| matches!(event,
        SessionEvent::Assistant { chunk, .. }
            if chunk.text.as_deref().is_some_and(|text| text.contains("连续 3 次返回空响应"))
    )));
}

#[tokio::test]
async fn unverified_text_only_step_stops_after_one_state_correction() {
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let llm = Arc::new(TextThenTextLlm {
        calls: AtomicUsize::new(0),
        requests: Mutex::new(vec![]),
    });
    let hook: Arc<dyn Hook> = Arc::new(AllowHook);
    let _a = ctx.provide(log.clone());
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
    assert_eq!(
        requests.len(),
        2,
        "同一受控状态下的纯文本空转只允许一次校正，第二次必须终止"
    );
    assert!(
        requests[1]
            .iter()
            .any(|message| message.content.contains("用户已授权修复"))
    );
    assert!(log.replay().iter().any(|event| matches!(event,
        SessionEvent::Assistant { chunk, .. }
            if chunk.text.as_deref().is_some_and(|text| text.contains("已停止空转"))
    )));
    assert!(log.replay().iter().any(|event| matches!(event,
        SessionEvent::Delivery { report, .. }
            if report.outcome == DeliveryOutcome::PartialDelivery
    )));
    assert!(!log.replay().iter().any(|event| matches!(event,
        SessionEvent::Assistant { chunk, .. }
            if chunk.text.as_deref().is_some_and(|text| text.contains("下一条“继续”") || text.contains("是否按以下理解继续"))
    )));
}

#[tokio::test]
async fn ambiguous_delivery_is_clarified_before_model_or_tool_calls() {
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let llm = Arc::new(TextThenTextLlm {
        calls: AtomicUsize::new(0),
        requests: Mutex::new(vec![]),
    });
    let hook: Arc<dyn Hook> = Arc::new(AllowHook);
    let _a = ctx.provide(log.clone());
    let provider: Arc<dyn LlmProvider> = llm.clone();
    let _b = ctx.provide(provider);
    let _c = ctx.provide(ToolRegistry::new());
    let _d = ctx.provide(hook);

    // 旧门禁语义（澄清前置于模型调用）属 Legacy 逃生门路径；控制器接管后
    // ask_user 受栈深前置约束，首回合先给予接地/定位机会而非直接反问（e0accb7 收敛）。
    AgentLoop::new()
        .with_governor(harness_runtime::GovernorMode::Legacy)
        .run_turn(
            &ctx,
            UserInput {
                text: "这个有问题，帮我修一下".into(),
                attachments: vec![],
            },
        )
        .await
        .unwrap();

    assert_eq!(llm.calls.load(Ordering::SeqCst), 0);
    let events = log.replay();
    assert!(events.iter().any(|event| matches!(event,
        SessionEvent::Assistant { chunk, .. }
            if chunk.text.as_deref().is_some_and(|text| text.contains("请选择一种方式回复") && text.contains("1. ") && text.contains("3. "))
    )));
    assert!(events.iter().any(|event| matches!(event,
        SessionEvent::Delivery { report, .. }
            if report.reason.as_deref().is_some_and(|reason| reason.starts_with("需要补充执行信息："))
    )));
}

#[tokio::test]
async fn exhausted_location_does_not_ask_the_user_to_locate_code() {
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let llm = Arc::new(ScriptedLlm {
        calls: AtomicUsize::new(0),
        initial_text_steps: 0,
        script: vec![
            scripted_call("s1", "search", serde_json::json!({"pattern": "大模型管理"})),
            scripted_call("s2", "search", serde_json::json!({"pattern": "系统管理"})),
            None,
        ],
        options: Mutex::new(vec![]),
    });
    let tools = ToolRegistry::new();
    tools.register(Arc::new(StaticTool {
        name: "search",
        output: "未找到匹配。",
    }));
    let _a = ctx.provide(log.clone());
    let provider: Arc<dyn LlmProvider> = llm;
    let _b = ctx.provide(provider);
    let _c = ctx.provide(tools);
    let hook: Arc<dyn Hook> = Arc::new(AllowHook);
    let _d = ctx.provide(hook);

    AgentLoop::new()
        .run_turn(
            &ctx,
            UserInput {
                text: "在系统管理->大模型管理增加图片和视频接口配置".into(),
                attachments: vec![],
            },
        )
        .await
        .unwrap();

    let events = log.replay();
    let prompts = events
        .iter()
        .filter(|event| {
            matches!(event,
                SessionEvent::Assistant { chunk, .. }
                    if chunk.text.as_deref().is_some_and(|text| text.contains("请选择一种方式回复"))
            )
        })
        .count();
    assert_eq!(prompts, 0, "技术定位失败不得转嫁给用户");
    assert!(!events.iter().any(|event| matches!(event,
        SessionEvent::Delivery { report, .. }
            if report.outcome == harness_session::DeliveryOutcome::NeedsUserInput
    )));
    assert!(events.iter().any(|event| matches!(event,
        SessionEvent::Delivery { report, .. }
            if report.outcome == harness_session::DeliveryOutcome::PartialDelivery
    )));
    assert!(!events.iter().any(|event| matches!(event,
        SessionEvent::Assistant { chunk, .. }
            if chunk.text.as_deref().is_some_and(|text| text.contains("门禁校正") || text.contains("候选文件"))
    )));
}

#[tokio::test]
async fn concrete_problem_replay_starts_with_locate_not_shell_verification() {
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let llm = Arc::new(OptionsCaptureLlm {
        options: Mutex::new(vec![]),
    });
    let hook: Arc<dyn Hook> = Arc::new(AllowHook);
    let _a = ctx.provide(log.clone());
    let provider: Arc<dyn LlmProvider> = llm.clone();
    let _b = ctx.provide(provider);
    let _c = ctx.provide(ToolRegistry::new());
    let _d = ctx.provide(hook);

    AgentLoop::new()
        .run_turn(
            &ctx,
            UserInput {
                // 具体变更请求（含 `修改为` 变换契约），无盲指代、无导航入口：
                // 应乐观放行、首步进入 locate（search），而非跳到 shell 验证或反问。
                text: "把会话窗口发送内容的自动换行逻辑修改为不截断显示".into(),
                attachments: vec![],
            },
        )
        .await
        .unwrap();

    let options = llm.options.lock().unwrap();
    assert!(!options.is_empty());
    assert_eq!(
        options[0].allowed_tools.as_deref(),
        Some(["search".into(), "fs".into(), "edit".into(), "shell".into()].as_slice())
    );
    assert_eq!(options[0].reasoning_effort.as_deref(), Some("none"));
    let events = log.replay();
    assert!(events.iter().any(|event| matches!(event,
        SessionEvent::Telemetry { telemetry, .. }
            if telemetry.intent == "AtomicRegression"
                && telemetry.phase == "locate"
                && telemetry.allowed_tools == vec!["search", "fs"]
    )));
}

#[tokio::test]
async fn quoted_menu_shortening_starts_from_the_grounded_file_not_repository_search() {
    let root = std::env::temp_dir().join(format!(
        "harness-quoted-menu-grounding-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/composer.rs"),
        "ui.button(\"添加到对话框附件\");",
    )
    .unwrap();

    let ctx = AppContext::new();
    let log = SessionLog::new();
    let llm = Arc::new(OptionsCaptureLlm {
        options: Mutex::new(vec![]),
    });
    let hook: Arc<dyn Hook> = Arc::new(AllowHook);
    let _a = ctx.provide(log.clone());
    let provider: Arc<dyn LlmProvider> = llm.clone();
    let _b = ctx.provide(provider);
    let _c = ctx.provide(ToolRegistry::new());
    let _d = ctx.provide(hook);
    let _e = ctx.provide(harness_core::Workspace::new(root.clone()));

    AgentLoop::new()
        .run_turn(
            &ctx,
            UserInput {
                text: "弹出菜单应包含“📎 添加到对话框附件”，文字精简一下，“添加到对话”".into(),
                attachments: vec![],
            },
        )
        .await
        .unwrap();

    let options = llm.options.lock().unwrap();
    assert!(!options.is_empty());
    assert_eq!(
        options[0].allowed_tools.as_deref(),
        Some(["search".into(), "fs".into(), "edit".into(), "shell".into()].as_slice()),
        "运行时已从旧文案直接落到 composer.rs，首步应读取候选而非从仓库根搜索"
    );
    assert_eq!(options[0].reasoning_effort.as_deref(), Some("none"));
    assert!(log.replay().iter().any(|event| matches!(event,
        SessionEvent::Telemetry { telemetry, .. }
            if telemetry.intent == "AtomicRegression"
                && telemetry.phase == "inspect"
                && telemetry.active_work_item.contains("添加到对话")
    )));
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn grounded_candidate_replay_skips_redundant_search() {
    let root = std::env::temp_dir().join(format!("harness-grounded-replay-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/profile.tsx"),
        "export const appCode = form.appCode;",
    )
    .unwrap();

    let ctx = AppContext::new();
    let llm = Arc::new(OptionsCaptureLlm {
        options: Mutex::new(vec![]),
    });
    let hook: Arc<dyn Hook> = Arc::new(AllowHook);
    let _a = ctx.provide(SessionLog::new());
    let provider: Arc<dyn LlmProvider> = llm.clone();
    let _b = ctx.provide(provider);
    let _c = ctx.provide(ToolRegistry::new());
    let _d = ctx.provide(hook);
    let _e = ctx.provide(harness_core::Workspace::new(root.clone()));

    AgentLoop::new()
        .run_turn(
            &ctx,
            UserInput {
                text: "修复应用档案页面 appCode 不显示".into(),
                attachments: vec![],
            },
        )
        .await
        .unwrap();

    let options = llm.options.lock().unwrap();
    assert!(!options.is_empty());
    assert_eq!(
        options[0].allowed_tools.as_deref(),
        Some(["search".into(), "fs".into(), "edit".into(), "shell".into()].as_slice())
    );
    assert_eq!(options[0].max_output_tokens, Some(3_072));
    assert_eq!(options[0].reasoning_effort.as_deref(), Some("low"));
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn premature_text_then_menu_rename_finishes_in_one_user_turn() {
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let llm = Arc::new(ScriptedLlm {
        calls: AtomicUsize::new(0),
        initial_text_steps: 1,
        script: vec![
            scripted_call(
                "s",
                "search",
                serde_json::json!({"pattern": "多端拼装", "dir": "src"}),
            ),
            scripted_call(
                "r",
                "fs",
                serde_json::json!({"op": "read", "path": "src/menu.rs"}),
            ),
            scripted_call(
                "e",
                "edit",
                serde_json::json!({"path": "src/menu.rs", "old": "多端拼装", "new": "智能体装配"}),
            ),
            scripted_call(
                "v",
                "shell",
                serde_json::json!({"cmd": "cargo test menu_name"}),
            ),
            None,
        ],
        options: Mutex::new(vec![]),
    });
    let tools = ToolRegistry::new();
    tools.register(Arc::new(StaticTool {
        name: "search",
        output: "共 1 条命中（格式：相对路径:行号: 内容）：\nsrc/menu.rs:9: 多端拼装",
    }));
    tools.register(Arc::new(StaticTool {
        name: "fs",
        output: "Menu { name: \"多端拼装\" }",
    }));
    tools.register(Arc::new(StaticTool {
        name: "edit",
        output: "updated src/menu.rs",
    }));
    tools.register(Arc::new(StaticTool {
        name: "shell",
        output: "test menu_name ... ok",
    }));
    let _a = ctx.provide(log.clone());
    let provider: Arc<dyn LlmProvider> = llm.clone();
    let _b = ctx.provide(provider);
    let _c = ctx.provide(tools);
    let hook: Arc<dyn Hook> = Arc::new(AllowHook);
    let _d = ctx.provide(hook);

    AgentLoop::new()
        .run_turn(
            &ctx,
            UserInput {
                text: "后台管理->多端拼装，这个菜单名称修改为“智能体装配”".into(),
                attachments: vec![],
            },
        )
        .await
        .unwrap();

    assert_eq!(llm.calls.load(Ordering::SeqCst), 6);
    let phases = log
        .replay()
        .iter()
        .filter_map(|event| match event {
            SessionEvent::Telemetry { telemetry, .. } => Some(telemetry.phase.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(phases.iter().any(|phase| phase == "inspect"));
    assert!(phases.iter().any(|phase| phase == "change"));
    assert!(phases.iter().any(|phase| phase == "verify"));
    assert!(log.replay().iter().any(|event| matches!(event,
        SessionEvent::Delivery { report, .. }
            if report.outcome == harness_session::DeliveryOutcome::Verified
    )));
}

#[tokio::test]
async fn v4_already_satisfied_replay_verifies_without_editing() {
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let llm = Arc::new(ScriptedLlm {
        calls: AtomicUsize::new(0),
        initial_text_steps: 0,
        script: vec![
            scripted_call("s", "search", serde_json::json!({"pattern": "version"})),
            scripted_call(
                "r",
                "fs",
                serde_json::json!({"op": "read", "path": "Cargo.toml"}),
            ),
            scripted_call("v", "shell", serde_json::json!({"cmd": "cargo check"})),
            None,
        ],
        options: Mutex::new(vec![]),
    });
    let tools = ToolRegistry::new();
    tools.register(Arc::new(StaticTool {
        name: "search",
        output: "共 1 条命中（格式：相对路径:行号: 内容）：\nCargo.toml:3: version = \"0.2.2\"",
    }));
    tools.register(Arc::new(StaticTool {
        name: "fs",
        output: "[package]\nversion = \"0.2.2\"",
    }));
    tools.register(Arc::new(StaticTool {
        name: "shell",
        output: "Finished dev profile",
    }));
    let _a = ctx.provide(log.clone());
    let provider: Arc<dyn LlmProvider> = llm.clone();
    let _b = ctx.provide(provider);
    let _c = ctx.provide(tools);
    let hook: Arc<dyn Hook> = Arc::new(AllowHook);
    let _d = ctx.provide(hook);

    AgentLoop::new()
        .run_turn(
            &ctx,
            UserInput {
                text: "把版本号修改为 0.2.2".into(),
                attachments: vec![],
            },
        )
        .await
        .unwrap();

    assert_eq!(llm.calls.load(Ordering::SeqCst), 4);
    assert!(!log.replay().iter().any(|event| matches!(event,
        SessionEvent::ToolCall { call, .. } if call.name == "edit"
    )));
    assert!(log.replay().iter().any(|event| matches!(event,
        SessionEvent::Telemetry { telemetry, .. }
            if telemetry.detail.contains("AlreadySatisfied")
    )));
}

#[tokio::test]
async fn document_implementation_cannot_finish_when_edit_tool_changes_nothing() {
    let root = std::env::temp_dir().join(format!(
        "harness-document-noop-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn existing() {}\n").unwrap();
    std::fs::write(root.join("docs/DESIGN.md"), "# New subsystem\n").unwrap();

    let ctx = AppContext::new();
    let log = SessionLog::new();
    let llm = Arc::new(ScriptedLlm {
        calls: AtomicUsize::new(0),
        initial_text_steps: 0,
        script: vec![
            scripted_call(
                "doc-search",
                "search",
                serde_json::json!({"pattern": "document-noop-implementation"}),
            ),
            scripted_call(
                "doc-read",
                "fs",
                serde_json::json!({"op": "read", "path": "src/lib.rs"}),
            ),
            scripted_call(
                "doc-edit",
                "edit",
                serde_json::json!({
                    "path": "src/lib.rs",
                    "old_text": "pub fn existing() {}",
                    "new_text": "pub fn implemented() {}"
                }),
            ),
            None,
        ],
        options: Mutex::new(vec![]),
    });
    let tools = ToolRegistry::new();
    tools.register(Arc::new(StaticTool {
        name: "search",
        output: "共 1 条命中（格式：相对路径:行号: 内容）：\nsrc/lib.rs:1: pub fn existing() {}",
    }));
    tools.register(Arc::new(StaticTool {
        name: "fs",
        output: "pub fn existing() {}",
    }));
    // 故意谎报 updated、但不触碰磁盘，复现截图中的“零改动假完成”。
    tools.register(Arc::new(StaticTool {
        name: "edit",
        output: "updated src/lib.rs",
    }));
    let _a = ctx.provide(log.clone());
    let provider: Arc<dyn LlmProvider> = llm;
    let _b = ctx.provide(provider);
    let _c = ctx.provide(tools);
    let hook: Arc<dyn Hook> = Arc::new(AllowHook);
    let _d = ctx.provide(hook);
    let _e = ctx.provide(Workspace::new(root.clone()));

    AgentLoop::new()
        .run_turn(
            &ctx,
            UserInput {
                text: "按 docs/DESIGN.md 实施开发".into(),
                attachments: vec![],
            },
        )
        .await
        .unwrap();

    let events = log.replay();
    assert!(events.iter().any(|event| matches!(
        event,
        SessionEvent::ToolResult { result, .. }
            if result.content.contains("[write-not-counted]")
    )));
    assert!(!events.iter().any(|event| matches!(
        event,
        SessionEvent::Delivery { report, .. }
            if report.outcome == harness_session::DeliveryOutcome::Verified
    )));
    assert_eq!(
        std::fs::read_to_string(root.join("src/lib.rs")).unwrap(),
        "pub fn existing() {}\n"
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// 流内直接吐 Err 的 Provider：复现「4xx 报错被当成最终回答」的假绿场景。
struct ErrorLlm;
#[async_trait]
impl LlmProvider for ErrorLlm {
    fn name(&self) -> &'static str {
        "error-test"
    }
    fn tools(&self) -> Vec<harness_llm::ToolSchema> {
        vec![]
    }
    fn stream(&self, _m: Vec<Message>) -> ChunkStream {
        Box::pin(futures::stream::iter(vec![Err(
            harness_core::error::Error::Llm("http 403 AccountOverdueError".into()),
        )]))
    }
}

#[tokio::test]
async fn provider_error_never_delivers_verified() {
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let tools = ToolRegistry::new();
    let hook: Arc<dyn Hook> = Arc::new(AllowHook);
    let mut registrations = vec![];
    registrations.push(ctx.provide(log.clone()));
    let provider: Arc<dyn LlmProvider> = Arc::new(ErrorLlm);
    registrations.push(ctx.provide(provider));
    registrations.push(ctx.provide(tools));
    registrations.push(ctx.provide(hook));

    let _ = AgentLoop::new()
        .run_turn(
            &ctx,
            UserInput {
                text: "hi".into(),
                attachments: vec![],
            },
        )
        .await; // 错误已内化为收口，不再上抛；两可（Ok/Err）都不许 Verified

    let outcomes: Vec<_> = log
        .replay()
        .into_iter()
        .filter_map(|e| match e {
            SessionEvent::Delivery { report, .. } => Some(report.outcome),
            _ => None,
        })
        .collect();
    assert_eq!(outcomes.len(), 1, "回合必须且只交付一次");
    assert!(
        !matches!(outcomes[0], harness_session::DeliveryOutcome::Verified),
        "provider 错误不得交付 Verified，实际 {:?}",
        outcomes[0]
    );
    let texts: Vec<String> = log
        .replay()
        .into_iter()
        .filter_map(|e| match e {
            SessionEvent::Assistant { chunk, .. } => chunk.text,
            _ => None,
        })
        .collect();
    assert!(
        texts.iter().any(|t| t.contains("[error]")),
        "错误须对用户可见: {texts:?}"
    );
}

/// 会话日志必须满足 Provider 的硬协议：assistant 宣告的每个 tool_call 都要有且只有一条
/// 同 `call_id` 的 ToolResult。搜索记忆化缓存曾把首次调用的整条 `ToolResult`（含
/// `call_id`）原样写入日志，使后续同参数调用既留下无响应的 tool_call（DeepSeek 400），
/// 又留下一条紧邻 assistant 并不持有的 tool 消息（OpenAI 兼容端 400）。
#[tokio::test]
async fn search_memo_hit_keeps_tool_results_pairing_one_to_one() {
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let llm = Arc::new(ScriptedLlm {
        calls: AtomicUsize::new(0),
        initial_text_steps: 0,
        script: vec![
            scripted_call(
                "memo-call-1",
                "search",
                serde_json::json!({"pattern": "target-anchor gate"}),
            ),
            scripted_call(
                "memo-call-2",
                "search",
                serde_json::json!({"pattern": "另一关键词"}),
            ),
            // 与第一条参数完全相同：命中缓存，但必须记在本条 call_id 下。
            scripted_call(
                "memo-call-3",
                "search",
                serde_json::json!({"pattern": "target-anchor gate"}),
            ),
            None,
        ],
        options: Mutex::new(vec![]),
    });
    let tools = ToolRegistry::new();
    tools.register(Arc::new(StaticTool {
        name: "search",
        output: "命中 2 行",
    }));
    let _a = ctx.provide(log.clone());
    let provider: Arc<dyn LlmProvider> = llm;
    let _b = ctx.provide(provider);
    let _c = ctx.provide(tools);
    let hook: Arc<dyn Hook> = Arc::new(AllowHook);
    let _d = ctx.provide(hook);

    AgentLoop::new()
        .run_turn(
            &ctx,
            UserInput {
                text: "反复检索同一关键词后汇总结论".into(),
                attachments: vec![],
            },
        )
        .await
        .unwrap();

    let events = log.replay();
    let announced: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::Assistant { chunk, .. } => Some(
                chunk
                    .tool_calls
                    .iter()
                    .map(|call| call.id.clone())
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .flatten()
        .collect();
    let mut responded: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for event in &events {
        if let SessionEvent::ToolResult { result, .. } = event {
            *responded.entry(result.call_id.as_str()).or_default() += 1;
        }
    }

    assert!(announced.len() >= 3, "脚本未产生预期的工具调用: {announced:?}");
    let missing: Vec<&String> = announced
        .iter()
        .filter(|id| responded.get(id.as_str()).copied().unwrap_or(0) == 0)
        .collect();
    assert!(
        missing.is_empty(),
        "assistant 宣告的 tool_call 缺少同 id 响应，Provider 会拒绝整个历史: {missing:?}"
    );
    let duplicated: Vec<&&str> = responded
        .iter()
        .filter(|(id, count)| announced.iter().any(|a| a == *id) && **count > 1)
        .map(|(id, _)| id)
        .collect();
    assert!(
        duplicated.is_empty(),
        "同一 tool_call 记录了多条响应，说明缓存把旧 call_id 带进了新步骤: {duplicated:?}"
    );
}

/// 记录真实派发次数的工具：准入降级后，被"建议"的动作必须仍然到达工具层。
struct CountingTool {
    name: &'static str,
    hits: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl DynTool for CountingTool {
    fn name(&self) -> &'static str {
        self.name
    }
    async fn call(&self, call: &ToolCall) -> Result<ToolResult> {
        self.hits.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult {
            call_id: call.id.clone(),
            ok: true,
            content: "命中 1 行：src/demo.py:1".into(),
            continuation_debt: 0,
        })
    }
}

/// 红线：任何一次工具调用都不得因「计数/阶段/关联」类判断被替换成错误工具结果。
#[tokio::test]
async fn advisory_gates_never_replace_a_dispatched_tool_result() {
    let ctx = AppContext::new();
    let log = SessionLog::new();
    // 同一文件重复读、与验收项无关联的搜索、重复 search：全是旧门禁最爱拦的形态。
    let llm = Arc::new(ScriptedLlm {
        calls: AtomicUsize::new(0),
        initial_text_steps: 0,
        script: vec![
            scripted_call("a1", "search", serde_json::json!({"pattern": "save_draft"})),
            scripted_call("a2", "fs", serde_json::json!({"op": "read", "path": "server/routers/strategy.py"})),
            scripted_call("a3", "fs", serde_json::json!({"op": "read", "path": "server/routers/strategy.py"})),
            scripted_call("a4", "search", serde_json::json!({"pattern": "save_draft"})),
            scripted_call("a5", "fs", serde_json::json!({"op": "read", "path": "server/routers/strategy.py"})),
            scripted_call("a6", "search", serde_json::json!({"pattern": "publish_version"})),
            None,
        ],
        options: Mutex::new(vec![]),
    });
    let search_hits = Arc::new(CountingTool {
        name: "search",
        hits: AtomicUsize::new(0),
    });
    let fs_hits = Arc::new(CountingTool {
        name: "fs",
        hits: AtomicUsize::new(0),
    });
    let tools = ToolRegistry::new();
    tools.register(search_hits.clone());
    tools.register(fs_hits.clone());
    let _a = ctx.provide(log.clone());
    let provider: Arc<dyn LlmProvider> = llm;
    let _b = ctx.provide(provider);
    let _c = ctx.provide(tools);
    let hook: Arc<dyn Hook> = Arc::new(AllowHook);
    let _d = ctx.provide(hook);

    AgentLoop::new()
        .run_turn(
            &ctx,
            UserInput {
                text: "策略页保存报错，定位并修复".into(),
                attachments: vec![],
            },
        )
        .await
        .unwrap();

    let events = log.replay();
    let denied: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::ToolResult { result, .. } => {
                let hit = ["gate]", "guard]"]
                    .iter()
                    .any(|marker| result.content.contains(marker));
                hit.then(|| result.content.chars().take(160).collect())
            }
            _ => None,
        })
        .collect();
    assert!(denied.is_empty(), "计数/阶段类判断仍在吞动作: {denied:#?}");
    // search 属 is_search_like，命中 SEARCH_MEMO 时合法地不进工具层（复用同查询输出），
    // 它不是拦停。此处只允许用「无拦停文本」约束它；非记忆化工具才断言真实派发。
    let _ = &search_hits;
    assert_eq!(
        fs_hits.hits.load(Ordering::SeqCst),
        3,
        "三次读取必须全部到达工具层"
    );
}
/// 带出站请求捕获的脚本模型：每轮固定发一个工具调用，同时留下发给模型的历史，
/// 用于断言提示是否随下一步请求注入。
struct CapturingScriptedLlm {
    calls: AtomicUsize,
    requests: Mutex<Vec<Vec<Message>>>,
    script: Vec<Option<ToolCall>>,
}

#[async_trait]
impl LlmProvider for CapturingScriptedLlm {
    fn name(&self) -> &'static str {
        "capturing-scripted-test"
    }
    fn tools(&self) -> Vec<harness_llm::ToolSchema> {
        vec![]
    }
    fn stream(&self, messages: Vec<Message>) -> ChunkStream {
        self.requests.lock().unwrap().push(messages);
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunk = match self.script.get(index).cloned().flatten() {
            Some(call) => Chunk {
                tool_calls: vec![call],
                ..Default::default()
            },
            None => Chunk {
                text: Some("已基于现有证据收尾".into()),
                ..Default::default()
            },
        };
        Box::pin(futures::stream::iter(vec![Ok(chunk)]))
    }
}

fn denied_contents(events: &[SessionEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::ToolResult { result, .. } => ["gate]", "guard]"]
                .iter()
                .any(|marker| result.content.contains(marker))
                .then(|| result.content.chars().take(160).collect()),
            _ => None,
        })
        .collect()
}

fn hinted(requests: &[Vec<Message>], skip_first: bool) -> bool {
    requests
        .iter()
        .skip(if skip_first { 1 } else { 0 })
        .any(|request| {
            request.iter().any(|message| {
                message.role == harness_llm::Role::User
                    && message.content.contains("未新增信息")
            })
        })
}

/// Task 3 契约：ActionGate 的 Advise 必须「动作照常派发 + 提示注入下一步请求」，
/// 且原因文本永不出现在任何工具结果或 tool 消息里（红线本体）。
///
/// 载体必须是非记忆化工具：shell 验证调用不属 is_search_like（不走 SEARCH_MEMO
/// 复用，故派发次数可断言）、每条命令签名不同（不触发 ToolRepeatGuard 的紧邻
/// 同签名与累计上限），且零写入时 link_proposal 会把验证调用的 supports 置空，
/// 恰好命中 supports.is_empty() 的 Advise 分支。
#[tokio::test]
async fn zero_gain_advice_is_injected_without_swallowing_the_action() {
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let llm = Arc::new(CapturingScriptedLlm {
        calls: AtomicUsize::new(0),
        requests: Mutex::new(vec![]),
        script: vec![
            scripted_call("v1", "shell", serde_json::json!({"command": "cargo test -p alpha"})),
            scripted_call("v2", "shell", serde_json::json!({"command": "cargo test -p beta"})),
            scripted_call("v3", "shell", serde_json::json!({"command": "cargo test -p gamma"})),
            None,
        ],
    });
    let shell_hits = Arc::new(CountingTool {
        name: "shell",
        hits: AtomicUsize::new(0),
    });
    let tools = ToolRegistry::new();
    tools.register(shell_hits.clone());
    let _a = ctx.provide(log.clone());
    let provider: Arc<dyn LlmProvider> = llm.clone();
    let _b = ctx.provide(provider);
    let _c = ctx.provide(tools);
    let hook: Arc<dyn Hook> = Arc::new(AllowHook);
    let _d = ctx.provide(hook);

    AgentLoop::new()
        .run_turn(
            &ctx,
            UserInput {
                text: "策略页保存报错，修复后验证".into(),
                attachments: vec![],
            },
        )
        .await
        .unwrap();

    let events = log.replay();
    let denied = denied_contents(&events);
    assert!(denied.is_empty(), "Advise 不得替换成工具结果: {denied:#?}");
    assert_eq!(
        shell_hits.hits.load(Ordering::SeqCst),
        3,
        "三次未关联验收项的验证调用必须全部到达工具层"
    );
    let results: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::ToolResult { result, .. } => Some(result),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 3, "每个 tool_call 恰有一条结果");
    assert!(results.iter().all(|result| result.ok), "放行的动作必须成功返回");
    let requests = llm.requests.lock().unwrap();
    assert!(
        hinted(&requests, true),
        "提示须以 user 消息随下一步请求注入"
    );
    let leaked = requests.iter().any(|request| {
        request.iter().any(|message| {
            message.role == harness_llm::Role::Tool && message.content.contains("未新增信息")
        })
    });
    assert!(!leaked, "Advise 原因文本不得进入任何 tool 消息");
}

/// 修正 3 契约：同签名 search 命中 SEARCH_MEMO 时合法地复用而不进工具层，它不是
/// 拦停。这里锁定复用的三项不变量——仍然给出成功结果、配对完整、只追加提示——
/// 防止将来把复用改回用 [tool-loop guard] 吞动作。
///
/// 复用只在守卫放行后才可达：ToolRepeatGuard 的累计上限是 2，故同签名至多出现
/// 两次，中间必须夹一个不同签名的动作以避开「紧邻同签名且成功」的拦截。
#[tokio::test]
async fn repeated_search_replays_from_memo_without_denial() {
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let llm = Arc::new(CapturingScriptedLlm {
        calls: AtomicUsize::new(0),
        requests: Mutex::new(vec![]),
        script: vec![
            // pattern 必须本测试独有：SEARCH_MEMO 是进程级 static 且键只含 (工具名, 参数)，
            // 与同二进制其它测试共用 pattern 会随并发顺序污染缓存，使首次派发数不确定。
            scripted_call("s1", "search", serde_json::json!({"pattern": "memo_contract_unique"})),
            scripted_call("f1", "fs", serde_json::json!({"op": "read", "path": "a.py"})),
            scripted_call("s2", "search", serde_json::json!({"pattern": "memo_contract_unique"})),
            scripted_call("f2", "fs", serde_json::json!({"op": "read", "path": "b.py"})),
            None,
        ],
    });
    let search_hits = Arc::new(CountingTool {
        name: "search",
        hits: AtomicUsize::new(0),
    });
    let fs_hits = Arc::new(CountingTool {
        name: "fs",
        hits: AtomicUsize::new(0),
    });
    let tools = ToolRegistry::new();
    tools.register(search_hits.clone());
    tools.register(fs_hits.clone());
    let _a = ctx.provide(log.clone());
    let provider: Arc<dyn LlmProvider> = llm.clone();
    let _b = ctx.provide(provider);
    let _c = ctx.provide(tools);
    let hook: Arc<dyn Hook> = Arc::new(AllowHook);
    let _d = ctx.provide(hook);

    AgentLoop::new()
        .run_turn(
            &ctx,
            UserInput {
                text: "策略页保存报错，定位后修复".into(),
                attachments: vec![],
            },
        )
        .await
        .unwrap();

    let events = log.replay();
    let denied = denied_contents(&events);
    assert!(denied.is_empty(), "缓存复用不是拦停: {denied:#?}");
    assert_eq!(
        search_hits.hits.load(Ordering::SeqCst),
        1,
        "同签名 search 只应真实派发一次，其余走复用"
    );
    let search_results: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::ToolResult { result, .. } if result.call_id.starts_with('s') => {
                Some(result)
            }
            _ => None,
        })
        .collect();
    assert_eq!(search_results.len(), 2, "两次 search 宣告都必须有响应");
    assert!(
        search_results.iter().all(|result| result.ok),
        "复用必须返回成功结果，不得退化为错误结果"
    );
    assert_eq!(fs_hits.hits.load(Ordering::SeqCst), 2, "不同文件的读取都要真实派发");
    assert!(
        hinted(&llm.requests.lock().unwrap(), true),
        "复用只追加一条提示"
    );
}
