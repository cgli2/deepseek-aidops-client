# 原生 Rust 编码代理 Harness —— 详细设计文档

> 设计目标：一个高性能、跨平台桌面编码代理（类 OpenAI Codex CLI），**照搬 dsh 的架构思想**，把 Cordis 的“运行时动态组合”替换为“**编译期组合 + 可选 WASM 沙箱**”。
> 硬性要求：并发多任务、启动快、idle 占用低、UI 流畅、工具执行（进程 spawn / 文件监听 / LSP 传输 / landlock·seccomp 沙箱）高效。

---

## 1. 文档目的与范围

本文定义该 harness 的分层架构、核心抽象、并发模型、沙箱策略与性能预算。范围覆盖：

- 微内核（`AppContext` + 类型化事件总线 + 会话追加日志）
- 能力接缝（Capability Seam）三角色模式
- 编译期插件组合（替代 Cordis 的 `cordis.yml` 运行时 patch）
- 多任务并发与结构化取消
- 工具执行管线与跨平台沙箱
- UI 作为事件总线的纯消费者

**不在范围**：具体模型权重、训练、远端 BFF 网关、产品账号体系。

---

## 2. 设计目标与非目标

### 目标

| 维度 | 目标 |
|---|---|
| 启动 | 冷启动到首个提示符 < 50ms（不含 LSP/网络懒加载） |
| idle 占用 | 事件驱动，无轮询；常驻内存仅会话日志 + 活动任务态，< 30MB |
| 并发 | 单进程内多 agent 会话并行；子代理扇出；结构化取消 |
| UI 流畅 | 渲染与核心解耦，独立任务 + 事件合并，无卡顿 |
| 工具执行 | `tokio::process` 异步 spawn；OS 级文件监听；LSP 异步 JSON-RPC |

### 非目标

- **默认不做运行时动态加载插件**：行为组合在编译期完成（feature flags + `compose()` builder）。需要动态扩展时，仅通过 **WASM** 加载不可信的用户脚本/工具 Provider。
- 不追求“改 YAML 不重编译”——这是用类型安全与启动速度换来的取舍，对 Codex 类应用完全够用。

---

## 3. 架构哲学（从 dsh 移植的 7 条原则）

| # | dsh 原则 | Rust 原生实现 |
|---|---|---|
| 1 | 一切皆插件 | 每个功能 = 一个向 `AppContext` 注册的插件；无特权核心 |
| 2 | 微内核 + `ctx.<key>` 服务仓库 | `AppContext` 持有 `TypeMap<Arc<dyn Service>>`，按类型取服务 |
| 3 | 能力接缝三角色 | `trait`（Definition）+ 多 `impl`（Provider）+ 仅依赖 trait 的 Consumer |
| 4 | 追加日志为真相源 | `SessionLog` 追加写；fork/resume/replay 全从日志派生 |
| 5 | 可逆注册 | 注册返回 RAII guard，Drop 时自动注销（比 Cordis 手动 disposer 更优雅） |
| 6 | 类型化事件（4 种分发） | `EventBus`：`emit`/`waterfall`/`parallel`/`serial` |
| 7 | 分层组合 | 编译期 `compose(profile)` + Cargo feature flags（替代 cordis.yml patch 层） |

---

## 4. Workspace / Crate 布局

```
harness/                      # cargo workspace
  harness-core/               # 微内核：context, registry, event-bus, lifecycle, session-log
  harness-runtime/            # tokio 编排 + agent loop + 工具管线
  harness-capability/         # 能力 trait（Definition）：shell/fs/editor/lsp/subagent/compaction
  harness-provider-local/     # 本地 Provider：bash/pwsh/fs/terminal
  harness-provider-sandbox/   # landlock+seccomp / App Sandbox / JobObject
  harness-provider-wasm/      # wasmtime 隔离的用户脚本/工具 Provider
  harness-llm/                # LlmProvider trait + OpenAI/DeepSeek/Anthropic/local/replay
  harness-tool/               # 模型可见工具（Consumer）：bash/fs/edit/...
  harness-session/            # 持久化、投影、标题、telemetry
  harness-ui/                 # UI 入口（trait）：tui(ratatui) / gui(slint)
  harness-acp/                # 可选 ACP stdio JSON-RPC 服务器
  harness-sdk/                # 可选的 out-of-process JSON-RPC 客户端
  bin/                        # dsh 二进制：按 profile 组合并启动
```

每个 crate 独立测试、独立 invariant 文档，遵循 dsh 的“约定即代码”纪律（见 §15）。

---

## 5. 核心抽象（详细）

### 5.1 AppContext 与 TypeMap 服务仓库

```rust
// harness-core/src/context.rs
pub trait Service: Send + Sync + 'static {}

pub struct AppContext {
    services: TypeMap,            // anymap / typemap：Type -> Arc<dyn Service>
    events:   EventBus,
    log:      SessionLog,
}

impl AppContext {
    /// 注册服务，返回 RAII guard；guard Drop 时自动注销。
    pub fn provide<S: Service>(&mut self, s: Arc<S>) -> Registration<S>;
    /// 取服务；未注册则编译/运行期报错（结构性保证 Consumer 必被满足）。
    pub fn get<S: Service>(&self) -> Arc<S>;
    pub fn events(&self) -> &EventBus;
    pub fn log(&self) -> &SessionLog;
}
```

`TypeMap` 用 `std::any::TypeId` 作键；`Arc<dyn Service>` 保证跨任务共享、零拷贝读取。

### 5.2 插件与编译期组合

```rust
// harness-core/src/plugin.rs
pub trait Plugin: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    /// 向 ctx 贡献服务/事件监听器；返回 disposer 由 compose 统一管理。
    fn register(self: Arc<Self>, ctx: &mut AppContext);
}

// bin/main.rs —— 编译期组合，替代 cordis.yml 的运行时 patch 层
pub fn compose(profile: Profile) -> (AppContext, ComposeGuard) {
    let mut ctx = AppContext::default();
    let mut guard = ComposeGuard::new();
    match profile {
        Profile::Tui => {
            guard.add(LocalBash::arc().register(&mut ctx));
            guard.add(LocalFs::arc().register(&mut ctx));
            guard.add(DeepSeek::arc().register(&mut ctx));
            guard.add(TuiUi::arc().register(&mut ctx));
        }
        Profile::Headless => {
            guard.add(LocalBash::arc().register(&mut ctx));
            guard.add(ReplayLlm::arc().register(&mut ctx));
        }
        Profile::Acp => { /* ... */ }
    }
    (ctx, guard)   // guard Drop = 全部插件卸载，不留残影
}
```

`#[cfg(feature = "wasm-tools")]` 控制是否把 `WasmProvider` 编入；未启用的代码被 dead-code 消除，缩小二进制、加速启动。

### 5.3 可逆注册（RAII）

```rust
pub struct Registration<S> { marker: PhantomData<S>, key: TypeId, ctx: Weak<()> }
impl<S> Drop for Registration<S> {
    fn drop(&mut self) { /* 从 TypeMap 移除 S，回滚事件订阅 */ }
}
```

卸载插件 = 丢弃其 `Registration` 集合；HMR 与测试可验证“卸载后所有注册消失”。

### 5.4 类型化事件总线（4 种分发）

```rust
// harness-core/src/event.rs
pub trait Event: Send + 'static { type Output; }

pub struct EventBus { /* 按 Event::TypeId 分组的类型擦除处理器表 */ }

impl EventBus {
    pub fn on<E: Event>(&self, h: impl Handler<E>) -> Registration;
    pub async fn emit<E: Event>(&self, e: E);                 // fire-and-forget（spawn 任务）
    pub async fn parallel<E: Event>(&self, e: E) -> Vec<...>; // await 所有观察者
    pub async fn serial<E: Event>(&self, e: E) -> E::Output;  // 注册顺序，返回末值
    pub async fn waterfall<E: Event>(&self, e: E,
        chain: Vec<Arc<dyn Waterfall<E>>>) -> E::Output;       // around-middleware
}
```

`waterfall` 是核心拦截机制：监听器拿到 `(args, next)`，调用 `next()` 委托，不调则短路：

```rust
pub trait Waterfall<E: Event>: Send + Sync {
    fn call(&self, args: E, next: &dyn Fn(E) -> E::Output) -> E::Output;
}
```

权限、hook、审计、指标均以 waterfall 串在工具管线前后，与具体工具家族无关。

### 5.5 会话追加日志（真相源）

```rust
// harness-session/src/log.rs
pub struct SessionLog { db: SqliteWal /* 或 redb/LMDB */ }
impl SessionLog {
    pub fn append(&self, ev: SessionEvent);   // 仅追加
    pub fn replay(&self) -> impl Iterator<Item = SessionEvent>;
    pub fn fork(&self, at: EventId) -> SessionLog;  // 派生新会话
}
```

`SessionEvent` 覆盖 `turn/start`、`step/start`、`assistant/*`、`tool/*`、`turn/end` 等。**运行时不变量**：任何到达模型请求的输入都必须有对应日志事件；模型可见状态只能从日志重建。

### 5.6 Agent 循环 / Turn-Step 生命周期

```rust
// harness-runtime/src/agent_loop.rs
impl AgentLoop {
    pub async fn run_turn(&self, input: UserInput) -> Result<()> {
        self.log.append(TurnStart);
        let mut debt = 1usize;
        while debt > 0 {
            debt -= 1;
            self.log.append(StepStart);
            let msg = self.events.waterfall(PreStep { input }, &self.pre_chain).await; // 可重写/拒绝
            let resp = self.llm.stream(msg).await?;
            self.log.append(Assistant { ..resp });
            for tc in resp.tool_calls {
                self.log.append(ToolCall { ..tc });
                let out = self.execute_tool(tc).await?;   // §8 管线
                debt += out.continuation_debt;
            }
            let stop = self.events.serial(TurnStopping { .. }).await; // 唯一终止检查点
            if stop.should_stop { break; }
        }
        self.log.append(TurnEnd);
        Ok(())
    }
}
```

`Turn` = 0..n `Step`；`debt` 计数控制续跑；`agent/turn-stopping` 为唯一串行终止点。

---

## 6. 能力接缝模式（含 Shell 示例）

```rust
// harness-capability/src/shell.rs —— Definition（纯 trait，零实现）
pub trait Shell: Send + Sync {
    async fn run(&self, req: ShellRequest) -> Result<ShellOutput>;
}

// harness-provider-local/src/bash.rs —— Provider A
pub struct LocalBash { sandbox: Arc<dyn Sandbox> }
impl Shell for LocalBash { /* tokio::process + pre_exec 沙箱 */ }

// harness-provider-wasm/src/shell.rs —— Provider B（隔离）
pub struct WasmShell { engine: Engine }
impl Shell for WasmShell { /* wasmtime 内执行，无宿主机 FS/网络 */ }

// harness-tool/src/bash.rs —— Consumer，仅依赖 trait
pub struct BashTool { shell: Arc<dyn Shell> }
impl BashTool {
    pub async fn call(&self, cmd: &str) -> ToolResult {
        let out = self.shell.run(ShellRequest::new(cmd)).await?; // 不知 Provider 是谁
        ToolResult::from(out)
    }
}
```

**判定标准**：把 `LocalBash` 换成 `WasmShell`，`BashTool` 零改动——即“一个 Provider swap 改变整个产品”。

---

## 7. 并发模型（多任务）

```rust
// harness-runtime/src/scheduler.rs
pub struct Scheduler {
    rt:      tokio::runtime::Handle,
    sessions: Arc<RwLock<HashMap<SessionId, SessionHandle>>>,
    cancel:  CancellationToken,   // tokio-util，分层取消
}

impl Scheduler {
    pub async fn spawn_session(&self, ctx: AppContext, task: Task) -> SessionId {
        let token = self.cancel.child_token();
        let id = SessionId::new();
        let handle = tokio::spawn(async move {
            let loop_ = AgentLoop::new(ctx);
            tokio::select! {
                r = loop_.run_turn(task.input) => r,
                _ = token.cancelled() => Err(Error::Cancelled),
            }
        });
        self.sessions.write().insert(id, SessionHandle { handle, token });
        id
    }
}
```

要点：

- **单 tokio 多线程运行时**（work-stealing），所有任务在其上调度；无“每进程一线程”。
- **结构化并发**：`JoinSet` + `CancellationToken` 实现层级取消（取消父 = 取消所有子代理）。
- **有界通道**：`tokio::sync::mpsc(cap)` 承载事件与工具结果，背压天然限制内存，保证 idle 低占用。
- **共享只读**：服务以 `Arc` 共享；配置用 `Arc<RwLock<..>>` 或 `arc-swap` 无锁热更新。
- **会话日志是跨任务同步点**：并行会话各自追加，互不阻塞（WAL 并发写）。

---

## 8. 工具执行管线

```
tools/pre-execute   (waterfall: hook / 权限 / 沙箱预检)
  → monotonic guards (不可重排的最终拒绝，fail-closed)
  → tools/execute    (waterfall: 超时 / 重试 / 指标)
  → tools/post-execute (waterfall: 接受 / 阻止 / 替换 / 附加上下文)
  → finalize_content (内容不变量)
  → tools/result     (冻结的权威结果，广播给会话日志与 UI)
```

权限/沙箱三轴（与 dsh 一致）：

| 轴 | 取值 |
|---|---|
| `sandbox_mode` | `ReadOnly` / `WorkspaceWrite` / `DangerFullAccess` |
| `approval_policy` | `Ask` / `Never` / `Unavailable`（默认 fail-closed） |
| `permission_preset` | 捆绑上述两轴的用户友好层 |

`bash` 与 `fs` 共享同一沙箱根，避免二者限制到不同目录。

---

## 9. 沙箱（landlock·seccomp + WASM）

```rust
// harness-provider-sandbox/src/lib.rs
pub trait Sandbox: Send + Sync {
    /// 在受限环境下 spawn 子进程；pre-exec 中套用平台隔离原语。
    fn spawn(&self, cmd: &Command, policy: &SandboxPolicy) -> Result<RestrictedChild>;
}

pub struct LandlockSeccomp;  // Linux：landlock 限文件 + seccomp-bpf 限 syscall + namespaces
pub struct AppSandbox;       // macOS：sandbox-exec / App Sandbox（无 seccomp）
pub struct JobObject;        // Windows：Job Object + AppContainer
```

要点：

- 沙箱作用于**被 spawn 的子进程**，不套在 harness 自身上。
- Linux：`landlock` crate 限 FS 访问，`seccompiler` 编译 BPF 限 syscall，`nix::unistd` 做 `unshare`/namespace。
- macOS/Windows 无 seccomp，抽象成 `Sandbox` trait 的平台实现，保持 Consumer 无感。
- **WASM = 不可信代码的天然沙箱**：`wasmtime` 默认无 FS/网络，仅暴露显式 host 函数；用于 workflow 引擎与用户自定义工具 Provider。

---

## 10. UI 层（作为事件总线消费者）

```rust
// harness-ui/src/lib.rs
pub trait Ui: Send + Sync { fn run(self: Arc<Self>, bus: EventBus, log: SessionLog); }

// 两种实现均为消费者，互不耦合
pub struct TuiUi;   // ratatui + crossterm（默认，最像 Codex CLI，开销最低）
pub struct SlintUi; // slint（原生 GUI 窗口，Rust 优先）
```

- UI 在独立 tokio 任务运行，仅订阅 `SessionEvent` 渲染，**不反向调用核心**。
- 渲染合并：用 `tokio::time::interval` 或事件 coalescing 节流，重解析放 worker 任务，保证流畅。
- UI 本身是能力接缝的 Consumer，故 TUI/GUI 可一键替换。

---

## 11. LLM 集成

```rust
// harness-llm/src/lib.rs
pub trait LlmProvider: Send + Sync {
    fn stream(&self, msgs: Vec<Message>)
        -> impl Stream<Item = Result<Chunk>> + Send;
    fn tools(&self) -> &[ToolSchema];
}

// Provider：OpenAI / DeepSeek / Anthropic（HTTP+SSE）、local（llama.cpp server）、replay（测试）
```

流式输出经 `async_stream` / `mpsc` 推送；工具调用由 schema → 模型 → `ToolCall` 回环（见 §5.6）。

---

## 12. 文件监听与 LSP 传输

- **文件监听**：`notify` crate（inotify / kqueue / FSEvents），OS 级推送，无忙轮询；供 `fs` 能力与热重载。
- **LSP 传输**：`tokio::process` 启动语言服务器，stdin/stdout 走异步 JSON-RPC（`tower-lsp` client 侧或自研轻量帧）。多 LSP 会话并发，结果汇入事件总线。

---

## 13. 分层配置（编译期）

```
base feat (core+runtime+local providers)
  → mode feat (tui / gui / acp)
    → provider feat (wasm-tools / deepseek / local-llm)
      → runtime overlay (TOML：仅用户设置，如 sandbox_mode / approval_policy / api_key)
```

- 行为组合 = `compose(profile)` + Cargo features；**运行期 TOML 只改设置，不改能力装配**。
- 这替代 Cordis 的 `cordis.patch.yml` 运行时 patch 层——代价是改行为需重编，收益是类型安全与启动速度。

---

## 14. 性能预算

| 维度 | 技术手段 | 目标 |
|---|---|---|
| 启动 | 单 tokio 运行时懒建；feature 裁剪 dead-code；LSP/网络懒加载 | < 50ms 到首提示 |
| idle | 事件驱动（`Notify`/condvar），无轮询；有界通道；OS 文件监听 | < 30MB，0% CPU |
| UI | 渲染独立任务 + 事件合并 + 解析入 worker | ≥ 60fps 无卡顿 |
| 工具执行 | `tokio::process` 异步 spawn（无每进程线程）；pre-exec 套沙箱；LSP 异步 | spawn < 5ms 开销 |
| 二进制 | `strip` + `LTO` + `codegen-units=1` + feature gating | 单文件 < 20MB |

---

## 15. 测试与运行时不变量

- 每 crate 单元测 + 事件回放快照测（复用 `SessionLog`）。
- **不变量断言**：
  1. 到达模型的输入必有对应日志事件；
  2. 切换 Provider 后 Consumer 代码零改动；
  3. 丢弃 `ComposeGuard` 后所有注册消失；
  4. `tools/result` 为冻结权威结果，后续不可变。
- CI 门控：`cargo test` + `cargo clippy` + `cargo fmt` + 文档/代码不漂移检查。

---

## 16. 风险与开放问题

| 风险 | 缓解 |
|---|---|
| macOS/Windows 无 seccomp | `Sandbox` trait 平台实现，Consumer 无感；文档标注能力差异 |
| 编译期组合降低灵活性 | 接受取舍；仅用户脚本走 WASM 动态加载 |
| WASM 异步/嵌入成本 | wasmtime 仅用于 workflow 与用户工具，不进热路径 |
| tokio 阻塞调用 | 同步 FS 走 `spawn_blocking` |
| 跨平台文件监听丢事件 | `notify` + 兜底周期校验哈希 |

---

## 17. 里程碑 / 构建顺序

- **M0** 骨架：`AppContext` + `TypeMap` + `EventBus` + RAII `Registration`
- **M1** `SessionLog` + `AgentLoop`（headless，replay LLM）
- **M2** 能力 trait + 本地 Provider（bash/fs/editor）
- **M3** 沙箱（landlock+seccomp，Linux 先行）
- **M4** LLM Provider + 工具调用闭环
- **M5** UI（TUI 默认）
- **M6** 子代理（多任务）+ LSP + 文件监听
- **M7** WASM workflow 沙箱

---

## 18. 附录：dsh → Rust 概念对照

| dsh (TS/Cordis) | Rust 原生 |
|---|---|
| `ctx.<key>` 服务仓库 | `AppContext` + `TypeMap<Arc<dyn Service>>` |
| `ctx.effect()` 可逆副作用 | RAII `Registration`（`Drop` 注销） |
| `cordis.yml` 运行时 patch | 编译期 `compose(profile)` + Cargo features |
| 类型化事件 4 模式 | `EventBus::{emit,parallel,serial,waterfall}` |
| 能力接缝三角色 | `trait` + 多 `impl` + 仅依赖 trait 的 Consumer |
| 追加日志真相源 | `SessionLog`（SQLite/WAL，仅追加） |
| Provider 动态加载 | 本地/远程编译期；不可信代码走 wasmtime |
| waterfall 中间件 | `Waterfall<E>` trait，`(args, next)` around 链 |
| 子代理 `ctx.subagents` | `Scheduler` + `JoinSet` + `CancellationToken` |

---

*设计决策记录（待确认项）：UI 默认 TUI（最像 Codex、开销最低），GUI 以 slint 作为可替换实现；Provider 集首批含 LocalBash/LocalFs/DeepSeek，WASM 与本地 LLM 走 feature 门控。若需 GUI 优先或不同 LLM 默认，调整 §4/§13 即可。*
