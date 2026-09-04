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
