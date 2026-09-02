//! 传感器：旧守卫降级后的信号生产者（spec §4.1 / §4.4）。只算信号，不做终止判断。

use crate::case_file::CaseFile;

/// 一个策略窗口内的增益分量。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WindowDelta {
    pub new_anchors: usize,
    pub new_eliminations: usize,
    pub write_increment: usize,
    pub new_user_signals: usize,
}

impl WindowDelta {
    /// 窗口总增益。控制器规则：`gain == 0` → pop 栈（spec §4.2）。
    pub fn gain(&self) -> usize {
        self.new_anchors + self.new_eliminations + self.write_increment + self.new_user_signals
    }
}

/// 取同一时间线上两份投影的差值（`window_base` 为窗口起点快照）。
/// 写入增量不在 CaseFile 里（它是 Runtime 侧计数），由调用方补。
pub fn delta_between(window_base: &CaseFile, now: &CaseFile) -> WindowDelta {
    WindowDelta {
        new_anchors: now.anchors.len().saturating_sub(window_base.anchors.len()),
        new_eliminations: now
            .eliminated
            .len()
            .saturating_sub(window_base.eliminated.len()),
        write_increment: 0,
        new_user_signals: now
            .user_signals
            .len()
            .saturating_sub(window_base.user_signals.len()),
    }
}

/// R4 / ExhaustedWithArtifact 的结构化资产（spec §3 R4 四要素）。
///
/// 标记词 `锚点：` / `假设：` / `补丁建议：` / `问项：` 是回放套件
/// `missing_artifact_violations` 度量器的解析契约——改名即破绿。缺失要素一律写
/// 显式占位，不得省略（「无锚点」也要说明为何无锚点）。
pub fn artifact_text(
    case: &CaseFile,
    hypothesis: &str,
    suggested_patch: &str,
    candidate_question: Option<&str>,
) -> String {
    const MAX_ARTIFACT_ANCHORS: usize = 8;
    let anchors = if case.anchors.is_empty() {
        "无（本回合未产生任何工具命中或路径证据）".to_string()
    } else {
        let visible = case
            .anchors
            .iter()
            .take(MAX_ARTIFACT_ANCHORS)
            .cloned()
            .collect::<Vec<_>>();
        let remaining = case.anchors.len().saturating_sub(visible.len());
        if remaining == 0 {
            visible.join("; ")
        } else {
            format!("{}; …另有 {remaining} 个锚点已省略", visible.join("; "))
        }
    };
    let hypothesis = if hypothesis.trim().is_empty() {
        "待形成（尚未收敛出可验证的根因假设）"
    } else {
        hypothesis.trim()
    };
    let suggested_patch = if suggested_patch.trim().is_empty() {
        "无（缺少可落地的改动形状）"
    } else {
        suggested_patch.trim()
    };
    format!(
        "【资产】锚点：{anchors}\n假设：{hypothesis}\n补丁建议：{suggested_patch}\n问项：{}",
        candidate_question.unwrap_or("无")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_llm::Chunk;
    use harness_session::SessionEvent;

    fn case_with(anchors: &[&str], signals: usize) -> CaseFile {
        let mut events: Vec<SessionEvent> = (0..signals)
            .map(|i| SessionEvent::TurnStart {
                id: i as u64,
                input: format!("输入 {i}"),
            })
            .collect();
        events.push(SessionEvent::Assistant {
            id: 99,
            chunk: Chunk {
                text: Some(anchors.join(" ")),
                ..Default::default()
            },
        });
        CaseFile::from_replay(&events)
    }

    #[test]
    fn gain_sums_all_four_components() {
        let delta = WindowDelta {
            new_anchors: 2,
            new_eliminations: 1,
            write_increment: 3,
            new_user_signals: 4,
        };
        assert_eq!(delta.gain(), 10);
        assert_eq!(
            WindowDelta::default().gain(),
            0,
            "无增益即 0，控制器据此换路"
        );
    }

    #[test]
    fn delta_counts_new_anchors_and_signals_only() {
        let window_base = case_with(&["a/one.rs"], 1);
        let now = case_with(&["a/one.rs", "b/two.rs", "c/three.rs"], 2);
        let delta = delta_between(&window_base, &now);
        assert_eq!(delta.new_anchors, 2, "{:?}", now.anchors);
        assert_eq!(delta.new_user_signals, 1);
        assert_eq!(delta.write_increment, 0, "写入增量由调用方补");
    }

    #[test]
    fn delta_never_goes_negative_when_window_base_is_ahead() {
        // 续跑时窗口基线可能取自更长历史：saturating_sub 保证不倒扣。
        let window_base = case_with(&["x/a.rs", "y/b.rs"], 3);
        let now = case_with(&["x/a.rs"], 1);
        let delta = delta_between(&window_base, &now);
        assert_eq!(delta.new_anchors, 0);
        assert_eq!(delta.new_user_signals, 0);
    }

    #[test]
    fn artifact_always_carries_all_four_labels() {
        let text = artifact_text(&CaseFile::default(), "", "", None);
        for label in ["锚点：", "假设：", "补丁建议：", "问项："] {
            assert!(text.contains(label), "{label} 缺失：{text}");
        }
        assert!(
            text.contains("无（本回合未产生任何工具命中或路径证据）"),
            "{text}"
        );
        assert!(text.contains("待形成"), "{text}");
    }

    #[test]
    fn artifact_lists_anchors_and_candidate_question() {
        let case = case_with(&["harness/src/lib.rs", "docs/spec.md"], 1);
        let text = artifact_text(
            &case,
            "门禁在澄清出口未去重",
            "把三处门禁收敛到 ask_user 前置裁决",
            Some("是否只修 src/lib.rs？"),
        );
        assert!(text.contains("harness/src/lib.rs"), "{text}");
        assert!(text.contains("门禁在澄清出口未去重"), "{text}");
        assert!(text.contains("是否只修 src/lib.rs？"), "{text}");
    }

    #[test]
    fn artifact_bounds_anchor_output() {
        let anchors = (0..20)
            .map(|index| format!("src/file_{index}.rs"))
            .collect::<Vec<_>>();
        let refs = anchors.iter().map(String::as_str).collect::<Vec<_>>();
        let text = artifact_text(&case_with(&refs, 1), "h", "p", None);
        assert!(text.contains("另有 12 个锚点已省略"), "{text}");
        assert!(!text.contains("src/file_8.rs"), "{text}");
    }
}
