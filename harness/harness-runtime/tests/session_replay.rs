//! 回放回归套件（绞杀者步骤①）：真实会话 jsonl → 脚本化 LLM/工具 → 重放 AgentLoop，
//! 四条红线（spec §3）断言作用于重放产出的新日志。红线测试 #[ignore] 封存，
//! 新控制器（步骤④）接管后移除标记；旧守卫代码上它们必须跑红（断言有效性证明）。

use std::collections::HashMap;

use harness_llm::{Chunk, ToolResult};
use harness_session::SessionEvent;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/");

/// 一个回合的重放脚本：用户输入 + 按日志顺序的模型响应 + call_id→工具结果表。
///
/// 模型响应的识别规则：`Assistant` 事件中「带 usage 或带 tool_calls」的 chunk 才是
/// 真实模型响应；门禁/恢复逻辑合成的纯文本助手事件（如「[需要澄清] …」）不进脚本，
/// 由 Runtime 在重放中自行再生成——这正是我们要断言的对象。
#[derive(Debug)]
struct ReplayedTurn {
    input: String,
    responses: Vec<Chunk>,
    tool_results: HashMap<String, ToolResult>,
    tool_names: Vec<String>,
}

fn load_fixture(name: &str) -> Vec<ReplayedTurn> {
    let raw = std::fs::read_to_string(format!("{FIXTURES}{name}"))
        .unwrap_or_else(|e| panic!("fixture {name} 读取失败: {e}"));
    let mut turns: Vec<ReplayedTurn> = vec![];
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let ev: SessionEvent = serde_json::from_str(line).expect("fixture 事件解析失败");
        match ev {
            SessionEvent::TurnStart { input, .. } => turns.push(ReplayedTurn {
                input,
                responses: vec![],
                tool_results: HashMap::new(),
                tool_names: vec![],
            }),
            // 不变量：fixture 恒以 TurnStart 开头；首个 TurnStart 之前的事件被有意丢弃。
            SessionEvent::Assistant { chunk, .. } => {
                if chunk.usage.is_some() || !chunk.tool_calls.is_empty() {
                    if let Some(t) = turns.last_mut() {
                        t.responses.push(chunk);
                    }
                }
            }
            SessionEvent::ToolCall { call, .. } => {
                if let Some(t) = turns.last_mut() {
                    if !t.tool_names.contains(&call.name) {
                        t.tool_names.push(call.name);
                    }
                }
            }
            SessionEvent::ToolResult { result, .. } => {
                if let Some(t) = turns.last_mut() {
                    t.tool_results.insert(result.call_id.clone(), result);
                }
            }
            _ => {}
        }
    }
    turns
}

#[test]
fn fixtures_parse_with_expected_turn_counts() {
    let symptom = load_fixture("7ba3370f_t03_14_symptom.jsonl");
    assert_eq!(symptom.len(), 12);
    let clarification = load_fixture("7ba3370f_t15_18_clarification.jsonl");
    assert_eq!(clarification.len(), 4);
    assert_eq!(clarification[0].input, "这个问题解决了吗？");
    // turn 15–18 是门禁复读，无工具调用、无真实模型响应
    assert!(clarification.iter().all(|t| t.tool_names.is_empty()));
    let gitfix = load_fixture("7ba3370f_t19_22_gitfix.jsonl");
    assert_eq!(gitfix.len(), 4);
    assert!(gitfix.iter().any(|t| t.tool_names.contains(&"edit".to_string())));
    assert_eq!(load_fixture("success_677bd6e0.jsonl").len(), 5);
}

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harness_capability::hook::{Hook, HookDecision, HookPayload};
use harness_core::error::Result;
use harness_core::types::UserInput;
use harness_core::AppContext;
use harness_llm::{ChunkStream, LlmProvider, Message, ToolCall, ToolSchema};
use harness_runtime::{AgentLoop, GovernorMode};
use harness_session::SessionLog;
use harness_tool::{DynTool, ToolRegistry};

struct AllowHook;
impl Hook for AllowHook {
    fn run(&self, _: &HookPayload) -> Result<HookDecision> {
        Ok(HookDecision::Allow)
    }
}

/// 按日志顺序逐条吐出录制的模型响应；脚本耗尽时返回收敛文本，
/// 使发散中的旧守卫循环能以某个 Delivery 收尾而不是挂死。
struct ReplayLlm {
    queue: Mutex<VecDeque<Chunk>>,
}

#[async_trait]
impl LlmProvider for ReplayLlm {
    fn name(&self) -> &'static str {
        "session-replay"
    }
    fn tools(&self) -> Vec<ToolSchema> {
        vec![]
    }
    fn stream(&self, _msgs: Vec<Message>) -> ChunkStream {
        let chunk = self.queue.lock().unwrap().pop_front().unwrap_or_else(|| Chunk {
            text: Some("[回放脚本已耗尽] 基于现有证据直接给出结论。".into()),
            ..Default::default()
        });
        Box::pin(futures::stream::iter(vec![Ok(chunk)]))
    }
}

/// 查表工具：按 call_id 返回日志录制的结果；未录制的调用返回失败结果。
struct ReplayTool {
    tool_name: &'static str,
    results: Arc<Mutex<HashMap<String, ToolResult>>>,
}

#[async_trait]
impl DynTool for ReplayTool {
    fn name(&self) -> &'static str {
        self.tool_name
    }
    async fn call(&self, call: &ToolCall) -> Result<ToolResult> {
        Ok(self
            .results
            .lock()
            .unwrap()
            .get(&call.id)
            .cloned()
            .unwrap_or(ToolResult {
                call_id: call.id.clone(),
                ok: false,
                content: format!("[replay] 未录制的工具调用: {}", call.name),
                continuation_debt: 0,
            }))
    }
}

/// fixture 中出现过的工具必须在此登记（DynTool::name 需要 'static）。
const KNOWN_TOOLS: [&str; 7] = ["search", "edit", "fs", "shell", "plan", "memory", "delegate"];

/// 重放一个会话：逐回合新建 AppContext，共享同一个内存 SessionLog（历史跨回合累积）。
async fn replay_session_with(fixture: &str, mode: GovernorMode) -> Arc<SessionLog> {
    let turns = load_fixture(fixture);
    let log = SessionLog::new();
    let mut all_results: HashMap<String, ToolResult> = HashMap::new();
    let mut needed: Vec<String> = vec![];
    for t in &turns {
        all_results.extend(t.tool_results.clone());
        for n in &t.tool_names {
            if !needed.contains(n) {
                needed.push(n.clone());
            }
        }
    }
    debug_assert_eq!(
        all_results.len(),
        turns.iter().map(|t| t.tool_results.len()).sum::<usize>(),
        "call_id 跨回合冲突：共享结果表会静默覆盖先前的录制结果"
    );
    for name in &needed {
        assert!(
            KNOWN_TOOLS.contains(&name.as_str()),
            "fixture 出现未登记的工具: {name}（在 KNOWN_TOOLS 中补充）"
        );
    }
    let results = Arc::new(Mutex::new(all_results));
    for turn in turns {
        let ctx = AppContext::new();
        let llm: Arc<dyn LlmProvider> = Arc::new(ReplayLlm {
            queue: Mutex::new(turn.responses.into()),
        });
        let tools = ToolRegistry::new();
        for name in KNOWN_TOOLS {
            if needed.iter().any(|n| n == name) {
                tools.register(Arc::new(ReplayTool {
                    tool_name: name,
                    results: results.clone(),
                }));
            }
        }
        let hook: Arc<dyn Hook> = Arc::new(AllowHook);
        let mut regs = vec![];
        regs.push(ctx.provide(log.clone()));
        regs.push(ctx.provide(llm));
        regs.push(ctx.provide(tools));
        regs.push(ctx.provide(hook));
        let _ = AgentLoop::new()
            .with_governor(mode)
            .run_turn(
                &ctx,
                UserInput {
                    text: turn.input.clone(),
                    attachments: vec![],
                },
            )
            .await;
        drop(regs);
    }
    log
}

async fn replay_session(fixture: &str) -> Arc<SessionLog> {
    replay_session_with(fixture, GovernorMode::Legacy).await
}

#[tokio::test]
async fn clarification_loop_replay_emits_delivery_per_turn() {
    let log = replay_session("7ba3370f_t15_18_clarification.jsonl").await;
    let events = log.replay();
    let turn_starts = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::TurnStart { .. }))
        .count();
    let deliveries = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::Delivery { .. }))
        .count();
    assert_eq!(turn_starts, 4, "重放应完整执行 4 个回合");
    assert_eq!(deliveries, 4, "每个回合必须以 Delivery 收尾");
    // 逐回合配对：每个 TurnStart 之后、下一个 TurnStart 之前必须恰好出现一个 Delivery。
    let mut open = false;
    for e in &events {
        match e {
            SessionEvent::TurnStart { .. } => {
                assert!(!open, "存在未以 Delivery 收尾就开始的回合");
                open = true;
            }
            SessionEvent::Delivery { .. } => {
                assert!(open, "出现无所属回合的 Delivery");
                open = false;
            }
            _ => {}
        }
    }
    assert!(!open, "最后一个回合未以 Delivery 收尾");
}

// ─── Red-line meters & gate tests (Task 4) ──────────────────────────────────

use harness_session::DeliveryOutcome;

#[derive(Debug, Default)]
struct TurnSummary {
    input: String,
    outcome: Option<DeliveryOutcome>,
    assistant_text: String,
    prompt_tokens: u64,
    signatures: Vec<String>,
}

/// 把重放日志折叠为逐回合摘要：Delivery 结局、助手全文、prompt token、工具签名。
/// 不变量：重放日志恒以 TurnStart 开头；此前的事件被有意丢弃（同 load_fixture）。
fn summarize(log: &SessionLog) -> Vec<TurnSummary> {
    let mut out: Vec<TurnSummary> = vec![];
    for ev in log.replay() {
        match ev {
            SessionEvent::TurnStart { input, .. } => out.push(TurnSummary {
                input,
                ..Default::default()
            }),
            SessionEvent::Assistant { chunk, .. } => {
                if let Some(t) = out.last_mut() {
                    if let Some(text) = chunk.text {
                        t.assistant_text.push_str(&text);
                    }
                }
            }
            SessionEvent::ToolCall { call, .. } => {
                if let Some(t) = out.last_mut() {
                    t.signatures.push(format!("{}:{}", call.name, call.args));
                }
            }
            SessionEvent::Delivery { report, .. } => {
                if let Some(t) = out.last_mut() {
                    // 同回合多次 Delivery 取最后一条（运行时最终态）；重复本身即 Runtime 缺陷。
                    t.outcome = Some(report.outcome);
                }
            }
            SessionEvent::Usage { usage, .. } => {
                if let Some(t) = out.last_mut() {
                    t.prompt_tokens += usage.prompt_tokens;
                }
            }
            _ => {}
        }
    }
    out
}

fn is_continuation(input: &str) -> bool {
    let t = input.trim();
    ["继续", "接着", "续", "恢复"].iter().any(|p| t.starts_with(p))
        || t.to_ascii_lowercase().starts_with("continue")
        || t.to_ascii_lowercase().starts_with("resume")
}

/// R1：续跑式回复不得以 NeedsUserInput 结束（用户永不说"继续"）。
fn r1_violations(turns: &[TurnSummary]) -> Vec<String> {
    turns
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            is_continuation(&t.input) && t.outcome == Some(DeliveryOutcome::NeedsUserInput)
        })
        .map(|(i, t)| format!("turn {} input={:?}", i + 1, t.input))
        .collect()
}

/// R2：同一澄清文案会话内不得出现第二次。
/// 前提：澄清模板文案不得内嵌逐回合变化的令牌（时间戳/计数器等），否则本度量会失效；
/// 该约束应在文案生成端强制。
fn r2_violations(turns: &[TurnSummary]) -> Vec<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut dups = vec![];
    for (i, t) in turns.iter().enumerate() {
        if t.outcome != Some(DeliveryOutcome::NeedsUserInput) {
            continue;
        }
        let key: String = t.assistant_text.chars().filter(|c| !c.is_whitespace()).collect();
        if key.is_empty() {
            continue;
        }
        match seen.get(&key) {
            Some(first) => dups.push(format!("turn {} 与 turn {} 澄清文案完全相同", first, i + 1)),
            None => {
                seen.insert(key, i + 1);
            }
        }
    }
    dups
}

/// R3：会话 prompt tokens 硬顶。
const PROMPT_CAP: u64 = 300_000;

fn r3_prompt_total(turns: &[TurnSummary]) -> u64 {
    turns.iter().map(|t| t.prompt_tokens).sum()
}

/// R4 资产锚点：助手文本中出现「含路径分隔符且带源码扩展名」的 token 即视为
/// 携带精确锚点（失败回合最低限度的结构化资产证据）。
/// 有意宽松：对行文中顺带提及路径的误报可接受——这是最低证据底线而非充分条件。
fn has_path_anchor(text: &str) -> bool {
    text.split_whitespace().any(|tok| {
        (tok.contains('/') || tok.contains('\\'))
            && [".rs", ".toml", ".md", ".json", ".py", ".ts", ".slint"]
                .iter()
                .any(|ext| tok.contains(ext))
    })
}

/// R4：失败/求助回合必须留结构化资产（至少一个精确锚点）。
fn r4_violations(turns: &[TurnSummary]) -> Vec<String> {
    turns
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            matches!(
                t.outcome,
                Some(
                    DeliveryOutcome::Interrupted
                        | DeliveryOutcome::SystemFailure
                        | DeliveryOutcome::NeedsUserInput
                )
            )
        })
        .filter(|(_, t)| !has_path_anchor(&t.assistant_text))
        .map(|(i, t)| format!("turn {} outcome={:?} 无锚点资产", i + 1, t.outcome))
        .collect()
}

/// A1（辅助）：守卫/熔断触发次数 = 断路型 Delivery（Interrupted/SystemFailure）
/// + 澄清门禁文案（「[需要澄清]」）出现次数。
fn a1_guard_trips(turns: &[TurnSummary]) -> usize {
    let breaker = turns
        .iter()
        .filter(|t| {
            matches!(
                t.outcome,
                Some(DeliveryOutcome::Interrupted | DeliveryOutcome::SystemFailure)
            )
        })
        .count();
    let gate_msgs = turns
        .iter()
        .filter(|t| t.assistant_text.contains("[需要澄清]"))
        .count();
    breaker + gate_msgs
}

/// A2（辅助）：同一工具签名跨回合重复的最大回合数。
fn a2_max_cross_turn_repeat(turns: &[TurnSummary]) -> usize {
    let mut by_sig: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, t) in turns.iter().enumerate() {
        let mut in_turn: std::collections::HashSet<&String> = std::collections::HashSet::new();
        for sig in &t.signatures {
            if in_turn.insert(sig) {
                by_sig.entry(sig.clone()).or_default().push(i);
            }
        }
    }
    by_sig.values().map(|v| v.len()).max().unwrap_or(0)
}

/// 澄清死循环段（turn 15–18）：R1（"继续"不得 NeedsUserInput）+ R2（文案不复读）+ R4（留资产）。
#[tokio::test]
#[ignore = "红线门禁：旧守卫代码上预期失败（跑红证明）；新控制器接管（步骤④）后移除"]
async fn red_lines_clarification_loop() {
    let log = replay_session("7ba3370f_t15_18_clarification.jsonl").await;
    let turns = summarize(&log);
    assert_eq!(turns.len(), 4);
    let (r1, r2, r4) = (r1_violations(&turns), r2_violations(&turns), r4_violations(&turns));
    assert!(r1.is_empty(), "R1 违例: {r1:?}");
    assert!(r2.is_empty(), "R2 违例: {r2:?}");
    assert!(r4.is_empty(), "R4 违例: {r4:?}");
}

/// 症状任务段（turn 3–14）：R1 + R3（300k token 顶）+ R4 + A1/A2 辅助。
#[tokio::test]
#[ignore = "红线门禁：旧守卫代码上预期失败（跑红证明）；新控制器接管（步骤④）后移除"]
async fn red_lines_symptom_task() {
    let log = replay_session("7ba3370f_t03_14_symptom.jsonl").await;
    let turns = summarize(&log);
    assert_eq!(turns.len(), 12);
    let (r1, r4) = (r1_violations(&turns), r4_violations(&turns));
    let tokens = r3_prompt_total(&turns);
    let (a1, a2) = (a1_guard_trips(&turns), a2_max_cross_turn_repeat(&turns));
    assert!(r1.is_empty(), "R1 违例: {r1:?}");
    assert!(tokens <= PROMPT_CAP, "R3 违例: prompt={tokens} > {PROMPT_CAP}");
    assert!(r4.is_empty(), "R4 违例: {r4:?}");
    assert!(a1 <= 12, "A1 违例: 守卫/熔断触发 {a1} > 12");
    assert!(a2 <= 2, "A2 违例: 跨轮重复 {a2} > 2");
}

/// git 修复段（turn 19–22）：R4（edit matched-0 / length 截断回合也要留资产）。
#[tokio::test]
#[ignore = "红线门禁：旧守卫代码上预期失败（跑红证明）；新控制器接管（步骤④）后移除"]
async fn red_lines_gitfix() {
    let log = replay_session("7ba3370f_t19_22_gitfix.jsonl").await;
    let turns = summarize(&log);
    assert_eq!(turns.len(), 4);
    let r4 = r4_violations(&turns);
    assert!(r4.is_empty(), "R4 违例: {r4:?}");
}

/// 成功会话回归：重放不得把健康会话跑坏（至少保留一个 Verified 交付）。
#[tokio::test]
async fn success_session_replay_keeps_verified() {
    let log = replay_session("success_677bd6e0.jsonl").await;
    let turns = summarize(&log);
    assert!(
        turns
            .iter()
            .any(|t| t.outcome == Some(DeliveryOutcome::Verified)),
        "成功会话重放后应仍有 Verified 交付: {turns:?}"
    );
}

/// A/B 冒烟：控制器模式下澄清死循环段被门禁拒绝后仍能每回合收尾（不挂死），
/// 且不出现门禁复读文案（R1/R2 前提）。
/// 断言边界：旧 outcome 链的 SystemFailure/Interrupted 收敛为 ExhaustedWithArtifact
/// 由 T9 的 `governor_mode_*` 测试负责，此处不预先要求。
#[tokio::test]
async fn governor_mode_terminates_clarification_loop_without_asking() {
    let log =
        replay_session_with("7ba3370f_t15_18_clarification.jsonl", GovernorMode::On).await;
    let turns = summarize(&log);
    assert_eq!(turns.len(), 4, "四个回合都要走完");
    assert!(r1_violations(&turns).is_empty(), "R1: {:?}", r1_violations(&turns));
    assert!(r2_violations(&turns).is_empty(), "R2: {:?}", r2_violations(&turns));
    assert!(
        !turns.iter().any(|t| t.assistant_text.contains("[需要澄清]")),
        "控制器模式下门禁复读文案不得出现：{:?}",
        turns.iter().map(|t| &t.assistant_text).collect::<Vec<_>>()
    );
}
