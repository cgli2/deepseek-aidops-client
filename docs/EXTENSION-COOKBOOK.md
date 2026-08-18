# Extension Cookbook（功能 → 扩展点映射）

本文件是 dsh「一切皆插件」思想在 harness 中的落地契约（完成文档 §11.3 / §11.5）。

**判定标准（可验证）**：每一个产品功能都必须映射到 `harness_core::extension::ExtensionPoint`
清单上的某个扩展点，并经由该扩展点的监听器 / 服务实现。**没有任何一行代码需要修改
`AgentLoop` / 工具管线本身**——这是「一切皆插件」的可验证标准。

## 1. 扩展点清单

| 扩展点 | 类型 | 接缝三角色 | 默认 Provider |
| --- | --- | --- | --- |
| `Llm` | 能力 | Definition/Provider/Consumer | `DeepSeek`（feature `deepseek`） |
| `Shell` | 能力 | 同上 | `LocalBash`（`harness-provider-local`） |
| `Fs` | 能力 | 同上 | `LocalFs` |
| `Editor` | 能力 | 同上 | `LocalEditor` |
| `Lsp` | 能力 | `harness-provider-local::LocalLsp` | stdio JSON-RPC、可配置语言服务器命令 |
| `Subagent` | 能力 | `harness-runtime::InProcessSubagent` | 隔离上下文/日志、并发限制、超时 |
| `FileWatcher` | 能力 | `harness-provider-local::PollingFileWatcher` | 跨平台新增/修改/删除检测与丢事件兜底 |
| `Compaction` | 能力 | 同上 | _(M1 占位)_ |
| `PreStep` | 生命周期事件 | 瀑布（around-middleware） | 空链（终态恒等） |
| `TurnStopping` | 生命周期事件 | 串行（唯一终止点） | 空监听器 |
| `ToolPreExecute` | 工具管线 | 瀑布 guard | 空链 |
| `ToolExecute` | 工具管线 | 观察者 | 空监听器 |
| `ToolPostExecute` | 工具管线 | 观察者 | 空监听器 |
| `ToolResult` | 工具管线 | 观察者（写日志） | `AgentLoop` 内置 |
| `Memory` | 能力 | Definition/Provider/Consumer | `FileMemory`（`harness-provider-memory`） |
| `Hook` | 能力（生命周期拦截） | Definition/Provider/Consumer | `ShellHook` / `NullHook`（`harness-provider-hook`） |
| `Git` | 能力 | Definition/Provider/Consumer | `GitCli`（`harness-provider-git`，含 Worktree） |

## 2. 三角色模型（能力接缝）

```
Definition  (harness-capability, 纯 trait, 零实现)   ← 本仓库只改这里声明接口
Provider    (harness-provider-*, 实现 trait, 可多份)  ← 换 Provider 改这里
Consumer    (harness-tool / harness-runtime, 仅依赖 trait) ← 永不直接依赖 Provider
```

换 `LocalBash` → `WasmShell` 时，`BashTool`（Consumer）源码**零改动**（不变量 2）。

## 3. 加一个新能力 Provider（例：`WasmShell`）

1. 在 `harness-capability/src/shell.rs` 确认 `Shell` trait 已是 Definition（已存在）。
2. 新建 `harness-provider-wasm/src/...` 实现 `Shell`（`impl Shell for WasmShell`）。
3. 在 bin 的 `compose.rs` 把 `LocalBash::new(...)` 换成 `WasmShell::new(...)`——
   **`BashTool::new(shell)` 那行一字不改**。
4. `ExtensionRegistry::declare(ExtensionPoint::Shell, "wasm-shell")` 登记归属。

## 4. 加一个新工具（例：`git` 工具）

1. 在 `harness-tool/` 新建 `git.rs`，实现 `DynTool`（依赖 `Arc<dyn Shell>` 或 `Arc<dyn Fs>`）。
2. `ToolRegistry::register(Arc::new(GitTool::new(...)))` 注册。
3. 无需触碰 `AgentLoop` —— 模型产出的 `ToolCall` 自动经 `dispatch` 路由。

## 5. 加一个生命周期钩子（例：PreStep 拒绝危险指令）

1. 实现 `Waterfall<PreStep>`（`call(&self, args, next) -> PreStep`）。
2. `bus.on_waterfall::<PreStep>(Arc::new(MyGuard))` 注册。
3. 不调 `next()` 即短路（拒绝）。**循环本身零改动**。

## 6. 动态插件（WASM，§11.4）

`harness-provider-wasm` 的 `WasmPluginLoader` 在 `wasm-tools` feature 下加载 `.wasm` 字节码，
经 host 导入表把受信能力（Shell/Fs/Editor）暴露给不可信代码。未开启时保留同名占位类型，
下游无需 `#[cfg]` 分支。注册路径与静态插件完全一致（均经 `Plugin::register`）。

## 7. 校验清单

- [ ] 新功能是否映射到某个 `ExtensionPoint`？
- [ ] Consumer 是否只依赖 `harness-capability` 的 trait，而非具体 Provider？
- [ ] 新 Provider 是否在 `ExtensionRegistry` 登记？
- [ ] 是否未修改 `AgentLoop` / 工具管线核心？

## 8. 借鉴 Codex 的四项系统能力（§13）

| 能力 | Definition | Provider | 扩展点 | 集成点 |
| --- | --- | --- | --- | --- |
| 记忆 Memory | `harness-capability::memory::Memory` | `harness-provider-memory::FileMemory` | `ExtensionPoint::Memory` | 可暴露为写/搜记忆工具（Consumer 仅依赖 `dyn Memory`） |
| 钩子 Hook | `harness-capability::hook::Hook` | `harness-provider-hook::{ShellHook,NullHook}` | `ExtensionPoint::Hook` | `agent_loop`（PreToolUse 前 / PostToolUse 后） |
| Git | `harness-capability::git::Git` | `harness-provider-git::GitCli` | `ExtensionPoint::Git` | 调度器 / 工具（仓库状态上下文） |
| Worktree | `git::Git::create_worktree` + `WorktreeGuard` | 同上 | （归 `Git`） | 调度器每会话隔离副本（M-扩展 TODO） |

### 8.1 加一个记忆后端（例：向量库）

1. 在 `harness-provider-*`（新建 crate）实现 `impl Memory for VectorMemory`。
2. 在 `bin/src/compose.rs` 把 `FileMemory::new(...)` 换成 `VectorMemory::new(...)`。
3. `ExtensionRegistry::declare(ExtensionPoint::Memory, "vector-memory")`。
   **循环、`BashTool`、任何其他 Consumer 零改动。**

### 8.2 加一个钩子命令

1. 在 `config/default.toml` 的 `[hooks]` 下加 `"pre_tool_use" = "scripts/guard.sh"`。
2. `scripts/guard.sh` 从 stdin 读 `HookPayload` JSON，写 `{"decision":"block","reason":"..."}` 即可阻断。
3. 无需重编 Rust——钩子是纯外部命令（表驱动）。
