//! 审核晋升（Phase 3）：把 `review/` 下的候选事实按门槛晋升为正式事实。
//!
//! 候选事实（Phase 2 产物）默认不进入上下文；只有经过审核并满足门槛才落到
//! `facts/`，从而可被索引器投影。门槛对应设计 §7 晋升表（L1 → L2）：
//! - 至少一个来源指针（`source_refs` 非空）
//! - 正文可独立理解（去掉标题与状态行后 ≥ `MIN_BODY_CHARS` 个非空白字符）
//! - 置信度 ≥ `CONFIDENCE_MIN`（由 [`approve`] 写入；未审核候选不晋升）
//! - 无同义/重复冲突（与已晋升事实及本批次的归一化正文键不同）

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::distill::sanitize_id;
use crate::frontmatter::{parse_markdown, render_markdown, FrontMatter, Layer, Status};

/// L1 → L2 晋升所需最低置信度。
pub const CONFIDENCE_MIN: f32 = 0.70;
/// 正文可独立理解的最小非空白字符数。
pub const MIN_BODY_CHARS: usize = 12;
/// 候选事实目录（Phase 2 产物，不被索引器投影）。
pub const REVIEW_DIR: &str = "review";
/// 晋升后的正式事实目录。
pub const FACTS_DIR: &str = "facts";

/// 单条候选的晋升结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionOutcome {
    pub id: String,
    pub promoted: bool,
    /// 拒绝原因；`promoted == true` 时为空。
    pub reasons: Vec<String>,
}

/// 一次晋升扫描的报告（纯数据，调用方决定如何持久化）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromotionReport {
    pub date: String,
    pub outcomes: Vec<PromotionOutcome>,
    pub promoted_paths: Vec<PathBuf>,
}

impl PromotionReport {
    pub fn promoted_count(&self) -> usize {
        self.promoted_paths.len()
    }

    pub fn rejected_count(&self) -> usize {
        self.outcomes.iter().filter(|o| !o.promoted).count()
    }
}

fn list_md(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("md") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn is_noise_line(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('#') || t.starts_with("> 状态：")
}

/// 归一化正文键：剔除标题/状态行，仅保留字母数字（含中文）并小写，截断 64 字符。
pub fn norm_key(body: &str) -> String {
    let mut out = String::new();
    for line in body.lines() {
        if is_noise_line(line) {
            continue;
        }
        for ch in line.chars() {
            if ch.is_alphanumeric() {
                out.push(ch.to_lowercase().next().unwrap_or(ch));
            }
        }
    }
    out.chars().take(64).collect()
}

fn body_chars(body: &str) -> usize {
    body.lines()
        .filter(|l| !is_noise_line(l))
        .flat_map(|l| l.chars())
        .filter(|c| !c.is_whitespace())
        .count()
}

/// 门槛判定：返回拒绝原因（空表示可晋升）。纯函数、确定性。
pub fn judge_candidate(fm: &FrontMatter, body: &str, existing: &HashSet<String>) -> Vec<String> {
    let mut reasons = Vec::new();
    if fm.source_refs.is_empty() {
        reasons.push("缺少来源指针".into());
    }
    if body_chars(body) < MIN_BODY_CHARS {
        reasons.push(format!("正文不足 {MIN_BODY_CHARS} 字，无法独立理解"));
    }
    if fm.confidence < CONFIDENCE_MIN {
        reasons.push(format!(
            "置信度 {:.2} 低于门槛 {:.2}（未审核）",
            fm.confidence, CONFIDENCE_MIN
        ));
    }
    let key = norm_key(body);
    if key.is_empty() {
        reasons.push("正文为空".into());
    } else if existing.contains(&key) {
        reasons.push("与已晋升事实同义/重复".into());
    }
    reasons
}

fn promoted_body(body: &str, date: &str) -> String {
    let mut out = String::new();
    for line in body.lines() {
        if is_noise_line(line) && line.trim().starts_with("> 状态：") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&format!("\n> 状态：active（Phase 3 审核晋升于 {date}）。\n"));
    out
}

/// 审核动作：为 `review/{id}.md` 写入置信度与 `reviewed` 标记（幂等覆盖）。
pub fn approve(
    knowledge_root: &Path,
    fact_id: &str,
    date: &str,
    confidence: f32,
) -> std::io::Result<PathBuf> {
    let path = knowledge_root
        .join(REVIEW_DIR)
        .join(format!("{}.md", sanitize_id(fact_id)));
    let raw = std::fs::read_to_string(&path)?;
    let parsed = parse_markdown(&raw);
    let mut fm = parsed.front_matter.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("候选事实缺少 front matter: {}", path.display()),
        )
    })?;
    fm.confidence = confidence;
    fm.updated_at = date.to_string();
    if !fm.tags.iter().any(|t| t == "reviewed") {
        fm.tags.push("reviewed".into());
    }
    std::fs::write(&path, render_markdown(&fm, &parsed.body))?;
    Ok(path)
}

/// 扫描 `review/`，把达标候选晋升到 `facts/`（删除原候选文件以保证幂等）。
pub fn promote_review_dir(knowledge_root: &Path, date: &str) -> std::io::Result<PromotionReport> {
    let facts_dir = knowledge_root.join(FACTS_DIR);
    let mut existing: HashSet<String> = HashSet::new();
    for p in list_md(&facts_dir) {
        if let Ok(raw) = std::fs::read_to_string(&p) {
            let parsed = parse_markdown(&raw);
            if let Some(fm) = &parsed.front_matter {
                if fm.status == Status::Active {
                    existing.insert(norm_key(&parsed.body));
                }
            }
        }
    }

    let mut report = PromotionReport {
        date: date.to_string(),
        ..Default::default()
    };
    for path in list_md(&knowledge_root.join(REVIEW_DIR)) {
        let stem = sanitize_id(
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("candidate"),
        );
        let raw = match std::fs::read_to_string(&path) {
            Ok(r) => r,
            Err(e) => {
                report.outcomes.push(PromotionOutcome {
                    id: stem,
                    promoted: false,
                    reasons: vec![format!("读取失败: {e}")],
                });
                continue;
            }
        };
        let parsed = parse_markdown(&raw);
        let mut fm = match parsed.front_matter {
            Some(f) => f,
            None => {
                report.outcomes.push(PromotionOutcome {
                    id: stem,
                    promoted: false,
                    reasons: vec!["缺少 front matter".into()],
                });
                continue;
            }
        };
        let id = if fm.id.is_empty() { stem } else { fm.id.clone() };
        let reasons = judge_candidate(&fm, &parsed.body, &existing);
        if !reasons.is_empty() {
            report.outcomes.push(PromotionOutcome {
                id,
                promoted: false,
                reasons,
            });
            continue;
        }

        fm.layer = Layer::L2;
        fm.status = Status::Active;
        fm.updated_at = date.to_string();
        if fm.importance < 0.5 {
            fm.importance = 0.5;
        }
        if !fm.tags.iter().any(|t| t == "promoted") {
            fm.tags.push("promoted".into());
        }
        let body = promoted_body(&parsed.body, date);
        std::fs::create_dir_all(&facts_dir)?;
        let out = facts_dir.join(format!("{}.md", sanitize_id(&id)));
        std::fs::write(&out, render_markdown(&fm, &body))?;
        existing.insert(norm_key(&body));
        std::fs::remove_file(&path)?;
        report.outcomes.push(PromotionOutcome {
            id,
            promoted: true,
            reasons: Vec::new(),
        });
        report.promoted_paths.push(out);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontmatter::{Kind, Scope};

    fn temp_root(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "harness-promotion-{}-{}-{}",
            tag,
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(dir.join(REVIEW_DIR)).unwrap();
        dir
    }

    fn candidate(id: &str, confidence: f32, body: &str, with_source: bool) -> String {
        let fm = FrontMatter {
            id: id.into(),
            kind: Kind::Fact,
            scope: Scope::Project,
            layer: Layer::L1,
            status: Status::Candidate,
            confidence,
            source_refs: if with_source {
                vec!["session:s1".into()]
            } else {
                Vec::new()
            },
            ..Default::default()
        };
        render_markdown(&fm, body)
    }

    #[test]
    fn promotes_candidate_passing_all_gates() {
        let root = temp_root("pass");
        std::fs::write(
            root.join(REVIEW_DIR).join("fact.a.md"),
            candidate(
                "fact.a",
                0.8,
                "# 事实A\n\nMSVC 工具链编译需要指定 stable-x86_64-pc-windows-msvc。\n",
                true,
            ),
        )
        .unwrap();

        let report = promote_review_dir(&root, "2026-02-14").unwrap();
        assert_eq!(report.promoted_count(), 1);
        assert_eq!(report.rejected_count(), 0);

        let out = root.join(FACTS_DIR).join("fact.a.md");
        assert!(out.exists(), "应晋升到 facts/");
        assert!(!root.join(REVIEW_DIR).join("fact.a.md").exists(), "原候选应移除");
        let parsed = parse_markdown(&std::fs::read_to_string(&out).unwrap());
        let fm = parsed.front_matter.expect("晋升产物应有 front matter");
        assert_eq!(fm.layer, Layer::L2);
        assert_eq!(fm.status, Status::Active);
        assert!(fm.tags.iter().any(|t| t == "promoted"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rejects_low_confidence_no_source_short_body_and_duplicate() {
        let root = temp_root("reject");
        std::fs::write(
            root.join(REVIEW_DIR).join("fact.low.md"),
            candidate("fact.low", 0.3, "# 低置信\n\n这是一条足够长但未审核的候选事实正文。\n", true),
        )
        .unwrap();
        std::fs::write(
            root.join(REVIEW_DIR).join("fact.nosrc.md"),
            candidate("fact.nosrc", 0.9, "# 无来源\n\n这是一条足够长但没有来源指针的正文。\n", false),
        )
        .unwrap();
        std::fs::write(
            root.join(REVIEW_DIR).join("fact.short.md"),
            candidate("fact.short", 0.9, "短\n", true),
        )
        .unwrap();
        // facts/ 已有同义事实（active），用于触发去重门禁
        std::fs::create_dir_all(root.join(FACTS_DIR)).unwrap();
        let existing = FrontMatter {
            id: "fact.dup".into(),
            layer: Layer::L2,
            status: Status::Active,
            ..Default::default()
        };
        std::fs::write(
            root.join(FACTS_DIR).join("fact.dup.md"),
            render_markdown(&existing, "# 已有事实\n\n重复的正文内容用于验证去重门禁。\n"),
        )
        .unwrap();
        std::fs::write(
            root.join(REVIEW_DIR).join("fact.dup2.md"),
            candidate("fact.dup2", 0.9, "# 已有事实\n\n重复的正文内容用于验证去重门禁。\n", true),
        )
        .unwrap();

        let report = promote_review_dir(&root, "2026-02-14").unwrap();
        assert_eq!(report.promoted_count(), 0);
        assert_eq!(report.rejected_count(), 4);
        // 被拒候选必须留在 review/（不静默删除）
        assert_eq!(list_md(&root.join(REVIEW_DIR)).len(), 4);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn approve_writes_confidence_then_candidate_promotes() {
        let root = temp_root("approve");
        std::fs::write(
            root.join(REVIEW_DIR).join("fact.b.md"),
            candidate("fact.b", 0.2, "# 事实B\n\n待审核的足够长正文内容。\n", true),
        )
        .unwrap();

        // 未审核（0.2 < 0.70）不晋升
        let report = promote_review_dir(&root, "2026-02-14").unwrap();
        assert_eq!(report.promoted_count(), 0);

        let path = approve(&root, "fact.b", "2026-02-14", 0.75).unwrap();
        let parsed = parse_markdown(&std::fs::read_to_string(&path).unwrap());
        let fm = parsed.front_matter.unwrap();
        assert!((fm.confidence - 0.75).abs() < 1e-6);
        assert!(fm.tags.iter().any(|t| t == "reviewed"));

        // 审核后可晋升
        let report = promote_review_dir(&root, "2026-02-14").unwrap();
        assert_eq!(report.promoted_count(), 1);
        std::fs::remove_dir_all(&root).ok();
    }
}
