# Agent 治理层彻底重设计：闭环控制器 + Case File

- 日期：2026-08-31
- 状态：设计已获批；步骤①（回放套件）、②（case file 投影 + 真实日志保真对拍）、③（工具层契约）、④（新闭环控制器接管终止权与澄清门禁，A/B 默认 Legacy）已实施，四条红线门禁已解除 `#[ignore]` 并在控制器模式下全绿（A/B 默认仍走旧路径）；实机三场景对照已通过红线级验收（2026-09-01，`governance_ab_run.py --acp` 自动跑，见 `2026-09-01-governance-phase2-onsite-ab-checklist.md` 判读，A2 辅助度量误报待阶段 3 细化）；待步骤⑤（旧计数器退位删除）
- 范围：harness-runtime 治理层 + 工具层契约；SessionEvent schema、LLM provider 接口、harness-ui 均不动
- 证据来源：`.harness/sessions/7ba3370f-fcbe-4993-b50a-f89f750ba929.jsonl`（22 轮、3.14M prompt tokens）、`harness/dist/harness_gui_trace.log` 时间戳交叉验证

## 1. 背景与问题陈述

会话 7ba3370f 的复盘表明，一个简单任务（消除 git 子进程黑框闪烁 + 非仓库工作区报错降级）跑了 22 轮、触发 7 次 Interrupted、4 次 SystemFailure、9 次 NeedsUserInput，仅 2 次 Verified 且均无代码落地。根因不是单个守卫参数失调，而是治理体系的形状问题：

1. **fail-stop 断路开关网络**：澄清门禁、target-anchor、no_info×4、硬预算、重复调用守卫、length 恢复各自独立终止回合，出口统一指向"停下交还用户"；自动接续只救有写入产出的回合，locate/诊断类任务定义上不产生写入，必然硬停。
2. **轮级无状态重编译**：`resume` 只恢复目标文本与已验证验收项（agent_loop.rs:361-399），搜索尝试/排除结论不跨轮携带；`ToolRepeatGuard` 每轮新建（agent_loop.rs:739）。"继续"≡ 同契约重编译 ≡ 同轨迹重放 ≡ 撞同一熔断。
3. **预算与难度反相关**：预算由意图语法信号供给；纯症状任务拿最小预算且无工作区索引（索引仅在 goal 带落地信号时构建，agent_loop.rs:449-452）。
4. **重复澄清熔断缺口**：`is_clarification_reply` 显式排除 continuation 请求（agent_loop.rs:356-357），而熔断条件要求其为真（agent_loop.rs:469-470），用户对澄清回"继续"时同一问题无限重复。
5. **模型层失败无吸收**：edit 凭记忆重构 old_text 致 `matched 0`；4096 输出上限 length 截断仅 1 次重试即硬停（agent_loop.rs:1453-1457）。

docs/ 下 V3–V5、交付改革、中断修复、澄清 ADR 五代文档是"一事故一机制"补丁史的证据；本设计的目标是换形状，而非再叠一层。

## 2. 目标与非目标

**目标**
- G1：决策权单点化——只有控制器能终止回合，守卫降级为传感器。
- G2：会话级状态持久化——turn 从 case file 出发，消灭无状态重放。
- G3：失败吸收下沉——工具层吸收 edit/search/command 的常见失败模式。
- G4：grounding 前馈化——执行前用测量（索引命中）选模式，而非语法信号猜预算。
- G5：制度性防退化——回放回归套件成为一切治理改动的前置门禁。

**非目标**
- 不改 SessionEvent schema 与 harness-ui；不改 LLM provider 接口；不重写 intent.rs（降级为先验提示，保留文件）；不动 expert-council 等多 agent 机制。

## 3. 验收红线（回放断言的权威来源）

- R1 用户不说"继续"：续跑式回复（继续/接着/续跑/恢复/continue/resume 前缀）不得以 ask_user/NeedsUserInput 结束，必须以行动或部分交付结束。
- R2 问题不重复：同一澄清问题会话内不得出现第二次；任何 ask_user 必须带工作区派生的候选列表，禁开放模板。
- R3 成本封顶：单会话 prompt tokens 硬顶 300,000；超顶后控制器只允许进入 partial_deliver，不得继续探索。
- R4 失败也留资产：任何回合结束（含失败）必带结构化 artifact：精确锚点 + 根因假设 + 建议补丁 + 至多一个候选问项。Delivered 回合的 artifact 即交付报告本身（现有 Delivery 事件），不额外构造。
- 辅助断言：A1 单会话守卫/传感器触发次数 ≤ 12；A2 同工具签名跨轮重复 ≤ 2。

## 4. 架构

### 4.1 单一闭环控制器

控制器循环：observe → measure → decide。decide 仅四分支：continue / switch_strategy / degrade / terminate。现有守卫改写为传感器，只产信号不终止回合：

| 旧机制 | 新角色 |
|---|---|
| no_information_count | gain 传感器（窗口内零增益信号） |
| target-anchor 门禁 | 策略窗口预算传感器 |
| ToolRepeatGuard | tried 去重传感器（签名已存在 → 换策略信号） |
| 硬预算 / stagnant_windows | 成本传感器（会话累计、栈深度） |
| 澄清门禁 / requires_clarification | ask_user 候选生成器（受 4.2 约束） |

### 4.2 策略栈与出口

策略 = (工具集, 搜索范围, 预算窗口, 退出条件)。默认栈（scoped 任务）自顶向下：

1. grounded_change：带 grounding 锚点直接 change + verify
2. broad_locate：全工作区搜索 / 字符串索引一跳
3. runtime_observe：诊断模式（cargo check 等运行时观察）
4. compact_reroute：紧凑检查点换路（清历史、最小快照）
5. degrade_goal：交付可验证子目标
6. partial_deliver：栈底常驻，ExhaustedWithArtifact（R4 的 artifact 构造点）

规则：
- 窗口内 gain=0 → pop 栈（switch_strategy）；栈空 → terminate 于 partial_deliver。
- 出口仅两个：`Delivered`、`ExhaustedWithArtifact`。
- ask_user 是工具调用而非出口；前置条件三者全满足：栈深度 ≤ 2（即剩余策略 ⊆ {degrade_goal, partial_deliver}）、非续跑式回复（R1）、问题不在 asked 集且带候选列表（R2）。
- Investigation 意图（纯提问）用只读栈变体：broad_locate → runtime_observe → partial_deliver，不含写入型策略；其 Delivered = 带证据锚点的回答，artifact 即回答本身。

### 4.3 case file（会话级世界模型）

字段：`tried[(tool, 归一化签名, 结果摘要)]`、`eliminated`、`anchors`、`user_signals`、`asked`、`budget_spent(prompt/completion)`、`stack_pos`。

持久化：单一事实源仍是 SessionLog（jsonl 追加式）。`CaseFile::from_replay(&[SessionEvent])` 确定性重建；轮内增量维护。不引入第二份持久化文件，UI 事件 schema 不变。

效果：任何搜索/编辑发出前先查 `tried`，签名已存在直接换策略——跨轮重放在构造上消失；"继续" = 继续该 case。

### 4.4 information_gain 与阈值

gain = 新锚点数 + 新排除假设数 + 写入增量 + 新用户信号，按策略窗口计量（窗口 = 该策略预算切片，默认 4 步）。

阈值空间收敛为 3 个参数：窗口大小 W（默认 4）、会话 token 顶（300k，R3）、ask_user 栈深度门槛（2）。现有 6 个计数器（no_information_count、correction_count、stagnant_windows、blocked_count 等）全部退位删除。

### 4.5 前馈 grounding 阶段

执行前 orientation 阶段，独立预算（≤3 次工具调用、≤20k tokens，不占策略窗口）：

- 三层索引，增量构建、落盘 `.harness/index.json`（沿用 workspace_index/learned.json 机制）。index.json 是工作区侧索引缓存；会话侧 case file 仍派生自 SessionLog，二者不构成双事实源。
  1. 报错文案/UI 串索引（harness-ui 中文字面量 + 错误模板；用户消息与源码串的一跳映射）；
  2. 模块/crate 地图（Cargo workspace 元数据）；
  3. 符号索引（pub fn/struct/impl/trait 轻量扫描）。
- 命中 → grounded_change 快路径；未命中 → 诊断模式（栈从 broad_locate 起）。intent 分类仅作先验提示，不再供给预算。

### 4.6 工具层契约

- edit：read-modify-write 原子化。old_text 失配时工具自读磁盘目标区间，返回精确原文 + 行号供模型重发；禁止模型凭记忆重构（turn 19 失败类的构造性消除）。
- search：作用域自动升级 dir → crate → workspace，在工具内部完成；空结果返回"已试 scope 列表 + 建议下一 scope"（接 case.tried），模型不再自行猜 scope。
- command：平台卫生内置（Windows CREATE_NO_WINDOW 等），不依赖模型记忆。

### 4.7 模型协议

- finish_reason=length 为正常事件：紧凑检查点 + act-or-conclude 续请，重试上限 2（现 1）；仍超 → switch_strategy（默认压缩上下文；仅当配置了多模型时可选换轻量模型），不再 Interrupted。
- thinking 占比 > 60% 输出时，恢复 prompt 注入"直接输出结论或工具调用，禁止只输出思考"。

### 4.8 可观测性

会话级聚合落 trace：outcome 分布、策略切换次数、ask_user 次数、token 成本、传感器触发计数。告警阈值：单会话 ask_user ≥ 2 或 Interrupted ≥ 1 → trace 标记 REGRESSION_SUSPECT，供回放套件收录。

## 5. 迁移计划（绞杀者，五步，每步可验证可回滚）

1. 回归套件先行：`harness-runtime/tests/session_replay.rs`；fixture = 7ba3370f 全量 + 分段（15–18 澄清循环、3–14 症状任务、19–22 git 修复）+ 一个成功会话；LLM 用日志 Assistant/tool_calls mock，工具用 ToolResult 重放。红线断言在旧代码上必须跑红（证明断言有效）。
2. case file write-only 并联：只记录不决策；与 7ba3370f 的离线分析对拍 tried/anchors 保真度。
3. 工具层契约独立上线（edit 原子化、search 升级、command 卫生）——不依赖控制器，立即消 turn 19 两类失败。
4. 新控制器接管决策：env/配置开关 A/B；回放套件全绿 + 实机同 fixture 对照。
5. 旧计数器/守卫逐个退位删除；goal_execution.rs/execution.rs 冗余收缩；docs V3–V5 等标记 deprecated。

## 6. 测试计划

- 单元：CaseFile::from_replay 确定性、tried 归一化、gain 计算、策略栈 pop 顺序、ask_user 前置条件。
- 回放：R1–R4 + A1/A2 断言；任何治理参数改动 PR 必过。
- 实机：重跑 7ba3370f 三场景（症状任务/续跑澄清/git 报错修复），全程无用户"继续"。

## 7. Done 的定义

回放套件四红线全绿；实机三场景自主收敛；6 个旧计数器代码删除；单会话 token 成本 ≤ 300k；失败回合 100% 带 artifact。

## 8. 风险与取舍

- 回放 mock 保真 ≠ 实机行为：步骤 4 以实机 A/B 兜底。
- 单一 gain 度量可能误判"慢热"探索：窗口 W 可调，且 runtime_observe 策略的运行时观察结果计入 gain。
- 控制器单点 = 单点故障：控制器自身 panic 必须回落 partial_deliver（R4），不得裸抛给用户。

## 附录：证据索引

- 无状态重编译：harness-runtime/src/agent_loop.rs:356-399
- 重复澄清熔断缺口：agent_loop.rs:469-470
- 索引条件构建：agent_loop.rs:449-452
- length 重试 1 次：agent_loop.rs:1453-1457
- Interrupted 出口：agent_loop.rs:1678；SystemFailure 出口：agent_loop.rs:1667
- 会话日志：.harness/sessions/7ba3370f-*.jsonl；turn 15–18 澄清循环、turn 19 edit matched 0 ×3、turn 22 length 截断
