//! 集成测试：DSML 流 → 工具执行闭环（回复质量与多步续跑两大修复的端到端验证）。
//!
//! 模拟 DeepSeek-v4 原生行为：工具调用以 DSML 文本跨帧分片写入 content，
//! 经 `dsml::filter_stream` 后必须解析为 `ToolCall` 并真实分发执行；
//! reasoning 增量只进 `Thinking` 事件，不进入模型上下文。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harness_capability::hook::Hook;
use harness_core::{AppContext, error::Result, types::UserInput};
use harness_llm::{
    Chunk, ChunkStream, LlmProvider, Message, ToolCall, ToolResult, ToolSchema, dsml,
};
use harness_provider_hook::NullHook;
use harness_runtime::AgentLoop;
use harness_session::{SessionEvent, SessionLog};
use harness_tool::{DynTool, ToolRegistry};

/// 脚本化 Provider：第一次调用发跨帧 DSML 工具调用 + reasoning；第二次发收尾文本。
struct ScriptedLlm {
    requests: Arc<Mutex<Vec<Vec<Message>>>>,
}

#[async_trait]
impl LlmProvider for ScriptedLlm {
    fn name(&self) -> &'static str {
        "scripted"
    }

    fn tools(&self) -> Vec<ToolSchema> {
        vec![]
    }

    fn stream(&self, msgs: Vec<Message>) -> ChunkStream {
        let n = {
            let mut g = self.requests.lock().unwrap();
            g.push(msgs);
            g.len()
        };
        let chunks: Vec<Chunk> = if n == 1 {
            vec![
                Chunk {
                    text: Some("我先来列一下目录。".into()),
                    reasoning: Some("thinking-secret".into()),
                    ..Default::default()
                },
                Chunk {
                    text: Some("请稍等<｜DSML｜tool_cal".into()),
                    ..Default::default()
                },
                Chunk {
                    text: Some(
                        "ls>\n<｜DSML｜invoke name=\"exec_command\">\n<｜DSML｜parameter name=\"cmd\" string=\"true\">dir /b</｜DSML｜parameter>\n</｜DSML｜invoke>\n</｜DSML｜tool_calls>"
                            .into(),
                    ),
                    ..Default::default()
                },
            ]
        } else {
            vec![Chunk {
                text: Some("完成：目录包含 3 个文件。".into()),
                ..Default::default()
            }]
        };
        dsml::filter_stream(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
    }
}

/// 记录型 shell 工具：捕获命令并返回固定输出。
struct RecShell(Arc<Mutex<Vec<String>>>);

#[async_trait]
impl DynTool for RecShell {
    fn name(&self) -> &'static str {
        "shell"
    }

    async fn call(&self, call: &ToolCall) -> Result<ToolResult> {
        let cmd = call
            .args
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        self.0.lock().unwrap().push(cmd);
        Ok(ToolResult {
            call_id: call.id.clone(),
            ok: true,
            content: "a.txt\nb.txt".into(),
            continuation_debt: 0,
        })
    }
}

#[tokio::test]
async fn dsml_text_becomes_executed_tool_and_context_stays_clean() {
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let _a = ctx.provide(log.clone());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let llm: Arc<dyn LlmProvider> = Arc::new(ScriptedLlm {
        requests: requests.clone(),
    });
    let _b = ctx.provide(llm);
    let tools = ToolRegistry::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    tools.register(Arc::new(RecShell(seen.clone())));
    let _c = ctx.provide(tools);
    let hook: Arc<dyn Hook> = Arc::new(NullHook);
    let _d = ctx.provide(hook);

    AgentLoop::new()
        .run_turn(
            &ctx,
            UserInput {
                text: "检查一下目录".into(),
                attachments: vec![],
            },
        )
        .await
        .unwrap();

    // 1) DSML 被解析、映射为 shell 工具并真实执行（exec_command → shell{command}）。
    let cmds = seen.lock().unwrap();
    assert_eq!(cmds.as_slice(), ["dir /b"]);

    // 2) 日志含 ToolCall/ToolResult/Thinking/StepStart，Turn/Step 生命周期完整。
    let events = log.replay();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SessionEvent::ToolCall { call, .. } if call.name == "shell"))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SessionEvent::ToolResult { result, .. } if result.ok))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SessionEvent::Thinking { text, .. } if text == "thinking-secret"))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SessionEvent::StepStart { step: 1, .. }))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SessionEvent::StepEnd { step: 2, .. }))
    );

    // 3) 回复正文不含 DSML 裸标记，且包含收尾总结（debt 续跑生效）。
    let assistant: String = events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::Assistant { chunk, .. } => chunk.text.clone(),
            _ => None,
        })
        .collect();
    assert!(!assistant.contains("DSML"));
    assert!(assistant.contains("完成：目录包含 3 个文件。"));

    // 4) 续跑请求（第二次模型调用）上下文：无 reasoning、无 DSML 残留，且含工具结果。
    let reqs = requests.lock().unwrap();
    assert_eq!(reqs.len(), 2, "工具结果应产生续跑 step");
    for m in &reqs[1] {
        if m.role == harness_llm::Role::System {
            continue; // 系统提示词措辞本身提及 DSML，不计入残留检查
        }
        assert!(!m.content.contains("thinking-secret"));
        assert!(!m.content.contains("DSML"));
    }
    assert!(reqs[1].iter().any(|m| m.content.contains("a.txt")));
}
