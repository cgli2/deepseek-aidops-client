//! 内存倒排索引（Phase 6 / 坑 3：规模性能——全量加载与线性扫描）。
//!
//! 设计边界（见 `docs/LOCAL_KNOWLEDGE_BASE_DESIGN.md` §11 坑 3）：
//! - 查询先走倒排取**候选集**，再对候选集精排，扫描范围从全量降到候选集；
//! - 写时增量更新：单条写入只影响其涉及的 term 集合，不重建全索引；
//! - 容量护栏：单个倒排项（分片）超过上限时显式告警，提示归档策略；
//! - 零第三方依赖：分词自带 ASCII 词 + CJK 单字/二元组，离线可用、可解释。

use std::collections::{BTreeSet, HashMap};

/// 默认单分片容量护栏（超过即告警，提示按时间窗口归档）。
pub const DEFAULT_SHARD_CAP: usize = 100_000;

/// 判断字符是否属于需要按字/二元组切分的 CJK 范围。
fn is_cjk(ch: char) -> bool {
    matches!(ch as u32,
        0x3040..=0x30FF   // 日文假名
        | 0x3400..=0x4DBF // CJK 扩展 A
        | 0x4E00..=0x9FFF // CJK 基本区
        | 0xAC00..=0xD7AF) // 韩文音节
}

/// 零依赖分词：ASCII 按字母数字切词并小写；CJK 产出单字与相邻二元组。
///
/// 返回值已排序去重，代表该文本的 term 集合。
pub fn tokenize(text: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut cjk: Vec<char> = Vec::new();

    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            flush_cjk(&mut cjk, &mut terms);
            word.push(ch.to_ascii_lowercase());
        } else if is_cjk(ch) {
            flush_word(&mut word, &mut terms);
            cjk.push(ch);
        } else {
            flush_word(&mut word, &mut terms);
            flush_cjk(&mut cjk, &mut terms);
        }
    }
    flush_word(&mut word, &mut terms);
    flush_cjk(&mut cjk, &mut terms);

    terms.sort();
    terms.dedup();
    terms
}

fn flush_word(word: &mut String, terms: &mut Vec<String>) {
    if !word.is_empty() {
        terms.push(std::mem::take(word));
    }
}

fn flush_cjk(cjk: &mut Vec<char>, terms: &mut Vec<String>) {
    if cjk.is_empty() {
        return;
    }
    for ch in cjk.iter() {
        terms.push(ch.to_string());
    }
    for pair in cjk.windows(2) {
        terms.push(pair.iter().collect::<String>());
    }
    cjk.clear();
}

/// term → 记录 id 集合的内存倒排索引，并保留 id → term 集合以支持增量更新。
#[derive(Debug, Clone)]
pub struct InvertedIndex {
    postings: HashMap<String, BTreeSet<String>>,
    doc_terms: HashMap<String, BTreeSet<String>>,
    shard_cap: usize,
}

impl Default for InvertedIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl InvertedIndex {
    /// 使用默认容量护栏创建空索引。
    pub fn new() -> Self {
        Self::with_shard_cap(DEFAULT_SHARD_CAP)
    }

    /// 指定容量护栏创建空索引（`shard_cap` 至少为 1）。
    pub fn with_shard_cap(shard_cap: usize) -> Self {
        Self {
            postings: HashMap::new(),
            doc_terms: HashMap::new(),
            shard_cap: shard_cap.max(1),
        }
    }

    pub fn shard_cap(&self) -> usize {
        self.shard_cap
    }

    /// 已索引的记录数。
    pub fn doc_count(&self) -> usize {
        self.doc_terms.len()
    }

    /// 倒排项（term）数量。
    pub fn term_count(&self) -> usize {
        self.postings.len()
    }

    /// 当前全部 term（字典序），供审计与统计输出。
    pub fn terms(&self) -> Vec<String> {
        let mut out: Vec<String> = self.postings.keys().cloned().collect();
        out.sort();
        out
    }

    /// 写入或更新一条记录：只调整该 id 涉及的 term 集合（坑 3 验收 2）。
    ///
    /// 同 id 重复写入时，旧文本独有的 term 会被清理，不残留脏倒排项。
    pub fn insert(&mut self, id: &str, text: &str) {
        if id.is_empty() {
            return;
        }
        let next: BTreeSet<String> = tokenize(text).into_iter().collect();
        let prev = self
            .doc_terms
            .insert(id.to_string(), next.clone())
            .unwrap_or_default();

        for term in prev.difference(&next) {
            self.drop_posting(term, id);
        }
        for term in next.difference(&prev) {
            self.postings
                .entry(term.clone())
                .or_default()
                .insert(id.to_string());
        }
    }

    /// 删除一条记录及其全部倒排项引用。
    pub fn remove(&mut self, id: &str) {
        if let Some(prev) = self.doc_terms.remove(id) {
            for term in &prev {
                self.drop_posting(term, id);
            }
        }
    }

    /// 候选集：命中任一查询 term 的 id，按命中 term 数降序、id 字典序稳定排序。
    ///
    /// 调用方只需对返回的候选集精排，不必扫描全量记录。
    pub fn candidates(&self, query: &str) -> Vec<String> {
        let mut hits: HashMap<&str, usize> = HashMap::new();
        for term in tokenize(query) {
            if let Some(ids) = self.postings.get(&term) {
                for id in ids {
                    *hits.entry(id.as_str()).or_insert(0) += 1;
                }
            }
        }
        let mut out: Vec<(usize, String)> = hits
            .into_iter()
            .map(|(id, matched)| (matched, id.to_string()))
            .collect();
        out.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        out.into_iter().map(|(_, id)| id).collect()
    }

    /// 容量护栏：倒排项内 id 数超过 `shard_cap` 的分片，按规模降序返回（坑 3 验收 3）。
    pub fn oversized_shards(&self) -> Vec<(String, usize)> {
        let mut out: Vec<(String, usize)> = self
            .postings
            .iter()
            .filter(|(_, ids)| ids.len() > self.shard_cap)
            .map(|(term, ids)| (term.clone(), ids.len()))
            .collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out
    }

    fn drop_posting(&mut self, term: &str, id: &str) {
        if let Some(ids) = self.postings.get_mut(term) {
            ids.remove(id);
            if ids.is_empty() {
                self.postings.remove(term);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_covers_ascii_and_cjk() {
        let terms = tokenize("ULID fact:01 记忆晋升");
        assert!(terms.contains(&"ulid".to_string()));
        assert!(terms.contains(&"fact".to_string()));
        assert!(terms.contains(&"01".to_string()));
        // CJK 单字 + 二元组
        assert!(terms.contains(&"记".to_string()));
        assert!(terms.contains(&"记忆".to_string()));
        assert!(terms.contains(&"晋升".to_string()));
        // 去重且有序
        let mut sorted = terms.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(terms, sorted);
    }

    #[test]
    fn candidates_shrink_scan_range_to_matching_docs() {
        let mut idx = InvertedIndex::new();
        for i in 0..200 {
            idx.insert(&format!("fact:{i:03}"), &format!("common token payload {i}"));
        }
        idx.insert("fact:zebra-a", "zebra striped archive");
        idx.insert("fact:zebra-b", "zebra migration note");
        assert_eq!(idx.doc_count(), 202);

        let hits = idx.candidates("zebra");
        assert_eq!(hits, vec!["fact:zebra-a".to_string(), "fact:zebra-b".to_string()]);
        // 候选集远小于全量：精排只需扫 2 条而非 202 条
        assert!(hits.len() * 10 < idx.doc_count());
        // 未命中任何 term 时返回空，调用方自然退化为“无结果”，不做全量兜底扫描
        assert!(idx.candidates("不存在的词xyz").is_empty());
    }

    #[test]
    fn candidates_rank_by_matched_term_count() {
        let mut idx = InvertedIndex::new();
        idx.insert("a", "index checkpoint");
        idx.insert("b", "index checkpoint mtime hash");
        // b 命中 4 个 term、a 命中 2 个 → b 排前
        assert_eq!(idx.candidates("index checkpoint mtime hash")[0], "b");
    }

    #[test]
    fn single_insert_updates_only_its_own_terms() {
        let mut idx = InvertedIndex::new();
        idx.insert("a", "alpha beta");
        let before = idx.terms();
        idx.insert("b", "gamma delta");
        let after = idx.terms();

        // 写入 b 只新增 b 自己的 term，a 的倒排项不被重建
        let added: Vec<&String> = after.iter().filter(|t| !before.contains(t)).collect();
        assert_eq!(added, vec![&"delta".to_string(), &"gamma".to_string()]);
        assert!(before.iter().all(|t| after.contains(t)));
        assert_eq!(idx.term_count(), before.len() + 2);
    }

    #[test]
    fn reinsert_prunes_stale_terms_and_remove_clears_postings() {
        let mut idx = InvertedIndex::new();
        idx.insert("a", "alpha beta");
        idx.insert("a", "alpha gamma");
        assert!(!idx.terms().contains(&"beta".to_string()));
        assert!(idx.terms().contains(&"gamma".to_string()));
        assert_eq!(idx.candidates("beta").len(), 0);
        assert_eq!(idx.doc_count(), 1);

        idx.remove("a");
        assert_eq!(idx.doc_count(), 0);
        assert_eq!(idx.term_count(), 0);
        assert!(idx.candidates("alpha").is_empty());
    }

    #[test]
    fn oversized_shard_guard_reports_over_cap() {
        let mut idx = InvertedIndex::with_shard_cap(1);
        idx.insert("a", "shared unique_a");
        idx.insert("b", "shared unique_b");
        idx.insert("c", "shared unique_c");

        let over = idx.oversized_shards();
        assert_eq!(over[0], ("shared".to_string(), 3));
        assert!(over.iter().all(|(_, n)| *n > idx.shard_cap()));

        // 默认护栏下同样数据不应告警
        let mut relaxed = InvertedIndex::new();
        relaxed.insert("a", "shared");
        relaxed.insert("b", "shared");
        assert!(relaxed.oversized_shards().is_empty());
        assert_eq!(relaxed.shard_cap(), DEFAULT_SHARD_CAP);
    }

    #[test]
    fn empty_id_and_empty_text_are_ignored_safely() {
        let mut idx = InvertedIndex::new();
        idx.insert("", "some text");
        idx.insert("a", "");
        assert_eq!(idx.doc_count(), 1);
        assert_eq!(idx.term_count(), 0);
        assert!(idx.candidates("").is_empty());
    }
}
