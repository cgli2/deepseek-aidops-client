# 阶段 3（绞杀者步骤⑤）：旧计数器退位删除 + 实机遗留治理 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:executing-plans（子代理不可用时 inline 执行）。步骤用 `- [ ]` 复选框跟踪。

**Goal:** 让新控制器成为唯一回合路径——provider 错误假绿收口、A/B 默认切 On、A2 按意图细化、stack_pos 投影写回 spec、随后删除全部旧守卫计数器与 Legacy 分支、收缩冗余、文档标记 deprecated，实机复跑六项全绿收官。

**Architecture:** 先修行为缺口（T1 假绿 / T2 默认翻转 / T3 度量误报 / T4 投影缺口），每步回放 8/8 + 全量回归守门；行为等价性被实机与回放双重证明后，才执行 T5/T6 的大删除（旧守卫、6 计数器、GovernorMode::Legacy 整条路径），最后 T7 文档退位、T8 实机复跑与推送。

**Tech Stack:** Rust（harness-runtime 为主）、Python（红线度量/编排器，`python -X utf8`）、ACP 无头编排（`--acp`）、cargo（在 `harness/` 下，MSVC：`scripts/build.bat`）。

---

## 权威规格与既有交付

- spec：`docs/superpowers/specs/2026-08-31-agent-governance-redesign-design.md` §4.1（传感器映射表）、§5 步骤⑤、§6 完成判据（「6 个旧计数器代码删除；单会话 ≤300k；失败回合 100% 带 artifact」）。
- 阶段 2 已完成且实机红线级验收通过（`2026-09-01-governance-phase2-onsite-ab-checklist.md` 判读）；本计划消化其全部遗留输入：②gate ask_user 带锚点、③提示共存退位、④stack_pos、⑤A2 细化、provider 假绿缺陷、A/B 默认切 On。

## 已核实代码事实（执行前无需再探索；行号为 2026-09-01 实测）

1. **假绿根因**：`agent_loop.rs:1051-1064` 流 `Err(e)` → 只 append `[error] {e}` Assistant 文本 + `hard_stop=true`；该文本被当成模型最终回答 → `raw_outcome=Verified`；出口收口块（`:1833`）对 Verified 不介入。**修复=让 provider 错误终止置 SystemFailure**。
2. A/B 分支点仅两处实体：`:525`（TurnGovernor 构造）、`:1833`（出口收口条件）；`parse_governor_mode` 在 `:40-44`（默认 Legacy）。
3. 旧守卫块：`[强制收敛]/[自动接续]` `:1674-1690`（`MAX_STAGNANT_WINDOWS=3` `:854`）；`[换路]/[降至栈底]` 控制器提示 `:1726-1739` **保留**；旧空响应/循环恢复 `[error]` 终止分支 `:1598`、`:1612`；`ToolRepeatGuard`（agent_loop.rs 内联，`:310`/`:832` 区域）。
4. 计数器：`goal_execution.rs` `no_information_count`/`correction_count` 字段 `:517-518`、breaker `:1466-1525`、测试 `:2146-2172`；`execution.rs` `budget.stagnant_windows` `:829-1005`、测试 `:1558-1608`；`blocked_count` 仅 `task_ledger.rs:126` 统计 → telemetry 映射（`agent_loop.rs:1963`）——**属账本遥测，不删**；spec 第六个退位对象 = target-anchor 门禁的硬停用法（已按阶段2收敛为传感器，删除其残留终止判断）。
5. 回放套件：`tests/session_replay.rs` 8 测试全绿；`replay_session_with(fixture, GovernorMode::On)`；`red_lines_symptom_task` 内含 Legacy 对照（`:495`）；`fixtures` 下 5 个 jsonl。
6. 度量双侧：`session_replay.rs::a2_max_cross_turn_repeat` 与 `harness/scripts/governance_redline_check.py`（同源语义，改一处必改另一处）。
7. ACP：`bin/src/main.rs` 已接线；`aidops-desktop.exe --acp`；settings 表结构/密文 BLOB 约定见阶段 2 收尾（Python 写 settings 必须 bytes）。
8. 待 deprecated 文档：`docs/AGENT_GOAL_EXECUTION_FRAMEWORK_V3.md`、`AGENT_GOAL_SOLVING_MECHANISM_V4.md`、`AGENT_GOAL_SOLVING_MECHANISM_V5.md`、`AGENT_DELIVERY_REFORM_PLAN.md`、`agent-loop-interruption-root-cause-and-fix.md`、`intent-clarification-gate-signal-driven-adr.md`。
9. 工作区是 git 仓（main，已同步 origin）；**cargo 一律在 `harness/` 下跑**；GUI profile 下会话落 `<project>/.harness/sessions/`；重建用 `MSYS_NO_PATHCONV=1 cmd /c "scripts\\build.bat package"`。

## 决策边界

- 不新增任何独立守卫/终态；回合终止来源保持唯一（控制器观测点 + 用户取消 + 明确 provider/系统错误）。
- T5 删除前，T1–T4 必须让回放与实机双绿，作为「行为已被新路径覆盖」的证明；删除只做搬运删除，不改控制器逻辑。
- `[需要澄清]` 门禁合成文本、ReplayLlm 耗尽回退 chunk 文本**不得改动**（回放收敛依赖）。
- SessionEvent schema、LLM provider 接口、harness-ui 不动（spec 范围条款）。

## 文件结构

| 文件 | 动作 | 职责 |
|---|---|---|
| `harness-runtime/src/agent_loop.rs` | 修改 | T1 错误收口、T5 守卫块删除、GovernorMode 移除 |
| `harness-runtime/tests/agent_tool_loop.rs` | 修改 | T1 失败测试（ErrorLlm） |
| `harness-runtime/tests/session_replay.rs` | 修改 | T3 A2 分类、T5 模式参数清理 |
| `harness/scripts/governance_redline_check.py` | 修改 | T3 A2 同步细化 |
| `harness-runtime/src/governor/strategy.rs` | 修改 | T4 栈位暴露（经遥测字段） |
| `harness-runtime/src/case_file.rs` | 修改 | T4 current_strategy 投影 |
| `harness-runtime/src/goal_execution.rs` / `execution.rs` | 修改 | T5 计数器删除、T6 冗余收缩 |
| `docs/*.md`（8 项见事实 8） | 修改 | T7 deprecated 头 |
| spec / checklist / 本计划 | 修改 | 状态回写 |

---

### Task 1: provider 错误收口（假绿修复，双模式生效）

**Files:** Modify `harness-runtime/src/agent_loop.rs`（`:1051` 区域 + 回合收尾 outcome 判定）、`harness-runtime/tests/agent_tool_loop.rs`

- [ ] **Step 1: 失败测试** — 在 `agent_tool_loop.rs` 追加（ctx 样板逐字取自本文件 `tool_result_is_sent_back_and_turn_finishes`（`:258-285`），已核实 `Error::Llm(String)` 存在于 `harness-core/src/error.rs:18`）：

```rust
/// 流内直接吐 Err 的 Provider：复现「4xx 报错被当成最终回答」的假绿场景。
struct ErrorLlm;
#[async_trait]
impl LlmProvider for ErrorLlm {
    fn name(&self) -> &'static str { "error-test" }
    fn tools(&self) -> Vec<harness_llm::ToolSchema> { vec![] }
    fn stream(&self, _m: Vec<Message>) -> ChunkStream {
        Box::pin(futures::stream::iter(vec![Err(harness_core::error::Error::Llm(
            "http 403 AccountOverdueError".into(),
        ))]))
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
        .run_turn(&ctx, UserInput { text: "hi".into(), attachments: vec![] })
        .await; // 错误已内化为收口，不再上抛；两可（Ok/Err）都不许 Verified

    let outcomes: Vec<_> = log.replay().into_iter().filter_map(|e| match e {
        SessionEvent::Delivery { report, .. } => Some(report.outcome),
        _ => None,
    }).collect();
    assert_eq!(outcomes.len(), 1);
    assert!(!matches!(outcomes[0], DeliveryOutcome::Verified),
        "provider 错误不得交付 Verified，实际 {:?}", outcomes[0]);
    let texts: Vec<String> = log.replay().into_iter().filter_map(|e| match e {
        SessionEvent::Assistant { chunk, .. } => chunk.text,
        _ => None,
    }).collect();
    assert!(texts.iter().any(|t| t.contains("[error]")), "错误须对用户可见: {texts:?}");
}
```

（导入名以该文件既有 `use` 为准，缺 `DeliveryOutcome` 则从 `harness_session` 补 import。）

- [ ] **Step 2: 跑红** — `cargo test -p harness-runtime --test agent_tool_loop provider_error`；Expected: FAIL（当前 Verified）。
- [ ] **Step 3: 实现** — `agent_loop.rs` 流 Err 分支（`:1051-1064`）：append `[error] {e}` Assistant 事件后，除 `hard_stop = true` 外置 `provider_error_seen = true`（回合级新标志，局部 `let mut`），并在收尾 outcome 判定处（`raw_outcome` 产生点之前）：若 `provider_error_seen` 且 `raw_outcome` 将取 Verified，则改写 `raw_outcome = DeliveryOutcome::SystemFailure`、`raw_reason = Some("llm provider error（已终止流读取）: {e 摘要}")`。出口收口块（`:1833`）因此自动产出 PartialDelivery + 四要素资产（On）或 SystemFailure+reason（Legacy 残存期）。摘要文本经新标志携带 `e.to_string()`。
- [ ] **Step 4: 跑绿 + 全量** — `cargo test -p harness-runtime --test agent_tool_loop && cargo test -p harness-runtime --test session_replay && cargo test -p harness-runtime`；Expected: 全绿（session_replay 8/8 不变）。
- [ ] **Step 5: Checkpoint + Commit** — `test(governance): provider 流错误收口为 SystemFailure，杜绝假绿 Verified`。

### Task 2: A/B 默认切 On（保留 legacy 逃生门一个阶段）

**Files:** Modify `agent_loop.rs:40-44`、`lib.rs` 导出、`agent_loop.rs` 内 `parse_governor_mode` 测试（`:2934-2945`）

- [ ] **Step 1: 改语义（先写测试）** — 更新既有测试断言：`parse_governor_mode(None) == On`；`Some("legacy"|"off"|"0") => Legacy`；其余一律 On。实现：

```rust
/// 解析 `HARNESS_GOVERNOR`：默认控制器接管（On）；仅显式 legacy/off/0 回退旧路径
/// （步骤⑤删除 Legacy 前保留一个阶段的逃生门）。
pub fn parse_governor_mode(value: Option<&str>) -> GovernorMode {
    match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("legacy" | "off" | "0") => GovernorMode::Legacy,
        _ => GovernorMode::On,
    }
}
```

- [ ] **Step 2: 回放适配** — `session_replay.rs`：`replay_session(fixture)` 默认改传 `GovernorMode::On`；`red_lines_*` 保持 On；`clarification_loop_replay_emits_delivery_per_turn` 与 Legacy 对照断言（`:495` 区域）显式改为 `replay_session_with(.., GovernorMode::Legacy)`（此时 Legacy 仍在，对照语义保留）。GUI 侧默认行为变化 = 有意接管，写进 commit 说明。
- [ ] **Step 3: 全绿 + 实机快验** — `cargo test -p harness-runtime`；再 `python -X utf8 scripts/governance_ab_run.py --smoke --profile "openai · deepseek-v4-pro"`（不带 HARNESS_GOVERNOR 亦须全绿，冒烟即走控制器）。
- [ ] **Step 4: Commit** — `feat(governance): A/B 默认切换控制器接管，HARNESS_GOVERNOR=legacy 保留逃生门`。

### Task 3: A2 按调用意图细化（双侧同步）

**Files:** Modify `session_replay.rs`（meter + symptom 测试）、`harness/scripts/governance_redline_check.py`、spec §3 A2 行

- [ ] **Step 1: 定义意图分类（Rust 侧先红）** — 新增并让 meter 只统计**探索型**签名（`search:` 与写类/未分类 shell 计入；`fs:` 纯 read/list、验证类 shell、`plan:`、`memory:` 豁免）：

```rust
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
```

`a2_max_cross_turn_repeat` 内 `for sig in &t.signatures` 改 `for sig in t.signatures.iter().filter(|s| is_exploratory(s))`。新增单测：

```rust
#[test]
fn a2_exempts_readback_and_verify_repeats() {
    let mk = |sigs: &[&str]| TurnSummary { signatures: sigs.iter().map(|s| s.to_string()).collect(), ..Default::default() };
    let turns = vec![
        mk(&["fs:{\"op\":\"read\",\"path\":\"a.py\"}", "shell:{\"command\":\"python -m py_compile a.py\"}"]),
        mk(&["fs:{\"op\":\"read\",\"path\":\"a.py\"}"]),
        mk(&["fs:{\"op\":\"read\",\"path\":\"a.py\"}"]),
    ];
    assert_eq!(a2_max_cross_turn_repeat(&turns), 0, "纯读取/编译验证重复不计 A2");
    let loops = vec![mk(&["shell:{\"command\":\"dir /s\"}"]), mk(&["shell:{\"command\":\"dir /s\"}"]), mk(&["shell:{\"command\":\"dir /s\"}"])];
    assert_eq!(a2_max_cross_turn_repeat(&loops), 3);
}
```

- [ ] **Step 2: Python 同步** — `governance_redline_check.py::meters` 的 A2 用同规则（`is_exploratory(sig)`：`sig.startswith("fs:")` 且 op read/list → False；`search:` → **True 保留**（探索）——注意 fs 读豁免、search 不豁免；`shell:` 命令含 py_compile/cargo check/build/test → False；`plan:`/`memory:` → False）。
- [ ] **Step 3: spec §3 写回** — A2 定义句改为「同一**探索型**工具签名跨回合重复 ≤2（纯读取/验证编译类豁免，2026-09-01 实机 S1 判读）」。
- [ ] **Step 4: 复算验证** — `cargo test -p harness-runtime --test session_replay`（8/8 绿）；`python -X utf8 harness/scripts/governance_redline_check.py harness/ab-runs/20260901-203859/S1-controller.jsonl` → **六项全绿 exit=0**；基线 `7ba3370f_full.jsonl` 重打分违例集合不得缩小（仍含 R1,R2,R3,R4,A1）。
- [ ] **Step 5: Commit** — `feat(governance): A2 按调用意图细化，回读/验证型重复豁免（实机误报修正）`。

### Task 4: stack_pos 进投影（无 schema 变更）+ spec §4.3 写回

**Files:** Modify `governor/strategy.rs`（暴露 `position()`）、`agent_loop.rs`（遥测写入用现有字段）、`case_file.rs`（`current_strategy` 字段）、其单测、spec §4.3

- [ ] **Step 1: StrategyStack::position()** — `pub fn position(&self) -> usize`，返回**已消耗窗口数** = `for_task/at_bottom` 所用栈的固定总深 − `self.stack.len()`（读实现后以 `WINDOW_STEPS`/栈构造为准确定为纯函数，无状态副作用）；单测：初始栈 `position()==0`，每次 `pop()` 后 `+1`。
- [ ] **Step 2: 投影路径** — 控制器每回合观测点在**既有** `Telemetry` 事件 `next_action` 字段追加 `strategy=<label>@<position>` 后缀（ExecutionTelemetry 无 schema 变化，仅字符串载荷复用）；`case_file.rs` 新增 `pub current_strategy: Option<(String, usize)>`，投影时从最新 Telemetry 的 next_action 尾部解析（解析失败=None，不得 panic）。补投影单测（构造带后缀的 Telemetry 事件）。
- [ ] **Step 3: spec §4.3 写回** — 策略栈小节追加：「栈位经 Telemetry.next_action 的 `strategy=<label>@<pos>` 后缀携带，CaseFile 投影为 current_strategy（schema 冻结下的显式决定，2026-09-01）」。
- [ ] **Step 4: 全绿 + Commit** — `cargo test -p harness-runtime`；`feat(governance): 策略栈位经遥测载荷进 CaseFile 投影，spec §4.3 写回`。
- [ ] **Step 5: 阶段 2 遗留② 收编（gate ask_user 带候选锚点）** — 定位 `:525` 构造点后 ask_user 三重前置满足时的**早返回路径**（NeedsUserInput 提前 return，不经 `:1833` 收口块），让其同样 append 一条 `artifact_text(...)` Assistant 事件且问题文本内嵌工作区候选（复用 `candidate` 拼装逻辑）。测试：脚本化 LLM 在无锚点 + 空转信号下触发 ask_user → 断言 Delivery(NeedsUserInput) 前存在含路径锚点的 Assistant 事件。若勘察后确认该路径同样必经 `:1833`（即遗留②已不存在），在本计划勾选处注明证据（文件:行）后跳过实现。

### Task 5: 旧守卫与 6 计数器退位删除（大删除，逐子步全绿）

**Files:** Modify `agent_loop.rs`、`goal_execution.rs`、`execution.rs`、`session_replay.rs`、涉及单测

前置判据：T1–T4 全绿 + 本 Task 每小步后 `cargo test -p harness-runtime --test session_replay` 保持 8/8（红线在 On 下）。

- [ ] **Step 5a: 删 `[强制收敛]/[自动接续]` 块** — `agent_loop.rs:1660-1695` 整段（含 `MAX_STAGNANT_WINDOWS`）；`execution.rs` 的 `stagnant_windows` 字段/累加/重置（`:829,:886,:992,:1005`）与其测试（`:1558-1608` 涉及断言改写为等价预算语义）。编译 → 测试 → commit `refactor(governance): 自动接续/强制收敛守卫退位，终止来源唯一化`。
- [ ] **Step 5b: 删 goal_execution 熔断计数** — `no_information_count`/`correction_count` 字段（`:517-518`）、`MAX_NO_INFORMATION` 与 breaker（`:1466-1525`）、相关测试（`:2146-2172` 删除）；调用点逐一改接 gain 传感器信号（阶段 2 已提供）或移除；commit `refactor(governance): no_information/correction 计数器退位（gain 传感器替代）`。
- [ ] **Step 5c: ToolRepeatGuard 降级** — 终止改纯信号：重复命中 → 只记 eliminated/tried 事实，控制器经 `strategy.rs` 现有 `[换路]` 机制处置；`:1598`/`:1612` 两个旧 `[error]` 终止分支删除或并入控制器观测点（先 grep 其 On 路径可达性，确保 On 下这些分支不再终止回合；Legacy 下保留到最后）。
- [ ] **Step 5d: 移除 GovernorMode** — 全文件删 `enum GovernorMode`/`parse_governor_mode`/`with_governor`/`governor` 字段与 `:525/:1833` 条件（On 语义成为唯一路径）；`session_replay.rs` 的 `replay_session_with(.., mode)` 收敛回 `replay_session(fixture)`，Legacy 对照测试（clarification 4-delivery、symptom `:495` 对照）**改判为对照基线指标常量**（把 Legacy 实测值固化为注释/断言常量，不再运行 Legacy 路径）；`HARNESS_GOVERNOR` env 读取移除（README/architecture 记一行历史）；commit `feat(governance): Legacy 守卫路径整体退位，控制器为唯一回合路径`。
- [ ] **Step 5e: 全量回归** — `cargo test --workspace` + `cargo clippy --workspace -- -D warnings`（新警告清零，既有 ui 3 条豁免）。

### Task 6: goal_execution.rs / execution.rs 冗余收缩

- [ ] Step 1: `cargo build 2>&1 | grep -E "never used|dead_code"` 与 grep 死 pub 函数清单 → 删除仅服务于已退位守卫的助手/结构/事件文本；`goal_execution.rs` 目标 ~-15%（163KB→≤140KB），`execution.rs` 预算结构收缩到成本传感器所需（300k 顶、窗口 W）。
- [ ] Step 2: 全测试绿 + commit `refactor(governance): goal_execution/execution 冗余收缩（步骤⑤收尾）`。

### Task 7: 文档退位标记

- [ ] Step 1: 事实 8 的 6 份文档首行插入统一块：`> ⚠️ DEPRECATED（2026-09-01）：本文所述补丁机制已由闭环控制器 + Case File 取代（见 docs/superpowers/specs/2026-08-31-agent-governance-redesign-design.md）。保留作事故史证。`
- [ ] Step 2: `docs/architecture.md` 治理章节指向 spec + 阶段计划；spec 状态行改「步骤①–⑤全部完成，五步迁移收官」；commit `docs(governance): V3–V5 与五代补丁史文档标记 deprecated`。

### Task 8: 收官验证（回放 + 实机复跑 + 推送）

- [ ] Step 1: `cargo test --workspace` 全绿；`--test session_replay` 8/8。
- [ ] Step 2: 重建 `scripts\build.bat package` → `python -X utf8 scripts/governance_ab_run.py --scenarios S1,S2,S3 --modes controller --profile "openai · deepseek-v4-pro"` → **exit=0 三场景六项全绿**（A2 细化后 S1 应转绿）；结果回写对照清单判读（追加阶段 3 复跑行）。
- [ ] Step 3: `git push origin main`；记忆进度条目更新为「五步收官，遗留=GUI 黑框手验」。

## 已知风险

1. T5d 删 Legacy 后 `agent_loop.rs` 体积仍 14 万字节——只删不搬（收缩在 T6），避免一次巨型 diff。
2. A2 细化可能让真实「search 复读」漏检——search 保留计入了断言（Task 3 Step 2 特意不豁免）。
3. 实机模型输出有随机性：T8 若某场景因模型发挥触发新违例（如 R4 边界），按清单出口转 fixture 复现，先修后收官。
