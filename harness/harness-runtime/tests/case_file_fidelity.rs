//! 绞杀者步骤②验收：Case File 只记录不决策，与真实会话日志对拍 tried/anchors/asked
//! 保真度（spec §5 步骤 2）。读的是**原始会话** fixture，不是重放产出的新日志——
//! 世界模型必须能从真实失败会话读出正确形状，才有资格在步骤④接管决策。

use harness_runtime::CaseFile;
use harness_session::SessionEvent;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/");

fn case_of(name: &str) -> CaseFile {
    let raw = std::fs::read_to_string(format!("{FIXTURES}{name}"))
        .unwrap_or_else(|e| panic!("fixture {name} 读取失败: {e}"));
    let events: Vec<SessionEvent> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("fixture 事件解析失败"))
        .collect();
    CaseFile::from_replay(&events)
}

#[test]
fn projection_is_deterministic_on_real_logs() {
    for fixture in [
        "7ba3370f_t03_14_symptom.jsonl",
        "7ba3370f_t15_18_clarification.jsonl",
        "7ba3370f_t19_22_gitfix.jsonl",
        "success_677bd6e0.jsonl",
    ] {
        assert_eq!(case_of(fixture), case_of(fixture), "{fixture} 派生不确定");
    }
}

#[test]
fn user_signals_match_turn_counts() {
    assert_eq!(
        case_of("7ba3370f_t03_14_symptom.jsonl").user_signals.len(),
        12
    );
    assert_eq!(
        case_of("7ba3370f_t15_18_clarification.jsonl")
            .user_signals
            .len(),
        4
    );
    assert_eq!(
        case_of("7ba3370f_t19_22_gitfix.jsonl").user_signals.len(),
        4
    );
    assert_eq!(case_of("success_677bd6e0.jsonl").user_signals.len(), 5);
    assert_eq!(case_of("7ba3370f_full.jsonl").user_signals.len(), 22);
}

#[test]
fn clarification_segment_repeats_one_question_and_runs_no_tools() {
    // 复盘根因 4（spec §1）：turn 15–18 是同一澄清文案的无限复读。
    let case = case_of("7ba3370f_t15_18_clarification.jsonl");
    assert!(
        case.tried.is_empty(),
        "澄清段不该有任何工具调用：{:?}",
        case.tried
    );
    assert_eq!(
        case.asked.len(),
        1,
        "四回合复读应折叠为一条 asked：{:?}",
        case.asked
    );
}

#[test]
fn gitfix_segment_records_failed_edit_attempts() {
    // 复盘证据（spec §附录）：turn 19 连续 3 次 edit matched 0。
    let case = case_of("7ba3370f_t19_22_gitfix.jsonl");
    let edits: Vec<_> = case.tried.iter().filter(|t| t.tool == "edit").collect();
    assert!(!edits.is_empty(), "gitfix 段应记录 edit 尝试");
    assert!(edits.iter().any(|t| !t.ok), "至少要有一条失败的 edit 记录");
    assert!(
        edits.iter().any(|t| t.summary.contains("matched")),
        "edit 失配摘要应进入 tried.summary：{:?}",
        edits.iter().map(|t| &t.summary).collect::<Vec<_>>()
    );
}

#[test]
fn symptom_segment_accumulates_anchors() {
    let case = case_of("7ba3370f_t03_14_symptom.jsonl");
    assert!(!case.tried.is_empty(), "症状段必须有工具尝试");
    assert!(!case.anchors.is_empty(), "工具命中必须沉淀出精确锚点");
    assert!(
        case.anchors.iter().all(|a| {
            (a.contains('/') || a.contains('\\'))
                && [".rs", ".toml", ".md", ".json", ".py", ".ts", ".slint"]
                    .iter()
                    .any(|ext| a.contains(ext))
        }),
        "锚点必须同时具备路径分隔符与源码扩展名：{:?}",
        case.anchors
    );
}

#[test]
fn full_session_token_cost_exceeds_red_line_cap() {
    // 同时是 R3 度量器的有效性证明：原会话真实成本必须远超 300k，否则 R3 在回放里
    // 永远读 0、红线形同虚设（阶段 1 遗留疑虑在此关闭）。
    let case = case_of("7ba3370f_full.jsonl");
    assert!(
        case.prompt_tokens > harness_runtime::PROMPT_CAP,
        "复盘记录 3.14M prompt tokens，实际读出 {}（顶 {}）",
        case.prompt_tokens,
        harness_runtime::PROMPT_CAP
    );
}
