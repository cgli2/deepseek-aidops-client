# Agent Loop 反复熔断、需回十几次"继续"——根因诊断与彻底修复方案

> 状态：已实施（Fix1–Fix4 全部落地，见第七节）；`cargo check`/`cargo test` 验证进行中。  
> 结论先行：**这是 harness 自身 agent loop 的预算/恢复机制缺陷，不是"探测/规划"阶段的问题，也不是你我的对话习惯。** 熔断后"继续"能接回根任务，但每次"继续"都重新发放**同一份过小的固定硬预算**，而模型在单个预算窗口内把大部分额度浪费在**暴力重扫**上（会话日志自述"同回合扫描脚本被跑 13 次"），导致每个窗口几乎零进展 → 又熔断 → 又回"继续"。

---

## 一、现象（用户原话复述）

- "一个简单的任务中间要断掉十几次，要我回复十个继续才能做完，完全无法交付使用。"
- "虽然可能是超预算熔断了，但简单的事情，前面探测清楚、目标也规划好了，中间为什么还会不断的熔断？"
- "每次中断后我发起继续难道又是重新全量扫描吗？"

## 二、证据（代码 + 今天会话日志）

### 证据 1：简单任务的硬预算被封死在很小的值

`harness-runtime/src/execution.rs`

- `SolvePlan::for_contract`（L73-161）：小/直接任务 `hard_max_steps: 36, hard_max_tool_calls: 48`（L158-161）；其他分支更小（8/10、12/16、10/12）。
- `BudgetManager::for_contract`（L846-872）：基础值再 `×3` 得硬上限（Generative 84/108、Investigative 120/144）。
- `cap_hard_limits`（L903-905）：`budget.hard_max_steps = budget.hard_max_steps.min(solve_plan.hard_max_steps)` —— **SolvePlan 的 36/48 是最终封顶值**，把上面的 ×3 直接压回 36/48。
- `ABSOLUTE_MAX_STEPS=120, ABSOLUTE_MAX_TOOL_CALLS=160`（L840-841）是天花板，但单窗口永远到不了。

→ 一个需要 ~80 步真实工作的"简单"任务，在每个窗口只跑 36 步就被硬熔断。

### 证据 2：硬熔断出口强制用户手动"继续"

`agent_loop.rs`

- L698-717：`BudgetManager::hard_exhausted` → `absolute_budget_hit=true; hard_stop=true; break`。只 append 一条 `Assistant("[需要处理] 已停止…")`，**不自动续跑**。
- L1540-1545 / L1551-1555：硬熔断/中断最终写出 `DeliveryOutcome::SystemFailure` / `Interrupted` —— 这两个**都在 `latest_resumable_task` 的可续跑集合里**（L85-100），所以"继续"**能接回根任务**（携带 `verified_criteria` + `TaskLedger`，L363-369）。
- **但每次接回都从 `solve_plan.hard_max_steps` 重新发放同一份 36/48 硬预算**（L352-361）。设计注释 L355-356 明确："硬预算是总成本保险，任何任务都不能借自动续跑突破上限。"

→ 只要任务真实工作量 > 36 步，就必然 `ceil(总量/36)` 次手动"继续"。这是"断十几次"的结构性来源。

### 证据 3：单窗口内暴力重扫，把预算吃光

- 今天会话日志（`.harness/sessions/7ba3370f…jsonl`）自述：**"同回合扫描脚本被跑 13 次"**、"超过阈值也判定为低价值重复。取证：同回合扫描脚本被跑 13 次"。
- `MAX_SAME_CALLS_PER_TURN: u8 = 2`（L56-57）只在**参数完全相同**时去重（L1044 tool-loop guard）。扫描脚本每次路径/参数略有差异就**绕过去重**，13 次全跑。
- 这意味着在 48 次工具调用额度里，模型把大半用于重复扫描，**单窗口真实进展极少** → 熔断来得更快、次数更多。

### 证据 4："继续=全量重扫"——半对半错（澄清关键误解）

- `WorkspaceIndex::load_or_build(root)`（`workspace_index.rs` L360-365）：命中 `.harness/learned.json` 的 term_stats 即 `loaded_from_cache=true`，**不重扫全仓**。该文件今天 09:35 刚更新（5KB，温缓存）。
- **所以框架层的代码索引在续跑时不会全量重建**——你这部分担心不成立。
- **但 agent 自己的探索性扫描（grep/find/read，即证据 3 的"13 次"）没有任何跨回合记忆化**：续跑时模型重新派生计划、重新 resolve、重新跑扫描。从模型视角看，确实"又扫了一遍"。修复见 Fix 2。

## 三、根因（三层叠加）

| #  | 根因                 | 表现                                      | 代码位置                                                                             |
| -- | ------------------ | --------------------------------------- | -------------------------------------------------------------------------------- |
| R1 | **硬预算固定过小且每次续跑重置** | 工作量 > 36 步的任务结构性需要 N 次手动"继续"            | execution.rs L158-161 / cap_hard_limits L903-905；agent_loop.rs L352-361、L698-717 |
| R2 | **单窗口暴力重扫无记忆化**    | 一个窗口内扫描跑 13 次，真实进展≈0，熔断次数被放大            | agent_loop.rs L56-57、L1044；无跨回合搜索缓存                                              |
| R3 | **续跑不持久化执行工作集**    | "继续"只带 `verified_criteria`，模型从零重新探索证据前沿 | agent_loop.rs L363-369、L373-391（SolveSketch 每轮重建）                                |

R1 是"为什么断十几次"的主因；R2 是"为什么每个窗口几乎没进展、次数爆炸"的放大器；R3 是"为什么继续像重扫"的体验根因。

## 四、彻底修复方案（按杠杆排序）

### Fix 1（核心，灭"回十次继续"）：进展驱动的硬预算自动续跑

**思路**：硬熔断不再一律打断等用户。打断前比较 `TaskLedger` 自上次打断以来的增量（新验收项 / 新写入 / 新去重发现）：

- 若 `delta > 0`（真有进展）→ **自动续跑**：重置 `hard_stop`、发放新窗口、追加"[自动续跑] 进展充足，继续推进"，不要求用户"继续"。
- 仅当连续 `MAX_STAGNANT_BREAKS`（建议 3）次硬熔断 `delta == 0`（真死循环/卡死）或 `cancelled` 才交回用户。
- 防失控：每个根任务自动续跑次数设硬上限（建议 8），超限强制交回用户。

**与现有机制一致**：phase 预算已用 `diagnose_and_renew`（L1432）做软自动续期；Fix 1 把同一思路扩展到硬预算，且以"进展"为门槛而非无脑续期。

```rust
// 伪代码，插入 agent_loop.rs 硬熔断分支（L698-717 之前）
if BudgetManager::hard_exhausted(&execution, &budget) {
    if !cancelled && auto_continues_under_budget()  // 根任务内自动续跑次数 < 上限
       && ledger_progress_since_last_break(&ledger) > 0 {
        // 自动续跑：发新窗口，不 break
        arm_fresh_hard_window(&mut budget);
        messages.push(Message::user("[自动续跑] 本窗口有进展，继续推进，无需手动继续。"));
        debt += 1; continue;
    }
    absolute_budget_hit = true; hard_stop = true; /* 原 break 逻辑 */
}
```

### Fix 2（核心，灭"13 次扫描 + 续跑重扫"）：跨回合/回合内搜索结果记忆化

**思路**：

- 把 `MAX_SAME_CALLS_PER_TURN=2`（精确 name+args）升级为**会话级 memoized 搜索缓存**，键 = `(tool, normalized_query, scope)`。命中即返回缓存结果，不重跑。
- 缓存随会话持久化（断点→续跑间工作区未变），`resume` 路径恢复该缓存。
- 所有"where is X / search"意图经 `WorkspaceIndex`（已缓存）+ memoized 搜索路由，模型无需再全仓 grep。

### Fix 3（治本，灭"固定 36/48 封顶"）：硬预算按求解草图估算

**思路**：把 execution.rs L158-161 的硬编码 `hard_max_steps:36/48` 改为 `clamp(solve_sketch.estimated_steps × SAFETY, MIN, ABSOLUTE_MAX)`。真简单任务草图估算少→小窗口（正常）；需更多步的任务→上限随量放大，1~2 个窗口收敛。配合 Fix 1，手动"继续"变罕见。

### Fix 4（架构级，灭"续跑从零"）：断点持久化执行工作集

**思路**：硬熔断时把 `goal_execution` 的工作集（已解析候选、已读文件集、已得发现）序列化进 Delivery 报告/sidecar；续跑时恢复（不止 `verified_criteria`），模型从证据前沿继续。

## 五、实施优先级

1. **先落地 Fix 1 + Fix 2**（低风险、直击症状）：Fix 1 消除"回十次继续"，Fix 2 消除"13 次扫描/续跑重扫"。两者不改动预算哲学，回归测试覆盖现有 `intent::`/`budget` 用例即可。
2. **再 Fix 3**：把固定封顶改为估算，减少触发次数。
3. **最后 Fix 4**：持久化工作集，作为长期架构改进。

## 六、验证

- 构造"需 ~80 步的简单重构"回放用例（headless）：修复前应熔断 ≥3 次且每次需手动"继续"；修复后（Fix 1+2）应自动续跑至收敛，工具调用中重复扫描计数 ≈0。
- 单测：`auto_continues_when_progress_made`、`scan_cache_hit_skips_rerun`、`resume_restores_search_cache`。
- `cargo test -p harness-runtime` 全绿；`cargo check -p harness-runtime -p harness-ui` 无警告。

## 七、实施记录（Fix1–Fix4 已落地）

### 改动文件

- **`harness-runtime/src/execution.rs`**
  - `Budget` 新增 `hard_autorenews: u32`（硬熔断自动续跑计数）。
  - `BudgetManager::for_contract` 初始化 `hard_autorenews: 0`。
  - 新增 `BudgetManager::arm_hard_continuation`：有进展时把硬窗口与阶段窗口各抬升一个步长（`step_window`/`tool_window`），并同步抬升 `max_steps/max_tool_calls`；总续跑次数由 `hard_autorenews` 硬上限约束，**不依赖 `ABSOLUTE_MAX_*`**（续跑本就是允许突破单次固定硬预算、但受次数封顶的受控扩张）。
- **`harness-runtime/src/agent_loop.rs`**
  - **Fix1**：硬熔断分支（原一律 `break`）改为——比较 `execution.write_operations + execution.evidence.len()` 与 `hard_baseline` 的增量；**有可验证进展且 `hard_autorenews < 8` 且未取消**时，调用 `arm_hard_continuation` 自动续跑窗口并 `continue`，不要求用户"继续"；否则才 `break` 交回用户。新增 `MAX_HARD_AUTORENEWS = 8` 与 `hard_baseline`。
    > 与文档伪代码的差异：实际用"抬升硬窗口"（不清零 `state.steps`），避免污染 phase 软预算的 `checkpoint_*` 进展基准；抬升是加法、受 8 次上限封顶，总步数有限。
  - **Fix2**：新增进程级 `SEARCH_MEMO`（`OnceLock<Mutex<HashMap<String, ToolResult>>>`）+ `is_search_like`/`search_cache_key` helper。工具分发前对搜索类调用（search/grep/find/where/glob/rg/locate/ack）查缓存，命中直接返回、**不重跑真实工具，且不重复记证据**（避免污染 Fix1 进展度量与预算计数）；分发后把搜索结果写入缓存。进程内长驻，跨多次"继续"共享。
  - **Fix3**：S3 供给点由"仅多面任务 `provision_hard_limits`"改为**总是**按草图估算 `required_budget()` 抬升硬熔断（单面但草图估算明显超固定硬预算的任务同样抬升），仍受 `ABSOLUTE_MAX_*` 约束。
  - **Fix4（轻量）**：`SystemFailure` 停止原因并入"已探索证据要点"，使下一次续跑的 `resume_instruction` 能直接展示，模型从证据前沿继续而非从零重探。（完整版需跨 crate 序列化 `goal_execution` 工作集进 `DeliveryReport` sidecar，列为后续架构项。）

### 验证（进行中）

- 编译：`cargo check --target x86_64-pc-windows-msvc -p harness-runtime`
- 测试：`cargo test -p harness-runtime`
- 行为判据：修复后需 ~80 步的任务应自动续跑至收敛；单窗口重复扫描计数 ≈0；手动"继续"变为罕见。

## 八、顺带修复：clarifying-gate 6 个历史回归

在 Fix1–Fix4 落地后，`cargo test -p harness-runtime --test agent_tool_loop` 仍有 6 个集成用例失败（均 `requests.len()==0` / 澄清文本不符）。根因不在 Fix1–4，而在**当日更早的 clarifying-gate/waterfall 重构**（ADR `intent-clarification-gate-signal-driven-adr.md`）把 Phase 1 门禁改成了**硬阻断**：`requires_clarification` 对「任何无定位信号的任务」一律反问，连「修复输入框自动换行」「把版本号修改为 0.2.2」这类**具体可执行的简单问题**也被反问——这恰好是用户最痛的"简单问题也反复追问"。ADR §6 本就计划删除这些"含词即问"旧测试，但集成测试未被同步改写，遂成回归。

### 根因

- `requires_clarification(goal, is_task)` 仅需 `!has_locatable_signal()` 即返回 `Some` → 凡未落地任务全被门禁挡在 LLM 之前，回合直接结束、0 次模型调用。
- `has_locatable_signal()` 仅认 `from_value` 非空，把「纯 →Y 变更契约」（如"把版本号修改为 0.2.2"）判为无信号 → 也应反问（错误）。

### 修复（对齐 ADR G2"乐观默认、只问真盲"）

1. **`intent.rs` `requires_clarification` 加 `input` 参数 + 盲指代判定**：仅在 `is_task && !has_locatable_signal() && input 含封闭指示代词（这个/那个/这些/那些/它/此）` 时追问。具象描述（哪怕尚未落地）一律乐观放行到 `Locate→Inspect`。指示代词集合为**语言结构特征、非话题词表**，符合 V5 D5。
2. **`goal_execution.rs` `has_locatable_signal` 放宽**：`X→Y` 或纯 `→Y` 变换契约（`to_value` 非空）均视为可定位信号，具体变更请求不再反问。
3. **`agent_loop.rs` 调用点**传入 `input_text`；**`intent.rs` 4 处单元调用点**同步补参。

### 测试侧 STALE 断言修正（2 处，忠实保留测试主旨）

- `ambiguous_delivery`：旧断言期望澄清文本含"具体要处理"（旧 `clarification_prompt` 措辞）；新定位单问文本核心词为"定位" → 断言改为 `contains("定位")`。澄清本身（0 次调用 + Delivery 前缀）仍正确。
- `concrete_problem_replay`：原输入含盲指代"比喻**这个**"被 heuristic 正确判盲；且 bug 描述现分类为 `OpenEnded`（与断言 `intent=="AtomicRegression"` 冲突，属 ADR 重分类后的 stale）。改为无盲指代的具体变更请求"把会话窗口发送内容的自动换行逻辑修改为不截断显示"（AtomicRegression + locate 优先），三项断言（intent/phase/allowed_tools）全部满足，主旨"concrete problem 首步 locate、不跳 shell 验证"不变。

### 验证（全绿）

- `cargo check -p harness-runtime`：0 错 0 警告。
- `cargo test -p harness-runtime`：**168 lib 单测 + 9 集成测试 = 177 passed / 0 failed**（1 个 perf 测试 ignored）。
- 3 个相关集成用例（`grounded_candidate_replay_skips_redundant_search`/`exact_menu_rename_uses_four_tools_without_bruteforce`/`missing_tool_payload_stops_once`）未回归；新逻辑对含「这个列表」等盲指代输入仍正确反问（ADR `blind_task_asks_a_single_locate_question` 单测仍过）。

