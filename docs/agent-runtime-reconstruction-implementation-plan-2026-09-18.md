# Agent 任务执行机制重构实施方案

> 状态：实施中；第 7 节记录当前代码结果与尚未完成的验收项。本文的 P0–P5 完成门槛仍是最终基线，不能把当前测试通过等同于整个重构完成。
>
> 分析基准：2026-09-18 当前工作树；会话日志最新至 2026-09-16。`agent_loop.rs`、`execution.rs`、`goal_execution.rs`、`intent.rs` 和 `tests/delivery_workflow.rs` 有未提交改动，实施时必须保留并重新评估这些改动。

## 1. 目标与边界

目标是让一个用户目标在同一个任务生命周期内被正确理解、推进和验收：Agent 能自行完成技术定位；找到实现后能改动；预算或模型故障后能从有效断点恢复；只有证据覆盖目标时才报告完成。用户只有在产品语义、风险边界或外部授权确实需要其决定时才被询问。

本次重构聚焦交互式 Agent 主链路：输入解释、任务契约、定位、工具准入、执行、预算、恢复、交付。Council、监控演进和其他插件仅需要适配同一交付状态，不在第一阶段重写。保留 `SessionLog` 作为可重放的事实源；不要另建一套与日志竞争的持久化真相源。

“彻底解决”的可检验含义如下：

1. 已知失败会话能在固定回放中正确终止；尤其不能把搜索命中、文件读取或绿色构建误记为 UI 故障已修复。
2. 明确的修复请求不因用户没给文件名而要求用户代做代码定位。
3. 同一任务的暂停和恢复保留目标、已检查文件、失败假设、有效写入及验收状态；失效证据才重查。
4. 每次停止都落盘唯一、结构化、与用户可见结论一致的交付报告。
5. 总成本有上限；达到上限时诚实暂停或阻塞，不凭重复搜索换取无限预算。

## 2. 取证结论与现有方案的缺口

| 现象 | 证据 | 机制缺口 |
| --- | --- | --- |
| “新建项目”确定按钮看不到，首轮零编辑却 `Verified`，下一轮用户反馈仍不可见 | `.harness/sessions/62b946c8-1c5d-4735-a64a-78b661d6b6ff.jsonl`；`execution.rs` 的 `GeneralDomainPolicy::select_strategy`、`requires_verification`、`can_complete` | 故障陈述可能走 `Direct/OpenEnded`；完成门禁没有稳定地继承用户要“修好可观察故障”的要求。构建、搜索和代码存在性也不能验证视觉结果。 |
| 同一混合会话 25 回合中出现 40 次搜索、9 次 `NeedsUserInput`、8 次 `Interrupted`、4 次 `SystemFailure` | `.harness/sessions/7ba3370f-fcbe-4993-b50a-f89f750ba929.jsonl`；`agent_loop.rs` 的澄清、grounding、预算和出口分支 | 技术定位失败与真正需要用户决策没有清晰区分；多个守卫能改变停止原因。这个会话包含多个任务，数字用于说明故障形态，不能当作单任务成功率。 |
| “提交一下代码”先 27 步仍 `PartialDelivery`，下一次“提交代码”才 `Verified`；简单解释 Git 警告又反复空转 | `.harness/sessions/05470692-653a-41b5-ba07-add985ac9cb7.jsonl` | 特殊任务路由与通用求解图、完成判定脱节；分类修补只能覆盖某些措辞。当前未提交代码已对解释类问题做局部修复，仍需回放验证。 |
| 续跑仍可能重复扫描或复用旧结果 | `agent_loop.rs::restore_resume_frontier`、`SEARCH_MEMO`、`search_cache_key` | 前者重放历史成功的搜索/读取但不按当前文件版本验效；后者只按会话和参数缓存，没有写后失效机制。 |
| 文档中的“已修复”“全绿”不能证明真实交付 | `docs/agent-loop-interruption-root-cause-and-fix.md`、`harness/ab-runs/20260915-171909/summary.md` | 前者以预算/搜索补丁为主，后者三个 `PartialDelivery` 被标为“全绿”；缺少与用户可观察结果绑定的端到端成功判据。 |

当前源代码还存在两组需要统一的控制权：`GoalExecution` 与 `ExecutionState` 都维护完成/阶段投影；`TurnGovernor`、硬预算、响应恢复、模型无动作守卫和 `TurnStopping` 都可能结束循环。`harness/INVARIANTS.md` 声称“唯一终止检查点”和“预算耗尽是终态”，应在改造中与实际控制流重新对齐。

## 3. 目标架构：一份任务记录、一个裁决入口

```text
用户输入/附件
    ↓
任务解释：确定目标、操作意图、约束、可观察验收项
    ↓
TaskRecord（由 SessionLog 事件重建的唯一执行投影）
    ↓
下一动作裁决 → 工具调用 → 结构化工具结果 → 更新 TaskRecord
    ↑                                            ↓
    └──────────── 未完成且有合法下一动作 ──────────┘
                                                 ↓
                              单一终止/交付裁决 → DeliveryReport
```

### 3.1 任务契约

对现有 `TaskContract` 做增量演进，不另造并行的“任务计划”。至少要有：

- `task_id`、原始输入、后来补充的输入、附件引用、工作区身份；
- `requested_outcome`：`Answer`、`Diagnose`、`Change`、`Verify`、`RepositoryOperation`，以及无法确定时的 `Undetermined`；
- 明确的用户约束和每项验收标准；每项标注证据需求，例如文件净变化、特定测试、运行行为或视觉检查；
- 需要用户决策的问题，必须携带已观察事实和“不同选项会改变什么”。文件路径、代码入口、测试命令属于 Agent 要解决的技术问题。

规则：词表和 `IntentProfile` 只提供候选，不直接授予“只读”“可完成”或“需要追问”的最终权限。对“按钮看不到”“点击后闪黑窗”“旧状态未刷新”这类症状，即使没有“修复”一词，也应结合对话上下文识别用户是否要求解决故障。可使用一次结构化模型解释，校验输出；解析失败时进入有界定位与观察，不将未知任务默认为可直接回答。新用户消息先与当前任务关系匹配：补充、纠错、询问状态、新目标；不能仅靠“继续”前缀决定是否续跑。

### 3.2 执行状态与证据

在 `TaskRecord` 中按验收项记录 `Pending → Located → Inspected → Changed → Verified`；只读任务走 `Pending → Investigated → Verified`。`NeedsUserDecision`、`Paused`、`Blocked`、`Cancelled` 是明确的停止原因，不伪装为 `Verified`。状态迁移只由工具事实和验证器驱动，模型文字只能提出候选结论。

每条证据至少绑定 `criterion_id`、工具调用、目标文件/资源、工作区版本或文件哈希、观察时间、成功/失败、证据类型。文件写入后，依赖旧文件内容的定位、读取和验证证据自动失效；失败的 shell 命令和无命中搜索可用于排除假设，不能用于完成验收。`write_operations` 只说明发生过写入，不能代替“写对了目标”；`cargo check` 只证明编译，不能代替 UI 行为或视觉验收。

交付裁决采用一个入口，例如 `evaluate_delivery(&TaskRecord) -> DeliveryDecision`。其必要条件为：

- `Change`：每项验收有相关净变化或精确幂等证明，且有与该项匹配、在最后一次写入之后取得的验证证据；
- `Diagnose`：有足以支持根因和排除主要替代解释的读取/运行证据；
- `Verify`：实际执行了指定检查，并保留退出码及关键输出；
- `Answer`：问题的答案可由用户给出的材料或已读取来源支持；
- `RepositoryOperation`：按 Git 实际状态和操作结果验收，不能映射到源码修改阶段。

若无法自动验证视觉或运行行为，应报告“已修改、代码检查通过、行为尚未验证”，不能标 `Verified`。报告中的完成状态、验收项及用户可见结论必须从同一裁决结果生成，防止前后矛盾。

### 3.3 下一动作与工具准入

将 `ExecutionState::tool_phase`、`GoalExecution::allowed_tools`、`ActionGate` 和 `TurnGovernor` 对下一动作的判断收敛为一个 `next_action` 函数。它返回当前验收项、目标假设、允许工具及具体原因。工具 schema 与实际准入使用同一结果，避免模型看不到可用的恢复工具，或模型看得到却被后端拒绝。

定位阶段先用用户提供的路径、符号、界面入口和已有调用链；成功定位后优先读取目标实现。两次同类动作没有新的区分性证据时，换假设或报告技术阻塞。读到目标实现后，修改工具必须可达；若目标仍有歧义，允许一次有明确依赖理由的定向读取。不要用简单的“搜索次数”强行迫使改动错误文件。

### 3.4 预算与恢复

把预算合并为任务总成本与短进展窗口两层。总成本覆盖模型请求数、工具调用数、token、耗时及长任务控制面的全局配额；窗口只用于检查是否获得有效进展。进展限定为新的目标定位、能够改变判断的诊断、净写入、新的验收证明，不包含换关键词搜索、重复读取或同一命令的再次成功。

预算触顶前先落盘检查点。检查点包含当前任务契约、各验收项状态、已验证与已否定的假设、文件哈希、最近有效工具结果、剩余成本及停止原因。已有授权和成本额度内可以内部续行；总额耗尽时转为 `Paused/Blocked` 并给出可恢复状态，不自动获得无界新预算。取消和超时也必须形成完整报告；控制器的异常出口不能只写 `TurnEnd`。

搜索结果复用应按工作区身份、查询、范围及文件版本判定；工作区修改后失效相关结果。优先把“已定位哪些文件、为什么”的精简事实写入检查点，避免长驻进程级缓存成为第二套隐形状态。恢复时只重查失效证据，不回放所有旧搜索作为当前事实。

## 4. 分阶段实施与代码落点

每阶段单独提交、单独回放；前一阶段验收不过，不继续叠加补丁。

| 阶段 | 实施内容与主要文件 | 完成门槛 |
| --- | --- | --- |
| P0：固定基线 | 在 `harness/harness-runtime/tests/` 加入从失败日志脱敏提炼的确定性回放；建立同一输入下的任务类型、工具轨迹、最终报告断言。记录当前工作树与干净 `HEAD` 的差异，避免把未提交修复当作历史行为。 | “零编辑假绿”“反复澄清”“定位后无法编辑”“恢复重扫”“空响应”“取消无报告”均有修复前可复现的失败断言；保留已有成功用例。 |
| P1：修任务契约与完成裁决 | 调整 `intent.rs`、`execution.rs`、`delivery_workflow.rs`、`agent_loop.rs`。显式区分回答、诊断、修复、验证、Git 操作；将终态计算集中到一个纯函数。 | 全部失败回放不再假绿；每个 `Verified` 的验收证据均可追溯到目标及最后写入后的验证。 |
| P2：统一动作状态 | 以 `goal_execution.rs` 的工作项为基础合并 `execution.rs` 的阶段/完成投影；`task_ledger.rs` 只作 UI 展示投影。统一工具展示与准入，移除旧分支对停止/完成的直接写入权。 | 搜索命中→读取→编辑→验证链路可达；任何拒绝都有下一步合法动作或结构化阻塞原因。 |
| P3：持久化断点 | 在 `harness-session/src/log.rs` 增加版本化任务/检查点事件与兼容读取；`agent_loop.rs` 从日志重建当前执行前沿；`workspace_index.rs` 与搜索缓存按版本失效。 | 进程退出后可恢复；已验证项不重做，变更文件的旧证据不复用；相同未变更工作区不全量重扫。 |
| P4：预算与出口合并 | 调整 `execution.rs::BudgetManager`、`governor/`、`agent_loop.rs`、`controller.rs`、`long_horizon.rs`。统一停止优先级、总成本与进展窗口；所有出口生成一次报告。同步修订 `harness/INVARIANTS.md`。 | 预算、超时、取消、Provider 错误、工具失败都有一致报告；无人工反复“继续”才能完成预算内任务。 |
| P5：灰度与清理 | 用回放和真实受控任务对照新旧机制。通过后移除旧完成/阶段守卫、过期特殊词表与无版本缓存；保留一版可观测回退开关，再删除回退路径。 | 达到第 5 节指标，且没有安全/权限回归。 |

迁移期间可保留旧公开 `DeliveryOutcome` 枚举以兼容 UI 和日志。内部先使用更明确的 `Completed/Paused/NeedsUserDecision/Blocked/Cancelled` 裁决，再做显式映射；不能让不同模块各自重新解释 `PartialDelivery`。旧日志缺少文件哈希时一律把相关读取证据标为待复核，不能当作已验证事实。

## 5. 回放矩阵与发布门槛

回放只保留必要的输入、工具结果和状态转换；会话中的个人路径、附件和秘密信息先脱敏。至少覆盖以下场景：

| 场景 | 必须观察到的结果 |
| --- | --- |
| “确定按钮看不到”，搜索、读取和 `cargo check` 成功但零相关改动 | 不能 `Verified`；应继续修复或诚实报告阻塞。 |
| 已修改 UI，但仅编译通过、未验证按钮可见 | 报告已改动和未验证项，不声称视觉故障已解决。 |
| 定位并读取了目标文件 | 编辑操作可达；不要求用户提供 Agent 已发现的路径。 |
| 用户说“继续”“还是没好”“修好了吗” | 分别恢复、修正当前任务或回答当前状态；不得丢失根目标或自动开启无关新任务。 |
| 同一搜索在文件未变与已变两种情况下重试 | 前者可复用定位事实；后者重新验证，不能返回陈旧命中。 |
| 模型空包、截断、纯文本空转、工具失败、取消、超时 | 有限恢复后形成一个准确的终态报告，工具调用协议完整。 |
| 多验收项任务只完成一项 | 保留已验证项，列出剩余项；不得整体 `Verified`。 |
| Git 提交、纯解释、只读诊断 | 使用对应验收条件，不进入源码修复闭环。 |

发布硬门槛：固定失败集的错误 `Verified` 为 0；所有终止路径有且只有一份一致的 `DeliveryReport`；明确目标的回放不因缺文件名触发无关澄清；修改后再验证的顺序得到机器断言；预算内可完成的回放不需要用户反复输入“继续”。同时记录每任务模型请求数、工具调用数、重复搜索数、有效写入数、已验证验收项数、人工追问数、总 token 与耗时，以同一批任务比较改造前后。`cargo test` 全绿是必要条件，不是替代这些行为门槛的交付结论。

真实 UI 验证须使用可重复的操作和截图/可观察状态；若暂时没有自动化能力，就明确把视觉验收留为待验证，不能用编译结果代替。灰度期间同时观察旧日志兼容、长任务控制面预算、取消传播和工具权限；任何假绿或文件写入越界立即回退新机制，并保留失败记录用于修复。

## 6. 实施时的约束与交付物

- 每个 PR 附：变更范围、对应失败回放、通过/未通过的验收项、与当前未提交改动的关系。不要把工作树已有改动覆盖掉。
- `SessionLog` 是重建来源；`TaskRecord`、`TaskLedger`、`CaseFile`、缓存和 UI 状态都是可重建投影，不能独立宣布交付完成。
- 不通过增加更多关键词、更多自动续期次数或更长系统提示作为主要修复手段；这些只会延后错误终止。
- 文档中“已实施”“全绿”须对应可运行命令及真实结果；缺少行为验证时标为“待验证”。
- 最终交付包括：实现代码、版本化日志迁移、失败回放与成功回放、实际运行指标、更新后的不变量文档、回退说明。

相关既有设计：`docs/AGENT_DELIVERY_REFORM_PLAN.md`、`docs/AGENT_GOAL_SOLVING_MECHANISM_V5.md`、`docs/agent-loop-interruption-root-cause-and-fix.md`、`docs/agent-target-gate-repair-2026-09-13.md`。本方案以真实交付裁决和单一执行状态为优先级，吸收这些文档中的有效能力，但不把其“已实施”记录视为本方案的验收证明。

## 7. 当前实施记录（2026-09-18）

本轮在原有未提交修改上增量开发，未回退它们。实际完成的是最危险交付路径的第一批防护，不是 P0–P5 的全部门槛。

| 阶段 | 当前结果 | 剩余工作 |
| --- | --- | --- |
| P0 | 新增“按钮不可见但只有搜索/读取”“修改后只跑编译”“视觉故障跑无关单元测试”回放；现有运行时测试通过 | 反复澄清、恢复重扫、取消/超时、跨进程恢复和多验收项还缺完整端到端回放与指标基线 |
| P1 | `TaskContract` 增加请求结果与证据类型；修复任务未写入不能完成；视觉问题不接受静态字串、编译或普通单元测试作验收；`ExecutionState` 与 `GoalExecution` 均执行该门禁 | 自然语言解释仍使用保守信号，未知请求未接入结构化模型解释；缺少真正的视觉/行为验证器和纯函数式统一交付裁决 |
| P2 | 新增统一 `next_action`：受控任务的模型工具 schema、后端准入、遥测和完成门禁都取自同一 `GoalExecution` 阶段；纯文本空转校正明确给出已授权的下一动作 | 旧 `ExecutionState` 阶段投影仍为非受控/Git 回退路径服务，尚未做成唯一状态对象 |
| P3 | 搜索缓存改为单回合局部缓存，观察到写入即失效；续跑时旧 `fs.read` 视图须与当前磁盘精确一致，旧搜索结果不再无版本重放。新增 v1 `TaskCheckpoint` 事件，将已验收项关联文件 BLAKE3 指纹；变更任务续跑只恢复指纹仍匹配的验收项，旧日志保守重验 | 检查点尚未覆盖否定假设、剩余成本和精确任务关系；未做跨进程多验收项端到端恢复测试 |
| P4 | 控制器异常/超时/取消出口补一份结构化报告，已有报告不重复写；取消优先于预算停止原因。软预算续期严格受 `max_renewals` 限制；软窗口和最终收尾窗口均不能越过任务硬步数/工具调用总额；到达硬额立即停止，不再因进展或可重试编辑自动续跑 | 尚未统一模型 token、耗时和长任务控制面的任务级总成本；旧出口归一化仍可能丢失具体暂停/故障类型 |
| P5 | 尚未开始灰度或真实 UI 任务对照 | 需完成剩余 P0–P4 后再做发布判定与回退演练 |

验证：`cd harness && cargo test --workspace --offline --quiet` 通过（含 1 项既有 ignored）；`cargo fmt --all -- --check` 未通过，输出涉及大量与本次无关的既有未格式化文件，因此本轮未执行全仓格式化。没有真实 UI 操作与截图证据，故不能声称“新建项目”按钮视觉问题已经由本次机制改造解决。

### 7.1 第二轮（2026-09-18，P1+P2 裁决单一化，进行中）

按第 4 节顺序先做 P1 剩余项“纯函数式统一交付裁决”。本轮只新增文件，未改动上述 13 个 WIP 文件，以免与上一轮未提交改动混淆。

新增 `harness/harness-runtime/src/delivery_decision.rs`：

- `StopCause` 把“为什么停止”从 7 个交织布尔量收敛为单一枚举，并显式区分预算停止与 `HardStop`（前者映射 `SystemFailure`、后者映射 `Interrupted`，与改造前出口语义一致，不顺手改用户可见状态）。
- `DeliveryFacts` 是回合结束时对执行投影的只读快照；`evaluate_delivery(&DeliveryFacts) -> DeliveryDecision` 为纯函数，不读时钟、不访问工作区、不写日志。
- `evidence_covers_requirement(requirement, verification_only, tool_signature)` 收编此前在 `execution.rs:726` 与 `goal_execution.rs:1910` 各自硬编码的同一视觉/行为规则，作为唯一判定点；`Visual` 恒为 false，因为 Runtime 目前没有可重复的视觉观察器。
- 逐项结论 `CriterionVerdict` 与 `DeliveryDecision::into_report()` 同源生成，报告不得出现 `outcome=Verified` 而验收项 `satisfied=false`。`Undetermined` 无自证口径，缺证据时一律不判完成。

新增 `harness/harness-runtime/tests/delivery_decision.rs`：按第 5 节回放矩阵中可由纯函数判定的行写断言（零编辑假绿、只编译通过、改而未验证、多验收项部分完成、取消优先、provider 错误、预算触顶、技术阻塞不改写为追问、硬熔断保持 `Interrupted`、只读回答、Git 操作、裁决确定性）。

尚未完成：`agent_loop.rs:2550-2628` 的十路出口链、`ExecutionState::can_complete`/`delivery_report`、`CompletionJudge` 与 `GoalExecution` 门禁仍各自判定，即“移除其余分支直接宣布完成的权限”这一步没有落地。（注：十路出口链已在 §7.2 接入单一裁决；`can_complete`/`delivery_report` 仍作为 fail-closed 降级防线保留，未做成唯一状态对象。）

**本轮验证状态：未验证。** 会话期间执行环境拒绝运行 `cargo`（权限分类器持续不可用，只读 `git status`/`git diff` 可用），因此上述新模块与测试**从未编译或运行过**，仅经过人工静态核对（已据此修正 match 穷尽性与跨字段闭包捕获两处会编译失败的问题，并补回 `Interrupted` 语义）。按第 6 节要求，这里不得记为“通过”。另外 `lib.rs` 中 `pub mod delivery_decision;` 是本轮唯一的既有文件改动，且该模块当前无人引用。

> 更正（2026-09-19，见 §7.2）：上述新模块与测试已在可运行 `cargo` 的环境中首次真实编译并通过；“未验证”状态作废。

### 7.2 第三轮（2026-09-19，P1+P2 十路出口链接入单一裁决，已完成本步）

本轮先补上第二轮遗留的验证，再落地 §7.1 列为“尚未完成”的核心一步：让 `agent_loop.rs` 回合终态由 `evaluate_delivery` 唯一裁决。

验证补做（第二轮的新模块从未编译过）：

- `cargo test -p harness-runtime --test delivery_decision --offline` → 17 passed；`delivery_decision.rs`（src+tests）首次真实编译并通过，第二轮的“未验证”作废。
- 静态核对发现的 `HardBudgetCeiling` 原因文本不含“预算”，会被下游 `concise_incomplete_status` 的预算分支漏判，本轮改为“已达到任务硬预算上限（步数/工具调用总额）……”；该文本不被 `delivery_decision` 测试断言，改动安全。

接入改动（仅动既有文件 2 处，未回退任何 WIP 改动）：

- `execution.rs`：`requires_verification` 由私有改为 `pub(crate)`，供裁决采集事实（唯一改动，1 行）。
- `agent_loop.rs`：删除回合末尾十路 `if/else` 出口链（原 `2550-2628`），改为 `derive_stop_cause(...)` 把取消/provider 错误/求解图自认完成/需用户决策/空转/有改动未验证/硬预算/硬熔断/软窗口按**与原链完全一致的优先级**收敛为单一 `StopCause`；`build_delivery_facts(...)` 从 `ExecutionState`+`GoalExecution` 采集只读事实（`has_execution_evidence` 直接复用 `GoalExecution::has_execution_evidence()`，与旧 `needs_user_input()` 门禁同源）；`evaluate_delivery(&facts)` 产出 `DeliveryDecision`，回合只消费 `decision.outcome()` 与 `decision.status_reason()`。
- 现在**唯一能宣布 `Completed/Verified` 的地方是 `evaluate_delivery`**：`CompletionJudge`/`GoalExecution` 只提出候选（`solver_claims_complete`），`ExecutionState::delivery_report`/`can_complete` 退为 fail-closed 降级防线（只能把 `Verified` 降为 `PartialDelivery`，不能反向升级）。这落地了 §7.1“移除其余分支直接宣布完成的权限”。

已知取舍：原 `absolute_budget_hit` 分支会把“已探索证据”键名拼进停止原因（Fix4 轻量续跑提示）。该提示仅在 Legacy 模式存活（On 模式下原因被 `concise_incomplete_status` 覆写），且无任何测试断言；本轮为保持“原因文本单一来源”将其去除，续跑前沿改由 P3 的 `TaskCheckpoint`（文件指纹）承载。

验证（可运行命令与真实结果）：

- `cargo build -p harness-runtime --offline` → 无 error、无新增 warning。
- `cargo test -p harness-runtime --offline` → 全绿：lib 326 passed/1 ignored；`agent_tool_loop` 22 passed（含 `provider_error_never_delivers_verified`、`exhausted_location_does_not_ask_the_user_to_locate_code`、`premature_text_then_menu_rename_finishes_in_one_user_turn`、`document_implementation_cannot_finish_when_edit_tool_changes_nothing`）；`delivery_decision` 17 passed；`delivery_workflow` 7 passed（git 提交/纯解释 Verified、隐形按钮与零净变化 not Verified、repair 闭环）；其余测试文件全绿。
- `cargo test --workspace --offline` → 本次运行无失败；`cargo fmt` 仅规范化本轮新增/改动代码，未触碰既有未格式化文件。

发现但未处理（越出本方案范围，建议单独修）：`harness-capability/src/index.rs` 的 `make_workspace()` 用 `subsec_nanos()` 命名单临时目录，全工作区并行压测时偶发目录名碰撞，导致 `first_index_upserts_all`/`deleted_file_drops_postings_and_checkpoint` 间歇性失败（单独 `-p harness-capability --lib` 或 `--test-threads=1` 恒过，40/40）。这是测试隔离缺陷，会污染第 5 节“`cargo test` 全绿”发布门槛，但与交付裁决无关。

仍未完成（P1–P5 剩余门槛，非本步范围）：`delivery_report` 尚未直接消费 `DeliveryDecision::into_report()`（仍保留 `can_complete`/`read_only_verified` 第二套投影，只降级不升级）；未接入结构化模型解释；无可重复的视觉/行为验证器（`EvidenceRequirement::Visual` 恒不自证）；跨进程多验收项端到端恢复、任务级 token/耗时总成本、灰度与真实 UI 对照（§5 视觉验收仍为“待验证”）均未做。没有真实 UI 操作与截图证据，不能声称“新建项目”按钮视觉问题已由本次机制改造解决。

### 7.3 第四轮（2026-09-19，P1 报告单一来源 + P3 持久化断点补齐，已完成本步）

**P1（终态报告单一来源）**：终态 `DeliveryReport` 不再由 `ExecutionState::delivery_report` 独立投影，改为直接消费 `DeliveryDecision::into_report()`；控制器归一化只覆写 `outcome`/`reason`，不再重建 criteria/verification。`can_complete` 降级为 fail-closed 防线：当裁决给出 `Verified` 但 `execution.can_complete()` 为假时，强制降级为 `PartialDelivery`（只降级、绝不升级）。同时修复只读 `Investigated` 验收项证据为空的缺口——`evaluate_delivery` 在 criteria 映射里，对无 verification 记录但已读取证据的只读项回填 `read_evidence`。新增 `read_only_verified_report_carries_evidence` 断言只读 `Verified` 报告的 `criteria[0].evidence` 与 `verification` 非空（`delivery_decision` 由 17→18 测试）。

**P3（持久化断点补齐）**：`TaskCheckpoint` 升级到 v2，新增三个 `#[serde(default)]` 字段——`rejected_hypotheses`（已被证据否定的假设描述）、`remaining_steps`、`remaining_tool_calls`（落盘时任务硬预算剩余）。兼容读取：旧 v1 检查点缺少新字段时回落到 serde 缺省，`validated_resume_report` 接受 `version ∈ {1,2}`。`task_checkpoint()` 从 `goal.items` 收集 `attempts>0` 且 `Rejected` 的假设（排除 `default_hypotheses` 里预置 Rejected 的占位项），并从 `Budget` 计算剩余成本。续跑消费：重建执行前沿后调用 `GoalExecution::restore_rejected_hypotheses` 把已否定假设回灌（活跃假设被否定时前移到下一个未否定项；全部否定时按有界重开语义复位首项），并在 v2 检查点存在时用 `remaining_*` 收紧新窗口硬上限（取“本回合重估”与“上回合剩余”的较小者），使续跑在原始总额内守恒而非重获无界预算；v1 检查点不收紧，避免把旧日志误判为总额耗尽而立即硬停。

验证（可运行命令与真实结果）：

- `cargo build -p harness-runtime -p harness-session --offline` → 无 error、无新增 warning。
- `cargo test -p harness-session --offline task_checkpoint` → `task_checkpoint_round_trips_without_affecting_old_events` 通过（v2 往返 + v1 原始 JSON 兼容反序列化，新字段回落缺省）。
- `cargo test -p harness-runtime --offline --lib -- cross_process_resume restore_rejected_hypotheses change_checkpoint_restores` → 3 passed：`cross_process_resume_restores_verified_criteria_and_conserves_budget`（日志落盘→`open_latest` 重开模拟新进程→检查点 v2/剩余成本/否定假设完整往返；已验证且文件未变项不重做，未完成项继续，变更文件旧证据不复用）、`restore_rejected_hypotheses_skips_already_excluded_paths`、`change_checkpoint_restores_only_matching_file_versions`。
- `cargo test -p harness-runtime -p harness-session --offline` → 全绿：runtime lib 328 passed/1 ignored（较上轮 +2）；`delivery_decision` 18 passed；`agent_tool_loop` 22、`delivery_workflow` 7 及其余测试文件全绿；session 全绿。

已知取舍与边界：`rejected_hypotheses` 承载的是 `default_hypotheses` 的通用假设描述，语义粒度较粗；其价值主要在于满足检查点内容契约并阻止把上一回合已排除的主假设当作全新方向重试，精确的“已定位哪些文件、为什么”前沿仍由 `restore_resume_frontier`（按当前磁盘校验的成功 read/search 回放）与文件指纹承担。“精确任务关系匹配”属 P1 意图分类范畴，不在 P3 检查点内。

### 7.4 第五轮（2026-09-19，P4 任务级总成本统一 + 出口类型不丢失，已完成本步）

**任务级总成本统一**：此前模型 token 散落在 `Usage` 事件与 `turn_prompt_tokens`、耗时无聚合、控制面步数/工具调用总额只在 `Budget`。本轮在回合入口建立单一成本累加（`turn_started_at: Instant`、`turn_model_requests`、`turn_total_tokens`），在唯一终态出口收敛成一份 `TaskCost` 快照写入遥测。`ExecutionTelemetry` 新增 `#[serde(default)]` 字段 `model_requests`/`total_tokens`/`elapsed_ms`，旧日志缺省回落 0；`append_telemetry` 重构为 `build_telemetry(.., cost: Option<TaskCost>)`，中间遥测成本恒为缺省、只有终态填真实值，避免与终值混淆。这直接为 §5 发布门槛“记录每任务模型请求数、工具调用数、总 token 与耗时，以同一批任务比较改造前后”提供结构化来源。

**出口归一化保留具体暂停/故障类型**：`decision.outcome()` 已能区分 `SystemFailure`（Blocked/预算暂停）、`Interrupted`（硬停）、`PartialDelivery`、`NeedsUserInput`、`Cancelled`、`Verified`；但控制器归一化（spec §4.2 两出口）会把前三者对用户收敛为 `PartialDelivery`，具体类型在 `else` 分支被 move 掉而丢失。本轮在归一化前留存 `specific_outcome` 标签，并在唯一终态出口写入遥测新字段 `terminal_outcome`，使“基础设施失败/被中断”与“部分交付”在诊断与指标上仍可区分，用户可见 outcome 的收敛行为不变。

**INVARIANTS.md 同步**：新增“唯一交付裁决”“版本化断点与续跑守恒（v2）”“任务级总成本与出口类型不丢失”三条不变量，并把结尾免责声明更新为“唯一裁决/版本化断点/任务级总成本已落地并测试覆盖；结构化模型解释、可重复视觉/行为验证器、任务关系精确匹配、灰度真实 UI 对照尚未实现，视觉验收仍为待验证”。

验证（可运行命令与真实结果）：

- `cargo build -p harness-runtime -p harness-session --offline` → 无 error、无新增 warning。
- `cargo test -p harness-runtime --offline --test agent_tool_loop provider_error_never_delivers_verified` → 通过：provider 错误回合的公开 outcome 非 Verified，且终态遥测 `terminal_outcome == "SystemFailure"`、`model_requests >= 1`（证明具体故障类型未被收敛丢失、成本已计入）。
- `cargo test -p harness-session --offline` → `telemetry_event_survives_json_round_trip` 通过（成本字段往返 + 缺字段旧遥测兼容回落 0）。
- `cargo test -p harness-runtime -p harness-session --offline` → 全绿：runtime lib 328 passed/1 ignored；`agent_tool_loop` 22、`delivery_decision` 18、`delivery_workflow` 7 及其余测试文件全绿；session 全绿。

仍未完成（P4 余量 / P5 / P0，非本步范围）：`BudgetManager`/`governor`/`controller.rs`/`long_horizon.rs` 的停止优先级与软/硬窗口尚未做结构性合并（本轮只统一了成本计量与出口类型保留，未改停止裁决顺序）；“无人工反复‘继续’才能完成预算内任务”需灰度真实任务对照，暂无自动化证据（待验证）。P5 可重复视觉/行为验证器缺失，`EvidenceRequirement::Visual` 恒不自证，§5 视觉验收仍为“待验证”，不得以编译代替。P0 反复澄清/取消超时/指标基线等确定性回放尚未补齐。

### 7.5 第六轮（2026-09-19，P0 确定性回放补齐，已完成本步）

对照 §5 回放矩阵与 P0 验收（line 96）逐一核对六个具名失败场景的确定性回放覆盖，发现五个已由既有测试承载、仅“取消无报告”缺端到端回放，本轮补齐：

| P0 具名场景 | 承载测试（均可运行） |
| --- | --- |
| 零编辑假绿 | `delivery_decision::zero_edit_symptom_task_is_never_verified`、`session_replay::red_lines_symptom_task` |
| 反复澄清 | `session_replay::red_lines_clarification_loop`（R2：同一澄清文案会话内不得出现第二次）、`session_replay::governor_mode_terminates_clarification_loop_without_asking` |
| 定位后无法编辑 | `agent_tool_loop::exhausted_location_does_not_ask_the_user_to_locate_code`、`document_implementation_cannot_finish_when_edit_tool_changes_nothing` |
| 恢复重扫 | `agent_tool_loop::repeated_search_replays_from_memo_without_denial`、`agent_loop::change_checkpoint_restores_only_matching_file_versions`、`agent_loop::cross_process_resume_restores_verified_criteria_and_conserves_budget`（P3 新增） |
| 空响应 | `agent_tool_loop::empty_provider_response_is_retried_without_polluting_session_history`、`missing_tool_payload_stops_once_instead_of_three_empty_retries` |
| 取消无报告 | **本轮新增** `agent_tool_loop::cancellation_mid_turn_emits_exactly_one_cancelled_report` |
| 多验收项只完成一项 | `delivery_decision::multi_criterion_partial_completion_is_not_verified`（P3 跨进程回放亦覆盖） |
| Git/纯解释/只读诊断 | `delivery_decision::repository_operation_is_accepted_by_command_not_write`、`read_only_answer_completes_without_write`、`delivery_workflow` git 提交/纯解释 Verified |

新增回放 `cancellation_mid_turn_emits_exactly_one_cancelled_report`：用一个永不产出的 `PendingLlm`（`futures::stream::pending`）配合**预取消**的 `CancellationToken`，使 `tokio::select!` 确定性命中取消分支；断言回合收敛成**恰好一份** `Delivery`、outcome 为 `Cancelled`（不被归一化改写成 `PartialDelivery`），且以 `TurnEnd` 闭合（否则 UI 轮询死循环）。这是“取消传播→单一报告”的端到端证据，补齐了此前只有纯函数 `cancel_wins_over_complete_evidence` 的缺口。

验证（可运行命令与真实结果）：

- `cargo test -p harness-runtime --offline --test agent_tool_loop cancellation_mid_turn` → 1 passed。
- `cargo test -p harness-runtime -p harness-session --offline` → 全绿：runtime lib 328 passed/1 ignored；`agent_tool_loop` 23 passed（较上轮 +1）；`session_replay` 15、`delivery_decision` 18、`delivery_workflow` 7 及其余测试文件全绿；session 全绿。

仍未完成（P0 余量 / P5）：**指标基线**（§5 发布门槛“以同一批任务比较改造前后”的每任务模型请求数/工具调用数/重复搜索数/有效写入数/已验证项数/人工追问数/总 token/耗时）——结构化计量字段已在 P4 落地（遥测 `model_requests`/`total_tokens`/`elapsed_ms` + 既有 `step`/`tool_calls`/`verified_count` 等），但“改造前后对照”需要在真实任务批上实跑采集，无法由单元测试伪造，**标为待验证**。P5 可重复视觉/行为验证器仍缺失，`EvidenceRequirement::Visual` 恒不自证，§5 视觉验收仍为“待验证”。

### 7.6 第七轮（2026-09-19，P5 可行性评估：视觉验证器与灰度清理，结论=待验证，不伪造）

P5 的验收（line 101）是“用回放和真实受控任务对照新旧机制；**通过后**移除旧完成/阶段守卫、过期词表与无版本缓存；保留一版可观测回退开关，再删除回退路径”。本步只做**可行性评估**并如实定级，不实施清理、不伪造视觉验收。

**视觉/行为验证器可行性 = 当前不可行（缺运行时能力）**。证据：`delivery_decision.rs::evidence_covers_requirement` 对 `EvidenceRequirement::Visual` 恒返回 `false`，并在源码注释明确“Runtime 目前没有可重复的视觉观察器：截图、界面树和实机操作都不在工具结果里”。全仓 grep 确认 `harness-runtime/src` 与 `harness-tool/src` 无任何截图/界面树/像素比较能力（`agent_loop.rs:4073` 的 `screenshot.png` 只是测试附件路径，非验证器）。要落地一个可重复视觉验证器，需要新增：①一个能产出确定性可视观察的工具/Provider（harness-ui 为 egui/eframe 桌面端，需截图或无障碍树探针）；②golden 图像/结构差异比较协议；③把该观察接入 `EvidenceRequirement::Visual` 的自证路径。这是一项独立特性（新工具+新 Provider+新验证协议），不是测试补齐，且依赖真实 UI 自动化基础设施，超出本轮“交付裁决单一化”的范围。

**因此当前正确状态是 fail-closed**：视觉类验收一律不自证 `Verified`，只报告“已改动+未验证项”，等待人工确认。该硬边界已被 `delivery_decision::compile_and_static_evidence_do_not_certify_visual_or_behavior` 与 `zero_edit_symptom_task_is_never_verified` 机器断言，并有 `delivery_workflow` 的“隐形按钮 not Verified”回放覆盖。**不得以编译通过、源码字串或无关单元测试冒充视觉验收**（§5、§6 line 122/129）。

**灰度对照与指标基线 = 待验证**。§5 发布门槛要求“以同一批任务比较改造前后”的每任务指标与“预算内可完成的回放不需要用户反复输入‘继续’”。P4 已把成本计量结构化落到遥测，但真实灰度对照需要：实机运行 `dist/aidops-desktop.exe`、一批受控任务、采集改造前后指标。本环境无真实任务批与 UI 自动化，无法产出可复核的对照数据，**标为待验证**，不伪造“达标”。

**旧守卫/回退开关清理 = 暂不执行（前置条件未满足）**。P5 明确“通过后移除”。灰度对照既为待验证，则 `GovernorMode::Legacy` 逃生门、`can_complete`/`delivery_report` 的 fail-closed 第二套投影、旧阶段守卫都**必须保留**——它们既是回退安全网，也是当前唯一裁决的反向升级防线。在灰度真实通过前删除它们会违反方案自身的时序，并移除尚未赚得的回退路径。

结论：P5 的视觉验证器与灰度清理**不具备在当前环境如实完成的条件**，按 §6 line 129 与既有记忆约束标为“待验证”，不以编译或单元测试替代。可交付的部分（fail-closed 视觉边界的机器断言）已在 P1/本轮覆盖并保持全绿。




