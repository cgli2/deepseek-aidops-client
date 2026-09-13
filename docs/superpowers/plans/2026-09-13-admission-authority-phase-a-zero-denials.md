# 准入权唯一化·阶段 A（拦停归零）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:executing-plans（子代理不可用时 inline 执行）。步骤用 `- [ ]` 复选框跟踪。

**Goal:** 让「计数/阶段/关联类」判断失去吞掉工具动作的能力——动作照常执行、判断降级为提示——从而把 `gate]` / `guard]` 从工具结果里彻底清零，并让当前三条红测试自洽。

**Architecture:** 在 `GateDecision` 上增加 `Advise` 变体，形成「真实外部约束 → `Deny`；证据不足/疑似空转/次数用尽 → `Advise`」的唯一词汇表；`agent_loop.rs` 的工具准入段成为唯一消费点，`Advise` 放行动作并把原因作为提示注入下一步请求，只有 `Deny` 才产生错误工具结果。`PhaseBudget` 等计数字段全部保留为遥测（spec D2），终止权仍唯一归 `TurnGovernor`。

**Tech Stack:** Rust（`harness-runtime` 为主）、cargo（**必须在 `harness/` 下跑**，MSVC 工具链）、`scripts/governance_ab_run.py` 实机复跑（`python -X utf8`）。

---

## 本计划覆盖范围与后续计划

spec `2026-09-13-agent-admission-authority-consolidation-design.md` 的 §5 有 S1–S5 五个切片。**本计划只做 S1+S2**（§4.1、§4.2 的准入语义部分）。理由：这一片独立产出可测的完整行为（红线「零拦停」成立、三条红测试自洽），且 S3（§4.3 事实级记忆）、S4（§4.4 完成判定）、S5（阶段 3 的 5c/5d/T6/T7 与大删除）各自需要以「拦停已归零」为前提重新观测，混在一片里会让回归无法定位。

不在本计划内、**必须另立计划**的项：fixture 采集与交付率指标（spec §6 的 10 fixture 与 `governance_redline_check.py` 双侧同步）、§4.3 压缩豁免、§4.4 完成判定、§4.5 的 `[error]` 文本回喂、GovernorMode 移除。

## 已核实代码事实（执行前无需再探索；行号为 2026-09-13 本机实测）

1. `GateDecision` 定义在 `harness-runtime/src/execution.rs:1373-1376`，仅两个变体 `Allow` / `Deny(String)`；`ActionGate::authorize_with_tools`（`:1391`）转调 `authorize_impl`（`:1400`），后者现有四个 `Deny`：动态工具白名单（`:1411`）、`supports.is_empty()` → 「该调用未关联任何验收标准」（`:1417-1419`）、`tool_calls >= hard_max_tool_calls`（`:1420-1425`）、OpenEnded 三条搜索封顶（`:1430-1443`）。
2. `agent_loop.rs:1552` 是唯一消费点：`if let GateDecision::Deny(reason) = ActionGate::authorize_with_tools(...)`，产 `[execution gate] {reason}` 的失败 `ToolResult`（`:1561`）并入 `messages`（`:1572`）。
3. `GoalExecution::allows_tool_call`（`goal_execution.rs:1577`）返回 `Result<(), String>`，拒绝点：写入阶段限制（`:1584`）、**阶段预算耗尽**（`:1596-1606`，文案「当前 {} 阶段预算已耗尽；必须切换假设或报告精确阻塞」）、工具白名单（`:1626`）、目录枚举（`:1633`）、锚点外泛搜（`:1649`、`:1666-1672`）、编辑目标未确认（`:1680-1692`）、读取目标不在候选（`:1698-`）。
4. 阶段预算常量：`PhaseBudget::default()` = `locate 2 / inspect 4 / change 2 / verify 2`（`goal_execution.rs:246-257`）；消耗点 `item.phase_attempts.increment(action.phase)`（`:1845`）。
5. 另两处会吞动作：`ToolRepeatGuard::should_block`（`agent_loop.rs:1436`，产 `[tool-loop guard]`）与 `LocateStepGate::allows`（`agent_loop.rs:1512`，产 `[controlled-delivery guard]`）；`MAX_CONSECUTIVE_LOCATE_CALLS_PER_TURN = 10` 在 `agent_loop.rs:97`，用于 `:496`。
6. 提示注入通道已存在：`loop_recovery_prompts: Vec<String>`（`agent_loop.rs:1321` 区域声明，`:1454`/`:1927` 等 push），在 `:1970` 处 `for prompt in loop_recovery_prompts { messages.push(Message::user(prompt)) }`。上限常量 `MAX_LOOP_RECOVERY_PROMPTS = 2` 在 `:1063` 区域。
7. 三条红测试（实测 `cargo test -p harness-runtime --no-fail-fast`）：`delivery_workflow::repair_reproduces_edits_retries_and_verifies_in_one_request`（拦停文本 `[execution gate] 该调用未关联任何验收标准`）、`agent_tool_loop::concrete_problem_replay_starts_with_locate_not_shell_verification`、`agent_tool_loop::quoted_menu_shortening_starts_from_the_grounded_file_not_repository_search`。其余 300 单测与 10 个集成二进制全绿。
8. 测试样板（**逐字复制来源，勿自造**）：ctx 装配样板取 `harness-runtime/tests/agent_tool_loop.rs:476-495`（`AppContext::new` + `SessionLog::new` + `ctx.provide(log.clone())` + `Arc<dyn LlmProvider>` + `ToolRegistry::new()` + `Arc<dyn Hook>`/`AllowHook` + `AgentLoop::new().run_turn(&ctx, UserInput{ text, attachments: vec![] }).await.unwrap()`）；脚本化模型取 `ScriptedLlm`（`:98-141`）与 `scripted_call`（`:143-149`）；静态工具取 `StaticTool { name, output }`（`:77-96`）；真实磁盘场景取 `harness-runtime/tests/delivery_workflow.rs:14-35`（`Script`）与 `:37-`（`DiskTool`）。
9. 回放入口：`harness-runtime/tests/session_replay.rs:182` `async fn replay_session_with(fixture: &str, mode: GovernorMode) -> Arc<SessionLog>`、`:243` `replay_session(fixture)`（默认 On）、`:10` `const FIXTURES`；现有 5 个 fixture 在 `harness-runtime/tests/fixtures/`。
10. 工作区是 git 仓（main）；`harness/harness-runtime/src/delivery_workflow.rs` 与 `tests/delivery_workflow.rs` 目前是**未跟踪**文件，属用户进行中的工作，改动前须先 `git status` 确认。

## 决策边界（不得越界）

- **只有真实外部约束保留拒绝能力**：访问策略（`AccessPolicy`，`agent_loop.rs:1568`）、钩子阻断（`:1609`）、沙箱/IO 错误、工作区外路径、`read_only` 任务的写入、shell 外部副作用审批。这些分支的现有行为**一字不改**。
- 计数字段（`phase_budget`、`phase_attempts`、`consecutive_locate`、`ToolRepeatGuard` 累计表）**不删**（spec D2），只摘除其拒绝能力，继续供遥测与 `TurnGovernor` 消费。
- `[需要澄清]` 门禁合成文本与 `ReplayLlm` 耗尽回退 chunk 文本不得改动（回放收敛依赖，阶段 3 边界沿用）。
- `SessionEvent` schema、LLM provider 接口、`harness-ui` 不动。
- `Advise` 的原因文本**永不**写入 `ToolResult`（红线本体），只进 `Message::user` 提示与 `Thinking` 遥测。

## 文件结构

| 文件 | 动作 | 职责 |
|---|---|---|
| `harness-runtime/src/execution.rs` | 修改 | `GateDecision::Advise` 变体；`authorize_impl` 四个分支归类 |
| `harness-runtime/src/goal_execution.rs` | 修改 | `allows_tool_call` 错误类型改 `GateDecision`；计数/阶段/锚点类分支改 `Advise` |
| `harness-runtime/src/agent_loop.rs` | 修改 | 唯一消费点：放行 `Advise`、注入提示、摘除 repeat/locate 拒绝能力 |
| `harness-runtime/src/delivery_workflow.rs` | 修改 | `admit()` 裁决词汇表落点（spec D1）；提示去重与每回合上限 |
| `harness-runtime/tests/agent_tool_loop.rs` | 修改 | 零拦停红线、放行语义、提示注入、上限各一测试 |
| `harness-runtime/tests/delivery_workflow.rs` | 只读 | 现有红线场景，须自行转绿，**不得放宽断言** |
| `harness-runtime/tests/session_replay.rs` | 修改 | 对 5 个 fixture 增加「零拦停」断言 |
| `docs/superpowers/specs/2026-09-13-...-design.md` | 修改 | 状态回写：§4.1/§4.2 阶段 A 落地结果 |

---

### Task 1: 零拦停红线（全量断言，先红）

**Files:**
- Modify: `harness-runtime/tests/agent_tool_loop.rs`（文件末尾追加）
- Modify: `harness-runtime/tests/session_replay.rs`（`:243` 之后追加）

- [ ] **Step 1: 写失败测试（工具确实被派发，且无拒绝文本）**

追加到 `harness-runtime/tests/agent_tool_loop.rs` 末尾。`CountingTool` 是本计划新定义的测试工具，用来证明动作「照常执行」而不是被吞掉：

```rust
/// 记录真实派发次数的工具：准入降级后，被"建议"的动作必须仍然到达工具层。
struct CountingTool {
    name: &'static str,
    hits: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl harness_tool::DynTool for CountingTool {
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
    let search_hits = Arc::new(CountingTool { name: "search", hits: AtomicUsize::new(0) });
    let fs_hits = Arc::new(CountingTool { name: "fs", hits: AtomicUsize::new(0) });
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
    assert_eq!(search_hits.hits.load(Ordering::SeqCst), 3, "三次 search 必须全部到达工具层");
    assert_eq!(fs_hits.hits.load(Ordering::SeqCst), 3, "三次读取必须全部到达工具层");
}
```

- [ ] **Step 2: 对现有 fixture 同样断言零拦停**

追加到 `harness-runtime/tests/session_replay.rs`（`:245` 之后）。`FIXTURES` 常量在 `:10`：

```rust
/// 真实历史会话回放后，不得再出现任何被门禁替换的工具结果。
#[tokio::test]
async fn replayed_sessions_emit_no_denied_tool_results() {
    for fixture in [
        "7ba3370f_full.jsonl",
        "7ba3370f_t03_14_symptom.jsonl",
        "7ba3370f_t15_18_clarification.jsonl",
        "7ba3370f_t19_22_gitfix.jsonl",
        "success_677bd6e0.jsonl",
    ] {
        let events = replay_session(fixture).await.replay();
        let denied: Vec<String> = events
            .iter()
            .filter_map(|event| match event {
                SessionEvent::ToolResult { result, .. } => ["gate]", "guard]"]
                    .iter()
                    .any(|marker| result.content.contains(marker))
                    .then(|| result.call_id.clone()),
                _ => None,
            })
            .collect();
        assert!(denied.is_empty(), "{fixture} 仍产生拦停结果: {denied:?}");
    }
}
```

- [ ] **Step 3: 跑红并记录实际违例集合**

```bash
cd harness && cargo test -p harness-runtime --test agent_tool_loop advisory_gates_never_replace
cd harness && cargo test -p harness-runtime --test session_replay replayed_sessions_emit_no_denied
cd harness && cargo test -p harness-runtime --test delivery_workflow
```
Expected: 三条全部 **FAIL**。把每条实际出现的拦停文本原样抄进下一步的提交说明，作为「哪些门禁在吞动作」的实测清单（预期至少含 `[tool-loop guard]`、`[execution gate] 该调用未关联任何验收标准`）。若某条意外为绿，说明该路径本就不拦停——记录事实，继续。

- [ ] **Step 4: 不改实现，提交红线**

```bash
git add harness/harness-runtime/tests/agent_tool_loop.rs harness/harness-runtime/tests/session_replay.rs
git commit -m "test(governance): 零拦停红线入册，实测列出仍在吞动作的门禁"
```

### Task 2: `GateDecision::Advise` 与 `authorize_impl` 分支归类

**Files:**
- Modify: `harness-runtime/src/execution.rs:1373-1443`
- Test: `harness-runtime/tests/agent_tool_loop.rs`

- [ ] **Step 1: 加变体并写明语义**

`execution.rs:1373` 改为：

```rust
pub enum GateDecision {
    Allow,
    /// 真实外部约束：动作不得执行（访问策略、沙箱、写冲突、用户审批、IO 错误）。
    Deny(String),
    /// 证据不足、疑似空转或计数用尽：动作照常执行，原因作为提示注入下一步请求。
    /// 它不得产生工具结果——见 spec R1/R3。
    Advise(String),
}
```

同文件 grep 全部 `GateDecision::Deny` 构造点，逐个按下述归类改写；`authorize_impl` 内四个分支的改写：`:1411`（动态白名单）、`:1417`（未关联验收标准）、`:1420`（绝对调用上限）、`:1430`（OpenEnded 三条搜索封顶）→ 全部 `GateDecision::Advise`。文案保留原意，只把命令式禁止改为陈述式提示，例如 `:1418` 改：

```rust
        if proposal.supports.is_empty() {
            return GateDecision::Advise(
                "该调用未关联具体验收项：说明它要回答什么问题，避免无关探索".into(),
            );
        }
```

- [ ] **Step 2: 编译并按编译器定位漏改的匹配点**

```bash
cd harness && cargo test -p harness-runtime --no-fail-fast 2>&1 | grep -E "^error|non-exhaustive" | head -20
```
Expected: 若干 `non-exhaustive patterns` 错误，逐个补 `Advise` 分支。`agent_loop.rs:1552` 的 `if let GateDecision::Deny(reason)` 不报错但仍只处理 Deny——Task 3 处理，此处先记录为待办，**不要**顺手改成通配。

- [ ] **Step 3: 全量跑一次，确认只有 Task 1 的红线仍然红**

```bash
cd harness && cargo test -p harness-runtime --no-fail-fast 2>&1 | grep -E "^test result|FAILED" | head
```
Expected: `advisory_gates_never_replace_a_dispatched_tool_result` 仍红（消费点尚未放行 Advise），其余测试不得新增失败。

- [ ] **Step 4: 提交**

```bash
git add harness/harness-runtime/src/execution.rs
git commit -m "refactor(governance): GateDecision 增加 Advise，真实约束与计数判断分道"
```

### Task 3: 唯一消费点放行 `Advise`

**Files:**
- Modify: `harness-runtime/src/agent_loop.rs:1541-1575`
- Test: `harness-runtime/tests/agent_tool_loop.rs`

- [ ] **Step 1: 写失败测试（提示进请求，动作仍执行）**

追加到 `agent_tool_loop.rs`。本测试需要看到出站请求，故用带 `requests` 捕获的 provider（形态抄 `ProgressAtCapLlm` 的 `stream` 捕获写法，`:251-252`）：

```rust
/// 每次请求都固定吐一个重复 search，制造「零增益」提示场景。
struct RepeatSearchLlm {
    calls: AtomicUsize,
    requests: Mutex<Vec<Vec<Message>>>,
}

#[async_trait]
impl LlmProvider for RepeatSearchLlm {
    fn name(&self) -> &'static str {
        "repeat-search-test"
    }
    fn tools(&self) -> Vec<harness_llm::ToolSchema> {
        vec![]
    }
    fn stream(&self, messages: Vec<Message>) -> ChunkStream {
        self.requests.lock().unwrap().push(messages);
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunk = if call < 4 {
            Chunk {
                tool_calls: vec![ToolCall {
                    id: format!("rep-{call}"),
                    name: "search".into(),
                    args: serde_json::json!({"pattern": "同一个关键词"}),
                }],
                ..Default::default()
            }
        } else {
            Chunk {
                text: Some("已基于现有证据收尾".into()),
                ..Default::default()
            }
        };
        Box::pin(futures::stream::iter(vec![Ok(chunk)]))
    }
}

#[tokio::test]
async fn zero_gain_advice_is_injected_without_swallowing_the_action() {
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let llm = Arc::new(RepeatSearchLlm {
        calls: AtomicUsize::new(0),
        requests: Mutex::new(vec![]),
    });
    let hits = Arc::new(CountingTool { name: "search", hits: AtomicUsize::new(0) });
    let tools = ToolRegistry::new();
    tools.register(hits.clone());
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
                text: "反复用同一关键词定位后修复".into(),
                attachments: vec![],
            },
        )
        .await
        .unwrap();

    let events = log.replay();
    assert!(
        !events.iter().any(|event| matches!(event,
            SessionEvent::ToolResult { result, .. }
                if result.content.contains("gate]") || result.content.contains("guard]"))),
        "零增益只提示，不得拦停"
    );
    assert_eq!(hits.hits.load(Ordering::SeqCst), 4, "四次重复 search 必须全部派发");
    let requests = llm.requests.lock().unwrap();
    let advised = requests.iter().skip(1).any(|request| {
        request.iter().any(|message| {
            message.role == harness_llm::Role::User && message.content.contains("未新增信息")
        })
    });
    assert!(advised, "提示须随下一步请求注入: {:?}", requests.last().map(|r| r.len()));
}
```

同一步再补一条锁定提示通量契约的单元测试（先写、此时 `note_advice` / `render_advisories` 尚不存在，编译即为红）：

```rust
// 追加到 harness-runtime/src/delivery_workflow.rs 末尾
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advisories_deduplicate_and_are_capped_at_one_per_step() {
        let mut seen = Vec::new();
        note_advice(&mut seen, "相同参数的调用不会带来新信息");
        note_advice(&mut seen, "相同参数的调用不会带来新信息");
        note_advice(&mut seen, "inspect 阶段已用去 5 次动作");
        assert_eq!(seen.len(), 2, "同类提示必须去重");
        assert_eq!(render_advisories(&seen).len(), 1, "每回合最多注入一条提示");
    }
}
```

- [ ] **Step 2: 跑红**

```bash
cd harness && cargo test -p harness-runtime --test agent_tool_loop zero_gain_advice
cd harness && cargo test -p harness-runtime --lib delivery_workflow
```
Expected: 集成测试 FAIL（当前动作被 `[tool-loop guard]` 吞掉，`hits` 远小于 4）；单元测试 **编译失败**（`cannot find function note_advice`），这是预期的红。

- [ ] **Step 3: 改造消费点**

`agent_loop.rs:1552` 起，把只处理 `Deny` 的分支改成三态，且 `Advise` 不产生工具结果：

```rust
                    match ActionGate::authorize_with_tools(
                        &proposal,
                        &execution,
                        &budget,
                        &runtime_allowed_tools,
                    ) {
                        // 真实外部约束：保持原有拒绝行为与文案通道。
                        GateDecision::Deny(reason) => {
                            // 原样搬运现 agent_loop.rs:1553-1575 的主体：构造
                            // `[execution gate] {reason}` 失败 ToolResult、写日志、
                            // repeat_guard.record_result、push 到 messages、置
                            // step_had_tools、continue。逐字保留，不改一个字符。
                        }
                        // 计数/关联类：动作照常派发，原因变成提示。
                        GateDecision::Advise(reason) => {
                            crate::delivery_workflow::note_advice(&mut advisories, &reason);
                        }
                        GateDecision::Allow => {}
                    }
```

在该步工具循环开始前声明 `let mut advisories: Vec<String> = Vec::new();`（与 `loop_recovery_prompts` 同处），并在原 `for prompt in loop_recovery_prompts { ... }`（`:1970`）之前追加：

```rust
            for hint in crate::delivery_workflow::render_advisories(&advisories) {
                messages.push(Message::user(hint));
            }
```

- [ ] **Step 4: 落 `delivery_workflow` 的两个函数（spec D1）**

`delivery_workflow.rs` 末尾追加：

```rust
/// 准入提示的唯一落点：去重、限量，并保证它只成为提示而不是工具结果。
pub(crate) fn note_advice(advisories: &mut Vec<String>, reason: &str) {
    let line = format!("未新增信息的可能：{reason}");
    if !advisories.contains(&line) {
        advisories.push(line);
    }
}

/// 每回合最多注入 1 条提示：多条同类提示只会挤掉真正需要的上下文。
pub(crate) fn render_advisories(advisories: &[String]) -> Vec<String> {
    advisories
        .iter()
        .take(1)
        .map(|line| format!("[运行时提示] {line}；如仍需该动作，直接继续。"))
        .collect()
}
```

- [ ] **Step 5: 跑绿 + 提交**

```bash
cd harness && cargo test -p harness-runtime --test agent_tool_loop zero_gain_advice
cd harness && cargo test -p harness-runtime --lib delivery_workflow
git add harness/harness-runtime/src/agent_loop.rs harness/harness-runtime/src/delivery_workflow.rs harness/harness-runtime/tests/agent_tool_loop.rs
git commit -m "feat(governance): ActionGate 提示类判断放行动作，提示注入唯一经 delivery_workflow"
```
Expected: 两条均 PASS。

### Task 4: `allows_tool_call` 改用同一词汇表

**Files:**
- Modify: `harness-runtime/src/goal_execution.rs:1577-1710`
- Modify: `harness-runtime/src/agent_loop.rs:1477-1500`
- Test: `harness-runtime/tests/agent_tool_loop.rs`

- [ ] **Step 1: 写失败测试**

追加到 `agent_tool_loop.rs`：阶段预算（默认 locate 2 / inspect 4）用尽后，第 5 次定向读取仍须派发。

```rust
#[tokio::test]
async fn phase_budget_exhaustion_no_longer_blocks_reads() {
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let script: Vec<Option<ToolCall>> = (0..6)
        .map(|index| {
            scripted_call(
                &format!("read-{index}"),
                "fs",
                serde_json::json!({"op": "read", "path": format!("src/part_{index}.py")}),
            )
        })
        .chain(std::iter::once(None))
        .collect();
    let llm = Arc::new(ScriptedLlm {
        calls: AtomicUsize::new(0),
        initial_text_steps: 0,
        script,
        options: Mutex::new(vec![]),
    });
    let hits = Arc::new(CountingTool { name: "fs", hits: AtomicUsize::new(0) });
    let tools = ToolRegistry::new();
    tools.register(hits.clone());
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
                text: "逐个读取六个模块文件后修复保存报错".into(),
                attachments: vec![],
            },
        )
        .await
        .unwrap();

    assert_eq!(hits.hits.load(Ordering::SeqCst), 6, "六个不同文件的读取都该放行");
    assert!(
        !log.replay().iter().any(|event| matches!(event,
            SessionEvent::ToolResult { result, .. }
                if result.content.contains("gate]") || result.content.contains("guard]"))),
    );
}
```

- [ ] **Step 2: 跑红**

```bash
cd harness && cargo test -p harness-runtime --test agent_tool_loop phase_budget_exhaustion
```
Expected: FAIL（第 3/5 次起被 `[target-anchor gate] 当前 … 阶段预算已耗尽` 拦下）。

- [ ] **Step 3: 改错误类型并归类**

`allows_tool_call` 签名改为返回 `Result<(), GateDecision>`（在 `goal_execution.rs` 头部 `use crate::execution::{.., GateDecision}` 补上该项），按下列归类逐点改写（`goal_execution.rs`）：

| 行号 | 分支 | 改写为 |
|---|---|---|
| `:1584` | `read_only` 或非白名单 `edit` 时写入 | `Err(GateDecision::Deny(..))`（真实约束，保持） |
| `:1596-1606` | **阶段预算耗尽** | `Err(GateDecision::Advise(format!("{} 阶段已用去 {} 次动作；说明本次动作将新增什么证据", phase.as_str(), item.phase_attempts.get(phase))))` |
| `:1626` | 阶段工具白名单 | `Advise` |
| `:1633` | 目录枚举 | `Advise` |
| `:1649` / `:1666-1672` | 锚点外泛搜 / 回根泛搜 | `Advise` |
| `:1680-1692` | 编辑目标未确认 | `Deny`（真实约束：未读文件不得盲写） |
| `:1698-` | 读取目标不在候选 | `Advise` |

`agent_loop.rs:1477` 的调用点同步改为三态，`Advise` 走 Task 3 的 `note_advice`，`Deny` 保持 `[target-anchor gate]` 通道不变。

- [ ] **Step 4: 跑绿 + 全量**

```bash
cd harness && cargo test -p harness-runtime --test agent_tool_loop phase_budget_exhaustion
cd harness && cargo test -p harness-runtime --no-fail-fast 2>&1 | grep -E "^test result|FAILED"
```
Expected: 新测试绿；Task 1 两条红线转绿；`delivery_workflow` 与两条遥测测试仍红（Task 6 处理），**不得出现新的失败**。

- [ ] **Step 5: 提交**

```bash
git add harness/harness-runtime/src/goal_execution.rs harness/harness-runtime/src/agent_loop.rs harness/harness-runtime/tests/agent_tool_loop.rs
git commit -m "refactor(governance): 阶段预算与锚点判断退位为提示，仅保留未确认目标的写入拒绝"
```

### Task 5: `ToolRepeatGuard` / `LocateStepGate` / locate 计数摘除拒绝能力

**Files:**
- Modify: `harness-runtime/src/agent_loop.rs:496`、`:1436`、`:1512`
- Test: `harness-runtime/tests/agent_tool_loop.rs`

- [ ] **Step 1: 写失败测试（连续同签名仍派发，但只提示一次）**

```rust
#[tokio::test]
async fn repeated_identical_calls_run_and_produce_exactly_one_hint() {
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let script: Vec<Option<ToolCall>> = (0..4)
        .map(|index| {
            scripted_call(
                &format!("same-{index}"),
                "search",
                serde_json::json!({"pattern": "target-anchor gate"}),
            )
        })
        .chain(std::iter::once(None))
        .collect();
    let llm = Arc::new(ScriptedLlm {
        calls: AtomicUsize::new(0),
        initial_text_steps: 0,
        script,
        options: Mutex::new(vec![]),
    });
    let hits = Arc::new(CountingTool { name: "search", hits: AtomicUsize::new(0) });
    let tools = ToolRegistry::new();
    tools.register(hits.clone());
    let _a = ctx.provide(log.clone());
    let provider: Arc<dyn LlmProvider> = llm;
    let _b = ctx.provide(provider);
    let _c = ctx.provide(tools);
    let hook: Arc<dyn Hook> = Arc::new(AllowHook);
    let _d = ctx.provide(hook);

    AgentLoop::new()
        .run_turn(
            &ctx,
            UserInput { text: "同一关键词反复搜索直到确认结论".into(), attachments: vec![] },
        )
        .await
        .unwrap();

    assert_eq!(hits.hits.load(Ordering::SeqCst), 4, "重复调用须照常派发");
    let events = log.replay();
    assert!(!events.iter().any(|event| matches!(event,
        SessionEvent::ToolResult { result, .. }
            if result.content.contains("guard]") || result.content.contains("gate]"))));
}
```

- [ ] **Step 2: 跑红**

```bash
cd harness && cargo test -p harness-runtime --test agent_tool_loop repeated_identical_calls
```
Expected: FAIL（`[tool-loop guard]` 在重复命中时替换了工具结果）。

- [ ] **Step 3: 三处改为只产信号**

- `:1436` `if repeat_guard.should_block(&sig)`：保留判定与 `note_recovery`，但分支体不再构造 `blocked` 工具结果，改为 `note_advice(&mut advisories, "相同参数的调用不会带来新信息；换参数、换工具或基于现有结果收尾")`，**不 `continue`**，让动作继续走到 `pending`。
- `:1512` `if !locate_step_gate.allows(true, &sig)`：同样改为 `note_advice(..)`，不再替换结果；`LocateStepGate` 仍计数（遥测）。
- `:496` `locate_probes_exhausted`：从 `should_block` 的返回式中移除（`repeated_success || failed_retries_exhausted`），`consecutive_locate` 字段保留并继续自增，供 §6 指标观测。`MAX_CONSECUTIVE_LOCATE_CALLS_PER_TURN`（`:97`）保留为遥测阈值常量。

- [ ] **Step 4: 跑绿 + 全量**

```bash
cd harness && cargo test -p harness-runtime --test agent_tool_loop repeated_identical_calls
cd harness && cargo test -p harness-runtime --no-fail-fast 2>&1 | grep -E "^test result|FAILED"
```
Expected: 新测试绿；Task 1/3/4 全绿；仅余 Task 6 的三条既有红测试。

- [ ] **Step 5: 提交**

```bash
git add harness/harness-runtime/src/agent_loop.rs harness/harness-runtime/tests/agent_tool_loop.rs
git commit -m "refactor(governance): 重复与定位守卫退位为信号，终止仍由控制器唯一负责"
```

### Task 6: 收敛三条既有红测试并收官

**Files:**
- Modify: `harness-runtime/tests/agent_tool_loop.rs:953-975`、`:1006-1030`（按实测行号）
- Modify: `harness-runtime/tests/delivery_workflow.rs`
- Modify: `docs/superpowers/specs/2026-09-13-agent-admission-authority-consolidation-design.md`

- [ ] **Step 1: 判读两条遥测断言的实际取值（spec §10 的未核实项）**

临时在 `quoted_menu_shortening_starts_from_the_grounded_file_not_repository_search` 断言前打印，跑一次后**立即还原**：

```bash
cd harness && cargo test -p harness-runtime --test agent_tool_loop quoted_menu_shortening -- --nocapture 2>&1 | grep -A 8 "TELEMETRY"
```
在断言前插入：
```rust
    for event in log.replay() {
        if let SessionEvent::Telemetry { telemetry, .. } = event {
            println!("TELEMETRY intent={:?} phase={:?} allowed={:?} item={:?}",
                telemetry.intent, telemetry.phase, telemetry.allowed_tools, telemetry.active_work_item);
        }
    }
```
判定：断言是「测试超前于实现」还是「实现有缺口」，逐条写下结论。

- [ ] **Step 2: 按结论二选一处理**

- 属测试超前：把断言改为当前真实契约，并在注释写明该行为何时由哪个切片补齐（不得只放宽以图省事）。
- 属实现缺口：先补实现（预期落在 `execution.rs` 的遥测映射或 `delivery_workflow::tools`），保持断言不变。

- [ ] **Step 3: `delivery_workflow` 红线转绿**

```bash
cd harness && cargo test -p harness-runtime --test delivery_workflow
```
Expected: 两个测试全绿。若仍出现 `gate]`，回到 Task 3–5 找出未归类干净的分支——**禁止修改 `delivery_workflow.rs` 测试的断言**。

- [ ] **Step 4: 全量验证**

```bash
cd harness && cargo test --workspace
cd harness && cargo clippy -p harness-runtime --all-targets
cd harness && git diff --check
```
Expected: 全绿、无新警告、无空白错误。

- [ ] **Step 5: 实机三场景复跑（重放绿灯不等于真机成功）**

```bash
cd harness && MSYS_NO_PATHCONV=1 cmd /c "scripts\\build.bat package"
cd harness && python -X utf8 scripts/governance_ab_run.py --scenarios S1,S2,S3 --modes controller --profile "openai · deepseek-v4-pro"
```
Expected: `exit=0`。记录三场景各自的调用数、首次写入前调用数、`gate]`/`guard]` 计数——阶段 B 的交付率红线要用同一份口径对照。

- [ ] **Step 6: spec 状态回写 + 提交**

spec 头部状态追加「阶段 A（拦停归零）已落地；§6 交付率与 §4.3/§4.4 待阶段 B」，并在 §10 勾掉已核实的未核实项。

```bash
git add harness/harness-runtime/tests docs/superpowers/specs/2026-09-13-agent-admission-authority-consolidation-design.md
git commit -m "docs(governance): 阶段 A 收官，零拦停实测数据回写 spec"
```

## 已知风险

1. 放行重复调用后，真·死循环只剩 `TurnGovernor` 三重兜底（策略栈 / R3 成本顶 / gain 传感器）。Task 6 Step 5 的实机复跑是唯一能证伪它的检查，不可跳过。
2. 每回合单条提示可能不足以纠正模型：若实机出现同形态空转，调 `render_advisories` 的上限而不是把判断改回 `Deny`。
3. `Advise` 走 `Message::user` 注入，会被 `apply_context_budget` 计入预算并在下一回合被压缩掉——阶段 B 的 §4.3 事实级记忆处理，本阶段不重复解决。
4. 工作区含用户未提交的 `delivery_workflow.rs`/`tests/delivery_workflow.rs`，且它们正被本次改动直接修改；执行前须与用户确认这两个文件的改动是否可以入库，避免混提。
