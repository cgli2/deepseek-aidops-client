# Agent 目标问题求解机制 V4

## 1. 结论

系统当前的主要问题不是熔断阈值太小，而是求解控制面存在三套互相独立的判断：

- `IntentProfile / GeneralDomainPolicy` 决定任务类型；
- `ExecutionState::tool_phase` 决定阶段和工具白名单；
- `GoalExecution` 维护另一套工作项状态与目标锚点。

它们没有共享同一个状态事实，因而会出现“目标是修复，策略却是核验”“工作区已经找到候选文件，阶段仍要求重新搜索”“工具成功但任务没有进展”等冲突。预算只能在冲突持续一段时间后停止循环，不能把错误路径校正成正确路径。

V4 的核心改造是：**只保留一个证据驱动的求解状态机作为控制源；模型只提出当前状态允许的候选动作，运行时负责计划、状态迁移、信息增益判断、自我校正和完成验收。**

## 2. 日志根因

### 2.1 具体故障被错误分类

真实会话：

> 我发送一段不长的内容，到会话窗口就会自截断换行呢？……我的原始内容是没有换行的

运行时将它编译为 `OpenEnded`，阶段却显示为 `verify`，只开放 `shell`。随后模型执行：

1. `git status`；
2. 枚举全部 GUI 文件；
3. 搜索 `user`；
4. 搜索 `role`。

4 次调用没有读取真正相关的会话渲染文件，也没有获得编辑权限，最终返回 `Blocked`。这条链路证明分类、阶段和工具集在第一次模型调用前已经错位。

### 2.2 简单改动被放大成探索任务

同一日志中的“输入优化短内容无效果”执行了 25 次工具调用，其中 12 次搜索、13 次文件读取、0 次修改，最终 `Interrupted`。这说明“成功读取了新内容”仍被视为进展，系统没有检查结果是否确认/排除了一个假设，或是否使工作项更接近验收。

“项目版本号修改为 0.2.2”执行 14 次调用，同一个编辑调用重复 4 次，8 次结果失败，最终 `Blocked`。这说明重复守卫只识别字面调用，无法识别“目标文件已经处于期望状态”这一语义完成条件。

### 2.3 工作区预扫描没有变成执行锚点

V3 的 `WorkspaceGrounder` 可以发现实体命中的文件，但扫描结果只进入提示词，没有推进 `ExecutionState` 的阶段，也没有成为 `GoalExecution` 的候选目标。后续动态白名单仍要求模型先调用 `search`，等于丢弃了已经支付成本得到的确定性证据。

### 2.4 双状态机可能互相否决

`ExecutionState` 通过全局 evidence/write 计数推进 `Locate → Inspect → Change → Verify`，`GoalExecution` 则按工作项推进 `Pending → Inspecting → ReadyToChange → Changed → Verified`。同一个调用要同时通过两套门禁；任一状态落后都会拒绝本来正确的动作，而被拒绝的动作又会产生新一轮模型纠错，形成“门禁—重试—熔断”循环。

### 2.5 完成与校正发生得太晚

当前机制主要在软预算窗口或模型停止调用工具后要求“收敛”。此时错误路径已经消耗多轮上下文。正确的校正时机应是每个动作结果返回后：若没有新增目标、没有排除假设、没有产生变更、没有验证验收项，立即切换下一条有限候选路径，而不是等到总预算接近耗尽。

## 3. 设计目标

V4 优先优化以下指标，顺序不可颠倒：

1. 验收完成率；
2. 错误完成率为零；
3. 首次有效动作命中率；
4. 无信息工具调用数；
5. 总工具调用数和总 token；
6. 安全熔断触发率。

目标回放指标：

- 单点 UI/配置回归：通常 1 次定位、1～2 次读取、1 次修改、1 次验证；
- 已给出明确文件或工作区候选命中：跳过搜索，直接读取；
- 连续 2 个动作无信息增益：在当前假设内停止，不等待总熔断；
- 简单交付在 8 次工具调用内完成或给出带证据的精确阻塞原因；
- 任何任务都不能用通用“达到熔断，请补充范围”作为终态理由。

## 4. 唯一控制模型

```text
User Input
   │
   ▼
Goal Compiler ── 缺关键槽位 ──▶ Clarification
   │
   ▼
Workspace Grounding ── 候选文件/符号 ──┐
   │                                    │
   ▼                                    ▼
Solve Graph ──▶ Active Work Item ──▶ Action Contract
                     ▲                   │
                     │                   ▼
                Correction Policy ◀── Evidence Reducer
                     │
                     ▼
              Verification Gate ──▶ Delivery Judge
```

`SolveGraph` 是阶段、允许工具、候选目标、尝试次数和完成状态的唯一控制源。旧 `ExecutionState` 只保留计量、兼容日志和交付报告，不再独立推断工具阶段。

## 5. 目标契约

目标编译不再把一个关键词直接映射为策略，而是提取以下正交维度：

```rust
struct GoalContract {
    objective: String,
    outcome: OutcomeKind,       // Answer / Diagnose / Change / Verify / Generate / Monitor
    target: TargetSpec,         // 页面、组件、文件、接口、符号、操作入口
    actual_state: Option<String>,
    expected_state: Option<String>,
    acceptance: Vec<AcceptanceCriterion>,
    constraints: Vec<String>,
    confidence: GoalConfidence,
}
```

关键规则：

- “具体对象 + 异常现象”在代码工作区中默认为 `Change`，即使用户没有显式说“修复”；
- “为什么/根因”且没有要求修改时为 `Diagnose`；
- “测试/编译/检查某结果”才是 `Verify`，不能仅凭问号或普通“检查”进入核验阶段；
- 无法确定 `Change` 还是 `Answer` 时，优先执行只读定位，定位后再决定，不允许直接进入开放式全工具探索；
- 只有缺少目标对象、实际问题或期望结果之一且运行时无法从上下文恢复时才澄清。

## 6. 求解图与工作项

```rust
struct SolveGraph {
    goal: GoalContract,
    shared_targets: Vec<TargetCandidate>,
    items: BTreeMap<WorkItemId, WorkItem>,
    active_item: WorkItemId,
    hypotheses: VecDeque<Hypothesis>,
    phase_budget: PhaseBudget,
}

enum WorkItemState {
    Pending,
    Located,
    Inspected,
    ReadyToChange,
    Changed,
    Verified,
    NeedsUserInput,
    Failed,
}

struct TargetCandidate {
    path: String,
    symbol: Option<String>,
    source: CandidateSource,    // User / Grounder / Search / Reference
    confidence: f32,
}
```

状态转换只能由证据触发：

- 工作区或搜索得到候选文件：`Pending → Located`；
- 读取确认候选文件包含目标调用链：`Located → Inspected/ReadyToChange`；
- diff 与验收项关联：`ReadyToChange → Changed`；
- 最后一次相关写入后的验证成功：`Changed → Verified`；
- 验证失败：回到具体失败工作项的 `Inspected`，不能清空整张图。

## 7. 工作区确认与候选复用

工作区确认只负责生成候选，不对“前 320 个文件无命中”做强否定。确定性规则：

1. 用户给出的明确路径优先级最高；
2. manifest、已有 git diff、最近会话命中的文件可作为低成本种子；
3. 实体命中文件直接写入 `shared_targets`，执行阶段从 `Located` 开始；
4. 只有完整索引明确无命中，或两个相互独立的定向探针均无命中，才能判定工作区不匹配；
5. 有候选时允许 `fs` 读取候选，禁止强迫模型重新 `search`；
6. 锚点约束按规范化的目录边界判断，不能用简单字符串前缀把 `src/a` 与 `src/ab` 混为一类。

## 8. Action Contract

模型每轮只看到一个活动工作项和最多两个候选动作。工具调用必须绑定：

```rust
struct ActionContract {
    work_item_id: String,
    phase: Phase,
    hypothesis_id: String,
    purpose: String,
    expected_signal: Signal,
    on_hit: Transition,
    on_miss: Transition,
    max_cost: u8,
}
```

运行时门禁检查工具类型、路径、工作项和预期信号。一次模型回复中的动作必须满足依赖关系；定位结果尚未返回时，不并行执行依赖其结果的读取或编辑。

## 9. 信息增益与自我校正

工具执行成功不等于任务进展。`EvidenceReducer` 将结果归为：

- `TargetFound`：发现新文件、符号或引用；
- `HypothesisRejected`：定向无命中或读取得到反证；
- `DataFlowConfirmed`：确认关键调用链；
- `ChangeApplied`：产生与工作项绑定的真实 diff；
- `AlreadySatisfied`：目标本来就处于期望状态；
- `VerificationPassed / VerificationFailed`；
- `NoInformation`：目录枚举、重复命中、门禁拒绝、与当前假设无关的成功输出。

校正规则：

1. `NoInformation` 不推进状态、不续预算；
2. 同一假设第一次无信息：切换备用探针；
3. 同一阶段第二次无信息：结束该假设，进入下一候选或精确澄清；
4. `AlreadySatisfied` 直接进入验证，禁止重复编辑；
5. 工具被门禁拒绝时由运行时给出合法候选，不让模型用多个回合猜白名单；
6. 每轮最多一个存在状态依赖的动作，独立验证任务才允许并行。

## 10. 分层规划

规划只细化到当前需要的层级：

- L0 目标：用户最终可观察结果；
- L1 验收项：独立交付面；
- L2 当前工作项：下一条可闭环修改面；
- L3 下一动作：一次可证伪工具调用。

简单问题不创建长计划。单工作项直接进入 L3；多交付面只创建 L1/L2 图；架构级任务才生成完整阶段计划。这样规划成本与问题复杂度匹配。

## 11. 验证与完成判定

### Change

必须同时满足：

1. 所有验收项有 `ChangeApplied` 或 `AlreadySatisfied`；
2. 每项都有覆盖关系明确的验证证据；
3. 验证时间晚于最后一次相关写入；
4. 无 `Pending / NeedsUserInput / Failed` 工作项。

### Diagnose / Answer

不要求代码写入，但必须有能支持结论的证据，并明确区分事实、推断和未确认项。模型停止输出不能自动等价为完成。

### 终态

- `Verified`：目标全部满足；
- `NeedsUserInput`：缺少一个用户才能提供的信息，问题必须具体；
- `PartialDelivery`：已有可保留修改或已完成部分工作项；
- `SystemFailure`：运行时、模型或工具本身失败；
- `Cancelled`：用户取消。

## 12. 预算

预算服务于策略约束，不负责求解：

- 单工作项阶段预算：Locate 2、Inspect 2、Change 2、Verify 2；
- `TargetFound / HypothesisRejected / ChangeApplied / Verification*` 才可消耗并推进阶段；
- `NoInformation` 连续两次直接触发校正，不等待软预算；
- 安全总预算保持不可扩张，只处理运行时缺陷；
- 达到总预算时根据工作项状态生成具体终态，不能输出通用熔断文案。

## 13. 实施状态

V4 闭环已完成，并保持旧日志可反序列化、开放式任务与 legacy 路径可回退：

1. 增强目标编译：识别“具体对象 + 异常状态”的隐式修复请求，修正 `Direct/OpenEnded/Verification` 错配；
2. 将 `WorkspaceGrounder` 命中的候选文件写入 `GoalExecution`，有候选时直接允许读取；
3. 让受控任务的允许工具、阶段与完成判定只以 `SolveGraph` 为控制源；旧 `ExecutionState` 仅保留统计和 legacy 路径；
4. 引入语义证据分类，目录枚举、门禁拒绝和重复命中不再算进展；
5. 实现完整 `ActionContract`（阶段、假设、工具、目标、预期信号、命中/未命中迁移和成本）；
6. 增加有限假设队列、阶段预算、全局无信息与校正计数；
7. GoalCompiler 产出机器可比期望值，实现 `AlreadySatisfied → Verify`，禁止重复编辑；
8. 验证失败按报错路径映射回具体工作项，保留其他已完成面的状态；
9. Session telemetry 与 UI 展示目标、活动工作项、全部工作项状态、活动假设、下一动作和校正指标；
10. 增加 `HARNESS_GOAL_EXECUTOR=legacy` 灰度回退；默认启用 V4；
11. 补齐真实回放：短文本换行首阶段、工作区实体复用、完整 Locate→Inspect→Change→Verify、版本已满足跳过编辑，以及验证失败精准回退。

## 14. 验收用例

### 精确菜单重命名

- “菜单名称修改为 B / A 重命名为 B”编译为单工作项原子替换，不进入范围探索；
- 执行链最多为 `Locate → Inspect → Change → Verify`，已有 Grounder 候选时跳过 Locate；
- OpenAI 兼容网关返回终止帧 `message.tool_calls`、旧式 `function_call` 或对象参数时均能还原工具调用；
- 上游仅声明 `finish_reason=tool_calls` 却没有工具载荷时立即报告一次协议错误，禁止把同一请求重试三次形成伪熔断。

### 短文本被错误换行

- 编译为单点 `Change`，不是 `OpenEnded` 或 `Verify`；
- 首阶段允许 `search`，若 Grounder 已给出候选则允许 `fs`；
- 不执行无关 `git status` 或全目录枚举；
- 读取会话气泡渲染链后修改并验证，或在 8 次调用内给出精确证据阻塞。

### 输入优化短内容无效果

- 两次无信息动作后必须切换假设或停止；
- 不允许 12 次搜索 + 13 次读取 + 0 次修改；
- 只有确认数据流的读取才推进到 Change。

### 版本号修改

- 读取 manifest 后若已经是 `0.2.2`，记录 `AlreadySatisfied`；
- 不重复执行同一个替换 4 次；
- 直接验证版本一致性并交付。

### 工作区不匹配

- 有限扫描无命中不能立即硬判错项目；
- 两个独立定向探针无命中后返回 `NeedsUserInput`；
- 问题包含已检查的路径、实体和需要用户确认的唯一信息。

## 15. 迁移与回退

V4 复用现有 `TaskContract`、日志事件和 `DeliveryReport`，`GoalExecution` 对外以 `SolveGraph` 暴露并成为受控任务的唯一控制源。`ExecutionState::tool_phase` 仅供开放式任务和显式 legacy 回退使用，不参与 V4 受控任务的门禁或完成判定。设置 `HARNESS_GOAL_EXECUTOR=legacy`（也接受 `off/false/0/v3`）可即时回退；不设置时默认启用 V4。
