# WASM 沙箱工作环境设计规划

> **目标**：在 dsh harness 现有微内核 + "一切皆插件" 架构上，实现一个可用、完整、可按需创建的 WASM 沙箱工作环境，让 harness 能方便地接入沙箱、根据实际需要创建隔离工作环境。
>
> **核心约束**：不破坏已有功能（现有 WasmPluginLoader / WasmPluginRuntime / 平台沙箱 / LocalBash / LocalFs / LocalEditor / Agent Loop / 工具管线全部保持源码兼容）。

---

## 1. 现状分析

### 1.1 已有的两套隔离机制

| 机制 | crate | 作用层 | 当前能力 | 局限 |
|------|-------|--------|---------|------|
| **平台沙箱** | harness-provider-sandbox | OS 进程级 | Windows JobObject（进程树回收）、Linux landlock+seccomp（FS 限制 + ptrace 拦截） | 仅作用于 LocalBash spawn 的子进程；无独立工作环境概念；无资源配额 |
| **WASM 插件** | harness-provider-wasm | wasmtime 线性内存 | 加载 .wasm/.wat、host_log + shell_run 桥接、on_load/on_unload 生命周期 | 仅插件级（activate/deactivate）；无独立 FS / 工作目录；无资源限制；无环境管理 |

### 1.2 现有架构的关键不变量（必须遵守）

1. **会话日志是真相源** — 沙箱环境不能引入新的状态真相源。
2. **Consumer 永不直接依赖 Provider** — BashTool/FsTool/EditTool 只依赖 Arc<dyn Shell>/Arc<dyn Fs>/Arc<dyn Editor>，换 Provider 零改动。
3. **UI 是事件总线纯消费者** — 沙箱环境管理不反向调用核心循环。
4. **Plugin 拓扑组合 + 可逆注册** — 新能力经 Plugin::register 贡献，Registration Drop 即回滚。

### 1.3 现有接缝可复用点

- `harness-capability` 的 `Shell`/`Fs`/`Editor` trait — WasmShell/WasmFs/WasmEditor 可直接实现，Consumer 零改动
- `harness-provider-sandbox` 的 `Sandbox` trait — 可作为 WasmSandboxEnv 底层隔离增强层
- `harness-provider-wasm` 的 `WasmPluginLoader` — 已支持 .wasm/.wat，可复用 Engine/Linker/HostState/ShellBridge 模式
- `harness-core` 的 `Plugin`/`AppContext`/`Registration` 机制 — 新能力经标准插件注册接入

---

## 2. 总体架构

### 2.1 分层设计（不新增 crate，扩展现有 crate）

```
bin/compose.rs (组装根)
  └─ SandboxEnvPlugin (新 Plugin)
       → ctx.provide::<dyn SandboxEnv>(WasmSandboxEnv::new())
       → ctx.provide::<dyn Shell>(WasmShell::new(env))     [可选]
       → ctx.provide::<dyn Fs>(WasmFs::new(env))           [可选]
       → ctx.provide::<dyn Editor>(WasmEditor::new(env))   [可选]

harness-capability (Definition — 新增 trait)
  └─ sandbox_env.rs (新)
       ├─ trait SandboxEnv  — 创建/销毁/列表/快照/恢复
       ├─ struct EnvConfig   — 资源配额、能力授权、挂载点
       ├─ struct EnvHandle   — 环境句柄（id, root, status）
       └─ struct EnvSnapshot — 快照元数据

harness-provider-wasm (Provider — 扩展实现)
  ├─ loader.rs (现有，不改)
  └─ env.rs (新)
       ├─ WasmSandboxEnv — impl SandboxEnv，管理多个 WasmSandbox
       ├─ WasmSandbox — 单个沙箱环境实例（独立 Store + VFS + 配额）
       ├─ WasmShell — impl Shell（委托给指定沙箱）
       ├─ WasmFs — impl Fs（委托给指定沙箱的虚拟 FS）
       └─ WasmEditor — impl Editor（委托给指定沙箱）

harness-provider-sandbox (现有，不改动)
  └─ Sandbox trait — 可被 WasmSandboxEnv 作为额外隔离层复用
```

### 2.2 核心设计决策

| 决策 | 选择 | 理由 |
|------|------|------|
| 沙箱运行时 | wasmtime（复用现有依赖） | 已有 WasmPluginLoader 用 wasmtime 21，不引入新依赖 |
| 输入格式 | .wasm + .wat（wasmtime 原生支持两者） | Module::new 已能解析 WAT 文本，无需额外工具链 |
| 虚拟 FS | 内存 overlay 模式 | 默认零宿主 FS 访问；按授权挂载宿主目录为只读/读写层 |
| 资源限制 | wasmtime fuel + 内存上限 + 超时 | wasmtime 原生支持，无需 OS 级 cgroup |
| 能力授权 | 显式 capability bridge（沿用现有 ShellBridge 模式） | 保持"WASM 侧零直接能力"不变量 |
| 环境隔离 | 每 Sandbox 独立 Store + 独立 Linker | wasmtime Store 天然隔离线性内存与状态 |
| 环境标识 | EnvHandle.id (UUID) + 可选 name | 支持按名/按 ID 查找 |
| 与现有插件关系 | WasmPluginRuntime 不变，WasmSandboxEnv 是平行新能力 | 不破坏已有插件管理功能 |

---

## 3. 详细设计

### 3.1 新增 Definition：harness-capability/src/sandbox_env.rs

新增纯 trait 定义文件，声明沙箱环境管理接口。关键类型：

- **EnvQuota** — 资源配额：fuel（指令预算）、max_memory_bytes、timeout_ms、max_envs
- **EnvCapabilities** — 能力授权：shell/fs/editor/network（默认全 false，零直接能力不变量）
- **MountSpec** — 宿主目录挂载映射：virtual_path → host_path + ro/rw 模式
- **EnvConfig** — 创建环境的完整配置：name + quota + capabilities + mounts + env_vars + init_module
- **EnvHandle** — 环境句柄：id(UUID) + name + root + status
- **EnvStatus** — 状态枚举：Created/Running/Paused/Destroyed
- **EnvSnapshot** — 快照元数据：id + env_id + created_at + label
- **EnvStats** — 资源使用统计：fuel_consumed/remaining + memory_used/limit + uptime + operations
- **trait SandboxEnv** — 核心接口：create/destroy/list/status/snapshot/restore/snapshots/exec/stats

trait 签名：

```rust
#[async_trait]
pub trait SandboxEnv: Any + Send + Sync + 'static {
    async fn create(&self, config: EnvConfig) -> Result<EnvHandle>;
    async fn destroy(&self, id: &str) -> Result<()>;
    async fn list(&self) -> Result<Vec<EnvHandle>>;
    async fn status(&self, id: &str) -> Result<EnvStatus>;
    async fn snapshot(&self, id: &str, label: Option<String>) -> Result<EnvSnapshot>;
    async fn restore(&self, snapshot_id: &str) -> Result<EnvHandle>;
    async fn snapshots(&self, env_id: &str) -> Result<Vec<EnvSnapshot>>;
    async fn exec(&self, env_id: &str, code: &str) -> Result<String>;
    async fn stats(&self, env_id: &str) -> Result<EnvStats>;
}
```

### 3.2 新增 Provider 实现：harness-provider-wasm/src/env.rs

核心结构：

- **WasmSandboxEnv** — 环境管理器，持有 wasmtime Engine + envs HashMap + snapshots HashMap
- **WasmSandbox** — 单个沙箱实例：独立 VFS(HashMap<String, Vec<u8>>) + mounts + config + linker_template
- **EnvHostState** — 沙箱内 host 侧状态：vfs + guest_log + shell_bridge + quota
- **WasmShell** — impl Shell，委托给指定沙箱的 shell bridge
- **WasmFs** — impl Fs，委托给指定沙箱的虚拟 FS
- **WasmEditor** — impl Editor，委托给指定沙箱的虚拟 FS

关键实现逻辑：

1. **create(config)** — 检查 max_envs 上限 → 生成 UUID → build_linker(按 capabilities) → 创建 WasmSandbox → 若有 init_module 则加载执行 on_load → 返回 EnvHandle
2. **exec(env_id, code)** — Module::new(engine, code) 编译 WAT/WASM → Store::new + set_fuel → linker.instantiate → 调用 main/_start/run → 返回 guest 日志
3. **snapshot(env_id)** — 克隆当前 VFS → 保存 config + VFS → 返回 EnvSnapshot
4. **restore(snapshot_id)** — 读取快照 → 用快照 config 创建新环境 → 恢复 VFS → 返回新 EnvHandle
5. **destroy(env_id)** — 从 HashMap 移除 → Arc Drop → Store/Instance/VFS 自动释放

### 3.3 Host 导入表扩展（build_linker）

根据 EnvCapabilities 决定暴露哪些 host 函数：

- `env.host_log` — 始终可用（日志不构成安全风险）
- `env.shell_run` — 仅当 capabilities.shell == true 时暴露（复用现有 loader.rs 的 ShellBridge 模式）
- `env.fs_read` / `env.fs_write` — 仅当 capabilities.fs == true 时暴露（操作虚拟 FS）
- `env.fs_list` — 仅当 capabilities.fs == true 时暴露

未授权的导入函数不在 Linker 中注册 → guest 模块引用未导入函数时实例化失败 → 零直接能力不变量保持。

### 3.4 新增 Plugin：SandboxEnvPlugin（bin/src/compose.rs）

```rust
pub struct SandboxEnvPlugin {
    pub config: SandboxEnvConfig,
}

impl Plugin for SandboxEnvPlugin {
    fn name(&self) -> &'static str { "sandbox-env" }
    fn deps(&self) -> &[&'static str] { &["harness-core"] }
    fn register(self: Arc<Self>, ctx: &AppContext) -> Vec<Registration> {
        if !self.config.enabled { return vec![]; }  // 默认关闭，零影响
        let env: Arc<dyn SandboxEnv> = WasmSandboxEnv::new();
        regs.push(ctx.provide(env.clone()));
        // 声明扩展点
        // ...
        regs
    }
}
```

### 3.5 新增 ExtensionPoint

在 harness-core/src/extension.rs 的 ExtensionPoint 枚举追加 `SandboxEnv`。

### 3.6 新增 Error 变体

在 harness-core/src/error.rs 的 Error 枚举追加 `SandboxEnv(String)`。

### 3.7 配置扩展（config/default.toml）

```toml
[sandbox_env]
enabled = false                          # 默认关闭，不影响现有行为

[sandbox_env.default_quota]
fuel = 1000000000                        # ~10 亿条指令
max_memory_bytes = 67108864              # 64 MiB
timeout_ms = 30000
max_envs = 16

[sandbox_env.default_capabilities]
shell = false                            # 默认零直接能力
fs = false
editor = false
network = false
```

---

## 4. 沙箱工作环境管理流程

### 4.1 创建环境

```
用户/Agent → SandboxEnv::create(EnvConfig)
  → WasmSandboxEnv::create()
    → 检查 max_envs 上限
    → 生成 UUID
    → 构建 Linker（按 capabilities 决定 host 导入函数）
    → 创建 WasmSandbox 实例（独立 Store + VFS + 配额）
    → 若有 init_module：加载并执行 on_load
    → 返回 EnvHandle
```

### 4.2 在环境中执行

```
Agent → SandboxEnv::exec(env_id, wat_code)
  → WasmSandbox::exec_code()
    → Module::new(engine, code)  // 编译 WAT/WASM
    → Store::new(engine, EnvHostState)  // 独立 Store
    → store.set_fuel(quota.fuel)  // 资源限制
    → linker.instantiate(store, module)
    → 调用 main/_start/run
    → 返回 guest 日志

或：Agent → BashTool → WasmShell::run()
  → 在指定沙箱环境中经 shell_run host bridge 执行
```

### 4.3 快照与恢复

```
SandboxEnv::snapshot(env_id)
  → 克隆当前 VFS 内容
  → 保存 config + VFS 快照
  → 返回 EnvSnapshot

SandboxEnv::restore(snapshot_id)
  → 读取快照
  → 用快照 config 创建新环境
  → 恢复 VFS 内容
  → 返回新 EnvHandle
```

### 4.4 销毁

```
SandboxEnv::destroy(env_id)
  → 从 envs HashMap 移除
  → Arc<WasmSandbox> Drop → Store/Instance 自动释放
  → 虚拟 FS 内存自动回收
```

---

## 5. 与现有功能的兼容性保证

### 5.1 不变量保持矩阵

| 不变量 | 现有实现 | 新增实现 | 保证 |
|--------|---------|---------|------|
| WASM 侧零直接能力 | WasmPluginLoader 默认无 shell/fs | WasmSandboxEnv 默认 capabilities 全 false | 一致 |
| Consumer 不依赖 Provider | BashTool → Arc<dyn Shell> | WasmShell impl Shell | 零改动 |
| 会话日志是真相源 | SessionLog 唯一真相源 | 沙箱环境不写 SessionLog | 不引入新真相源 |
| UI 是事件总线消费者 | UI 只订阅 SessionLog | 沙箱管理经 Arc<dyn SandboxEnv> 服务 | 不反向调用 |
| 可逆注册 | Registration Drop 回滚 | SandboxEnvPlugin 返回 Registration | 一致 |

### 5.2 现有代码影响清单

| 文件 | 改动类型 | 影响 |
|------|---------|------|
| harness-capability/src/sandbox_env.rs | **新增** | 新文件，不改现有 trait |
| harness-capability/src/lib.rs | 追加 1 行 | `pub mod sandbox_env;` |
| harness-core/src/extension.rs | 追加 1 枚举值 | `SandboxEnv` |
| harness-core/src/error.rs | 追加 1 变体 | `SandboxEnv(String)` |
| harness-provider-wasm/src/env.rs | **新增** | 新文件 |
| harness-provider-wasm/src/lib.rs | 追加导出 | `pub mod env;` + feature gate |
| harness-provider-wasm/Cargo.toml | 追加依赖 | uuid, chrono（feature-gated） |
| bin/src/compose.rs | 追加 Plugin | SandboxEnvPlugin（默认 disabled） |
| config/default.toml | 追加配置段 | `[sandbox_env]`（默认 enabled = false） |
| **现有所有文件** | **不改** | loader.rs / LocalBash / LocalFs / LocalEditor / AgentLoop / ToolRegistry / SessionController 全部不变 |

### 5.3 Feature Gate 策略

```toml
# harness-provider-wasm/Cargo.toml
[features]
wasm-tools = ["dep:wasmtime", "dep:tokio"]           # 现有
sandbox-env = ["wasm-tools", "dep:uuid", "dep:chrono"]  # 新增，依赖 wasm-tools
```

- `--all-features` 构建（打包脚本已用）自动包含 sandbox-env。
- 不开启 sandbox-env 时，WasmSandboxEnv 不编译，零影响。
- bin/Cargo.toml 的 sandbox-env feature 控制是否注册 SandboxEnvPlugin。

---

## 6. 演进路线

### Phase 1（MVP — 本设计覆盖范围）

- [x] SandboxEnv Definition trait
- [x] WasmSandboxEnv 基础实现（create/destroy/list/exec/snapshot/restore）
- [x] WasmShell / WasmFs / WasmEditor capability bridge
- [x] 虚拟 FS（内存 HashMap overlay）
- [x] wasmtime fuel + 超时限制
- [x] 配置驱动（[sandbox_env]，默认关闭）
- [x] SandboxEnvPlugin 组合接入

### Phase 2（增强）

- [ ] 虚拟 FS 增强：目录树、权限检查、文件锁
- [ ] 挂载点 overlay：只读宿主目录映射 + 可写 overlay 层
- [ ] 环境模板：预置环境配置（Python/Node/Rust 工具链模拟）
- [ ] 网络隔离：network capability bridge（HTTP 代理白名单）
- [ ] 环境间通信：共享内存 / 消息传递
- [ ] GUI 面板：沙箱环境管理 UI（创建/列表/快照/恢复）

### Phase 3（高级）

- [ ] wasmtime component model：用组件模型替代手写 ABI
- [ ] 环境迁移：序列化整个 Store 状态（依赖 wasmtime 持久化能力）
- [ ] 分布式沙箱：跨进程 / 跨机器沙箱环境
- [ ] 沙箱镜像市场：预构建环境模板分发
- [ ] 与平台沙箱叠加：WASM 沙箱 + JobObject/landlock 双层隔离

---

## 7. 测试策略

### 7.1 单元测试（harness-provider-wasm/src/env.rs）

- `create_and_destroy_env` — 创建环境后状态为 Running，销毁后为 Destroyed
- `exec_wat_code` — 在沙箱中执行 WAT 代码，验证 host_log 输出
- `snapshot_and_restore` — 快照后销毁原环境，从快照恢复，验证 VFS 内容
- `fuel_limit_enforced` — 设置极小 fuel，执行死循环 WAT，验证因 fuel 耗尽而失败
- `wasm_fs_read_write` — 通过 WasmFs 写入/读取虚拟 FS 文件
- `zero_capabilities_by_default` — 默认不授权 shell，引用 shell_run 的 WAT 实例化失败

### 7.2 集成测试

- SandboxEnvPlugin 默认 disabled 时不影响现有 Agent Loop
- 开启 sandbox-env feature 后，Agent 可通过 SandboxEnv 创建环境并执行代码
- 验证 WasmShell 替换 LocalBash 后 BashTool 行为一致

---

## 8. 实施步骤（建议顺序）

| 步骤 | 内容 | 涉及 crate | 风险 |
|------|------|-----------|------|
| 1 | 新增 harness-core/src/error.rs 的 SandboxEnv 变体 | harness-core | 无（追加枚举值） |
| 2 | 新增 harness-core/src/extension.rs 的 SandboxEnv 扩展点 | harness-core | 无（追加枚举值） |
| 3 | 新增 harness-capability/src/sandbox_env.rs Definition | harness-capability | 无（新文件） |
| 4 | 在 harness-capability/src/lib.rs 追加 pub mod sandbox_env; | harness-capability | 无（追加 1 行） |
| 5 | 新增 harness-provider-wasm/src/env.rs Provider 实现 | harness-provider-wasm | 中（核心实现） |
| 6 | 在 harness-provider-wasm/src/lib.rs 追加 pub mod env; | harness-provider-wasm | 无 |
| 7 | 更新 harness-provider-wasm/Cargo.toml 追加依赖 + feature | harness-provider-wasm | 低 |
| 8 | 在 bin/src/compose.rs 追加 SandboxEnvPlugin | bin | 低（默认 disabled） |
| 9 | 在 config/default.toml 追加 [sandbox_env] 段 | config | 无 |
| 10 | 编写单元测试 + 集成测试 | harness-provider-wasm | 中 |
| 11 | cargo check --all-features 验证编译 | 全 workspace | — |
| 12 | cargo test -p harness-provider-wasm --all-features | harness-provider-wasm | — |

---

## 9. 关键设计总结

1. **不破坏已有功能**：所有新增代码都是新文件或追加导出，现有 loader.rs / LocalBash / LocalFs / AgentLoop / ToolRegistry / SessionController 一行不改。
2. **默认关闭**：[sandbox_env] enabled = false，不开启时零影响。
3. **遵守三条不变量**：会话日志真相源、Consumer 不依赖 Provider、UI 纯消费。
4. **复用现有 wasmtime 依赖**：不引入新运行时，Engine/Module/Linker/Store 全部复用。
5. **WAT + WASM 双格式支持**：wasmtime Module::new 原生支持两者，无需额外工具链。
6. **资源限制三重保障**：fuel（指令预算）+ 内存上限 + 超时。
7. **零直接能力不变量**：默认 capabilities 全 false，仅经显式 host bridge 交互。
8. **可按需创建**：SandboxEnv::create(EnvConfig) 按实际需要创建隔离环境，支持快照/恢复。
9. **Consumer 零改动**：WasmShell/WasmFs/WasmEditor 实现现有 trait，BashTool/FsTool/EditTool 源码不改。
10. **可演进**：Phase 1 内存 VFS → Phase 2 overlay 挂载 → Phase 3 component model。
