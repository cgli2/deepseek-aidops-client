//! Simulated reports must never authorize a binary release or rollback.
use harness_runtime::monitoring::*;
fn report() -> ReviewReport {
    ReviewReport { repair_id: "forged".into(), build_passed: true,
        baseline: BaselineComparison { spec_id: "s".into(), before_ok: false, after_ok: true },
        golden_set: vec![], fault_drill: None, all_passed: true, report_path: None }
}
#[test]
fn forged_report_and_approver_name_cannot_release() {
    let root = std::env::temp_dir().join(format!("repair-denied-{}", uuid::Uuid::new_v4()));
    assert!(matches!(SelfRepairPipeline::release(&root, &report(), "alice", "approved", vec![], "better".into(), 1),
        Err(SelfRepairError::IndependentPipelineRequired)));
    assert!(!root.exists());
}
#[test]
fn incomplete_and_unattended_reports_remain_denied() {
    let root = std::env::temp_dir();
    let mut r = report();
    r.all_passed = false;
    assert!(matches!(SelfRepairPipeline::release(&root, &r, "alice", "", vec![], "".into(), 1), Err(SelfRepairError::ReportNotApproved)));
    assert!(matches!(SelfRepairPipeline::release(&root, &report(), " ", "", vec![], "".into(), 1), Err(SelfRepairError::UnattendedReleaseDenied)));
}
#[test]
fn rollback_never_reports_success_by_toggling_a_record() {
    assert!(matches!(SelfRepairPipeline::rollback(&std::env::temp_dir(), "r"), Err(SelfRepairError::IndependentPipelineRequired)));
}
