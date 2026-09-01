//! Case file：会话级世界模型（spec §4.3）。
//!
//! 单一事实源仍是 SessionLog；`CaseFile` 是它的**确定性投影**，不引入第二份持久化。
//! 回合从 case file 出发：`tried` 里已存在的签名直接换策略，跨轮无状态重放构造性消失。

use std::collections::{BTreeSet, HashMap};

use harness_session::{DeliveryOutcome, SessionEvent};

use crate::execution::normalized_signature;

/// 一次工具尝试：工具名 + 归一化签名 + 成否 + 紧凑摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriedEntry {
    pub tool: String,
    pub signature: String,
    pub ok: bool,
    pub summary: String,
}

/// 会话级世界模型。全部字段由 `absorb` 从事件流折叠得出，无外部写入路径。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaseFile {
    pub tried: Vec<TriedEntry>,
    /// 已排除的策略标签，控制器 pop 时追加（spec §4.4 gain 计量项之一）。
    pub eliminated: BTreeSet<String>,
    /// 精确锚点：含路径分隔符且带源码扩展名的 token（R4 的最低证据单位）。
    pub anchors: BTreeSet<String>,
    /// 用户信号：按序的每回合用户原话。
    pub user_signals: Vec<String>,
    /// 已问过的澄清文案（归一形式）。R2「同一问题不得问第二次」的判据。
    pub asked: BTreeSet<String>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub last_outcome: Option<DeliveryOutcome>,
    /// call_id → (工具名, 归一化签名)。ToolResult 不带工具名，需借 ToolCall 配对；
    /// 属投影内部状态，参与相等性以保证全量派生与增量 absorb 结果一致。
    pending_calls: HashMap<String, (String, String)>,
    /// 本回合（最近一次 TurnStart 之后）的助手全文缓冲，Delivery 时结算进 asked。
    /// 用状态而非「回看事件切片」：全量单次 fold 与回合内增量 absorb 必须同义，
    /// 而切片回溯在批次里含后续 TurnStart 时会把澄清文本挂错回合。
    turn_text: String,
}

/// 澄清文案归一：剥离所有空白。回放套件 R2 度量器使用同一判据。
pub fn normalize_question(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// 锚点扩展名集合（与阶段 1 `has_path_anchor` 保持一致）。
const ANCHOR_EXTENSIONS: [&str; 7] = [".rs", ".toml", ".md", ".json", ".py", ".ts", ".slint"];

/// 从锚点 token 上剥除的句读/括号尾缀。
const ANCHOR_TRIM_END: [char; 12] = [
    '，', '。', '；', '、', '）', '】', '！', '"', '\'', ',', ';', ')',
];

/// 从自由文本抽取精确锚点：含 `/` 或 `\` 且含源码扩展名的空白分隔 token。
pub fn extract_anchors(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|tok| tok.trim_end_matches(ANCHOR_TRIM_END.as_ref()))
        .filter(|tok| {
            (tok.contains('/') || tok.contains('\\'))
                && ANCHOR_EXTENSIONS.iter().any(|ext| tok.contains(ext))
        })
        .map(|tok| tok.to_string())
        .collect()
}

/// 工具结果摘要：仅保留前 160 字符，控制 case file 体积（投影会被频繁重建）。
fn summarize(content: &str) -> String {
    content.chars().take(160).collect()
}

impl CaseFile {
    /// 从完整事件流确定性重建（spec §4.3：fork / resume / replay 共用此路径）。
    pub fn from_replay(events: &[SessionEvent]) -> Self {
        let mut case = Self::default();
        case.absorb(events);
        case
    }

    /// 折叠一批事件。全量派生与回合内增量共用这一条实现（DRY）。
    pub fn absorb(&mut self, events: &[SessionEvent]) {
        for ev in events {
            match ev {
                SessionEvent::TurnStart { input, .. } => {
                    self.user_signals.push(input.clone());
                    self.turn_text.clear();
                }
                SessionEvent::Assistant { chunk, .. } => {
                    if let Some(text) = chunk.text.as_deref() {
                        self.turn_text.push_str(text);
                        for anchor in extract_anchors(text) {
                            self.anchors.insert(anchor);
                        }
                    }
                }
                SessionEvent::ToolCall { call, .. } => {
                    self.pending_calls.insert(
                        call.id.clone(),
                        (call.name.clone(), normalized_signature(call)),
                    );
                }
                SessionEvent::ToolResult { result, .. } => {
                    let (tool, signature) = self.pending_calls.remove(&result.call_id).unwrap_or((
                        "unknown".into(),
                        format!("unknown:{}", result.call_id),
                    ));
                    self.tried.push(TriedEntry {
                        tool,
                        signature,
                        ok: result.ok,
                        summary: summarize(&result.content),
                    });
                    for anchor in extract_anchors(&result.content) {
                        self.anchors.insert(anchor);
                    }
                }
                SessionEvent::Usage { usage, .. } => {
                    self.prompt_tokens += usage.prompt_tokens;
                    self.completion_tokens += usage.completion_tokens;
                }
                SessionEvent::Delivery { report, .. } => {
                    if report.outcome == DeliveryOutcome::NeedsUserInput {
                        let key = normalize_question(&std::mem::take(&mut self.turn_text));
                        if !key.is_empty() {
                            self.asked.insert(key);
                        }
                    }
                    self.last_outcome = Some(report.outcome.clone());
                }
                _ => {}
            }
        }
    }

    /// 该签名是否已在本会话尝试过（spec §4.3 的构造性去重入口）。
    pub fn is_tried(&self, signature: &str) -> bool {
        self.tried.iter().any(|t| t.signature == signature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_llm::{Chunk, ToolCall, ToolResult, Usage};

    fn turn(input: &str) -> SessionEvent {
        SessionEvent::TurnStart {
            id: 0,
            input: input.into(),
        }
    }

    fn assistant(text: &str) -> SessionEvent {
        SessionEvent::Assistant {
            id: 0,
            chunk: Chunk {
                text: Some(text.into()),
                ..Default::default()
            },
        }
    }

    fn call(id: &str, name: &str, args: serde_json::Value) -> SessionEvent {
        SessionEvent::ToolCall {
            id: 0,
            call: ToolCall {
                id: id.into(),
                name: name.into(),
                args,
            },
        }
    }

    fn result(id: &str, ok: bool, content: &str) -> SessionEvent {
        SessionEvent::ToolResult {
            id: 0,
            result: ToolResult {
                call_id: id.into(),
                ok,
                content: content.into(),
                continuation_debt: 0,
            },
        }
    }

    fn usage(prompt: u64, completion: u64) -> SessionEvent {
        SessionEvent::Usage {
            id: 0,
            usage: Usage {
                prompt_tokens: prompt,
                completion_tokens: completion,
                total_tokens: prompt + completion,
            },
        }
    }

    fn delivery(outcome: DeliveryOutcome, reason: Option<&str>) -> SessionEvent {
        SessionEvent::Delivery {
            id: 0,
            report: harness_session::DeliveryReport {
                outcome,
                criteria: vec![],
                verification: vec![],
                reason: reason.map(|r| r.to_string()),
            },
        }
    }

    #[test]
    fn from_replay_is_deterministic_and_accumulates_usage() {
        let events = vec![turn("消除 git 黑框"), usage(120, 30), usage(200, 40)];
        let a = CaseFile::from_replay(&events);
        assert_eq!(a, CaseFile::from_replay(&events), "同一事件流必须得到同一投影");
        assert_eq!(a.prompt_tokens, 320);
        assert_eq!(a.completion_tokens, 70);
        assert_eq!(a.user_signals, vec!["消除 git 黑框".to_string()]);
    }

    #[test]
    fn tried_signature_neutralizes_cd_prefix() {
        // 与旧守卫共用 normalized_signature：字面不同、语义相同的命令归为一个签名。
        let events = vec![
            turn("跑测试"),
            call(
                "c1",
                "shell",
                serde_json::json!({"command": "cd /d F:/w/harness && cargo test"}),
            ),
            result("c1", true, "test result: ok"),
            call("c2", "shell", serde_json::json!({"command": "cargo test"})),
            result("c2", true, "test result: ok"),
        ];
        let case = CaseFile::from_replay(&events);
        assert_eq!(case.tried.len(), 2);
        assert_eq!(case.tried[0].signature, case.tried[1].signature);
        assert!(case.is_tried(&case.tried[0].signature));
        assert!(!case.is_tried("shell:{\"command\":\"其它\"}"));
    }

    #[test]
    fn anchors_come_from_tool_results_and_assistant_text() {
        let events = vec![
            turn("定位实现"),
            call("c1", "search", serde_json::json!({"pattern": "GitCli"})),
            result(
                "c1",
                true,
                "harness/harness-provider-git/src/lib.rs:61: fn git_command",
            ),
            assistant("根因在 provider-git/src/lib.rs，未加 CREATE_NO_WINDOW。"),
        ];
        let case = CaseFile::from_replay(&events);
        assert!(
            case.anchors
                .iter()
                .any(|a| a.contains("harness-provider-git/src/lib.rs")),
            "{:?}",
            case.anchors
        );
        assert!(
            case.anchors
                .iter()
                .all(|a| a.contains('/') || a.contains('\\')),
            "{:?}",
            case.anchors
        );
    }

    #[test]
    fn asked_records_clarification_text_only_for_needs_user_input() {
        let question = "需要补充执行信息：请确认目标模块";
        let events = vec![
            turn("改一下"),
            assistant(question),
            delivery(DeliveryOutcome::NeedsUserInput, Some(question)),
            turn("继续"),
            assistant("已交付。"),
            delivery(DeliveryOutcome::Verified, None),
        ];
        let case = CaseFile::from_replay(&events);
        assert_eq!(case.asked.len(), 1, "{:?}", case.asked);
        assert!(case.asked.contains(&normalize_question(question)));
        assert_eq!(case.last_outcome, Some(DeliveryOutcome::Verified));
    }

    #[test]
    fn unpaired_tool_result_still_recorded() {
        // 中断会话可能缺 ToolCall 配对：不得丢证据，退化为 unknown 签名。
        let case = CaseFile::from_replay(&[turn("x"), result("orphan", false, "matched 0")]);
        assert_eq!(case.tried.len(), 1);
        assert_eq!(case.tried[0].tool, "unknown");
        assert!(!case.tried[0].ok);
    }
}
