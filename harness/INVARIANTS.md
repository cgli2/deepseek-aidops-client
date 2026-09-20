# 运行时不变量（Invariants）

本文件汇总 harness 必须满足、且由代码/测试可验证的不变量（设计文档 §8）。编译期组合
（`compose_plugins`）保证了多数结构性不变量，无需运行时检查。

## 结构性（编译期即可保证）

1. **会话真相源**：模型可见的一切必须从 `SessionLog` 重建；到达模型的输入必有对应
   `SessionEvent::*`（`AgentLoop::run_turn` 在 `llm.stream` 前已 `log.append(Assistant/...)`）。
   fork/resume/replay 全从日志派生。

2. **换 Provider 不改 Consumer**：`BashTool`/`FsTool`/`EditTool` 只依赖
   `Arc<dyn Shell>` / `Arc<dyn Fs>` / `Arc<dyn Editor>`；把 `LocalBash` 换成 `WasmShell`，
   Consumer 源码零改动。判定标准：grep `harness-tool` 不得出现任何具体 Provider 类型。

3. **可逆注册（effect 自动回滚）**：`ComposeGuard` 持有的所有 `Registration` drop 后，
   `ctx.get::<S>()` 必须失败（服务已移除），且对应事件订阅消失。`Registration` 的 `Drop`
   同时回滚服务与处理器两类贡献。

4. **工具结果不可变**：`ToolRegistry::dispatch` 对未知工具返回 `ok=false` 的冻结结果，
   不 panic、不修改日志。

5. **组合即生命周期**：`compose_plugins` 返回的 `guard` 存活 = 插件集合存活；`guard` drop
   即卸载全部插件（无残影）。`AppContext` 可 `Clone` 并 move 进 `tokio::spawn`。

6. **唯一终止检查点**：`agent/turn-stopping`（`TurnStopping`）是循环唯一可终止处（serial，
   无 `next()`）；其余阶段（PreStep 瀑布、工具管线）不得自行终止。

## 安全 / 隔离

7. **沙箱作用于子进程**：`Sandbox::prepare` 只套用在被 `spawn` 的子进程 `Command` 上，
   不套在 harness 自身（§9）。

8. **fail-closed 审批**：默认 `ApprovalPolicy::Ask`；缺失配置时按"拒绝"处理。

9. **WASM 零直接能力**：`harness-provider-wasm` 加载的不可信代码只能调用 host 显式导入的
   函数，不得直接触碰文件系统/网络/进程；能力由受信 Provider 经导入表暴露（§11.4）。

## 借鉴 Codex 的系统能力（§13）

11. **钩子 fail-closed**：`ShellHook` 在命令执行失败（非 0 退出 / 无 JSON 输出）时默认返回
    `Block`，安全优先于便利；未配置任何钩子时退化为 `NullHook`（全放行），循环不中断。

12. **Worktree 必清理**：`WorktreeGuard` 在 `Drop`（含 panic 展开）时调用 `git worktree remove`，
    不存在孤儿 worktree；进入即承诺、离开即清理（呼应 dsh `effect()` 回滚，不变量 3）。

13. **记忆 / 钩子 / Git 皆为能力接缝**：替换其 Provider（如 `FileMemory` → 向量库、`ShellHook`
    → WASM 钩子）不改 Consumer 与 `AgentLoop`；三者均以 `ExtensionPoint::{Memory,Hook,Git}`
    登记（不变量 2 的延伸）。

## 并发

10. **单运行时 + 层级取消**：所有会话在单个 tokio 运行时内，经 `JoinSet` + `CancellationToken`
    层级取消；父取消传播到子代理。

## 长任务 DRLA P0–P2

14. **WAL 先于状态**：`DurableDag` 的每次合法状态变化先同步追加 JSONL WAL，再替换
    内存状态；重放要求序号连续，仅容忍崩溃留下的最后一条不完整记录。

15. **硬事实必须有独立证据**：`FactMatrix::write_hard` 必须经 `ArtifactVerifier`
    重新计算允许目录内工件的 SHA-256；摘要轨永远不能直接驱动质量门禁。

16. **修复能量严格下降**：只有带 Verifier 身份与证据哈希的快照能更新
    `EnergyLedger`；能量持平进入 HITL，回升被拒绝，新增 skipped test 被视为 Goodhart
    违规并拒绝。

17. **预算耗尽是终态**：常规预算不能使用恢复储备；储备只用于写断点与部分交付报告，
    随后任务必须写入 `TaskStatus::BudgetExhausted`，不得无限挂起或自动续期。

18. **外部副作用显式分级**：可补偿与不可逆副作用必须有幂等键；可补偿动作必须绑定
    补偿操作，不可逆动作还必须绑定到同一提案、动作和负载摘要的持久化 HITL 确认；
    幂等键不得被不同提案复用。

19. **契约锁尽力而为且失败关闭**：Rust/TypeScript/TSX 公共接口由 Tree-Sitter 快照；
    删除或修改已锁接口必须被拒绝。宏展开、FFI、反射等盲区仍以编译和测试为最终门禁。

20. **租约可回收、预算全局化**：Worker 心跳只能续租自己的 Running 任务；过期租约由
    Watchdog 回收并受 `max_retries` 限制。所有 Worker 共享持久化 RPM/TPM 与总 Token 预算，
    429 进入带确定性抖动的指数退避。

21. **工件不可变、权威选择显式**：Artifact Vault 以 BLAKE3 内容寻址并在读取时复核；
    逻辑键发布使用 generation CAS。权威版本只能由具名 Aggregator 决策，或由与候选哈希
    精确绑定的 VersionConvergence HITL 决策选出。

22. **跨重启控制面可重放**：DAG、黑板事件、HITL 决策、副作用 Saga 和预算状态均持久化；
    JSONL 只修复崩溃产生的末尾半条记录，中段损坏或序号断裂必须失败关闭。

## 交互式 Agent 过渡期安全规则

- `Delivery` 是交付结论，`TurnEnd` 只表示回合闭合。控制器捕获的取消、超时和异常在尚无 `Delivery` 时先补结构化报告；已有报告不得重复生成。
- **唯一交付裁决**：终态由纯函数 `evaluate_delivery(&DeliveryFacts) -> DeliveryDecision` 单点裁决，不读时钟/工作区/日志，相同输入必得相同结论。`CompletionJudge`、`GoalExecution`、模型文本只能提出候选（`solver_claims_complete`），无权直接产出 `Verified`。终态 `DeliveryReport` 的验收项与证据由 `DeliveryDecision::into_report()` 单一来源生成；`ExecutionState::can_complete`/`delivery_report` 只作 fail-closed 降级防线（可把 `Verified` 降为 `PartialDelivery`，绝不反向升级）。
- 视觉故障不能靠源码字串、`cargo check` 或无关单元测试进入 `Verified`。缺少实际界面证据时保留为未验证。
- 搜索结果缓存只在当前回合有效，观察到工作区写入后失效。历史 `fs.read` 只有在当前磁盘视图逐字一致时才可恢复；无版本信息的历史搜索不能当作当前事实。变更任务从 `TaskCheckpoint` 恢复验收项时，关联文件指纹必须全部匹配；旧日志缺少指纹则重验。
- **版本化断点与续跑守恒**：`TaskCheckpoint` 当前为 v2，携带各验收项文件 BLAKE3 指纹、已否定假设（`rejected_hypotheses`）与落盘时硬预算剩余（`remaining_steps`/`remaining_tool_calls`）；读取兼容 v1（新字段缺省回落，`version ∈ {1,2}` 均接受）。续跑重建执行前沿后回灌已否定假设（不再把已排除路径当新方向重试），并在 v2 断点存在时用剩余成本收紧新窗口硬上限，使总额守恒而非重获无界预算；v1 断点不收紧，避免把旧日志误判为总额耗尽。
- **任务级总成本与出口类型不丢失**：唯一终态出口写入一份成本快照（模型请求数、总 token、墙钟耗时）到遥测，与 `Budget` 持有的步数/工具调用总额共同构成每任务指标。控制器把 `SystemFailure`/`Interrupted`/`Blocked` 对用户收敛为 `PartialDelivery` 时，归一化前的具体终态类型必须保留在遥测 `terminal_outcome`，不得因收敛而丢失故障/暂停区分。
- 预算分为有次数上限的软窗口和不可续期的硬总额：软窗口最多使用 `max_renewals` 次；常规续期和最终收尾窗口的步数/工具调用额度都不得越过硬总额。命中硬总额立即停止，不因进展、失败重试线索或模型文本自动获配新额度。
- 上述规则是当前防护边界。唯一交付裁决、版本化断点（v2）与任务级总成本已落地并由测试覆盖；结构化模型解释、可重复的视觉/行为验证器、任务关系精确匹配与灰度真实 UI 对照尚未实现，视觉验收仍为“待验证”。完整门槛见 `docs/agent-runtime-reconstruction-implementation-plan-2026-09-18.md`。
