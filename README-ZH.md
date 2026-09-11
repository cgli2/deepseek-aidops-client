# DeepSeek-AIOps Harness

[English](README.md)

面向本地工程工作的原生 Rust 编码 Agent 平台，配套 **AIOPS Desktop** 桌面客户端。它将图形界面、
模型工具调用、工作区操作与可恢复的长任务编排置于同一个轻量运行时中。

## 项目定位

Harness 不依赖 Electron 或 WebView，而是使用 Rust 与 egui/eframe 构建原生桌面体验。它关注的
不是堆叠功能数量，而是让 Agent 在本地工作区中保持低开销、可控、可恢复，适合日常编码、DevOps、
AIOps、CI 冒烟和资源受限的开发环境。

## 核心亮点

- **原生桌面端**：会话历史、项目切换、Markdown 渲染、深浅主题、模型配置和本地加密密钥存储。
- **完整 Agent 闭环**：流式模型响应、Function Calling、工具执行、结果回填和继续推理在同一运行时完成。
- **多模型与离线回放**：支持 DeepSeek、OpenAI 兼容接口、Anthropic 兼容接口、本地模型和 Replay。
- **工作区感知工具**：文件系统、精确编辑、Shell、搜索、计划、记忆和 Git 能力均受策略与边界约束。
- **可恢复长任务**：持久化 DAG、WAL 恢复、租约看门狗、Token/RPM/TPM 预算、质量门、不可变工件库
  以及不可逆操作的人工检查点。
- **微内核插件架构**：通过 Definition / Provider / Consumer 能力接缝组合 GUI、TUI、Headless、ACP、
  沙箱和 WASM 能力。

## 快速开始

### 前置条件

- 通过 [rustup](https://rustup.rs/) 安装 Rust stable
- Windows 需要 Visual Studio Build Tools（MSVC 工具链）
- 使用真实模型时准备相应 API Key；也可先使用离线 Replay 模式

### 从源码运行

```bash
cd harness

# 离线回放：适合 UI 开发、CI 和冒烟验证
HARNESS_REPLAY=1 cargo run -p harness-bin

# 使用 DeepSeek
DEEPSEEK_API_KEY=sk-... cargo run -p harness-bin
```

默认配置会启动 GUI。模型提供商、地址、模型名与 API Key 也可以在桌面端「模型设置」中保存。
PowerShell 请先用 `$env:变量名 = "值"` 设置环境变量，再执行 Cargo 命令。

### 常用命令

```bash
make help                 # 查看全部命令
make dev                  # 启动桌面端
make dev-replay           # 使用离线 Replay 模型启动
make dev-watch            # UI / 入口源码变化后自动重编译并重启
make check                # 全工作区编译检查
make test                 # 全工作区测试
```

Windows 请使用随仓库提供的脚本，它会配置 MSVC 目标：

```bat
cd harness
scripts\build.bat check -p harness-bin
scripts\build.bat package
```

## 长任务编排

长任务控制面状态保存于 `<工作区>/.harness/long-horizon/`，可从 CLI 操作：

```bash
cd harness
cargo run -p harness-bin -- lh status
cargo run -p harness-bin -- lh submit "完成构建、测试并修复所有失败"
cargo run -p harness-bin -- lh approve <checkpoint-id> "已核对暂存工件"
cargo run -p harness-bin -- lh reject <checkpoint-id> "风险不可接受"
```

交互式长任务默认**没有墙钟时间上限**，由用户取消、进程退出和 Worker 租约处理停止与恢复。
仅当设置正整数 `HARNESS_TURN_TIMEOUT_SECS` 时才启用运维截止时间；未设置或设为 `0` 表示不限时。

## 架构概览

| 层 | 主要 crate | 职责 |
| --- | --- | --- |
| 微内核 | `harness-core` | 类型化应用上下文、事件、插件、工作区与配置 |
| 领域原语 | `harness-llm`、`harness-session` | 模型 Provider 契约与追加式会话真相源 |
| 能力接缝 | `harness-capability` | 工具和集成的纯 trait 定义 |
| Provider | `harness-provider-*` | Local、Sandbox、WASM、Memory、Git、Hook 等实现 |
| 运行时 | `harness-runtime`、`harness-tool` | Agent 循环、调度、治理和模型可见工具 |
| 边界与界面 | `harness-acp`、`harness-sdk`、`harness-ui`、`bin` | ACP、SDK、桌面/TUI 表现层与程序集成 |

三个关键原则：会话事件是事实来源；Consumer 只依赖能力 trait 而不依赖具体 Provider；UI 订阅
事件来呈现状态，不反向驱动 Agent 循环。完整图示与依赖关系见[架构文档](docs/architecture.md)。

## 安全与数据

- 文件和编辑操作限制在所选工作区内。
- Shell 受策略控制；处理不可信任务时，请配置 Hook 或使用受限账户/外部容器。
- WASM Provider 默认没有直接宿主权限，只有显式启用能力桥接后才可访问宿主能力。
- 桌面端 API Key 使用 AES-256-GCM 加密后保存于本地设置数据库。
- 会话日志位于 `<工作区>/.harness/sessions/`；长任务控制面位于
  `<工作区>/.harness/long-horizon/`。

## 文档

- [English README](README.md)
- [系统设计](docs/system-design-completion.md)
- [架构与依赖图](docs/architecture.md)
- [扩展开发手册](harness/extensions/EXTENSION-COOKBOOK.md)
- [长时程自主编排设计](docs/Long-Horizon%20Autonomous%20Orchestration%20Architecture.md)

## License

本项目基于 [MIT License](LICENSE) 开源。Copyright © 2026 cgli.
