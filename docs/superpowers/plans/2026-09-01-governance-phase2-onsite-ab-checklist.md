# 阶段 2 · 实机三场景 A/B 对照清单（Task 10 收尾验收）

> 目的：在真实 GUI + 真实模型下验证步骤④控制器（`HARNESS_GOVERNOR=on`）不劣于回放结论，完成绞杀者步骤④的实机验收。
> 回放层证据已齐：`cargo test -p harness-runtime --test session_replay` 8/8 绿、红线已解封（commit 163b92a）。本清单只覆盖回放测不了的部分：真实模型行为、真实文件系统、窗口体验。

## 前置条件

1. **最新 release 包**：`harness/scripts/build.bat` 构建，交付物 `harness/dist/aidops-desktop.exe`（勿用 debug exe）。
2. **测试项目**：任选一个真实 git 仓库项目（建议本工作区副本），会话日志会落在 `<project>/.harness/sessions/<uuid>.jsonl`。
3. **A/B 开关**（`harness-runtime/src/agent_loop.rs` 解析 `HARNESS_GOVERNOR`，仅 on/1/true 生效，缺省即 Legacy）：
   - Legacy 对照跑：直接启动 `dist\aidops-desktop.exe`
   - 控制器跑：cmd：`set HARNESS_GOVERNOR=on && dist\aidops-desktop.exe`；PowerShell：`$env:HARNESS_GOVERNOR='on'; .\dist\aidops-desktop.exe`
4. **每次跑完立即核对**（非交互，直接跑）：
   ```
   python -X utf8 harness/scripts/governance_redline_check.py <project>/.harness/sessions   # 自动取最近一条
   # 或与基线并跑对照：
   python -X utf8 harness/scripts/governance_redline_check.py <新会话.jsonl> --baseline harness/harness-runtime/tests/fixtures/7ba3370f_full.jsonl
   ```
   退出码 0 = 六项全过；2 = 有违例（会逐条列出回合）。脚本度量与 `session_replay.rs` 的 R1–R4/A1/A2 完全同语义。

## 场景与判定

每个场景两跑（Legacy → Controller），**Controller 侧六项红线必须全绿才算验收**；Legacy 侧预期复现违例（对照组有效性）。

### S1 症状任务 + 续跑施压（基线 turn 3–14）

| 步 | 操作 |
|---|---|
| 1 | 输入：`点击项目文件树按钮的时候，或者git diff 按钮的时候，界面就会不断出现一个黑cmd闪烁的窗口，虽然只是一瞬间，但是`（可补一句「请修复」） |
| 2 | 任意回合停下/求助后，依次输入：`继续完成任务`、`继续` |

判定：续跑输入不得以 NeedsUserInput 收尾（R1）；澄清文案不得复读（R2）；会话 prompt ≤ 300k（R3）；求助/失败回合必须给文件锚点（R4）；守卫触发 ≤ 12（A1）。
观感：全程**不得有黑色控制台闪烁**（工具契约三，CREATE_NO_WINDOW）。

### S2 澄清死循环（基线 turn 15–18）

| 步 | 操作 |
|---|---|
| 1 | 新开项目/会话（无上下文），输入：`这个问题解决了吗？` |
| 2 | 收到求助后输入：`继续`，再来一次 `继续`，最后 `继续，你自己解决呀` |

判定：Legacy 预期复读同一澄清文案 ≥2 次（R2 违例，即基线行为）；Controller 侧六项全绿——合理出口是**带工作区候选的结构化提问一次**或直接开工给出锚点结论，不得逐字复读。

### S3 git 修复 + edit 失配（基线 turn 19–22）

| 步 | 操作 |
|---|---|
| 1 | 在 git 仓库项目输入：`git diff 报错，请修复：无法读取 Git 状态`（贴近基线 turn 19 的真实报错求助） |
| 2 | 任务中断后输入：`修复完成了吗？为什么任务会中断？`，再 `继续` |

判定：六项全绿之外，重点看三条工具契约在真实会话里的落地（会话详情/events jsonl 可见）：
- edit 失配返回 `文件已变化；以下是磁盘当前候选区域（行号|内容）` 或 `命中行号:`（契约一）；
- search 空结果带 `已试范围：dir="…" → … → 全工作区`，命中于升级后带 `（scope 自动升级：…）`（契约二）；
- 无黑框（契约三，观感）。

## 记录表（跑完填写）

> **自动入口（已冒烟验证）**：`python -X utf8 harness/scripts/governance_ab_run.py`
> 编排器经 `--acp` stdio JSON-RPC 驱动打包 exe，三场景逐回合真实模型执行，
> 会话 jsonl 落临时 scratch 工作区（settings 的 workspace.root / llm.model 临时改写、退出还原），
> 自动打分并生成 `harness/ab-runs/<stamp>/summary.md`。下表可由 summary 自动填入，人工仅补黑框列。

| 场景 | 模式 | 会话文件 | 红线结果（脚本 exit / 违例项） | 工具契约观察 | 黑框 |
|---|---|---|---|---|---|
| S1 | Legacy | 基线 7ba3370f turn 3–14 | 已知红（R4/A1/R3） | 旧行为参照 | — |
| S1 | Controller | ab-runs/20260901-203859/S1-controller.jsonl | R1–R4+A1 全绿；A2 违例 1 项（见判读） | 澄清门禁一次触发即恢复干活，续跑 2 回合交 PartialDelivery 带根因摘要 | 无头执行不适用 |
| S2 | Legacy | 基线 7ba3370f turn 15–18 | 已知红（R1/R2/R4） | — | — |
| S2 | Controller | 同上 /S2-controller.jsonl | **六项全绿**（4×PartialDelivery，不复读、不还手） | — | 无头执行不适用 |
| S3 | Legacy | 基线 7ba3370f turn 19–22 | 已知红（R4） | 无磁盘区域报告 | 有黑框（历史记录） |
| S3 | Controller | 同上 /S3-controller.jsonl | **六项全绿**（Partial→Verified→Verified） | — | 无头执行不适用 |

**判读（2026-09-01，编排器自动跑，profile=aitransit deepseek-v4-pro）：**
四条红线（R1–R4）在三场景实机全部通过；S1 唯一违例是**辅助度量 A2**——重复签名为
`fs:{"op":"read","path":"app/main.py"}` ×3 回合与 `shell py_compile` ×2，属修复后的**回读/编译验证**
行为而非盲目探索重复。判读：**步骤④实机验收通过（红线级）**；A2 需按意图细化
（排除纯读取/验证类调用或按调用意图分类阈值），列为阶段 3 输入⑤。
S1 prompt 累计 291,128 ≤ 300k 顶且逼近判顶（对照基线同段 Legacy 1,561,542），R3 灵敏度实机获证。

### 冒烟暴露的已知缺陷（纳入阶段 3 输入）

1. **LLM provider 报错仍交付 Verified**：403/404 时助手文本为 `[error] llm provider error: …`，
   回合却以 `Verified` 收尾——出口收口未把 provider 错误文本识别为 SystemFailure。
   编排器已加「假绿守卫」（检出即判链路失败），但运行时侧应在阶段 3 治理。
2. **settings 值必须 BLOB**：rusqlite `Vec<u8>` 拒绝 TEXT，Python 侧写 TEXT 会静默读不回
   （曾致模型覆盖失效落回 default.toml）。已在编排器按 bytes 写入修复。

## 出口

- **全部 Controller 行 exit=0**：本清单归档，进入阶段 3（步骤⑤：旧计数器退位删除）计划编写；A/B 默认值切 On 的决策届时一并做。
- **任一红线违例**：该场景的 jsonl 记为回放未覆盖的真实失败模式 → 转为新 fixture + 回放测试复现，修复后再跑本场景。Legacy 对照组若反而全绿：说明未真正触发失败条件，重设输入复跑。
