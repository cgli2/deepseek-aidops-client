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
