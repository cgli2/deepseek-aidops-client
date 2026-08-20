# DeepSeek-AIOps Harness

DeepSeek-AIOps 的原生 Rust 编码代理（移植 dsh 微内核「一切皆插件」思想），配套 **AIOPS Desktop** 桌面客户端（egui/eframe GUI）。已打通 GUI → DeepSeek → 工具调用 → 文件/命令执行 → 工具结果回传 → 继续推理的完整闭环。

## 设计动机：为什么重复造轮子？

市面上的主流 AI 编码代理（Codex、OpenCode、Qoder、WorkBuddy 等）多基于 Electron / WebView 或 Node / Python 技术栈：功能齐全，但 **启动慢、内存与 CPU 占用高、常驻后台代价大**，在配置一般的机器上容易拖累开发体验。

本 Harness 选择 **纯 Rust 原生** 路线，不与它们比"功能堆叠"，而是比"轻、快、省"：

- **零运行时、零 WebView、零 Electron**：二进制仅几 MB 到十几 MB，冷启动以秒计；egui 即时模式 GUI 直接绘制在原生窗口，不挂浏览器进程。
- **极致性能与低占用**：无 GC 停顿、无解释器开销；长会话与大上下文下仍保持低内存、低 CPU，交互不卡顿。
- **可裁剪的微内核**：能力以插件挂载，可按需编译 `GUI / TUI / headless` 三形态，交付物只含要用到的能力（WASM / 沙箱可默认关闭）。
- **为常驻而生**：适合 DevOps / AIOps 后台常驻、CI 冒烟、资源受限环境——这是重型客户端难以兼顾的场景。

> 一句话：**别人把 Agent 做成功能齐全的"操作系统"，我们把 Agent 做成随手即用的"瑞士军刀"**——极致高效、极低开销的用户体验，是本项目存在的唯一理由。

> 系统设计见 [`docs/system-design-completion.md`](docs/system-design-completion.md)；
> **架构图与层次图**（L0–L7 依赖方向、运行时数据流、crate 职责）见 [`docs/architecture.md`](docs/architecture.md)；
> 功能→扩展点映射见 [`harness/extensions/EXTENSION-COOKBOOK.md`](harness/extensions/EXTENSION-COOKBOOK.md)。

## 内容说明

- **AIOPS Desktop（GUI）**：气泡消息流 + 自动滚底、深/浅色主题切换、Markdown 渲染与右键复制、历史会话管理（按项目隔离/重命名/精简/清空）、项目快速切换、插件管理（核心插件恒启用 + WASM 插件导入）、模型配置多 Profile（API Key 经 **AES-256-GCM** 加密存入 SQLite，跨操作系统通用）。
- **多模型接入**：DeepSeek / OpenAI / Anthropic / Local / Replay（离线回放），SSE 流式 + Function Calling 工具分片累积。
- **模型可见工具**：`fs`（工作区读写/列表，拒绝越界路径）、`edit`（唯一精确替换）、`bash`（平台沙箱内执行，默认 30 秒超时）；工具结果自动回填继续推理，单回合最多 32 步防失控。
- **系统级能力（借鉴 Codex）**：记忆（`.harness-memory/` 文件持久化）、钩子（`pre_tool_use` 可阻断危险调用，fail-closed）、Git CLI 集成 + Worktree RAII 守卫。
- **进程外边界**：ACP stdio JSON-RPC 服务器 + SDK 客户端。
- **会话真相源**：`<工作区>/.harness/sessions/*.jsonl`，fork/resume/replay 全派生自它。

## 架构说明

采 **微内核 + “一切皆插件”** 设计：`harness-core` 只提供内核原语（服务仓库、事件总线、插件组合、配置、扩展点），所有业务能力以 **Definition / Provider / Consumer** 三角色挂在能力接缝上。

> 📐 完整的 **层次图（L0–L7）** 与 **运行时数据流图** 见 [`docs/architecture.md`](docs/architecture.md)（Mermaid 渲染，含每层的 crate 依赖方向、外部边界，以及本次 A/B/C/D + 图标 + 版本管理分别落在哪一层）。

**分层（自上而下为依赖方向，上层依赖下层；`harness-core` 是地基，不依赖任何内部 crate）：**

| 层 | Crate（角色） | 职责 |
|----|---------------|------|
| **L0 微内核** | `harness-core` | `AppContext`(TypeMap) · 事件总线 · `Plugin` 拓扑排序 · 配置（原子写 + 热重载）· `Workspace` · `Update` · `ui_input` |
| **L1 领域原语** | `harness-llm`、`harness-session` | LLM 流式 I/O 与 Provider；`SessionLog`（真相源，fork/resume/replay 全派生自它） |
| **L2 能力定义** | `harness-capability` | 纯 trait 接缝：`Shell`/`Fs`/`Editor`/`Lsp`/`Subagent`/`Compaction`/`Memory`/`Hook`/`Git`（零实现） |
| **L3 能力实现** | `harness-provider-*`（local/sandbox/wasm/memory/hook/git） | Provider 落地：bash/fs/editor/lsp/watcher、WASM 沙箱、钩子、Git… |
| **L4 运行时与工具** | `harness-runtime`、`harness-tool` | tokio 编排 + Agent 循环 + 多任务调度；模型可见工具（Consumer，只认 trait） |
| **L5 协议边界** | `harness-acp`、`harness-sdk` | ACP stdio JSON-RPC 服务器（事件总线消费者）+ 宿主侧客户端 |
| **L6 表现层** | `harness-ui` | `Ui` trait + `NullUi` / `TuiUi` / `EguiUi`（AIOPS Desktop）；设置、更新横幅、图标 |
| **L7 组装根** | `bin` | 组合入口：Profile → `compose_plugins` → run（产物 `aidops-desktop`） |

（快速 ASCII 概览见下；精确依赖箭头以 [`docs/architecture.md`](docs/architecture.md) 为准。）

```
harness-core        微内核：AppContext(TypeMap) + 类型化事件总线 + 可逆注册 + Plugin 拓扑组合
harness-session     会话追加日志（真相源，fork/resume/replay 全派生自它）
harness-llm         LlmProvider trait + 消息/工具契约 + Replay/DeepSeek/OpenAI/Anthropic/Local
harness-capability  能力接缝 Definition（纯 trait：Shell/Fs/Editor/Lsp/Subagent/Compaction/Memory/Hook/Git）
harness-provider-*  Provider 实现：local（bash/fs/editor/lsp/watcher）、memory、hook、
                    git、sandbox（landlock/seccomp/JobObject/Null）、wasm（Wasmtime 沙箱导入）
harness-tool        模型可见工具（Consumer，仅依赖 capability trait）
harness-runtime     tokio 编排 + Agent 循环 + 工具管线 + 多任务调度（层级取消）
harness-ui          Ui 入口（trait）+ NullUi / TuiUi / EguiUi（AIOPS Desktop）
harness-acp         ACP stdio JSON-RPC 服务器（进程外边界）
harness-sdk         进程外 JSON-RPC 客户端（宿主侧边界）
bin                 组合入口：Profile → compose_plugins → run（产物 aidops-desktop）
```

**三条关键不变量**（详见架构文档 §1）：

1. **会话日志是真相源**：模型可见的一切都能从 `SessionLog`（追加事件流）重建；运行时不在别处另存状态。
2. **Consumer 永不直接依赖 Provider**：工具只依赖 `capability` 的纯 trait；换 `LocalBash` 为 `WasmShell`，工具源码零改动。
3. **UI 是事件总线的纯消费者**：UI 只订阅 `SessionLog` / 事件总线渲染，不反向调用核心循环。

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
docs/               系统设计 / 架构文档（architecture.md 含层次图与运行时图）/ 分析文档
```

## 构建与打包

前提：安装 Rust stable（`rustup`）。打包命令统一使用 `--all-features`，确保交付物包含全部能力（GUI / TUI / WASM / 沙箱）。

### 快捷命令

仓库根目录提供统一 `Makefile`。日常开发只需记住：

```bash
make help                 # 查看全部命令及可覆盖变量
make doctor               # 检查 Rust、Xcode 工具和签名证书
make install-dev-tools    # 首次安装 cargo-watch
make dev                  # 启动 GUI 开发版
make dev-replay           # 离线 Replay 模型启动，不需要 API Key
make dev-watch            # 源码变化后自动重编译并重启 GUI
make dev-replay-watch     # Replay 离线热重启开发模式
make check                # workspace 全功能编译检查
make test                 # workspace 全功能测试
make logs                 # 跟踪 macOS GUI 诊断日志
```

指定应用打开后的默认工作区：

```bash
make dev WORKSPACE=/path/to/project
make dev-watch WORKSPACE=/path/to/project
```

`dev-watch` 属于自动重编译并重启模式，不是 Web 前端式的组件 HMR。修改 Rust 源码后，
`cargo-watch` 会停止旧进程、增量编译并重新启动应用；SQLite 设置、会话日志等持久化数据
会保留，尚未保存的输入框内容等内存状态会清空。使用 `Ctrl+C` 停止监听。

### 融合窗口标题栏

GUI 默认在 macOS 和 Windows 启用融合标题栏：macOS 保留原生交通灯并把工作台延伸到
标题栏区域；Windows 使用自绘最小化、最大化/恢复、关闭按钮以及窗口拖动和八方向缩放。
Linux 和其他平台继续使用系统装饰。遇到远程桌面或特殊窗口管理环境时可临时回退：

```bash
AIOPS_NATIVE_TITLEBAR=0 make dev
```

也可在「系统配置 → 窗口外观」中持久化开关，修改后重启应用生效；环境变量优先级高于
持久化设置，适合在窗口异常时强制恢复系统标题栏。

Windows 发布前需要在 Windows 10/11 真机验证 100%–200% DPI、跨屏拖动、最大化恢复和
Snap Layout；macOS 发布前需要验证普通、最大化、全屏及多显示器交通灯安全区域。

构建和 macOS 发布：

```bash
make icon                 # 重新生成 Windows/macOS/运行时图标
make package              # 当前平台默认打包
make package-mac-dev      # Apple Development 本地测试包
make package-mac-release  # Developer ID 正式签名，不上传 Apple
make package-mac-notarize # 正式签名、公证、staple
make verify-mac           # 验证签名、公证票据并输出 SHA-256
```

公证默认使用钥匙串 Profile `aidops-notary`，可通过
`make package-mac-notarize NOTARY_PROFILE=其他名称` 覆盖。Apple ID 专用密码只应保存在
`notarytool` 钥匙串 Profile 中，不要写入命令、Makefile 或仓库。

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
./scripts/build.sh package       # release 构建 → dist/AIOPS Desktop.app + DMG
```

Bundle Identifier 为 `com.clotee.aidops`，Team ID 为 `VATCH8RNM8`。默认 `auto`
模式会优先选择 `Developer ID Application`，否则使用已安装的团队开发证书，最后才回退
ad-hoc。也可以通过 `MACOS_SIGNING_MODE=development|release|adhoc` 明确指定。

正式 DMG 发布只接受 `Developer ID Application`，不会误用面向 App Store 的
`Apple Distribution` 证书。签名并公证：

```bash
MACOS_SIGNING_MODE=release \
MACOS_NOTARY_PROFILE="aidops-notary" \
./scripts/build.sh package
```

`MACOS_NOTARY_PROFILE` 需提前通过 `xcrun notarytool store-credentials` 保存。
设置 `MACOS_UNIVERSAL=1` 可同时构建 Apple Silicon 与 Intel 并合并为 Universal 2；
默认只构建当前 Mac 的架构。可通过 `MACOS_BUILD_NUMBER` 覆盖 Bundle build number。

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

- **配置数据库**：macOS 使用 `~/Library/Application Support/com.clotee.aidops/settings.db`；Windows/Linux 默认存放在可执行文件旁边 `DeepSeekAIOps/settings.db`，不可写时回退平台目录或当前目录。旧版 Windows 数据会在首次启动时自动迁移（含密钥文件），迁移记录写入 `harness_gui_trace.log`。
- **密钥文件**：`settings.key`（AES-256-GCM 本地密钥）与 `settings.db` 同目录，首次保存密钥时生成。
- **会话日志**：按项目隔离，`<工作区>/.harness/sessions/*.jsonl`。
- **启动诊断**：macOS 写入上述 Application Support 目录；其他平台写到可执行文件旁的 `harness_gui_trace.log`。

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

## License

本项目以 **MIT 协议** 开源。完整的许可证文本见仓库根目录 [`LICENSE`](LICENSE) 文件。

> Copyright (c) 2026 cgli
>
> 任何人可按 MIT 条款自由使用、复制、修改、合并、分发、再许可及销售本软件及其副本，详见 [`LICENSE`](LICENSE)。
