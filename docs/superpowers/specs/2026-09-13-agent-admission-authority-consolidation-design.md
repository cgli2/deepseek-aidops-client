# Agent 准入权唯一化与证据驱动推进（治理重构阶段 3 收口）

> 状态：已评审（2026-09-13）· 决策点 D1–D4 已裁定（见 §9）· 进入实施计划
> 前序：`2026-08-31-agent-governance-redesign-design.md`、`docs/superpowers/plans/2026-09-01-governance-phase3-counter-retirement.md`、`docs/AGENT_GOAL_SOLVING_MECHANISM_V5.md`、`docs/agent-target-gate-repair-2026-09-13.md`
> 基座：继续推进工作区在途的 `harness-runtime/src/delivery_workflow.rs`，不另起控制面
> 实施切片与验收见 §5、§6；每片的代码动作以对应计划文件为准

## 1. 结论

治理重构已经把「**什么时候停**」收归 `TurnGovernor`（阶段 2 实机红线通过、阶段 3 T1–T4 与 5a/5b 已提交）。但没有收的是另一半：「**下一步允许做什么**」。它仍然散落在三处按调用次数计费的硬门禁里，任何一处都可以把一次正常动作变成一条错误工具结果。

这就是「一直空跑无果 / 预算消耗完中断 / 循环无意义确认 / 每轮重复重跑 / 定位到问题也不能交付」的共同根因：**模型对问题的理解和执行器对进度的度量，之间只靠"动作计数"这一层代理耦合**，所以两者必然漂移；漂移的表现就是执行器在模型看懂之前先把它的动作吞掉。

方案：把准入裁决收敛成**单一 `Admission` 策略**，并规定它只有两种合法输出——因**真实外部约束**拒绝（访问策略、沙箱、写冲突、用户审批），或因**证据不足**给出提示但**绝不吞掉动作**。"次数用完"不再是拒绝理由。终止权仍唯一归 `TurnGovernor`。

## 2. 证据

### 2.1 五个真实会话（`.harness/sessions/*.jsonl` 全量解析）

| 会话 | 回合 | 工具调用 | 写入 | 门禁拒绝 | 交付结果 |
|---|---|---|---|---|---|
| `bada6772`（示例日志：策略页保存报错） | 3 | 77 | **0** | 19（`target-anchor`） | 要求用户回「写入修改」，终局「执行预算已耗尽，没有产生文件改动」 |
| `5910d2c4`（任务=重构 agent 机制本身） | 7 | 92 | **0** | 47（`tool-loop` 37 / `target-anchor` 8 / `controlled-delivery` 2） | 死于此前的 tool_call 协议 400 |
| `99372653`（首句即「修一个简单 UI 问题，跑了很多轮没改成」） | 17 | 315 | 11 | **110** | 12 PartialDelivery / 5 Verified |
| `77431bf5` | 3 | 50 | 3 | 21 | 2 PartialDelivery / 1 Verified |
| `14a40bb7` | 4 | 33 | 8 | 13 | 2 PartialDelivery / 2 Verified |

两点比「0 写入」更说明问题：

- **门禁拦停并不区分成败会话。** 三个最终交付了的会话合计被拦 144 次，最坏一例 315 次调用只换来 11 次写入（近 29 次调用才落一次改动）。门禁没有换来正确性，只换来成本与回合数。
- **反证成环**：`5910d2c4` 正是被指派去修这个机制的会话，它以与示例日志完全相同的模式失败（0 写入 + 47 次拦停）。机制的失效是自我复现的，而 `99372653` 的第一句话说明同一症状已被反复报告。

同一份解析还给出：`bada6772` 里 16 个被读文件中 **12 个被重复读**，`server/routers/strategy.py` 被读 **11 次**；22/77 次工具结果为失败或拒绝（28.6%）。

值得注意：示例日志中模型其实已经推出真因（思考链里出现「strong concrete bug (NameError in publish_version」），随后仍然零变更——「定位到也不能交付」不是能力问题，是准入问题。

### 2.2 在途修复的自有红线是红的

`docs/agent-target-gate-repair-2026-09-13.md` 记录的门控修复，其回归测试当前状态（实测 `cargo test -p harness-runtime --no-fail-fast`）：

- `delivery_workflow::repair_reproduces_edits_retries_and_verifies_in_one_request` **FAILED**，断言是「任何工具结果都不得含 `gate]`」；实测拦截文本：`[execution gate] 该调用未关联任何验收标准`。
- `agent_tool_loop::concrete_problem_replay_starts_with_locate_not_shell_verification` **FAILED**（遥测 `phase=="locate" && allowed_tools==["search","fs"]` 未达成）。
- `agent_tool_loop::quoted_menu_shortening_starts_from_the_grounded_file_not_repository_search` **FAILED**（遥测 `intent/phase/active_work_item` 未达成）。

三个 mock provider 均不读请求历史（形参 `_messages`），且 `Telemetry` 在构造请求之前落盘，故与今日已修的 tool_call 协议缺陷无因果路径；它们是门控修复本身尚未落地。除上述两个二进制外，300 项单测与另外 10 个集成测试二进制全绿。

**关键定位**：`delivery_workflow` 已经为实施类任务旁路了 `goal_execution` 的阶段门禁（`agent_loop.rs:1462`、`:1477` 的 `&& !implementation_workflow`），但 `ActionGate::authorize_with_tools`（调用点 `agent_loop.rs:1552`，实现 `execution.rs:1391`，拒绝文本 `execution.rs:1418`）仍在硬拒。**门禁拆了一半。**

### 2.3 方向漂移

最近一次提交 `6fc9cbb fix(runtime): cap consecutive locate-only probes per turn` 新增了 `MAX_CONSECUTIVE_LOCATE_CALLS_PER_TURN = 10`（`agent_loop.rs:97`，用于 `:496` 的准入判断）。这是在给门禁层**再加**一个计数器，与「计数器退位」（阶段 3 步骤⑤）方向相反。缺的不是更多守卫，是把最后一层准入权交出去。

## 3. 根因（代码锚点）

**R1 准入权三处并存，互不可见。**
`ActionGate::authorize_with_tools`（`execution.rs:1391`）→ `GoalExecution::allows_tool_call`（`goal_execution.rs:1577`）→ `ToolRepeatGuard::should_block` / `LocateStepGate::allows`（`agent_loop.rs:1436`、`:1512`）。三处都能独立返回错误工具结果；没有任何一处能看到另外两处的剩余余地，也就无法判断"这次拒绝是否真的有必要"。旁路必须逐处加 `&& !implementation_workflow`，这正是拆不干净的机制原因。

**R2 预算按动作次数计费，不按信息增益。**
`PhaseBudget::default()` = `locate 2 / inspect 4 / change 2 / verify 2`（`goal_execution.rs:246`），消耗点 `phase_attempts.increment`（`:1845`），裁决点 `:1601`。跨层缺陷（前端组件 → API 路由 → 仓储 → schema）实测需要 16 个文件，而定位预算只有 6 次。且计数对"第一次读某个新文件"和"第 N 次重读同一文件"一视同仁——它既防不住空转，也保护不了必要动作。已知的 `concrete_read` 豁免（`:1596` 的 `.max(16)`）是给该模型打补丁，不是换度量。

**R3 拒绝话术教模型放弃。**
`:1602` 的文案「必须切换假设或报告精确阻塞，禁止继续同类动作」——在模型已经定位到根因、只差一次确认读取时，唯一"合规"的输出就是报告阻塞。于是「定位到问题也不能交付」被机制自身生产出来。

**R4 上下文遗忘与预算是同一缺陷的两面。**
`compress_stale_tool_results`（`agent_loop.rs:2780`）把 12 条之前的工具输出截到 300 字符，`apply_context_budget` 把旧回合压成 480 字符摘要。模型因此忘记自己读过什么 → 重读（`strategy.py` ×11）→ 烧掉 R2 的预算 → 触发 R3 的拦停。今天读不到 ≠ 没读过。

**R5 完成判定与授权语义互相打脸。**
判定要求 `write_operations > 0` 才能收敛，产出「未完成：缺少"写入修改"步骤」并要求用户回「写入修改」；而 `delivery_workflow::instructions`（`delivery_workflow.rs:23`）已明确声明原请求含写入授权、且"不能把内部缺失步骤当成用户需要补充的授权"。同一回合内两条指令相反，即「循环无意义确认」。

**R6 历史中毒会让上下文永久不可用。**
`[error] {e}` 以 assistant 文本写入日志（`agent_loop.rs:1351`）并随历史回喂；叠加已修复的搜索缓存 `call_id` 泄漏（协议 400），连续失败后上下文既非法又自增殖（示例日志尾部的 `f8f8` 乱码即模型对自身错误回显的叠加）。协议面今日已收口为唯一出口 `prepare_request_messages`；错误文本回喂面仍待处理。

## 4. 目标设计

### 4.1 单一准入策略，两种合法输出

新增 `Admission`，落在 `delivery_workflow.rs` 内（D1 已裁定：不新建 `admission.rs`，避免第四份策略文件；该模块已是"阶段仅指导、不构成写入授权"的同一语义源），作为实施/修复类任务的唯一裁决者。输出只有：

- `Deny(reason)` —— **仅限真实外部约束**：访问策略、沙箱拒绝、写冲突/非工作区路径、需要用户审批的外部副作用、工具自身 IO 错误。
- `Proceed(hint)` —— 其余一切情况，包括证据不足、疑似重复、疑似空转：**动作照常执行**，把判断作为一条提示随下一步请求注入。

约束：**任何因计数达阈值而产生的 `Deny` 都不存在**。`PhaseBudget`、`phase_attempts`、`ToolRepeatGuard`、`LocateStepGate`、`consecutive_locate` 只保留为 `TurnGovernor` 的输入信号与遥测，不再有拒绝能力。D2 已裁定：`PhaseBudget` 的字段本身保留（不删结构），只摘掉裁决用途，使 §6 的重读与拦停指标仍能就地观测。

红线（升级为全量回归断言，不只 `delivery_workflow`）：`SessionEvent::ToolResult.content` 不得出现 `gate]` / `guard]`。

### 4.2 度量换成信息增益

判定一个动作是否推进，看它是否向 `CaseFile` 新增了**事实**：新文件、未覆盖过的行区间、新的失败签名、首次成功验证。`CaseFile`（`case_file.rs`，阶段 2 已落地且与真实日志对拍过 6 项）就是这件事的现成载体，不需要新子系统。

增益为零连续 3 次（起始常量 `MAX_ZERO_GAIN_STREAK = 3`，与策略栈窗口 W 同处配置）→ 交给 `TurnGovernor` 现有策略栈 `[换路]` / 栈底常驻处置，而不是由门禁就地拒绝。成本上限仍由 R3（300k）单一顶兜底。

### 4.3 已确认事实不再被压缩掉

把「已确认目标文件 + 其关键摘录」提升为 `CaseFile` 一等事实，并：

1. 压缩豁免：这类条目不受 `compress_stale_tool_results` / `apply_context_budget` 截断（现在无差别截到 300 字符）。D3 已裁定：用户显式上传的附件摘录同享豁免——沿用 `render_attachment_context` 已有的每文件 8 000 字符上限，另加单回合豁免合计 24 000 字符，超出部分照常压缩。
2. 重读即复用：同一文件同一区间再次请求时，返回已存事实摘要 + 「如需最新内容需显式刷新」，而不是重发原文，也不重跑工具（现 `SEARCH_MEMO` 只覆盖 search 类，且必须保持今日修好的 call_id 语义）。

目标不是省 token，是消除 R4 那个"忘了自己读过"的重读循环。

### 4.4 完成判定按验收项↔证据映射

`DeliveryOutcome` 由「每个验收项是否有对应成功证据」决定，`write_operations == 0` 降为诊断字段。零写入但正确回答了"为什么报错"并给出定位结论，是 `Delivered` 的一种，而不是必须先补一次写入。反向仍成立：有写入但无验证不得 `Verified`（`delivery_workflow` 测试的 `no_op` 分支已覆盖，保持）。

### 4.5 历史层收口（部分已落地）

已完成：`enforce_tool_call_protocol` + `prepare_request_messages` 作为唯一出站关口（实时三处与回放路径共用）；搜索缓存只存载荷不存 `call_id`。
待做：provider 错误文本改为不进入模型上下文的事件（R6），或至少在重建时剥离 `[error]` 前缀的 assistant 文本。

### 4.6 收尾阶段 3 未竟事项

`ToolRepeatGuard` 降级为纯信号（5c）、移除 `GovernorMode::Legacy`（5d）、`goal_execution.rs` / `execution.rs` 冗余收缩（T6）、6 份文档标 deprecated（T7）。阶段 2 计划文件 0/59 勾选但工作已提交——补记状态行，消除"看起来没做"的流程债。

## 5. 迁移切片（绞杀者，每片须回放全绿再进下一片）

| 切片 | 内容 | 该片的可见结果 |
|---|---|---|
| S1 | 把 `ActionGate` 对实施类任务的硬拒改成 `Proceed(hint)`；判读并收敛 §2.2 三条红测试（先定"测试超前"还是"实现缺口"） | `delivery_workflow` 红线转绿；三条遥测测试自洽 |
| S2 | `deny → annotate` 全量化；`PhaseBudget` / `phase_attempts` 剥除拒绝能力；删除 `MAX_CONSECUTIVE_LOCATE_CALLS_PER_TURN` 的准入用途 | `gate]` / `guard]` 全量零出现 |
| S3 | 增益事实进 `CaseFile`；压缩豁免与重读复用 | 同文件重复读 ≤1；重复读不消耗预算 |
| S4 | 完成判定改验收项↔证据映射 | 零写入但正确定位不再被要求"补授权" |
| S5 | 5c / 5d / T6 / T7 + 10 fixture 重放红线收官 | `GovernorMode` 消失；Legacy 守卫代码不存在 |

每片独立可回滚，且都跑同一套重放红线，避免"改好了 A 症状、B 症状悄悄回来"。

## 6. 硬验收：端到端重放交付率

沿用 `harness/scripts/governance_ab_run.py` + `governance_redline_check.py`，新增交付率指标。

**fixture 集（10 个）构成修正**：实测 `harness/ab-runs/` 只有 S1/S2/S3 三个场景、各 1 次运行，且都是 2026-09-01 **验收通过**的运行而非失败样本；其中仅 S1（GUI 黑框闪烁）属非代码交付面，S2 是「这个问题解决了吗？」的追问、不可作任务，S3 属代码修复。因此集合按可得的真实语料组成：

- 现有 5 个 `tests/fixtures/`（含失败基线 `7ba3370f_*` 三片、gitfix 片与成功对照 `success_677bd6e0`）；
- 本次新增 2 个真实失败会话：`bada6772`（策略页保存报错）、`5910d2c4`（重构 agent 机制本身）；
- S1（GUI 黑框）作为第 3 个非代码面；
- 其余 2 个**必须新采集**：给 `governance_ab_run.py` 增加 S4（文档/配置类交付，验证不以编译或单测为判据）与 S5（跨仓 UI 行为类），跑出真实会话后转 fixture。D4 要求的「至少 2 个非代码面」由 S1 + 新采集满足，**不能靠现有 ab-runs 存量凑数**。

采集脚本与场景定义属 S1 切片之前的前置工作（见实施计划 Task 0）。

**红线（全部须同时满足）**：

1. **交付率**：10 个中 ≥8 个产出经验证的修改（真跑磁盘与测试，判据同 `delivery_workflow` 现有断言），其余 ≤2 个必须是 `Delivered` 且结论正确（如"零写入但已定位"）。
2. **零拦截**：任何工具结果不含 `gate]` / `guard]`。
3. **零重读**：同一文件同一区间跨回合重复读 ≤1（沿用已细化的 A2 语义，`fs` 纯读豁免改为"由 §4.3 复用兜住"，指标口径写进脚本，双侧同步）。
4. **首次写入前调用数 ≤ 12**。
5. 既有红线不劣化：provider 错误不得 `Verified`、单会话 ≤300k、失败回合 100% 带四要素资产、基线 `7ba3370f_full.jsonl` 违例集合不得缩小。

指标口径必须同时写进 `session_replay.rs` 与 `governance_redline_check.py`（同源双语义，改一处必改另一处——阶段 3 已踩过这个坑）。

## 7. 风险与对策

| 风险 | 对策 |
|---|---|
| 去掉拒绝后真死循环失去刹车 | 三重既有兜底不动：`TurnGovernor` 策略栈与栈底常驻、R3 成本顶、gain 传感器；差别是它们**换路**而不是**吞动作** |
| 模型无门禁后乱写文件 | 保留真实外部约束不拆：edit 精确锚点、访问策略、shell 审批、`artifact_baselines` 回滚检测（先改后滚回不得 `Verified`） |
| 重放 fixture 用脚本化模型，绿灯不等于真机成功 | 红线 1 之外仍须 `governance_ab_run.py --scenarios S1,S2,S3` 实机复跑；阶段 3 T8 未做正是本项缺口 |
| 提示注入过多抵消省下的往返 | `Proceed(hint)` 同类提示去重且每回合上限 1 条（现 `MAX_LOOP_RECOVERY_PROMPTS` 语义可复用） |
| 改动面大、中途不可交付 | 按 §5 切片，每片自带绿判据；S1 单独就能让当前红测试转绿 |

## 8. 明确不做

- 不新建第三个控制面，不引入新的事件/状态 schema（`SessionEvent` 保持冻结）。
- 不扩充词表、不按关键词判定业务领域（V5 第一、二原则仍有效）。
- 不在本次处理 GUI 黑框与 subagent 编排。
- 不做"更聪明的阶段划分"——阶段只作为提示内容，不作为准入条件。

## 9. 决策点（2026-09-13 已裁定）

| 编号 | 裁定 | 已写回 |
|---|---|---|
| D1 | `Admission` 扩进 `delivery_workflow.rs`，不新建 `admission.rs`，避免第四份策略文件 | §4.1 |
| D2 | `PhaseBudget` 保留字段作纯遥测，只摘除裁决用途，供 §6 指标继续观测 | §4.1 约束段 |
| D3 | 压缩豁免包括用户显式附件摘录，受每文件 8 000、单回合合计 24 000 字符上限 | §4.3 |
| D4 | 10 个 fixture 中至少 2 个为非代码交付面（文档/UI/配置），取自 `ab-runs` | §6 |

## 10. 本文已核实与未核实

已核实（本机实测）：§2.1 五个会话的回合数、调用数、写入数、门禁拒绝分类与交付结果（脚本解析全量 jsonl，脚本为一次性诊断、未落库）；§2.2 三条红测试与其失败文本；§2.3 与 §3 全部代码锚点的当前行号；今日协议修复的回归结果。

未核实：`concrete_problem_replay` / `quoted_menu_shortening` 两条遥测断言的**实际** `phase`/`allowed_tools` 取值（需要临时探针，本文档未改测试文件），已列为 S1 首项；示例日志对应会话的模型与参数配置（jsonl 未记录 profile）。
