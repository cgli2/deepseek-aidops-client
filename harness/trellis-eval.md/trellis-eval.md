# Trellis 引入评估（mindfold-ai/Trellis）

> 评估对象：https://github.com/mindfold-ai/Trellis.git
> 结论：**可做成 Rust 原生自定义插件引入，不破坏原有架构；默认关闭、按需开启。**

## 1. Trellis 核心思想与能力

Trellis 把"项目规格（spec）"当作第一类对象，用**任务状态机**（新任务 / 进行中 / 完成）驱动开发循环：

- **Spec 驱动**：项目规格是持续可见的权威上下文，LLM 以规格为准推进开发。
- **任务状态机**：维护任务清单文件，跟踪每个任务的生命周期（new → in_progress → done）。
- **围绕开发意图**：关注"要做什么、做到哪一步"，而非"怎么执行"（执行交给下层工具链）。

## 2. 本平台（harness）已有能力对照

| Trellis 能力 | 平台对应物 | 关系 |
|---|---|---|
| Spec 注入（持续上下文） | `harness-runtime` agent_loop 的 PreStep 消息链 | **复用扩展点**：以 PreStep 瀑布中间件改写系统消息，不触碰执行内核 |
| 任务状态机 | `harness-runtime` 的 task/session（执行编排） | **不同抽象层次**：平台管"执行编排"，Trellis 管"开发意图"，通过 PreStep 桥接 |
| 任务清单文件 | `harness-provider-memory` 的 wiki/skill 资产 | 互补：Trellis 是轻量 JSON 状态文件，memory 是语义化知识库 |

## 3. 重叠 / 冲突评估

- **与 `harness-provider-hook` 不冲突**：hook 是外部命令（进程边界），Trellis 是进程内数据面注入。
- **与 task/session 不冲突**：Trellis 不改执行内核，只注入消息与维护任务文件，失败静默不阻断循环。
- **与 memory provider 不冲突**：领域不同（轻量状态 vs 语义知识）。
- **架构影响**：零内核改动，只新增一个 provider crate + 一个配置项，注册在既有 `Plugin` 抽象上。

## 4. 引入方式

- crate：`harness-provider-trellis`（本 workspace 成员）
- 配置：`[trellis] enabled = false`（默认关闭，未启用时零副作用）
- 装配：`bin/src/main.rs` 插件列表追加 `TrellisPlugin::new(config.trellis.clone())`
- 开关：改配置文件 `enabled = true` 并填 `spec_file` / `tasks_file` 路径即启用

## 5. 验证

- `cargo check -p harness-provider-trellis -p harness-bin` 通过（无新增警告）。
- 默认 `enabled = false`：`register` 返回空注册表，行为与未引入前完全一致。
