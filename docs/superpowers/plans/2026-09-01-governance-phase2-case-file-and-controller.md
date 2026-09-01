# Agent 治理重设计·阶段 2 实施计划（Case File + 闭环控制器 A/B）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 落地绞杀者步骤②（Case File 作为 SessionLog 的确定性投影 + 与真实日志保真对拍）与步骤④（闭环控制器接管终止权与澄清门禁，A/B 开关），使阶段 1 封存的三条红线门禁测试在控制器模式下解除 `#[ignore]` 后全绿。

**Architecture:** 三个新单元——`case_file.rs`（世界模型投影，纯派生，无第二事实源）、`governor/`（策略栈 + 增益传感器 + TurnGovernor 决策，全为可独立测试的纯逻辑）、`agent_loop.rs` 的四处窄接缝（A/B 开关、三处澄清门禁、R3 成本顶前置 + 步末观测、回合末 outcome 收口）。`GovernorMode::Legacy`（默认）下旧守卫行为逐字不变；`On` 下旧守卫并行运行但失去终止权——出口收敛为 Delivered / ExhaustedWithArtifact 两类。

**Tech Stack:** Rust（workspace `edition = "2024"`）、tokio、async-trait、serde_json、thiserror。无新第三方依赖。

---

## 权威规格与既有交付

- Spec：`docs/superpowers/specs/2026-08-31-agent-governance-redesign-design.md`（§3 红线、§4.1–4.4、§5 步骤②④、§6 测试计划、§7 Done 定义）
- 阶段 1 交付：`harness/harness-runtime/tests/session_replay.rs`（回放驱动器 `replay_session`、`summarize`、度量器 `r1_/r2_/r4_violations`、`r3_prompt_total`、`a1_guard_trips`、`a2_max_cross_turn_repeat`，三条红线测试带 `#[ignore]`）、`harness/harness-runtime/tests/fixtures/`（5 个真实会话 fixture）

## 已核实代码事实（执行前无需再探索）

- `pub struct AgentLoop;`（单元结构体）+ `pub fn new() -> Self { Self }`：`agent_loop.rs:32`、`:314-317`。真实入口 `run_turn_cancellable(ctx, input, cancellation, injected_goal)`（`:337`），`run_turn`/`run_turn_with_goal_execution` 均委托它。`AgentLoop::new()` 调用点：`harness-acp/src/server.rs:67`、`controller.rs:253`、`scheduler.rs:39`、`subagent.rs:69`、`tests/agent_tool_loop.rs`（7 处）——新增字段后 `new()` 仍在，调用点零改动。
- `let history = log.replay();` 在 `:355`；`let intent = crate::IntentProfile::compile(&task_text);` 在 `:438`；`IntentProfile { kind: IntentKind, is_task, .. }`，`IntentKind::{AtomicRegression, ScopedChange, Investigation, OpenEnded}`（`intent.rs:20-49`）；`goal_execution` 在 `:406-422` 构造完毕，`goal_execution.goal.has_locatable_signal()`（`goal_execution.rs:124`）。
- 澄清门禁三处，各自 `append(TurnStart) + append(Assistant) + append(Delivery(NeedsUserInput)) + append(Telemetry) + append(TurnEnd) + return Ok(())`：① `:458-504`（`IntentProfile::requires_clarification`，内含 `repeated` 熔断，question 变量来自 `let question = clar.question;`）② `:517-554`（`goal_execution.inspect_for_clarification(root)`）③ `:563-598`（`grounding.needs_user_input()`，question 来自 `grounding.user_question(&goal_execution.goal)`）。
- 主循环 `while debt > 0 { steps += 1; execution.steps = steps; debt -= 1;`（`:764-767`），硬熔断检查 `:768-807`，`StepStart` 落盘 `:808`。循环状态声明区 `:736-763`（`let mut steps`、`MAX_LOOP_RECOVERY_PROMPTS`、`hard_stop`/`cancelled`/`delivery_verified`/`budget_exhausted`/`absolute_budget_hit` 等）。
- 每步 Usage 落盘 `:1423-1430`（`step_usage` 在 `log.append` 处被 move）。预算/完成判定 `:1536-1605`。唯一终止检查点 `:1607-1615`。终局 outcome 链 `:1632-1690`（`let terminal_reason = goal_execution.actionable_terminal_reason();` 起头，`let (outcome, reason) = if delivery_verified {..} else if .. {..};`），紧随 `:1691-1694` append `Delivery`。
- `ExecutionState` 公开字段含 `steps / tool_calls / write_operations / evidence / changed_criteria / satisfied_criteria`（`execution.rs:338-363`）。`fn normalized_signature(call: &ToolCall) -> String` 已实现且**私有**（`execution.rs:760-781`，shell 剥离 `cd` 前缀、fs/edit 路径分隔符归一，输出 `"{name}:{args}"`）。
- `EventId = u64`（`log.rs:11`，测试可写字面量）。`SessionLog::replay() -> Vec<SessionEvent>` 为内存 Vec clone（`:525`）；`replay_from(start) -> (usize, Vec<SessionEvent>)` 增量读（`:530`）。
- `harness_llm`：`ToolCall { id, name, args: serde_json::Value }`（`:206`）、`ToolResult { call_id, ok, content, continuation_debt }`（`:222`）、`Usage { prompt_tokens, completion_tokens, total_tokens: u64 }`（`:185`）、`Chunk { text: Option<String>, ..Default }`。
- `DeliveryOutcome::{Verified, NeedsUserInput, PartialDelivery, SystemFailure, Blocked, Interrupted, Cancelled}`，derive 为 `Debug, Clone, Serialize, Deserialize, PartialEq, Eq`（**无 Ord** → 不能进 BTreeSet）。`DeliveryReport { outcome, criteria, verification, reason: Option<String> }`（`log.rs:199-205`）；`execution.delivery_report(outcome, reason) -> DeliveryReport`（`execution.rs:579`）。
- 环境：cargo 一律在 `F:\workspace\deepseek-aidops-stable\harness` 执行；git bash；Python `-X utf8`；禁 curl。**本仓库是 git 仓库**，远端 `origin = git@github.com:cgli2/deepseek-aidops-client.git`，分支 `main`，历史提交用中文 conventional commits。

## 决策边界（避免误读）

1. 控制器只取得两类权力：**是否允许 ask_user**（三处门禁统一裁决）与**是否终止回合**（R3 成本顶 + 策略栈耗尽）。旧预算续期/收敛提示在 `On` 下**继续并行运行**——其退位删除属步骤⑤（阶段 3），本阶段不删任何旧计数器，否则无法 A/B 对照。
2. 红线跑绿的机制是"违例形状被结构性消除"（不再产生 `Interrupted/SystemFailure/NeedsUserInput`），**不是**放宽检查。为此 T9 新增更强的 `missing_artifact_violations` 反作弊锁：`On` 下每个非 `Verified` 回合必须带四要素结构化资产。
3. 不修改 `SessionEvent` schema、harness-ui、LLM provider 接口、`intent.rs` 分类逻辑（spec §2 非目标）。
4. **本计划不覆盖的 spec 章节**（各自独立可验证，属阶段 3）：§4.5 三层索引的前馈 grounding 增强（本计划只用既有的 `has_locatable_signal` + `WorkspaceIndex` 命中作 grounded 判据）、§4.7 模型协议（`length` 重试 1→2 次、thinking 占比注入）、§4.8 会话级 trace 聚合与 `REGRESSION_SUSPECT` 告警。
5. **对 spec §4.3 字段表的一处偏差**：case file 字段中 `stack_pos` 不进入 `CaseFile`。它是回合级策略状态而非会话级事实，`SessionLog` 无源可派（放入投影会造成"字段有但永远为 0"的假覆盖）；运行时由 `TurnGovernor::stack.remaining()` 提供。本计划实施时应把该偏差写回 spec §4.3。

## 文件结构

```
harness/harness-runtime/src/
├── case_file.rs            [新建] CaseFile 投影
├── governor/mod.rs         [新建] TurnGovernor + Decision + PROMPT_CAP + is_continuation_request
├── governor/strategy.rs    [新建] Strategy + StrategyStack
├── governor/sensors.rs     [新建] WindowDelta/gain + artifact_text
├── execution.rs            [修改] normalized_signature → pub(crate)
├── agent_loop.rs           [修改] A/B 开关、三处门禁、R3 前置、outcome 收口
└── lib.rs                  [修改] 模块注册与导出
harness/harness-runtime/tests/
├── case_file_fidelity.rs   [新建] 步骤②保真对拍
└── session_replay.rs       [修改] replay_session_with + artifact 度量器 + 红线解除
```

---

## Task 1: CaseFile——SessionLog 的确定性投影

**Files:**
- Modify: `harness/harness-runtime/src/execution.rs:760`
- Create: `harness/harness-runtime/src/case_file.rs`
- Modify: `harness/harness-runtime/src/lib.rs`

- [ ] **Step 1: 把签名归一化提升为 crate 内可见（DRY 前置）**

`execution.rs:760` 的 `fn normalized_signature(call: &ToolCall) -> String {` 改为 `pub(crate) fn normalized_signature(call: &ToolCall) -> String {`。函数体一字不改。理由：case file 的 `tried` 签名必须与旧 `ToolRepeatGuard` 同源，否则两套"重复"定义会漂移（spec §4.3）。

- [ ] **Step 2: 创建 `case_file.rs`（完整内容）**

```rust
//! Case file：会话级世界模型（spec §4.3）。
//!
//! 单一事实源仍是 SessionLog；`CaseFile` 是它的**确定性投影**，不引入第二份持久化。
//! 回合从 case file 出发：`tried` 里已存在的签名直接换策略，跨轮无状态重放构造性消失。

use std::collections::{BTreeSet, HashMap};

use harness_session::{DeliveryOutcome, SessionEvent};

use crate::execution::normalized_signature;

/// 一次工具尝试：工具名 + 归一化签名 + 成否 + 紧凑摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriedEntry {
    pub tool: String,
    pub signature: String,
    pub ok: bool,
    pub summary: String,
}

/// 会话级世界模型。全部字段由 `absorb` 从事件流折叠得出，无外部写入路径。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaseFile {
    pub tried: Vec<TriedEntry>,
    /// 已排除的策略标签，控制器 pop 时追加（spec §4.4 gain 计量项之一）。
    pub eliminated: BTreeSet<String>,
    /// 精确锚点：含路径分隔符且带源码扩展名的 token（R4 的最低证据单位）。
    pub anchors: BTreeSet<String>,
    /// 用户信号：按序的每回合用户原话。
    pub user_signals: Vec<String>,
    /// 已问过的澄清文案（归一形式）。R2「同一问题不得问第二次」的判据。
    pub asked: BTreeSet<String>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub last_outcome: Option<DeliveryOutcome>,
    /// call_id → (工具名, 归一化签名)。ToolResult 不带工具名，需借 ToolCall 配对；
    /// 属投影内部状态，参与相等性以保证全量派生与增量 absorb 结果一致。
    pending_calls: HashMap<String, (String, String)>,
}

/// 澄清文案归一：剥离所有空白。回放套件 R2 度量器使用同一判据。
pub fn normalize_question(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// 锚点扩展名集合（与阶段 1 `has_path_anchor` 保持一致）。
const ANCHOR_EXTENSIONS: [&str; 7] = [".rs", ".toml", ".md", ".json", ".py", ".ts", ".slint"];

/// 从锚点 token 上剥除的句读/括号尾缀。
const ANCHOR_TRIM_END: [char; 12] = [
    '，', '。', '；', '、', '）', '】', '！', '"', '\'', ',', ';', ')',
];

/// 从自由文本抽取精确锚点：含 `/` 或 `\` 且含源码扩展名的空白分隔 token。
pub fn extract_anchors(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|tok| tok.trim_end_matches(ANCHOR_TRIM_END::as_ref()))
        .filter(|tok| {
            (tok.contains('/') || tok.contains('\\'))
                && ANCHOR_EXTENSIONS.iter().any(|ext| tok.contains(ext))
        })
        .map(|tok| tok.to_string())
        .collect()
}

/// 工具结果摘要：仅保留前 160 字符，控制 case file 体积（投影会被频繁重建）。
fn summarize(content: &str) -> String {
    content.chars().take(160).collect()
}

impl CaseFile {
    /// 从完整事件流确定性重建（spec §4.3：fork / resume / replay 共用此路径）。
    pub fn from_replay(events: &[SessionEvent]) -> Self {
        let mut case = Self::default();
        case.absorb(events);
        case
    }

    /// 折叠一批事件。全量派生与回合内增量共用这一条实现（DRY）。
    pub fn absorb(&mut self, events: &[SessionEvent]) {
        for ev in events {
            match ev {
                SessionEvent::TurnStart { input, .. } => self.user_signals.push(input.clone()),
                SessionEvent::Assistant { chunk, .. } => {
                    if let Some(text) = chunk.text.as_deref() {
                        for anchor in extract_anchors(text) {
                            self.anchors.insert(anchor);
                        }
                    }
                }
                SessionEvent::ToolCall { call, .. } => {
                    self.pending_calls.insert(
                        call.id.clone(),
                        (call.name.clone(), normalized_signature(call)),
                    );
                }
                SessionEvent::ToolResult { result, .. } => {
                    let (tool, signature) = self.pending_calls.remove(&result.call_id).unwrap_or((
                        "unknown".into(),
                        format!("unknown:{}", result.call_id),
                    ));
                    self.tried.push(TriedEntry {
                        tool,
                        signature,
                        ok: result.ok,
                        summary: summarize(&result.content),
                    });
                    for anchor in extract_anchors(&result.content) {
                        self.anchors.insert(anchor);
                    }
                }
                SessionEvent::Usage { usage, .. } => {
                    self.prompt_tokens += usage.prompt_tokens;
                    self.completion_tokens += usage.completion_tokens;
                }
                SessionEvent::Delivery { report, .. } => {
                    if report.outcome == DeliveryOutcome::NeedsUserInput {
                        let key = normalize_question(&turn_assistant_text(events));
                        if !key.is_empty() {
                            self.asked.insert(key);
                        }
                    }
                    self.last_outcome = Some(report.outcome.clone());
                }
                _ => {}
            }
        }
    }

    /// 该签名是否已在本会话尝试过（spec §4.3 的构造性去重入口）。
    pub fn is_tried(&self, signature: &str) -> bool {
        self.tried.iter().any(|t| t.signature == signature)
    }
}

/// 给定事件批次中最近一次 TurnStart 之后的助手全文（澄清文案去重用）。
///
/// 调用约定：一律传入「本回合完整事件」或全量事件。增量批次若不含 TurnStart，
/// `rposition` 会取 0，等价于折叠整批文本——因此 agent_loop 中的 ask_user 判定
/// 一律使用 `from_replay(&log.replay())` 的全量投影，不受此约定影响。
fn turn_assistant_text(events: &[SessionEvent]) -> String {
    let start = events
        .iter()
        .rposition(|e| matches!(e, SessionEvent::TurnStart { .. }))
        .unwrap_or(0);
    events[start..]
        .iter()
        .filter_map(|e| match e {
            SessionEvent::Assistant { chunk, .. } => chunk.text.clone(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}
```

- [ ] **Step 3: 追加单元测试（同文件末尾）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use harness_llm::{Chunk, ToolCall, ToolResult, Usage};

    fn turn(input: &str) -> SessionEvent {
        SessionEvent::TurnStart {
            id: 0,
            input: input.into(),
        }
    }

    fn assistant(text: &str) -> SessionEvent {
        SessionEvent::Assistant {
            id: 0,
            chunk: Chunk {
                text: Some(text.into()),
                ..Default::default()
            },
        }
    }

    fn call(id: &str, name: &str, args: serde_json::Value) -> SessionEvent {
        SessionEvent::ToolCall {
            id: 0,
            call: ToolCall {
                id: id.into(),
                name: name.into(),
                args,
            },
        }
    }

    fn result(id: &str, ok: bool, content: &str) -> SessionEvent {
        SessionEvent::ToolResult {
            id: 0,
            result: ToolResult {
                call_id: id.into(),
                ok,
                content: content.into(),
                continuation_debt: 0,
            },
        }
    }

    fn usage(prompt: u64, completion: u64) -> SessionEvent {
        SessionEvent::Usage {
            id: 0,
            usage: Usage {
                prompt_tokens: prompt,
                completion_tokens: completion,
                total_tokens: prompt + completion,
            },
        }
    }

    fn delivery(outcome: DeliveryOutcome, reason: Option<&str>) -> SessionEvent {
        SessionEvent::Delivery {
            id: 0,
            report: harness_session::DeliveryReport {
                outcome,
                criteria: vec![],
                verification: vec![],
                reason: reason.map(|r| r.to_string()),
            },
        }
    }

    #[test]
    fn from_replay_is_deterministic_and_accumulates_usage() {
        let events = vec![turn("消除 git 黑框"), usage(120, 30), usage(200, 40)];
        let a = CaseFile::from_replay(&events);
        assert_eq!(a, CaseFile::from_replay(&events), "同一事件流必须得到同一投影");
        assert_eq!(a.prompt_tokens, 320);
        assert_eq!(a.completion_tokens, 70);
        assert_eq!(a.user_signals, vec!["消除 git 黑框".to_string()]);
    }

    #[test]
    fn tried_signature_neutralizes_cd_prefix() {
        // 与旧守卫共用 normalized_signature：字面不同、语义相同的命令归为一个签名。
        let events = vec![
            turn("跑测试"),
            call("c1", "shell", serde_json::json!({"command": "cd /d F:/w/harness && cargo test"})),
            result("c1", true, "test result: ok"),
            call("c2", "shell", serde_json::json!({"command": "cargo test"})),
            result("c2", true, "test result: ok"),
        ];
        let case = CaseFile::from_replay(&events);
        assert_eq!(case.tried.len(), 2);
        assert_eq!(case.tried[0].signature, case.tried[1].signature);
        assert!(case.is_tried(&case.tried[0].signature));
        assert!(!case.is_tried("shell:{\"command\":\"其它\"}"));
    }

    #[test]
    fn anchors_come_from_tool_results_and_assistant_text() {
        let events = vec![
            turn("定位实现"),
            call("c1", "search", serde_json::json!({"pattern": "GitCli"})),
            result("c1", true, "harness/harness-provider-git/src/lib.rs:61: fn git_command"),
            assistant("根因在 provider-git/src/lib.rs，未加 CREATE_NO_WINDOW。"),
        ];
        let case = CaseFile::from_replay(&events);
        assert!(
            case.anchors.iter().any(|a| a.contains("harness-provider-git/src/lib.rs")),
            "{:?}",
            case.anchors
        );
        assert!(case.anchors.iter().all(|a| a.contains('/') || a.contains('\\')), "{:?}", case.anchors);
    }

    #[test]
    fn asked_records_clarification_text_only_for_needs_user_input() {
        let question = "需要补充执行信息：请确认目标模块";
        let events = vec![
            turn("改一下"),
            assistant(question),
            delivery(DeliveryOutcome::NeedsUserInput, Some(question)),
            turn("继续"),
            assistant("已交付。"),
            delivery(DeliveryOutcome::Verified, None),
        ];
        let case = CaseFile::from_replay(&events);
        assert_eq!(case.asked.len(), 1, "{:?}", case.asked);
        assert!(case.asked.contains(&normalize_question(question)));
        assert_eq!(case.last_outcome, Some(DeliveryOutcome::Verified));
    }

    #[test]
    fn unpaired_tool_result_still_recorded() {
        // 中断会话可能缺 ToolCall 配对：不得丢证据，退化为 unknown 签名。
        let case = CaseFile::from_replay(&[turn("x"), result("orphan", false, "matched 0")]);
        assert_eq!(case.tried.len(), 1);
        assert_eq!(case.tried[0].tool, "unknown");
        assert!(!case.tried[0].ok);
    }
}
```

- [ ] **Step 4: 运行，确认失败**

Run: `cargo test -p harness-runtime case_file`
Expected: `error[E0432]`/unresolved module —— 模块尚未注册（预期红灯）。

- [ ] **Step 5: 注册模块与导出（`lib.rs`）**

在 `pub mod builtin_profile;` 之后插入 `pub mod case_file;`；在 `pub use` 区追加：

```rust
pub use case_file::{extract_anchors, normalize_question, CaseFile, TriedEntry};
```

- [ ] **Step 6: 运行，确认全绿**

Run: `cargo test -p harness-runtime`
Expected: 原 168 个单测 + 新增 5 个全绿，各集成测试不回归。

- [ ] **Step 7: Commit**

```bash
git add harness/harness-runtime/src/case_file.rs harness/harness-runtime/src/execution.rs harness/harness-runtime/src/lib.rs
git commit -m "feat(governance): 新增 CaseFile 投影，会话世界模型从 SessionLog 确定性派生"
```

---

## Task 2: 策略栈（spec §4.2）

**Files:**
- Create: `harness/harness-runtime/src/governor/mod.rs`（本任务建最小壳）
- Create: `harness/harness-runtime/src/governor/strategy.rs`
- Modify: `harness/harness-runtime/src/lib.rs`

- [ ] **Step 1: 创建 `governor/mod.rs` 壳**

```rust
//! 单一闭环控制器（spec §4.1）：observe → measure → decide。
//!
//! 决策只有四分支：continue / switch_strategy / degrade / terminate。旧守卫在本模块
//! 里降级为传感器——只产信号，不终止回合；终止权收归 TurnGovernor（Task 4 落地）。

pub mod strategy;
```

- [ ] **Step 2: 创建 `governor/strategy.rs`**

```rust
//! 策略栈：策略 = (工具集, 搜索范围, 预算窗口, 退出条件) 的可弹出序列（spec §4.2）。

use std::fmt;

/// 单个策略帧。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// 带 grounding 锚点直接 change + verify
    GroundedChange,
    /// 全工作区搜索 / 字符串索引一跳
    BroadLocate,
    /// 诊断模式（运行时观察，如 cargo check）
    RuntimeObserve,
    /// 紧凑检查点换路（清历史、最小快照）
    CompactReroute,
    /// 交付可验证子目标
    DegradeGoal,
    /// 栈底常驻：ExhaustedWithArtifact 的构造点
    PartialDeliver,
}

impl fmt::Display for Strategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Strategy::GroundedChange => "grounded_change",
            Strategy::BroadLocate => "broad_locate",
            Strategy::RuntimeObserve => "runtime_observe",
            Strategy::CompactReroute => "compact_reroute",
            Strategy::DegradeGoal => "degrade_goal",
            Strategy::PartialDeliver => "partial_deliver",
        })
    }
}

/// 策略窗口大小 W：单个策略的尝试步数（spec §4.4 三参数之一）。
pub const WINDOW_STEPS: usize = 4;

/// 自顶向下的策略栈。`frames.last()` 是当前策略；栈底恒为 `PartialDeliver`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrategyStack {
    frames: Vec<Strategy>,
}

const FULL: [Strategy; 6] = [
    Strategy::GroundedChange,
    Strategy::BroadLocate,
    Strategy::RuntimeObserve,
    Strategy::CompactReroute,
    Strategy::DegradeGoal,
    Strategy::PartialDeliver,
];

impl StrategyStack {
    /// 读写任务默认栈：grounding 命中从 grounded_change 起，未命中从 broad_locate 起
    /// （spec §4.5「未命中 → 诊断模式」）。
    pub fn for_task(grounded: bool) -> Self {
        Self {
            frames: if grounded { FULL.to_vec() } else { FULL[1..].to_vec() },
        }
    }

    /// Investigation 意图的只读栈变体（spec §4.2）：不含写入型策略。
    pub fn read_only() -> Self {
        Self {
            frames: vec![
                Strategy::BroadLocate,
                Strategy::RuntimeObserve,
                Strategy::PartialDeliver,
            ],
        }
    }

    pub fn current(&self) -> Option<Strategy> {
        self.frames.last().copied()
    }

    /// 剩余帧数（含当前帧）。
    pub fn remaining(&self) -> usize {
        self.frames.len()
    }

    /// 当前是否已在栈底。栈底不可弹出。
    pub fn at_bottom(&self) -> bool {
        matches!(self.current(), Some(Strategy::PartialDeliver))
    }

    /// ask_user 前置之一：剩余策略 ⊆ {degrade_goal, partial_deliver}（即栈深 ≤ 2）。
    pub fn allow_ask_user(&self) -> bool {
        self.frames
            .iter()
            .all(|s| matches!(s, Strategy::DegradeGoal | Strategy::PartialDeliver))
    }

    /// 弹出当前策略；已在栈底返回 `None`（由控制器判定终止）。
    pub fn pop(&mut self) -> Option<Strategy> {
        if self.at_bottom() {
            return None;
        }
        self.frames.pop()
    }
}
```

- [ ] **Step 3: 追加测试（同文件末尾）**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grounded_stack_starts_at_grounded_change() {
        let stack = StrategyStack::for_task(true);
        assert_eq!(stack.current(), Some(Strategy::GroundedChange));
        assert_eq!(stack.remaining(), 6);
        assert!(!stack.allow_ask_user(), "栈顶时不许提问：还有 5 层可试");
    }

    #[test]
    fn ungrounded_stack_skips_grounded_change() {
        let stack = StrategyStack::for_task(false);
        assert_eq!(stack.current(), Some(Strategy::BroadLocate));
        assert_eq!(stack.remaining(), 5);
    }

    #[test]
    fn read_only_stack_has_no_write_strategies() {
        let stack = StrategyStack::read_only();
        assert_eq!(stack.current(), Some(Strategy::BroadLocate));
        assert_eq!(stack.remaining(), 3);
    }

    #[test]
    fn popping_down_to_bottom_enables_ask_user_gate() {
        let mut stack = StrategyStack::for_task(false);
        for expected in [
            Strategy::BroadLocate,
            Strategy::RuntimeObserve,
            Strategy::CompactReroute,
        ] {
            assert_eq!(stack.pop(), Some(expected));
        }
        assert_eq!(stack.current(), Some(Strategy::DegradeGoal));
        assert!(stack.allow_ask_user(), "仅剩 degrade_goal + partial_deliver");
    }

    #[test]
    fn bottom_strategy_is_never_popped() {
        let mut stack = StrategyStack::read_only();
        assert_eq!(stack.pop(), Some(Strategy::BroadLocate));
        assert_eq!(stack.pop(), Some(Strategy::RuntimeObserve));
        assert!(stack.at_bottom());
        assert_eq!(stack.pop(), None, "栈底常驻，返回 None 由控制器判定终止");
        assert_eq!(stack.current(), Some(Strategy::PartialDeliver));
    }

    #[test]
    fn strategy_display_labels_match_spec_naming() {
        assert_eq!(Strategy::PartialDeliver.to_string(), "partial_deliver");
        assert_eq!(Strategy::DegradeGoal.to_string(), "degrade_goal");
    }
}
```

- [ ] **Step 4: 注册并运行**

`lib.rs` 插入 `pub mod governor;`，`pub use` 区追加：

```rust
pub use governor::strategy::{Strategy, StrategyStack, WINDOW_STEPS};
```

Run: `cargo test -p harness-runtime governor`
Expected: `test result: ok. 6 passed`

- [ ] **Step 5: Commit**

```bash
git add harness/harness-runtime/src/governor/ harness/harness-runtime/src/lib.rs
git commit -m "feat(governance): 策略栈落地，ask_user 栈深前置与栈底常驻语义固定"
```

---

## Task 3: 增益传感器与 R4 资产构造点（spec §4.4 / §3 R4）

**Files:**
- Create: `harness/harness-runtime/src/governor/sensors.rs`
- Modify: `harness/harness-runtime/src/governor/mod.rs`、`lib.rs`

- [ ] **Step 1: 创建 `sensors.rs`**

```rust
//! 传感器：旧守卫降级后的信号生产者（spec §4.1 / §4.4）。只算信号，不做终止判断。

use crate::case_file::CaseFile;

/// 一个策略窗口内的增益分量。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WindowDelta {
    pub new_anchors: usize,
    pub new_eliminations: usize,
    pub write_increment: usize,
    pub new_user_signals: usize,
}

impl WindowDelta {
    /// 窗口总增益。控制器规则：`gain == 0` → pop 栈（spec §4.2）。
    pub fn gain(&self) -> usize {
        self.new_anchors + self.new_eliminations + self.write_increment + self.new_user_signals
    }
}

/// 取同一时间线上两份投影的差值（`window_base` 为窗口起点快照）。
/// 写入增量不在 CaseFile 里（它是 Runtime 侧计数），由调用方补。
pub fn delta_between(window_base: &CaseFile, now: &CaseFile) -> WindowDelta {
    WindowDelta {
        new_anchors: now.anchors.len().saturating_sub(window_base.anchors.len()),
        new_eliminations: now.eliminated.len().saturating_sub(window_base.eliminated.len()),
        write_increment: 0,
        new_user_signals: now
            .user_signals
            .len()
            .saturating_sub(window_base.user_signals.len()),
    }
}

/// R4 / ExhaustedWithArtifact 的结构化资产（spec §3 R4 四要素）。
///
/// 标记词 `锚点：` / `假设：` / `补丁建议：` / `问项：` 是回放套件
/// `missing_artifact_violations` 度量器的解析契约——改名即破绿。缺失要素一律写
/// 显式占位，不得省略（「无锚点」也要说明为何无锚点）。
pub fn artifact_text(
    case: &CaseFile,
    hypothesis: &str,
    suggested_patch: &str,
    candidate_question: Option<&str>,
) -> String {
    let anchors = if case.anchors.is_empty() {
        "无（本回合未产生任何工具命中或路径证据）".to_string()
    } else {
        case.anchors.iter().cloned().collect::<Vec<_>>().join("; ")
    };
    let hypothesis = if hypothesis.trim().is_empty() {
        "待形成（尚未收敛出可验证的根因假设）"
    } else {
        hypothesis.trim()
    };
    let suggested_patch = if suggested_patch.trim().is_empty() {
        "无（缺少可落地的改动形状）"
    } else {
        suggested_patch.trim()
    };
    format!(
        "【资产】锚点：{anchors}\n假设：{hypothesis}\n补丁建议：{suggested_patch}\n问项：{}",
        candidate_question.unwrap_or("无")
    )
}
```

- [ ] **Step 2: 追加测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use harness_llm::Chunk;
    use harness_session::SessionEvent;

    fn case_with(anchors: &[&str], signals: usize) -> CaseFile {
        let mut events: Vec<SessionEvent> = (0..signals)
            .map(|i| SessionEvent::TurnStart {
                id: i as u64,
                input: format!("输入 {i}"),
            })
            .collect();
        events.push(SessionEvent::Assistant {
            id: 99,
            chunk: Chunk {
                text: Some(anchors.join(" ")),
                ..Default::default()
            },
        });
        CaseFile::from_replay(&events)
    }

    #[test]
    fn gain_sums_all_four_components() {
        let delta = WindowDelta {
            new_anchors: 2,
            new_eliminations: 1,
            write_increment: 3,
            new_user_signals: 4,
        };
        assert_eq!(delta.gain(), 10);
        assert_eq!(WindowDelta::default().gain(), 0, "无增益即 0，控制器据此换路");
    }

    #[test]
    fn delta_counts_new_anchors_and_signals_only() {
        let window_base = case_with(&["a/one.rs"], 1);
        let now = case_with(&["a/one.rs", "b/two.rs", "c/three.rs"], 2);
        let delta = delta_between(&window_base, &now);
        assert_eq!(delta.new_anchors, 2, "{:?}", now.anchors);
        assert_eq!(delta.new_user_signals, 1);
        assert_eq!(delta.write_increment, 0, "写入增量由调用方补");
    }

    #[test]
    fn delta_never_goes_negative_when_window_base_is_ahead() {
        // 续跑时窗口基线可能取自更长历史：saturating_sub 保证不倒扣。
        let window_base = case_with(&["x/a.rs", "y/b.rs"], 3);
        let now = case_with(&["x/a.rs"], 1);
        let delta = delta_between(&window_base, &now);
        assert_eq!(delta.new_anchors, 0);
        assert_eq!(delta.new_user_signals, 0);
    }

    #[test]
    fn artifact_always_carries_all_four_labels() {
        let text = artifact_text(&CaseFile::default(), "", "", None);
        for label in ["锚点：", "假设：", "补丁建议：", "问项："] {
            assert!(text.contains(label), "{label} 缺失：{text}");
        }
        assert!(text.contains("无（本回合未产生任何工具命中或路径证据）"), "{text}");
        assert!(text.contains("待形成"), "{text}");
    }

    #[test]
    fn artifact_lists_anchors_and_candidate_question() {
        let case = case_with(&["harness/src/lib.rs", "docs/spec.md"], 1);
        let text = artifact_text(
            &case,
            "门禁在澄清出口未去重",
            "把三处门禁收敛到 ask_user 前置裁决",
            Some("是否只修 src/lib.rs？"),
        );
        assert!(text.contains("harness/src/lib.rs"), "{text}");
        assert!(text.contains("门禁在澄清出口未去重"), "{text}");
        assert!(text.contains("是否只修 src/lib.rs？"), "{text}");
    }
}
```

- [ ] **Step 3: 导出并运行**

`governor/mod.rs` 的 `pub mod strategy;` 之后追加 `pub mod sensors;`；`lib.rs` 追加：

```rust
pub use governor::sensors::{artifact_text, delta_between, WindowDelta};
```

Run: `cargo test -p harness-runtime governor`
Expected: `test result: ok. 11 passed`

- [ ] **Step 4: Commit**

```bash
git add harness/harness-runtime/src/governor/ harness/harness-runtime/src/lib.rs
git commit -m "feat(governance): 增益传感器与 R4 四要素资产构造点"
```

---

## Task 4: TurnGovernor 决策函数

**Files:**
- Modify: `harness/harness-runtime/src/governor/mod.rs`（整体替换）
- Modify: `harness/harness-runtime/src/agent_loop.rs:101-106`（`is_continuation_request` 迁出）
- Modify: `harness/harness-runtime/src/lib.rs`

- [ ] **Step 1: 写失败测试（追加到 `governor/mod.rs` 末尾）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use harness_llm::Chunk;
    use harness_session::SessionEvent;

    fn case_with_anchors(anchors: &[&str]) -> CaseFile {
        CaseFile::from_replay(&[
            SessionEvent::TurnStart {
                id: 0,
                input: "定位问题".into(),
            },
            SessionEvent::Assistant {
                id: 1,
                chunk: Chunk {
                    text: Some(anchors.join(" ")),
                    ..Default::default()
                },
            },
        ])
    }

    #[test]
    fn continues_until_window_is_full() {
        let case = CaseFile::default();
        let mut gov = TurnGovernor::new(&case, true, false);
        for step in 1..WINDOW_STEPS {
            assert_eq!(gov.observe(&case, step, 0), Decision::Continue, "step {step}");
        }
    }

    #[test]
    fn zero_gain_at_window_end_switches_strategy() {
        let case = CaseFile::default();
        let mut gov = TurnGovernor::new(&case, true, false);
        assert_eq!(gov.observe(&case, WINDOW_STEPS, 0), Decision::SwitchStrategy);
        assert_eq!(gov.current_strategy(), Some(Strategy::BroadLocate), "grounded_change 已弹出");
    }

    #[test]
    fn positive_gain_resets_window_without_popping() {
        let mut gov = TurnGovernor::new(&CaseFile::default(), true, false);
        let now = case_with_anchors(&["a/b.rs"]);
        assert_eq!(gov.observe(&now, WINDOW_STEPS, 0), Decision::Continue);
        assert_eq!(gov.current_strategy(), Some(Strategy::GroundedChange));
        assert_eq!(gov.observe(&now, WINDOW_STEPS * 2, 0), Decision::Continue, "窗口基线已重置，锚点不重复计增益");
    }

    #[test]
    fn write_increment_counts_as_gain() {
        let case = CaseFile::default();
        let mut gov = TurnGovernor::new(&case, true, false);
        // 锚点零增长但有 1 次成功写入 → 仍算有增益，不换路（写入型策略的进展来源）。
        assert_eq!(gov.observe(&case, WINDOW_STEPS, 1), Decision::Continue);
    }

    #[test]
    fn degrade_at_bottom_then_terminate() {
        // 只读栈：broad_locate → runtime_observe →（栈底）partial_deliver
        let case = CaseFile::default();
        let mut gov = TurnGovernor::new(&case, false, true);
        assert_eq!(gov.observe(&case, WINDOW_STEPS, 0), Decision::SwitchStrategy);
        assert_eq!(gov.current_strategy(), Some(Strategy::RuntimeObserve));
        assert_eq!(gov.observe(&case, WINDOW_STEPS * 2, 0), Decision::Degrade);
        assert!(gov.stack.at_bottom());
        assert_eq!(
            gov.observe(&case, WINDOW_STEPS * 3, 0),
            Decision::Terminate(Termination::ExhaustedWithArtifact),
            "栈底零增益是全系统唯一的回合终止来源"
        );
    }

    #[test]
    fn eliminated_strategies_are_recorded_once() {
        let case = CaseFile::default();
        let mut gov = TurnGovernor::new(&case, false, true);
        gov.observe(&case, WINDOW_STEPS, 0);
        assert_eq!(gov.eliminated().len(), 1);
        let now = {
            let mut c = case.clone();
            c.eliminated = gov.eliminated().clone();
            c
        };
        assert_eq!(delta_between(&case, &now).new_eliminations, 1);
        // 新基线吸收 eliminated，故同一条排除不会被再计一次。
        assert_eq!(gov.observe(&now, WINDOW_STEPS * 2, 0), Decision::Degrade);
        assert_eq!(delta_between(gov.window_base(), &now).new_eliminations, 0);
    }

    #[test]
    fn prompt_ceiling_blocks_request_before_it_is_issued() {
        let gov = TurnGovernor::new(&CaseFile::default(), true, false);
        assert!(!gov.should_stop_before_request(PROMPT_CAP - 10, 0));
        assert!(gov.should_stop_before_request(PROMPT_CAP - 10, 10), "累计+增量下界到顶");
        assert!(gov.should_stop_before_request(PROMPT_CAP, 0), "已到顶即停，不得再探索");
        assert!(gov.should_stop_before_request(PROMPT_CAP + 1, 0), "超顶同样拦死");
    }

    #[test]
    fn ask_user_requires_all_three_preconditions() {
        let case = CaseFile::default();
        let mut gov = TurnGovernor::new(&case, false, false);
        let q = "目标模块是哪个（候选：harness-tool、harness-runtime）";
        assert!(!gov.ask_user_allowed(&case, "这个问题解决了吗？", q), "前置①栈深不满足");

        gov.stack = StrategyStack::for_task(false);
        while !gov.stack.allow_ask_user() {
            gov.stack.pop();
        }
        assert!(gov.ask_user_allowed(&case, "目标在哪", q));
        assert!(!gov.ask_user_allowed(&case, "继续", q), "R1：续跑回复永不问用户");
        assert!(
            !gov.ask_user_allowed(&case, "目标在哪", "目标模块是哪个？"),
            "R2：开放模板问题一律禁止"
        );
        let mut asked = case.clone();
        asked.asked.insert(normalize_question(q));
        assert!(!gov.ask_user_allowed(&asked, "目标在哪", q), "R2：同一问题不得问第二次");
    }

    #[test]
    fn continuation_prefix_list_is_single_sourced() {
        for text in ["继续", "接着做", "续跑一下", "恢复任务", "Continue please", "RESUME"] {
            assert!(is_continuation_request(text), "{text}");
        }
        assert!(!is_continuation_request("这个问题解决了吗？"));
    }
}
```

Run: `cargo test -p harness-runtime governor`
Expected: 编译失败——`TurnGovernor` / `Decision` / `Termination` / `PROMPT_CAP` / `window_base()` 未定义。

- [ ] **Step 2: 把 `is_continuation_request` 单点化到 governor**

`agent_loop.rs:101-106` 的整段

```rust
fn is_continuation_request(text: &str) -> bool {
    let trimmed = text.trim().to_lowercase();
    ["继续", "接着", "续跑", "恢复", "continue", "resume"]
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
}
```

从 `agent_loop.rs` 删除，改在其顶部 `use` 区（`:27` 附近）加：

```rust
use crate::governor::is_continuation_request;
```

`:143 / :357 / :358` 三处调用点保持原名不动。

- [ ] **Step 3: 用下列完整内容替换 `governor/mod.rs`**

```rust
//! 单一闭环控制器（spec §4.1）：observe → measure → decide。
//!
//! 决策只有四分支：continue / switch_strategy / degrade / terminate。旧守卫降级为
//! 传感器——只产信号，不终止回合；终止权收归 `TurnGovernor`。出口仅两类：
//! `Delivered` 与 `ExhaustedWithArtifact`。ask_user 是工具调用而非出口，受三重前置约束。

pub mod sensors;
pub mod strategy;

pub use sensors::{artifact_text, delta_between, WindowDelta};
pub use strategy::{Strategy, StrategyStack, WINDOW_STEPS};

use std::collections::BTreeSet;

use crate::case_file::{normalize_question, CaseFile};

/// 会话 prompt tokens 硬顶（spec §3 R3）。回放套件引用同一常量，禁止两处各自写死。
pub const PROMPT_CAP: u64 = 300_000;

/// 候选列表标记：ask_user 的问题必须带工作区派生的候选（R2「禁开放模板」）。
pub const CANDIDATE_MARKERS: [&str; 2] = ["候选：", "可选："];

/// 续跑式回复前缀。单点定义：agent_loop 的 resume 判定、控制器 ask_user 前置、
/// 回放套件 R1 度量器共用，避免「同一概念三处各写一份」的漂移。
pub fn is_continuation_request(text: &str) -> bool {
    let trimmed = text.trim().to_lowercase();
    ["继续", "接着", "续跑", "恢复", "continue", "resume"]
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
}

/// 问题是否带候选列表（R2 硬前置）。
pub fn has_candidates(question: &str) -> bool {
    CANDIDATE_MARKERS.iter().any(|m| question.contains(m))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Termination {
    /// 交付成立（Runtime 验收通过）。
    Delivered,
    /// 策略栈耗尽：带 R4 四要素资产收尾，绝不裸停交还用户。
    ExhaustedWithArtifact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Continue,
    SwitchStrategy,
    Degrade,
    Terminate(Termination),
}

/// 回合控制器。一个回合一个实例，持有策略栈与窗口基线。
#[derive(Debug, Clone)]
pub struct TurnGovernor {
    pub stack: StrategyStack,
    window_base: CaseFile,
    window_base_step: usize,
    window_base_writes: usize,
    eliminated: BTreeSet<String>,
    /// 决策审计轨迹：遥测与 A/B 对照用，不参与决策本身。
    pub decisions: Vec<Decision>,
}

impl TurnGovernor {
    /// `grounded` 来自 grounding 命中；`read_only` 对应 Investigation 意图。
    pub fn new(case: &CaseFile, grounded: bool, read_only: bool) -> Self {
        let stack = if read_only {
            StrategyStack::read_only()
        } else {
            StrategyStack::for_task(grounded)
        };
        Self {
            stack,
            window_base: case.clone(),
            window_base_step: 0,
            window_base_writes: 0,
            eliminated: BTreeSet::new(),
            decisions: vec![],
        }
    }

    pub fn current_strategy(&self) -> Option<Strategy> {
        self.stack.current()
    }

    pub fn eliminated(&self) -> &BTreeSet<String> {
        &self.eliminated
    }

    /// 测试/A-B 对照用：窗口基线投影（eliminated 已在此前 pop 时并入）。
    pub fn window_base(&self) -> &CaseFile {
        &self.window_base
    }

    /// 换路/降级提示引用的当前策略标签。
    pub fn strategy_hint(&self) -> String {
        self.stack
            .current()
            .map(|s| s.to_string())
            .unwrap_or_else(|| Strategy::PartialDeliver.to_string())
    }

    /// R3 前置判顶：`prompt_so_far` 为会话累计 prompt tokens，`last_prompt_tokens`
    /// 为上一轮请求的实际 prompt。上下文单调不减，故后者是下一轮增量的下界，
    /// 据此拦停可保证累计值不越过硬顶（spec §3「超顶只允许 partial_deliver」）。
    pub fn should_stop_before_request(&self, prompt_so_far: u64, last_prompt_tokens: u64) -> bool {
        prompt_so_far.saturating_add(last_prompt_tokens) >= PROMPT_CAP
    }

    /// 步末观测：最新投影 + 累计步数 + 累计写入数 → 唯一决策。
    pub fn observe(&mut self, now: &CaseFile, step: usize, total_writes: usize) -> Decision {
        let steps_in_window = step.saturating_sub(self.window_base_step);
        let mut delta = delta_between(&self.window_base, now);
        delta.write_increment = total_writes.saturating_sub(self.window_base_writes);

        let decision = if steps_in_window < WINDOW_STEPS {
            Decision::Continue
        } else if delta.gain() > 0 {
            self.restart_window(now, step, total_writes);
            Decision::Continue
        } else if let Some(popped) = self.stack.pop() {
            self.eliminated.insert(popped.to_string());
            self.restart_window(now, step, total_writes);
            if self.stack.at_bottom() {
                Decision::Degrade
            } else {
                Decision::SwitchStrategy
            }
        } else {
            // pop 返回 None（已在栈底）且窗口零增益 → 唯一的终止分支。
            Decision::Terminate(Termination::ExhaustedWithArtifact)
        };
        self.decisions.push(decision);
        decision
    }

    /// ask_user 三重前置（spec §4.2）：栈深 ≤ 2、非续跑回复、不在 asked 且带候选。
    pub fn ask_user_allowed(&self, case: &CaseFile, input_text: &str, question: &str) -> bool {
        self.stack.allow_ask_user()
            && !is_continuation_request(input_text)
            && has_candidates(question)
            && !case.asked.contains(&normalize_question(question))
    }

    /// 窗口重新起算：新基线并入已记录的排除策略，使控制器自身产生的排除信号不被重复计数。
    fn restart_window(&mut self, now: &CaseFile, step: usize, total_writes: usize) {
        let mut base = now.clone();
        base.eliminated.extend(self.eliminated.iter().cloned());
        self.window_base = base;
        self.window_base_step = step;
        self.window_base_writes = total_writes;
    }
}
```

- [ ] **Step 4: 更新导出（`lib.rs`）**

```rust
pub use governor::{
    artifact_text, delta_between, has_candidates, is_continuation_request, normalize_question,
    Decision, Termination, TurnGovernor, WindowDelta, CANDIDATE_MARKERS, PROMPT_CAP,
};
```

与 T1/T3 已加的行合并去重：`normalize_question`、`artifact_text`、`delta_between`、`WindowDelta` 各只保留一处导出（保留 `governor::{...}` 这一条，删除 `case_file::{...}` 与 `governor::sensors::{...}` 中的重复项；`case_file::{extract_anchors, CaseFile, TriedEntry}` 保留）。

- [ ] **Step 5: 运行**

Run: `cargo test -p harness-runtime`
Expected: `governor` 相关 19 passed（6+5+8），全 crate 不回归。

- [ ] **Step 6: Commit**

```bash
git add harness/harness-runtime/src/governor/mod.rs harness/harness-runtime/src/agent_loop.rs harness/harness-runtime/src/lib.rs
git commit -m "feat(governance): TurnGovernor 落地唯一决策函数与 ask_user 三重前置"
```

---

## Task 5: Case File 保真对拍（步骤②验收）

**Files:**
- Create: `harness/harness-runtime/tests/case_file_fidelity.rs`

- [ ] **Step 1: 创建对拍测试（完整内容）**

```rust
//! 绞杀者步骤②验收：Case File 只记录不决策，与真实会话日志对拍 tried/anchors/asked
//! 保真度（spec §5 步骤 2）。读的是**原始会话** fixture，不是重放产出的新日志——
//! 世界模型必须能从真实失败会话读出正确形状，才有资格在步骤④接管决策。

use harness_runtime::CaseFile;
use harness_session::SessionEvent;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/");

fn case_of(name: &str) -> CaseFile {
    let raw = std::fs::read_to_string(format!("{FIXTURES}{name}"))
        .unwrap_or_else(|e| panic!("fixture {name} 读取失败: {e}"));
    let events: Vec<SessionEvent> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("fixture 事件解析失败"))
        .collect();
    CaseFile::from_replay(&events)
}

#[test]
fn projection_is_deterministic_on_real_logs() {
    for fixture in [
        "7ba3370f_t03_14_symptom.jsonl",
        "7ba3370f_t15_18_clarification.jsonl",
        "7ba3370f_t19_22_gitfix.jsonl",
        "success_677bd6e0.jsonl",
    ] {
        assert_eq!(case_of(fixture), case_of(fixture), "{fixture} 派生不确定");
    }
}

#[test]
fn user_signals_match_turn_counts() {
    assert_eq!(case_of("7ba3370f_t03_14_symptom.jsonl").user_signals.len(), 12);
    assert_eq!(case_of("7ba3370f_t15_18_clarification.jsonl").user_signals.len(), 4);
    assert_eq!(case_of("7ba3370f_t19_22_gitfix.jsonl").user_signals.len(), 4);
    assert_eq!(case_of("success_677bd6e0.jsonl").user_signals.len(), 5);
    assert_eq!(case_of("7ba3370f_full.jsonl").user_signals.len(), 22);
}

#[test]
fn clarification_segment_repeats_one_question_and_runs_no_tools() {
    // 复盘根因 4（spec §1）：turn 15–18 是同一澄清文案的无限复读。
    let case = case_of("7ba3370f_t15_18_clarification.jsonl");
    assert!(case.tried.is_empty(), "澄清段不该有任何工具调用：{:?}", case.tried);
    assert_eq!(case.asked.len(), 1, "四回合复读应折叠为一条 asked：{:?}", case.asked);
}

#[test]
fn gitfix_segment_records_failed_edit_attempts() {
    // 复盘证据（spec §附录）：turn 19 连续 3 次 edit matched 0。
    let case = case_of("7ba3370f_t19_22_gitfix.jsonl");
    let edits: Vec<_> = case.tried.iter().filter(|t| t.tool == "edit").collect();
    assert!(!edits.is_empty(), "gitfix 段应记录 edit 尝试");
    assert!(edits.iter().any(|t| !t.ok), "至少要有一条失败的 edit 记录");
    assert!(
        edits.iter().any(|t| t.summary.contains("matched")),
        "edit 失配摘要应进入 tried.summary：{:?}",
        edits.iter().map(|t| &t.summary).collect::<Vec<_>>()
    );
}

#[test]
fn symptom_segment_accumulates_anchors() {
    let case = case_of("7ba3370f_t03_14_symptom.jsonl");
    assert!(!case.tried.is_empty(), "症状段必须有工具尝试");
    assert!(!case.anchors.is_empty(), "工具命中必须沉淀出精确锚点");
    assert!(
        case.anchors.iter().all(|a| {
            (a.contains('/') || a.contains('\\'))
                && [".rs", ".toml", ".md", ".json", ".py", ".ts", ".slint"]
                    .iter()
                    .any(|ext| a.contains(ext))
        }),
        "锚点必须同时具备路径分隔符与源码扩展名：{:?}",
        case.anchors
    );
}

#[test]
fn full_session_token_cost_exceeds_red_line_cap() {
    // 同时是 R3 度量器的有效性证明：原会话真实成本必须远超 300k，否则 R3 在回放里
    // 永远读 0、红线形同虚设（阶段 1 遗留疑虑在此关闭）。
    let case = case_of("7ba3370f_full.jsonl");
    assert!(
        case.prompt_tokens > harness_runtime::PROMPT_CAP,
        "复盘记录 3.14M prompt tokens，实际读出 {}（顶 {}）",
        case.prompt_tokens,
        harness_runtime::PROMPT_CAP
    );
}
```

- [ ] **Step 2: 运行**

Run: `cargo test -p harness-runtime --test case_file_fidelity`
Expected: `test result: ok. 6 passed`

**若 `clarification_segment_repeats_one_question_and_runs_no_tools` 失败**：说明原始澄清文案内嵌了逐回合变化的令牌（序号/时间戳）。这本身是阶段 2 的有效发现——按 DONE_WITH_CONCERNS 原样上报，**禁止**靠放宽 `normalize_question` 修绿（那会让 R2 度量器一起失效）。
**若 `full_session_token_cost_exceeds_red_line_cap` 失败**：说明该 fixture 未记录 `Usage` 事件，则 R3 无法在回放中被验证，只能靠实机对照——把该限制写入 spec §5 步骤④的验收说明，并继续后续任务。

- [ ] **Step 3: Commit**

```bash
git add harness/harness-runtime/tests/case_file_fidelity.rs
git commit -m "test(governance): case file 与真实会话日志保真对拍"
```

---

## Task 6: AgentLoop 的 A/B 开关（spec §5 步骤④「env/配置开关」）

**Files:**
- Modify: `harness/harness-runtime/src/agent_loop.rs:29-32`、`:314-317`
- Modify: `harness/harness-runtime/src/lib.rs:22`

- [ ] **Step 1: 写失败测试（追加到 `agent_loop.rs` 现有 `mod tests` 内）**

```rust
    #[test]
    fn governor_mode_is_explicit_and_defaults_to_legacy() {
        // 默认必须走旧路径：绞杀者要求新控制器显式 opt-in，实机随时可回滚。
        assert_eq!(AgentLoop::new().governor_mode(), GovernorMode::Legacy);
        assert_eq!(
            AgentLoop::new().with_governor(GovernorMode::On).governor_mode(),
            GovernorMode::On
        );
        // 解析函数独立可测，不依赖真实进程环境（edition 2024 下 env 写入是 unsafe）。
        assert_eq!(parse_governor_mode(None), GovernorMode::Legacy);
        assert_eq!(parse_governor_mode(Some("on")), GovernorMode::On);
        assert_eq!(parse_governor_mode(Some(" 1 ")), GovernorMode::On);
        assert_eq!(parse_governor_mode(Some("TRUE")), GovernorMode::On);
        assert_eq!(parse_governor_mode(Some("legacy")), GovernorMode::Legacy);
        assert_eq!(parse_governor_mode(Some("")), GovernorMode::Legacy);
    }
```

Run: `cargo test -p harness-runtime governor_mode`
Expected: 编译失败——`GovernorMode` / `governor_mode` / `with_governor` / `parse_governor_mode` 未定义。

- [ ] **Step 2: 实现开关**

`agent_loop.rs:29-32` 的

```rust
/// Agent 循环 / Turn-Step 生命周期（原 §5.6）。
///
/// `Turn` = 0..n `Step`；`debt` 计数控制续跑；`agent/turn-stopping` 为唯一串行终止点。
pub struct AgentLoop;
```

替换为

```rust
/// 治理路径选择（spec §5 步骤④：新控制器接管决策走 A/B）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GovernorMode {
    /// 旧守卫网络拥有终止权（默认，保证实机可回滚）。
    Legacy,
    /// 终止权收归 TurnGovernor；旧守卫并行运行但只产信号。
    On,
}

/// 解析 `HARNESS_GOVERNOR`：只有显式 on/1/true 才启用，其余一律 Legacy。
pub fn parse_governor_mode(value: Option<&str>) -> GovernorMode {
    match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("on" | "1" | "true") => GovernorMode::On,
        _ => GovernorMode::Legacy,
    }
}

/// Agent 循环 / Turn-Step 生命周期（原 §5.6）。
///
/// `Turn` = 0..n `Step`；`debt` 计数控制续跑；`agent/turn-stopping` 为唯一串行终止点。
pub struct AgentLoop {
    governor: GovernorMode,
}

impl Default for AgentLoop {
    fn default() -> Self {
        Self::new()
    }
}
```

`agent_loop.rs:314-317` 的

```rust
impl AgentLoop {
    pub fn new() -> Self {
        Self
    }
```

替换为

```rust
impl AgentLoop {
    pub fn new() -> Self {
        Self {
            governor: parse_governor_mode(std::env::var("HARNESS_GOVERNOR").ok().as_deref()),
        }
    }

    /// 显式指定治理路径：回放套件与实机 A/B 对照用它绕开进程环境变量。
    pub fn with_governor(mut self, mode: GovernorMode) -> Self {
        self.governor = mode;
        self
    }

    pub fn governor_mode(&self) -> GovernorMode {
        self.governor
    }
```

- [ ] **Step 3: 导出并回归**

`lib.rs:22` 改为：

```rust
pub use agent_loop::{parse_governor_mode, AgentLoop, DeterministicCompaction, GovernorMode};
```

Run: `cargo test -p harness-runtime`
Expected: 全绿。`harness-acp` / `scheduler` / `subagent` / `controller` / `agent_tool_loop.rs` 的 `AgentLoop::new()` 调用点零改动。

Run: `cargo test -p harness-acp -p harness-runtime`
Expected: 全绿（确认跨 crate 构造点未受影响）

- [ ] **Step 4: Commit**

```bash
git add harness/harness-runtime/src/agent_loop.rs harness/harness-runtime/src/lib.rs
git commit -m "feat(governance): AgentLoop 增加 GovernorMode A/B 开关，默认保持旧守卫路径"
```

---

## Task 7: 三处澄清门禁收敛到 ask_user 前置裁决

**Files:**
- Modify: `harness/harness-runtime/src/agent_loop.rs`（`use` 区、`:438` 后建 governor、gate 1 `:458-504`、gate 2 `:517-554`、gate 3 `:563-598`）
- Modify: `harness/harness-runtime/tests/session_replay.rs`（加 `replay_session_with` 过渡）

- [ ] **Step 1: 写失败测试（追加到 `agent_loop.rs` 的 `mod tests`）**

```rust
    #[test]
    fn ask_user_gate_blocks_questions_under_controller() {
        let case = CaseFile::default();
        // legacy（governor=None）一律允许——旧行为逐字保持。
        assert!(ask_user_permitted(None, &case, "改一下", "目标是谁（候选：a、b）"));
        assert!(ask_user_permitted(None, &case, "继续", "目标是谁？"));

        // 控制器：栈顶不满足深度前置。
        let gov = TurnGovernor::new(&case, false, false);
        assert!(
            !ask_user_permitted(Some(&gov), &case, "这个问题解决了吗？", "目标是谁（候选：a、b）"),
            "还有多层策略未试，禁止把负担丢回用户"
        );

        // 降到栈底后仍要拒续跑式回复。
        let mut gov = TurnGovernor::new(&case, false, false);
        gov.stack = StrategyStack::read_only();
        while !gov.stack.allow_ask_user() {
            gov.stack.pop();
        }
        assert!(!ask_user_permitted(Some(&gov), &case, "继续", "目标是谁（候选：a、b）"));
        assert!(ask_user_permitted(Some(&gov), &case, "目标在哪", "目标是谁（候选：a、b）"));
    }

    #[test]
    fn with_candidates_appends_workspace_candidates() {
        let mut goal = GoalContract::default();
        goal.entities = vec!["GitCli".into(), "WorktreeGuard".into()];
        let q = with_candidates("工作区里没找到目标实体", &goal);
        assert!(q.contains("候选："), "{q}");
        assert!(q.contains("GitCli") && q.contains("WorktreeGuard"), "{q}");
        // 无候选可派生时不硬凑：开放模板问题会被 gate 直接拒绝（R2 禁开放模板）。
        let empty = GoalContract::default();
        assert_eq!(with_candidates("问题", &empty), "问题");
    }
```

**前置核对**：`GoalContract` 是否 `Default`、字段是否为 `entities` / `navigation`（`goal_execution.rs:124-135` 用到这两个字段）。若 `GoalContract` 未 derive `Default`，改用 `GoalContract::from_input(&"x".into())` 之类的既有构造并如实报告；若字段名不同，用真实字段名改写这两处测试。**不得为此修改 `GoalContract` 本体。**

Run: `cargo test -p harness-runtime ask_user_gate`
Expected: 编译失败——`ask_user_permitted` / `with_candidates` 未定义。

- [ ] **Step 2: 实现两个 helper（放在 `impl AgentLoop` 之前，模块级私有）**

```rust
/// 控制器模式下澄清提问是否被允许（spec §4.2 三重前置）；Legacy 一律允许，
/// 使 A/B 两条路径的旧行为逐字一致。
fn ask_user_permitted(
    governor: Option<&TurnGovernor>,
    case: &CaseFile,
    input_text: &str,
    question: &str,
) -> bool {
    match governor {
        None => true,
        Some(gov) => gov.ask_user_allowed(case, input_text, question),
    }
}

/// 给问题补上工作区派生的候选列表（R2 硬前置）。无可派生候选时原样返回，
/// 由 `ask_user_permitted` 按「开放模板」拒绝。
fn with_candidates(question: &str, goal: &crate::GoalContract) -> String {
    let mut candidates: Vec<String> = goal.entities.clone();
    candidates.extend(goal.navigation.iter().cloned());
    candidates.sort();
    candidates.dedup();
    if candidates.is_empty() {
        return question.to_string();
    }
    format!("{}（候选：{}）", question, candidates.join("、"))
}
```

`agent_loop.rs` 顶部 `use` 区追加：

```rust
use crate::case_file::CaseFile;
use crate::governor::{Decision, TurnGovernor};
use crate::StrategyStack;
```

（`is_continuation_request` 的 `use` 已在 T4 加过。）

- [ ] **Step 3: 在三处门禁之前构造 governor 与投影**

`agent_loop.rs:438` 的 `let intent = crate::IntentProfile::compile(&task_text);` 之后插入：

```rust
        // 步骤②/④：Case File 是 SessionLog 的只读投影（不构成第二事实源）。
        // 只有控制器模式才建 TurnGovernor；Legacy 下 governor 恒为 None，
        // 所有收敛点都走旧分支。
        let case_file = CaseFile::from_replay(&history);
        let mut governor = (self.governor == GovernorMode::On).then(|| {
            TurnGovernor::new(
                &case_file,
                goal_execution.goal.has_locatable_signal(),
                matches!(intent.kind, crate::IntentKind::Investigation),
            )
        });
```

- [ ] **Step 4: gate 1（`:458-471`）收敛**

把

```rust
            let question = clar.question;
```

与

```rust
            let repeated = is_clarification_reply
                && last_assistant_text(&history).as_deref() == Some(question.as_str());
            if !repeated {
```

改为

```rust
            let question = with_candidates(&clar.question, &goal_execution.goal);
            let repeated = is_clarification_reply
                && last_assistant_text(&history).as_deref() == Some(question.as_str());
            let permitted =
                ask_user_permitted(governor.as_ref(), &case_file, &input_text, &question);
            if !repeated && permitted {
```

（`repeated` 比较的是补候选后的文本，与本回合实际会写出的文案一致，故熔断判据仍然成立。）

- [ ] **Step 5: gate 2（`:517-522`）收敛**

把

```rust
            if let Some(clar) = goal_execution.inspect_for_clarification(root) {
                let question = clar.question;
                let item_id = ledger
```

改为

```rust
            if let Some(clar) = goal_execution.inspect_for_clarification(root) {
                let question = with_candidates(&clar.question, &goal_execution.goal);
                if ask_user_permitted(governor.as_ref(), &case_file, &input_text, &question) {
                let item_id = ledger
```

并在该 `if let Some(clar)` 块的结尾 `return Ok(());`（`:551`）之后、块闭合 `}`（`:552`）之前补一个 `}` 以闭合新加的 `if`（即原块体整体缩进进 `if ask_user_permitted(...)` 内）。

- [ ] **Step 6: gate 3（`:563-566`）收敛**

把

```rust
            if !goal_execution.goal.code_entities.is_empty() && grounding.needs_user_input() {
                let question = grounding.user_question(&goal_execution.goal);
                let item_id = ledger
```

改为

```rust
            if !goal_execution.goal.code_entities.is_empty() && grounding.needs_user_input() {
                let question =
                    with_candidates(&grounding.user_question(&goal_execution.goal), &goal_execution.goal);
                if ask_user_permitted(governor.as_ref(), &case_file, &input_text, &question) {
                let item_id = ledger
```

同样在该块的 `return Ok(());`（`:596`）之后补一个 `}` 闭合新 `if`。

- [ ] **Step 7: 格式化并回归**

Run: `cargo fmt -p harness-runtime && cargo test -p harness-runtime`
Expected: 全绿。重点确认 `agent_tool_loop.rs`（9 个测试覆盖门禁路径）与 `session_replay.rs`（3 passed / 3 ignored）——它们都在 Legacy 下运行，行为必须逐字不变。若 `unused_variable: governor` 报警，说明 T8 尚未使用 `governor` 的可变性：把 `let mut governor` 保留并接受一次 `unused_mut` 警告，或在本步把 Step 8 的冒烟测试一并做完。

- [ ] **Step 8: 控制器模式冒烟测试**

`session_replay.rs`：把现有 `async fn replay_session(fixture: &str) -> Arc<SessionLog>` 整体改名为

```rust
async fn replay_session_with(fixture: &str, mode: GovernorMode) -> Arc<SessionLog> {
```

并把函数体内 `AgentLoop::new()` 改为 `AgentLoop::new().with_governor(mode)`；紧接其后补一层薄委托，使既有 4 个调用点零改动：

```rust
async fn replay_session(fixture: &str) -> Arc<SessionLog> {
    replay_session_with(fixture, GovernorMode::Legacy).await
}
```

`use` 区追加 `use harness_runtime::GovernorMode;`。文件末尾追加：

```rust
/// A/B 冒烟：控制器模式下澄清死循环段仍每回合收尾（不因门禁被拒而挂死），
/// 且不再产生任何"停下交还用户"型结局（R1/R2 的结构性前提）。
#[tokio::test]
async fn governor_mode_terminates_clarification_loop_without_asking() {
    let log = replay_session_with("7ba3370f_t15_18_clarification.jsonl", GovernorMode::On).await;
    let turns = summarize(&log);
    assert_eq!(turns.len(), 4, "四个回合都要走完");
    assert!(r1_violations(&turns).is_empty(), "R1: {:?}", r1_violations(&turns));
    assert!(r2_violations(&turns).is_empty(), "R2: {:?}", r2_violations(&turns));
    assert!(
        turns.iter().all(|t| !matches!(
            t.outcome,
            Some(DeliveryOutcome::NeedsUserInput)
                | Some(DeliveryOutcome::Interrupted)
                | Some(DeliveryOutcome::SystemFailure)
                | Some(DeliveryOutcome::Blocked)
        )),
        "控制器模式出口只有 Delivered / ExhaustedWithArtifact：{:?}",
        turns.iter().map(|t| t.outcome.clone()).collect::<Vec<_>>()
    );
}
```

Run: `cargo test -p harness-runtime --test session_replay`
Expected: `4 passed; 0 failed; 3 ignored`。

- [ ] **Step 9: Commit**

```bash
git add harness/harness-runtime/src/agent_loop.rs harness/harness-runtime/tests/session_replay.rs
git commit -m "feat(governance): 三处澄清门禁收敛到控制器 ask_user 前置裁决，问题补工作区候选"
```

---

## Task 8: R3 成本顶前置 + 控制器观测点驱动的终止

**Files:**
- Modify: `harness/harness-runtime/src/agent_loop.rs`（循环状态区 `:736-737`、循环头 `:764`、Usage 落盘 `:1423-1430`、终止检查点 `:1607`）
- Modify: `harness/harness-runtime/tests/session_replay.rs`

- [ ] **Step 1: 写失败测试**

```rust
/// R3：控制器模式下会话 prompt tokens 必须被前置拦在硬顶之下。
#[tokio::test]
async fn governor_mode_caps_session_prompt_tokens() {
    let log = replay_session_with("7ba3370f_t03_14_symptom.jsonl", GovernorMode::On).await;
    let turns = summarize(&log);
    let total = r3_prompt_total(&turns);
    assert!(
        total <= harness_runtime::PROMPT_CAP,
        "R3 违例：控制器重放累计 prompt={total} > 顶 {}",
        harness_runtime::PROMPT_CAP
    );
}
```

Run: `cargo test -p harness-runtime --test session_replay governor_mode_caps`
Expected: FAIL——症状段真实成本远超 300k，此刻没有任何判顶逻辑。

- [ ] **Step 2: 加回合内控制器状态**

`agent_loop.rs:736` 的 `let mut steps = 0usize;` 替换为

```rust
        let mut steps = 0usize;
        // 控制器模式专属状态。Legacy 下 governor 为 None，这些变量恒为初值且不被读。
        // session_prompt_tokens / last_prompt_tokens：会话累计 prompt 与上一轮实际
        // prompt（R3 增量下界）。必须在每步 Usage 落盘处即时累加——session_case 只在
        // 回合末观测点 absorb，用它判顶会让「一步超顶」和「多步超顶」都不触发。
        let mut session_prompt_tokens = case_file.prompt_tokens;
        let mut last_prompt_tokens = 0u64;
        let mut session_case = case_file.clone();
        let mut case_cursor = history.len();
```

- [ ] **Step 3: 循环头前置判顶**

`agent_loop.rs:764-765` 的

```rust
        while debt > 0 {
            steps += 1;
```

替换为

```rust
        while debt > 0 {
            // R3 前置：到顶就绝不向模型发请求——超顶后唯一去路是收尾交付。
            // 放在 steps 自增与 StepStart 之前，避免留下"开了步却没请求"的空步骤。
            if let Some(gov) = governor.as_ref() {
                if gov.should_stop_before_request(session_prompt_tokens, last_prompt_tokens) {
                    log.append(SessionEvent::Assistant {
                        id: log.gen_id(),
                        chunk: Chunk {
                            text: Some(format!(
                                "【成本顶】会话 prompt tokens {} + 预计增量 {} ≥ 硬顶 {}，停止探索并进入收尾交付。",
                                session_prompt_tokens,
                                last_prompt_tokens,
                                crate::governor::PROMPT_CAP
                            )),
                            ..Default::default()
                        },
                    });
                    break;
                }
            }
            steps += 1;
```

- [ ] **Step 4: Usage 落盘处记录上一轮 prompt**

`agent_loop.rs:1423-1430` 的

```rust
            if step_usage.total_tokens > 0 {
                log.append(SessionEvent::Usage {
```

替换为

```rust
            if step_usage.total_tokens > 0 {
                // 判顶前置需要「上一轮实际发了多少 prompt」与「会话累计 prompt」，
                // 二者都必须在 step_usage 被 move 进事件之前取。
                last_prompt_tokens = step_usage.prompt_tokens;
                session_prompt_tokens += step_usage.prompt_tokens;
                log.append(SessionEvent::Usage {
```

- [ ] **Step 5: 步末观测与唯一终止**

`agent_loop.rs:1607-1612` 的

```rust
            // 唯一终止检查点（serial，无 next()）。
            let stop = bus
                .serial(TurnStopping {
                    will_stop: debt == 0,
                })
                .await;
```

替换为

```rust
            // 控制器观测点：把本步新事件折叠进投影，由 TurnGovernor 给出唯一决策。
            // 换路/降级只注入提示（旧守卫并行存续，其退位属步骤⑤）；
            // Terminate 是全系统唯一的回合终止来源（spec §4.1 G1）。
            if let Some(gov) = governor.as_mut() {
                let (next_cursor, fresh) = log.replay_from(case_cursor);
                case_cursor = next_cursor;
                session_case.absorb(&fresh);
                match gov.observe(&session_case, steps, execution.write_operations) {
                    Decision::SwitchStrategy => messages.push(Message::user(format!(
                        "[换路] 本窗口零增益，策略已切换至 {}。禁止重复 case file 中已尝试过的 {} 次调用。",
                        gov.strategy_hint(),
                        session_case.tried.len()
                    ))),
                    Decision::Degrade => messages.push(Message::user(
                        "[降至栈底] 请交付可验证的子目标：停止扩大探索，把已确认的部分整理为结构化交付（已完成、证据锚点、未完成原因、下一步）。",
                    )),
                    Decision::Terminate(_) => break,
                    Decision::Continue => {}
                }
            }
            // 唯一终止检查点（serial，无 next()）。
            let stop = bus
                .serial(TurnStopping {
                    will_stop: debt == 0,
                })
                .await;
```

- [ ] **Step 6: 运行**

Run: `cargo test -p harness-runtime --test session_replay`
Expected: `5 passed; 0 failed; 3 ignored`（R3 冒烟此刻应已转绿）

Run: `cargo test -p harness-runtime`
Expected: 全绿（Legacy 路径逐字不受影响）

若 R3 冒烟仍失败：检查 `session_prompt_tokens` 是否在每步 Usage 落盘时累加（`step_usage` 会被 `log.append` move，累加必须在 move 之前）；再确认循环头判顶读的就是它。**禁止**通过把断言改成 `>=` 之类的反向比较"修绿"。

- [ ] **Step 7: Commit**

```bash
git add harness/harness-runtime/src/agent_loop.rs harness/harness-runtime/tests/session_replay.rs
git commit -m "feat(governance): R3 成本顶前置与控制器观测点作为唯一回合终止来源"
```

---

## Task 9: 出口收口为 Delivered / ExhaustedWithArtifact + R4 资产落盘

**Files:**
- Modify: `harness/harness-runtime/src/agent_loop.rs:1632-1694`（outcome 链 + 收口块）
- Modify: `harness/harness-runtime/tests/session_replay.rs`（加强度量器 + 冒烟测试）

- [ ] **Step 1: 写失败测试**

`session_replay.rs` 在度量器区（`a2_max_cross_turn_repeat` 之后）追加：

```rust
/// R4 加强判据：非 Verified 回合的助手文本必须含 `artifact_text` 的四要素标记。
/// 这是「红线跑绿不是因为检查变宽」的反作弊锁（spec §7 失败回合 100% 带 artifact）。
fn missing_artifact_violations(turns: &[TurnSummary]) -> Vec<String> {
    turns
        .iter()
        .enumerate()
        .filter(|(_, t)| t.outcome != Some(DeliveryOutcome::Verified))
        .filter(|(_, t)| {
            !(t.assistant_text.contains("锚点：")
                && t.assistant_text.contains("假设：")
                && t.assistant_text.contains("补丁建议：")
                && t.assistant_text.contains("问项："))
        })
        .map(|(i, t)| format!("turn {} outcome={:?} 缺四要素资产", i + 1, t.outcome))
        .collect()
}
```

文件末尾追加：

```rust
/// R4（加强）：控制器模式下每个非 Verified 回合都要带结构化资产。
#[tokio::test]
async fn governor_mode_every_non_verified_turn_carries_artifact() {
    for fixture in [
        "7ba3370f_t03_14_symptom.jsonl",
        "7ba3370f_t15_18_clarification.jsonl",
        "7ba3370f_t19_22_gitfix.jsonl",
    ] {
        let log = replay_session_with(fixture, GovernorMode::On).await;
        let turns = summarize(&log);
        let missing = missing_artifact_violations(&turns);
        assert!(missing.is_empty(), "{fixture} 缺资产：{missing:?}");
    }
}
```

Run: `cargo test -p harness-runtime --test session_replay governor_mode_every_non_verified`
Expected: FAIL——收口块尚不存在，失败回合的 reason 仍是旧文案、无四要素。

- [ ] **Step 2: outcome 链改名（唯一改动是绑定名）**

`agent_loop.rs:1633` 的

```rust
        let (outcome, reason) = if delivery_verified {
```

改为

```rust
        let (raw_outcome, raw_reason) = if delivery_verified {
```

原链的其余分支一字不改。

- [ ] **Step 3: 插入收口块**

在 outcome 链结束的 `};`（`:1690`）与原 `log.append(SessionEvent::Delivery {`（`:1691`）之间插入：

```rust
        // 出口收口（spec §4.2）：控制器模式下只剩两个出口。Verified 即 Delivered；
        // 用户取消保持 Cancelled（强行改判会剥夺用户的取消语义，且它不是治理失败）；
        // 其余一律收敛为 PartialDelivery + R4 四要素资产，且资产以 Assistant 事件
        // 对用户可见——「失败也留资产」若不落到会话流就等于没留。
        let (outcome, reason) = if self.governor == GovernorMode::On
            && !matches!(
                raw_outcome,
                DeliveryOutcome::Verified | DeliveryOutcome::Cancelled
            ) {
            let (next_cursor, fresh) = log.replay_from(case_cursor);
            case_cursor = next_cursor;
            session_case.absorb(&fresh);
            let mut final_case = session_case.clone();
            if let Some(gov) = governor.as_ref() {
                final_case.eliminated.extend(gov.eliminated().iter().cloned());
            }
            let hint = goal_execution.next_action_hint();
            let candidate = terminal_reason
                .as_deref()
                .map(|r| format!("是否按以下理解继续：{r}"));
            let artifact = artifact_text(
                &final_case,
                raw_reason.as_deref().unwrap_or(""),
                &hint,
                candidate.as_deref(),
            );
            log.append(SessionEvent::Assistant {
                id: log.gen_id(),
                chunk: Chunk {
                    text: Some(artifact.clone()),
                    ..Default::default()
                },
            });
            (
                DeliveryOutcome::PartialDelivery,
                Some(format!(
                    "{artifact}\n原始结论：{}",
                    raw_reason.unwrap_or_else(|| format!("{raw_outcome:?}"))
                )),
            )
        } else {
            (raw_outcome, raw_reason)
        };
```

`agent_loop.rs` 顶部 T7 加的 `use crate::governor::{Decision, TurnGovernor};` 扩为：

```rust
use crate::governor::{artifact_text, Decision, TurnGovernor};
```

**编译注意**：`goal_execution.next_action_hint()` 返回 `String`（`agent_loop.rs:1598` 处它被直接内插进 `format!`），故 `&hint` 满足 `&str` 形参；若实际返回 `Option<String>`，改成 `raw_reason.as_deref().unwrap_or("")` 同法的 `hint.as_deref().unwrap_or("")` 并在报告中写明实际签名。

- [ ] **Step 4: 运行**

Run: `cargo test -p harness-runtime --test session_replay`
Expected: `6 passed; 0 failed; 3 ignored`

Run: `cargo test -p harness-runtime`
Expected: 全绿

若资产断言失败：先看 `final_case.anchors` 是否为空（空则 `artifact_text` 写占位、仍应含四要素）；再确认收口块确实在 `On` 分支执行（`self.governor` 与 `governor.is_some()` 必须同源于 T6/T7 的同一判据，不要引入第二处开关读取）。**禁止**通过删掉某个要素来"修绿"。

- [ ] **Step 5: Commit**

```bash
git add harness/harness-runtime/src/agent_loop.rs harness/harness-runtime/tests/session_replay.rs
git commit -m "feat(governance): 出口收口为 Delivered/ExhaustedWithArtifact 并落 R4 四要素资产"
```

---

## Task 10: 红线解除封存、A/B 对照与阶段收尾

**Files:**
- Modify: `harness/harness-runtime/tests/session_replay.rs`
- Modify: `docs/superpowers/specs/2026-08-31-agent-governance-redesign-design.md:4`
- Modify: `docs/superpowers/plans/2026-09-01-governance-phase2-case-file-and-controller.md`（追加实施记录）

- [ ] **Step 1: 度量器常量单点化**

`session_replay.rs` 删除测试内的

```rust
const PROMPT_CAP: u64 = 300_000;
```

并把所有 `PROMPT_CAP` 引用改为 `harness_runtime::PROMPT_CAP`（`use harness_runtime::PROMPT_CAP;` 亦可，与既有 `use harness_runtime::{GovernorMode, ...}` 合并）。阶段 1 的 `fn is_continuation` 保留（它是度量器自己的口径，测试内没有控制器函数的副本需要删除），只在其上方补一行注释：「控制器侧的续跑判据是 `harness_runtime::is_continuation_request`，两者必须保持同一前缀表；改一处必改另一处」。

- [ ] **Step 2: 三条红线测试解除封存并切到控制器模式**

把 `red_lines_clarification_loop` / `red_lines_symptom_task` / `red_lines_gitfix` 三个测试的 `#[ignore = "..."]` 整行删除，`replay_session(...)` 改为 `replay_session_with(..., GovernorMode::On)`，并各追加资产断言。三者的最终形态：

```rust
/// 澄清死循环段（turn 15–18）：R1 + R2 + R4 + 资产锁。控制器模式正式门禁（步骤④已接管）。
#[tokio::test]
async fn red_lines_clarification_loop() {
    let log = replay_session_with("7ba3370f_t15_18_clarification.jsonl", GovernorMode::On).await;
    let turns = summarize(&log);
    assert_eq!(turns.len(), 4);
    let (r1, r2, r4) = (r1_violations(&turns), r2_violations(&turns), r4_violations(&turns));
    assert!(r1.is_empty(), "R1 违例: {r1:?}");
    assert!(r2.is_empty(), "R2 违例: {r2:?}");
    assert!(r4.is_empty(), "R4 违例: {r4:?}");
    let missing = missing_artifact_violations(&turns);
    assert!(missing.is_empty(), "R4 资产缺失: {missing:?}");
}

/// 症状任务段（turn 3–14）：R1 + R3 + R4 + A1 + 资产锁 + A2 的 A/B 对照。
#[tokio::test]
async fn red_lines_symptom_task() {
    let log = replay_session_with("7ba3370f_t03_14_symptom.jsonl", GovernorMode::On).await;
    let turns = summarize(&log);
    assert_eq!(turns.len(), 12);
    let (r1, r4) = (r1_violations(&turns), r4_violations(&turns));
    let tokens = r3_prompt_total(&turns);
    let a1 = a1_guard_trips(&turns);
    assert!(r1.is_empty(), "R1 违例: {r1:?}");
    assert!(tokens <= harness_runtime::PROMPT_CAP, "R3 违例: prompt={tokens} > 顶");
    assert!(r4.is_empty(), "R4 违例: {r4:?}");
    assert!(a1 <= 12, "A1 违例: 守卫/熔断触发 {a1} > 12");
    let missing = missing_artifact_violations(&turns);
    assert!(missing.is_empty(), "R4 资产缺失: {missing:?}");

    // A2 用 A/B 对照而非绝对值：回放里的模型是录制脚本，其跨轮重复调用是
    // 原会话既成事实，任何控制器都无法在回放中改变它。绝对 A2 门禁属实机验收
    // （spec §6 三场景），此处只断言控制器不把重复问题放大。
    let legacy = summarize(&replay_session("7ba3370f_t03_14_symptom.jsonl").await);
    let (a2_on, a2_legacy) = (a2_max_cross_turn_repeat(&turns), a2_max_cross_turn_repeat(&legacy));
    assert!(
        a2_on <= a2_legacy,
        "A2 退化：控制器模式跨轮重复 {a2_on} > 旧守卫 {a2_legacy}"
    );
}

/// git 修复段（turn 19–22）：R4 + 资产锁（edit matched-0 / length 截断回合也要留资产）。
#[tokio::test]
async fn red_lines_gitfix() {
    let log = replay_session_with("7ba3370f_t19_22_gitfix.jsonl", GovernorMode::On).await;
    let turns = summarize(&log);
    assert_eq!(turns.len(), 4);
    let r4 = r4_violations(&turns);
    assert!(r4.is_empty(), "R4 违例: {r4:?}");
    let missing = missing_artifact_violations(&turns);
    assert!(missing.is_empty(), "R4 资产缺失: {missing:?}");
}
```

- [ ] **Step 3: 成功会话双模式对照**

保留 `success_session_replay_keeps_verified`（Legacy），并追加：

```rust
/// A/B 对照：控制器接管不得把健康会话跑坏。
#[tokio::test]
async fn governor_mode_success_session_still_verifies() {
    let log = replay_session_with("success_677bd6e0.jsonl", GovernorMode::On).await;
    let turns = summarize(&log);
    assert!(
        turns.iter().any(|t| t.outcome == Some(DeliveryOutcome::Verified)),
        "成功会话在控制器模式下应仍有 Verified 交付: {turns:?}"
    );
    assert!(
        r3_prompt_total(&turns) <= harness_runtime::PROMPT_CAP,
        "健康会话也不应突破成本顶: {}",
        r3_prompt_total(&turns)
    );
}
```

Run: `cargo test -p harness-runtime --test session_replay -- --test-threads=1`
Expected: `9 passed; 0 failed; 0 ignored`

若某条红线此刻仍红：这是**真实发现**，必须原样上报并保留红灯，禁止放宽断言或改回 `#[ignore]`。逐条定位方法：R1/R2 红 → 检查是否还有第四处未收敛的 NeedsUserInput 出口（`grep -n "DeliveryOutcome::NeedsUserInput" agent_loop.rs` 只应出现在 gate1-3 与 outcome 链的 `raw_*` 分支）；R3 红 → 判顶分支没生效；R4/资产红 → 收口块分支未进入；A2 退化 → `case.tried` 未累积（`replay_from` 游标问题）。

- [ ] **Step 4: 全局验证**

```bash
cd harness
cargo test --workspace 2>&1 | tail -40
cargo clippy -p harness-runtime --all-targets 2>&1 | tail -20
```
Expected: workspace 全绿；clippy 无新增 error（warning 若与本次改动无关可记录后放行）。

- [ ] **Step 5: 实机双跑对照（spec §5 步骤④的硬性验收项）**

回放 mock 保真 ≠ 实机行为（spec §8 风险 1），故步骤④必须在实机上对照一次同场景。用打包交付物跑，不用 debug 产物：

```bash
cd harness
# 旧守卫基线
cargo build --release            # 按项目既有打包入口产出 dist/aidops-desktop.exe
# 记录：会话「消除 git 子进程黑框 + 非仓库工作区报错降级」跑完后，
#   .harness/sessions/<id>.jsonl 的 outcome 分布、prompt tokens 合计、用户消息条数
# 然后切控制器再跑同一输入序列：
HARNESS_GOVERNOR=on ./目标平台打包命令
```

对照表（把实测值写进 Step 7 的实施记录，任一恶化都要作为发现上报，不得静默放行）：

| 指标 | Legacy 实测 | Governor 实测 | 期望 |
|---|---|---|---|
| 用户消息条数（含"继续"） | ? | ? | Governor 侧应为 1（无续跑） |
| `NeedsUserInput` / `Interrupted` / `SystemFailure` 计数 | ? | ? | Governor 侧应为 0 |
| 会话 prompt tokens 合计 | ? | ? | Governor 侧 ≤ 300k |
| 非 Verified 回合四要素资产覆盖率 | ? | ? | Governor 侧 100% |
| 步数 / 工具调用数 | ? | ? | 不显著恶化（策略提示与旧续期提示共存的代价） |
| git 子进程黑框 | ? | ? | 两种模式都无（阶段 1 T7 契约，与开关无关） |

实机不可用时（无 GUI 环境/无该工作区权限）：本步标记为 SKIPPED 并写入实施记录的「遗留清单」第 1 条，**不得**据此声称步骤④完成——此时 spec 状态行只能写「④已实施待实机对照」。

- [ ] **Step 6: 更新 spec 状态行**

`docs/superpowers/specs/2026-08-31-agent-governance-redesign-design.md:4` 改为（Step 5 若为 SKIPPED，则把「④（新控制器接管决策，A/B 默认 Legacy）已实施」改写为「④已实施，实机对照待补」，其余不变）：

```markdown
- 状态：设计已获批；步骤①（回放套件）、②（case file 并联与保真对拍）、③（工具层契约）、④（新控制器接管决策，A/B 默认 Legacy）已实施，红线门禁已解除封存并全绿；待步骤⑤（旧计数器退位删除、实机三场景验收）
```

- [ ] **Step 7: 在本计划文件末尾追加「阶段 2 实施记录」**

记录以下实测证据（不是模板，必须填真实数值）：

```markdown
---

## 阶段 2 实施记录（2026-09-01）

- 红线门禁：`cargo test -p harness-runtime --test session_replay -- --test-threads=1` → N passed / 0 failed / 0 ignored（附实际输出）
- 跑绿证据：R1/R2/R3/R4 各自在控制器模式下为空的实测计数；A1 = ?，A2 On = ? vs Legacy = ?
- R3 实测：控制器模式症状段累计 prompt = ? / 顶 300000；旧守卫模式 = ?（对照）
- 保真对拍：case_file_fidelity 6 项结果，含 full fixture 读出的真实 prompt tokens
- 实机双跑对照（Step 5）：Legacy vs Governor 的六项指标实测值；若 SKIPPED，写明阻塞原因
- 遗留与实机清单（转交步骤⑤/阶段 3）：
  1. 实机三场景（症状任务 / 续跑澄清 / git 报错修复）完整跑通——Step 5 只对照了 1 个场景；
  2. 非 Windows 平台的 git 子进程卫生（阶段 1 的 CREATE_NO_WINDOW 只覆盖 Windows；Linux/macOS 侧由 harness-provider-sandbox 负责，需在对应平台实机确认）；
  3. 策略切换提示（[换路] / [降至栈底]）与旧 `[自动接续]` / `[强制收敛]` 提示的共存效果——退位删除属步骤⑤；
  4. A2 绝对门禁（≤2）在实机三场景上复核；
  5. 若 spec §5 步骤② 的 eliminated 保真需要跨会话持久，再评估是否落 `.harness/case.json`（本阶段刻意不落第二份文件）。
```

- [ ] **Step 8: Commit 并推送（推送前需用户确认）**

```bash
git add docs/superpowers/plans/2026-09-01-governance-phase2-case-file-and-controller.md docs/superpowers/specs/2026-08-31-agent-governance-redesign-design.md
git commit -m "docs(governance): 归档阶段 2 实施记录与红线全绿证据"
```

向用户复述结果并取得确认后再 `git push origin main`。

---

## 已知风险与校准预案

| 风险 | 触发信号 | 预案 |
|---|---|---|
| 门禁收敛后 `agent_tool_loop.rs` 某测试行为漂移 | T7 Step 7 红灯 | 该测试大概率在断言"该问就问"；控制器模式默认关闭（Legacy），故失败必然来自 Legacy 分支被误改——回看 Step 4-6 的 `if permitted {` 是否吃掉了 Legacy 走的分支 |
| `GoalContract` 无 `Default` 或字段名不同 | T7 Step 1 编译失败 | 按测试内「前置核对」改用既有构造/真实字段，不改库代码 |
| `next_action_hint()` 返回类型与 T9 假设不符 | T9 Step 3 编译失败 | 按 T9 编译注意里的两种写法择一，并在 commit message 里记录实际签名 |
| 澄清段在 On 模式下挂死（门禁被拒后模型请求永不收敛） | T7 Step 8 超时 | 回放脚本耗尽时 `ReplayLlm` 恒发收敛文本，理论上不挂；若挂，检查 `debt` 是否被旧续期分支无限复活（此时属真实缺陷，上报） |
| `full` fixture 无 `Usage` 事件 → R3 在回放中不可验 | T5 Step 2 最后一条断言失败 | 按 T5 Step 2 说明：把限制写入 spec §5，R3 转实机验证；不要伪造 token |
| 策略切换提示与旧续期提示互相干扰，步数上升 | T10 Step 3 `a2_on > a2_legacy` 或 R3 逼近顶 | 记录实测；若确属干扰，把 T8 Step 5 的注入改为「仅当旧守卫本轮未注入提示时才注入」的单发闸门，并在实施记录写明 |

## 阶段 2 完成定义

1. `cargo test -p harness-runtime --test session_replay -- --test-threads=1` → **9 passed / 0 ignored**（三条红线在控制器模式下绿，且资产锁生效）。
2. `cargo test --workspace` 全绿；`GovernorMode::Legacy`（默认）行为与阶段 1 逐字一致，实机可随时回滚。
3. Case File 对真实会话日志的保真对拍 6 项全绿（步骤②验收）。
4. 每条红线为何变绿都有可解释的机制（出口收口 / 成本顶前置 / asked 去重），且新增的 `missing_artifact_violations` 锁排除了"检查变宽导致的假绿"。
5. spec 状态行与实施记录已更新，实机清单已移交步骤⑤。

---

## 阶段 2 实施记录（2026-09-01，inline 执行）

子代理在本平台不可用，改用 executing-plans 逐 Task TDD 内联执行；每个 Task 先红后绿、独立提交。

- 红线门禁：`cargo test -p harness-runtime --test session_replay -- --test-threads=1` → **8 passed / 0 failed / 0 ignored**（三红线已解 `#[ignore]` 切 On 模式）。
- 跑绿证据（实测）：症状段 On 累计 prompt = **294,928**（判顶生效，卡在顶下），Legacy = **1,561,542**（远超顶，自证 replay 复现了真实成本）；A1 = 1（≤ 12）；A2 On = 2 = Legacy = 2（不退化）；R1/R2/R4 违例集均空；`missing_artifact_violations` 三 fixture 均空（非 Verified 回合 100% 带四要素资产）。
- 保真对拍：`case_file_fidelity` 6 项全绿；full fixture 读出真实 prompt tokens > 300k，关闭阶段 1 的 R3 敏感性疑虑。
- 全库：`cargo test --workspace` 42 结果行全 ok、0 失败；`cargo clippy -p harness-runtime --all-targets` 0 error。
- 提交序列：T1 125087d / T2 08b95f8 / T3 74a3f10 / T4 55e1199 / T5 b12e286 / T6 4dac4af / T7 e0accb7 / T8 9a33924 / T9 b6629c6 / T10（本次）。

### 执行中偏离计划之处（均已回归测试兜底）
1. **T1 设计缺陷修正**：`asked` 的回合助手文本改用投影内 `turn_text` 缓冲结算，而非对事件切片反向找 `TurnStart`——全量单次 fold 的切片含后续回合时回溯法会把澄清文本挂错回合（T1 单元测试跑红暴露）。
2. **T2 构造方向修正**：`FULL` 常量原按 spec 文字顺序把栈顶帧写在 vector 头部，与 `current()/pop()` 取尾部相反，测试跑红暴露；改为「栈底在头、栈顶在尾」，`for_task(false)` 取 `FULL[..5]`。
3. **T4 测试断言修正**：计划版 `positive_gain` 用例把「基线重置」误预期为给当前策略续命，与零增益即换路的控制器语义矛盾；改为验证「旧锚点被基线吸收后不重复计增益」。
4. **T5/T7 GoalContract 无 Default**：测试改用既有 `compile()` 构造后显式设字段（计划已预见）。
5. **T7 冒烟断言边界收缩**：计划把「出口无 SystemFailure」放在 T7，但 outcome 收口是 T9 职责——四回合以 SystemFailure 收尾正是待收口的旧终态；T7 冒烟收缩为其真正该验的（不挂死 + 无门禁复读文案）。
6. **T8 重大保真缺口修正（阶段 1 遗留）**：真实 token 成本只记录在独立 `Usage` 事件（Assistant chunk 不带 usage），阶段 1 驱动器完全丢弃 → 重放 token 恒为 0、R3 与判顶逻辑无从验证、R3 测试假绿。现按请求序把录制的 `Usage` 注入 `ReplayLlm` 响应；判顶增量下界 `last_prompt` 回合起点取历史末条 `Usage.prompt`（上下文单调）非 0，否则每回合首请求无条件发出会一步跨顶（实测修正前 339,934 > 顶）。R3 测试改为自证式 A/B。
7. **T10 断言合并**：T8/T9 的独立 `governor_mode_caps` / `every_non_verified` 测试断言已折进解除封存的 `red_lines_*`（症状段复用同一次 Legacy 基线同时证 R3 超顶与 A2 不退化），删除重复测试避免对 1.4MB/4.7MB fixture 的多次昂贵重放。

### 遗留与实机清单（转交步骤⑤ / 阶段 3）
1. 实机双跑对照（spec §5 步骤④硬性验收，六项指标）：本机无打包实机环境，**未执行**，标 SKIPPED；步骤④完成宣告以「回放四红线全绿」为据，实机对照待补。
2. gate 触发的 ask_user（On 模式满足三重前置时）仍走 NeedsUserInput 早返回，不带四要素资产——其 reason 带 `CLARIFICATION_REASON_PREFIX`，R4 度量器会要求锚点。当前四 fixture 均不触发该路径（续跑/开放模板被拒）；实机若出现需补「gate 问题也带候选锚点」。
3. 策略切换提示 `[换路]/[降至栈底]` 与旧 `[自动接续]/[强制收敛]` 提示在 On 模式共存（旧守卫未退位），步数成本略升——退位删除属步骤⑤。
4. 绝对 A2 ≤ 2 门禁转实机三场景复核。
5. `stack_pos` 未进 CaseFile 投影（回合级状态，SessionLog 无源派），需写回 spec §4.3。
