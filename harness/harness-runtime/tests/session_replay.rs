//! 回放回归套件（绞杀者步骤①）：真实会话 jsonl → 脚本化 LLM/工具 → 重放 AgentLoop，
//! 四条红线（spec §3）断言作用于重放产出的新日志。红线测试 #[ignore] 封存，
//! 新控制器（步骤④）接管后移除标记；旧守卫代码上它们必须跑红（断言有效性证明）。

use std::collections::HashMap;

use harness_llm::{Chunk, ToolResult, Usage};
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
    usages: Vec<Usage>,
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
                usages: vec![],
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
            // 真实 token 成本只记录在独立 Usage 事件（Assistant chunk 不带 usage）；
            // 采集进回合队列，重放时按请求序重新注入，R3 与判顶才有可测对象。
            SessionEvent::Usage { usage, .. } => {
                if let Some(t) = turns.last_mut() {
                    t.usages.push(usage);
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
    assert!(
        gitfix
            .iter()
            .any(|t| t.tool_names.contains(&"edit".to_string()))
    );
    assert_eq!(load_fixture("success_677bd6e0.jsonl").len(), 5);
}

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harness_capability::hook::{Hook, HookDecision, HookPayload};
use harness_core::AppContext;
use harness_core::error::Result;
use harness_core::types::UserInput;
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
/// `usages` 按请求序附加录制成本到 chunk（原始 Assistant chunk 不带 usage），
/// 使 agent_loop 写出 Usage 事件、重放 token 成本与原始会话对齐（R3 可测）。
struct ReplayLlm {
    queue: Mutex<VecDeque<Chunk>>,
    usages: Mutex<VecDeque<Usage>>,
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
        let mut chunk = self
            .queue
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Chunk {
                text: Some("[回放脚本已耗尽] 基于现有证据直接给出结论。".into()),
                ..Default::default()
            });
        if chunk.usage.is_none() {
            chunk.usage = self.usages.lock().unwrap().pop_front();
        }
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
const KNOWN_TOOLS: [&str; 7] = [
    "search", "edit", "fs", "shell", "plan", "memory", "delegate",
];

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
            usages: Mutex::new(turn.usages.into()),
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
    replay_session_with(fixture, GovernorMode::On).await
}

#[tokio::test]
async fn clarification_loop_replay_emits_delivery_per_turn() {
    let log =
        replay_session_with("7ba3370f_t15_18_clarification.jsonl", GovernorMode::Legacy).await;
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
    telemetry_text: String,
    prompt_tokens: u64,
    prompt_peak: u64,
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
            SessionEvent::Telemetry { telemetry, .. } => {
                if let Some(t) = out.last_mut() {
                    t.telemetry_text.push_str(&telemetry.detail);
                }
            }
            SessionEvent::Usage { usage, .. } => {
                if let Some(t) = out.last_mut() {
                    // 自动续跑会在同一用户请求内开启压缩后的新窗口。安全红线约束的是
                    // 单次请求/窗口，而不是把多个已压缩窗口重新累加成一个虚假峰值。
                    t.prompt_tokens = t.prompt_tokens.saturating_add(usage.prompt_tokens);
                    t.prompt_peak = t.prompt_peak.max(usage.prompt_tokens);
                }
            }
            _ => {}
        }
    }
    out
}

fn is_continuation(input: &str) -> bool {
    let t = input.trim();
    ["继续", "接着", "续", "恢复"]
        .iter()
        .any(|p| t.starts_with(p))
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
        let key: String = t
            .assistant_text
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
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

/// R3：单次执行回合 prompt tokens 峰值。硬顶单点取自
/// `harness_runtime::PROMPT_CAP`（控制器侧判顶与度量器必须同一常量）。
fn r3_prompt_peak(turns: &[TurnSummary]) -> u64 {
    turns.iter().map(|t| t.prompt_peak).max().unwrap_or(0)
}

fn prompt_total(turns: &[TurnSummary]) -> u64 {
    turns.iter().map(|t| t.prompt_tokens).sum()
}

/// 一旦历史累计超过单回合边界，后续回合仍须能请求模型；否则“继续”会被历史
/// Usage 永久拦在首请求之前。这里不依赖用户文案，只验证跨回合预算确实重置。
fn requested_after_historical_total_crossed_cap(turns: &[TurnSummary]) -> bool {
    let mut historical = 0u64;
    for turn in turns {
        if historical >= harness_runtime::PROMPT_CAP && turn.prompt_tokens > 0 {
            return true;
        }
        historical = historical.saturating_add(turn.prompt_tokens);
    }
    false
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

/// R4：执行失败回合必须在内部遥测保留结构化资产（至少一个精确锚点）。
/// 资产不再倾倒到助手正文；面向用户只显示可回答的简短选择。
/// NeedsUserInput 正是因为没有可靠锚点，不得为了测试而向用户伪造或泄露路径。
fn r4_violations(turns: &[TurnSummary]) -> Vec<String> {
    turns
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            matches!(
                t.outcome,
                Some(DeliveryOutcome::Interrupted | DeliveryOutcome::SystemFailure)
            )
        })
        .filter(|(_, t)| !has_path_anchor(&t.telemetry_text))
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

/// A2 意图分类（spec §3，2026-09-01 实机判读）：回读/编译验证类重复是健康的交付
/// 自查，不计跨轮重复；search 与其余 shell 属探索，保留计入。
fn is_exploratory(sig: &str) -> bool {
    if sig.starts_with("search:") || sig.starts_with("delegate:") {
        return true;
    }
    if sig.starts_with("fs:") {
        return sig.contains("\"op\":\"edit\"") || sig.contains("\"op\":\"write\"");
    }
    if sig.starts_with("shell:") {
        return !["check", "build", "compile", "test", "py_compile"]
            .iter()
            .any(|k| sig.contains(k));
    }
    if sig.starts_with("plan:") || sig.starts_with("memory:") {
        return false;
    }
    true // 未分类按探索计，宁严勿漏
}

/// A2（辅助）：同一探索型工具签名跨回合重复的最大回合数（回读/验证类豁免）。
fn a2_max_cross_turn_repeat(turns: &[TurnSummary]) -> usize {
    let mut by_sig: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, t) in turns.iter().enumerate() {
        let mut in_turn: std::collections::HashSet<&String> = std::collections::HashSet::new();
        for sig in t.signatures.iter().filter(|s| is_exploratory(s.as_str())) {
            if in_turn.insert(sig) {
                by_sig.entry(sig.clone()).or_default().push(i);
            }
        }
    }
    by_sig.values().map(|v| v.len()).max().unwrap_or(0)
}

/// R4 加强判据：非 Verified 回合的内部遥测必须含 `artifact_text` 四要素。
/// 助手正文必须保持用户可读，资产锁转移到不会展示的 Telemetry。
fn missing_artifact_violations(turns: &[TurnSummary]) -> Vec<String> {
    turns
        .iter()
        .enumerate()
        .filter(|(_, t)| t.outcome != Some(DeliveryOutcome::Verified))
        .filter(|(_, t)| {
            !(t.telemetry_text.contains("锚点：")
                && t.telemetry_text.contains("假设：")
                && t.telemetry_text.contains("补丁建议：")
                && t.telemetry_text.contains("问项："))
        })
        .map(|(i, t)| format!("turn {} outcome={:?} 缺四要素资产", i + 1, t.outcome))
        .collect()
}

/// 澄清死循环段（turn 15–18）：R1 + R2 + R4 + 资产锁。控制器模式正式门禁（步骤④已接管）。
#[tokio::test]
async fn red_lines_clarification_loop() {
    let log = replay_session("7ba3370f_t15_18_clarification.jsonl").await;
    let turns = summarize(&log);
    assert_eq!(turns.len(), 4);
    let (r1, r2, r4) = (
        r1_violations(&turns),
        r2_violations(&turns),
        r4_violations(&turns),
    );
    assert!(r1.is_empty(), "R1 违例: {r1:?}");
    assert!(r2.is_empty(), "R2 违例: {r2:?}");
    assert!(r4.is_empty(), "R4 违例: {r4:?}");
    let missing = missing_artifact_violations(&turns);
    assert!(missing.is_empty(), "R4 资产缺失: {missing:?}");
}

/// 症状任务段（turn 3–14）：R1 + R3 + R4 + A1 + 资产锁 + A2 的 A/B 对照。
#[tokio::test]
async fn red_lines_symptom_task() {
    let log = replay_session("7ba3370f_t03_14_symptom.jsonl").await;
    let turns = summarize(&log);
    assert_eq!(turns.len(), 12);
    let (r1, r4) = (r1_violations(&turns), r4_violations(&turns));
    let tokens = r3_prompt_peak(&turns);
    let a1 = a1_guard_trips(&turns);
    let a2_on = a2_max_cross_turn_repeat(&turns);
    assert!(r1.is_empty(), "R1 违例: {r1:?}");
    assert!(
        tokens <= harness_runtime::PROMPT_CAP,
        "R3 违例: 单回合 prompt 峰值={tokens} > 顶 {}",
        harness_runtime::PROMPT_CAP
    );
    assert!(
        requested_after_historical_total_crossed_cap(&turns),
        "历史累计触顶后，后续回合必须仍可请求模型（明确续跑不能被永久锁死）"
    );
    assert!(r4.is_empty(), "R4 违例: {r4:?}");
    assert!(a1 <= 12, "A1 违例: 守卫/熔断触发 {a1} > 12");
    let missing = missing_artifact_violations(&turns);
    assert!(missing.is_empty(), "R4 资产缺失: {missing:?}");

    // 同一次 Legacy 基线重放同时承担两项对照（避免第三次昂贵 replay）：
    //   ① R3 自证：Legacy 成本必须 > 顶，否则 replay 没复现真实 token 成本；
    //   ② A2 不退化：控制器模式跨轮重复不得高于旧守卫（绝对 A2 ≤ 2 属实机验收，
    //      回放里的模型是录制脚本，其跨轮重复是既成事实，控制器无法改变）。
    let legacy_log =
        replay_session_with("7ba3370f_t03_14_symptom.jsonl", GovernorMode::Legacy).await;
    let legacy = summarize(&legacy_log);
    let legacy_tokens = legacy.iter().map(|t| t.prompt_tokens).max().unwrap_or(0);
    assert!(
        legacy_tokens > harness_runtime::PROMPT_CAP,
        "R3 对照失效：Legacy 单回合成本 {legacy_tokens} 未超顶，replay 没复现真实成本"
    );
    let a2_legacy = a2_max_cross_turn_repeat(&legacy);
    assert!(
        a2_on <= a2_legacy,
        "A2 退化：控制器 {a2_on} > 旧守卫 {a2_legacy}"
    );
}

/// git 修复段（turn 19–22）：R4 + 资产锁（edit matched-0 / length 截断回合也要留资产）。
#[tokio::test]
async fn red_lines_gitfix() {
    let log = replay_session("7ba3370f_t19_22_gitfix.jsonl").await;
    let turns = summarize(&log);
    assert_eq!(turns.len(), 4);
    let r4 = r4_violations(&turns);
    assert!(r4.is_empty(), "R4 违例: {r4:?}");
    let missing = missing_artifact_violations(&turns);
    assert!(missing.is_empty(), "R4 资产缺失: {missing:?}");
}

/// A2 意图细化回归（阶段 3 T3）：回读/编译验证豁免，探索重复保留。
#[test]
fn a2_exempts_readback_and_verify_repeats() {
    let mk = |sigs: &[&str]| TurnSummary {
        signatures: sigs.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    };
    let verify = vec![
        mk(&[
            "fs:{\"op\":\"read\",\"path\":\"a.py\"}",
            "shell:{\"command\":\"python -m py_compile a.py\"}",
        ]),
        mk(&["fs:{\"op\":\"read\",\"path\":\"a.py\"}"]),
        mk(&["fs:{\"op\":\"read\",\"path\":\"a.py\"}"]),
    ];
    assert_eq!(
        a2_max_cross_turn_repeat(&verify),
        0,
        "纯读取/编译验证重复不计 A2"
    );
    let loops = vec![
        mk(&["shell:{\"command\":\"dir /s\"}"]),
        mk(&["shell:{\"command\":\"dir /s\"}"]),
        mk(&["shell:{\"command\":\"dir /s\"}"]),
    ];
    assert_eq!(
        a2_max_cross_turn_repeat(&loops),
        3,
        "探索型 shell 重复仍须计入"
    );
}

/// 成功会话回归：重放不得把健康会话跑坏（至少保留一个 Verified 交付）。
#[tokio::test]
async fn success_session_replay_keeps_verified() {
    let log = replay_session_with("success_677bd6e0.jsonl", GovernorMode::Legacy).await;
    let turns = summarize(&log);
    assert!(
        turns
            .iter()
            .any(|t| t.outcome == Some(DeliveryOutcome::Verified)),
        "成功会话重放后应仍有 Verified 交付: {turns:?}"
    );
}

/// A/B 冒烟：控制器模式下澄清死循环段被门禁拒绝后仍能每回合收尾（不挂死），
/// 且不出现门禁复读文案（R1/R2 前提）。outcome 收口与资产由 red_lines_* 断言。
#[tokio::test]
async fn governor_mode_terminates_clarification_loop_without_asking() {
    let log = replay_session_with("7ba3370f_t15_18_clarification.jsonl", GovernorMode::On).await;
    let turns = summarize(&log);
    assert_eq!(turns.len(), 4, "四个回合都要走完");
    assert!(
        r1_violations(&turns).is_empty(),
        "R1: {:?}",
        r1_violations(&turns)
    );
    assert!(
        r2_violations(&turns).is_empty(),
        "R2: {:?}",
        r2_violations(&turns)
    );
    assert!(
        !turns
            .iter()
            .any(|t| t.assistant_text.contains("[需要澄清]")),
        "控制器模式下门禁复读文案不得出现：{:?}",
        turns.iter().map(|t| &t.assistant_text).collect::<Vec<_>>()
    );
}

/// A/B 对照：控制器接管不得把健康会话跑坏。
#[tokio::test]
async fn governor_mode_success_session_still_verifies() {
    let log = replay_session_with("success_677bd6e0.jsonl", GovernorMode::On).await;
    let turns = summarize(&log);
    assert!(
        turns
            .iter()
            .any(|t| t.outcome == Some(DeliveryOutcome::Verified)),
        "成功会话在控制器模式下应仍有 Verified 交付: {turns:?}"
    );
    assert!(
        r3_prompt_peak(&turns) <= harness_runtime::PROMPT_CAP,
        "健康回合也不应突破成本顶: {}",
        r3_prompt_peak(&turns)
    );
    assert!(prompt_total(&turns) > 0, "健康会话必须实际产生 Usage");
}
