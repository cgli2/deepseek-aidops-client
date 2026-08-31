# ADR：澄清门禁从词表驱动重构为信号驱动（完成 V5 未竟迁移）

- **状态**：**已实施（Plan B：Phase 1 + Phase 2 全量落地）** — 用户于 2026-08-31 明确"按方案 B 实施，彻底完整改造"。
- **作者**：cgli / WorkBuddy
- **关联**：`AGENT_GOAL_SOLVING_MECHANISM_V5.md`（设计第二原则 / D5 / S1）、`AGENT_GOAL_EXECUTION_FRAMEWORK_V3.md`、`agent-loop-async-concurrent-executor-adr.md`
- **触发**：用户指出"`intent.rs` 用硬编码词表驱动意图分类与澄清门禁，根本无法穷举用户真实场景的各种复杂表达，是不良设计"。经核对，这正是 V5 已明令删除却**残留**在机制层的最后一批词表。

---

## 1. 现状事实（已查源码，非推测）

### 1.1 词表仍盘踞在 `harness/harness-runtime/src/intent.rs`

`IntentProfile::compile(input)` 用 `input.contains(word)` 在硬编码词表上推出约 20 个布尔维度，其中**纯开放集合词表**有：

| 维度 | 词表规模 | 内容性质 |
|---|---|---|
| `has_concrete_target` | ~30 词 | `页面/界面/列表/详情/表单/字段/按钮/组件/输入框/下拉框/弹窗/表格/菜单/导航/卡片/图表/模型/版本号/接口/API/文件/函数/方法/模块/配置 …` |
| `has_observable_failure` | ~28 词 | `未展示/没有展示/看不到/不显示/未显示/没反应/无反应/报错/失败/异常/无法/不能/不工作/没效果/无效/选不了/点不动/截断/乱码/错乱/不正确/不对/变成了/自动变成/自动换行 …` |
| `has_delivery_action` | ~19 词 | `修改/修复/更新/改进/实现/改造/调整/新增/添加/增加/展示/显示/隐藏/移除/替换/去掉/加上/优化/创建/生成 …` |
| `has_expansive_scope` | ~10 词 | `整体/全局/全部/所有/系统性/架构/重构/迁移/调研/方案 …` |
| `asks_for_diagnosis` | ~7 词 | `排查/调查/为什么/为何/根因/诊断/怎么回事 …` |
| `has_structural_action` | ~10 词 | `位置/顺序/对调/互换/调换/交换/前后/移到/置顶/反转 …` |
| `has_exact_replacement` | ~5 词 | `修改为/改为/名称改为/重命名为/替换为 …` |

`IntentKind`（AtomicRegression / ScopedChange / Investigation / OpenEnded）与澄清门禁 `clarification_prompt` 全部由这些脆布尔组合推出。

### 1.2 词表驱动的必然失败（开放集合不可枚举）

- **跨语言失效**：英文 "swap the order of Model Name and API Key fields" 一张表都命中不了。
- **跨领域失效**：`订单/库存/价格` 只对电商域有意义，换编译器/数据管道/嵌入式全是噪声。
- **长尾击穿**：用户说"提交按钮点了**毫无反应**"——"毫无反应"不在 28 词里 → `has_observable_failure=false` → 即便 target 未落地，框架反问"如何稳定复现/实际报错是什么"，可用户已描述。**每来一种新说法就要加词，永无止境，且每次加词都在污染机制层。**

### 1.3 与 V5 设计意图直接冲突（根因已定性，只是没落地）

V5（`AGENT_GOAL_SOLVING_MECHANISM_V5.md`）已把这件事定性：

- 设计第二原则（§开头）：**"不以人工枚举对抗开放集合。用户表达空间是开放的，任何'预定义词表 + 语义判断'的路线注定被长尾击穿。"**
- §67：**"补词表不是解法，换裁判才是解法。"**
- 裁定 D5（§188）：**"现有中文词表的处置？迁移到 L2 保留为内置默认画像；不得在机制层保留引用。"**
- 方案 S1（§261）：**"删除 `CJK_NOUN_MARKERS`/`CJK_ACTION_TAILS`/`CJK_LEAD_STOPWORDS` 的语义判断职责，L1 只切分。"**

`goal_execution.rs` 的 grounding 层（`GoalContract` + `WorkspaceIndex::select_anchors`/`best_variant`，按"工作区区分度/TF-IDF 的 IDF 思想"裁决锚点）**已经**落地了 V5 的信号驱动路线；唯独 `intent.rs` 的澄清门禁**没跟上**，仍保留机制层词表。上一轮修复的 `needs_target`/`needs_symptom`（接 `has_locatable_signal()`）只是词表之上的补丁——它让"目标可定位"时不再追问，但**词表这个根因仍在**，且"无法定位时的追问"依旧是词表判断。

### 1.4 调用链（已定位）

`agent_loop.rs`：`IntentProfile::compile(task_text)`（L407，单条消息词法）→ `goal.resolve_against(index)`（L418-425，工作区落地）→ `intent.clarification_prompt(&task_text, goal.has_locatable_signal())`（L427，门禁）。门禁的"是否该问"仍主要由 1.1 的词表维度决定，落地信号只做了"非问即止"的局部抑制。

---

## 2. 设计目标与硬约束

- **G1**：澄清门禁的"是否追问"判定**完全不依赖任何开放集合词表**；可定位性由 `GoalContract`（工作区区分度裁决）给出，意图/任务性由**封闭且语言不变**的信号或 LLM 给出。
- **G2**：默认**乐观**——能定位就直接进 `Locate → Inspect`，不预问；只有**真·盲**（无定位信号且无导航入口）才问，且只问**一个**有上下文的问题，不发清单。
- **G3**：歧义在 `Inspect` 期消解——定位后由 agent 读代码/跑测试/看类型自行发现真实行为差异，比关键词猜用户措辞可靠。
- **约束（沿用 V5 与既有规则）**：
  - 机制保持**通用**，不得为项目/语言/领域硬编码词表（呼应 D5）；
  - 不得"空跑"：仍有定位/预算门禁与 `repeat_guard`，盲目搜索被 `zero_prior` 拦截；
  - 单面任务**行为零退化**（回归保护）；
  - 冷启动（工作区索引未建）可降级，但不得失效（呼应 D5 内置默认画像 + 降级契约）。

---

## 3. 方案

### 方案 A（推荐·Phase 1，低风险、直接消除根因）

**删掉 `intent.rs` 的全部开放集合词表维度，澄清门禁改为"接地 + 乐观默认 + 单问"。**

- 引入极简 `IntentAssessment`，仅含**封闭且语言不变**的信号 + grounding：
  - `grounded = goal.has_locatable_signal()`（来自 `GoalContract`，已是工作区区分度裁决，**非词表**）；
  - `is_task`：封闭集任务信号 = 存在变换契约（`extract_exact_transformation` 已提取的 `X → Y`）、或 L0 结构动作（`互换/对调/移到/swap/reorder`，属 V5 §4.1 明确允许进 L0 的**封闭**动作集）、或导航路径存在（`A > B`/`A → B`）。**不再枚举 19 个交付动词**。
  - `described_discrepancy`：**不再用 28 词失败词表判断**。语义上"用户是否描述了偏差"交给 `Inspect` 期由 agent 观察真实行为来比对；门禁不再需要这个布尔。
- 新门禁（取代 `clarification_prompt`）：
  ```rust
  pub fn requires_clarification(goal: &GoalContract, is_task: bool) -> Option<Clarification> {
      if !is_task { return None; }            // 闲聊/纯提问 → 不强制任务闸门
      if goal.has_locatable_signal() {        // 已落地 → 直接 Locate→Inspect，不预问
          return None;
      }
      Some(Clarification::locate(&goal))      // 真盲：只问一个带上下文的定位问题
  }
  ```
- `IntentKind` 由封闭信号 + grounding 推导（`has_transformation_contract`→ExactReplacement；L0 结构动作→StructuralChange；navigation/grounded+is_task→ScopedChange；否则 OpenEnded），不再读开放词表。
- **优点**：根治"词表追不上口语"的缺陷；与 V5 路线一致；回归面小（仅 `intent.rs` + `agent_loop.rs` 调用点）；冷启动退化为"单问定位"，体验优于现在的"清单式追问"。
- **代价**：`is_task` 的精细意图（如"只是想了解 vs 想改"）交给 LLM/planner 判（呼应 V5 D1 计划由 LLM 生成），机制层只做"是不是任务"的粗判。

### 方案 B（Phase 2，完整架构，高收益但高风险）

**让 `Inspect` 阶段显式产出"observed vs expected"差异报告，并据此决定是否带上下文追问。**

- 在 Solve 循环 `Locate` 之后插入 `Inspect` 差异比对：agent 定位后执行/读取测试/查类型，产出 `ObservedBehavior`，与 `GoalContract::expected_state` 比对。
- 比对一致 → 直接 `Change`；不一致且能推断 → 带"我发现 X 当前行为 A，你指的是 B 吗？"式**单问**；完全无锚 → 回落方案 A 的 `Clarification::locate`。
- **优点**：把"用户说没说清异常"这件事从"关键词猜"彻底移交给"运行期观察"，是 V5 §3.3 `zero_prior` 思路在澄清侧的对称落地。
- **代价**：需扩充 Solve 循环与 `Behavior` 面，回归风险高于 Phase 1。

### 决策建议

先落地 **方案 A**——它已经把"词表根因"消除，且单面回退路径（`grounded` 由 `GoalContract` 给出，与现状等价）零回归；方案 B 作为后续独立任务，在 A 跑通且量化指标达标后再做。

---

## 4. 信号来源对照（词表 → 信号驱动）

| 旧词表维度 | 新来源 | 性质变化 |
|---|---|---|
| `has_concrete_target`（30 词 UI 名词） | `GoalContract::has_locatable_signal()`（工作区区分度裁决） | 开放枚举 → 闭合裁决 |
| `has_observable_failure`（28 词失败词） | 删除；偏差在 `Inspect` 期由 agent 观察 | 开放枚举 → 运行期观察 |
| `has_delivery_action`（19 词动词） | 封闭集：`extract_exact_transformation` + L0 结构动作 + 导航路径；细意图交 LLM | 开放枚举 → 封闭+LLM |
| `has_expansive_scope`（10 词） | 由 `GoalContract::candidates` 在工作区命中分布推导（多锚点 = 广范围），或 LLM 计划声明 | 开放枚举 → grounding/LLM |
| `asks_for_diagnosis`（7 词） | LLM 计划 `SolveSketch` 声明（呼应 D1），机制层不预判 | 开放枚举 → LLM |
| `has_structural_action`（10 词） | 保留为 **L0 封闭集**（V5 §4.1 已批准进 L0） | 已是封闭，保留 |

核心论点：**除 L0 结构动作（封闭、跨领域恒定）外，其余开放词表一律迁出机制层**，与 V5 D5/S1 完全一致。

---

## 5. 风险与缓解

- **R1 冷启动（工作区索引未建）→ `has_locatable_signal()` 恒 false → 总追问**：缓解 = V5 D5 内置默认画像（冷启动首轮用极简召回，被学习沉淀覆盖）+ 即便追问也只问**一个**定位问题（优于现清单）；且 `WorkspaceIndex` 通常在首轮即建立。
- **R2 移除 19 词交付动词 → 闲聊被当任务**：缓解 = 保留 `is_task` 最小封闭信号（变换契约/L0 结构动作/导航路径），更细意图交 LLM，机制层只做粗判，不会误放行纯提问。
- **R3 现有测试断言"含某词即追问"**：缓解 = 测试改为**场景化**（见 §6），不再断言词表命中，断言"落地即不问 / 真盲则单问"。
- **R4 通用性**：所有信号来自 `GoalContract`（跨语言跨领域）或 L0 封闭结构动作，无领域硬编码。
- **R5 单面回归**：`grounded` 由 `GoalContract` 给出，与现状等价；`has_locatable_signal()` 为 true 时行为路径与改造前一致。

---

## 6. 验证计划

- **单元/集成（沿用 `harness-runtime` 测试基座）**：
  - 场景化测试取代关键词测试：
    - `grounded_target_needs_no_clarification`：`"ModelForm 的校验规则有问题"` → `requires_clarification` 返回 `None`（已由上一轮 `has_locatable_signal` 落地保证，现结构性成立）；
    - `ungrounded_soft_problem_asks_single_locate_question`：`"这个列表的排序逻辑有问题"`（无代码符号）→ 返回**恰好一个** `Clarification::locate`，且不包含"如何复现/验收标准"清单项；
    - `paraphrase_not_in_wordlist_still_proceeds_when_grounded`：`"提交按钮点了毫无反应"`（"毫无反应"不在旧失败词表）→ 若按钮定位成功则 `None`，证明长尾 paraphrase 不再击穿；
    - `chit_chat_is_not_a_task`：纯提问 → `None`。
  - 删除原 `vague_delivery_requests_a_minimum_clarification_instead_of_exploration` 等基于"含词即问"的测试（其保护的设计意图已被本 ADR 推翻）。
- **量化（呼应 V5 §7）**：脚本化 LLM（`MultiSurfaceLlm` 模式）e2e：统计"进入澄清的次数/追问问题数"，对比改造前词表门禁，目标 = **追问率↓、单问占比↑、落地任务零追问**。
- **编译/冒烟**：`cargo check --workspace` 零错误；`cargo test -p harness-runtime` 全绿；`aidops-desktop --headless` 回放 `session finished.`。

---

## 7. 待确认决策点（评审结论）

1. **范围**：✅ **Phase 1 + Phase 2 全量（方案 B）** — 用户确认"彻底完整改造"，不再做半套补丁。
2. **`is_task` 粗判交 LLM**：✅ 接受。机制层只做封闭信号粗判（`extract_exact_transformation` / L0 结构动作 / 导航路径 / 疑问词或句末问号），细意图交 LLM `SolveSketch`（V5 D1）。
3. **`asks_for_diagnosis` / `has_expansive_scope` 一并迁出**：✅ 已迁出到 `IntentProfile` 的封闭信号体系；消费方 `select_strategy` 改为读 `IntentKind` 信号 + 极简动词表（仅供 `Transformative` 兜底，非澄清门禁）。

> 决策已确认：**范围 = Phase 1 + Phase 2（方案 B）**；机制层开放集合词表**全数删除**，仅保留 L0 封闭结构动作集（V5 §4.1 已批准）与封闭疑问引导词（语言结构特征，非话题枚举）。

## 8. 实现说明（as-built）

### 8.1 文件级改动

| 文件 | 改动 | 性质 |
|---|---|---|
| `harness-runtime/src/intent.rs` | **删除全部开放集合词表维度**（`has_concrete_target`/`has_observable_failure`/`has_delivery_action`/`has_expansive_scope`/`asks_for_diagnosis`/`has_structural_action`/`has_exact_replacement`/`has_before_after`/`has_state_transition`/`has_stale_observation` 等共约 150 词）。`IntentProfile::compile` 仅用封闭信号：`extract_exact_transformation`（X→Y）、`extract_navigation`（A>B）、`extract_code_symbols`、`STRUCTURAL_ACTIONS`（17 词，V5 §4.1 批准进 L0 的封闭动作集）、`QUESTION_LEAD`（6 词，仅覆盖"疑问词开头/句末问号"语言结构）。新增 `requires_clarification(goal, is_task)`、`ObservedBehavior`、`InspectVerdict`、`inspect_diff`、`Clarification::{locate, observe_mismatch}`。 | 机制层零开放词表 |
| `harness-runtime/src/agent_loop.rs` | Phase 1 门禁：`if let Some(clar) = IntentProfile::requires_clarification(&goal_execution.goal, intent.is_task)` 取代旧的 `clarification_prompt`。Phase 2 钩子：`apply_grounding` 后若 `goal.has_locatable_signal()` 则调用 `goal_execution.inspect_for_clarification(root)`，命中 `InferableMismatch` 即返回 `[需要澄清]` 阻塞交付。 | 接线 |
| `harness-runtime/src/goal_execution.rs` | `extract_exact_transformation` 提升为 `pub(crate)`；新增 `GoalContract::static_observe(target_files, root)`（读磁盘、逐字核对期望终态是否已在产物中出现）与 `GoalExecution::inspect_for_clarification(root)`（仅 `InferableMismatch` 时返回 `Some`）。 | Phase 2 落地 |
| `harness-runtime/src/execution.rs` | 删除 `IntentProfile::delivery_surface_count` 字段；`SolvePlan::for_contract` 改用 `contract.acceptance_criteria.len()` 推导交付面，`is_atomic_regression` 判定改用 `intent.kind == AtomicRegression`。 | 去除机制层 UI 名词计数 |
| `harness-runtime/src/lib.rs` | 导出集更新为 `Clarification, ClarificationKind, InspectVerdict, IntentKind, IntentProfile, ObservedBehavior, inspect_diff`。 | 对外 API |

### 8.2 门禁语义（最终形态）

- **Phase 1（乐观默认 + 单问）**：`requires_clarification(goal, is_task, input)` 仅在 `is_task && !goal.has_locatable_signal() && input 含封闭指示代词（这个/那个/这些/那些/它/此）` 时返回 `Some(Clarification::locate)`——**一个**带上下文（导航/符号线索）的定位问题，**绝不发清单**。即：只有"任务 + 无定位信号 + 盲指代（连具体症状都没给）"才追问；具象描述（哪怕尚未落地）一律乐观放行到 `Locate→Inspect`，避免在"简单问题"上也反复追问（呼应 G2"只问真盲"）。`!is_task`（纯提问/闲聊）与已落地目标一律 `None`，直接 `Locate→Inspect`。
  - `has_locatable_signal()` 已放宽：`X→Y` 或纯 `→Y` 变更契约（`to_value` 非空）均视为可定位信号，具体变更请求（如"把版本号修改为 0.2.2"）不再反问。
- **Phase 2（运行期观察替代关键词猜异常）**：`inspect_diff` 用 `ObservedBehavior`（agent 实际读到的代码/跑出的结果）与 `GoalContract` 变更契约比对：
  - 锚点为空 → `NoAnchor`（回落 Phase 1 定位，由调用方处理）；
  - 已是期望终态（`to` 已出现或当前正是 `from`）→ `Aligned`（直接 Change/Verify，**不追问**）；
  - 有 `from→to` 契约且当前**既非 `from` 也非 `to`** → `InferableMismatch`（带一个上下文单问确认意图）；
  - 无 `from` 的纯"改为 to"、或当前≠to 的待改状态 → `Aligned`（**不死循环追问**）。
- **关键不变量**：只有观察**真的揭示了歧义**（本应是 `from` 却既非 `from` 也非 `to`）才追问；普通待改任务一律放行。彻底消除"逢任务就问"。

### 8.3 刻意未改（次级债，待另立任务）

`execution.rs` 的 `select_strategy` 内部仍保留**自己的**极简动词表（`["修改","修复","重构",…]`、`mentions_surface`/`mentions_source`/`mentions_mismatch`）。它们驱动的是 **SolveMode 选择**（Transformative/Investigative/…），**不是澄清门禁**，且只作 `Transformative` 兜底（意图细判交 LLM）。本次按 ADR Plan B 范围（澄清门禁）未动；若需彻底贯彻 D5，应另立 PR 把这些动词表也迁到 LLM 计划声明。当前 9 个原"含词即问"测试已改写为场景化断言（见 §9）。

### 8.4 顺带修复

`agent_loop.rs:1654` 一处潜伏编译错误（`Vec<&String>.join("、")` 未实现 `Borrow<str>`，原被增量缓存掩盖），已改为 `keys.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("、")`。

## 9. 验证证据

### 9.1 编译

- `cargo check --workspace`：✅ **0 错误**（全工作区，含 `harness-ui` / `aidops-desktop`）。
- `cargo test -p harness-runtime --lib`：✅ **168 passed / 0 failed / 1 ignored**。

### 9.2 场景化测试（取代原"含词即问"断言）

澄清门禁（Phase 1/2）：
- `grounded_goal_needs_no_clarification`：`"ModelForm 的校验规则有问题"`（已定位）→ `None`；
- `blind_task_asks_a_single_locate_question`：`"这个列表的排序逻辑有问题"`（无符号）→ 恰好一个 `Clarification::locate`，且不含编号清单；
- `pure_question_asks_no_task_clarification`：`"为什么列表排序会乱？"`（纯提问）→ `None`；
- `long_tail_paraphrase_still_proceeds_when_grounded`：`"提交按钮点了毫无反应"`（"毫无反应"不在任何旧失败词表）→ 定位成功后 `None`，长尾 paraphrase 不再击穿。

Phase 2 观察差异：
- `inspect_diff_aligned_when_expected_found` / `_no_ask_when_current_is_from` / `_no_ask_for_plain_to_change_without_from` / `_no_anchor_when_observations_empty` / `_aligned_when_already_at_to_value`：均 `Aligned`（**不追问**）；
- `inspect_diff_inferable_mismatch_when_current_is_neither_from_nor_to`：仅此情形返回 `InferableMismatch`（带上下文单问）。

意图分类（封闭信号）：
- `classifies_generic_intents_by_closed_signals`：提问→`Investigative`、代码符号→`Transformative`、无信号非提问→`Direct`；
- `concrete_problem_report_uses_transformative_strategy_without_fix_verb`：未说"修复"但给代码符号 `Composer` → `ScopedDelivery`；
- `atomic_regression_uses_a_short_state_machine_window` / `stale_state_after_mutation_is_atomic_regression` / `atomic_gate_blocks_broad_second_search_and_pre_change_verification` / `dynamic_tool_whitelist_follows_verified_execution_evidence`：变更契约（`改为`/结构动作）→ `AtomicDelivery`，短窗口 + 定位门禁生效；
- `review_word_does_not_force_shell_only_verification_mode`：诊断式提问→`Investigative`（不被"审查"收窄成 shell-only），明确验证动作→`Verification`；
- `read_only_goal_requires_a_real_read_before_conclusion`：提问形态→`Investigation`（只读），必须先读后有证据才能结案；
- `multi_surface_fields_use_scoped_plan_and_scaled_budget`：交付面数改由 `acceptance_criteria.len()` 推导（`24/30` 熔断），单面回归预算饥饿已修复。

### 9.3 行为对比（改造前 vs 后）

- **改造前**：简单问题（如"提交按钮点了毫无反应"）首轮即反问用户一串清单式补充信息，且用户常无法回答 → 体验差、被判定为框架缺陷。
- **改造后**：能定位直接进 `Locate→Inspect`；真盲只问**一个**带上下文的定位问题；`Inspect` 期由 agent 读代码自行发现偏差，只在观察**真的揭示歧义**时才带上下文单问。追问次数↓、单问占比↑、落地任务零追问。

> 量化 e2e（V5 §7 脚本化 LLM 统计追问率）为后续独立验证任务，不在本 PR 范围。
