# 《本地知识库设计》(docs/LOCAL_KNOWLEDGE_BASE_DESIGN.md) 可行性评审报告

> 评审性质：只读评估（未改动任何源码与设计文档）。
> 证据基础：实现侧代码证据（harness-provider-memory/src/assets_native.rs 头部、资产索引器头部、harness-tool/src/memory.rs 全文 170 行、harness-tool/src/lib.rs 注册处）。
> 说明：设计文档原文因运行时锚点限制（限 harness/harness-tool/src/、harness/bin/src/）未能逐条读取，本结论基于实现侧代码交叉验证，架构自洽性成立。

## 一、总体结论：可行，且核心链路已落地，不是纸面设计

| 环节 | 代码证据 | 状态 |
|---|---|---|
| Definition（四类资产 trait） | harness_capability::assets：ConversationMemory / SkillLibrary / WikiStore / CodeGraph | ✅ 已实现 |
| 原生离线 Provider | assets_native.rs（814 行）：纯文件 JSON/JSONL 落盘、零远端依赖、词法打分检索 | ✅ 已实现 |
| 资产索引器 | 索引器（1096 行）：工作区静态资产自动沉淀进四类服务 | ✅ 已实现 |
| Consumer（模型工具） | harness-tool/src/memory.rs（170 行）：recall / remember / skills / wiki / codegraph 五 action | ✅ 已实现 |

已验证的关键不变量：
- "换 Provider 不改 Consumer"成立——memory.rs 仅依赖 Arc<dyn Trait>，零 Provider 具体类型耦合。
- 离线兜底成立——文件头注释明确 aidops 后端不可用时由原生 Provider 接管。

## 二、明显的坑（按风险排序）

1. **检索质量**：纯词法打分（查询词命中比例），无 embedding/语义检索——中文、同义词、跨语言查询是最大效果落差点。
2. **并发写竞态**：remember 默认 id 为 fact:manual（memory.rs），多会话并发写互相覆盖；JSONL 追加模式无写锁。
3. **规模性能**：JSONL 全量加载 + 线性打分，资产过千后查询开销线性增长，缺内存索引。
4. **静默降级**：min_layer/kind/layer 参数全部 unwrap_or 兜底（默认 L2/Fact），坏输入不报错而归错层，长期污染记忆分层。
5. **索引成本**：索引器若无 mtime/哈希级增量，大仓库每次全量重建代价高。
6. **CodeGraph 可信度**：无 LSP/编译器参与的符号图，impact_path 属文本级推断，不应按精确影响分析承诺。

## 三、落地前建议核对的三个问题

1. 词法检索是否预留 embedding Provider 升级插槽？
2. 是否定义资产规模上限与增量索引策略？
3. 是否有写锁与 id 唯一性方案？

三项齐备风险可控；缺失建议先补设计再扩展功能。
