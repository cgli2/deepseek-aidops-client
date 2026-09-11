//! 会话与日志闭环（Phase 2）：从 `SessionEvent` 流确定性蒸馏三类知识产物。
//!
//! 对应 docs/LOCAL_KNOWLEDGE_BASE_DESIGN.md §9 Phase 2 验收：
//! 「完成一次任务后可见摘要、日志和带来源候选；未审核候选不进入默认上下文」。
//!
//! - L1 会话摘要：`conversations/{date}/session-{id}.md`（kind: session, layer: L1, status: active）
//! - 候选事实队列：`review/{id}.md`（kind: fact, layer: L2, status: candidate）
//! - 每日开发日志：`conversations/{date}/journal.md`（kind: journal, layer: L1, status: active）
//!
//! 纯离线、零 LLM 的规则蒸馏（方案 A）：确定性、幂等、可回放；LLM 语义化提炼
//! 属 Phase 5。`review/` 已被 `index_knowledge_base` 排除投影，保证未审核候选
//! 不进入默认上下文（Phase 2 验收项）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use harness_llm::Usage;
use harness_session::{DeliveryOutcome, DeliveryReport, SessionEvent};

use crate::frontmatter::{render_markdown, FrontMatter, Kind, Layer, Scope, Status};

/// 摘要截断长度（chars），控制产物体积。
const GOAL_PREVIEW_LEN: usize = 200;
const TEXT_PREVIEW_LEN: usize = 400;
const RESULT_PREVIEW_LEN: usize = 160;

/// 单次工具调用记录（ToolCall 与 ToolResult 按 call id 配对）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallRecord {
    pub name: String,
    /// `None` 表示只有调用没有结果（会话中断）。
    pub ok: Option<bool>,
    pub result_preview: String,
}

/// 交付判定快照（来自 `Delivery` 事件）。
#[derive(Debug, Clone)]
pub struct DeliverySnapshot {
    pub event_id: u64,
    pub outcome: String,
    pub verified: bool,
    pub criteria_total: usize,
    pub criteria_satisfied: usize,
    pub reason: Option<String>,
    pub verification: Vec<String>,
    /// (已满足验收项描述, 证据列表)——候选事实队列的原料。
    pub evidence: Vec<(String, Vec<String>)>,
}

/// 单会话蒸馏结果（纯数据，不含 I/O）。
#[derive(Debug, Clone)]
pub struct SessionDigest {
    pub session_id: String,
    /// 首个用户输入（截断）。
    pub goal: String,
    /// 最后一段非空助手文本（截断）。
    pub final_text: String,
    pub turns: usize,
    pub tools: Vec<ToolCallRecord>,
    pub delivery: Option<DeliverySnapshot>,
    pub usage: Usage,
    /// 来源指针：`session:{sid}#event:{id}`（对应 SessionEvent.id）。
    pub source_refs: Vec<String>,
}

/// 候选事实（未审核；落 review/ 后不进入默认上下文，待 Phase 3 审核晋升）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateFact {
    pub id: String,
    pub text: String,
    pub source_refs: Vec<String>,
}

/// 日志条目（每会话一行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEntry {
    pub session_id: String,
    pub goal: String,
    pub outcome: String,
    pub tool_calls: usize,
}

/// 蒸馏落盘的文件产物。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenDistillation {
    pub summary_path: PathBuf,
    pub candidate_paths: Vec<PathBuf>,
}

/// `DeliveryOutcome` → 稳定字符串标签。
pub fn outcome_label(outcome: &DeliveryOutcome) -> &'static str {
    match outcome {
        DeliveryOutcome::Verified => "verified",
        DeliveryOutcome::NeedsUserInput => "needs-user-input",
        DeliveryOutcome::PartialDelivery => "partial-delivery",
        DeliveryOutcome::SystemFailure => "system-failure",
        DeliveryOutcome::Blocked => "blocked",
        DeliveryOutcome::Interrupted => "interrupted",
        DeliveryOutcome::Cancelled => "cancelled",
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        return t.to_string();
    }
    let mut out: String = t.chars().take(max).collect();
    out.push('…');
    out
}

fn one_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().to_string()
}

fn event_ref(session_id: &str, id: u64) -> String {
    format!("session:{session_id}#event:{id}")
}

/// 规整 id/文件名：仅保留字母数字与 `.-_`，其余转 `-` 并小写。
pub fn sanitize_id(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push('-');
        }
    }
    out
}

/// 确定性蒸馏：从事件流提取目标、工具调用、交付判定、用量与来源指针。
/// 同一事件流重复调用产出完全一致（可回放、幂等）。
pub fn distill_session(session_id: &str, events: &[SessionEvent]) -> SessionDigest {
    let mut goal = String::new();
    let mut final_text = String::new();
    let mut turns = 0usize;
    let mut usage = Usage::default();
    let mut tools: Vec<ToolCallRecord> = Vec::new();
    let mut tool_index: HashMap<String, usize> = HashMap::new();
    let mut delivery: Option<DeliverySnapshot> = None;
    let mut source_refs: Vec<String> = Vec::new();

    for ev in events {
        match ev {
            SessionEvent::TurnStart { id, input } => {
                turns += 1;
                if goal.is_empty() {
                    goal = truncate_chars(input, GOAL_PREVIEW_LEN);
                }
                source_refs.push(event_ref(session_id, *id));
            }
            SessionEvent::Assistant { chunk, .. } => {
                if let Some(text) = &chunk.text {
                    if !text.trim().is_empty() {
                        final_text = truncate_chars(text, TEXT_PREVIEW_LEN);
                    }
                }
            }
            SessionEvent::ToolCall { id, call } => {
                tool_index.insert(call.id.clone(), tools.len());
                tools.push(ToolCallRecord {
                    name: call.name.clone(),
                    ok: None,
                    result_preview: String::new(),
                });
                source_refs.push(event_ref(session_id, *id));
            }
            SessionEvent::ToolResult { result, .. } => {
                if let Some(&i) = tool_index.get(&result.call_id) {
                    tools[i].ok = Some(result.ok);
                    tools[i].result_preview = truncate_chars(&result.content, RESULT_PREVIEW_LEN);
                }
            }
            SessionEvent::Delivery { id, report } => {
                delivery = Some(delivery_snapshot(*id, report));
                source_refs.push(event_ref(session_id, *id));
            }
            SessionEvent::Usage { usage: u, .. } => {
                usage = usage.saturating_add(*u);
            }
            _ => {}
        }
    }

    SessionDigest {
        session_id: session_id.to_string(),
        goal,
        final_text,
        turns,
        tools,
        delivery,
        usage,
        source_refs,
    }
}

fn delivery_snapshot(event_id: u64, report: &DeliveryReport) -> DeliverySnapshot {
    let mut satisfied = 0usize;
    let mut evidence = Vec::new();
    for c in &report.criteria {
        if c.satisfied {
            satisfied += 1;
            if !c.evidence.is_empty() {
                evidence.push((c.description.clone(), c.evidence.clone()));
            }
        }
    }
    DeliverySnapshot {
        event_id,
        outcome: outcome_label(&report.outcome).to_string(),
        verified: report.outcome == DeliveryOutcome::Verified,
        criteria_total: report.criteria.len(),
        criteria_satisfied: satisfied,
        reason: report.reason.clone(),
        verification: report.verification.clone(),
        evidence,
    }
}

/// 候选事实提取：仅从「已验证交付」的验收证据派生带来源候选；
/// 未验证会话不产生候选事实（不把不可信内容排进队列）。
pub fn extract_candidates(digest: &SessionDigest, date: &str) -> Vec<CandidateFact> {
    let d = match &digest.delivery {
        Some(d) if d.verified => d,
        _ => return Vec::new(),
    };
    let delivery_ref = event_ref(&digest.session_id, d.event_id);
    let mut out = Vec::new();
    for (i, (desc, evs)) in d.evidence.iter().enumerate() {
        let mut text = format!("验收证据：{desc}");
        for e in evs {
            text.push_str(&format!("\n- {e}"));
        }
        out.push(CandidateFact {
            id: sanitize_id(&format!("fact.{date}.{}.{}", digest.session_id, i)),
            text,
            source_refs: vec![delivery_ref.clone()],
        });
    }
    if out.is_empty() {
        // verified 但无验收证据明细：退化为会话级候选。
        out.push(CandidateFact {
            id: sanitize_id(&format!("fact.{date}.{}", digest.session_id)),
            text: format!("任务已验证交付：{}", digest.goal),
            source_refs: vec![delivery_ref],
        });
    }
    out
}

/// 渲染 L1 会话摘要（kind: session, layer: L1, status: active，含来源指针）。
pub fn render_session_summary(digest: &SessionDigest, project_id: &str, date: &str) -> String {
    let fm = FrontMatter {
        id: sanitize_id(&format!("session.{date}.{}", digest.session_id)),
        kind: Kind::Session,
        scope: Scope::Session,
        project_id: project_id.to_string(),
        layer: Layer::L1,
        status: Status::Active,
        created_at: date.to_string(),
        updated_at: date.to_string(),
        confidence: 0.9,
        importance: 0.5,
        tags: vec!["session".into(), "distilled".into()],
        source_refs: digest.source_refs.clone(),
        ..Default::default()
    };

    let title = if digest.goal.is_empty() {
        digest.session_id.clone()
    } else {
        one_line(&digest.goal)
    };
    let ok = digest.tools.iter().filter(|t| t.ok == Some(true)).count();
    let failed = digest.tools.iter().filter(|t| t.ok == Some(false)).count();
    let outcome = digest
        .delivery
        .as_ref()
        .map(|d| d.outcome.as_str())
        .unwrap_or("无判定");

    let mut body = String::new();
    body.push_str(&format!("# 会话摘要：{title}\n\n"));
    body.push_str(&format!("- 会话：`{}`\n", digest.session_id));
    body.push_str(&format!("- 日期：{date}\n"));
    body.push_str(&format!("- 回合数：{}\n", digest.turns));
    body.push_str(&format!(
        "- 工具调用：{} 次（成功 {ok} / 失败 {failed}）\n",
        digest.tools.len()
    ));
    body.push_str(&format!(
        "- Token：prompt {} / completion {} / total {}\n",
        digest.usage.prompt_tokens, digest.usage.completion_tokens, digest.usage.total_tokens
    ));
    body.push_str(&format!("- 交付：{outcome}\n"));

    body.push_str("\n## 目标\n\n");
    body.push_str(if digest.goal.is_empty() {
        "（无用户输入）"
    } else {
        &digest.goal
    });
    body.push('\n');

    if let Some(d) = &digest.delivery {
        body.push_str("\n## 交付判定\n\n");
        body.push_str(&format!("- 结论：{}\n", d.outcome));
        body.push_str(&format!(
            "- 验收项：{}/{} 满足\n",
            d.criteria_satisfied, d.criteria_total
        ));
        if let Some(r) = &d.reason {
            body.push_str(&format!("- 原因：{r}\n"));
        }
        if !d.verification.is_empty() {
            body.push_str("- 运行时验证：\n");
            for v in &d.verification {
                body.push_str(&format!("  - {v}\n"));
            }
        }
    }

    if !digest.tools.is_empty() {
        body.push_str("\n## 工具调用\n\n");
        for t in &digest.tools {
            let mark = match t.ok {
                Some(true) => "✓",
                Some(false) => "✗",
                None => "?",
            };
            body.push_str(&format!("- {mark} `{}`", t.name));
            if !t.result_preview.is_empty() {
                body.push_str(&format!(" — {}", one_line(&t.result_preview)));
            }
            body.push('\n');
        }
    }

    if !digest.final_text.is_empty() {
        body.push_str("\n## 最终回复（截断）\n\n");
        body.push_str(&digest.final_text);
        body.push('\n');
    }

    render_markdown(&fm, &body)
}

/// 渲染单条候选事实（status: candidate——未审核，不进入默认上下文）。
pub fn render_candidate(fact: &CandidateFact, project_id: &str, date: &str) -> String {
    let fm = FrontMatter {
        id: fact.id.clone(),
        kind: Kind::Fact,
        scope: Scope::Project,
        project_id: project_id.to_string(),
        layer: Layer::L2,
        status: Status::Candidate,
        created_at: date.to_string(),
        updated_at: date.to_string(),
        source_refs: fact.source_refs.clone(),
        ..Default::default()
    };
    let body = format!(
        "# 候选事实：{}\n\n{}\n\n> 状态：candidate（未审核，不进入默认上下文；审核通过后晋升为正式事实）。\n",
        one_line(&fact.text),
        fact.text
    );
    render_markdown(&fm, &body)
}

/// 从蒸馏结果生成日志条目。
pub fn journal_entry(digest: &SessionDigest) -> JournalEntry {
    JournalEntry {
        session_id: digest.session_id.clone(),
        goal: if digest.goal.is_empty() {
            "（无目标）".into()
        } else {
            one_line(&digest.goal)
        },
        outcome: digest
            .delivery
            .as_ref()
            .map(|d| d.outcome.clone())
            .unwrap_or_else(|| "无判定".into()),
        tool_calls: digest.tools.len(),
    }
}

/// 每日开发日志聚合（kind: journal, layer: L1）。相同输入输出一致（幂等）。
pub fn render_daily_journal(project_id: &str, date: &str, entries: &[JournalEntry]) -> String {
    let fm = FrontMatter {
        id: sanitize_id(&format!("journal.{date}.{project_id}")),
        kind: Kind::Journal,
        scope: Scope::Project,
        project_id: project_id.to_string(),
        layer: Layer::L1,
        status: Status::Active,
        created_at: date.to_string(),
        updated_at: date.to_string(),
        tags: vec!["journal".into(), "daily".into()],
        ..Default::default()
    };
    let mut body = format!("# 开发日志 {date}\n\n");
    if entries.is_empty() {
        body.push_str("今日无会话记录。\n");
    } else {
        body.push_str("| 会话 | 目标 | 交付 | 工具调用 |\n|---|---|---|---|\n");
        for e in entries {
            body.push_str(&format!(
                "| `{}` | {} | {} | {} |\n",
                sanitize_id(&e.session_id),
                e.goal.replace('|', "/"),
                e.outcome,
                e.tool_calls
            ));
        }
    }
    render_markdown(&fm, &body)
}

/// 把蒸馏结果落盘到知识根（Phase 0 模板相对路径）：
/// - `conversations/{date}/session-{sid}.md`（L1 摘要）
/// - `review/{fact-id}.md`（候选事实，不被索引器投影）
///
/// 幂等：同一 (date, session) 路径固定、内容确定性生成，重复调用为覆盖。
pub fn write_distillation(
    knowledge_root: &Path,
    digest: &SessionDigest,
    project_id: &str,
    date: &str,
) -> std::io::Result<WrittenDistillation> {
    let conv = knowledge_root.join("conversations").join(date);
    std::fs::create_dir_all(&conv)?;
    let summary_path = conv.join(format!("session-{}.md", sanitize_id(&digest.session_id)));
    std::fs::write(
        &summary_path,
        render_session_summary(digest, project_id, date),
    )?;

    let review = knowledge_root.join("review");
    let mut candidate_paths = Vec::new();
    for fact in extract_candidates(digest, date) {
        std::fs::create_dir_all(&review)?;
        let p = review.join(format!("{}.md", fact.id));
        std::fs::write(&p, render_candidate(&fact, project_id, date))?;
        candidate_paths.push(p);
    }
    Ok(WrittenDistillation {
        summary_path,
        candidate_paths,
    })
}

/// 写每日日志到 `conversations/{date}/journal.md`（幂等覆盖）。
pub fn write_daily_journal(
    knowledge_root: &Path,
    project_id: &str,
    date: &str,
    entries: &[JournalEntry],
) -> std::io::Result<PathBuf> {
    let conv = knowledge_root.join("conversations").join(date);
    std::fs::create_dir_all(&conv)?;
    let path = conv.join("journal.md");
    std::fs::write(&path, render_daily_journal(project_id, date, entries))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontmatter::parse_markdown;
    use harness_llm::{Chunk, ToolCall, ToolResult};
    use harness_session::DeliveryCriterion;

    fn sample_events() -> Vec<SessionEvent> {
        vec![
            SessionEvent::TurnStart {
                id: 1,
                input: "修复登录 bug".into(),
            },
            SessionEvent::ToolCall {
                id: 2,
                call: ToolCall {
                    id: "c1".into(),
                    name: "fs_read".into(),
                    args: serde_json::json!({"path": "a.rs"}),
                },
            },
            SessionEvent::ToolResult {
                id: 3,
                result: ToolResult {
                    call_id: "c1".into(),
                    ok: true,
                    content: "fn login() {}".into(),
                    continuation_debt: 0,
                },
            },
            SessionEvent::ToolCall {
                id: 4,
                call: ToolCall {
                    id: "c2".into(),
                    name: "shell".into(),
                    args: serde_json::json!({}),
                },
            },
            SessionEvent::ToolResult {
                id: 5,
                result: ToolResult {
                    call_id: "c2".into(),
                    ok: false,
                    content: "exit 1".into(),
                    continuation_debt: 1,
                },
            },
            SessionEvent::Assistant {
                id: 6,
                chunk: Chunk {
                    text: Some("已修复，测试通过。".into()),
                    ..Default::default()
                },
            },
            SessionEvent::Usage {
                id: 7,
                usage: Usage {
                    prompt_tokens: 100,
                    completion_tokens: 20,
                    total_tokens: 120,
                },
            },
            SessionEvent::Delivery {
                id: 8,
                report: DeliveryReport {
                    outcome: DeliveryOutcome::Verified,
                    criteria: vec![DeliveryCriterion {
                        id: "user-objective".into(),
                        description: "登录 bug 修复".into(),
                        satisfied: true,
                        evidence: vec!["test login_ok passed".into()],
                    }],
                    verification: vec!["cargo test -p app login".into()],
                    reason: None,
                },
            },
            SessionEvent::TurnEnd { id: 9 },
        ]
    }

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "harness_distill_{tag}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn distill_extracts_goal_tools_delivery_usage() {
        let d = distill_session("s-123", &sample_events());
        assert_eq!(d.goal, "修复登录 bug");
        assert_eq!(d.turns, 1);
        assert_eq!(d.tools.len(), 2);
        assert_eq!(d.tools[0].name, "fs_read");
        assert_eq!(d.tools[0].ok, Some(true));
        assert_eq!(d.tools[1].ok, Some(false));
        assert_eq!(d.final_text, "已修复，测试通过。");
        assert_eq!(d.usage.total_tokens, 120);
        let del = d.delivery.as_ref().unwrap();
        assert!(del.verified);
        assert_eq!(del.criteria_satisfied, 1);
        assert_eq!(del.criteria_total, 1);
        assert_eq!(del.evidence.len(), 1);
        // 来源指针：TurnStart / ToolCall×2 / Delivery
        assert!(d.source_refs.contains(&"session:s-123#event:1".to_string()));
        assert!(d.source_refs.contains(&"session:s-123#event:8".to_string()));
        assert_eq!(d.source_refs.len(), 4);
    }

    #[test]
    fn summary_markdown_has_l1_frontmatter_and_refs() {
        let d = distill_session("s-123", &sample_events());
        let md = render_session_summary(&d, "proj-a", "2026-02-11");
        let parsed = parse_markdown(&md);
        let fm = parsed.front_matter.expect("front matter");
        assert_eq!(fm.kind, Kind::Session);
        assert_eq!(fm.layer, Layer::L1);
        assert_eq!(fm.status, Status::Active);
        assert_eq!(fm.project_id, "proj-a");
        assert!(!fm.source_refs.is_empty());
        assert!(parsed.body.contains("修复登录 bug"));
        assert!(parsed.body.contains("verified"));
        assert!(parsed.body.contains("cargo test -p app login"));
    }

    #[test]
    fn candidates_come_from_verified_evidence_only() {
        let d = distill_session("s-123", &sample_events());
        let facts = extract_candidates(&d, "2026-02-11");
        assert_eq!(facts.len(), 1);
        assert!(facts[0].text.contains("登录 bug 修复"));
        assert!(facts[0]
            .source_refs
            .iter()
            .all(|r| r.starts_with("session:s-123#event:")));

        let md = render_candidate(&facts[0], "proj-a", "2026-02-11");
        let fm = parse_markdown(&md).front_matter.unwrap();
        assert_eq!(fm.status, Status::Candidate);
        assert_eq!(fm.kind, Kind::Fact);
        assert_eq!(fm.layer, Layer::L2);
        assert!(!fm.source_refs.is_empty());
    }

    #[test]
    fn unverified_session_yields_no_candidates() {
        let events = vec![
            SessionEvent::TurnStart {
                id: 1,
                input: "做点什么".into(),
            },
            SessionEvent::Delivery {
                id: 2,
                report: DeliveryReport {
                    outcome: DeliveryOutcome::SystemFailure,
                    criteria: vec![],
                    verification: vec![],
                    reason: Some("工具链失败".into()),
                },
            },
        ];
        let d = distill_session("s-9", &events);
        assert!(extract_candidates(&d, "2026-02-11").is_empty());
        let md = render_session_summary(&d, "p", "2026-02-11");
        assert!(md.contains("system-failure"));
        assert!(md.contains("工具链失败"));
    }

    #[test]
    fn journal_aggregates_and_is_deterministic() {
        let d1 = distill_session("s-1", &sample_events());
        let d2 = distill_session("s-2", &sample_events());
        let entries = vec![journal_entry(&d1), journal_entry(&d2)];
        let md = render_daily_journal("proj-a", "2026-02-11", &entries);
        let fm = parse_markdown(&md).front_matter.unwrap();
        assert_eq!(fm.kind, Kind::Journal);
        assert_eq!(fm.layer, Layer::L1);
        assert!(md.contains("s-1") && md.contains("s-2"));
        // 幂等：相同输入两次渲染输出一致
        assert_eq!(md, render_daily_journal("proj-a", "2026-02-11", &entries));
    }

    #[test]
    fn write_distillation_is_idempotent() {
        let root = temp_root("write");
        let kb = root.join(".harness-memory");
        std::fs::create_dir_all(&kb).unwrap();

        let d = distill_session("s-123", &sample_events());
        let w1 = write_distillation(&kb, &d, "proj-a", "2026-02-11").unwrap();
        assert_eq!(
            w1.summary_path.file_name().and_then(|n| n.to_str()),
            Some("session-s-123.md")
        );
        assert_eq!(
            w1.summary_path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str()),
            Some("2026-02-11")
        );
        assert_eq!(w1.candidate_paths.len(), 1);
        assert!(w1.candidate_paths[0].starts_with(kb.join("review")));

        let content1 = std::fs::read_to_string(&w1.summary_path).unwrap();
        let w2 = write_distillation(&kb, &d, "proj-a", "2026-02-11").unwrap();
        assert_eq!(w1, w2);
        let content2 = std::fs::read_to_string(&w2.summary_path).unwrap();
        assert_eq!(content1, content2); // 幂等：覆盖而非重复

        let jp = write_daily_journal(&kb, "proj-a", "2026-02-11", &[journal_entry(&d)]).unwrap();
        assert!(jp.exists());

        // 落盘文件可被 Phase 0 解析器解析（闭环自证）
        let fm = parse_markdown(&content2).front_matter.unwrap();
        assert_eq!(fm.kind, Kind::Session);

        let _ = std::fs::remove_dir_all(&root);
    }
}
