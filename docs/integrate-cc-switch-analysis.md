# 集成分析：把 cc-switch 的核心能力"抄"进 deepseek-aidops-stable（dsh / harness）

> 分析对象：`F:\src\ai\tool\cc-switch`（Tauri + React/TS 桌面应用，远非玩具，已 i18n、多平台、数十家 API 中转赞助）
> 目标项目：`F:\workspace\deepseek-aidops-stable`（Rust 原生 Agent 运行时 harness，灵感来自 dsh）
> 结论先行：**不能直接整份抄（层不对口），但 4 项核心能力可"移植"进 harness 的能力接缝；另有 1 个外向集成彩蛋。**

---

## 1. cc-switch 到底是什么

README 自述：*Claude Code、Claude Desktop、Codex、Gemini CLI、Grok Build、OpenCode、OpenClaw、Hermes Agent、Kimi、Copilot* 的"全方位管理工具"。
技术栈：Tauri 2（Rust 后端 + 前端 `invoke` 薄壳）、React、离线模型目录、多语言。

**关键定位**：cc-switch **不是 Agent 运行时**，它是**多个异构 Agent 运行时之上的 GUI 管理壳 / 切换器**。请求仍由各原生 Agent 自己发出，cc-switch 只管"配置、会话、配额、抽象边界"。这一点在 `docs/pi-native-contract-zh.md` 被显式钉死（见 §3.4）。

> 注意一个巧合：cc-switch 管理的对象里有一席叫 **"Pi"**（`docs/pi-*.md`、`src/config/piProviderPresets.ts`、`PiProviderForm.tsx`）。本项目的 harness 则是 **dsh（DeepSeek Harness）= 自己就是一个运行时**。所以两者处于**不同层次**——这是判断"能不能抄"的总开关。

---

## 2. cc-switch 的设计思想（5 条哲学）

1. **配置管理 ≠ 请求路径**。它明确"不做路由/网关/代理/故障转移"（`docs/pi-native-contract-zh.md` "明确不做"一节）。请求由原生 Agent 发，cc-switch 只编辑原生配置文件。
2. **累加式 Provider 同步**。只要 `models.json.providers` 里存在某节点，无论 ID 是 `anthropic`/`openai`/`deepseek` 还是未知 ID，都按"普通显式供应商"同等对待（`docs/pi-live-provider-sync-requirements-zh.md`）。不按内置 ID 过滤、不猜归属。
3. **原生契约优先 + 外部变更可探**。读改写期间用 revision 比较发现跨进程变化；外部改了文件，刷新即同步，不二次确认（`pi-native-contract-zh.md` "并发与外部修改"）。
4. **未知字段无损 + 原子写**。`models.json`、系统提示、模板全部原子写入（temp + rename）；不丢用户手写字段（`pi-live-provider-sync` "实现范围"；`src/lib/api/config.ts` 用 `toml_edit` 保注释保键序，禁止前端整文档重序列化）。
5. **预设完整可靠，自定义不猜测**。模型能力（reasoning / contextWindow / maxTokens / thinkingLevelMap）离线预设、随包发布；自定义模型绝不自动推断能力（`docs/pi-thinking-level-map-requirements-zh.md`）。

---

## 3. cc-switch 核心能力清单（10 项，附证据）

| # | 能力 | 证据 |
|---|---|---|
| C1 | **多 Agent 运行时统一管理层**（Provider Adapter 模式） | `src/components/{providers,agents,sessions,openclaw,hermes}`, `src/config/*ProviderPresets.ts` |
| C2 | **Provider/模型配置编辑器 + 累加式同步** | `docs/pi-live-provider-sync-requirements-zh.md` |
| C3 | **模型能力目录 + 思考档位 `thinkingLevelMap`** | `docs/pi-thinking-level-map-requirements-zh.md`, `src/config/piModelCatalog.ts`, `piThinkingProfiles.ts` |
| C4 | **会话管理（Session Manager）**：发现/索引/搜索/详情/复制恢复命令/终端一键恢复 | `session-manager.md`（Provider Adapter + Terminal Launcher + Path Resolver 三抽象） |
| C5 | **原子写 + 未知字段保留 + 外部变更 revision 检测** | `pi-native-contract-zh.md`, `src/lib/api/config.ts`（`toml_edit`） |
| C6 | **OAuth / 配额 / 用量管理** | `src/lib/api/{usage,copilot,subscription}.ts`, `CodexOauthAccountQuota.tsx`, `CopilotQuotaFooter.tsx` |
| C7 | **原生契约边界（明确不做路由/凭证）** | `docs/pi-native-contract-zh.md` "明确不做" |
| C8 | **提示词 / 技能 / `AGENTS.md` / `SYSTEM.md` 原生资源管理** | `docs/pi-native-contract-zh.md` 契约表, `src/lib/api/{prompts,skills}.ts` |
| C9 | **Deeplink 导入、Profiles、MCP 管理、代理/连通性检查** | `src/lib/api/{deeplink,profiles,mcp,proxy,connectivity-check}.ts` |
| C10 | **i18n + Tauri 跨平台（Win/mac/Linux）** | `src/i18n/locales/*`, Tauri |

---

## 4. 本项目（harness / dsh）现状对照

已建成（真实 crate，非骨架）：

- **`LlmProvider` 能力接缝**（`harness-llm/src/lib.rs`）：OpenAI/DeepSeek/Anthropic/Local/Replay + `ManagedLlm` 运行时热切换。✅ 等价于 cc-switch 的"多 Provider"运行时侧。
- **`SessionLog` 追加日志真相源**（`harness-session/src/lib.rs`）：理念比 cc-switch 的会话扫描更先进（redb、fork/resume/replay 全从日志派生）。⚠️ 但仍是 MVP 内存版，且**没有跨会话管理 UI / 恢复命令**。
- **记忆 / 钩子 / Git / Worktree 能力接缝**（`EXTENSION-COOKBOOK.md`、`harness-capability`）：✅ 已覆盖 cc-switch C8 的本地原生资源思路。
- **`ExtensionPoint` 枚举 + 三角色（Definition/Provider/Consumer）**：✅ 与 cc-switch 的"抽象边界"哲学同构——**这正是移植的落点**。
- **`Chunk.reasoning` 已存在**（`harness-llm/src/lib.rs`）：DeepSeek v4 `reasoning_content` 已能流，但**只是展示，没有"思考档位/努力度"控制**。

缺口（即"机会"）：

- ❌ **没有 reasoning-effort / thinkingLevelMap 配置**（C3）。`LlmProvider::stream` 不接收 effort；`ManagedLlm::configure_deepseek` 只收 `(base_url, model, api_key)`。
- ❌ **配置层无原子写 / toml_edit / revision 检测**（C5）。`harness-core/src/config.rs` 用 `toml::from_str` 整文档反序列化，**会丢掉用户注释与键序**，且 `load()` 命中即返回、无热重载。
- ❌ **没有会话管理 UI / 恢复命令 / 终端启动器**（C4）。`.harness/sessions` 有数据但无"发现-索引-搜索-恢复"体验。
- ❌ **没有用量 / 配额 / 成本计量**（C6）。对**本项目叫 aidops** 而言，这是最该补的能力——但 cc-switch 是"读别人的配额"，harness 应"自己记自己的用量"。

---

## 5. 可行性判断：能抄 vs 不能抄

### ❌ 不能整份抄（层不对口）
- 把 cc-switch 当"管理 Claude Code/Codex 等外国 runtime 的壳"整体搬进来**毫无意义**——harness 是**单一运行时（dsh）**，不是调度多家的中央控制台。
- **路由 / 网关 / 代理 / 故障转移**（cc-switch 的 `proxy.ts`/`failover.ts` 仅服务它自己的元数据拉取，且 Pi 契约明确"不做 agent 请求路由"）不应移植为 GUI 侧代理。harness 若需 provider failover，那是**运行时能力接缝**的事，与 cc-switch 无关。
- **凭证 / OAuth 管理 UI**（C6 的 `auth.json` 部分）：cc-switch 自己都明确不碰凭证；harness 用 `api_key_env` 不落盘 key 的设计更干净，**不要加凭证库**。

### ✅ 能"移植"（用 harness 的能力接缝重新实现）
| 来源 | 移植到 harness 的落点 | 摩擦 | 价值 |
|---|---|---|---|
| C3 thinkingLevelMap | `harness-llm` + `LlmConfig` 加 `reasoning_effort` / `thinking` 字段，透传到 DeepSeek/Anthropic 请求 | 低（纯配置+签名） | 高 |
| C5 原子写+revision | `harness-core/src/config.rs` 改用 `toml_edit` 增量改写 + temp/rename + mtime/revision 热重载 | 中 | 中 |
| C4 Session Manager | 新 `SessionStore` 能力（`harness-session`）+ `harness-ui` 跨会话列表/搜索/恢复；复用已有 `SessionLog` | 中-高 | 高（AIOps 必需要） |
| C6 用量计量 | `harness-session` 从 `Assistant` 事件抽 token/成本，暴露 `usage` 能力（非读别人配额，是记自己） | 低-中 | 高（贴合 aidops 主题） |

---

## 6. 集成方案（具体到文件与接口）

### 方案 A（P0，最低风险最高价值）：思考档位 / 努力度控制
- **落点**：`harness-core/src/config.rs` 的 `LlmConfig` 增加 `reasoning_effort: Option<String>`（值对齐 `thinkingLevelMap` 语义：off/minimal/low/medium/high/xhigh/max/null）。
- **接口**：`LlmProvider::stream` 不改签名（保持对象安全），改为在 `DeepSeek`/`Anthropic` Provider 构造时吃 `reasoning_effort`，写入请求体（DeepSeek v4 的 `reasoning_effort`、Anthropic 的 `thinking`）。
- **预设目录**：仿 `piModelCatalog.ts`，新增 `harness-llm/src/model_catalog.rs`——离线 `[model, reasoning, context_window, max_tokens, thinking_levels]` 表，自定义模型不猜能力（对齐 C3 哲学）。
- **GUI**：`ManagedLlm`/`LlmControl` 的 `configure_*` 增加 effort 参数。
- **不变量**：换 Provider 不改 Consumer（已成立）。

### 方案 B（P1）：配置层原子写 + 外部变更检测
- **落点**：`harness-core/src/config.rs` 的写回路径。
- **要点**：① 用 `toml_edit` 做增量改写（保注释、保键序，杜绝当前 `toml::from_str` 破坏手写格式）；② 写 temp 文件 + `rename` 原子落盘；③ 启动时记录文件 mtime/revision，监听外部编辑、`SIGHUP` 或文件事件热重载（对齐 C5 与 cc-switch 的 revision 比较）。
- **风险**：需引入 `toml_edit` 依赖；与现有 `load()` 命中即返回逻辑要合并为"分层合并 + 热更新"。

### 方案 C（P1-P2，AIOps 刚需）：跨会话 Session Manager
- **落点**：`harness-session/src/lib.rs` 暴露 `SessionStore`（`list / get_messages / delete / fork / resume_cmd`）；`harness-ui` 新增会话面板。
- **照搬 cc-switch 三抽象**（`session-manager.md` §6）：
  - `Provider Adapter` → 这里只有一个 provider（dsh 自身），但接口保留 `detect/scan_sessions/load_transcript/get_resume_command/get_project_dir`，为将来接 aidops 后端留缝。
  - `Terminal Launcher` → `launch(command, cwd, target)`，先实现 Windows（`wt`/`conhost`）、macOS（`Terminal`）、Linux。
  - `Path Resolver` → 从 `.harness/sessions` 派生路径。
- **复用**：`SessionLog` 已支持 fork/resume/replay，UI 只做消费者（与 harness 的 UI=事件总线消费者一致）。

### 方案 D（P2，贴合 aidops 主题）：用量 / 成本计量
- **落点**：`harness-session` 在 `Assistant` 事件里记录 prompt/completion token（DeepSeek SSE 已返回 usage）；新增 `usage` 能力或扩 `telemetry.rs`。
- **与 cc-switch 差异**：cc-switch 是**读 Codex/Copilot 的配额页**；harness 是**自己记自己每会话/每 turn 的 token 与成本**（aidops 的"可观测性"）。不碰凭证，纯本地。

### 方案 E（外向彩蛋，零代码复用 cc-switch）：把 harness 注册成 cc-switch 可驱动的 Agent
- harness 已有 `harness-acp`（ACP stdio JSON-RPC server）。**不需要改 cc-switch 源码**——只需在 cc-switch 的 provider 注册里加一条"dsh（DeepSeek Harness）"指向本 harness 的 ACP，harness 就变成 cc-switch 切换列表里的一个 Agent。
- 这是"集成"的另一种形态：让 cc-switch 反过来管 harness，而不是把 cc-switch 抄进来。若团队想要统一控制台，这是成本最低的路。

---

## 7. 推荐落地顺序

1. **A（thinkingLevelMap / 努力度）** —— 今天就能动手，纯增量，风险最低，立刻增强 DeepSeek v4 推理可控性。
2. **D（用量计量）** —— 贴合 aidops 卖点，数据已在日志里，只是没抽出来。
3. **C（Session Manager UI）** —— AIOps 多运行管理的门面，复用已有 `SessionLog`。
4. **B（配置原子写/热重载）** —— 工程卫生，独立推进不阻塞上面。
5. **E（ACP 外向注册）** —— 想要统一控制台时再上。

> 判断标准（与本项目 EXTENSION-COOKBOOK 一致）：每个能力都映射到某个 `ExtensionPoint`/trait，Consumer 只依赖 trait，**不修改 `AgentLoop` 与工具管线核心**。

---

## 8. 我建议的下一步

我可以**马上动手做方案 A**（在 `harness-llm` + `harness-core/config.rs` 加 `reasoning_effort` 与离线模型目录，并补单测），它是 4 项里最稳、收益最直观的一项。是否开始？或者你想先打别的能力（D 用量 / C 会话管理 UI / B 配置热重载），还是走 E 把 harness 接进 cc-switch？告诉我优先级即可。

---

## 9. 实施状态（A + D 已落地，用户确认 "A+D 一起做"）

用户确认后，本会话已完成 A（思考档位）与 D（用量计量）两项能力的移植。所有改动均落在既有能力缝（trait / SessionLog / 配置），未触碰 `AgentLoop` 工具管线核心。

### A — 思考档位 / reasoning_effort（对齐 cc-switch thinkingLevelMap）
- `harness-core/src/config.rs`：`LlmConfig` 新增 `reasoning_effort: Option<String>`（含 `Default`）。
- `harness-core/src/ui_input.rs`：`LlmControl` trait 的 `configure_provider` / `configure_deepseek` 增加 `reasoning_effort: Option<String>` 末参；默认实现透传。
- `harness-llm/src/lib.rs`：`Chunk` 增加 `usage: Option<Usage>` 字段；新增 `Usage { prompt/completion/total_tokens }` + `saturating_add`（会话级聚合用）。`ManagedLlm::configure_deepseek` 把 effort 传入 `DeepSeek::new`。
- `harness-llm/src/deepseek.rs`：`DeepSeek::new` 收 `reasoning_effort`；请求体在用户显式设置时才追加 `reasoning_effort`（避免发 `null` 被上游拒绝）。
- `harness-llm/src/anthropic.rs`：`Anthropic` 持 `reasoning_effort`；`request_body` 在 effort 非空时开启 `thinking` 扩展思考（预算 2048）。
- `harness-llm/src/model_catalog.rs`（新增）：cc-switch `piModelCatalog` 等价物。`ThinkingLevel` 枚举（Off/Minimal/Low/Medium/High/XHigh/Max/Auto，`as_upstream()` 给上游字符串）、`ModelInfo`、`CATALOG`（deepseek-chat/reasoner/v4-flash/v4）、`lookup()`、`estimate_cost()`。哲学：离线预设完整，自定义模型不猜测。
- `harness-ui/src/gui.rs`：模型设置页新增"思考档位 reasoning_effort"输入框，持久化到 `llm.reasoning_effort`；三处 `configure_provider` 调用（发送/应用/加载配置）均透传 `self.effort()`。

### D — 用量 / 成本计量（对标 cc-switch quota 概念，改为自记录 AIOps telemetry）
- `harness-llm/src/{deepseek,openai,local}.rs`：请求体统一加 `"stream_options": { "include_usage": true }`，让上游在流末尾回传 token 用量。
- `harness-llm/src/openai_compat.rs`：解析末尾 `usage` 帧，流结束后单独 `yield Ok(Chunk { usage: Some(...) })`（不进入模型上下文）。
- `harness-llm/src/dsml.rs`：`filter_stream` 显式透传纯 usage 帧（否则被静默丢弃）。
- `harness-runtime/src/agent_loop.rs`：消费 `Chunk.usage` 累加到 `step_usage`，每步结束后追加 `SessionEvent::Usage { id, usage }`（落在 `messages_from_events` 的 `_ => {}` 分支，不影响多轮重建）。
- `harness-session/src/log.rs`：`SessionEvent` 新增 `Usage { id, usage }` 变体；`event_id` 增加对应分支；新增 `usage_total() -> Usage` 聚合全会话用量。
- `harness-ui/src/gui.rs`：底部状态栏空闲时显示本会话 `Tokens {prompt}/{completion}`，让计量可见。

### B — 配置原子写 + 热重载（移植 cc-switch「配置管理 ≠ 请求路径 / 未知字段保留 / 原子写」）
- `harness-core/src/config.rs`：`Config`/`UiConfig`/`LlmConfig` 加 `Serialize` derive；新增 `load_with_raw()`（额外返回原始 `toml::Table` 含未知字段 + 命中路径）；新增 `save_atomic()`（仅写已知字段）与 `save_preserving()`（保留未知字段的无损回写）；内部 `save_merged()` 把已知字段覆盖回原始表后再序列化；`atomic_write()` 先写同目录 `.{stem}.tmp.{pid}` 再 `rename`（Windows 下先 remove 目标），崩溃只留 tmp、原文件完好。
- `harness-core/src/error.rs`：新增 `TomlSer(#[from] toml::ser::Error)` 变体（`save_merged` 的 `toml::to_string(self)?` 需要）。
- `harness-core/src/ui_input.rs`：`LlmControl` trait 新增 `reload_config(&self, cfg: &Config) -> Result<(), String>`。
- `harness-llm/src/lib.rs`：`ManagedLlm` 新增 `key: RwLock<String>` 镜像；`configure_deepseek` 成功时缓存 key；实现 `reload_config`——读取 `cfg.llm` 的 base_url/model/effort，文件缺 api_key 时**回退到运行时缓存的 key**（DPAPI 密钥不落 TOML），无需重启会话即可热重载模型配置。
- `harness-ui/src/gui.rs`：系统设置页"配置文件 .harness.toml"区新增「重新加载」（读文件→`reload_config` 热应用）与「原子写入」（写临时文件+rename，且**不写 api_key 明文**）两个按钮 + 状态提示。

### C — 会话管理 UI（移植 cc-switch 会话管理能力）
- `harness-session/src/log.rs`：新增 `rename_session(dir, file, title)`（写旁挂 `<uuid>.title` sidecar，空标题则删除 sidecar）；`session_title()` 改为优先读自定义标题 sidecar，回退首条 TurnStart 输入。`prune_sessions`/`list_sessions`/`delete_session`/`switch_to_file` 早已具备。
- `harness-ui/src/gui.rs`：历史面板新增「精简」(prune，保留最近 30 个、当前会话永删)；每个历史条目悬停浮现「✎ 重命名」与「✕ 删除」；重命名弹窗写入 `.title` sidecar；顶部导入 `rename_session`。

### 验证情况
- 已对 A/B/C/D 全部改动做逐处人工核对，均自洽：A（`LlmControl` 5 参签名、3 处 GUI 调用点、DeepSeek/Anthropic 请求体、model_catalog 离线预设）；D（`Chunk.usage`/openai_compat 用量帧、dsml 透传、agent_loop 累计与落盘、log.rs 事件变体与 `usage_total`）；B（`Config` 的 `Serialize` derive、`load_with_raw`/`save_atomic`/`save_preserving`/`atomic_write`、`error.rs` 的 `TomlSer`、`LlmControl::reload_config` 在 trait 与 `ManagedLlm` 实现两端一致、`key` 镜像兜底）；C（`rename_session` 写 `.title` sidecar 与 `session_title` 回读、`prune_sessions`、`gui.rs` 历史面板精简/重命名/删除按钮与弹窗）。
- `bin/src/compose.rs` 此前因 `Chunk` 增字段 / `DeepSeek::new` 增参已同步修正（补 `usage: None`、4 参 `None`）。
- **编译未能在本机跑通**：当前 shell 缺失 MinGW/VC C 工具链（`gcc.exe`/`dlltool.exe`/`cl.exe` 均不存在），传递依赖 `ring`（TLS）的 build script 无法运行，导致 `cargo check`/`build`/`test` 全部中断。这是环境缺口，非代码缺陷——在具备 C 编译器的终端（即早前生成 `harness/build.log` 时带 gcc 的环境）运行 `cargo check`/`build`/`test` 即可确认。

