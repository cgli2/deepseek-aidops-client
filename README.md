# DeepSeek-AIOps Harness

DeepSeek-AIOps 的原生 Rust 编码代理（移植 dsh 微内核「一切皆插件」思想），配套 **AIOPS Desktop** 桌面客户端（egui/eframe GUI）。已打通 GUI → DeepSeek → 工具调用 → 文件/命令执行 → 工具结果回传 → 继续推理的完整闭环。

> 系统设计见 [`docs/system-design-completion.md`](docs/system-design-completion.md)；
> 功能→扩展点映射见 [`harness/extensions/EXTENSION-COOKBOOK.md`](harness/extensions/EXTENSION-COOKBOOK.md)。

## 内容说明

- **AIOPS Desktop（GUI）**：气泡消息流 + 自动滚底、深/浅色主题切换、Markdown 渲染与右键复制、历史会话管理（按项目隔离/重命名/精简/清空）、项目快速切换、插件管理（核心插件恒启用 + WASM 插件导入）、模型配置多 Profile（API Key 经 **AES-256-GCM** 加密存入 SQLite，跨操作系统通用）。
- **多模型接入**：DeepSeek / OpenAI / Anthropic / Local / Replay（离线回放），SSE 流式 + Function Calling 工具分片累积。
- **模型可见工具**：`fs`（工作区读写/列表，拒绝越界路径）、`edit`（唯一精确替换）、`bash`（平台沙箱内执行，默认 30 秒超时）；工具结果自动回填继续推理，单回合最多 32 步防失控。
- **系统级能力（借鉴 Codex）**：记忆（`.harness-memory/` 文件持久化）、钩子（`pre_tool_use` 可阻断危险调用，fail-closed）、Git CLI 集成 + Worktree RAII 守卫。
- **进程外边界**：ACP stdio JSON-RPC 服务器 + SDK 客户端。
- **会话真相源**：`<工作区>/.harness/sessions/*.jsonl`，fork/resume/replay 全派生自它。

## 架构说明

Cargo workspace（`harness/`，16 个 crate），微内核 + 能力接缝三角色（Definition / Provider / Consumer）：

```
harness-core        微内核：AppContext(TypeMap) + 类型化事件总线 + 可逆注册 + Plugin 拓扑组合
harness-session     会话追加日志（真相源，fork/resume/replay 全派生自它）
harness-llm         LlmProvider trait + 消息/工具契约 + Replay/DeepSeek/OpenAI/Anthropic/Local
harness-capability  能力接缝 Definition（纯 trait：Shell/Fs/Editor/Lsp/Subagent/Compaction/Memory/Hook/Git）
harness-provider-*  Provider 实现：local（bash/fs/editor/lsp/watcher）、memory、hook、
                    git、sandbox（landlock/seccomp/JobObject/Null）、wasm（Wasmtime 沙箱导入）
harness-tool        模型可见工具（Consumer，仅依赖 capability trait）
harness-runtime     tokio 编排 + Agent 循环 + 工具管线 + 多任务调度（层级取消）
harness-ui          UI 入口（trait）+ NullUi / TuiUi / EguiUi（AIOPS Desktop）
harness-acp         ACP stdio JSON-RPC 服务器（进程外边界）
harness-sdk         进程外 JSON-RPC 客户端（宿主侧边界）
bin                 组合入口：Profile → compose_plugins → run（产物 aidops-desktop）
```

**插件机制要点**（详见设计文档 §11）：

- 可逆副作用：`Registration` 的 `Drop` = 自动回滚（等价 dsh `effect()`）。
- 依赖声明组合：`Plugin::deps()` → 拓扑排序 → 按序 `register`。
- WASM 动态加载不可信 Provider：`harness-provider-wasm`（feature `wasm-tools`，默认零直接宿主能力，仅显式 `host_log` / `shell_run` 桥接）。
- 平台沙箱：Windows JobObject（进程树随宿主回收）、Linux landlock + seccomp；内核/系统不支持时优雅降级。

## 目录结构

```
harness/            Cargo workspace（全部源码 + 构建脚本 + 配置）
├── bin/            可执行入口（aidops-desktop）
├── harness-*/      各 crate（见架构说明）
├── scripts/        build.bat（Windows）/ build.sh（Linux/macOS）
├── config/         default.toml（hooks、模型等默认配置）
├── extensions/     EXTENSION-COOKBOOK.md（扩展开发手册，随发布包分发）
└── dist/           打包交付物（aidops-desktop[.exe] + config + cookbook）
docs/               系统设计 / 架构分析文档
```

## 构建与打包

前提：安装 Rust stable（`rustup`）。打包命令统一使用 `--all-features`，确保交付物包含全部能力（GUI / TUI / WASM / 沙箱）。

### Windows

需要 VS Build Tools（提供 `vcvars64.bat` / link.exe）。脚本会自动初始化 MSVC 环境并切到 `x86_64-pc-windows-msvc` 目标，无需手动设置（默认 GNU 目标缺 `dlltool.exe` 会失败）。

```bat
cd harness
scripts\build.bat package        :: release 构建 → dist\aidops-desktop.exe
scripts\build.bat check -p harness-ui --all-features   :: 编译检查
scripts\build.bat test -p harness-ui                   :: 单元测试
```

产物：`harness\dist\aidops-desktop.exe`（连同 `config\default.toml`、`EXTENSION-COOKBOOK.md`）。
注意：若 `dist\aidops-desktop.exe` 正在运行会锁定文件导致打包失败，先关闭程序再重试。

### Linux

```bash
cd harness
./scripts/build.sh package       # release 构建 → dist/aidops-desktop
```

GUI（eframe + OpenGL）需要图形开发库，若链接报错可安装：

```bash
# Debian/Ubuntu 示例
sudo apt install build-essential pkg-config libxkbcommon-dev libssl-dev
```

沙箱能力需内核 ≥ 5.13（landlock），否则自动降级为无沙箱执行。

### macOS

```bash
cd harness
./scripts/build.sh package       # release 构建 → dist/aidops-desktop
```

原生 Metal/OpenGL 与 AppKit 支持开箱即用，无额外依赖。首次运行如被 Gatekeeper 拦截，可用 `xattr -d com.apple.quarantine dist/aidops-desktop` 或右键打开。

### iOS

**不支持**。本项目为桌面编码代理（键盘/鼠标工作流 + 本地文件系统/Shell 工具），egui/eframe 桌面形态不适用于 iOS；iOS 不在构建矩阵内。

> 交叉编译说明：三平台脚本均在**各自平台本机**构建，不支持脚本化的跨平台交叉编译（Windows↔Linux 需要额外配置目标工具链与图形库）。

## 运行

```bash
# headless 回放闭环（不依赖真实 LLM，便于 CI / 冒烟）
HARNESS_REPLAY=1 cargo run -p harness-bin

# 真实 DeepSeek
DEEPSEEK_API_KEY=sk-... cargo run -p harness-bin

# TUI / GUI 形态（对应 feature 需先编入）
cargo run -p harness-bin --features tui -- --tui
```

直接运行发布包：

```powershell
$env:DEEPSEEK_API_KEY = "sk-..."                       # 可选，也可在 GUI「模型设置」中保存
$env:HARNESS_WORKSPACE = "D:\path\to\project"          # 可选；默认是启动目录
.\dist\aidops-desktop.exe
```

### 数据存放位置

- **配置数据库**：默认存放在**可执行文件旁边** `DeepSeekAIOps\settings.db`（便携：程序拷走数据跟着走）；exe 目录不可写时回退 `%LOCALAPPDATA%\DeepSeekAIOps`（Windows），再回退当前目录。旧版本存于 `%LOCALAPPDATA%` 的数据会在首次启动时自动迁移（含密钥文件），迁移记录写入 `harness_gui_trace.log`。
- **密钥文件**：`settings.key`（AES-256-GCM 本地密钥）与 `settings.db` 同目录，首次保存密钥时生成。
- **会话日志**：按项目隔离，`<工作区>/.harness/sessions/*.jsonl`。
- **启动诊断**：GUI 无控制台时的 trace 写到可执行文件旁的 `harness_gui_trace.log`。

### 钩子配置示例（借鉴 Codex Hooks）

在 `config/default.toml` 的 `[hooks]` 表下，把生命周期事件映射到外部命令：

```toml
[hooks]
pre_tool_use = "scripts/check-tool.sh"     # 命令异常 / 返回 {"decision":"block"} 即阻断
post_tool_use = "scripts/audit-tool.sh"
```

钩子命令从 stdin 读取 JSON `HookPayload`，向 stdout 写 `{"decision":"allow"|"block","reason":"..."}`。未配置任何钩子时退化为 `NullHook`（全放行）。详见设计文档 §13.4。

## 安全边界

- FS/编辑器执行工作区边界检查；`bash` 在平台沙箱之上仍可触达宿主资源（JobObject 限生命周期不限权限、landlock 需内核 ≥ 5.13）。
- 运行不受信任务前请配置 `pre_tool_use` 钩子或使用外部容器/受限账户。
- WASM 插件默认零直接宿主能力，仅经显式 capability bridge 交互。

详见设计文档 §13 与 [`harness/extensions/EXTENSION-COOKBOOK.md`](harness/extensions/EXTENSION-COOKBOOK.md)。
