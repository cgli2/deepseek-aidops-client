# Agent 目标执行框架 V3

## 1. 问题定义

当前失败不是单纯的预算不足，而是控制权放错了位置：模型在每一轮自由决定下一次工具调用，运行时只在调用生成后做白名单、重复和总量检查。只要模型没有形成正确的代码路径，10 次、30 次或 100 次预算都会以不同速度到达同一个 `Blocked`。

框架的首要指标必须从“没有无限空转”改为“验收项真正完成”。工具数是成本约束，不是求解器。

针对以下示例：

> 管理后台 → 多端拼装 → 应用档案；列表、新增、编辑没有展示 appCode 和 subAppCode。

正确的运行时行为应是：识别两个字段和三个交付面，确认当前工作区包含目标模块，生成三个工作项，优先定位共享的数据模型或页面配置，逐项完成修改，最后执行一次能覆盖三项的验证。若工作区不含目标代码，应在少量确定性探针后询问准确的项目路径，而不是运行 30 次后给出通用熔断信息。

## 2. 设计原则

1. **运行时持有计划，模型提供候选。** 模型不能直接控制循环，只能在当前工作项允许的动作集合中提出候选动作。
2. **验收项拥有独立状态。** 不能用全局 `write_operations > 0` 推动整个任务进入验证。
3. **每个动作必须可证伪。** 工具调用必须包含目的、预期信号、命中转移和未命中转移。
4. **负结果也是证据。** 搜索无命中会淘汰假设，不允许换同义词无限搜索。
5. **澄清由证据触发。** 只有语义缺失或工作区身份不匹配时才追问，并带上已确认事实和一个具体问题。
6. **完成优先于调用数。** 预算按工作项和阶段分配；安全总上限只作为最后保险，不能作为主要控制器。
7. **终态必须可行动。** 结束只能是 `Verified`、`NeedsUserInput`、`PartialDelivery`、`SystemFailure` 或 `Cancelled`；禁止无上下文的通用 `Blocked`。

## 3. 总体架构

```text
User Request
    │
    ▼
GoalCompiler ──低置信度──▶ ClarificationGate
    │
    ▼
WorkspaceGrounder ──身份不匹配──▶ NeedsUserInput
    │
    ▼
PlanGraphBuilder
    │  Goal → AcceptanceCriteria → WorkItems
    ▼
WorkItemScheduler
    │
    ▼
HypothesisController → ActionSpec → Tool
    ▲                         │
    └──── EvidenceReducer ◀───┘
    │
    ▼
VerificationGate → DeliveryJudge → Verified / actionable terminal state
```

LLM 只参与三个受限位置：从自然语言提取结构化目标、为当前假设提出候选路径、解释无法由确定性逻辑判断的工具结果。状态迁移、预算、工具许可和完成判定全部由运行时负责。

## 4. 核心数据模型

```rust
struct GoalContract {
    objective: String,
    navigation: Vec<String>,
    entities: Vec<String>,
    expected_state: String,
    constraints: Vec<String>,
    confidence: f32,
}

struct AcceptanceCriterion {
    id: String,
    surface: String,
    expected_state: String,
    verification_scope: String,
}

struct WorkItem {
    id: String,
    criterion_id: String,
    state: WorkItemState,
    candidate_targets: Vec<TargetRef>,
    active_hypothesis: Option<String>,
    attempts_by_phase: PhaseAttempts,
    change_evidence: Vec<EvidenceRef>,
    verification_evidence: Vec<EvidenceRef>,
}

enum WorkItemState {
    Pending,
    Grounding,
    Locating,
    Inspecting,
    ReadyToChange,
    Changed,
    Verified,
    NeedsUserInput,
    Failed,
}

struct ActionSpec {
    work_item_id: String,
    phase: Phase,
    tool: String,
    arguments: Value,
    purpose: String,
    expected_signal: String,
    on_hit: Transition,
    on_miss: Transition,
    hypothesis_id: String,
}
```

没有 `ActionSpec` 的工具调用不执行。模型生成非法调用时，运行时不再通过新的模型回合反复纠错，而是使用 `on_miss` 转移到下一假设或澄清状态。

## 5. GoalCompiler：把输入编译为目标图

GoalCompiler 不通过单个动词决定任务模式，而是提取：

- 导航上下文：`管理后台 / 多端拼装 / 应用档案`；
- 实体或高信号符号：`appCode / subAppCode`；
- 交付面：`列表 / 新增 / 编辑`；
- 实际状态：页面不可见；
- 期望状态：两个字段在三个交付面可见；
- 范围与风险；
- 各字段置信度。

编译结果为三个验收项，而不是一个笼统的 `user-objective`：

1. 列表展示 appCode 和 subAppCode；
2. 新增表单展示并提交 appCode 和 subAppCode；
3. 编辑表单展示、回填并提交 appCode 和 subAppCode。

仅当关键槽位缺失时进入澄清：

- 缺少目标对象：询问具体页面、模块或路径；
- 缺少可观察现象：询问实际结果和复现步骤；
- “优化/改进”没有验收口径：询问期望指标；
- 不询问运行时能够从工作区确定的文件名、框架或实现细节。

## 6. WorkspaceGrounder：先确认项目身份

很多长时间空转实际是工作区、分支或子项目不匹配。执行任何业务搜索前，运行时用最多两个确定性探针建立工作区快照：

1. manifest、语言和前端入口；
2. 高信号实体或导航词是否存在。

结果分流：

- 命中实体：进入目标文件定位；
- 只命中导航：沿路由、菜单或页面注册追踪组件；
- 两者均未命中：停止工具调用，返回 `NeedsUserInput`，明确说明“当前工作区未找到 appCode/subAppCode 或应用档案入口”，并询问正确项目/子目录；
- 禁止把工作区不匹配当成“继续全仓搜索”的理由。

## 7. PlanGraphBuilder：按交付面建工作项

计划不是模型输出的一段文本，而是运行时可恢复的图：

```text
共享定位：实体/路由/页面配置
    ├── WorkItem A：列表列定义
    ├── WorkItem B：新增表单 schema / initial values / submit payload
    └── WorkItem C：编辑表单 schema / data mapping / submit payload
                         │
                         ▼
                 Shared Verification
```

共享定位只执行一次。命中的模型、类型、表单 schema 或页面组件作为三个工作项的共同证据，禁止每个工作项从仓库根重新搜索。

## 8. HypothesisController：结果驱动的有限探索

每个阶段最多维护三个假设，且每个假设只有一个首选探针：

| 阶段 | 首选假设 | 命中 | 未命中 |
| --- | --- | --- | --- |
| Grounding | 当前仓库包含字段符号 | 建立实体锚点 | 检查导航入口 |
| Locating | 三个交付面由共享配置驱动 | 读取共享配置及引用 | 检查独立列表/表单组件 |
| Inspecting | 字段已在类型中但 UI schema 缺失 | 进入最小修改 | 检查 API DTO / 映射层 |
| Change | 当前文件能覆盖该工作项 | 写入并记录 diff | 切换候选目标 |
| Verify | 一次构建/测试覆盖全部变更面 | 标记已验证 | 根据错误映射回具体工作项 |

约束：

- 同一阶段连续两个动作没有产生新目标、排除假设或代码变更，立即停止该路径；
- 一个搜索模式最多一次宽范围调用，后续必须限定到命中目录；
- 失败命令只允许一次修正参数后的重试；
- 禁止通过不同关键词绕过“同一假设已失败”的限制。

## 9. WorkItemScheduler：逐项推进，而非全局阶段

当前实现使用全局 `write_operations`，会在第一个编辑后让整个任务进入 Verify。V3 为每个工作项保存状态：

- A 已 Changed、B/C Pending：调度 B，不进入 Verify；
- A/B/C 全部 Changed：进入共享 Verify；
- B 缺少目标但 A/C 可完成：继续 A/C，B 标记 NeedsUserInput；
- 验证失败：根据错误文件或测试映射回对应工作项，不重置整张计划。

调度优先级为：可直接修改的工作项 → 只差局部读取的工作项 → 需要新假设的工作项。这样模型不会因为某一项暂时困难而重新扫描全部范围。

## 10. EvidenceReducer：用信息增益而非调用成功计进展

成功返回不等于进展。以下才算新证据：

- 新的目标文件或符号；
- 淘汰一个未验证假设；
- 确认一条数据流或引用关系；
- 成功写入与某验收项绑定的 diff；
- 成功验证与验收项绑定的结果。

重复读取、不同关键词命中同一文件、`dir`/`ls` 结果、被门禁拒绝的调用均不增加进展，也不获得预算续期。

## 11. 预算与终止

预算分两层，但不再靠扩大总数字解决问题：

- 阶段预算：每个工作项 Locate 2 次、Inspect 3 次、Change 2 次、Verify 2 次；
- 安全总预算：防止运行时缺陷导致失控，仅作为最后保险。

接近安全上限时，DeliveryJudge 根据证据分流：

- 无目标命中：`NeedsUserInput`，携带已搜索位置和准确问题；
- 已定位但未改：`SystemFailure`，说明哪个状态迁移或工具失败；
- 部分工作项 Changed：`PartialDelivery`，保留 diff、已完成项和剩余项；
- 全部 Changed 但验证失败：`PartialDelivery`，附验证错误；
- 禁止输出“补充范围或约束后继续”这种没有证据的通用信息。

只要工作项持续从 Pending → Located → Changed → Verified 迁移，就允许在安全总额内继续；仅仅新增搜索结果不能续期。

## 12. 澄清协议

澄清问题必须基于证据并且一次最多三个：

```text
当前工作区已确认：React/Vite 项目，存在“多端拼装”路由；
未找到：appCode、subAppCode 或“应用档案”组件引用。
请确认“应用档案”是否位于另一个子项目/分支；若是，请提供目录或切换工作区。
```

用户回复后，运行时把答案写入原 GoalContract，保留已确认的工作区事实和已验证项，不重新从零分类。

## 13. 完成判定

任务只有在以下条件全部满足时为 `Verified`：

1. 每个验收项至少有一条 ChangeEvidence；
2. 每个验收项被验证命令或等价检查覆盖；
3. 验证发生在最后一次相关写入之后；
4. 没有 Pending、NeedsUserInput 或 Failed 工作项；
5. 模型正文、计划状态和普通搜索结果不能充当完成证据。

共享构建可以一次覆盖多个交付面，但必须记录覆盖关系；显式列出的互不相关需求不能自动共享验证。

## 14. 可观测性

UI 不再只显示“思考 N 条、工具 N 次”，而显示：

```text
目标：应用档案字段展示
工作区：已确认
列表：已修改
新增：正在定位表单 schema
编辑：待处理
下一动作：读取已命中的 form schema
```

详细遥测记录 GoalContract、PlanGraph、每次 ActionSpec、假设转移、证据增量、工作项状态和终态原因。它们不进入模型上下文。

## 15. 实施顺序

### 阶段 A：确定性内核

1. 新增 `goal_contract.rs`、`plan_graph.rs`、`work_item.rs` 和 `action_spec.rs`；
2. 将 `ExecutionState` 从全局阶段改为工作项状态集合；
3. 为工具调用增加 ActionSpec 门禁；
4. 增加 WorkspaceGrounder；
5. 保留当前硬熔断作为临时保险，但移除其用户可见的通用 Blocked 文案。

### 阶段 B：AgentLoop 接入

1. 每轮只向模型发送当前工作项、当前假设、已确认事实和允许动作；
2. 工具结果由 EvidenceReducer 处理并触发确定性状态迁移；
3. 模型停止时由 DeliveryJudge 判定下一状态，不能自行宣告完成；
4. 澄清回复写回原计划图。

### 阶段 C：回放和灰度

1. 用真实失败会话生成脱敏回放；
2. 对同一任务比较 V2 与 V3 的完成率、工具数和无信息调用数；
3. 通过 `HARNESS_GOAL_EXECUTOR=v3` 灰度；
4. 达到验收指标后将 V3 设为默认，V2 仅保留短期回退。

## 16. 必须通过的验收回放

对“应用档案列表、新增、编辑缺少 appCode/subAppCode”构建一个最小前端仓库夹具，固定以下断言：

- GoalContract 产生 3 个验收项、3 个工作项和 1 个共享验证节点；
- 首次定位只允许一个高信号搜索；
- 一个工作项 Changed 后不会提前进入 Verify；
- 无命中两次后进入带证据的 NeedsUserInput，不到达通用熔断；
- 正常路径修改三个交付面并验证后为 Verified；
- 完成路径不超过 12 次工具调用；
- 任意路径都不会输出通用 `[blocked] 已达到绝对熔断`；
- 工作区错误时不修改任何文件，并在 4 次调用内提出准确问题。

同时覆盖：单点按钮故障、共享 schema 驱动的多个表单、三个独立需求、验证失败、模型空响应、重复搜索和会话恢复。

## 17. 迁移判断

现有关键词分类、动态预算和全局 ToolPhase 可以作为 V3 上线前的安全护栏，但不应继续承担求解职责。后续修复应优先落在目标图、工作项状态和证据转移上；继续添加业务关键词或扩大工具上限只会让失败更晚发生。
