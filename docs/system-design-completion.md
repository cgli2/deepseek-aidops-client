# 系统设计完善说明（System Design Completion）

> 本文是 `native-rust-agent-harness-design.md` 的**补全与落地版**。原设计确立了架构思想、分层、抽象与里程碑，但多处仍为伪代码或"待确认项"。本文补齐：
> 1. 已固化的设计决策（原 §18 待确认项）；
> 2. 缺位的**具体类型定义**（错误、事件、消息、工具、配置）；
> 3. `TypeMap` / `EventBus` 的**可落地 Rust 实现方案**（含与文档 `&mut self` 签名的有意偏差说明）；
> 4. 配置 TOML schema 与分层覆盖规则；
> 5. Cargo workspace 工程化（feature 分层、依赖、构建 profile）；
> 6. 模块级文件布局；
> 7. 构建 / 运行 / 测试命令与运行时不变量断言；
8. **dsh 插件机制移植**：可逆副作用（`effect()` 自动回滚）、依赖声明的组合顺序、`Extension` 契约与"功能→扩展点"映射表（cookbook）、以及 WASM 动态加载不可信 Provider（见 §11）。
>
> 读者应先读原设计文档，再读本补全。代码骨架在 `harness/`（见 §7 与 README）。

---

## 1. 已固化的设计决策（原 §18 待确认项）

| 待确认项 | 决策 | 影响范围 |
|---|---|---|
| UI 默认形态 | **默认 TUI**（ratatui + crossterm）；GUI 作为 `feature = "gui"` 下的 slint 可替换实现 | §4 / §10 / §13 |
| 首批 Provider 集 | `LocalBash`、`LocalFs`、`DeepSeek` 为一等公民（默认编入）；`WasmProvider` 与 `LocalLlm` 经 `feature` 门控 | §4 / §13 |
| 本地 LLM 默认 | 默认不编入；`feature = "local-llm"` 启用 `llama.cpp` HTTP server 适配 | §11 / §13 |
| Agent 循环终止语义 | 唯一终止检查点 = `agent/turn-stopping`（serial，无 `next()`）；其余阶段不得自行终止 | §5.6 |
| 会话日志存储 | MVP 用 **redb**（纯 Rust 嵌入式、零依赖、WAL 风格追加）；后续可换 SQLite | §5.5 / §M1 |
| 编译期组合入口 | `compose(profile: Profile) -> AppContext`；`Profile` 为枚举 `{ Tui, Headless, Acp, Gui }` | §5.2 |

> 取舍声明：本实现接受"改行为需重编"的代价，以换取类型安全与启动速度（原 §2 非目标）。**仅用户脚本 / 用户自定义工具**走 WASM 动态加载，不进入热路径。

---

## 2. 核心类型定义（补全原 §5 伪代码）

所有类型集中在 `harness-core`（`error.rs`、`types.rs`、`event.rs`）与 `harness-session`（`log.rs`）。下面给出**权威字段**，骨架须严格对齐。

### 2.1 错误与结果

```rust
// harness-core/src/error.rs
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("service not registered: {0}")]
    ServiceMissing(&'static str),
    #[error("event handler registration conflict")]
    HandlerConflict,
    #[error("tool execution cancelled")]
    Cancelled,
    #[error("sandbox policy denied: {0}")]
    SandboxDenied(String),
    #[error("llm provider error: {0}")]
    Llm(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
```

约定：`harness-*` 各 crate **不自定义** Error 枚举；统一 `use harness_core::error::{Error, Result};`。跨 crate 错误用 `#[from]` 收敛。

### 2.2 会话事件（追加日志真相源）

```rust
// harness-session/src/log.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SessionEvent {
    TurnStart   { id: EventId, input: String },
    StepStart   { id: EventId, turn: EventId },
    PreStep     { id: EventId, msg: Vec<Message> },   // 经 waterfall 后的请求
    Assistant   { id: EventId, chunk: Chunk },
    ToolCall    { id: EventId, call: ToolCall },
    ToolResult  { id: EventId, result: ToolResult },
    TurnStopping{ id: EventId, will_stop: bool },
    TurnEnd     { id: EventId, turn: EventId },
    // 非持久扩展点（实时）不写入日志：如 UI 渲染、指标
}

pub type EventId   = u64;            // 单调自增，全局唯一
pub type SessionId = uuid::Uuid;     // 跨进程稳定标识
```

**运行时不变量（强）**：任何进入 `LlmProvider::stream` 的 `Vec<Message>` 必须存在对应的 `PreStep` 日志事件；模型可见状态只能从 `SessionLog::replay()` 重建（原 §5.5）。CI 用 `cargo test` 中的 `invariant_log_derives_model_input` 断言。

### 2.3 消息与流式分片（LLM 契约）

```rust
// harness-llm/src/lib.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message { pub role: Role, pub content: String, pub tool_calls: Vec<ToolCall> }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role { System, User, Assistant, Tool }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk { pub text: Option<String>, pub tool_calls: Vec<ToolCall> }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall { pub id: String, pub name: String, pub args: serde_json::Value }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema { pub name: String, pub description: String, pub json_schema: serde_json::Value }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult { pub call_id: String, pub ok: bool, pub content: String,
                        pub continuation_debt: usize }   // 见 §2.5
```

### 2.4 用户 / 任务 / 配置

```rust
// harness-core/src/types.rs
#[derive(Debug, Clone)]
pub struct UserInput { pub text: String, pub attachments: Vec<Attachment> }
pub struct Attachment { pub path: PathBuf, pub mime: String }

// harness-runtime/src/task.rs
#[derive(Debug, Clone)]
pub struct Task { pub session: SessionId, pub input: UserInput }

// harness-core/src/config.rs  （见 §5 配置 schema）
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Config {
    pub llm: LlmConfig,
    pub sandbox_mode: SandboxMode,
    pub approval_policy: ApprovalPolicy,
    pub permission_preset: PermissionPreset,
}
```

### 2.5 续跑债务（debt）语义

原 §5.6 的 `debt` 计数在 `ToolResult::continuation_debt` 上承载：
- 普通工具返回 `0` → 不新增 step；
- 需要模型继续（如 `subagent.report` 回传）返回 `>0` → 累加债务，循环续跑；
- 循环终止**只看** `agent/turn-stopping` 的 `will_stop`，不靠 debt 归零决定。

---

## 3. TypeMap 服务仓库（落地实现，补全原 §5.1）

原文档 `provide(&mut self)` 签名在本实现中**有意改为 `&self`**，理由：用 `Arc<AppContextInner>` + 内部可变性（`RwLock`）使 `AppContext: Clone`，便于把 ctx 廉价 move 进 `tokio::spawn` 的任务（原 §7 `spawn_session(ctx)`）。所有权语义不变，所有运行时不变量仍成立。

```rust
// harness-core/src/context.rs
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::{Arc, RwLock, Weak};

pub trait Service: Any + Send + Sync + 'static {}
impl<T: Any + Send + Sync + 'static> Service for T {}

struct Inner {
    services: RwLock<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
    handlers:  RwLock<HandlerTable>,   // 见 §4
}

#[derive(Clone)]
pub struct AppContext { inner: Arc<Inner> }

impl AppContext {
    pub fn new() -> Self {
        Self { inner: Arc::new(Inner {
            services: RwLock::new(HashMap::new()),
            handlers:  RwLock::new(HandlerTable::new()),
        })}
    }
    /// 注册服务，返回 RAII guard；Drop 时自动从 TypeMap 移除（可逆注册，原 §5.3）。
    pub fn provide<S: Service>(&self, s: Arc<S>) -> Registration {
        let tid = TypeId::of::<S>();
        self.inner.services.write().insert(tid, s);
        Registration { inner: Arc::downgrade(&self.inner), tid, _marker: PhantomData }
    }
    /// 取服务；未注册 → panic 在 compose 期被结构化保证（Consumer 必被满足）。
    pub fn get<S: Service>(&self) -> Arc<S> {
        self.inner.services.read().get(&TypeId::of::<S>())
            .and_then(|a| a.clone().downcast::<S>().ok())
            .expect("service must be registered before use")
    }
    pub fn events(&self) -> EventBusView { EventBusView { inner: self.inner.clone() } }
    pub fn handlers(&self) -> &RwLock<HandlerTable> { &self.inner.handlers }
}
```

`Registration` 的 Drop：

```rust
pub struct Registration { inner: Weak<Inner>, tid: TypeId, _marker: PhantomData<()> }
impl Drop for Registration {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade() {
            inner.services.write().remove(&self.tid);   // 回滚服务注册
        }
    }
}
```

> 注：事件订阅的回滚通过 `HandlerTable` 的订阅 id 实现（见 §4）。`Registration` 可同时持有一个"事件退订闭包"集合以覆盖订阅型插件——骨架用 `Registration` 仅管服务，`ComposeGuard` 管事件订阅的批量回滚。

---

## 4. 类型化事件总线（落地实现，补全原 §5.4）

四种分发：`emit`（fire-and-forget，spawn）、`parallel`（await 全部观察者）、`serial`（注册顺序，返回末值）、`waterfall`（around-middleware 链）。采用**类型擦除**存储：按 `Event::TypeId` 分组，处理器存为 `Arc<dyn ErasedHandler>`。

```rust
// harness-core/src/event.rs
pub trait Event: Any + Send + 'static { type Output: Send + 'static; }

pub trait Handler<E: Event>: Send + Sync {
    fn handle(&self, e: E) -> impl Future<Output = ()> + Send;
}
pub trait Waterfall<E: Event>: Send + Sync {
    fn call(&self, args: E, next: &dyn Fn(E) -> E::Output) -> E::Output;
}

// 类型擦除表
pub struct HandlerTable {
    emit:     HashMap<TypeId, Vec<Arc<dyn Any + Send + Sync>>>,
    parallel: HashMap<TypeId, Vec<Arc<dyn Any + Send + Sync>>>,
    serial:   HashMap<TypeId, Vec<Arc<dyn Any + Send + Sync>>>,
    waterfall:HashMap<TypeId, Vec<Arc<dyn Any + Send + Sync>>>,
}
```

`EventBusView`（从 `AppContext::events()` 取）暴露：

```rust
impl EventBusView {
    pub fn on<E: Event>(&self, h: Arc<dyn Handler<E>>) -> Registration;       // 订阅 emit
    pub async fn emit<E: Event>(&self, e: E);                                 // spawn 任务
    pub async fn parallel<E: Event>(&self, e: E) -> Vec<()>;                   // await 所有
    pub async fn serial<E: Event>(&self, e: E) -> E::Output;                   // 顺序末值
    pub async fn waterfall<E: Event>(&self, e: E,
        chain: &[Arc<dyn Waterfall<E>>]) -> E::Output;                         // around 链
}
```

`waterfall` 实现要点：把 `chain` 折叠成闭包 `f`，令 `next = chain[i+1..]` 的调用；末项 `next` 为终止处理器（如真正发起 LLM 请求）。监听器可**不调 `next` 以短路**（原 §5.4）。

---

## 5. 配置 schema 与分层覆盖（补全原 §13）

运行期 TOML **只改设置，不改能力装配**（能力由 `compose(profile)` + Cargo features 决定）。

```toml
# harness/config/default.toml  （base，随二进制内置）
[llm]
provider = "deepseek"          # deepseek | openai | anthropic | local | replay
base_url = "https://api.deepseek.com"
model    = "deepseek-chat"
api_key_env = "DEEPSEEK_API_KEY"   # 不在文件内存密钥

sandbox_mode     = "WorkspaceWrite"   # ReadOnly | WorkspaceWrite | DangerFullAccess
approval_policy  = "Ask"              # Ask | Never | Unavailable
permission_preset = "balanced"        # minimal | balanced | permissive
```

覆盖顺序（高优先级覆盖低）：`builtin default` → `Cargo feature 默认` → `~/.config/harness/config.toml` → `./.harness.toml`（项目） → CLI flag。骨架提供 `Config::load()` 按此顺序 `toml::from_str` 后 `merge`。

---

## 6. 编译期组合（补全原 §5.2）

```rust
// bin/src/main.rs
pub fn compose(profile: Profile) -> AppContext {
    let ctx = AppContext::new();
    match profile {
        Profile::Tui => {
            ctx.provide(Arc::new(LocalBash::new()));
            ctx.provide(Arc::new(LocalFs::new()));
            ctx.provide(Arc::new(DeepSeek::from_config(&CONFIG)));
            ctx.provide(Arc::new(TuiUi::new()));
        }
        Profile::Headless => {
            ctx.provide(Arc::new(LocalBash::new()));
            ctx.provide(Arc::new(ReplayLlm::new()));   // 测试 / CI
        }
        Profile::Acp => { /* harness-acp JSON-RPC server */ }
        Profile::Gui => { /* slint UI，feature="gui" */ }
    }
    ctx
}
```

`ComposeGuard`：因 `AppContext` 用 `Arc<Inner>` 且服务注册为 RAII，本实现中"卸载 = 丢弃持有 `Registration` 的 guard 集合"。骨架提供 `ComposeGuard { regs: Vec<Registration> }`，Drop 时逐个 drop。`compose` 不再返回 guard（ctx 自身管理生命周期）；`Plugin::register` 返回的 `Registration` 由调用方收集进 `ComposeGuard`。**不变量**：丢弃 guard 后 `AppContext::get::<S>()` 必须 fail（测试 `invariant_guard_unload_clears_registry`）。

**插件机制（移植 dsh，详见 §11）**：`compose` 中插件按 `Plugin::deps()` 声明的依赖做**拓扑排序**后再注册，等价于 dsh 的 `inject` 自动推导加载顺序；每个插件通过 `register()` 贡献服务（`ctx.provide`）与事件监听器（`ctx.events().on`），二者均返回 `Registration`——即 dsh 的**可逆副作用 `effect()`**，丢弃即自动回滚。用户态不可信扩展（自定义工具 / workflow）不走编译期 `compose`，而由 `harness-provider-wasm` 的 `WasmPluginLoader` 在运行期加载并注册为 Provider（见 §11.4）。

---

## 7. Cargo workspace 工程化（补全原 §4）

### 7.1 根 `Cargo.toml`（feature 分层）

```toml
[workspace]
resolver = "2"
members = [
  "harness-core", "harness-runtime", "harness-capability",
  "harness-provider-local", "harness-provider-sandbox", "harness-provider-wasm",
  "harness-llm", "harness-tool", "harness-session", "harness-ui",
  "harness-acp", "harness-sdk", "bin",
]

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "MIT"

[workspace.dependencies]
tokio      = { version = "1", features = ["full"] }
tokio-util = { version = "0.7", features = ["rt"] }
async-trait= "0.1"
thiserror  = "1"
serde      = { version = "1", features = ["derive"] }
serde_json = "1"
futures    = "0.3"
uuid       = { version = "1", features = ["v4", "serde"] }
tracing    = "0.1"

# 二进制体积 / 速度
[profile.release]
lto            = true
codegen-units  = 1
strip          = true
opt-level      = 3
```

### 7.2 feature 分层（替代 Cordis patch 层）

```toml
# bin/Cargo.toml
[features]
# 骨架默认仅编 deepseek，便于 CI 快速编译；TUI 是产品默认形态（见 §1），通过 --features tui 启用。
default      = ["deepseek"]
tui          = ["harness-ui/tui"]
gui          = ["harness-ui/gui"]
acp          = ["harness-acp"]
wasm-tools   = ["harness-provider-wasm/wasm-tools"]
local-llm    = ["harness-llm/local"]
deepseek     = ["harness-llm/deepseek"]
```

### 7.3 关键外部依赖（按 crate）

| crate | 依赖 |
|---|---|
| harness-core | tokio, tokio-util, async-trait, thiserror, serde, uuid |
| harness-runtime | harness-core, tokio, futures, async-stream, tracing |
| harness-capability | harness-core |
| harness-provider-local | harness-capability, harness-core, tokio |
| harness-provider-sandbox | harness-core, nix（Linux）, landlock, seccompiler |
| harness-provider-wasm | harness-core, wasmtime（feat） |
| harness-llm | harness-core, reqwest（feat http）, async-stream, serde_json |
| harness-tool | harness-core, harness-capability |
| harness-session | harness-core, redb, serde |
| harness-ui | harness-core, ratatui+crossterm（feat tui） / slint（feat gui） |
| harness-acp | harness-core, tokio, serde_json（JSON-RPC） |
| harness-sdk | tokio, serde_json |

---

## 8. 运行时不变量与测试清单（补全原 §15）

每 crate 单元测 + 事件回放快照测。骨架须落地的断言（`#[test]`）：

1. `invariant_log_derives_model_input`：构造一段 replay，断言 `LlmProvider::stream` 的输入可由 `SessionLog::replay()` 重建。
2. `invariant_provider_swap_zero_change`：用 `ReplayLlm` 替换 `DeepSeek`，`BashTool` 源码不变仍可运行（通过 trait 对象注入验证）。
3. `invariant_guard_unload_clears_registry`：`ComposeGuard` drop 后 `ctx.get::<S>()` panic。
4. `invariant_tool_result_frozen`：`tools/result` 事件写入后，下游无法再修改（用 `Arc<Mutex<...>>` 冻结 + 测试二次写入失败）。
5. `invariant_plugin_effect_reversible`：插件注册后 `ctx.get::<S>()` 可用；其全部 `Registration` drop 后该服务与所有事件订阅消失（等价 dsh `effect()` 回滚，由 `invariant_guard_unload_clears_registry` 覆盖）。

CI 门控：`cargo fmt --check && cargo clippy -- -D warnings && cargo test`。

---

## 9. 模块级文件布局（骨架落点）

```
harness/
  Cargo.toml                      # workspace + [workspace.dependencies] + [profile.release]
  config/default.toml
  bin/
    Cargo.toml
    src/main.rs                   # parse args → Profile → compose() → run()
    src/compose.rs                # compose(profile) 编译期装配
  harness-core/
    Cargo.toml
    src/lib.rs  context.rs  event.rs  error.rs  types.rs  plugin.rs  config.rs  extension.rs
  harness-runtime/
    Cargo.toml
    src/lib.rs  agent_loop.rs  scheduler.rs  task.rs
  harness-capability/
    Cargo.toml
    src/lib.rs  shell.rs  fs.rs  editor.rs  lsp.rs  subagent.rs  compaction.rs
  harness-provider-local/
    Cargo.toml
    src/lib.rs  bash.rs  fs.rs  editor.rs
  harness-provider-sandbox/
    Cargo.toml
    src/lib.rs  landlock_seccomp.rs  app_sandbox.rs  job_object.rs
  harness-provider-wasm/
    Cargo.toml                    # feature="wasm-tools"
    src/lib.rs  loader.rs         # WasmPluginLoader：运行期动态注册不可信 Provider（§11.4）
  extensions/
    EXTENSION-COOKBOOK.md         # 功能→扩展点映射表（dsh extension cookbook 移植，§11.3）
  harness-llm/
    Cargo.toml
    src/lib.rs  openai.rs  deepseek.rs  anthropic.rs  local.rs  replay.rs
  harness-tool/
    Cargo.toml
    src/lib.rs  bash.rs  fs.rs  edit.rs
  harness-session/
    Cargo.toml
    src/lib.rs  log.rs  project.rs  telemetry.rs
  harness-ui/
    Cargo.toml                    # features: tui / gui
    src/lib.rs  tui.rs  gui.rs
  harness-acp/
    Cargo.toml
    src/lib.rs  server.rs
  harness-sdk/
    Cargo.toml
    src/lib.rs  client.rs
```

---

## 10. 构建 / 运行 / 验证（补全原 §14/§17）

```bash
# 默认 TUI（deepseek + local providers）
cargo run -p harness-bin -- --profile tui

# headless（replay，CI / 测试）
cargo run -p harness-bin -- --profile headless --replay ./fixtures/turn-01.jsonl

# ACP stdio 服务器
cargo run -p harness-bin --features acp -- --profile acp

# WASM 用户工具 + 本地 LLM（feature 门控）
cargo run -p harness-bin --features "wasm-tools,local-llm" -- --profile tui

# 验证
cargo fmt --check && cargo clippy -- -D warnings && cargo test

# 体积 / 启动预算检查（release）
cargo build --release && ls -lh target/release/harness-bin
```

性能预算（见原 §14）作为 CI 注释门控：release 二进制 < 20MB（feature 裁剪 + LTO + strip），冷启动 < 50ms，idle < 30MB。

---

## 11. 插件机制（dsh 移植要点）

dsh 的"一切皆插件"由五条可验证纪律支撑（见 `deepseek-harness-architecture-analysis.md` §一/§四）。本实现逐条落地如下。

### 11.1 插件即可逆副作用（`effect()`）

dsh 的 `ctx.effect()` 返回 disposer，卸载时自动回滚。本实现中**服务注册**与**事件订阅**都返回同一个 `Registration`（RAII）：`AppContext::provide` 与 `EventBusView::on` 各返回一个 `Registration`，`Registration` Drop 时从 `TypeMap` / `HandlerTable` 移除对应条目。因此"插件 = 一组 `Registration`"，卸载插件 = 丢弃其 `Registration` 集合，不留残影。这正是 dsh 可逆副作用的 Rust 等价物。

```rust
// harness-core/src/plugin.rs
pub trait Plugin: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    /// 声明依赖的其它插件名；compose 据此拓扑排序（等价 dsh inject 推导加载顺序）。
    fn deps(&self) -> &[&'static str] { &[] }
    /// 向 ctx 贡献服务与事件监听器；每个贡献都是可逆副作用，返回 Registration。
    fn register(self: Arc<Self>, ctx: &AppContext) -> Vec<Registration>;
}
```

### 11.2 依赖声明的组合顺序（拓扑排序 = `inject`）

dsh 用 `inject` 声明依赖、自动推导加载顺序。本实现用 `Plugin::deps()` 显式声明，由 `compose_plugins` 做拓扑排序后按序 `register`，保证被依赖的服务先就位（结构性保证 Consumer 必被满足）。

```rust
// harness-core/src/plugin.rs
pub fn compose_plugins(plugins: Vec<Arc<dyn Plugin>>) -> (AppContext, ComposeGuard) {
    let ordered = topo_sort(&plugins);                 // 按 deps() 拓扑序
    let ctx = AppContext::new();
    let mut guard = ComposeGuard::new();
    for p in ordered {
        for r in p.register(&ctx) { guard.add(r); }     // 每个 effect 收集进 guard
    }
    (ctx, guard)
}
```

> `bin/src/compose.rs` 的 `compose(profile)` 是上述 `compose_plugins` 的特化：按 profile 选插件集合后调用。

### 11.3 Extension 契约与"功能→扩展点"映射表（cookbook）

dsh 的 extension cookbook 是一张"功能→机制映射表"，**每个产品功能都映射到某个文档化扩展点上的监听器，没有一行代码修改循环本身**。本实现用两样东西落地：

1. **`ExtensionPoint` 枚举**：把 40+ 能力接缝（capability seam）与关键生命周期事件点显式列为扩展点，作为编译期可检查的"扩展点清单"。
2. **`ExtensionRegistry`（运行时）**：插件在 `register()` 时声明"我服务于哪个 `ExtensionPoint`"，用于审计与 cookbook 校验；`extensions/EXTENSION-COOKBOOK.md` 给出功能→扩展点的人工映射表（CI 占位校验文档不漂移）。

```rust
// harness-core/src/extension.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionPoint {
    Llm, Shell, Fs, Editor, Lsp, Subagent, Compaction,
    PreStep, TurnStopping, ToolPreExecute, ToolExecute, ToolPostExecute, ToolResult,
}
pub struct ExtensionRegistry {
    served: std::sync::RwLock<std::collections::HashMap<ExtensionPoint, Vec<&'static str>>>,
}
impl ExtensionRegistry {
    pub fn declare(&self, point: ExtensionPoint, plugin: &'static str);
    pub fn served_by(&self, point: ExtensionPoint) -> Vec<&'static str>;
}
```

判定标准（与原设计 §6 一致）：换一个 Provider（如 `LocalBash` → `WasmShell`），Consumer（`BashTool`）源码零改动——因为 Consumer 只依赖 `ExtensionPoint::Shell` 对应的 `Shell` trait，不依赖具体 Provider。

### 11.4 动态插件：WASM 加载不可信 Provider

dsh 的 Provider 可动态加载；本设计仅在"不可信用户脚本 / 用户自定义工具"上保留动态加载，且**只走 WASM**（默认无 FS/网络，仅暴露显式 host 函数），不进热路径（原 §16 风险缓解）。`harness-provider-wasm` 提供 `WasmPluginLoader`：

```rust
// harness-provider-wasm/src/loader.rs
pub struct WasmPluginLoader { engine: Option<Engine> }   // wasmtime，feature="wasm-tools"
impl WasmPluginLoader {
    /// 运行期加载 .wasm 模块并注册为 Provider；返回 Registration（卸载即退出的可逆副作用）。
    pub fn load(&self, bytes: &[u8], ctx: &AppContext) -> Result<Registration> {
        // 1) 实例化 wasm 模块（默认无 FS/网络 host 函数）
        // 2) 调用模块导出的 `register(ctx)` 回调 → 注册为 Provider
        // 3) 返回 Registration（drop = 卸载，等价于 ctx.effect() 回滚）
        todo!("M7：接入 wasmtime，host 函数仅暴露沙箱化 FS/网络")
    }
}
```

> 编译期 `compose` 与运行期 `WasmPluginLoader` 都产出 `Registration`，故二者对 ctx 而言**完全同构**——这是"一切皆插件"在 Rust 中的统一收口。

### 11.5 包级约定（"约定即代码"门控，移植 dsh §四·借鉴七）

每个 crate 维护两份约定文档：`INVARIANTS.md`（运行时不变量）与 `MODEL-EXPERIENCE.md`（模型体验契约）。CI 门控：

```bash
cargo fmt --check            # 格式
cargo clippy -D warnings     # lint 零警告
cargo test                   # 不变量断言
cargo doc --no-deps          # 文档生成（doc-sync 占位：文档与代码不漂移）
# 文档字数预算（占位脚本）：architecture ≤ 1800 词，AGENTS ≤ 1600 词
```

---

## 12. 与原设计的偏差清单（透明记录）

| 位置 | 原设计 | 本补全 | 理由 |
|---|---|---|---|
| §5.1 `provide` | `&mut self` | `&self`（内部 `Arc<RwLock>`） | 使 `AppContext: Clone`，可 move 进 `tokio::spawn` |
| §5.3 `Registration` | `Weak<()>` 占位 | `Weak<Inner>` + `TypeId` 真实回滚 | 落地可行性 |
| §5.2 `compose` 返回 | `(ctx, ComposeGuard)` | `(AppContext, ComposeGuard)`；guard 即插件集合生命周期 | guard drop = 卸载全部插件（可逆注册自动回滚，不变量 3/5） |
| §5.5 存储 | SQLite/redb/LMDB | 锁定 **redb** | 纯 Rust、零 C 依赖、追加友好 |
| §9 沙箱 | 三平台并列 | Linux landlock+seccomp 先行，mac/win 留 trait 占位 | 平台能力差异（原 §16 风险） |
| §13 features | 概念 | 落地为 `default=["deepseek"]`；`tui`/`gui`/`acp`/`wasm-tools`/`local-llm` 为可选 feature | 工程化；TUI 为产品默认形态但骨架默认 headless（Profile）便于 CI |
| §11 插件机制 `Ui` | — | `Ui: Any + Send + Sync + 'static` | 使 `Arc<dyn Ui>` 满足 `Service`，UI 也可作为服务注册（与 `Arc<dyn Shell>` 同构） |
| §5.1 TypeMap 取值 | `Arc<dyn Any>` 直取 trait object | `ServiceCell<X>` 包裹（Sized 单元） | `Any::downcast` 只接受 Sized 目标；trait object 服务需经 Sized 单元存/取（不变量 2 落地关键） |
| §13 记忆/钩子/Git | — | `Memory`/`Hook`/`Git` trait + `FileMemory`/`ShellHook`/`GitCli` Provider + `ExtensionPoint::{Memory,Hook,Git}` | 借鉴 Codex 系统级能力，复用既有能力接缝（Definition/Provider/Consumer） |
| §13.4 钩子配置 | — | `Config.hooks: HashMap<String,String>`（`[hooks]` 表） | TOML 表驱动钩子，零代码改动注入外部命令 |
| §13.6 worktree | — | `WorktreeGuard`（Drop 自动移除） | 可逆副作用，呼应 dsh `effect()` 回滚：进入即承诺，离开即清理 |

---

## 13. 借鉴 Codex 的核心能力（记忆 · 钩子 · Git · Worktree）

### 13.1 动机

Codex CLI 的生产力来自四个"系统级"能力，它们与本项目"能力接缝 / 一切皆插件"哲学
**同构**，可直接挂在既有骨架上，无需改动循环本身：

- **记忆（Memory）**：跨会话持久化学到的偏好与项目约定。
- **钩子（Hooks）**：在生命周期点注入用户命令，可阻断 / 审计。
- **Git 集成**：代理读仓库状态、生成 diff、提交。
- **Worktree**：每个任务在隔离工作副本中并行，互不污染。

### 13.2 映射表（功能 → 扩展点 → Provider → Consumer）

| 能力 | Definition（`harness-capability`） | Provider | 扩展点 | Consumer |
|---|---|---|---|---|
| 记忆 | `memory::Memory` | `harness-provider-memory::FileMemory` | `ExtensionPoint::Memory` | 工具 / 循环（可暴露为写记忆工具） |
| 钩子 | `hook::Hook` | `harness-provider-hook::{ShellHook, NullHook}` | `ExtensionPoint::Hook` | `harness-runtime::agent_loop`（PreToolUse / PostToolUse） |
| Git | `git::Git` | `harness-provider-git::GitCli` | `ExtensionPoint::Git` | 调度器 / 工具（仓库状态上下文） |
| Worktree | `git::Git::create_worktree` / `remove_worktree` + `WorktreeGuard` | 同上 | （归 `Git` 扩展点） | 调度器（每会话隔离副本） |

**判定标准（不变）**：把 `FileMemory` 换成向量库 Provider、`ShellHook` 换成 WASM 钩子，
循环与 Consumer 零改动。

### 13.3 记忆机制

- `MemoryEntry { scope, key, value, updated_at }`，按 `MemoryScope::{Project, User, Session}` 隔离。
- `FileMemory`：`<root>/.harness-memory/<scope>/<key>` 一文件一条，写即落盘 + 内存索引；
  `search` 为朴素子串（可换 fts / 向量索引）。
- 与 `SessionLog` **正交**：`SessionLog` 是 *单会话、只追加* 的运行时真相源；
  `Memory` 是 *跨会话、可检索* 的持久层（类比 Codex 的 `memory/` 文件）。
- 后续：可暴露 `write_memory` / `search_memory` 工具给 LLM（Consumer 仅依赖 `dyn Memory`）。

### 13.4 钩子

- `HookEvent::{SessionStart, PreTurn, PreToolUse, PostToolUse, PostTurn, SessionEnd}`，覆盖工具管线与循环。
- `ShellHook`：`Config.hooks["<event>"] = "<command>"`；把 `HookPayload`（JSON）经 stdin 传给命令，
  命令回 `{"decision":"allow"|"block","reason":"..."}`。命令执行失败 → 默认 **阻断**（fail-closed）。
- `NullHook`：未配置时全放行，保证循环不中断。
- 集成点（`agent_loop::run_turn`）：`PreToolUse` 在 `tools.dispatch` 前；返回 `Block` 则跳过该工具并
  记录 `[blocked by hook]` 结果；`PostToolUse` 在分发后（审计 / 后处理挂钩点）。

### 13.5 Git 集成

- `GitCli`（零 C 绑定，全部 `git -C <repo>` 子进程）：`status` / `diff` / `commit` / `current_branch`。
- `commit` 返回 sha；`status` 含 `branch` / `dirty` / `ahead` / `behind`（基于 `@{upstream}...HEAD`）。

### 13.6 Worktree（隔离并行）

- `Git::create_worktree(name, base)` / `remove_worktree(wt)`。
- `WorktreeGuard`（RAII）：构造时建 worktree，`Drop` 自动移除——呼应 dsh `effect()` 回滚：
  **进入即承诺，离开即清理**，即使中途 panic 也不留孤儿 worktree。
- 调度器可对每个 `Task` 创建 `WorktreeGuard`，把 `wt.path()` 作为该会话 Shell / Fs Provider 的 cwd，
  实现多任务并行互不污染。**当前骨架已落地能力 + Provider + 守卫，调度器接入为 M-扩展（TODO）。**

### 13.7 不变量补充

- **不变量 9**：钩子默认 fail-closed——命令异常即阻断，安全优先于便利。
- **不变量 10**：Worktree 由 `WorktreeGuard` 保证在作用域结束（含 panic）时移除。
- **不变量 11**：记忆 / 钩子 / Git 均为能力接缝，替换 Provider 不改 Consumer 与循环。

---

*本文档与 `native-rust-agent-harness-design.md`、`deepseek-harness-architecture-analysis.md` 共同构成设计基线。代码骨架见 `harness/`，构建说明见根目录 `README.md`。*
