# ADR：Agent Loop 主推进改为 Async 并发执行器

- **状态**：已实现（Phase 1，已合入 `agent_loop.rs` 运行器，编译/测试/发布构建/回放均通过）
- **作者**：cgli / WorkBuddy
- **关联**：`AGENT_GOAL_SOLVING_MECHANISM_V5.md` §6.2（G2 DAG 全并行）、§7（S5 验收：端到端耗时/漏改率/回落率）
- **触发**：用户指令「把 loop 主推进改写为 async 并发执行器」。背景：S5 机制层（G1/G2/G4/G5）已实现并接入运行器（见 `2026-08-30.md` 闭环记录），但 loop 主推进仍是**单一活动面 tick 模型**，多面任务未真正并发推进。

---

## 1. 现状事实（已查源码，非推测）

`harness/harness-runtime/src/agent_loop.rs`（2564 行）的 `run_turn_cancellable`：

1. **单一 `messages` 线程**驱动**一个** `llm.stream_with_options(...)` 流（`agent_loop.rs:756`），每个迭代 = 一次模型往返。
2. 模型返回 `tool_calls` → 对每个调用**串行**跑门禁（`link_proposal`/`action_spec`、`repeat_guard`、target-anchor gate、`locate_step_gate`、`ActionGate`、`AccessPolicy`、`PreToolUse` 钩子），通过的收集进 `pending`。
3. **`pending` 内的工具调用已用 `futures::future::join_all` 并行 dispatch**（`agent_loop.rs:1072-1106`）——**工具 I/O 层已经是并发的**。
4. 结果回写证据、`settle_static_convergence`、`ledger.verify`、push tool message。

**结论**：当前"并发"只发生在「同一次模型响应内的多个工具调用」这一层。**真正的瓶颈是模型会话层**——所有交付面只能跟着「一条共享模型对话」的节奏走；surface B 的第二步动作必须等 surface A 的那次模型往返让出后才能被模型发出。这正是 V5 §6.2「按依赖 DAG 全并行」在运行器里尚未兑现的部分。

已具备的前置能力（来自 S5）：`admit_concurrent(MAX_PARALLEL_SURFACES=4)`、`parallel_write_groups()`（同文件写冲突串行化）、`next_admitted_surface()`、`action_spec` 已允许服务 active **或** 已准入并发面、`parallel_plan()`/`concept_coverage_checklist()` 已注入提示。

---

## 2. 设计目标与硬约束

- **G**：多面任务里，N 个已准入且无写冲突的交付面**各自独立推进**，互不等待对方的模型往返。
- **约束（沿用 V5 与用户既有规则）**：
  - 机制保持**通用**，不得为项目/语言/领域硬编码；
  - 不得「空跑」：每个面复用现有门禁/`repeat_guard`/循环恢复逻辑，预算硬上限仍全局生效；
  - 单面任务**行为零变化**（回归保护）；
  - 协议不变：`assistant` 的 `tool_call` 必须在其 `tool_result` 之前（`join_all` 已保证一一配对）。

---

## 3. 方案

### 方案 A（推荐·Phase 1，低风险、立即可见）
**「每轮每准入面一次模型往返，并发 dispatch」**——保留单一协调器，但把「一轮 = 一次全量模型往返」改为「一轮 = 对 `admit_concurrent` 集合里的每个面各发一次模型往返，并发执行，结果合并」。

- 提取 `run_surface_round(surface_id, shared_state, llm, tools, hook) -> SurfaceRoundOutput`（复用现有门禁/dispatch 逻辑，只是 `action_spec`/`link_proposal` 已按面作用域）。
- 协调器对准入集合 `join_all` 并发跑各面 round；同写冲突组（`parallel_write_groups`）内的面按组顺序跑，组内并发。
- 每面维护自己的局部 `messages` 线程（作用域化提示 = `render_for_model` 过滤到该面 + 该面的 `parallel_plan` 切片 + 相关 `concept_coverage_checklist` 行），round 结束再合并进统一 `log`/`messages`。
- **优点**：真正并发推进多面、复用全部现有门禁、回归面小、工具层并行已验证。
- **代价**：每轮 N 次模型调用（成本/首字延迟上升），跨面「同一响应内协同」被弱化为「轮间共享 `GoalExecution` 状态」。

### 方案 B（Phase 2，完整架构，高风险）
**「每准入面一条常驻 tokio 任务，各自独立流 + 共享状态 Mutex 化」**——真正持久并发。

- 引入 `ConcurrentSurfaceScheduler`：`Arc<Mutex<GoalExecution>>`/`ExecutionState`/`TaskLedger`/`BudgetManager`/`RepeatGuard`；`SessionLog` 改为可并发 append。
- 每个准入面 spawn 一条常驻任务，跑自己的 `llm.stream_with_options` 循环，按 `step_attributed` 收敛到该面；协调器 `join!` 各任务、按轮重准入、处理 compact/length 恢复/取消传播。
- **优点**：完全兑现 V5 §6.2「DAG 全并行、独立节奏」。
- **代价**：2564 行函数大改、共享状态锁纪律、协议配对/重放/取消/压缩的并发化、回归风险高。建议**默认关闭、feature flag 守护**，验证后再开。

### 决策建议
先落地 **方案 A**（Phase 1）——它已经把「模型回合级并发」补上，且单一面回退路径不变、风险可控；方案 B 作为后续独立任务，在 A 跑通且量化指标达标后再做。

---

## 4. 共享状态并发模型（方案 A/B 共通）

| 对象 | 现状 | 并发处理 |
|---|---|---|
| `goal_execution` | `&mut` | `Arc<Mutex<>>`；面循环只改自己面 items + 全局 ledger/budget（加锁段） |
| `execution` / `ledger` / `budget` / `repeat_guard` | `&mut` | 同上，`Arc<Mutex<>>` |
| `llm` (`&dyn LlmProvider`) | `&self` | 已可并发（`stream_with_options(&self)`），跨任务复用 |
| `tools` (`ToolRegistry`) / `hook` (`&dyn Hook`) | 已在 `join_all` 内并发调用 | 已验证并发安全 |
| `log` (`SessionLog`) | 单线程 append | 包 `Arc<Mutex<>>` 或改用 append-only 并发结构 |
| `messages` | 统一线程 | 每面局部线程，轮边界合并（保序） |

---

## 5. 风险与缓解

- **R1 共享状态竞态** → 细粒度 Mutex + 明确锁纪律；面只改自身 items。
- **R2 协议配对** → 每面独立线程，合并时保持该面 `tool_call→tool_result` 连续。
- **R3 空跑** → 复用现有 `repeat_guard`/循环恢复/预算硬上限；某面达终态/恢复耗尽即独立停止。
- **R4 通用性** → 作用域提示由同一 `GoalExecution` render API 生成，无领域硬编码。
- **R5 单面回归** → `admit_concurrent().len() <= 1` 时**完全走原单循环路径**，行为不变。
- **R6 compact/length 恢复/重放** → 留在协调器层；面循环「每轮一次模型往返」回报，协调器对合并历史做 compact/恢复。

---

## 6. 验证计划

- **单元/集成（沿用现有 harness-runtime 测试基座）**：
  - `three_surface_plan()` 驱动方案 A 协调器，断言：3 面在≤2 轮内全部 Verified（对比旧单循环需 3 轮）；`parallel_write_groups` 同文件面被串行化。
  - 单面任务走原路径，断言输出/事件序列与改造前逐字节一致（回归基线）。
  - 取消传播：cancel 时每面补占位 tool result，无孤儿 `tool_call`。
- **量化（V5 §7 三项指标）**：脚本化多面求解 e2e（无真实 LLM，用 replay/local provider 在临时目录写产物）跑出 **端到端耗时↓、漏改率↓、回落率持平**；对比 S5-e2e 基线数字。
- **编译/冒烟**：`cargo check --workspace` 零错误；`cargo test -p harness-runtime` 全绿；`aidops-desktop --headless` 回放 `session finished.`。

---

## 7. 待确认决策点

1. **范围**：仅 Phase 1（方案 A）/ Phase 1+2（方案 B，feature flag）/ 直接上 Phase 2？
2. **每轮模型调用数上升的取舍**：接受 N 次调用换独立性，还是保留「单响应多工具并行」为主、仅对无写冲突的就绪面并发开新轮？
3. **开关**：是否默认开启并发执行器（单面回退保证安全），还是先 flag 守护？

> 决策已于会话内确认：**范围 = Phase 1（方案 A）+ 成本优先**；并发执行器默认开启，单写冲突组（最常见）退化为原单流路径做零回归保护；多写冲突组时对每组并发开一轮作用域化模型往返。

## 8. 实现说明（as-built）

实现未采用 ADR 原描述的 `run_surface_round` 辅助函数 + 每面局部 `messages` 线程合并，
而是用**更低风险**的「融合流」 realization，二者在「模型回合级并发」目标上等价：

- 在 `agent_loop.rs` 的 step 构建处（`llm.stream_with_options` 一行）改为分支：
  - `parallel_write_groups().len() <= 1`（或 `!controlled_delivery`）→ 原单一流，**字节一致**；
  - 否则对每个写冲突组（上限 `MAX_PARALLEL_SURFACES=4`）并发创建一条
    `llm.stream_with_options` 流，其提示由 `goal_execution.render_for_model_scoped(group)`
    作用域化（只列该组面 + 聚焦标记，但仍含全局 `parallel_plan` 与跨面漏改清单），
    再用 `futures::stream::select_all` 融合为单一 `ChunkStream`。
- 现有 `s.next()` 循环（门禁 / `repeat_guard` / `locate_step_gate` / `ActionGate` /
  `AccessPolicy` / `PreToolUse` 钩子 / 写冲突串行）/ `join_all` 并行 dispatch /
  结果回写 **完全未改动**，因此单面路径零回归、共享状态无竞态。
- 并发只发生在「模型 I/O」这一 I/O 密集层；门禁与状态仍是串行单线程，符合 R1/R2/R5。
- `render_for_model_scoped` 由 `render_for_model` 抽出公共 `render_with_filter` 模板实现，
  单面作用域（`scope.len()<=1`）直接退化为 `render_for_model`，保证字节一致。

### 运行时证明：e2e 注入缝 + 脚本化 LLM（已补，闭合"打包后还是原样"缺口）

此前闭环报告只能证明「并发代码路径可执行（headless exit 0）」，但**从未在运行时真正驱动
多面并发的 loop**——`run_turn_cancellable` 总是用 `UserInput` 现场重建单面 `GoalExecution`，
而并发分支要求 ≥2 个 `ReadyToChange` 交付面（带候选目标文件），测试无法注入。

**注入缝**（`agent_loop.rs:296`）：
```rust
pub async fn run_turn_with_goal_execution(
    &self, ctx: &AppContext, input: UserInput, goal: GoalExecution,
) -> Result<()> {
    self.run_turn_cancellable(ctx, input, CancellationToken::new(), Some(goal)).await
}
```
`run_turn_cancellable` 第 4 参 `injected_goal: Option<GoalExecution>` 在 `Some` 时跳过现场重建、
直接用注入的多面 `GoalExecution`；`None` 时（原 3 个调用点 controller/subagent/run_turn）行为不变。
同步的 3 个调用点（controller.rs / subagent.rs / run_turn 包装）均传 `None`，默认路径零回归。

**e2e 测试** `harness-runtime/tests/concurrent_executor_e2e.rs`（`MultiSurfaceLlm` 实现
`LlmProvider` 驱动整条 `run_turn`，无需真实 key）：
- 构造 4 交付面，前 2 面改 `shared.tsx`、后 2 面改 `other.tsx` → `parallel_write_groups` 返回 2 组；
- 通过 `run_turn_with_goal_execution` 注入 `ReadyToChange` 多面 `GoalExecution`；
- 脚本 LLM 从系统提示解析 `·本轮聚焦 item-x/item-y]` 标记，对每组发一个 `edit` 调用；
- 断言：(1) ≥2 次模型调用携带 `本轮聚焦`（并发多组分支确实执行）；(2) `edit` 被派发到 ≥2 个
  不同文件（`shared.tsx`/`other.tsx`）——证明**跨组并行 dispatch**；(3) 回合正常结束无 panic。

这条测试把「打包后真的并发推进多面」从机制层单测升格为**运行期证据**，直接回应"还是原样式"痛点。

### 已知限制（诚实边界）

- **真实-key 量化指标仍待采集**：headless 回放走 replay 回落，仅证明并发闭环代码路径可执行
  （exit 0，`[harness] session finished.`）。**现已补脚本化 LLM 的 e2e 证明并发分支在运行期
  确实开多组作用域流并跨组并行 dispatch**（见上节），但 V5 §7 三项**量化**指标
  （耗时↓/漏改↓/回落持平的*具体数字*）需在配置真实 DeepSeek key 后用真实模型跑一遍 e2e 才能产出。
  属测量问题，非「是否并发」的正确性问题。
- **归属仍走全局 round-robin**：融合流里各组的 tool_calls 汇合后，经既有
  `link_proposal`/`action_spec` 按准入集 round-robin 归属（与既有多面行为一致），
  作用域提示只影响"模型发什么调用"，不直接锁死"调用归哪面"。属 Phase 1 取舍，非回归。
- **`concept_coverage_checklist` 非字节确定（已修复）**：原 `build_concept_registry` 遍历
  `HashMap` 致漏改概念/面排序漂移；已在 `missing_coverage_report` 内对符号与漏改面排序，
  并在 `concept_coverage_checklist` 外层排序加符号 tie-breaker，输出现**字节确定**。
  新增 `missing_coverage_report_is_order_independent` 与 `concept_coverage_checklist_is_byte_deterministic`
  两个确定性回归测试坐实。
- Phase 2（方案 B：每面常驻 tokio 任务 + `Arc<Mutex>` 共享状态）仍属后续独立任务。

## 9. 验证证据（2026-08-30）

- `cargo check -p harness-runtime`：零错误。
- `cargo test -p harness-runtime`：**163 lib + 9 + 1 + 1 + 1(e2e) 全绿**（`concurrent_executor_e2e`
  单独一个测试二进制，断言 ≥2 个写冲突组各开一轮作用域流 + `edit` 跨组并行派发到 ≥2 文件）。
  新增并发测试 `parallel_write_groups_serializes_shared_file_and_splits_independent`、
  `render_for_model_scoped_degenerates_for_single_surface_and_focuses_multi`，
  确定性回归测试 `missing_coverage_report_is_order_independent`、
  `concept_coverage_checklist_is_byte_deterministic`。
- `cargo test -p harness-runtime --test concurrent_executor_e2e -- --nocapture`：
  `concurrent_executor_opens_multiple_scoped_streams ... ok`（运行期并发分支实证）。
- `cargo build --release --target x86_64-pc-windows-msvc --bin aidops-desktop`：成功。
- `aidops-desktop --headless` 回放：`[harness] session finished.`（exit 0）。
