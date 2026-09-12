//! Regression coverage for the 2026-09-12 monitoring review.
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
fn long_chinese_conclusion_does_not_panic() {
    assert!(std::panic::catch_unwind(|| hot_guard::conclusion_fingerprint(&"中".repeat(100))).is_ok());
}
#[test]
fn wal_restart_preserves_sequence() {
    let p = root();
    for _ in 0..2 {
        let mut w = WalWriter::open(&p, Arc::new(WalStats::default())).unwrap();
        w.append(event(EventKind::TurnFinished, "s"));
        w.flush();
    }
    let (events, _) = read_wal(&p.join("events-000001.jsonl")).unwrap();
    assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 2]);
}
#[test]
fn low_risk_flag_never_authorizes_code() {
    assert!(governor().authorize(&[ChangeKind::CodeOrBusinessRule { description: "code patch".into() }]).is_err());
}
#[test]
fn bundle_ids_are_immutable() {
    let p = root();
    let first = PolicyBundle::stage(&p, "same", None, "a".into(), vec![], &[("policy".into(), b"old".to_vec())], 1).unwrap();
    assert!(PolicyBundle::stage(&p, "same", None, "a".into(), vec![], &[("policy".into(), b"new".to_vec())], 2).is_err());
    assert_eq!(fs::read(first.dir.join("policy")).unwrap(), b"old");
}
#[test]
fn activation_rejects_tampered_unevaluated_bundle() {
    let p = root();
    let b = PolicyBundle::stage(&p.join("bundles"), "b", None, "a".into(), vec![], &[("policy".into(), b"old".to_vec())], 1).unwrap();
    fs::write(b.dir.join("policy"), "tampered").unwrap();
    assert!(PolicyBundle::load_verified(&b.dir).is_err());
    let grad = Graduator::new(p.join("state"), governor());
    assert!(grad.activate(&b, 2).is_err());
}
#[test]
fn duplicate_failure_cannot_reactivate_quarantined_bundle() {
    let p = root();
    let grad = Graduator::new(p.join("state"), governor());
    fs::create_dir_all(p.join("state")).unwrap();
    fs::write(p.join("state/active-manifest.json"), serde_json::to_vec(&ActiveManifest {
        active_bundle_id: "bad".into(), previous_stable_id: Some("stable".into()), activated_ms: 1,
    }).unwrap()).unwrap();
    grad.record_canary_outcome("bad", false, "regression", 2).unwrap();
    assert_eq!(grad.read_active().unwrap().active_bundle_id, "stable");
    grad.record_canary_outcome("bad", false, "duplicate", 3).unwrap();
    assert_eq!(grad.read_active().unwrap().active_bundle_id, "stable");
}
#[test]
fn simulated_repair_cannot_claim_a_real_build() {
    let p = root();
    let fault = FaultKind::ForcedError { step_index: 0, message: "failure".into() };
    let candidate = RepairCandidate {
        patch: PatchSpec { repair_id: "r".into(), rationale: "test".into(), created_ms: 1,
            files: vec![PatchFile { path: "lib.rs".into(), old_content: "fn f() {}".into(), new_content: "this is invalid rust !!!".into() }] },
        defect: fault.clone(),
    };
    let spec = ReplaySpec { spec_id: "s".into(), origin_session_id: "s".into(), description: "".into(),
        steps: vec![ReplayStep { step_id: 0, input_prompt: "p".into(), mocked_model_response: "r".into(), tool_calls: vec![] }], injected_faults: vec![fault] };
    assert!(matches!(SelfRepairPipeline::evaluate(&p, &candidate, &spec, &[], None, 1), Err(SelfRepairError::IndependentPipelineRequired)));
    assert!(!p.join(".self-repair").exists());
}
#[test]
fn patch_cannot_escape_isolated_worktree() {
    let p = root();
    let sandbox = p.join("sandbox");
    fs::create_dir_all(&sandbox).unwrap();
    fs::write(p.join("victim.txt"), "old").unwrap();
    let patch = PatchSpec { repair_id: "r".into(), rationale: "test".into(), created_ms: 1,
        files: vec![PatchFile { path: "../victim.txt".into(), old_content: "old".into(), new_content: "overwritten".into() }] };
    assert!(patch.apply_to(&sandbox).is_err());
    assert_eq!(fs::read_to_string(p.join("victim.txt")).unwrap(), "old");
}
#[test]
fn interleaved_sessions_keep_separate_incident_windows() {
    let events = vec![
        event(EventKind::RepeatedConclusion { fingerprint: "a".into(), repeat_count: 3 }, "a"),
        event(EventKind::RepeatedConclusion { fingerprint: "b".into(), repeat_count: 9 }, "b"),
        event(EventKind::GuardTerminated { reason: "a stopped".into() }, "a"),
    ];
    let book = IncidentBook::fold(&events);
    assert_eq!(book.incidents[0].session_id, "a");
    assert_eq!(book.incidents[0].max_repeat, 3);
}

#[tokio::test]
async fn priority_queue_preserves_delivery_when_normal_is_full() {
    let (tap, rx) = EventTap::new(1);
    tap.emit(AgentEventEnvelope::new("s", EventClass::ControlFlow, EventKind::TurnStarted, 1));
    tap.emit(AgentEventEnvelope::new("s", EventClass::ControlFlow, EventKind::TurnStarted, 2));
    tap.emit(AgentEventEnvelope::new("s", EventClass::ControlFlow, EventKind::TurnFinished, 3));
    assert!(matches!(rx.try_recv().unwrap().kind, EventKind::TurnFinished));
    assert_eq!(tap.stats().dropped(), 1);
}

#[test]
fn wal_recovers_partial_tail_and_rejects_corruption() {
    use std::io::Write;
    let p = root();
    {
        let mut w = WalWriter::open(&p, Arc::new(WalStats::default())).unwrap();
        w.append(event(EventKind::TurnFinished, "s"));
    }
    let path = p.join("events-000001.jsonl");
    fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"{half").unwrap();
    {
        let mut w = WalWriter::open(&p, Arc::new(WalStats::default())).unwrap();
        w.append(event(EventKind::TurnFinished, "s"));
    }
    let (events, bad) = read_wal(&path).unwrap();
    assert_eq!(bad, 0);
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].seq, 2);
    let raw = fs::read_to_string(&path).unwrap().replace("TurnFinished", "TurnStarted");
    fs::write(&path, raw).unwrap();
    assert!(WalWriter::open(&p, Arc::new(WalStats::default())).is_err());
}

#[test]
fn any_new_evidence_resets_old_conclusions() {
    let mut g = HotGuard::default();
    g.observe("a", 0);
    g.observe("a", 0);
    g.observe("b", 1);
    assert_eq!(g.observe("a", 0), GuardVerdict::Pass);
}

#[test]
fn replay_tool_timeout_is_not_silently_ignored() {
    let s = ReplaySpec { spec_id: "s".into(), origin_session_id: "s".into(), description: String::new(),
        steps: vec![ReplayStep { step_id: 0, input_prompt: "p".into(), mocked_model_response: "r".into(),
            tool_calls: vec![MockedToolCall { tool_name: "test".into(), arguments_json: "{}".into(), simulated_output: "ok".into() }] }],
        injected_faults: vec![FaultKind::ToolTimeout { tool_name: "test".into() }] };
    assert!(!DeterministicRunner::run(&s).success);
}

#[test]
fn bundle_rejects_windows_escape_forms_before_writes() {
    let p = root();
    for path in ["../outside", "C:/outside", ".. /outside", "NUL", "folder/../outside", "folder\\..\\outside"] {
        assert!(PolicyBundle::stage(&p, "bundle", None, "a".into(), vec![], &[(path.into(), b"data".to_vec())], 1).is_err(), "{path}");
        assert!(!p.join("bundle").exists());
    }
}

#[tokio::test]
async fn real_agent_emits_metadata_only_wal_and_observer_is_idempotent() {
    use harness_core::{AppContext, Config, Workspace, UserInput};
    use harness_llm::{Chunk, LlmProvider, ReplayLlm};
    use harness_capability::hook::{Hook, HookDecision, HookPayload};
    use harness_session::SessionLog;
    struct Allow;
    impl Hook for Allow {
        fn run(&self, _: &HookPayload) -> harness_core::Result<HookDecision> { Ok(HookDecision::Allow) }
    }
    let p = root();
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let mut config = Config::default();
    config.self_monitor.sidecar = false;
    let llm: Arc<dyn LlmProvider> = ReplayLlm::new(vec![Chunk { text: Some("你好".into()), ..Default::default() }]);
    let hook: Arc<dyn Hook> = Arc::new(Allow);
    let _regs = [ctx.provide(log), ctx.provide(Workspace::new(p.clone())), ctx.provide(Arc::new(config)),
        ctx.provide(llm), ctx.provide(hook), ctx.provide(harness_tool::ToolRegistry::new())];
    harness_runtime::AgentLoop::new().run_turn(&ctx, UserInput { text: "hello secret-password-test".into(), attachments: vec![] }).await.unwrap();
    let spool = fs::read_dir(p.join(".harness/self-monitor/spool")).unwrap().next().unwrap().unwrap().path();
    let path = spool.join("events-000001.jsonl");
    let mut complete = false;
    for _ in 0..100 {
        if let Ok((events, _)) = read_wal(&path) {
            if events.iter().any(|e| matches!(e.kind, EventKind::TurnFinished)) { complete = true; break; }
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(complete, "AgentLoop must emit real terminal events");
    let raw = fs::read_to_string(path).unwrap();
    assert!(!raw.contains("secret-password-test"));
    observer::collect(&spool).unwrap();
    let before = fs::read(spool.join("observer/report.json")).unwrap();
    observer::collect(&spool).unwrap();
    assert_eq!(before, fs::read(spool.join("observer/report.json")).unwrap());
}
