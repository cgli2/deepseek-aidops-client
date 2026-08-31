//! 端到端验证：async 并发执行器（Phase 1）在运行时真正生效。
//!
//! 构造一个多面 `ReadyToChange` 任务：N 个交付面分成 2 个「写冲突组」
//! （前两个面改 `shared.tsx`，后两个面改 `other.tsx`）。当 `parallel_write_groups`
//! 返回 >1 组时，`agent_loop` 会对每个组并发开一轮作用域化模型往返（融合流），
//! 而非单一流串行推进。本测试用脚本化 LLM 驱动整条 loop，断言：
//!   1) 至少 2 次模型调用携带 `本轮聚焦` 标记（并发多组分支确实执行）；
//!   2) `edit` 工具被并发派发到 ≥2 个不同文件（跨组并行 dispatch）；
//!   3) 回合正常结束（未 panic / 未无限循环）。
//!
//! 这直接坐实「打包后真的并发推进多面」，而非仅机制层单测。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harness_capability::hook::Hook;
use harness_core::{error::Result, types::UserInput, AppContext};
use harness_llm::{
    Chunk, ChunkStream, LlmProvider, Message, RequestOptions, Role, ToolCall, ToolResult,
    ToolSchema,
};
use harness_provider_hook::NullHook;
use harness_runtime::{AgentLoop, GoalExecution, TaskContract, WorkItemState};
use harness_session::SessionLog;
use harness_tool::{DynTool, ToolRegistry};

/// 记录型 edit 工具：捕获被派发的 (路径) 调用并返回成功。
struct RecEdit {
    seen: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl DynTool for RecEdit {
    fn name(&self) -> &'static str {
        "edit"
    }
    async fn call(&self, call: &ToolCall) -> Result<ToolResult> {
        let path = call
            .args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        self.seen.lock().unwrap().push(path.clone());
        Ok(ToolResult {
            call_id: call.id.clone(),
            ok: true,
            content: format!("edited {path}"),
            continuation_debt: 0,
        })
    }
}

/// 脚本化多面 LLM：对每个作用域流，从提示里提取聚焦面 id，发一个 `edit` 调用。
struct MultiSurfaceLlm {
    requests: Arc<Mutex<Vec<Vec<Message>>>>,
    calls: AtomicUsize,
    surface_file: Arc<HashMap<String, String>>,
}

#[async_trait]
impl LlmProvider for MultiSurfaceLlm {
    fn name(&self) -> &'static str {
        "multi-surface-scripted"
    }
    fn tools(&self) -> Vec<ToolSchema> {
        vec![]
    }
    fn stream(&self, messages: Vec<Message>) -> ChunkStream {
        self.stream_with_options(messages, RequestOptions::default())
    }
    fn stream_with_options(&self, messages: Vec<Message>, _options: RequestOptions) -> ChunkStream {
        {
            let mut g = self.requests.lock().unwrap();
            g.push(messages.clone());
        }
        let idx = self.calls.fetch_add(1, Ordering::SeqCst);
        // 提取聚焦面 id：作用域提示形如 `·本轮聚焦 item-1/item-2]`。
        // 注意：提示以 `·本轮聚焦 ` 开头（空格在前），必须用 split_whitespace 跳过前导
        // 分隔符（裸 split 会返回前导空串），再按 `/` 拆出首个面 id。
        let focused = messages.iter().find_map(|m| {
            if m.role == Role::System && m.content.contains("本轮聚焦") {
                m.content
                    .split("本轮聚焦")
                    .nth(1)
                    .and_then(|rest| rest.split_whitespace().next())
                    .and_then(|tok| tok.split('/').next())
                    .map(|s| s.trim_matches(|c| c == ']' || c == '[').to_string())
            } else {
                None
            }
        });
        // 退化：找不到聚焦标记（单面作用域退化为全局提示）时，退回首个已知面。
        let id = focused.unwrap_or_else(|| self.surface_file.keys().next().cloned().unwrap_or_default());
        let file = self
            .surface_file
            .get(&id)
            .cloned()
            .unwrap_or_else(|| format!("{id}.tsx"));
        let chunk = Chunk {
            tool_calls: vec![ToolCall {
                id: format!("call-{idx}"),
                name: "edit".into(),
                args: serde_json::json!({ "path": file, "content": "x" }),
            }],
            ..Default::default()
        };
        Box::pin(futures::stream::iter(vec![Ok(chunk)]))
    }
}

#[tokio::test]
async fn concurrent_executor_opens_multiple_scoped_streams() {
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let _a = ctx.provide(log.clone());

    // 构造多面任务，并把面分成 2 个写冲突组（前两个共享 shared.tsx，后两个共享 other.tsx）。
    let objective = "- 列表展示\n- 详情展示\n- 新增表单\n- 编辑弹窗";
    let contract = TaskContract::from_input(objective);
    let mut ge = GoalExecution::from_contract(&contract);
    let mut keys: Vec<String> = ge.items.keys().cloned().collect();
    keys.sort();
    assert!(keys.len() >= 4, "契约应产出 ≥4 个交付面，实际 {keys:?}");
    let mut surface_file: HashMap<String, String> = HashMap::new();
    for (i, id) in keys.iter().enumerate() {
        let file = if i < 2 { "shared.tsx" } else { "other.tsx" };
        let item = ge.items.get_mut(id).expect("面应存在");
        item.state = WorkItemState::ReadyToChange;
        item.candidate_targets = vec![file.to_string()];
        surface_file.insert(id.clone(), file.to_string());
    }

    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let llm: Arc<dyn LlmProvider> = Arc::new(MultiSurfaceLlm {
        requests: requests.clone(),
        calls: AtomicUsize::new(0),
        surface_file: Arc::new(surface_file),
    });
    let _b = ctx.provide(llm);
    let tools = ToolRegistry::new();
    tools.register(Arc::new(RecEdit {
        seen: seen.clone(),
    }));
    let _c = ctx.provide(tools);
    let hook: Arc<dyn Hook> = Arc::new(NullHook);
    let _d = ctx.provide(hook);

    let result = AgentLoop::new()
        .run_turn_with_goal_execution(
            &ctx,
            UserInput {
                text: objective.into(),
                attachments: vec![],
            },
            ge,
        )
        .await;
    assert!(result.is_ok(), "并发回合应正常结束：{result:?}");

    // 1) 并发分支确实执行：存在 ≥2 次携带 `本轮聚焦` 的模型调用（每个写冲突组一轮作用域流）。
    let focused_calls = requests
        .lock()
        .unwrap()
        .iter()
        .filter(|msgs| {
            msgs.iter()
                .any(|m| m.role == Role::System && m.content.contains("本轮聚焦"))
        })
        .count();
    assert!(
        focused_calls >= 2,
        "应至少对 2 个写冲突组各开一轮作用域流（并发分支），实际仅 {focused_calls} 次带 本轮聚焦"
    );

    // 2) 跨组并行 dispatch：edit 被派发到 ≥2 个不同文件。
    let edits = seen.lock().unwrap();
    let distinct_files: HashSet<&String> = edits.iter().collect();
    assert!(
        distinct_files.len() >= 2,
        "edit 应被并发派发到 ≥2 个不同文件，实际调用：{edits:?}"
    );
    assert!(
        edits.len() >= 2,
        "应发生 ≥2 次 edit 派发（跨组并发），实际：{edits:?}"
    );
}
