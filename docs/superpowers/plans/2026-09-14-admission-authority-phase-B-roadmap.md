# 准入权唯一化·阶段 B 路线图（工作流拆分与验收判据）

> **For agentic workers:** 本文件是**路线图，不是可执行计划**。它只做三件事：把工作拆分、锁定顺序与依赖、给每条工作流写出锚点与验收判据。
> 每条工作流在被领取时，必须先按 superpowers:writing-plans 写出它自己的可执行计划（格式参照 `2026-09-13-admission-authority-phase-a-zero-denials.md`：已核实代码事实 → 计划修正 → 决策边界 → 文件结构 → bite-sized Task），再按 superpowers:executing-plans 执行。
> 直接把本文件当计划执行会踩坑：B3/B5 含未裁定项（见文末「待你裁定」），B7 含外部成本授权。

**Goal:** 把 spec `2026-09-13-agent-admission-authority-consolidation-design.md` 中阶段 A（S1+S2 拦停归零）未覆盖的部分做完——度量从「动作计数」换成「信息增益」、已确认事实不被压缩掉、完成判定按验收项↔证据映射、历史层收口、旧守卫与 Legacy 分支删除，并以 10 fixture 的端到端交付率作为硬验收。

**Architecture:** 不新建子系统。增益事实进既有的 `CaseFile`（阶段 2 已落地且与真实日志对拍过 6 项）；压缩豁免与重读复用改 `agent_loop.rs` 的两个既有收口函数；完成判定改 `ExecutionState::can_complete` 与 `DeliveryOutcome` 的映射依据；治理层剩余删除动作（5d/T6/T7）沿阶段 A 的「标记分类法 + 唯一消费点」收口。终止权仍唯一归 `TurnGovernor`。

**Tech Stack:** Rust（`harness-runtime` 为主、`harness-session` 涉及 `DeliveryOutcome`）、cargo（**必须在 `harness/` 下跑**，MSVC 工具链）、Python 编排脚本 `harness/scripts/governance_ab_run.py` 与 `governance_redline_check.py`（`python -X utf8`）。

**Spec:** `docs/superpowers/specs/2026-09-13-agent-admission-authority-consolidation-design.md`（§4.2/§4.3/§4.4/§4.5/§4.6/§5 S3-S5/§6；阶段 A 落地结果见其 §4.1.1）

## Global Constraints（每条工作流都隐含包含）

- 只有真实外部约束保留拒绝能力：访问策略、钩子阻断、沙箱/IO 错误、工作区外路径、`read_only` 任务的写入、shell 外部副作用审批、同回复完全相同调用的去重。
- **标记分类法（阶段 A 确立，不得回退）**：`gate]` / `guard]` 永久专指「门禁吞掉动作」，全量红线要求它零出现；真实外部约束用 `[constraint denied]`，写入未生效观察用 `[write-not-counted]`，同回复重复调用用 `[duplicate-call]`。
- `Advise` 的原因文本**永不**写入 `ToolResult`，只进 `Message::user` 提示与 `Thinking` 遥测；提示唯一出口是 `delivery_workflow::{note_advice, render_advisories}`（同类去重、每步至多一条）。
- 计数字段保留为遥测（spec D2），不删结构；终止权唯一归 `TurnGovernor`。
- `SessionEvent` schema、LLM provider 接口、`harness-ui` 不动（spec §8「不引入新的事件/状态 schema」）。
- `[需要澄清]` 门禁合成文本与 `ReplayLlm` 耗尽回退 chunk 文本不得改动（回放收敛依赖）。
- 指标口径必须**同时**写进 `harness-runtime/tests/session_replay.rs` 与 `harness/scripts/governance_redline_check.py`（同源双语义，改一处必改另一处——阶段 3 踩过这个坑）。
- 禁止 `cargo fmt`：仓库基线非 rustfmt-clean，跑一次会把无关代码卷进 diff。
- clippy 基线：仓库存量 108 条警告与本轮无关，验收标准是**改动涉及文件零新警告**，不是零警告。

---

## 开工顺序与依赖

| # | 工作流 | 需模型端点 | 需先裁定 | 建议子计划文件名 |
|---|---|---|---|---|
| B0 | 阶段 A 实机三场景复跑（补跑） | 是 | 否（等端点） | 不立计划，跑完记回阶段 A 计划 Step 5 |
| B1 | `SEARCH_MEMO` 会话作用域修复 | 否 | 否 | `2026-09-14-phase-b1-search-memo-session-scope.md` |
| B2 | §4.5 `[error]` 文本不再回喂 | 否 | 否 | `2026-09-14-phase-b2-error-text-out-of-context.md` |
| B3 | §4.2 度量换成信息增益 | 否 | 是（Q1 无关，但需定 streak 归属） | `2026-09-14-phase-b3-gain-based-measurement.md` |
| B4 | §4.3 已确认事实不被压缩 + 重读即复用 | 否 | 否 | `2026-09-14-phase-b4-fact-level-memory.md` |
| B5 | §4.4 完成判定按验收项↔证据映射 | 否 | **是（Q1）** | `2026-09-14-phase-b5-completion-by-evidence.md` |
| B6 | §4.6 5c(已完成)/5d/T6/T7 收尾删除 | 否 | 否 | `2026-09-14-phase-b6-counter-retirement.md` |
| B7 | §6 10 fixture 集与交付率红线 | **是** | **是（Q2/Q3）** | `2026-09-14-phase-b7-delivery-rate-harness.md` |
| B8 | 流程债：阶段 2/3 计划补状态行 | 否 | 否 | 合进 B6 的文档提交即可 |

依赖关系：B1 → B4（重读复用要建在按会话隔离的缓存上，否则跨会话污染会伪装成「复用成功」）；B3 → B4（增益判定与豁免同读 `CaseFile`）；B4 + B5 → B7（交付率红线的「零重读」与「Delivered 且结论正确」要有行为才有意义）；B0 与 B7 都卡在同一个前置——可用端点。

建议顺序：**B1 → B2 → B8 → B6 → B3 → B4 → B5 → B7 → B0**。理由：先做完不需要端点、不需要裁定的确定性删除与缺陷修复，把有设计风险的（B3/B5）放在中间，最后一次性把需要端点的采集（B7/B0）合并跑，省一轮真实 API 开销。

---

## B1 `SEARCH_MEMO` 会话作用域修复

**锚点（实测）**
- 定义：`harness-runtime/src/agent_loop.rs:150-157` —— `static SEARCH_MEMO: OnceLock<Mutex<HashMap<String, CachedSearchOutput>>>`，键 `search_cache_key(name, args) = format!("{name}::{args:?}")`（`:183-185`），**不含会话与工作区**。
- 注释与实现不符：`:1636` 处写「搜索类调用先查**会话级**记忆化缓存」，实际是进程级。
- 读点：`:1637-1662`（命中即复用，不进工具层，并 `note_advice`）；写点：`:1770-1780`。
- 受影响测试：`tests/agent_tool_loop.rs` 的 `repeated_search_replays_from_memo_without_denial`（已用独有 pattern `"memo_contract_unique"` 隔离）、`search_memo_hit_keeps_tool_results_pairing_one_to_one`（`:1379`）。

**现状实测**：同一测试二进制内两个测试共用 pattern `"save_draft"` 时互相污染，前者偶发 `hits=0`（阶段 A 修正 7 记录在案）。真实产品面上：A 会话的搜索结果会被 B 会话原样拿到，工作区变更后仍复用。

**目标**：缓存作用域与注释一致——按会话（`SessionLog` 生命周期）隔离；同会话内的复用行为与 call_id 配对语义保持今日样子（阶段 A 红线不得回退）。

**验收判据**
1. 新测试：两个独立 `AppContext` + 两个独立 `SessionLog`，相同 args 的 search 在会话 A 派发后，会话 B 同 args 调用必须**真实派发**（计数工具 `hits == 1`），而不是复用 A 的输出。
2. 既有 `repeated_search_replays_from_memo_without_denial` 与 `search_memo_hit_keeps_tool_results_pairing_one_to_one` 保持绿，且**去掉独享 pattern 的隔离注释后仍然绿**（污染根因消除）。
3. `replayed_sessions_emit_no_denied_tool_results` 的录制值豁免口径可保留（它防的是旧数据），但注释须更新：跨 `call_id` 复用不再可能。
4. `cargo test --workspace` exit=0；改动文件零新 clippy 警告。

---

## B2 §4.5 历史层收口：`[error]` 文本不回喂

**锚点（实测）**
- 写入点：`harness-runtime/src/agent_loop.rs:1356`（provider 错误以 `[error] {…}` 作为 assistant 文本落日志）；另一生产者 `harness-runtime/src/council.rs:418`。
- 唯一出站关口：`agent_loop.rs:2717 fn prepare_request_messages`（阶段 3 已收口，实时三处与回放共用）。
- 协议面已修：搜索缓存只存载荷不存 `call_id`。

**目标（spec §4.5 待做项）**：provider 错误文本改为不进入模型上下文的事件，或至少在重建请求时剥离 `[error]` 前缀的 assistant 文本。spec R6 指出叠加后果：连续失败后上下文既非法又自增殖。

**验收判据**
1. 新测试：构造一次 provider 错误后继续的回合序列，断言 `prepare_request_messages` 的输出里**不存在**以 `[error]` 开头的 assistant 消息，而 `SessionLog` 仍保留该事件（UI 可见性不丢）。
2. 回放路径同口径：`tests/session_replay.rs` 增加一条「出站请求无 `[error]` 前缀」断言。
3. `[需要澄清]` 与 `ReplayLlm` 耗尽回退文本**不得**被误剥离（Global Constraints 明令保留）。
4. 既有 `dsml_agent_loop` 与协议 400 相关测试保持绿。

---

## B3 §4.2 度量换成信息增益

**锚点（实测）**
- `MAX_ZERO_GAIN_STREAK` 全仓 **0 处**——该常量与判定都还没实现。
- 现有代理度量：`goal_execution.rs` 的 `no_information_count` / `item.no_information_streak`（`record_gate_rejection` 内，`:1927` 区域），以及 `ToolRepeatGuard::flags_repeat`（阶段 A 后只产提示）。
- 载体已存在：`case_file.rs:14-29` —— `TriedEntry { tool, signature, ok, summary }`、`eliminated: BTreeSet<String>`、`anchors: BTreeSet<String>`。

**目标**：判定动作是否推进，改看它是否向 `CaseFile` 新增**事实**（新文件、未覆盖过的行区间、新的失败签名、首次成功验证）；增益为零连续 3 次 → 交给 `TurnGovernor` 既有策略栈 `[换路]` / 栈底常驻处置，门禁不就地拒绝。成本上限仍由 R3（300k）单一顶兜底。

**需先定的设计点（写子计划时必答）**
- 「事实」在 `TriedEntry` 上如何表达才不新增 `SessionEvent` schema（约束：schema 冻结）。倾向：增益判定读 `CaseFile`，不新增事件类型。
- 常量落点：与策略栈窗口 W 同处配置（spec §4.2 原话）。

**验收判据**
1. 单测：同一文件重复读第二次起 `gain == 0`；新文件 / 新区间 / 新失败签名 / 首次成功验证各自 `gain > 0`——四类各一条断言。
2. 集成：连续 3 次零增益后，日志出现控制器 `[换路]` 处置，且**没有**任何工具结果被替换（`denied_contents` 为空）。
3. 红线不劣化：`advisory_gates_never_replace_a_dispatched_tool_result`、`repeated_identical_calls_run_and_are_hinted_at_most_once_per_step` 保持绿。
4. `no_information_count` 降级为纯遥测（保留字段，D2）。

---

## B4 §4.3 已确认事实不再被压缩掉

**锚点（实测）**
- `agent_loop.rs:2787 fn compress_stale_tool_results`：`RECENT_FULL = 12`、`STALE_EXCERPT = 300`，无差别截断 12 条之前的工具输出。
- `agent_loop.rs:2975 fn apply_context_budget`：预算来自 `harness_core::tuning::context_budget_chars()` → `HARNESS_CONTEXT_MAX_CHARS` → 默认 96k；旧回合压成短摘要。
- 现状因果链（spec R4）：读不到 → 模型以为没读过 → 重读 → 烧预算。实测样本：`server/routers/strategy.py` 在 `bada6772` 被读 11 次。

**目标**
1. 压缩豁免：「已确认目标文件 + 其关键摘录」与用户显式上传的附件摘录不受截断（spec D3：每文件 8 000 字符沿用 `render_attachment_context`，另加单回合豁免合计 24 000 字符，超出部分照常压缩）。
2. 重读即复用：同一文件同一区间再次请求时返回已存事实摘要 + 「如需最新内容需显式刷新」，不重发原文、不重跑工具。`SEARCH_MEMO` 现有 call_id 语义（阶段 A 修好的）必须保持。

**验收判据**
1. 单测：构造 20 条工具结果，断言已确认事实条目在 `compress_stale_tool_results` 后长度未被截到 300，而非事实条目仍被截（两者必须在同一输入里对照）。
2. 单测：豁免合计超过 24 000 字符时，超出部分照常压缩（封顶生效，不是无限豁免）。
3. 回放指标：`7ba3370f_*` 与 `success_677bd6e0` 重放后「同文件同区间重复读」次数 ≤1（口径按 §6 写进 `session_replay.rs` 与 `governance_redline_check.py` 双侧）。
4. 依赖 B1：跨会话污染未修前，本判据可能被污染结果伪装成达成——B1 必须先绿。

---

## B5 §4.4 完成判定按验收项↔证据映射

**锚点（实测）**
- `harness-runtime/src/execution.rs:803 fn can_complete`（`write_operations` 在该文件 19 处引用，含 `:495 :524 :572 :581 :648 :668 :676 :793`）。
- 未完成话术：`agent_loop.rs:2458`「未完成：执行证据不足，无法安全修改。\n下一步：…」等。
- `harness-session/src/log.rs:177-187` —— `DeliveryOutcome` 现有变体：`Verified / NeedsUserInput / PartialDelivery / SystemFailure / Blocked / Interrupted`，**没有 `Delivered`**。
- 已被本轮消解的一半：spec R5 里「要求用户回『写入修改』」的文本在 `src/` 中已 grep 不到（35 个会话对应缺陷的产物在 `dist/`），说明该症状已由 `delivery_workflow::instructions` 侧缓解，但判定仍受 `write_operations` 影响。

**目标**：`DeliveryOutcome` 由「每个验收项是否有对应成功证据」决定，`write_operations == 0` 降为诊断字段。反向仍成立：有写入但无验证不得 `Verified`（`delivery_workflow` 测试的 `no_op` 分支已覆盖，保持）。

**⚠ 阻塞于 Q1**：spec §6 红线 1 要求「其余 ≤2 个必须是 `Delivered` 且结论正确」，§4.4 说零写入正确定位「是 `Delivered` 的一种」，而 §8 禁止引入新状态 schema。三者不能同时成立——见文末「待你裁定 Q1」。裁定前本工作流只能写到「设计 + 判据」，不能写实现步骤。

**验收判据（待 Q1 后细化）**
1. 零写入但正确回答「为什么报错」并给出定位结论的场景 → 判定为 Q1 选定的那个终态，且**不**要求用户补授权。
2. 有写入但无验证 → 不得 `Verified`（既有 `no_op` 断言保持）。
3. 篡改/回滚场景不得 `Verified`：`self_repair_release.rs`、`monitoring*` 相关既有测试保持绿（`artifact_baselines` 回滚检测是保留项）。
4. `write_operations` 在判定式中的角色变为诊断（断言：改它的值不影响本场景终态）。

---

## B6 §4.6 收尾删除（5c 已完成 / 5d / T6 / T7）

**锚点（实测）**
- 5c **已完成**：`ToolRepeatGuard` 已降级为纯信号（阶段 A Task 5，`9a5ed9f`）。勿重做。
- 5d `GovernorMode::Legacy`：`agent_loop.rs:38`（枚举变体）、`:44`（「步骤⑤删除 Legacy 前保留一个阶段的逃生门」注释）、`:47`（`HARNESS_GOVERNOR=legacy|off|0` 解析）、`:258 :747 :1068`（三处「Legacy 下恒为 None」说明性注释）、`:4274-4276`（测试）；另有 `tests/session_replay.rs:275` 的 `clarification_loop_replay_emits_delivery_per_turn` 用 `GovernorMode::Legacy` 驱动。
- T6 冗余收缩：`goal_execution.rs`（4723 行）与 `execution.rs`（2640 行）中的旧原子规则与旧阶段机；阶段 A 已把 `execution.rs` 四条 legacy 原子规则降为提示（`authorize()` 现仅测试触达）。
- T7 待 deprecated 文档六份（清单在 `docs/superpowers/plans/2026-09-01-governance-phase3-counter-retirement.md` 事实 8，第 27 行）：`docs/AGENT_GOAL_EXECUTION_FRAMEWORK_V3.md`、`AGENT_GOAL_SOLVING_MECHANISM_V4.md`、`AGENT_GOAL_SOLVING_MECHANISM_V5.md`、`AGENT_DELIVERY_REFORM_PLAN.md`、`agent-loop-interruption-root-cause-and-fix.md`、`intent-clarification-gate-signal-driven-adr.md`；另 `2026-09-01-...-phase3-...md:218` 要求 `docs/architecture.md` 治理章节指向 spec。

**验收判据**
1. `GovernorMode` 消失或只剩单一控制器路径：全仓 grep `Legacy` 在 `harness-runtime/src` 命中数为 0；`HARNESS_GOVERNOR` 环境变量不再改变行为。
2. 迁移安全：现有以 `GovernorMode::Legacy` 为输入的测试改为控制器路径后**仍覆盖同一症状**（不允许直接删测试了事）。
3. T6 收缩不得引入行为变化：`cargo test --workspace` exit=0，且阶段 A 的四条红线全绿（它们是本项的回归护栏）。
4. T7：六份文档头部有 deprecated 标记并指向本 spec；`docs/architecture.md` 治理章节指向 spec + 阶段计划。

---

## B7 §6 硬验收：10 fixture 集与交付率指标

**锚点（实测）**
- 现有 5 个 fixture 合计约 **12.5 MB**：`7ba3370f_full.jsonl` 6.1M、`7ba3370f_t19_22_gitfix.jsonl` 4.7M、`7ba3370f_t03_14_symptom.jsonl` 1.4M、`success_677bd6e0.jsonl` 311K、`7ba3370f_t15_18_clarification.jsonl` 6.8K。
- 指标常量已存在：`harness/scripts/governance_redline_check.py:17-19` `PROMPT_CAP = 300_000`、`A1_CAP = 12`、`A2_CAP = 2`（A1=首次写入前调用数、A2=同参重读豁免）。**尚无交付率与零重读口径。**
- 场景定义：`harness/scripts/governance_ab_run.py:39 SCENARIOS`（现仅 S1/S2/S3），`run_scenario` 在 `:202`。
- **采集面已变化**：spec §6 点名的 `bada6772`（策略页保存报错、零写入）**已不在** `.harness/sessions/`；该目录现有 35 个会话，含 spec §2.1 的 `99372653`（35M，最坏一例）与 `5910d2c4`。单文件最大 63M。
- 仓库无 `.gitattributes`，即**未启用 git-lfs**。

**目标（spec §6 全部须同时满足）**：交付率 ≥8/10（真跑磁盘与测试）；零拦截（运行时合成 `gate]`/`guard]` = 0）；零重读（同文件同区间跨回合重复读 ≤1）；首次写入前调用数 ≤12；既有红线不劣化（provider 错误不得 `Verified`、单会话 ≤300k、失败回合 100% 带四要素资产、基线 `7ba3370f_full.jsonl` 违例集合不得缩小）。

**⚠ 阻塞于 Q2 + Q3**：需要可用端点与新会话采集授权；需要决定多 MB 会话如何入库。

**验收判据**
1. fixture 清单文件化（10 个名字 + 来源会话 + 是否非代码面），spec D4 要求的「≥2 个非代码交付面」可被清单直接证明，不靠现有 ab-runs 存量凑数。
2. 交付率与零重读两项指标在 `session_replay.rs` 与 `governance_redline_check.py` **两侧同时存在且口径一致**（写进同一次提交；差异由测试断言，不靠人记）。
3. 五类红线在实机 `governance_ab_run.py` 下 `exit=0`，`summary.md` 落盘并记录三场景（或 S1–S5）各自的调用数、首次写入前调用数、`gate]`/`guard]` 计数。
4. 重放绿灯**不算**通过——spec §7 明确「脚本化模型绿灯不等于真机成功」。

---

## B8 流程债（合进 B6 的文档提交）

- `docs/superpowers/plans/2026-09-01-governance-phase2-case-file-and-controller.md`：实测 **58 项 `- [ ]`、0 项 `- [x]`**，而对应工作早已提交（spec §4.6 末句点名这条）。
- `docs/superpowers/plans/2026-09-01-governance-phase3-counter-retirement.md`：实测 **31 项 `- [ ]`、0 项 `- [x]`**，其中 5c 已由阶段 A 完成。
- 动作：两份文件头部各补一行「实际进度状态（日期，实测 commit 佐证）」+ 把已完成项勾选，或在文件头声明「本计划已由 X/Y 取代，保留为历史推演记录」。二者择一，不许留「看起来没做」。

---

## 阶段 A 已顺带完成 / 已定性，勿重做

- 5c `ToolRepeatGuard` 降级为纯信号（`9a5ed9f`）。
- `execution.rs` 四条 legacy 原子规则降为提示、`[goal-execution gate]` 降级（`714b330`）。
- 标记分类法与系统提示故障标签清单（`714b330`）。
- 遥测 `phase` 与 `active_work_item` 同源（`47b400c`）——spec §10 的「未核实项」两条已判读并回写。

## 待你裁定（三项，都不宜由我代答）

**Q1（阻塞 B5，影响 B7 判据）**：`Delivered` 与「不引入新状态 schema」冲突。可选：
- A. 复用 `PartialDelivery` 表达「零写入但已定位」，红线 1 文案改成「PartialDelivery 且结论正确」——不动 schema。
- B. 新增 `DeliveryOutcome::Delivered`——语义最干净，违反 spec §8，需同时改 `harness-session` 与 UI 分支。
- C. 零写入正确定位判 `Verified`——不建议，会把「未落盘」说成「已验证」。

**Q2（阻塞 B0、B7）**：实机采集授权。`governance_ab_run.py` 会真跑模型多回合会话（S4/S5 需新采），消耗真实 API 额度。需要：端点名（`model_profiles` 现为空表）+ 是否授权新采两场真实会话。

**Q3（影响 B7 形态）**：多 MB 会话如何入库。现状 5 fixture 已 12.5MB，按 10 fixture 与现有源会话体积（单文件最大 63M）估算会显著推大仓库。可选：A. 只入库裁剪后的回合子集（像 `7ba3370f_t03_14_*` 那样切片）；B. 启用 git-lfs（需装依赖并改写历史之外的新文件）；C. fixture 不入库，改由脚本从 `.harness/sessions/` 现取现用（CI 与本机不一致的风险）。
