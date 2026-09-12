//! Temporary review reproducers: passing assertions confirm defects, not acceptance.
use harness_runtime::monitoring::*;
use harness_runtime::monitoring::wal::WalStats;
use std::{fs, path::PathBuf, sync::Arc};

fn root() -> PathBuf {
    let p = std::env::temp_dir().join(format!("monitor-review-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&p).unwrap();
    p
}
fn event(kind: EventKind, session: &str) -> AgentEventEnvelope {
    AgentEventEnvelope::new(session, EventClass::Anomaly, kind, 1)
}
fn governor() -> Governor {
    Governor::new(GovernorConfig {
        mode: "evolve".into(), max_level: EvolutionLevel::E5RestrictedEvolution,
        shadow_enabled: true, canary_percent: 0, auto_promote_low_risk: true,
    })
}
#[test]
fn repro_chinese_conclusion_panics() {
    assert!(std::panic::catch_unwind(|| hot_guard::conclusion_fingerprint(&"中".repeat(100))).is_err());
}
#[test]
fn repro_wal_restart_reuses_sequence() {
    let p = root();
    for _ in 0..2 {
        let mut w = WalWriter::open(&p, Arc::new(WalStats::default())).unwrap();
        w.append(event(EventKind::TurnFinished, "s"));
        w.flush();
    }
    let (events, _) = read_wal(&p.join("events-000001.jsonl")).unwrap();
    assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 1]);
}
#[test]
fn repro_code_authorized_by_low_risk_flag() {
    assert!(governor().authorize(&[ChangeKind::CodeOrBusinessRule { description: "code patch".into() }]).is_ok());
}
#[test]
fn repro_restage_overwrites_immutable_bundle() {
    let p = root();
    let first = PolicyBundle::stage(&p, "same", None, "a".into(), vec![], &[("policy".into(), b"old".to_vec())], 1).unwrap();
    PolicyBundle::stage(&p, "same", None, "a".into(), vec![], &[("policy".into(), b"new".to_vec())], 2).unwrap();
    assert_eq!(fs::read(first.dir.join("policy")).unwrap(), b"new");
}
#[test]
fn repro_activation_accepts_tampered_unevaluated_bundle() {
    let p = root();
    let b = PolicyBundle::stage(&p.join("bundles"), "b", None, "a".into(), vec![], &[("policy".into(), b"old".to_vec())], 1).unwrap();
    fs::write(b.dir.join("policy"), "tampered").unwrap();
    assert!(PolicyBundle::load_verified(&b.dir).is_err());
    let grad = Graduator::new(p.join("state"), governor());
    assert!(grad.activate(&b, 2).is_ok());
}
#[test]
fn repro_duplicate_failed_canary_reactivates_bad_bundle() {
    let p = root();
    let grad = Graduator::new(p.join("state"), governor());
    for id in ["stable", "bad"] {
        let b = PolicyBundle::stage(&p.join("bundles"), id, None, "a".into(), vec![], &[], 1).unwrap();
        grad.activate(&b, 1).unwrap();
    }
    grad.record_canary_outcome("bad", false, "regression", 2).unwrap();
    assert_eq!(grad.read_active().unwrap().active_bundle_id, "stable");
    grad.record_canary_outcome("bad", false, "duplicate", 3).unwrap();
    assert_eq!(grad.read_active().unwrap().active_bundle_id, "bad");
}
#[test]
fn repro_invalid_code_and_no_golden_tests_pass_review() {
    let p = root();
    let fault = FaultKind::ForcedError { step_index: 0, message: "failure".into() };
    let candidate = RepairCandidate {
        patch: PatchSpec { repair_id: "r".into(), rationale: "test".into(), created_ms: 1,
            files: vec![PatchFile { path: "lib.rs".into(), old_content: "fn f() {}".into(), new_content: "this is invalid rust !!!".into() }] },
        defect: fault.clone(),
    };
    let spec = ReplaySpec { spec_id: "s".into(), origin_session_id: "s".into(), description: "".into(),
        steps: vec![ReplayStep { step_id: 0, input_prompt: "p".into(), mocked_model_response: "r".into(), tool_calls: vec![] }], injected_faults: vec![fault] };
    let report = SelfRepairPipeline::evaluate(&p, &candidate, &spec, &[], None, 1).unwrap();
    assert!(report.build_passed && report.all_passed);
}
#[test]
fn repro_patch_can_write_outside_isolated_worktree() {
    let p = root();
    let sandbox = p.join("sandbox");
    fs::create_dir_all(&sandbox).unwrap();
    fs::write(p.join("victim.txt"), "old").unwrap();
    let patch = PatchSpec { repair_id: "r".into(), rationale: "test".into(), created_ms: 1,
        files: vec![PatchFile { path: "../victim.txt".into(), old_content: "old".into(), new_content: "overwritten".into() }] };
    patch.apply_to(&sandbox).unwrap();
    assert_eq!(fs::read_to_string(p.join("victim.txt")).unwrap(), "overwritten");
}
#[test]
fn repro_incident_attributes_another_sessions_anomaly() {
    let events = vec![
        event(EventKind::RepeatedConclusion { fingerprint: "a".into(), repeat_count: 3 }, "a"),
        event(EventKind::RepeatedConclusion { fingerprint: "b".into(), repeat_count: 9 }, "b"),
        event(EventKind::GuardTerminated { reason: "a stopped".into() }, "a"),
    ];
    let book = IncidentBook::fold(&events);
    assert_eq!(book.incidents[0].session_id, "a");
    assert_eq!(book.incidents[0].max_repeat, 9);
}
