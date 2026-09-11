use std::any::Any;

use harness_core::error::Result;

/// 一条记忆条目。跨会话持久化的键值记忆，按 `scope` 隔离（项目 / 用户 / 会话）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryEntry {
    pub scope: MemoryScope,
    pub key: String,
    pub value: String,
    /// RFC3339 秒级时间戳字符串；骨架用 `String` 以避免引入 chrono 依赖。
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryScope {
    /// 项目级约定（如"本仓库用 redb 不用 SQLite"）。
    Project,
    /// 用户级偏好（如"回复用中文"）。
    User,
    /// 单次会话内的临时记忆（进程内有效）。
    Session,
}

impl MemoryScope {
    pub fn dir_name(&self) -> &'static str {
        match self {
            MemoryScope::Project => "project",
            MemoryScope::User => "user",
            MemoryScope::Session => "session",
        }
    }
}

/// 记忆机制能力（Definition）。借鉴 Codex 的跨会话记忆：
/// 代理把"学到的偏好 / 项目约定"写入记忆，后续会话检索复用。
///
/// 与 `SessionLog`（运行时真相源，单会话、只追加）正交——记忆是 *跨会话*、*可检索* 的持久层。
pub trait Memory: Any + Send + Sync + 'static {
    /// 写入 / 更新一条记忆（按 scope+key upsert）。
    fn write(&self, entry: MemoryEntry) -> Result<()>;
    /// 读取某 scope 下的全部记忆。
    fn read(&self, scope: MemoryScope) -> Result<Vec<MemoryEntry>>;
    /// 子串检索（骨架为朴素子串；可换 fts / 向量索引）。
    fn search(&self, query: &str) -> Result<Vec<MemoryEntry>>;
}

// ---------------------------------------------------------------------------
// Phase 5 / 坑 1：可插拔语义召回插槽（Definition 层）
//
// 边界：词法通道保持现状（零依赖、离线、可解释）作为默认召回；语义通道只是
// **可选增强**——provider 不可用时必须返回空，融合公式自然退化为纯词法排名，
// 因此"不配置 embedding 时检索行为与现状完全一致"。
// ---------------------------------------------------------------------------

/// 召回来源标注：每条结果都要能说明"它是怎么被召回的"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchedBy {
    /// 仅词法通道命中。
    Lexical,
    /// 仅语义通道命中。
    Semantic,
    /// 两个通道都命中。
    Both,
}

impl MatchedBy {
    pub fn as_str(self) -> &'static str {
        match self {
            MatchedBy::Lexical => "lexical",
            MatchedBy::Semantic => "semantic",
            MatchedBy::Both => "both",
        }
    }
}

/// 融合后的一条召回结果。
#[derive(Debug, Clone, PartialEq)]
pub struct RecallHit {
    /// 跨通道对齐用的稳定标识（事实 id / 页面 id / 路径）。
    pub id: String,
    /// RRF 融合分：`Σ 1/(k + rank)`，越大越靠前。
    pub score: f64,
    pub matched_by: MatchedBy,
}

/// 可插拔 embedding provider 插槽。原生默认实现为 [`NoopEmbedding`]；
/// 后续可换本地 ONNX 模型或 aidops 后端，调用方代码不变。
pub trait EmbeddingProvider: Any + Send + Sync + 'static {
    /// provider 标识，用于结果溯源与配置回显。
    fn name(&self) -> &'static str;
    /// 是否可用于召回（模型未加载 / 后端未配置时为 false）。
    fn available(&self) -> bool;
    /// 按相似度降序返回候选 id；`available() == false` 时必须返回空。
    fn recall(&self, query: &str, limit: usize) -> Vec<String>;
}

/// 默认实现：永远不可用、永远返回空 → 检索退化为纯词法（回归零破坏）。
pub struct NoopEmbedding;

impl EmbeddingProvider for NoopEmbedding {
    fn name(&self) -> &'static str {
        "noop"
    }

    fn available(&self) -> bool {
        false
    }

    fn recall(&self, _query: &str, _limit: usize) -> Vec<String> {
        Vec::new()
    }
}

/// RRF 融合常数（坑 1 指定 k=60）。
pub const RRF_K: f64 = 60.0;

/// 倒数排名融合（RRF）：`score = Σ 1/(k + rank)`，`rank` 为通道内 0 起始排名。
///
/// - 语义通道为空 → 结果与词法通道同序（纯词法退化）；
/// - 两通道都命中 → `matched_by = Both`，分数叠加自然上浮；
/// - 同分时按 id 字典序稳定排序，保证结果可复现。
pub fn rrf_merge(lexical: &[String], semantic: &[String], limit: usize) -> Vec<RecallHit> {
    let mut scores: std::collections::HashMap<String, (f64, MatchedBy)> =
        std::collections::HashMap::new();

    for (channel, ids) in [(MatchedBy::Lexical, lexical), (MatchedBy::Semantic, semantic)] {
        for (rank, id) in ids.iter().enumerate() {
            if id.is_empty() {
                continue;
            }
            let entry = scores.entry(id.clone()).or_insert((0.0, channel));
            entry.0 += 1.0 / (RRF_K + rank as f64);
            entry.1 = match (entry.1, channel) {
                (MatchedBy::Both, _) => MatchedBy::Both,
                (a, b) if a == b => a,
                _ => MatchedBy::Both,
            };
        }
    }

    let mut hits: Vec<RecallHit> = scores
        .into_iter()
        .map(|(id, (score, matched_by))| RecallHit { id, score, matched_by })
        .collect();
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
    hits.truncate(limit);
    hits
}

#[cfg(test)]
mod embedding_tests {
    use super::*;

    fn ids(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn noop_provider_is_unavailable_and_returns_empty() {
        let noop = NoopEmbedding;
        assert_eq!(noop.name(), "noop");
        assert!(!noop.available());
        assert!(noop.recall("任意查询", 10).is_empty());
    }

    #[test]
    fn empty_semantic_channel_degrades_to_pure_lexical_order() {
        let hits = rrf_merge(&ids(&["a", "b", "c"]), &[], 10);
        assert_eq!(
            hits.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
        assert!(hits.iter().all(|h| h.matched_by == MatchedBy::Lexical));
        assert!((hits[0].score - 1.0 / RRF_K).abs() < 1e-12);
    }

    #[test]
    fn both_channel_hit_is_marked_and_floats_to_top() {
        // b 在词法里排第 2、在语义里排第 1 → 叠加后应超过只被词法命中的 a。
        let hits = rrf_merge(&ids(&["a", "b"]), &ids(&["b", "c"]), 10);
        assert_eq!(hits[0].id, "b");
        assert_eq!(hits[0].matched_by, MatchedBy::Both);
        assert_eq!(hits[1].id, "a");
        assert_eq!(hits[1].matched_by, MatchedBy::Lexical);
        assert_eq!(hits[2].id, "c");
        assert_eq!(hits[2].matched_by, MatchedBy::Semantic);
    }

    #[test]
    fn limit_truncates_and_matched_by_is_explainable() {
        let hits = rrf_merge(&ids(&["a", "b", "c"]), &[], 2);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].matched_by.as_str(), "lexical");
        assert_eq!(MatchedBy::Both.as_str(), "both");
        assert_eq!(MatchedBy::Semantic.as_str(), "semantic");
    }
}
