//! 唯一交付裁决的行为门槛（《Agent 任务执行机制重构实施方案》§5 回放矩阵中可由
//! 纯函数判定的行）。这些断言只消费工具事实，不含模型文字，因此与 provider 无关。

use std::collections::{HashMap, HashSet};

use harness_runtime::delivery_decision::{
    evaluate_delivery, evidence_covers_requirement, DeliveryFacts, DeliveryStatus, StopCause,
};
use harness_runtime::execution::{Criterion, EvidenceRequirement, RequestedOutcome};
use harness_session::DeliveryOutcome;

fn criterion(id: &str) -> Criterion {
    Criterion {
        id: id.to_string(),
        description: format!("验收项 {id}"),
    }
}

fn facts<'a>(criteria: impl IntoIterator<Item = &'a str>) -> DeliveryFacts {
    DeliveryFacts {
        requested_outcome: RequestedOutcome::Change,
        evidence_requirement: EvidenceRequirement::Behavior,
        requires_verification: true,
        requires_workspace_change: true,
        criteria: criteria.into_iter().map(criterion).collect(),
        changed_criteria: HashSet::new(),
        verification: HashMap::new(),
        read_evidence: Vec::new(),
        write_operations: 0,
        solver_claims_complete: false,
        has_execution_evidence: true,
        stop: StopCause::Settled,
    }
}

fn with_change(mut facts: DeliveryFacts, ids: &[&str]) -> DeliveryFacts {
    facts.write_operations = ids.len();
    facts.changed_criteria = ids.iter().map(|id| (*id).to_string()).collect();
    facts
}

fn with_verification(mut facts: DeliveryFacts, id: &str) -> DeliveryFacts {
    facts
        .verification
        .insert(id.to_string(), vec![format!("shell:cargo test {id} => 0")]);
    facts
}

/// 「确定按钮看不到」：搜索、读取与编译都成功，但零相关改动，绝不能记为 Verified。
#[test]
fn zero_edit_symptom_task_is_never_verified() {
    let mut facts = facts(["c1"]);
    facts.evidence_requirement = EvidenceRequirement::Visual;
    facts.read_evidence = vec!["已读取 workspace.rs".into()];
    facts.solver_claims_complete = true;
    let decision = evaluate_delivery(&facts);
    assert!(!decision.is_completed());
    assert_ne!(decision.outcome(), DeliveryOutcome::Verified);
    assert!(matches!(decision.status, DeliveryStatus::Paused(_)));
}

/// 视觉与行为验收不接受编译或静态字串作为证明。
#[test]
fn compile_and_static_evidence_do_not_certify_visual_or_behavior() {
    assert!(!evidence_covers_requirement(
        EvidenceRequirement::Visual,
        false,
        "shell:cargo test --ui"
    ));
    // 视觉要求即使 verification_only 也不能自证：Runtime 没有可重复的视觉观察器。
    assert!(!evidence_covers_requirement(
        EvidenceRequirement::Visual,
        true,
        "shell:cargo test --ui"
    ));
    assert!(!evidence_covers_requirement(
        EvidenceRequirement::Behavior,
        false,
        "shell:cargo check"
    ));
    assert!(evidence_covers_requirement(
        EvidenceRequirement::Behavior,
        false,
        "shell:cargo test delivery"
    ));
    assert!(evidence_covers_requirement(
        EvidenceRequirement::Command,
        false,
        "shell:cargo check"
    ));
}

/// 已修改 UI 但只拿到编译通过：报告已改动与未验证项，不声称视觉故障已解决。
#[test]
fn changed_but_unverified_reports_pending_not_verified() {
    let facts = with_change(
        {
            let mut base = facts(["c1"]);
            base.evidence_requirement = EvidenceRequirement::Visual;
            base
        },
        &["c1"],
    );
    let decision = evaluate_delivery(&facts);
    assert_eq!(decision.outcome(), DeliveryOutcome::PartialDelivery);
    assert_eq!(decision.unverified_criteria(), vec!["c1"]);
    assert_eq!(decision.unmet_observations.len(), 1);
    assert!(decision.unmet_observations[0].contains("界面外观"));
}

/// 完整链路：定位 → 写入 → 写入后取得匹配验收要求的验证，才允许 Completed。
#[test]
fn change_plus_matching_post_write_verification_completes() {
    let facts = with_verification(with_change(facts(["c1"]), &["c1"]), "c1");
    let decision = evaluate_delivery(&facts);
    assert!(decision.is_completed());
    assert_eq!(decision.outcome(), DeliveryOutcome::Verified);
    assert!(decision.unmet_observations.is_empty());
}

/// 多验收项只完成一项：保留已验证项并列出剩余项，不得整体 Verified。
#[test]
fn multi_criterion_partial_completion_is_not_verified() {
    let facts = with_verification(
        with_change(facts(["c1", "c2"]), &["c1", "c2"]),
        "c1",
    );
    let decision = evaluate_delivery(&facts);
    assert!(!decision.is_completed());
    assert_eq!(decision.outcome(), DeliveryOutcome::PartialDelivery);
    assert_eq!(decision.unverified_criteria(), vec!["c2"]);
    let report = decision.into_report();
    assert_eq!(report.criteria.len(), 2);
    assert!(report.criteria[0].satisfied);
    assert!(!report.criteria[1].satisfied);
}

/// 取消优先于一切证据：用户取消就是 Cancelled，不是 Verified 也不是系统失败。
#[test]
fn cancel_wins_over_complete_evidence() {
    let mut facts = with_verification(with_change(facts(["c1"]), &["c1"]), "c1");
    facts.stop = StopCause::UserCancelled;
    let decision = evaluate_delivery(&facts);
    assert_eq!(decision.status, DeliveryStatus::Cancelled);
    assert_eq!(decision.outcome(), DeliveryOutcome::Cancelled);
}

/// Provider 错误文本不是模型回答，证据再完整也只能是运行时阻塞。
#[test]
fn provider_error_blocks_regardless_of_evidence() {
    let mut facts = with_verification(with_change(facts(["c1"]), &["c1"]), "c1");
    facts.stop = StopCause::ProviderError("stream reset".into());
    let decision = evaluate_delivery(&facts);
    assert!(matches!(decision.status, DeliveryStatus::Blocked(_)));
    assert_ne!(decision.outcome(), DeliveryOutcome::Verified);
}

/// 预算触顶必须诚实暂停并保留可恢复语义，不凭重复搜索换取无限预算。
#[test]
fn budget_exhaustion_never_reports_verified() {
    for cause in [StopCause::HardBudgetCeiling, StopCause::BudgetWindowExhausted] {
        let mut facts = facts(["c1"]);
        facts.stop = cause;
        facts.solver_claims_complete = true;
        let decision = evaluate_delivery(&facts);
        assert!(!decision.is_completed());
        assert_eq!(decision.outcome(), DeliveryOutcome::SystemFailure);
        assert!(matches!(decision.status, DeliveryStatus::Paused(_)));
    }
}

/// 非预算类硬熔断仍是 `Interrupted`，并保留具体原因；统一裁决不得把它并进
/// 预算耗尽的 `SystemFailure`。
#[test]
fn hard_stop_keeps_interrupted_semantics_with_reason() {
    let mut facts = facts(["c1"]);
    facts.stop = StopCause::HardStop("重复调用保护".into());
    let decision = evaluate_delivery(&facts);
    assert!(!decision.is_completed());
    assert_eq!(decision.outcome(), DeliveryOutcome::Interrupted);
    let report = decision.into_report();
    assert!(report
        .reason
        .is_some_and(|reason| reason.contains("重复调用保护")));
}

/// 已定位到实现时，「实现在哪」是 Agent 的技术问题，不得伪装成产品决策回抛用户。
#[test]
fn technical_blocking_point_is_not_rewritten_as_user_question() {
    let mut facts = facts(["c1"]);
    facts.has_execution_evidence = true;
    facts.stop = StopCause::NeedsUserDecision("请提供文件路径".into());
    let decision = evaluate_delivery(&facts);
    assert!(matches!(decision.status, DeliveryStatus::Paused(_)));
    assert_ne!(decision.outcome(), DeliveryOutcome::NeedsUserInput);
}

/// 缺少执行证据时的真实产品决策仍然形成问项，不被吞掉。
#[test]
fn genuine_user_decision_still_surfaces() {
    let mut facts = facts(["c1"]);
    facts.has_execution_evidence = false;
    facts.stop = StopCause::NeedsUserDecision("要保留旧口径还是新口径？".into());
    let decision = evaluate_delivery(&facts);
    assert!(matches!(decision.status, DeliveryStatus::NeedsUserDecision(_)));
    assert_eq!(decision.outcome(), DeliveryOutcome::NeedsUserInput);
}

/// 验证命令通过但零改动：这是基线自证，不是交付。
#[test]
fn baseline_verification_without_change_is_not_delivery() {
    let mut facts = facts(["c1"]);
    facts.stop = StopCause::BaselineVerifiedWithoutChange;
    facts = with_verification(facts, "c1");
    let decision = evaluate_delivery(&facts);
    assert_eq!(
        decision.outcome(),
        DeliveryOutcome::PartialDelivery,
        "零改动的基线验证不能收敛为完成"
    );
}

/// 纯解释与只读诊断由已读取来源验收，不要求写入。
#[test]
fn read_only_answer_completes_without_write() {
    for outcome in [RequestedOutcome::Answer, RequestedOutcome::Diagnose] {
        let mut facts = facts(["c1"]);
        facts.requested_outcome = outcome;
        facts.evidence_requirement = EvidenceRequirement::Explanation;
        facts.requires_verification = false;
        facts.requires_workspace_change = false;
        facts.read_evidence = vec!["已读取报错上下文".into()];
        let decision = evaluate_delivery(&facts);
        assert!(decision.is_completed(), "{outcome:?} 应由读取证据验收");
        assert_eq!(decision.outcome(), DeliveryOutcome::Verified);
    }
}

/// 未知请求不得默认可直接回答：没有可判定证据时不能成为 Completed。
#[test]
fn undetermined_request_is_not_completed_by_default() {
    let mut facts = facts(["c1"]);
    facts.requested_outcome = RequestedOutcome::Undetermined;
    facts.evidence_requirement = EvidenceRequirement::Explanation;
    facts.requires_verification = false;
    facts.requires_workspace_change = false;
    facts.read_evidence = vec!["搜索命中 3 处".into()];
    let decision = evaluate_delivery(&facts);
    assert!(!decision.is_completed());
}

/// 只读 Verified 报告必须携带证据锚点：不得出现 satisfied=true 而 evidence/verification 为空。
#[test]
fn read_only_verified_report_carries_evidence() {
    let mut facts = facts(["c1"]);
    facts.requested_outcome = RequestedOutcome::Diagnose;
    facts.evidence_requirement = EvidenceRequirement::Explanation;
    facts.requires_verification = false;
    facts.requires_workspace_change = false;
    facts.read_evidence = vec!["已读取报错上下文并定位根因".into()];
    let decision = evaluate_delivery(&facts);
    assert!(decision.is_completed());
    let report = decision.into_report();
    assert_eq!(report.outcome, DeliveryOutcome::Verified);
    assert!(report.criteria[0].satisfied);
    assert!(
        !report.criteria[0].evidence.is_empty(),
        "只读验收项须携带读取证据"
    );
    assert!(
        !report.verification.is_empty(),
        "Verified 报告须有非空验证摘要"
    );
}

/// Git 提交按命令结果验收，不进入源码修改闭环。
#[test]
fn repository_operation_is_accepted_by_command_not_write() {
    let mut facts = facts(["c1"]);
    facts.requested_outcome = RequestedOutcome::RepositoryOperation;
    facts.evidence_requirement = EvidenceRequirement::Command;
    facts.requires_workspace_change = false;
    facts = with_verification(facts, "c1");
    let decision = evaluate_delivery(&facts);
    assert!(decision.is_completed());
    assert_eq!(decision.unmet_observations.len(), 0);
    assert!(facts.changed_criteria.is_empty());
}

/// 相同输入必须得到相同裁决：终态不能依赖时钟、工作区或日志读取顺序。
#[test]
fn arbitration_is_deterministic() {
    let facts = with_verification(with_change(facts(["c1"]), &["c1"]), "c1");
    let first = evaluate_delivery(&facts);
    let second = evaluate_delivery(&facts);
    assert_eq!(first.status, second.status);
    assert_eq!(first.outcome(), second.outcome());
    assert_eq!(
        first.verification_summaries(),
        second.verification_summaries()
    );
}

/// 报告与裁决必须同源：不允许出现 outcome=Verified 而验收项 satisfied=false 的报告。
#[test]
fn report_is_consistent_with_arbitration() {
    for mut candidate in [
        facts(["c1"]),
        with_change(facts(["c1"]), &["c1"]),
        with_verification(with_change(facts(["c1"]), &["c1"]), "c1"),
    ] {
        candidate.solver_claims_complete = true;
        let decision = evaluate_delivery(&candidate);
        let report = decision.into_report();
        if report.outcome == DeliveryOutcome::Verified {
            assert!(report
                .criteria
                .iter()
                .all(|item| item.satisfied && !item.evidence.is_empty()));
            assert!(!report.verification.is_empty());
        } else {
            assert!(report
                .criteria
                .iter()
                .any(|item| !item.satisfied || item.evidence.is_empty()));
        }
    }
}
