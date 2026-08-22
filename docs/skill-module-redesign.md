# 技能（Skill）模块重设计 v2：约定目录 + 自动加载

> 状态：已实现（随本文档同批提交）。
> 相关代码：`harness-capability/src/{assets,index}.rs`、`harness-provider-memory/src/assets_native.rs`、
> `harness-runtime/src/agent_loop.rs`、`harness-ui/src/gui/{memory_panel,settings_view}.rs`、
> `bin/src/compose.rs`、示例资产 `harness/extensions/skill-packs/`。

## 0. 演进背景

v1（上一批）解决了「批量导入 / 存储可视化 / 幂等更新」，但技能身份绑定在
**来源绝对路径**上：文件散落在用户磁盘任意位置，Agent 无法自动发现，
导入体验依赖用户每次手动指定路径。

v2 将模型翻转为「**约定目录 + 自动加载**」：

- 技能包统一住进 `.harness-memory/skills/<包名>/`，目录即身份；
- Agent 启动自动扫描注册，无需任何手动操作；
- 自动加载的技能**默认未启用**，用户必须在 GUI 勾选后才进入匹配链路
  （安全默认：新技能不会未经确认就注入模型上下文）。

## 1. 约定目录与技能包规范

固定存放位置（与 `NativeSkillLibrary` 落盘根目录一致）：

```
<workspace>/.harness-memory/skills/
├── release-checklist/          ← 一个子目录 = 一个技能包
│   ├── SKILL.md                ← 必需：技能入口（大小写不敏感）
│   ├── resources/              ← 可选：资源文件（登记进 Skill.resource_files）
│   │   └── checklist-template.md
│   └── CHANGELOG.md            ← 可选：版本说明（不作为资源登记）
├── sql-review/
│   └── SKILL.md
├── pack_release-checklist.json ← 注册记录（sync 自动维护，勿手改）
└── sp-*.json                   ← 内置 Superpowers 技能记录（不受自动加载管辖）
```

`SKILL.md` 格式（与工作区自动索引共用解析器 `skill_from_markdown`）：

```markdown
# 发布检查清单            ← 技能名称

version: 1.1              ← 可选版本声明（头部块；支持 版本：/ 中英文冒号 / 列表前缀）

## 触发边界                ← match_skills 的匹配依据
发布新版本、上线部署……

## 执行步骤                ← 有序步骤，注入模型上下文
- 跑全量测试

## 验证规则                ← verify_skill 打分依据
- 测试全部通过
```

资源收集规则：包根 + 一层子目录内的普通文件（相对包根，`/` 分隔），
排除 `SKILL.md`、隐藏文件、`CHANGELOG.md` 与噪声目录，上限 32 个。

## 2. 生命周期语义（核心设计）

约定目录是技能包的**唯一事实来源（SSOT）**，记录 id 固定为
`pack:<目录名规整>`（`SKILL_PACK_ID_PREFIX`），与文件位置彻底解耦：

| 事件 | 行为 |
| --- | --- |
| 启动扫描发现新包目录 | 注册记录，**`enabled=false`**（默认未启用，等待用户勾选） |
| 重复扫描 / 重复导入同一包 | **更新内容**，保留用户设置的 `enabled`，绝不产生副本 |
| 用户在面板勾选「启用」 | `set_skill_enabled` 写 JSON；下一回合 `match_skills` 即可命中 |
| 用户在面板禁用 / 删除 | 下一回合立即停止注入；删除连带回收包目录（见下） |
| 包目录被移除（面板删除或资源管理器手动删） | 下次 sync 回收对应 `pack:` 记录，面板不再出现幽灵技能 |

两条删除路径都保证「删得干净、不会复活」：

- 面板「删除」→ `NativeSkillLibrary::delete_skill` 删 JSON 记录，
  且当 `source_path` 落在技能库目录内时**连带 `remove_dir_all` 包子目录**；
- 资源管理器手动删目录 → 下次启动/GUI 刷新时 sync 对账回收记录。

`source_path` 在库外的旧式记录（v1 绝对路径导入）删除时只删 JSON，
不触碰用户原始文件。

**来源路径不落绝对路径**：约定包的 `source_path` 存**相对技能库根的路径**
（JSON 里是 `"source_path": "包名/SKILL.md"` 而非 `F:/…`）——位置由
「库根 + 相对路径」推导，工作区搬迁/拷贝后依然有效；`delete_skill` 与 GUI
展示对相对（新）/绝对（旧式记录）双格式兼容。

## 3. 数据流转图

```
 ① 落盘（任选其一，都汇聚到约定目录）
 ┌─────────────────────────┐   ┌──────────────────────────────┐
 │ GUI「导入技能包(文件夹)」│   │ 用户手动把包目录放进          │
 │ install_skill_packs_from│   │ .harness-memory/skills/       │
 │ （递归发现→整体复制）    │   │ （下次启动自动生效）          │
 └────────────┬────────────┘   └───────────────┬──────────────┘
              ▼                                │
 ┌──────────────────────────────────────┐      │
 │ <ws>/.harness-memory/skills/<包名>/  │◄─────┘
 │   SKILL.md + resources/…             │
 └──────────────┬───────────────────────┘
                │ ② 对账注册（唯一入口 sync_skill_packs）
                │    · compose.rs 启动 spawn（紧随 ensure_builtin_skills）
                │    · GUI 导入完成后立即调用
                ▼
 ┌──────────────────────────────────────────────────────────┐
 │ sync_skill_packs(lib, packs_dir)                         │
 │  新包 → register(enabled=false)　旧包 → 更新且保留开关    │
 │  目录消失的 pack: 记录 → delete_skill 回收                │
 └──────────────┬───────────────────────────────────────────┘
                ▼
 ┌──────────────────────────────┐      同一 Arc<dyn SkillLibrary>
 │ GUI「技能管理」面板           │◄────────────────────┐
 │ 列表展示（含版本/资源/来源）  │                     │
 │ 勾选启用 ⇄ 禁用 / 删除       │                     │
 └──────────────────────────────┘                     │
                                                      │
 ┌──────────────────────────────────────────┐         │
 │ AgentLoop（每回合）                       │─────────┘
 │ match_skills(用户输入)   ← 仅 enabled=true │
 │ → render_skill_instructions（≤4 条）      │
 │ → messages.insert(1, system) 注入模型上下文│
 └──────────────────────────────────────────┘
```

关键不变量（沿用 v1）：GUI 与 Agent 循环持有**同一个
`Arc<dyn SkillLibrary>`**（`compose.rs` 中 `ctx.provide(skill)` 与
`make_ui(skill)` 同源）；`NativeSkillLibrary` 每次调用直读磁盘、无缓存，
因此面板上的启用/禁用/删除**无需重启、下一回合立即生效**。

## 4. API 变更说明

### 4.1 新增

| API | 位置 | 说明 |
| --- | --- | --- |
| `pub const SKILL_PACK_ID_PREFIX` | `index.rs` | 约定目录记录 id 前缀 `"pack:"`；该前缀记录由 sync 全生命周期管辖 |
| `pub async fn sync_skill_packs(lib, packs_dir) -> Result<SkillImportReport>` | `index.rs` | **自动加载对账入口**：注册/更新/回收（启动与 GUI 导入共用） |
| `pub fn install_skill_packs_from(src, packs_root) -> Result<Vec<PathBuf>>` | `index.rs` | 外部目录发现的包整体复制进约定目录（同名先删后拷 = 更新；来源已在库内则免复制） |
| `pub fn install_skill_file_into(file, packs_root) -> Result<PathBuf>` | `index.rs` | 单文件落盘为 `<packs_root>/<文件名>/SKILL.md` |
| `SkillImportReport.deleted` | `index.rs` | 回收数量（对账清理的记录数） |

### 4.2 变更

| 项 | 变更 |
| --- | --- |
| GUI `import_skill_folder` | 「就地解析注册（绝对路径 id）」→「复制进约定目录 + `sync_skill_packs`」 |
| GUI `import_skill_file` | 同上：单文件先落盘成包，再走统一对账 |
| `NativeSkillLibrary::delete_skill` | 删除 JSON 之余，若 `source_path` 在库目录内则连带回收包子目录（相对/绝对路径双兼容） |
| 约定包 `source_path` | 由绝对路径改为**相对技能库根**，不再把磁盘路径写死进 JSON |
| `compose.rs` 启动任务 | `ensure_builtin_skills` 后追加 `sync_skill_packs`，自动加载约定目录 |
| GUI 文案 | 明示「新注册技能包默认未启用，须勾选启用后才参与匹配」 |

### 4.3 不变（兼容性承诺）

- `SkillLibrary` trait 零改动；`Skill` 结构体零改动（v1 的 `source_path` 字段继续承担删除定位职责）；
- `import_skills` / `import_skill_dir` 保留（标注建议改走新链路），既有调用与测试不受影响；
- `bootstrap_assets` 工作区自动索引、内置 Superpowers 注册逻辑不变
  （`.harness-memory` 在 `SKIP_DIRS` 中，约定目录不会被二次索引）。

## 5. 向后兼容策略

1. **旧 JSON 记录（v1 绝对路径 id）**：继续可读、可切换、可删除；删除只移除
   JSON，不触碰外部原始文件。建议用户在面板删除后按新机制重新导入，
   以获得自动加载与目录化管理能力。不做强行迁移，避免破坏用户磁盘文件。
2. **绝对路径 source_path 自动迁移**：已注册的 `pack:` 记录若还存着旧的
   绝对路径，下次启动对账（sync 重写记录）时自动改写为相对路径，无需
   手工清理；非 `pack:` 前缀的旧式记录不受影响。
3. **旧 `.json` 与新包目录共存**：`all_skills` 只读可解析的 JSON 文件，
   子目录自然跳过，无冲突。
4. **默认未启用不是行为回退**：v1 导入即启用；v2 改为导入即禁用。这是
   有意的安全语义变更——未经用户确认的技能不得注入模型上下文。已在库中
   的旧记录状态不受影响。
5. **aidops 远程后端模式**：`sync_skill_packs` 只依赖 `dyn SkillLibrary`，
   远程 Provider 同样适用（本地目录扫描结果注册进远端库）。

## 6. Demo：从文件夹到 Agent 实际调用

### 6.1 GUI 全链路

1. AIOPS Desktop → 设置 → 技能管理，「存储位置」显示
   `<workspace>/.harness-memory/skills`；
2. 点「导入技能包（文件夹）」选择 `harness/extensions/skill-packs/`，
   提示：`已导入技能包：新增 2 个、更新 0 个（发布检查清单、SQL 变更审查）
   ——新增默认未启用，勾选启用后才参与匹配`；
3. 列表出现两条**未勾选**的技能（含版本、资源数、来源路径）；勾选「发布检查清单」；
4. 回到对话输入「明天要发布新版本，帮我过一遍检查」——`match_skills` 命中，
   触发边界/步骤/验证规则作为系统消息注入本轮上下文；
5. 再次导入同一文件夹：提示「更新 2 个」，无副本，启用状态保持；
6. 也可跳过导入：直接把包目录拷进技能目录，重启即自动加载（仍未启用，需勾选）；
7. 面板删除技能：记录与包子目录一并消失，重启不会复活。

### 6.2 自动化回归（等价于上述全链路）

- `agent_loop::tests::imported_skill_pack_flows_into_agent_context`：
  落盘 → sync 默认未启用（匹配为空）→ 勾选启用 → 中文输入命中 →
  渲染断言触发边界/步骤/验证在场 → 禁用立即失效 → 重复同步不产生副本。

## 7. 测试矩阵

| 测试 | 覆盖点 |
| --- | --- |
| `index::tests::sync_skill_packs_defaults_disabled_and_reconciles` | **新核心**：首注册默认未启用；启用后重同步保留状态；目录被删记录被回收 |
| `index::tests::install_skill_packs_copies_into_convention_dir` | 落盘复制（含资源子目录）、重复落盘不产生副本、单文件落盘 |
| `assets_native::tests::delete_skill_removes_convention_pack_dir` | 删除连带回收库内包目录；外部来源文件不被误删 |
| `agent_loop::tests::imported_skill_pack_flows_into_agent_context` | 全链路：落盘 → 自动加载 → 默认禁用 → 勾选 → 中文匹配 → 注入 → 禁用/重复同步 |
| `index::tests::parse_skill_pack_reads_version_resources_and_source` | 包解析：版本/触发/步骤/验证/资源过滤/来源路径 |
| `index::tests::discover_skill_packs_finds_nested_and_skips_noise` | 递归发现：根 + 嵌套包，跳过 `node_modules` |
| `index::tests::import_skills_is_idempotent_and_preserves_enabled` | 旧链路回归：幂等更新不产生副本、禁用状态不被覆盖 |
| `superpowers::tests::real_chinese_inputs_match_builtin_skills` | 既有守卫：中文长句必须命中内置技能（防「占坑不拉」） |

验证命令（Windows，MSVC 目标；cargo 需在 PATH，见 `scripts\build.bat`）：

```
scripts\build.bat test -p harness-capability -p harness-provider-memory -p harness-runtime
```
