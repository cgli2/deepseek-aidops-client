# Agent 精准交付与抗空转改造计划

## 目标与验收

将当前“模型自由决定下一步、运行时事后拦截”的循环替换为运行时主导的求解闭环。对一个明确、可观察的回归，Agent 必须以最少的定位、读取、修改和验证动作完成；无法继续时输出可恢复的 `Blocked`，而不是泛搜、耗尽上下文或虚报完成。

验收指标（先在模拟工具链固定，再在真实回放验证）：

- 明确单点回归：首次高信号定位后不允许无范围的第二次搜索；目标为不超过 8 次工具调用、6 次 LLM 请求。
- 每次工具调用必须关联一个假设、预期信号、当前验收项和下一阶段条件；连续两次没有新信号，停止探索并切换假设或 `Blocked`。
- 所有结束路径均落盘真实交付状态、剩余验收项和证据；`TurnEnd`、计划文本、模型自述均不能表示成功。
- `finish_reason=length` 不得原样重试：恢复请求必须显著缩小上下文并降低该请求预算；第二次同类失败给出结构化阻塞报告。

## 根因

现有 `harness-runtime/src/execution.rs` 的 `SolvePlan` 以关键词决定模式，示例“删除再添加后旧状态未刷新”会落到宽松的 `ScopedDelivery`，而非单点回归。`ActionGate` 只在工具调用生成后拦截，且除原子模式外不约束“为什么调用、预期看到什么、何时停止”。预算续期与进展延展没有总上限，导致不断更换关键词/读取范围仍可扩张。

`harness-runtime/src/agent_loop.rs` 会重建较长会话、技能、项目事实和工具上下文；长度恢复虽删除普通历史，却保留整个系统提示集合，仍可能接近模型上限。当前按字符而不是具体模型的上下文窗口和输出预留预算，且非原子任务沿用较大的 reasoning/output 设置。

## 架构

```
用户输入 + 当前工作区事实 + 已验证记忆
  → IntentCompiler
  → TaskLedger / HypothesisPlanner
  → PhaseController → ToolSelector → 受限 LLM 执行
  → EvidenceLedger → DeliveryGate / 学习卡
```

### 1. IntentCompiler：先理解问题，而不是先搜索

新增 `harness-runtime/src/intent.rs`，从输入生成确定性 `IntentProfile`：任务类型、对象、动作、失败前后对照、可观察症状、范围、风险及精确度。

“导航路径 + 先后操作 + 旧状态/不刷新/不同步 + 期望更新”应归为通用的 `TargetedStateSyncRegression`，而不是靠某个 UI 文案命中。它生成：目标表面、状态变更、观察点和最短候选调用链，而不是针对“模型下拉框”的特判。

高精确度任务不让模型创建自由计划：运行时初始化 `Locate → Inspect → Change → Verify`，默认禁止 `plan`、`delegate` 和目录枚举。

### 2. TaskLedger 与 HypothesisPlanner：计划是状态机，不是聊天文本

新增 `harness-runtime/src/task_ledger.rs`。一个 `LedgerItem` 包含验收项、阶段、状态（`Pending/Active/Evidence/Verified/Blocked`）、证据和阻塞原因。现有 `PlanUpdate` 仅作为模型的建议，不能修改该状态。

`HypothesisPlanner` 最多创建 2–3 个可证伪假设。每项包含：

- `hypothesis_id`、待解释的症状、最小作用范围；
- 唯一的首个探针、预期信号、命中/未命中后的转移；
- 允许工具、最大尝试数和所支持的验收项。

运行时在每次结果后判定“是否得到新信号”。无信号的同类动作不能用新关键词绕过；只能转移到下一个假设或生成 `Blocked`。

### 3. PhaseController 与 ToolSelector：调用前限制，而非调用后拒绝

在 `execution.rs` 拆出阶段与动作模型；在 `agent_loop.rs` 每一步根据阶段动态过滤发送给 LLM 的工具 schema：

| 阶段 | 可用工具 | 退出条件 |
| --- | --- | --- |
| Locate | 一个高信号 `search`，受目标目录/符号限制 | 命中文件/符号 |
| Inspect | 最小区间 `fs read`，最多紧邻调用链 | 能区分假设 |
| Change | `edit`/受控写入 | 成功写入 |
| Verify | 相关 `shell` 检查/测试 | 有成功验证证据 |
| Conclude | 无工具 | 所有项已验证或明确阻塞 |

`ActionProposal` 扩展为 `hypothesis_id`、`expected_signal`、`scope`、`phase`、`information_gain`。`ActionGate` 负责强制这些不变量；模型生成不合法调用不会获得第二轮“纠错式思考”。同一阶段仅允许可并行且彼此独立的只读探针。

### 4. 上下文快照与记忆：保留已知事实，不重放旧噪声

新增 `harness-runtime/src/context_snapshot.rs`：模型上下文只包含当前任务目标、活动假设、未满足验收项、已确认事实、最近一个工具结果和允许动作。旧会话不直接重放；只从 `ConversationMemory` 检索带工作区指纹、路径/符号、Git 版本和置信度的相关“解决卡”。

新增 `SolveCard` 到 memory provider：问题指纹、命中符号/路径、有效探针、无效假设、验证命令、结果和失效条件。文件哈希或相关 Git 变化后自动降权/失效，避免把旧结论当事实。

### 5. 长度与空响应恢复状态机

新增 `harness-runtime/src/recovery.rs`，区分 `OutputTruncated`、`ReasoningExhausted`、`ProtocolEmpty` 和 `ProviderFailure`。恢复只能进行一次，且必须改变请求：

- 按模型 `context_window` 估算 token，预留系统/工具/输出空间；未知模型按保守窗口处理；
- `OutputTruncated`：只保留紧凑快照并降低输出上限；
- `ReasoningExhausted`：关闭/降低 reasoning 并要求一个动作或最终结论；
- `ProtocolEmpty`：一次协议级重试；
- 同类第二次失败：写入 `DeliveryOutcome::Blocked`，附 finish reason、上下文估算、最后成功动作和恢复建议。

成功产生正文或工具结果后重置相应恢复计数。恢复不得保留全部系统提示并再额外追加 checkpoint。

### 6. 双层预算与可观测性

保留阶段软预算，但新增不可续期的总预算：LLM 请求数、工具调用数、无新证据次数、总时长、输入/输出 token。达到总预算后禁止新探索；若已写入则保留一次验证机会，随后交付 `Verified` 或 `Blocked`。

新增持久化 `ExecutionTelemetry` 事件：意图精确度、阶段转移、假设命中/淘汰、工具信息增益、上下文窗口/估算 token、输出预算、reasoning 档位、finish reason、恢复分支和总成本。UI 展示简短状态，详细记录用于回放和优化，不能占用模型上下文。

### 7. Council 与 UI 统一

Council 的每个节点映射到同一 `TaskLedger`；超时、降级、失败只可进入 `Blocked/Interrupted`，绝不能作为 `Done` 进入成功门禁。UI 用 Ledger 投影显示“当前假设、阶段、已验证/阻塞项”；模型计划只显示为建议。

## 实施顺序

1. 先增加 intent/ledger/phase 的纯 Rust 类型及状态迁移单测，不改变现有工具执行。
2. 接入 `AgentLoop`：从 `IntentProfile` 初始化 ledger，使用阶段工具白名单和假设门禁替换宽松的 ScopedDelivery 分支。
3. 将 `PlanTool` 与 Council 投影到 ledger；保留旧 `PlanUpdate` 兼容读取，移除其对真实进度的影响。
4. 接入 `ContextSnapshot`、模型窗口预算和恢复状态机；替换当前 length 重试分支。
5. 接入 `SolveCard` 检索/沉淀、Telemetry、UI 进度投影及 feature flag（先灰度 `HARNESS_SOLVE_CONTROLLER=v2`）。
6. 通过回放评测达到指标后再将 v2 设为默认；保留一次可观测的 v1 回退开关。

## 关键文件

- 修改：`harness/harness-runtime/src/agent_loop.rs`、`execution.rs`、`facts.rs`、`council.rs`
- 新增：`harness/harness-runtime/src/intent.rs`、`task_ledger.rs`、`context_snapshot.rs`、`recovery.rs`、`telemetry.rs`
- 修改：`harness/harness-session/src/log.rs`、`harness/harness-tool/src/plan.rs`、`harness/harness-llm/src/{lib.rs,model_catalog.rs,deepseek.rs,openai_compat.rs}`
- 修改：`harness/harness-provider-memory/src/assets_native.rs`、`harness/harness-ui/src/gui/{app_state.rs,workspace.rs}`

## 验证

新增 deterministic fake-LLM / fake-tool 端到端回放，至少覆盖：明确状态同步回归、未知范围调查、两次无信息搜索、重复读取、写入后验证、预算硬上限、部分输出 length、连续 length、reasoning-only、成功后恢复计数归零、模型窗口不足、取消、崩溃恢复、Council 超时。

CI 执行 `cargo test --workspace`，并输出每个回放的 LLM 请求数、工具数、token、耗时、验收完成率和误报完成率。针对明确回归，断言不发生目录泛扫、无重复成功调用、不会超过总预算，且最终状态必须是 `Verified` 或带原因的 `Blocked`。
