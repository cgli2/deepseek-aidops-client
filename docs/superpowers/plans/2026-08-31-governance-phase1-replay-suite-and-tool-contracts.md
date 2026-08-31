# Agent 治理重设计 · 阶段 1：回放回归套件 + 工具层契约（绞杀者步骤 ①③）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 先于任何治理改动落地回放回归套件（四条红线断言在旧代码上跑红，证明断言有效），并独立上线工具层三契约（edit 失配自读磁盘、search 作用域自动升级、git 子进程 CREATE_NO_WINDOW），构造性消除会话 7ba3370f 的 turn 19/turn 3–14 两类失败。

**Architecture:** 回放驱动器把历史会话 jsonl 解析为逐回合的「用户输入 + 模型响应脚本 + 工具结果表」，用脚本化 `LlmProvider` 与查表式 `DynTool` 驱动真实 `AgentLoop`，红线断言作用于重放产出的新 `SessionLog`。工具契约改动全部落在工具/Provider 层，不触碰控制器，与回放套件互不依赖、可并行。

**Tech Stack:** Rust（cargo workspace 位于 `harness/`，MSVC 工具链）、tokio、async-trait、serde_json；fixture 为 `.harness/sessions/` 下的真实会话 jsonl（SessionEvent 逐行序列化）。

**Spec:** `docs/superpowers/specs/2026-08-31-agent-governance-redesign-design.md` §3（红线）、§4.6（工具契约）、§5 步骤①③。

**范围边界：** 本计划只覆盖绞杀者五步的 ① 与 ③。步骤 ②（case file write-only 对拍）、④（新控制器 A/B）、⑤（旧守卫退位）属后续计划；本计划的红线测试以 `#[ignore]` 封存，步骤 ④ 接管后移除标记。

**环境约定：**
- 本工作区**不是 git 仓库**（无 `.git`）。所有「commit」步骤替换为 **Checkpoint**：跑指定测试命令并确认全绿。
- 一切 `cargo` 命令在 `F:\workspace\deepseek-aidops-stable\harness` 目录下执行。
- 测试命令模板：`cargo test -p <crate> [--test <name>] [-- --ignored]`。
- 已知坐标（探查于 2026-08-31，实施时如漂移以代码为准）：
  - `SessionEvent`/`DeliveryOutcome`/`SessionLog`：`harness-session/src/log.rs`（`DeliveryOutcome` 变体：Verified / NeedsUserInput / PartialDelivery / SystemFailure / Blocked / Interrupted / Cancelled）。
  - `AgentLoop::new().run_turn(&ctx, UserInput { text, attachments })`：入口；测试装配范式照抄 `harness-runtime/tests/agent_tool_loop.rs:258-300`（`ctx.provide(log)`、`Arc<dyn LlmProvider>`、`ToolRegistry`、`Arc<dyn Hook>`）。
  - `LlmProvider`：`harness-llm/src/lib.rs:260`，`stream` 返回 `ChunkStream = Pin<Box<dyn Stream<Item = Result<Chunk>> + Send>>`。
  - `Chunk { text, tool_calls, reasoning, usage, empty_response, finish_reason }`：`harness-llm/src/lib.rs:170`。
  - `DynTool` / `ToolRegistry`：`harness-tool/src/registry.rs:10`。
  - 会话日志目录：`<workspace>/.harness/sessions/*.jsonl`。

---

### Task 1: 制作回放 fixture（真实会话分段）

**Files:**
- Create: `harness/harness-runtime/tests/fixtures/7ba3370f_t03_14_symptom.jsonl`
- Create: `harness/harness-runtime/tests/fixtures/7ba3370f_t15_18_clarification.jsonl`
- Create: `harness/harness-runtime/tests/fixtures/7ba3370f_t19_22_gitfix.jsonl`
- Create: `harness/harness-runtime/tests/fixtures/7ba3370f_full.jsonl`
- Create: `harness/harness-runtime/tests/fixtures/success_677bd6e0.jsonl`
- Create（临时，跑完删除）: `_fixture_extract.py`

分段依据（来自 7ba3370f 复盘）：turn 3–14 = 症状任务守卫连环熔断（R1/R3/A2 证据）；turn 15–18 = 澄清模板四次复读（R2 证据，零工具调用、回放最快）；turn 19–22 = git 报错修复 + edit matched-0 + length 截断（R4 证据）。成功会话 = `677bd6e0`（5 回合、131k prompt、含 Verified 交付）。

- [ ] **Step 1: 写抽取脚本**

在工作区根写 `_fixture_extract.py`：

```python
import json, shutil
from pathlib import Path

ROOT = Path(__file__).resolve().parent
SRC = ROOT / ".harness/sessions/7ba3370f-fcbe-4993-b50a-f89f750ba929.jsonl"
OUT = ROOT / "harness/harness-runtime/tests/fixtures"
OUT.mkdir(parents=True, exist_ok=True)

lines = SRC.read_text(encoding="utf-8").splitlines()
bounds = [i for i, l in enumerate(lines) if l.startswith('{"TurnStart"')]
bounds.append(len(lines))
print("total turns:", len(bounds) - 1)
assert len(bounds) - 1 == 22, "7ba3370f 应为 22 回合，若不符先人工核对日志"

def dump(name, turn_lo, turn_hi):  # 1-based，闭区间
    seg = lines[bounds[turn_lo - 1]:bounds[turn_hi]]
    (OUT / name).write_text("\n".join(seg) + "\n", encoding="utf-8")
    print(name, len(seg), "events,", (OUT / name).stat().st_size, "bytes")

dump("7ba3370f_t03_14_symptom.jsonl", 3, 14)
dump("7ba3370f_t15_18_clarification.jsonl", 15, 18)
dump("7ba3370f_t19_22_gitfix.jsonl", 19, 22)
shutil.copy(SRC, OUT / "7ba3370f_full.jsonl")
shutil.copy(ROOT / ".harness/sessions/677bd6e0-bcd6-4b7f-bc6f-40fe3834fe54.jsonl",
            OUT / "success_677bd6e0.jsonl")
```

- [ ] **Step 2: 运行脚本**

Run: `python -X utf8 _fixture_extract.py`（工作区根目录）
Expected: `total turns: 22`，5 个文件生成；`..._t15_18_clarification.jsonl` 为四段中最小（应 < 100KB）。

- [ ] **Step 3: 校验 fixture 可解析且回合数正确**

Run:
```bash
python -X utf8 -c "import json,pathlib; [print(f.name, sum(1 for l in f.read_text(encoding='utf-8').splitlines() if l.startswith('{\"TurnStart\"}'))) for f in sorted(pathlib.Path('harness/harness-runtime/tests/fixtures').glob('*.jsonl'))]"
```
Expected: symptom=12，clarification=4，gitfix=4，full=22，success=5。

- [ ] **Step 4: 删除临时脚本，做 Checkpoint**

Run: `rm _fixture_extract.py`
Checkpoint: 5 个 fixture 文件存在于 `harness/harness-runtime/tests/fixtures/`。

---

### Task 2: 回放驱动器骨架——fixture 解析

**Files:**
- Create: `harness/harness-runtime/tests/session_replay.rs`

- [ ] **Step 1: 写解析器与失败测试**

创建 `harness/harness-runtime/tests/session_replay.rs`：

```rust
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
```

- [ ] **Step 2: 运行，确认编译通过且测试绿**

Run: `cargo test -p harness-runtime --test session_replay`
Expected: PASS（1 test）。若报缺 `serde_json`：它已是 `harness-runtime` 常规依赖（见其 Cargo.toml `[dependencies]`），不需要新增。

- [ ] **Step 3: Checkpoint**

Checkpoint: `cargo test -p harness-runtime --test session_replay` 全绿。

---

### Task 3: 脚本化 LLM 与查表工具——重放执行器

**Files:**
- Modify: `harness/harness-runtime/tests/session_replay.rs`

- [ ] **Step 1: 追加重放执行器与冒烟测试**

在 `session_replay.rs` 追加：

```rust
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harness_capability::hook::{Hook, HookDecision, HookPayload};
use harness_core::error::Result;
use harness_core::types::UserInput;
use harness_core::AppContext;
use harness_llm::{ChunkStream, LlmProvider, Message, ToolCall, ToolSchema};
use harness_runtime::AgentLoop;
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
async fn replay_session(fixture: &str) -> SessionLog {
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
}
```

- [ ] **Step 2: 运行，确认编译通过**

Run: `cargo test -p harness-runtime --test session_replay`
Expected: 2 tests PASS。

- [ ] **Step 3: 校准（若冒烟测试失败）**

若 `deliveries != 4`：在测试里临时打印 `log.replay()` 的事件类型序列，对照 `agent_loop.rs` 的终止路径（门禁澄清出口 :489/:538/:583、主出口 :1691）定位缺 Delivery 的回合。常见原因：
- 缺服务 panic（如 WorkspaceIndex 构建）→ 在 `replay_session` 中补注册最小 stub；
- 某回合以 `Err` 返回 → 把 `let _ =` 改为 `.expect("…")` 暂时暴露错误，修好后还原。
校准后本步必须回到全绿。

- [ ] **Step 4: Checkpoint**

Checkpoint: `cargo test -p harness-runtime --test session_replay` 全绿（2 tests）。

---

### Task 4: 红线度量器与红线门禁测试（旧代码上跑红）

**Files:**
- Modify: `harness/harness-runtime/tests/session_replay.rs`

- [ ] **Step 1: 追加回合摘要与红线度量器**

```rust
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
```

- [ ] **Step 2: 追加红线门禁测试（#[ignore] 封存）**

```rust
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
```

- [ ] **Step 3: 跑红证明——红线测试必须在旧代码上失败**

Run: `cargo test -p harness-runtime --test session_replay -- --ignored --test-threads=1`
Expected: 3 个红线测试 **全部失败**（典型失败：`red_lines_clarification_loop` 报 R2「澄清文案完全相同」；`red_lines_symptom_task` 报 R3 token 超顶）。把失败输出粘贴进实施记录——这是断言有效性的证据（spec §5 步骤①）。若有红线测试意外通过：说明该度量器对旧失败模式不敏感，**先修度量器**（对照 `_analysis_out.txt` 式的逐回合分析找到未捕获的违例形态），再重跑直到跑红。

- [ ] **Step 4: 默认套件保持全绿**

Run: `cargo test -p harness-runtime --test session_replay`
Expected: 3 tests PASS（`--ignored` 的 3 个被跳过）。

- [ ] **Step 5: Checkpoint**

Checkpoint: 默认全绿 + `-- --ignored` 三个红线测试跑红，两者证据都留存。

---

### Task 5: 工具契约一——edit 失配自读磁盘（read-modify-write 原子化）

**Files:**
- Modify: `harness/harness-provider-local/src/editor.rs`

证据：7ba3370f turn 19 连续 3 次 `old_text must match exactly once (matched 0)` 后守卫中断。契约（spec §4.6）：失配时工具自读磁盘，把候选区域的**磁盘原文 + 行号**回给模型，禁止凭记忆重构。

- [ ] **Step 1: 写失败测试**

在 `editor.rs` 底部 `mod tests` 内追加：

```rust
    #[tokio::test]
    async fn edit_zero_match_returns_disk_region_with_line_numbers() {
        let root = std::env::temp_dir().join(format!("harness-editor-mismatch-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("target.rs");
        // 磁盘现状是 v2；模型凭记忆发来 v1 的 old_text
        std::fs::write(&path, "fn main() {\n    println!(\"v2\");\n}\n").unwrap();
        let editor = LocalEditor::new(root.clone());
        let err = editor
            .apply(
                std::path::Path::new("target.rs"),
                r#"{"old_text":"fn main() {\n    println!(\"v1\");","new_text":"x"}"#,
            )
            .await
            .expect_err("matched 0 必须报错");
        let msg = format!("{err}");
        assert!(msg.contains("matched 0"), "{msg}");
        assert!(msg.contains("println!(\"v2\")"), "报告必须回读磁盘现状: {msg}");
        assert!(msg.contains("2|"), "报告必须带行号: {msg}");
        // 文件未被修改
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "fn main() {\n    println!(\"v2\");\n}\n"
        );
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir(root);
    }

    #[tokio::test]
    async fn edit_ambiguous_match_lists_hit_line_numbers() {
        let root = std::env::temp_dir().join(format!("harness-editor-amb-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("dup.txt");
        std::fs::write(&path, "dup\nmiddle\ndup\n").unwrap();
        let editor = LocalEditor::new(root.clone());
        let err = editor
            .apply(
                std::path::Path::new("dup.txt"),
                r#"{"old_text":"dup","new_text":"y"}"#,
            )
            .await
            .expect_err("matched 2 必须报错");
        let msg = format!("{err}");
        assert!(msg.contains("matched 2"), "{msg}");
        assert!(msg.contains("1") && msg.contains("3"), "报告必须给出命中行号: {msg}");
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir(root);
    }

    #[tokio::test]
    async fn edit_zero_match_without_anchor_suggests_reread() {
        let root = std::env::temp_dir().join(format!("harness-editor-noanchor-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("a.txt");
        std::fs::write(&path, "alpha\nbeta\n").unwrap();
        let editor = LocalEditor::new(root.clone());
        let err = editor
            .apply(
                std::path::Path::new("a.txt"),
                r#"{"old_text":"zzz qqq","new_text":"y"}"#,
            )
            .await
            .expect_err("matched 0 必须报错");
        let msg = format!("{err}");
        assert!(msg.contains("无任何锚点"), "{msg}");
        assert!(msg.contains("read"), "必须引导模型先重新读取文件: {msg}");
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir(root);
    }
```

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test -p harness-provider-local editor`
Expected: 3 个新测试 FAIL（现有错误消息不含区域报告），旧测试 `edit_replaces_exactly_once_and_rejects_ambiguous_matches` 仍 PASS。

- [ ] **Step 3: 实现失配报告**

`editor.rs`：在 `impl Editor for LocalEditor` 外新增函数，并替换 `apply` 中 `count != 1` 分支。

新增函数：

```rust
/// old_text 失配（0 次或多次）时的自愈报告：工具自读磁盘，给出候选区域的
/// 精确原文与行号，让模型以磁盘事实重发，禁止凭记忆重构（spec §4.6）。
fn mismatch_report(content: &str, old: &str, count: usize) -> String {
    if count > 1 {
        let lines: Vec<usize> = content
            .match_indices(old)
            .map(|(off, _)| content[..off].matches('\n').count() + 1)
            .collect();
        return format!(
            "old_text must match exactly once (matched {count}). 命中行号: {}。请扩大 old_text 的上下文使其唯一后重发。",
            lines
                .iter()
                .map(|l| l.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    // count == 0：文件已变化。用 old_text 的首个非空行作锚点，回读候选区域。
    let old_lines: Vec<&str> = old.lines().collect();
    let anchor = old_lines
        .iter()
        .find(|l| !l.trim().is_empty())
        .copied()
        .unwrap_or("");
    let file_lines: Vec<&str> = content.lines().collect();
    if !anchor.trim().is_empty() {
        if let Some(idx) = file_lines.iter().position(|l| l.contains(anchor.trim())) {
            let start = idx.saturating_sub(3);
            let end = (idx + old_lines.len() + 3).min(file_lines.len());
            let region: String = file_lines[start..end]
                .iter()
                .enumerate()
                .map(|(i, l)| format!("{}|{}\n", start + i + 1, l))
                .collect();
            return format!(
                "old_text must match exactly once (matched 0). 文件已变化；以下是磁盘当前候选区域（行号|内容），请用磁盘原文重发，禁止凭记忆重构：\n{region}"
            );
        }
    }
    format!(
        "old_text must match exactly once (matched 0). old_text 在文件中无任何锚点（文件共 {} 行）。请先用 fs 工具 read 该文件获取最新内容，再基于磁盘原文重发。",
        file_lines.len()
    )
}
```

替换 `apply` 中原分支（`harness-provider-local/src/editor.rs:62-68`）：

```rust
        let count = content.matches(old).count();
        if count != 1 {
            return Err(harness_core::error::Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                mismatch_report(&content, old, count),
            )));
        }
```

- [ ] **Step 4: 运行，确认全绿**

Run: `cargo test -p harness-provider-local`
Expected: 全部 PASS（3 新 + 1 旧）。

- [ ] **Step 5: Checkpoint**

Checkpoint: `cargo test -p harness-provider-local` 全绿。

---

### Task 6: 工具契约二——search 作用域自动升级（dir → crate → workspace）

**Files:**
- Modify: `harness/harness-tool/src/search.rs`

证据：7ba3370f turn 5/8/11 模型在 `dir=harness/harness-capability/src` 空手而回后被门禁掐断，而不是自动扩大范围。契约（spec §4.6）：升级在工具内部完成，空结果附「已试范围列表」。

- [ ] **Step 1: 写失败测试**

在 `search.rs` 底部新增：

```rust
#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use harness_capability::search::{Search, SearchHit, SearchRequest};
    use harness_core::error::Result;
    use harness_llm::{ToolCall, ToolResult};

    use super::SearchTool;
    use crate::registry::DynTool;

    /// 前 `empty_calls` 次 grep 返回空，之后返回单条命中；记录每次请求的 dir。
    struct ScriptedSearch {
        empty_calls: usize,
        calls: Mutex<usize>,
        dirs: Mutex<Vec<Option<std::path::PathBuf>>>,
    }

    #[async_trait]
    impl Search for ScriptedSearch {
        async fn grep(&self, req: SearchRequest) -> Result<Vec<SearchHit>> {
            let n = {
                let mut c = self.calls.lock().unwrap();
                *c += 1;
                *c
            };
            self.dirs.lock().unwrap().push(req.dir.clone());
            if n <= self.empty_calls {
                Ok(vec![])
            } else {
                Ok(vec![SearchHit {
                    path: std::path::PathBuf::from("crate-b/src/lib.rs"),
                    line: 7,
                    text: "found".into(),
                }])
            }
        }
    }

    async fn run_search(tool: &std::sync::Arc<dyn DynTool>, dir: Option<&str>) -> ToolResult {
        let mut args = serde_json::json!({"pattern": "GitCli"});
        if let Some(d) = dir {
            args["dir"] = serde_json::Value::String(d.into());
        }
        tool.call(&ToolCall {
            id: "c1".into(),
            name: "search".into(),
            args,
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn first_scope_hit_does_not_escalate() {
        let s = Arc::new(ScriptedSearch {
            empty_calls: 0,
            calls: Mutex::new(0),
            dirs: Mutex::new(vec![]),
        });
        let tool = SearchTool::new(s.clone());
        let res = run_search(&tool, Some("crate-a/src")).await;
        assert!(res.ok);
        assert_eq!(*s.dirs.lock().unwrap(), vec![Some("crate-a/src".into())]);
        assert!(!res.content.contains("scope 自动升级"));
    }

    #[tokio::test]
    async fn empty_dir_escalates_to_parent_then_workspace() {
        let s = Arc::new(ScriptedSearch {
            empty_calls: 2, // dir 与父级都空，第三级（工作区）命中
            calls: Mutex::new(0),
            dirs: Mutex::new(vec![]),
        });
        let tool = SearchTool::new(s.clone());
        let res = run_search(&tool, Some("workspace/crate-a/src")).await;
        assert!(res.ok);
        // 阶梯 = dir → 父级 → 再父级（相对路径逐级 parent）→ 全工作区；
        // 前两级空，第三级（dir="workspace"）命中，共发出 3 次请求。
        assert_eq!(
            *s.dirs.lock().unwrap(),
            vec![
                Some("workspace/crate-a/src".into()),
                Some("workspace/crate-a".into()),
                Some("workspace".into()),
            ]
        );
        assert!(res.content.contains("scope 自动升级"), "{}", res.content);
        assert!(res.content.contains("crate-b/src/lib.rs:7"), "{}", res.content);
    }

    #[tokio::test]
    async fn all_scopes_empty_reports_tried_ladder() {
        let s = Arc::new(ScriptedSearch {
            empty_calls: usize::MAX,
            calls: Mutex::new(0),
            dirs: Mutex::new(vec![]),
        });
        let tool = SearchTool::new(s.clone());
        let res = run_search(&tool, Some("a/b/c")).await;
        assert!(res.ok);
        assert!(res.content.contains("已试范围"), "{}", res.content);
        assert!(res.content.contains("dir=\"a/b/c\""), "{}", res.content);
        assert!(res.content.contains("全工作区"), "{}", res.content);
        // 不再要求模型猜下一层：建议里不再出现「去掉 dir 限定」这种手动升级指引
        assert!(!res.content.contains("去掉 dir 限定"), "{}", res.content);
    }

    #[tokio::test]
    async fn no_dir_starts_at_workspace_scope_once() {
        let s = Arc::new(ScriptedSearch {
            empty_calls: usize::MAX,
            calls: Mutex::new(0),
            dirs: Mutex::new(vec![]),
        });
        let tool = SearchTool::new(s.clone());
        let res = run_search(&tool, None).await;
        assert!(res.ok);
        assert_eq!(*s.dirs.lock().unwrap(), vec![None]);
    }
}
```

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test -p harness-tool`
Expected: 新测试编译失败或断言失败。已核实的依赖事实：`Search` trait 只有一个方法 `async fn grep(&self, req: SearchRequest) -> Result<Vec<SearchHit>>`（`harness-capability/src/search.rs:31-33`）；`SearchHit { path: PathBuf, line: u32, text: String }`（:19-22）；`harness-tool` 的 dev-dependencies 已含 tokio。

- [ ] **Step 3: 实现作用域阶梯**

`search.rs`：在 `impl SearchTool` 后新增阶梯函数，并重写 `call` 的搜索主体。

新增：

```rust
/// 作用域阶梯：给定 dir 起，逐级升到父目录（对应 crate 边界），最后到全工作区（None）。
/// 升级在工具内完成，模型不再自行猜 scope（spec §4.6）。
fn scope_ladder(dir: Option<&std::path::Path>) -> Vec<Option<std::path::PathBuf>> {
    let mut ladder = vec![dir.map(std::path::PathBuf::from)];
    let mut cur = dir;
    while let Some(d) = cur {
        cur = d.parent().filter(|p| !p.as_os_str().is_empty());
        ladder.push(cur.map(std::path::PathBuf::from));
    }
    ladder.dedup();
    ladder
}

fn scope_label(scope: &Option<std::path::PathBuf>) -> String {
    match scope {
        Some(d) => format!("dir=\"{}\"", d.display()),
        None => "全工作区".into(),
    }
}
```

把 `call` 中从 `let req = SearchRequest { … }` 到空结果返回的整段（`harness-tool/src/search.rs:63-92`）替换为：

```rust
        let scopes = scope_ladder(dir.as_deref());
        let mut tried: Vec<String> = vec![];
        let mut hits = vec![];
        for scope in scopes {
            tried.push(scope_label(&scope));
            let req = SearchRequest {
                pattern: pattern.clone(),
                dir: scope.clone(),
                max_results,
            };
            match self.search.grep(req).await {
                Ok(h) if h.is_empty() => continue,
                Ok(h) => {
                    hits = h;
                    break;
                }
                Err(e) => {
                    return Ok(ToolResult {
                        call_id: call.id.clone(),
                        ok: false,
                        content: format!("search 失败: {e}"),
                        continuation_debt: 0,
                    });
                }
            }
        }

        if hits.is_empty() {
            return Ok(ToolResult {
                call_id: call.id.clone(),
                ok: true,
                content: format!(
                    "未找到匹配（pattern=\"{pattern}\"）。已试范围：{}。建议：更换/缩短关键词，或改用符号名；不要为此编写临时扫描脚本。",
                    tried.join(" → ")
                ),
                continuation_debt: 0,
            });
        }

        let mut out = String::new();
        if tried.len() > 1 {
            out.push_str(&format!("（scope 自动升级：{}）\n", tried.join(" → ")));
        }
        out.push_str(&format!("共 {} 条命中（格式：相对路径:行号: 内容）：\n", hits.len()));
```

（保留原文件后续既有的命中行拼接与截断逻辑不变。）

- [ ] **Step 4: 运行，确认全绿**

Run: `cargo test -p harness-tool`
Expected: 全部 PASS。

- [ ] **Step 5: Checkpoint**

Checkpoint: `cargo test -p harness-tool` 全绿，且 `cargo test -p harness-runtime --test agent_tool_loop` 不回归。

---

### Task 7: 工具契约三——git 子进程平台卫生（CREATE_NO_WINDOW）

**Files:**
- Modify: `harness/harness-provider-git/src/lib.rs`

证据：7ba3370f turn 3–14「黑框闪烁」体验根因——`GitCli::run`（:47）与 `changed_files`（:109）裸调 `std::process::Command`，Windows GUI 进程下每个 git 子进程弹一帧控制台。`harness-provider-local/src/bash.rs:48` 已有正确做法可参照。

- [ ] **Step 1: 实现统一的子进程构造器**

`harness-provider-git/src/lib.rs`：在 `parse_ahead_behind` 前新增：

```rust
/// 统一的 git 子进程构造：Windows GUI 进程下必须带 CREATE_NO_WINDOW，
/// 否则每次调用都会闪一帧黑色控制台（会话 7ba3370f turn 3–14 的体验根因）。
fn git_command(repo: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}
```

- [ ] **Step 2: 替换三处 Command 构造**

`run`（`harness-provider-git/src/lib.rs:46-61`）改为：

```rust
    fn run(&self, args: &[&str]) -> Result<String> {
        let out = git_command(&self.repo(), args).output().map_err(Error::Io)?;
        if !out.status.success() {
            return Err(Error::Git(format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }
```

`changed_files` 开头（`harness-provider-git/src/lib.rs:109-114`）改为：

```rust
        let out = git_command(&self.repo(), &["status", "--porcelain", "-z"])
            .output()
            .map_err(Error::Io)?;
```

测试辅助 `init_repo`（`harness-provider-git/src/lib.rs:218-222`）改为：

```rust
        let mut cmd = git_command(path, &["init", "-q"]);
        cmd.current_dir(path);
        let output = cmd.output().unwrap();
```

- [ ] **Step 3: 运行，确认不回归**

Run: `cargo test -p harness-provider-git`
Expected: 2 tests PASS（`workspace_backed_git_follows_project_switches`、`non_repository_is_an_error_not_an_empty_change_list`）。

- [ ] **Step 4: Checkpoint**

Checkpoint: `cargo test -p harness-provider-git` 全绿。平台行为（无黑框）留待实机验证，记入后续计划的实机清单。

---

### Task 8: 全局验证与收尾

**Files:**
- 无新增修改

- [ ] **Step 1: 受影响 crate 全量测试**

Run: `cargo test -p harness-runtime -p harness-tool -p harness-provider-local -p harness-provider-git`
Expected: 全绿（红线门禁测试因 `#[ignore]` 被跳过）。

- [ ] **Step 2: 工作区整体编译**

Run: `cargo build --workspace`
Expected: 成功，无新增错误（警告若为既有则不处理）。

- [ ] **Step 3: 复跑跑红证据**

Run: `cargo test -p harness-runtime --test session_replay -- --ignored --test-threads=1`
Expected: 三个红线测试仍失败（与 Task 4 Step 3 一致）。把两次输出归档到实施记录。

- [ ] **Step 4: 更新 spec 状态**

修改 `docs/superpowers/specs/2026-08-31-agent-governance-redesign-design.md` 第 4 行状态行为：

```
- 状态：设计已获批；步骤①（回放套件，红线已跑红）与步骤③（工具层契约）已实施，待步骤②④⑤
```

- [ ] **Step 5: Checkpoint（本计划 Done）**

Checkpoint 判据（全部满足才算完成）：
1. `harness/harness-runtime/tests/fixtures/` 有 5 个 fixture，解析测试绿。
2. 默认 `cargo test -p harness-runtime --test session_replay` 全绿；`-- --ignored` 三个红线测试跑红且证据留存。
3. edit 失配返回磁盘区域报告（3 个新测试绿）。
4. search 作用域阶梯（4 个新测试绿）。
5. git 子进程统一走 `git_command`（既有测试绿）。
6. `cargo build --workspace` 成功。

---

## 后续计划（不在本计划内）

- **阶段 2**（绞杀者步骤 ②④）：case file write-only 并联对拍 → 新闭环控制器（策略栈 + information_gain + ask_user 前置条件 + 300k 顶）以 env 开关 A/B 接管；完成后移除本计划红线测试的 `#[ignore]`，回放套件转绿即为接管验收。
- **阶段 3**（绞杀者步骤 ⑤）：6 个旧计数器退位删除、`goal_execution.rs`/`execution.rs` 冗余收缩、docs V3–V5 标记 deprecated。

## 已知风险与校准预案

1. **回放保真**：模型响应识别规则（带 usage / tool_calls 的 Assistant 事件）可能误分个别事件，导致脚本与请求序列错位；`ReplayLlm` 的耗尽兜底保证不挂死，但红线跑红结论需在 Task 4 Step 3 人工核对失败原因确属守卫行为而非脚本错位。
2. **服务缺失**：`run_turn` 若依赖回放环境未注册的服务（如 Workspace 索引），在 Task 3 Step 3 按最小 stub 补齐，不引入真实文件系统扫描。
3. **成功会话漂移**：`success_session_replay_keeps_verified` 若因脚本漂移失败，先修驱动器保真度；该测试是「健康会话不被跑坏」的常设回归，不随步骤④解除。
