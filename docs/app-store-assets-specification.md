# AIOPS Desktop 应用描述与发布素材配置规范

> 本文档用于在 **MiracleLittleBox（奇迹小匣子）- 应用素材工坊** 中快速录入并批量生成全套应用商店分发素材（截屏套壳、AI 图标、切片、宣传海报），同时可作为 Apple App Store / 各大应用市场正式提审的标准元数据。

---

## 一、 基础属性配置 (Basic Attributes)

| 配置字段 | 建议值 | 说明 |
| :--- | :--- | :--- |
| **应用名称 (App Name)** | `AIOPS Desktop` | 官方客户端产物与品牌标识（中文亦可使用 `DeepSeek AIOps`） |
| **应用分类 (Category)** | `开发者工具` (Developer Tools) / `效率` (Productivity) | 专注研发与 DevOps 场景，优先选开发者工具 |
| **包名 / Bundle Identifier** | `com.deepseek.aidops.desktop` | 跨平台及 macOS 标准反向域名规范 |
| **主要发布平台 (Target Platform)** | `Apple App Store (macOS)` 或 `双平台全面支持 (All Stores)` | 依发布渠道按需选择 |

---

## 二、 核心理念与主旨简介 (Core Concept & Pitch)

> **使用指引**：在「新建应用」或「应用管理」弹窗的“基础属性”页中，复制以下整段填入 **「核心理念与主旨简介」** 多行文本框。该内容将作为系统全套营销文案与 AI 生图提示词的上下文底座。

```text
AIOPS Desktop（DeepSeek-AIOps Harness）是一款专为专业开发者与 SRE/DevOps 工程师打造的工业级本地自主编码与智能体工作台。针对传统 AI 插件依赖重型 Electron 导致内存占用高、复杂长任务中途崩溃不可恢复、代码变更缺乏确定性审查等痛点，应用采用纯 Rust 与原生 GUI 微内核构建，彻底剔除 WebView，带来极致响应速度与极低系统资源开销。深度对标 Codex 工业标准，原生集成结构化 CoT 思考链卡片、带行号差异比对（Git Diff）的协同检查器以及悬浮式指令控制舱。应用全面支持 DeepSeek、OpenAI、Anthropic 及本地离线模型，搭载 LHA 可恢复长时程自主编排引擎（支持 WAL 崩溃恢复、MVCC 工件投递与 HITL 人机检查点），具备严格受控的工作区感知工具体系与 AES-256 加密存储，在确保企业级代码隐私安全的前提下，提供极速、可控、确定性的下一代人机协同研发体验。
```

---

## 三、 截屏套壳配置 (Screenshot Studio)

在 MiracleLittleBox 的 **「截屏套壳配置」** 标签页中填入以下营销文案：

* **截屏统一主营销文案**：`原生极速，工业级本地编码智能体`
* **截屏统一副营销文案**：`Rust 原生微内核 • 流式 CoT 深度思考 • 可恢复长任务编排 • 协同检查器`
* **推荐外壳形态**：`macOS 窗口 / 极简桌面框架` 或 `深色工业视窗`
* **推荐背景光效**：`Apple Iris (极光渐变)` 或 `Deep Slate (钛金深灰)`

### 推荐 5 张展示画板文案规划

| 画板序号 | 展示功能界面 | 主标题 (Title) | 副标题描述 (Subtitle) |
| :---: | :--- | :--- | :--- |
| **01** | **智能体工作台总览**<br>(Main Workspace & Command Deck) | `工业级工作台 • 专注心流编程` | `Slate/Zinc 工业质感界面，悬浮指令控制舱，响应毫秒直达` |
| **02** | **推理执行流**<br>(CoT Thinking Chain & Tool Cards) | `透明思维轨迹 • 决策步步可视` | `流式展示结构化 CoT 思考链，ToolAction 步骤拆解，调用清晰可溯` |
| **03** | **协同检查器**<br>(Collaborative Inspector & Diff) | `三模态协同审查 • 变更确定掌控` | `带行号差异比对（Diff）、代码文件高亮预览与运行时 DAG 状态遥测 HUD` |
| **04** | **LHA 长时程编排**<br>(Long-Horizon Autonomous Engine) | `长任务自主编排 • 随时断点续航` | `持久化 DAG、WAL 崩溃自愈、质量门契约锁与高危操作人工检查点（HITL）` |
| **05** | **模型枢纽与本地安全**<br>(Model Hub & Local Security) | `模型自由切换 • 本地隐私至上` | `深度优化 DeepSeek 并兼容多模型，AES-256-GCM 密文存储，代码绝不泄露` |

---

## 四、 应用图标配置 (App Icon Configuration)

在 MiracleLittleBox 的 **「应用图标配置」** 标签页中填入：

* **设计美学风格**：`极简现代科技 (Modern Tech)` 或 `微光新拟态 (Dark Neumorphic)`
* **切片与预览蒙版**：`Apple 超椭圆圆角 (Squircle)`

### 1. 图标主旨构思与象征符号 (中文描述)
> 悬浮在深空碳灰背景上的极简科技魔方/智能体核心（Agent Core）。中央嵌有一枚发光湛蓝与青翠微光双色交织的几何无限棱镜（代表循环演进与自主推进），周围环绕着极简精密的代码编译折线与微光节点。硬核工业质感，深色暗调磨砂材质，无任何文字，符合 Apple macOS 原生图标设计规范。

### 2. AI 图像生成 Prompt (英文 Midjourney / DALL·E 3 / SD)
```text
Professional macOS app icon, an industrial sleek glowing geometric agent core floating in dark slate space, subtle neon cyan and deep blue accents, precision circuit and code prism refraction, Apple squircle shape, dark mode titanium matte finish, high fidelity, 8k resolution, minimalist, no text, clean developer tool aesthetics.
```

---

## 五、 宣传海报配置 (Marketing Posters)

在 MiracleLittleBox 的 **「宣传海报配置」** 标签页中填入：

* **宣传海报规格预设**：`应用商店横幅 (App Store Feature Banner)` / `桌面端横版推广海报`
* **宣传海报主标题**：`重塑专业开发者的自主智能体编码体验`
* **宣传海报副标题**：`零 WebView 纯原生 Rust 架构 • 对标 Codex 交互标准 • 掌控端到端工程闭环`

---

## 六、 应用商店官方发布文案 (App Store Connect 提交规范)

### 1. 推广副标题 (Subtitle，30 字符内)
`工业级本地自主编码智能体工作台`

### 2. 搜索关键词 (Keywords，100 字符内，英文逗号分隔)
`AI编码,DeepSeek,代码助手,DevOps,AIOps,智能体,Agent,Rust,代码审查,GitDiff,程序员工具,开发者工作台,本地模型`

### 3. 应用完整介绍 (App Description)
```text
【AIOPS Desktop —— 面向专业工程的高性能原生编码 Agent 工作台】

AIOPS Desktop 专为拒绝笨重框架、追求极致性能与确定性交付的专业开发者与运维工程师打造。应用采用 Rust 原生微内核与轻量图形界面，实现零 Electron / 零 WebView 开销，将大语言模型的自主编码与 DevOps 治理能力无缝注入本地开发工作区。

◆ 为什么选择 AIOPS Desktop？

1. 原生极速工作台（Native & Ultra-fast）
- 纯 Rust 与轻量 GUI 构建，毫秒级快速冷启动，内存占用远低于传统 Electron 客户端；
- 界面深度对标 Codex 工业标准，采用 Slate/Zinc 低饱和质感配色，配备悬浮式 Command Deck 指令控制舱；
- 会话导航支持智能时间聚类（今天/昨天/近7天/更早），工作区状态井井有条。

2. 结构化思维与透明执行（Structured CoT & Tool Cards）
- 完整呈现大模型 Chain-of-Thought（CoT）流式深度思考链；
- 结构化 ToolAction 步骤卡，清晰解构文件读取、精准行级编辑、Shell 容器执行等工具调用全过程，告别黑盒盲猜。

3. 三模态协同检查器（Collaborative Inspector）
- 代码文件即时高亮预览：快速审阅上下文关联源码；
- 结构化 Git Diff 变更集实时比对：变更前后带行号清晰对照，写盘收敛可控；
- 运行时 DAG 遥测 HUD：任务依赖链路、Token 开销与执行状态一目了然。

4. LHA 可恢复长时程自主编排（Long-Horizon Autonomous Engine）
- 面向复杂多阶段长任务，提供持久化 DAG 与 WAL 崩溃自愈能力，进程中断或重启随时断点续航；
- 契约锁机制（Contract Lock）：严格阻断公共 API 漂移与路径逃逸风险；
- 人机回环控制（HITL）：针对删除、推送、发布等不可逆高危操作强制触发人工确认检查点。

5. 多模型兼容与离线回放（Model Hub & Replay）
- 深度优化 DeepSeek 原生模型推理，同时全面兼容 OpenAI、Anthropic 协议及本地部署模型；
- 独家支持 Replay 离线回放模式，无外网环境下亦可验证工作流与工程交互。

6. 企业级隐私与边界防御（Security First）
- 严格的工作区受控沙箱，文件与命令执行受细粒度策略限制；
- API Key 采用 AES-256-GCM 强加密保存于本地安全存储库；
- 会话采用追加式本地持久化，无第三方云端 telemetry 劫持，守卫核心代码资产。

立刻开启 AIOPS Desktop，体验纯粹、极速、可控的下一代自主研发工作流！
```

### 4. 版本更新日志 (Release Notes / What's New)
```text
版本 0.3.0 更新亮点：
1. 【UI 重塑】深度对标 Codex 标准，上线全新 Slate/Zinc 工业级工作台与 Command Deck 悬浮指令舱。
2. 【检查器升级】全新三模态协同检查器，支持带行号 Git Diff 变更集审查与运行时 DAG 状态遥测。
3. 【执行引擎】强化 LHA 可恢复长任务编排，支持写盘收敛与预算动态治理。
4. 【体验优化】优化快捷键响应，Shift+Enter 支持多行编辑；完善离线 Replay 模式。
```
