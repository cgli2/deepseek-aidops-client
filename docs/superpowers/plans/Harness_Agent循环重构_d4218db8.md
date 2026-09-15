# Harness Agent 循环与长周期任务能力重构

## 根因诊断

| 症状 | 根因 | 位置 |
|---|---|---|
| 回复混乱、DSML 原文裸显 | DeepSeek-v4 以 DSML 文本输出工具调用，harness 只解析 OpenAI 式 `delta.tool_calls`，content 透传入库入屏；`should_offer_tools` 关键词门控常不传 tools | `harness-llm/src/openai_compat.rs`、`deepseek.rs` |
| 30 分钟无响应 | SSE 无 idle 超时（半开连接永久阻塞）；`reasoning_content` 思考期被丢弃、UI 无反馈；`busy` 常真 | `harness-llm/src/openai_compat.rs`、`harness-runtime/src/controller.rs` |
| 无法多步规划 | 8 步硬上限；模型可见工具仅 fs/edit/shell；`Subagent` 能力未暴露为工具；无 plan 工具；重启不恢复旧日志 | `agent_loop.rs`、`compose.rs`、`harness-session/src/log.rs` |

## 一、harness-llm：DSML 解析与流式加固

### 1. 新增 `harness-llm/src/dsml.rs`（核心）
- `DsmlFilter` 增量状态机：`push(text) -> Vec<DsmlItem>`、`finish() -> Vec<DsmlItem>`；`DsmlItem = Text(String) | Call(ToolCall)`。
  - Normal 态：缓冲可能是标记前缀的尾巴（`<｜DSML｜` 与 ASCII 变体 `<|DSML|` 均识别，防跨帧截断）；其余文本立即 `Text` 产出。
  - 捕获态：见到 `<｜DSML｜tool_calls>` 后收集至 `</｜DSML｜tool_calls>`；逐块解析 `<｜DSML｜invoke name="X">…</｜DSML｜invoke>`；参数 `<｜DSML｜parameter name="N" string="true|false">V</｜DSML｜parameter>`，`string="true"` 作字符串、`false` 作 JSON 字面量（失败回退字符串）。
  - 每个 invoke 闭合即产出 `Call`（流式尽早执行，不等流结束）。
- 原生工具名映射 `map_native(name, args) -> ToolCall`：`exec_command/run_command/bash→shell{command}`（取 `cmd`/`command`）；`read_file→fs{op:read}`；`write_file/create_file→fs{op:write}`；`list_directory/list_files→fs{op:list}`；`edit_file/str_replace→edit{path,old_text,new_text}`（兼容 `old_str/new_str`）；未知名透传（registry 返 unknown，模型自纠）。id 用 `dsml-{counter}`。
- `pub fn filter_stream(ChunkStream) -> ChunkStream`：包装任意 Provider 流，文本过 `DsmlFilter`，`Call` 转 `Chunk{tool_calls}`。
- `pub fn strip_dsml(&str) -> String`：移除完整 DSML 块与尾部不完整标记（GUI 防御渲染旧日志用）。
- 单元测试：跨帧拆分标记、exec_command 映射、string="false" JSON 参数、未闭合尾缀 hold、strip_dsml。

### 2. `openai_compat.rs`
- `stream_chat` 内部流改名为 inner，返回 `dsml::filter_stream(inner)`（DeepSeek/OpenAI/Local 全覆盖）。
- 解析 `delta.reasoning_content` → 产出 `Chunk{reasoning: Some(..)}`。
- **idle 超时**：`events.next()` 包 `tokio::time::timeout`（默认 120s，env `HARNESS_STREAM_IDLE_SECS` 覆盖），超时 yield `Err(Llm("模型服务长时间无响应…"))`。`Cargo.toml` 增加 `tokio = { version = "1", features = ["time"] }`（或 workspace 依赖）。

### 3. `lib.rs` / `deepseek.rs`
- `Chunk` 增加 `reasoning: Option<String>`（`#[serde(default, skip_serializing_if)]`）并 `derive(Default)`；全仓 17 处字面量补 `..Default::default()`。
- 删除 `should_offer_tools` 关键词门控：**恒传 tools + `tool_choice: "auto"`**（coding agent 定位）。
- `coding_tools()` 增加 `plan`、`delegate` 两个 schema（见下）。

## 二、harness-session：事件扩展与恢复

`log.rs`：
- 新增事件变体：`Thinking{id,text}`、`PlanUpdate{id,items:Vec<PlanItem>}`（`PlanItem{text,status: String}`，status ∈ pending/doing/done）、`StepStart{id,step}`、`StepEnd{id,step}`。
- 新增 `SessionLog::open_latest(dir) -> Arc<Self>`：扫 `*.jsonl` 取 mtime 最新；逐行 `serde_json` 解析（坏行跳过）重建 events/next-id，复用该文件追加；若尾事件非 `TurnEnd` 补一条 `TurnEnd`（闭合中断回合）。无文件则等同 `persistent` 新建。

## 三、harness-runtime：生命周期与 watchdog

### `agent_loop.rs`
- **结构化系统提示词**（替换现单段英文）：① 语言跟随用户；② 工具契约——仅用提供的 fs/edit/shell/plan/delegate，**严禁在正文输出任何工具标记/DSML/XML invoke**，普通对话直接答；③ 工作流——复杂任务先 `plan` 立计划→逐步执行→独立耗时子任务 `delegate`→结尾简洁总结；④ 输出格式 markdown、结论先行。
- Turn/Step 生命周期：每步写 `StepStart/StepEnd`；`reasoning` chunk 写 `Thinking` 事件（不进模型上下文，`messages_from_events` 忽略）。
- 步上限改 env `HARNESS_MAX_STEPS`（默认 24）。
- `messages_from_events` 保持合并逻辑，忽略新事件变体。

### `controller.rs`
- turn 级 watchdog：`run_turn_cancellable` 外套 `tokio::time::timeout`（env `HARNESS_TURN_TIMEOUT_SECS`，默认 1800），超时写 `[error] 回合超时` + `TurnEnd`，确保 `busy` 复位（杜绝 30 分钟假死）。

## 四、harness-tool：模型可见的规划与委托工具

- 新增 `src/plan.rs`：`PlanTool{log: Arc<SessionLog>}`，name=`plan`，args `{items:[{text,status?}]}`；校验后 append `PlanUpdate`，返回带编号计划回显。
- 新增 `src/delegate.rs`：`DelegateTool{sub: Arc<dyn Subagent>}`，name=`delegate`，args `{task}`；调 `spawn` 返回子代理终稿（仅依赖 capability Definition，符合三角色）。
- `lib.rs` 导出；`compose.rs` 注册两工具（PlanTool 需 log、DelegateTool 需 subagent，调整装配顺序）。

## 五、harness-ui：可见性渲染

`gui.rs` `poll_log`：
- `Thinking` → 状态行/灰色气泡「思考中…」（增量覆盖，不刷屏）。
- `PlanUpdate` → 计划气泡「[计划] 1. … 2. …」带状态符。
- `ToolCall` → 「[工具] name：参数摘要(≤120字)」；`ToolResult` → 「[结果] 前 200 字」——agent 行为全程可见。
- `append_assistant` 先过 `harness_llm::dsml::strip_dsml` 防御旧日志；`Cargo.toml` 增加 `harness-llm` 依赖。

## 六、装配与恢复入口

`compose.rs`：`SessionLog::persistent(...)` → `SessionLog::open_latest(...)`（重启自动恢复最近会话；GUI「新会话」按钮仍走 `clear()`）。

## 七、测试与验证

1. 单元：dsml 解析/映射/strip；session `open_latest` 中断恢复；plan/delegate 工具。
2. 集成：`harness-runtime/tests` 增用例——自定义 Provider 经 `filter_stream` 产出 DSML 文本流，断言工具被执行、debt 续跑、Thinking 不入模型上下文。
3. `cargo test --workspace` + `cargo build --release`，按交付规范复制 `dist/harness.exe`（release package）。
4. 手工验证清单：GUI 发「检查项目结构」→ 看到 [工具]/[结果] 气泡与连贯中文总结，无 DSML 裸显；断网/停滞 120s 报错而非假死；重启 GUI 历史会话恢复。

## 假设与边界

- DSML 标记以截图实际格式 `<｜DSML｜…>` 为准，兼容 `<|DSML|` ASCII 变体。
- 不引入新外部 crate（除 tokio time feature，workspace 已有 tokio）。
- `PreStep`/waterfall、沙箱、hook 管线保持不变；改动均落在 Consumer 可见行为与 Provider 流层，符合「换 Provider 不改 Consumer」不变量。

## 交付状态（补记）

- 实现随计划一并入库推送：`17465e2 fix(runtime): 统一任务策略到求解图，纯核验任务凭命令结果收口`（3 files，+199/-61）。
- 本文档自身入库提交：`4559172 docs(runtime): 归档 Harness Agent 循环重构计划文档`。
- 推送核对：远端 `refs/heads/main` 与本地 HEAD 一致（`git ls-remote` 实测同一哈希，`origin/main...HEAD` 双向计数 0 0）。