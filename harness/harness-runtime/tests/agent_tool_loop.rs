use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harness_capability::hook::{Hook, HookDecision, HookPayload};
use harness_core::error::Result;
use harness_core::types::UserInput;
use harness_core::AppContext;
use harness_llm::{
    Chunk, ChunkStream, LlmProvider, Message, RequestOptions, ToolCall, ToolResult, Usage,
};
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
        let chunk = match self.script.get(index).cloned().flatten() {
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
    finish_reason: &'static str,
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
        let chunk = if call == 0 {
            Chunk {
                empty_response: true,
                finish_reason: Some(self.finish_reason.into()),
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
        // 截图中的真实故障是 finish_reason=stop。它也必须换成紧凑检查点，
        // 不能只有 length 才压缩后重试。
        finish_reason: "stop",
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
        .any(|message| message.content.contains("[空响应恢复 1/1·最小快照]")));
    assert!(requests[1]
        .iter()
        .all(|message| message.role != harness_llm::Role::Tool));
    let events = log.replay();
    assert!(events.iter().any(|event| matches!(event, SessionEvent::Assistant { chunk, .. } if chunk.text.as_deref() == Some("恢复后的完整答复"))));
    assert!(!events.iter().any(|event| matches!(event, SessionEvent::Assistant { chunk, .. } if chunk.text.as_deref().is_some_and(|text| text.contains("返回了空内容")))));
}

#[tokio::test]
async fn continuation_gets_a_fresh_prompt_budget_after_previous_turn_hit_cap() {
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

    assert_eq!(llm.calls.load(Ordering::SeqCst), 1, "首回合应在边界暂停");
    let first_turn = log.replay();
    assert!(first_turn.iter().any(|event| matches!(event,
        SessionEvent::Assistant { chunk, .. }
            if chunk.text.as_deref().is_some_and(|text| text.contains("【执行预算暂停】"))
    )));
    assert!(!first_turn.iter().any(|event| matches!(event,
        SessionEvent::Assistant { chunk, .. }
            if chunk.text.as_deref().is_some_and(|text| text.contains("是否按以下理解继续"))
    )));

    AgentLoop::new()
        .run_turn(
            &ctx,
            UserInput {
                text: "继续".into(),
                attachments: vec![],
            },
        )
        .await
        .unwrap();

    assert!(
        llm.calls.load(Ordering::SeqCst) >= 2,
        "历史累计已触顶时，明确续跑仍必须真正请求模型"
    );
    assert!(llm.requests.lock().unwrap().iter().skip(1).any(|request| {
        request
            .iter()
            .any(|message| message.content.contains("[续跑任务]"))
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
    assert!(requests[1]
        .iter()
        .any(|message| message.content.contains("[预算窗口续期 1/4·最小断点]")));
    assert!(requests[1]
        .iter()
        .all(|message| message.tool_call_id.as_deref() != Some("cap-call-1")));
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
        .any(|message| message.content.contains("[V4 目标状态校正]")));
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
            if chunk.text.as_deref().is_some_and(|text| text.contains("定位"))
    )));
    assert!(events.iter().any(|event| matches!(event,
        SessionEvent::Delivery { report, .. }
            if report.reason.as_deref().is_some_and(|reason| reason.starts_with("需要补充执行信息："))
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
        Some(["search".into()].as_slice())
    );
    assert_eq!(options[0].reasoning_effort.as_deref(), Some("none"));
    let events = log.replay();
    assert!(events.iter().any(|event| matches!(event,
        SessionEvent::Telemetry { telemetry, .. }
            if telemetry.intent == "AtomicRegression"
                && telemetry.phase == "locate"
                && telemetry.allowed_tools == vec!["search"]
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
                text: "弹出菜单应包含“📎 添加到对话框附件”，文字精简一下，“添加到对话”"
                    .into(),
                attachments: vec![],
            },
        )
        .await
        .unwrap();

    let options = llm.options.lock().unwrap();
    assert!(!options.is_empty());
    assert_eq!(
        options[0].allowed_tools.as_deref(),
        Some(["fs".into(), "search".into()].as_slice()),
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
        Some(["fs".into(), "search".into()].as_slice())
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn exact_menu_rename_uses_four_tools_without_bruteforce() {
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let llm = Arc::new(ScriptedLlm {
        calls: AtomicUsize::new(0),
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

    assert_eq!(llm.calls.load(Ordering::SeqCst), 5);
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
