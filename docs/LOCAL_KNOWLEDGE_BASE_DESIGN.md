# 自循环本地知识库体系设计

> 状态：设计方案；未实现运行逻辑。  
> 范围：Harness 及后续接入的本地项目。  
> 目标：将代码、文档、会话、开发日志和技能沉淀为可检索、可验证、可演化的本地知识资产，使智能体在工作前获得必要上下文、在工作后积累可信经验。

## 1. 设计结论

项目已具备可复用的资产边界：

- `ConversationMemory`：L0–L3 对话记忆生命周期；
- `SkillLibrary`：带触发条件、步骤和验证规则的可执行经验；
- `WikiStore`：结构化知识页面与页面链接；
- `CodeGraph`：代码符号、调用关系与影响路径；
- `harness-capability::index`：技能、Markdown 和源码扫描入口。

因此不建立平行的记忆服务，而是在上述接口之上建立 **Memory Loop**：`.harness-memory/` 是唯一知识根；Markdown/JSONL 是可读、可版本化的真相源；现有四类资产是运行时查询投影；闭环为：

```text
捕获 → 提炼 → 去重/脱敏/校验 → L0-L3 晋升 → 混合检索 → 验证反馈
```

原始会话和模型推测不能直接成为长期事实。每一项长期资产必须具备来源、置信度、范围、状态与失效规则。

## 2. PageIndex 思想的本地化

采用 PageIndex 的“先树形导航、后按需展开”思想，不将文档切成脱离语义的碎片。

| 索引视图 | 内容 | 目的 |
| --- | --- | --- |
| 目录树 | 项目、分类、页面、标题层级、摘要 | 快速缩小知识域 |
| 实体关系 | 事实、技能、页面、文件、符号、会话的引用 | 回答原因、依赖和影响 |
| 倒排索引 | 标题、标签、关键词、路径、符号名、摘要 | 确定性快速召回 |

查询采用“**目录优先、证据展开**”：先返回候选节点的摘要、来源、新鲜度和评分理由；只在任务需要时展开原文片段、会话证据或源码。向量检索仅作为后续可选召回，不取代目录、实体和来源索引。

## 3. 本地知识根

```text
.harness-memory/
├── README.md                     # 维护约定与项目身份
├── config.toml                   # 保留策略、权重、脱敏规则
├── inbox/                        # 待分类捕获内容
├── sessions/YYYY/MM/             # L1 会话摘要和来源指针
├── journal/YYYY/MM/              # 每日开发日志
├── facts/
│   ├── user.jsonl                # 跨项目个人偏好
│   ├── projects/<project>.jsonl  # 项目事实、约束、决策
│   └── shared.jsonl              # 已审核跨项目知识
├── wiki/                         # 架构、决策、运行手册、领域知识
├── skills/
│   ├── project/<project>/<id>/SKILL.md
│   └── shared/<id>/SKILL.md
├── code/                         # 模块地图、变更摘要
├── review/                       # 候选事实和冲突项
├── archive/                      # 已替代、冷存档内容
└── index/                        # 可再生 catalog、倒排、关系、checkpoint
```

建议提交 Markdown、JSONL 和技能包到 Git；`index/` 作为可再生缓存，按项目选择是否提交。

## 4. 统一元数据

Markdown 使用 YAML front matter；JSONL 使用同名字段：

```yaml
id: decision.memory-storage-001
kind: decision                  # fact | preference | decision | wiki | skill | journal | session
scope: project                  # session | project | user | shared
project_id: harness
layer: L2                       # L0 | L1 | L2 | L3
status: active                  # candidate | active | superseded | archived
created_at: 2026-09-01T12:00:00Z
updated_at: 2026-09-01T12:00:00Z
confidence: 0.90
importance: 0.80
freshness_days: 180
tags: [memory, architecture]
entities: [ConversationMemory, WikiStore]
source_refs:
  - session:<id>#turn-12
  - git:<commit>
  - file:harness/harness-capability/src/assets.rs#L20-L60
supersedes: []
```

规则：

- 没有 `source_refs` 的内容只能处于 `candidate`；
- `project` 默认隔离，`user` 可跨项目，`shared` 必须审核；
- `superseded` 内容保留审计链，但不进入默认上下文；
- 内容过期后进入复核队列，而非直接删除。

## 5. L0–L3 生命周期

| 层级 | 生命周期 | 内容 | 晋升条件 |
| --- | --- | --- | --- |
| L0 工作记忆 | 当前会话 | 用户请求、工具摘要、计划、临时约束 | 会话结束后产生摘要候选 |
| L1 情景记忆 | 天至周 | 会话摘要、任务结果、失败原因、开发日志 | 有可复用事实、偏好、决策或经验 |
| L2 长期语义 | 月至长期 | 去重后的事实、偏好、项目约束、决策 | 主题稳定且经验证可升 L3 |
| L3 程序性资产 | 长期 | Wiki、技能、代码关系、运行手册 | 有验证、边界与失效条件 |

### 闭环

1. 任务开始：按项目范围检索活跃偏好、近期决策、相关技能、知识目录和代码关系。
2. 任务进行：由 `SessionLog`、`ConversationMemory::record_turn` 保存 L0；工具结果只保存摘要、路径和哈希。
3. 任务结束：生成 L1 摘要，包含目标、改动、验证、阻塞、下一步、候选事实及来源。
4. 同日会话聚合到开发日志，按“完成、决策、问题、下一步”组织。
5. 候选进入 `review/`；去重、冲突检测和来源校验后才晋升 L2。
6. 重复成功且边界明确的流程沉淀为 `SKILL.md`；稳定知识沉淀为 Wiki；源码变化更新 `CodeGraph`。
7. 验证成功提升置信度；验证失败或用户修正形成反证，降级或替代旧内容。

可提取：用户明确且长期有效的偏好、代码/测试/Git 支持的项目事实、已确认决策、已验证的可重复流程。

不得提取：密钥、令牌、密码、私密信息、模型推测、一次性细节、未经验证错误、被用户撤回内容。

## 6. 索引和检索

### 增量索引

扩展 `harness-capability::index`，使其：

1. 扫描 `skills/**/SKILL.md` 并同步 `SkillLibrary`；
2. 扫描 `wiki/`、`journal/`、`sessions/`，解析元数据、标题树、链接、实体并投影到 `WikiStore`；
3. 扫描 `facts/**/*.jsonl` 并投影 L2 到 `ConversationMemory`；
4. 沿用已有源码扫描更新 `CodeGraph`；
5. 使用内容哈希、修改时间和解析器版本维护 checkpoint，未变化文件跳过；
6. 建立页面、事实、技能、文件、符号之间的关系索引。

跳过 `.git`、依赖、构建目录、二进制和超大文件，并复用已有安全上限。

### 查询流程

1. 默认限定当前 `project_id`，仅在需要时扩展到 `user` / `shared`；
2. 用标题、标签、实体、路径、日期召回不超过 8 个目录节点；
3. 以关键词、实体、关系距离、置信度、重要性、时间衰减混合排序；
4. 每节点最多展开一个摘要及两个来源片段；代码问题优先调用 `CodeGraph`；
5. 上下文按“用户偏好 → 项目约束/决策 → 匹配技能 → 直接证据”截断；
6. 返回项必须带 `id`、摘要、来源、评分理由和新鲜度。

初始评分：

```text
score = 0.35 * lexical + 0.20 * entity_match + 0.15 * relation_proximity
      + 0.15 * confidence + 0.10 * importance + 0.05 * freshness
```

## 7. 质量门禁

| 晋升 | 最低门槛 |
| --- | --- |
| L0 → L1 | 有明确目标、结果、来源指针 |
| L1 → L2 | 可独立理解；至少一个来源；无同义冲突；置信度 ≥ 0.70 |
| L2 → L3 Wiki | 有结构化标题和来源，完成一次复核 |
| L2 → L3 Skill | 有触发边界、步骤、验证规则；至少成功验证一次；明确不适用情形 |
| project → shared | 跨项目验证或用户明确批准，且不含项目私有事实 |

冲突时不可覆盖旧事实。按 `kind + scope + 规范化主体` 形成冲突组，降低自动注入优先级，写入 `review/`；经用户确认或新强证据后将旧项标记为 `superseded`，以 `supersedes` 连接替代记录。

## 8. 集成点

| 现有组件 | 集成方式 | 最小新增能力 |
| --- | --- | --- |
| `SessionLog` | 回合结束生成 L1 摘要、来源指针 | `SessionDistiller` 异步任务 |
| `ConversationMemory` | 承载 L0、L2 | 元数据扩展或 sidecar 索引 |
| `SkillLibrary` | 同步 `.harness-memory/skills/` | 候选、审核状态、验证历史 |
| `WikiStore` | 页面、日志、摘要的结构化投影 | 元数据和目录解析 |
| `CodeGraph` | 写入文件/符号引用关系 | 关系索引写入器 |
| `harness-capability::index` | 统一的增量入口 | `index_memory_root` 与 checkpoint |
| `MemoryTool` | 提供 capture、recall、review、promote | 兼容扩展子命令 |
| GUI 记忆面板 | 浏览、审核、修订、删除、溯源 | L0–L3、scope、status、冲突筛选 |

## 9. 分阶段落地与验收

### Phase 0：知识根与规范

交付：目录模板、README、配置、日志/会话摘要模板、元数据约定。  
验收：可手工新增日志、决策页面和项目技能，且均有统一元数据及来源。

### Phase 1：静态增量索引

交付：知识根扫描、目录/倒排/关系索引、hash checkpoint、统计。  
验收：变更一个 Wiki 或技能后仅处理变化文件；可通过标题、标签、实体、路径、符号定位来源。

### Phase 2：会话与日志闭环

交付：L1 摘要、每日开发日志聚合、候选事实队列。  
验收：完成一次任务后可见摘要、日志和带来源候选；未审核候选不进入默认上下文。

### Phase 3：审核、晋升和技能化

交付：冲突检测、审核队列、事实晋升、技能候选、验证反馈。  
验收：重复成功流程可形成项目技能；失败能降低关联资产置信度；错误事实不会静默覆盖。

### Phase 4：跨项目个性化

交付：项目身份、用户偏好中心、共享知识审核、跨项目技能。  
验收：用户偏好跨项目生效；项目私有内容不泄漏；共享技能有来源和验证历史。

### Phase 5：可选本地语义召回

交付：可插拔 embedding 索引与混合排序。  
验收：关闭该能力后目录与关键词检索仍完整可用；启用后只提高召回，不削弱可解释性。

## 10. 首个实现切片

优先完成 Phase 0 + Phase 1 的最小切片，不要一开始引入自动提炼或向量数据库：

1. 新建 `.harness-memory/` 模板和本规范；
2. 为 Wiki、日志、会话摘要定义统一 front matter；
3. 扩展索引器扫描知识根并维护 checkpoint；
4. 将页面投影到既有 `WikiStore`，复用 UI 与 `MemoryTool` 查询链路；
5. 为增量跳过、目录优先查询、来源关系建立测试。

基础索引与审计链稳定后，再逐步接入自动会话提炼、审核闭环和可选语义召回。这能让系统在“越用越懂用户”的同时，始终保持知识可信、可解释和可回滚。

## 11. 六个关键坑的解决方案

> 本章针对评审（`docs/LOCAL_KNOWLEDGE_BASE_REVIEW.md`）指出的六个风险坑，逐一给出设计约定与验收标准。所有方案均落在既有四层架构（Definition → Provider → Indexer → Consumer）内，不推翻已落地链路。

### 坑 1：检索质量——纯词法打分无语义能力

**问题**：原生 Provider 检索用“查询词在条目文本中的命中比例”打分，中文分词、同义词、跨语言查询效果差。

**方案：混合检索管道 + 可插拔 Embedding 插槽**

- 词法通道保留现状（零依赖、离线可用、可解释），作为默认召回。
- 新增 `EmbeddingProvider` trait 插槽（Definition 层），原生实现为 `NoopEmbedding`（永远返回空，自动退化为纯词法），可选实现为本地 ONNX 模型或 aidops 后端。
- 双通道召回结果用 RRF（倒数排名融合）合并：`score = Σ 1/(k + rank_i)`，`k=60`；向量通道不可用时公式自然退化为纯词法排名。
- 排序理由保持可解释：每条结果附 `matched_by`（lexical / semantic / both）。

**验收**：
1. 不配置 embedding 时，检索行为与现状完全一致（回归零破坏）；
2. 配置本地 embedding 后，同义词/中文改写查询的 Top-5 命中率相对纯词法基线可量化提升；
3. 每条结果均携带 `matched_by` 标注。

### 坑 2：并发写竞态——默认 id 覆盖与无锁追加

**问题**：`remember` 默认 id 为 `fact:manual`，多会话并发写互相覆盖；JSONL 追加无写锁，存在交错行。

**方案：ULID 默认 id + 原子写 + 文件锁**

- id 策略：默认 id 改为 `fact:{ulid}`（时间有序、全局唯一）；需要 upsert 语义时必须显式传入业务 key，且对既有 id 做存在性检查——覆盖前先读旧值并返回 diff 摘要。
- 写路径：全量 JSON 文件采用 temp 文件 + 原子 rename；JSONL 追加在锁内执行 write + flush + fsync。
- 锁机制：知识根下 `.harness-memory/.lock` 哨兵文件，`create-new` 语义获取、持锁 PID 写入、进程退出自动释放；获取失败时指数退避重试（上限 5s），超时返回明确错误而非静默丢写。
- 陈旧锁检测：持锁 PID 不存活时判定为死锁残留，安全接管。

**验收**：
1. 8 并发写者各写 100 条事实，最终 800 条零丢失、零覆盖、零交错行；
2. 同一业务 key 并发 upsert，最终值为其中一次完整写入（非拼接损坏）；
3. 杀死持锁进程后，新写者在退避窗口内成功接管锁。

### 坑 3：规模性能——全量加载与线性扫描

**问题**：JSONL 全量加载 + 逐条线性打分，资产过千后每次查询开销线性增长。

**方案：内存倒排索引 + 分片存储 + 延迟构建**

- 启动时一次构建内存倒排索引（term → 记录 id 集合），查询先走索引取候选集，再对候选集精排，扫描范围从全量降到候选集。
- 存储 按 `kind` × `layer` 分片（如 `facts_L2.jsonl`、`sessions_L1.jsonl`），`recall` 只加载目标分片。
- 惰性加载：索引常驻内存，原文档体（长文本字段）按需读取；会话证据只在展开时读原文。
- 写时增量更新索引：单条写入只影响其涉及的 term 集合，不重建全索引。
- 容量护栏：单分片超过 10 万条时告警并提示归档策略（L1 会话摘要按时间窗口归档）。

**验收**：
1. 10 万条事实分片下，`recall` P95 延迟 < 100ms（本地 SSD）；
2. 单条写入不触发全量重建（索引更新为 O(变更条目数)）；
3. 与线性扫描基线对比，查询耗时随资产量的增长曲线由线性降为近常数（候选集规模受控）。

### 坑 4：静默降级——参数 unwrap_or 兜底归错层

**问题**：`min_layer` / `kind` / `layer` 参数全部 `unwrap_or` 兜底（默认 L2/Fact），非法输入不报错而是归错层，长期污染记忆分层。

**方案：严格校验 + 显式错误返回**

- 枚举参数（`kind` / `layer`）解析失败时直接返回结构化错误（`invalid_parameter: 期望值列表`），不再默认兜底。
- 可选参数缺省时用**文档化的默认值**并在结果中回显实际生效值（`applied_filters`），让调用方可发现默认行为。
- 写入路径（`remember`）强制要求 `kind` 显式给出，无默认；`layer` 未给出时按 `kind` 的规范映射推导并在结果中回显。
- 原生 Provider 与 aidops 后端 Provider 共用同一份校验规则（Definition 层提供 `validate_query` / `validate_write`），保证跨 Provider 行为一致。

**验收**：
1. 传入非法 `kind` / `layer`，工具返回错误而非静默成功；
2. 每次查询/写入结果均含 `applied_filters` 回显；
3. 两 Provider 对同一非法输入返回一致错误。

### 坑 5：索引成本——全量重建代价高

**问题**：索引器若无增量策略，大仓库每次启动或触发都全量扫描重建。

**方案：mtime+hash 双级 checkpoint 增量**

- 一级过滤：文件 mtime + size 与 checkpoint 比对，未变直接跳过（零读取成本）；
- 二级确认：mtime 变化但内容 hash 未变的（如 touch、格式化无实质变化），跳过重建。
- checkpoint 落盘 `.harness-memory/index/checkpoint.json`，记录每个文件的 `mtime / size / hash / last_indexed_at`。
- 变更传播最小化：只对变化文件重新解析并更新其涉及的目录项、倒排项与关系边，其余索引结构不动。
- 失效兜底：checkpoint 损坏或版本不匹配时回退全量重建一次，并输出统计（扫描数/变更数/耗时）供审计。

**验收**：
1. 修改 1 个 Wiki 文件后重建，只处理该文件（处理数 = 1）；
2. `touch` 不改内容的文件不触发重建；
3. 10 万文件仓库二次启动（无变更）索引耗时 < 5s；损坏 checkpoint 自动全量重建成功且统计正确。

### 坑 6：CodeGraph 可信度——无 LSP 的文本级推断

**问题**：本地符号图无编译器参与，`impact_path` 属文本级推断，误报漏报不可控，不应按精确影响分析承诺。

**方案：分级置信标注 + 声明式优先 + 调用方确认**

- 每条关系边携带 `confidence` 与 `source`：`declared`（显式声明：mod/use/impl 显式语法，置信高）> `inferred`（文本匹配推断：调用点正则/标识符匹配，置信中）> `heuristic`（启发式：文件名/命名约定关联，置信低）。
- `impact_path` 返回结果按最低边置信度分层呈现：路径全为 `declared` 时标注“高置信”；含 `inferred` 边时标注“含推断，需验证”；含 `heuristic` 边时仅作提示不作结论。
- `callers_of` / `callees_of` 结果逐条标注来源行号，允许智能体跳转源码自行确认——**把验证成本显式暴露给调用方，而不是伪装成确定性**。
- 能力边界写入工具描述：`impact_path` 文档明确“文本级推断，非编译器级分析；重构/宏/动态分发场景可能失真”，引导对高风险变更补跑测试。
- 升级路径：预留 LSP Provider 插槽（复用现有 `harness-capability/src/lsp.rs` 能力），接入后 `declared` 边占比提升，`heuristic` 边逐步退役。

**验收**：
1. 任意 `impact_path` 结果均含置信标注与来源行号；
2. 含 `heuristic` 边的路径不出现确定性表述；
3. 接入 LSP Provider 后同一查询的 `declared` 边占比可测量提升，且结果不劣化。

### 落地顺序建议

六坑按依赖关系分两批实施：

- **第一批（正确性优先）**：坑 4（严格校验）→ 坑 2（写锁与唯一 id）→ 坑 6（置信标注）。先保证写入的数据干净、可信，否则后续一切索引与检索都在污染数据上运行。
- **第二批（性能与能力）**：坑 5（增量索引）→ 坑 3（倒排索引）→ 坑 1（embedding 插槽）。在数据正确的基础上优化检索体验。

每坑独立可验收，允许按此顺序渐进合入，不阻塞既有 Phase 0–5 计划。

## 12. 首个实现切片：Phase 0 + Phase 1 代码落点（执行契约）

本章把设计落成可编译、可验证的最小代码单元。所有改动限定在两个 crate：`harness-capability`（front matter、索引器、checkpoint）与 `harness-provider-memory`（原生 Provider 的 id 与写锁）。

### 12.1 新增模块清单

| 文件 | 职责 | 对应阶段/坑 |
|---|---|---|
| `harness/harness-capability/src/frontmatter.rs` | 统一 front matter 值对象、Markdown 解析/渲染、`.harness-memory/` 模板生成 | Phase 0 |
| `harness/harness-capability/src/index.rs`（增量改造） | 扫描前读 checkpoint、仅处理变更文件、知识根投影到 WikiStore、完成后写回 checkpoint | Phase 1 / 坑 5 |
| `harness/harness-provider-memory/src/assets_native.rs`（增量改造） | 事实 id 改 `fact:{ulid}`、写入加 `.lock` 文件锁（PID 检测 + 指数退避） | 坑 2 |
| `harness/harness-capability/tests/incremental_index.rs` | 增量索引行为测试 | Phase 1 |

### 12.2 关键接口签名（编译契约）

```rust
// frontmatter.rs
pub enum Layer { L0, L1, L2, L3 }
pub enum Kind { Fact, Preference, Decision, Wiki, Skill, Journal, Session }
pub enum Scope { Session, Project, User, Shared }
pub enum Status { Candidate, Active, Superseded, Archived }

pub struct FrontMatter {
    pub id: String, pub kind: Kind, pub scope: Scope, pub project_id: String,
    pub layer: Layer, pub status: Status,
    pub created_at: String, pub updated_at: String,
    pub confidence: f32, pub importance: f32, pub freshness_days: u32,
    pub tags: Vec<String>, pub entities: Vec<String>,
    pub source_refs: Vec<String>, pub supersedes: Vec<String>,
}

pub struct ParsedMarkdown { pub front_matter: Option<FrontMatter>, pub body: String, pub warnings: Vec<String> }
pub fn parse_markdown(text: &str) -> ParsedMarkdown;
pub fn render_markdown(fm: &FrontMatter, body: &str) -> String;
pub fn harness_memory_template() -> Vec<(String, String)>;
```

```rust
// index.rs 增量：checkpoint
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct IndexCheckpoint { pub files: std::collections::HashMap<String, u64> } // path -> mtime_secs
pub fn load_checkpoint(root: &Path) -> IndexCheckpoint;
pub fn save_checkpoint(root: &Path, ck: &IndexCheckpoint) -> io::Result<()>;
// bootstrap_assets 内部：仅处理 mtime 变更或新出现的文件，结束后 save_checkpoint。
```

```rust
// assets_native.rs 坑 2：单调 id + 文件锁
fn next_fact_id() -> String;              // 返回 format!("fact:{}", ulid::Ulid::new())
fn with_lock<T>(root: &Path, f: impl FnOnce() -> T) -> T; // 获取 .harness-memory/.lock（PID 检测 + 指数退避，上限 5s）
```

### 12.3 行为不变量（测试锚点）

1. **front matter 容错**：YAML 头解析失败不致命，返回 `warnings` 且按无头处理（坑 4：显式回显，不静默降级）。
2. **checkpoint 增量**：两次连续索引，第二次仅处理 mtime 变更的文件；checkpoint 文件写回 `.harness-memory/index/checkpoint.json`。
3. **知识根投影**：`.harness-memory/wiki/**.md` 经 `parse_markdown` 解析后写入 `WikiStore`，front matter 中的 `id/kind/tags/entities` 优先于正文推断。
4. **id 单调唯一**：同一进程并发生成的事实 id 不重复、字典序与时间序一致（ULID）。
5. **写锁互斥**：并发写入同一资产文件时，后到请求等待锁释放后成功，不产生撕裂写。

### 12.4 验证命令

```bash
cargo +stable-x86_64-pc-windows-msvc check  --manifest-path harness/Cargo.toml -p harness-capability
cargo +stable-x86_64-pc-windows-msvc check  --manifest-path harness/Cargo.toml -p harness-provider-memory
cargo +stable-x86_64-pc-windows-msvc test   --manifest-path harness/Cargo.toml -p harness-capability frontmatter
cargo +stable-x86_64-pc-windows-msvc test   --manifest-path harness/Cargo.toml -p harness-capability --test incremental_index
```

全部通过即视为首个实现切片（Phase 0 + Phase 1）落地完成，并同时覆盖坑 2、坑 4、坑 5 的最小可行方案。

## 13. Phase 4 接线契约：晋升管线 → runtime 调用方（待实施）

现状（已验证）：`harness-capability/src/promotion.rs` 的 `approve` / `judge_candidate` /
`promote_review_dir` 单测全绿（`cargo +stable-x86_64-pc-windows-msvc test --manifest-path
harness/Cargo.toml -p harness-capability promotion` → 3 passed），但仓库内**零调用方**：
`promote_review_dir` 只在 promotion.rs 自身出现；`harness-runtime` 也不依赖 `harness-capability`。

唯一可用接线点是组合根 `harness/bin/src/compose.rs`——它已经在此加载知识根下的技能包
（`harness_capability::index::sync_skill_packs(&*skill, &cwd.join(".harness-memory").join("skills"))`）。

| 落点 | 改动 | 验收 |
| --- | --- | --- |
| `harness/harness-capability/src/promotion.rs` | 新增零依赖日期助手 `civil_from_days` / `date_from_secs` / `today()`（`harness-bin` 无 chrono，而晋升报告需要 `YYYY-MM-DD`） | `date_from_secs(0) == "1970-01-01"`；`today().len() == 10` |
| `harness/bin/src/compose.rs` | 提出 `kb_root = cwd.join(index::KNOWLEDGE_ROOT)` 并随技能包任务一起 move；在 `sync_skill_packs` 之后调用 `promotion::promote_review_dir(&kb_root, &promotion::today())`，`promoted_count() > 0` 时打印“事实晋升（Phase 4）：晋升 N 条到 facts/，拒绝 M 条”，出错只告警不阻断启动 | `cargo +stable-x86_64-pc-windows-msvc check --manifest-path harness/Cargo.toml -p harness-bin` 通过；已 `approve(≥0.70)` 的候选启动后从 `review/` 移到 `facts/`（`layer: L2`、`status: active`、`tags: promoted`），未审核候选保持原位 |

约束：晋升只在启动后台任务里跑一次且幂等；绝不把未审核候选（confidence 仍为默认值）提升为正式事实。

## 14. Phase 5 接线契约：可插拔语义召回插槽（坑 1，Definition 层已落地）

现状（已验证）：`harness-capability/src/memory.rs` 已具备语义召回插槽与融合公式，
`cargo +stable-x86_64-pc-windows-msvc test --manifest-path harness/Cargo.toml -p harness-capability embedding` → 4 passed。

| 落点 | 改动 | 验收 |
| --- | --- | --- |
| `harness/harness-capability/src/memory.rs` | 新增 `MatchedBy`（lexical/semantic/both）、`RecallHit{id,score,matched_by}`、`EmbeddingProvider` trait（`name/available/recall`）、默认实现 `NoopEmbedding`（永不可用、永返回空）、`RRF_K = 60.0` 与 `rrf_merge(lexical, semantic, limit)`（`Σ 1/(k+rank)`，同分按 id 字典序稳定排序） | 语义通道为空时结果与词法通道**同序**（纯词法退化）；双通道命中者 `matched_by = Both` 且分数叠加上浮；`limit` 截断生效；`NoopEmbedding.available() == false` |
| 剩余接线（未实施） | 原生 provider 召回路径把词法 id 列表与 `embedding.recall()` 结果交给 `rrf_merge`，并把 `matched_by` 回写到结果元数据 | 未配置 embedding 时逐条结果与现状一致（回归零破坏）；配置本地 embedding 后每条结果都能说明"它是怎么被召回的" |

约束：语义通道只是**可选增强**——provider 不可用必须返回空，绝不因缺模型而降低或阻断词法召回。

## 15. Phase 6 接线契约：内存倒排索引（坑 3，Definition 层与接线均已落地）

现状（已验证）：`harness-capability/src/inverted.rs` 提供零依赖分词与倒排索引，
`cargo +stable-x86_64-pc-windows-msvc test --manifest-path harness/Cargo.toml -p harness-capability inverted` → 7 passed；
`-p harness-capability` 全量 40 passed（复核基线见 §16）。

| 落点 | 改动 | 验收 |
| --- | --- | --- |
| `harness/harness-capability/src/inverted.rs` | `tokenize`（ASCII 小写切词 + CJK 单字/二元组，排序去重）；`InvertedIndex{new, with_shard_cap, insert, remove, candidates, oversized_shards, doc_count, term_count, terms}`；`DEFAULT_SHARD_CAP = 100_000` | 查询走倒排取候选集：202 条记录中 `candidates("zebra")` 只返回 2 条（精排范围下降两个数量级），未命中返回空且不做全量兜底扫描；单条 `insert` 只新增自身 term，不重建全索引；同 id 重写清理旧 term（`beta` 消失、`remove` 后 `term_count == 0`）；`with_shard_cap(1)` 下超限分片按规模降序告警 |
| 接线（已实施） | `index.rs` 知识根索引与 provider 召回改为"先 `candidates()` 取候选集 → 再对候选集精排"；写路径（新增/变更/删除文件）调用 `insert` / `remove` 做增量维护 | 索引常驻内存后召回不再全量加载；写单条文件的耗时与全库规模解耦。已验证：`index::knowledge_tests`（`recall_only_fetches_candidates` 只精排候选集、`deleted_file_drops_postings_and_checkpoint` 删除即摘除倒排项、`sync_maintains_inverted_index_incrementally` 写路径增量维护）与 `assets_native` 的 `recall_uses_inverted_candidates_with_incremental_write_path` 均在测试套件内；`-p harness-capability` 40 passed，`check -p harness-bin` 全依赖链通过 |

约束：倒排索引是**纯内存派生结构**，可随时由知识根重建，不作为事实来源，不新增落盘格式。
