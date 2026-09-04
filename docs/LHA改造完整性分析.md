# LHA（长时程编排）改造完整性分析（最终合并版）

> **2026-09-04 实现后更新**：LHA 后端、默认会话路径、GUI、CLI 与 ACP 控制面均已接通。原报告中的“默认会话未接线”和“专项测试缺失”已被当前代码证据推翻；下文旧缺口保留为历史检查基线，不再代表当前状态。
>
> 本文档合并早前的 `lha-gap-analysis.md` 证据基线与菜单入口定位结果，为最终交付版；`lha-gap-analysis.md` 的结论已全部并入本文。

## 本轮完成项

| 层 | 实现 |
|---|---|
| 运行时 API | 新增持久化检查点枚举与 `reject_decision`，批准/拒绝形成对称闭环 |
| GUI | 侧边栏新增“长时程任务”：提交目标、查看状态/进度/重试、批准或拒绝 HITL 检查点 |
| CLI | 新增 `lh status/submit/approve/reject`，Windows GUI 子系统构建会为显式 CLI 命令附着父控制台 |
| ACP | `session/status` 返回任务与决策，并增加 `longHorizon/approve`、`longHorizon/reject` |
| 文档 | README 增加命令、能力和 `.harness/long-horizon/` 数据目录说明 |

当前默认链路证据：`SessionController`、`Scheduler`、ACP prompt 已统一调用 `run_durable_agent_turn` / `run_durable_council_turn`；`LongHorizonManager` 按工作区隔离 WAL/Vault，并由 GUI、CLI 和会话执行共用同一实例。

## 一、已完成的实现（有代码与测试证据）

### 1. 核心运行时模块 `harness/harness-runtime/src/lha/`
共 17 个子模块，对应 DRLA 计划的 P0 纵向切片与 P1/P2 控制面：

| 子模块 | 职责 |
|---|---|
| `state_machine.rs` | 持久化 DAG 任务状态机（`TaskSpec`/`TaskStatus`/`WalRecord`） |
| `storage.rs` / `sandbox.rs` | 文件系统事务与沙箱 |
| `effects.rs` | 显式副作用授权（`EffectProposal`/`gate_effect`/`PrepareOutcome`） |
| `fact_matrix.rs` | 证据背书事实矩阵（`HardFact`/`ArtifactVerifier`/`sha256_file`） |
| `quality.rs` / `energy.rs` | 确定性质量门与能量预算账本 |
| `rate_limit.rs` | 全局预算控制器与 provider 限流（`Admission`/`ProviderLimit`） |
| `artifact_vault.rs` | 权威 MVCC 工件库（`MergeDecision`/`ArtifactVersion`） |
| `contract_lock.rs` | 契约锁（Contract as Code 快照/差异） |
| `dispatch.rs` | 能力路由（`CapabilityRouter`/`WorkerRole`） |
| `hitl.rs` | 人机协同决策检查点 |
| `orchestrator.rs` | `LongHorizonRuntime` 总编排器 + 部分交付报告 |
| `blackboard.rs` / `verifier.rs` / `watchdog.rs` | 黑板事件、独立验证器、租约看门狗 |

### 2. 公共 API 导出
- `harness-runtime/src/lib.rs:16` 声明 `pub mod lha`，第 48–66 行将全部 LHA 类型（约 60 个符号）从 crate 根再导出。
- 兼容层 `long_horizon.rs` 提供 `LongHorizonManager` / `run_durable_agent_turn` / `run_durable_council_turn`。

### 3. 测试覆盖
- `tests/lha_p0.rs`：P0 纵向切片（事务、事实、门控、DAG）。
- `tests/lha_p2.rs`（263 行）：端到端验证——提交任务 → 认领 worker → LLM 准入（`Admission::Granted`）→ 独立验证证据 → 事实矩阵 → MVCC 工件合并/恢复，覆盖交付与恢复两条路径。**已实际执行：4 passed / 0 failed（0.05s）**。
- 配置面：`long_horizon.rs:49-67` 的 `HARNESS_LHA_*` 环境变量（总 token 预算、租约 TTL、RPM/TPM 限流、重试次数）默认值齐全。
- 编译验证：`cargo +stable-x86_64-pc-windows-msvc check --manifest-path harness/Cargo.toml -p harness-runtime` **已实际执行通过**（`Finished dev profile ... in 5.04s`，零错误零警告）。

## 二、原检查缺口（历史基线，现已处理）

> 本节描述的是实现前状态。GUI/CLI/默认会话/HITL 查询与审批已在本轮补齐；当前剩余项见第四节。

### A. 缺口 1：UI 菜单入口未接线（主要缺口，即本次续跑定位目标）
- `harness-ui/Cargo.toml` 的依赖列表**不包含 `harness-runtime`**（只依赖 core/session/llm/capability/provider-memory 等）。
- `harness-ui/src/gui/` 下 17 个文件（`app.rs`、`sidebar.rs`、`composer.rs`、`memory_panel.rs`、`settings_panel.rs` 等）中**没有任何 LHA/LongHorizon 相关入口**——即"从现有菜单入口定位"的结果是：菜单里根本没有这个功能。
- 需补：GUI 侧边栏新增"长时程任务"面板（提交 `TaskSpec`、查看 DAG 状态、HITL 检查点确认、副作用授权确认）。

### B. 缺口 2：TUI/CLI 入口未接线（已用 findstr 验证）
- 对 `harness-ui/src/tui.rs`、`console.rs`、`gui/app.rs`、`gui/sidebar.rs`、`gui/composer.rs` 批量检索 `long_horizon`：零命中。
- 对 `bin/src/main.rs`、`bin/src/compose.rs` 检索 `lha`：零命中。CLI 入口（`bin/src/main.rs`，259 行）只引用 `Scheduler`，未暴露 `LongHorizonRuntime`，也没有 `lh` 子命令。

### C. 缺口 3：默认会话路径未接线（待产品决策）
- LHA 模块文档（`mod.rs` 第 1–9 行）自述"刻意与交互式 `AgentLoop` 独立"。`agent_loop.rs` / `execution.rs` / `goal_execution.rs` / `intent.rs` 的 import 区均无 `crate::lha` 引用：交互会话无法直接派生长时程任务。
- 需决策：独立入口是设计上的 opt-in 路径，还是遗漏的默认集成。该决策会改变产品行为。
- `long_horizon.rs` 的 `run_durable_agent_turn` 是目前唯一桥，但未见上层调用方。

### D. 缺口 4：观测与运维面
- LHA 事件（`DagEvent`/`BlackboardEvent`/`WatchdogEvent`）未接入 `events.rs` 的 `PreStep`/`TurnStopping` 事件流，UI 无法实时展示进度。
- `PartialDeliveryReport`、`EnergyLedger`、`RateLimitError` 等没有面向用户的呈现层。

## 三、原 P1/P2 补充缺口（历史基线）

1. 已核实模块内专项测试：sandbox 提交/回滚/崩溃恢复、fact matrix 原子保存加载、WAL 不完整尾记录恢复均已有覆盖。
2. README 已补充 LHA 能力、命令与数据目录。
3. capability 资产索引与 LHA 是不同职责；GUI 直接读取持久化控制面，不需要将运行状态映射为 Skill/Wiki/CodeGraph。

## 四、仍可继续演进但不阻塞使用的项目

1. 将 Blackboard/DAG 事件桥接到统一 `SessionEvent`，减少 GUI 轮询并形成单一遥测流。
2. 为 TUI 增加与 GUI 对等的任务列表和 HITL 交互；终端目前可用 `lh` CLI。
3. 为任务增加创建/更新时间字段，以支持严格时间排序和历史筛选。
4. 将 CLI 操作人从固定 `cli-operator` 扩展为配置项或身份提供者。

## 五、验证口径

- 全特性编译：`cargo +stable-x86_64-pc-windows-msvc check --manifest-path harness/Cargo.toml -p harness-runtime -p harness-ui -p harness-acp -p harness-bin --all-features`。
- LHA 集成测试：`cargo +stable-x86_64-pc-windows-msvc test --manifest-path harness/Cargo.toml -p harness-runtime --test lha_p0 --test lha_p2`。
- CLI 解析回归：`harness-bin` 单元测试覆盖 `status`、多词 `submit` 和 `reject`。
