use std::fs;

use harness_runtime::{
    Admission, ArtifactVerifier, CheckpointKind, CheckpointState, EffectClass, EffectProposal,
    FactMatrix, HardFact, LongHorizonRuntime, MergeDecision, PrepareOutcome, ProviderLimit,
    TaskSpec, TaskStatus, effect_payload_digest,
};
use serde_json::json;

#[test]
fn p1_p2_control_plane_delivers_and_recovers_authoritative_mvcc_artifact() {
    let root = std::env::temp_dir().join(format!("lha_p2_e2e_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let evidence_root = root.join("evidence");
    fs::create_dir_all(&evidence_root).unwrap();
    let runtime = LongHorizonRuntime::open(root.join("control"), 1_000).unwrap();
    runtime
        .register_provider(
            "primary",
            ProviderLimit {
                requests_per_minute: 60,
                tokens_per_minute: 60_000,
                request_burst: 4,
                token_burst: 1_000,
            },
            0,
        )
        .unwrap();
    runtime
        .submit(TaskSpec {
            task_id: "p2-delivery".into(),
            parent_id: None,
            dependencies: vec![],
            inputs: json!({"objective": "deliver immutable artifact"}),
            invariants: vec!["invariant.public_api".into()],
            expected_output_schema: json!({"type": "artifact"}),
            timeout_seconds: 900,
            max_retries: 2,
        })
        .unwrap();
    assert!(
        runtime
            .claim_next("worker-1", 1, 180_000)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        runtime.admit_llm("p2-delivery", "primary", 100, 2).unwrap(),
        Admission::Granted
    );

    let report_path = evidence_root.join("verification.json");
    fs::write(&report_path, "all independent checks passed").unwrap();
    let verifier = ArtifactVerifier::new(&evidence_root, "verifier-1").unwrap();
    let evidence = verifier.evidence_for(&report_path).unwrap();
    let mut facts = FactMatrix::default();
    for (key, value) in [
        ("build.status", json!("passed")),
        ("tests.failed", json!(0)),
        ("tests.skipped", json!(0)),
        ("invariant.public_api", json!(true)),
    ] {
        facts
            .write_hard(
                HardFact {
                    key: key.into(),
                    value,
                    evidence: evidence.clone(),
                },
                &verifier,
            )
            .unwrap();
    }
    let version = runtime
        .finalize(
            "p2-delivery",
            "release/main",
            b"immutable release payload",
            &facts,
            "aggregator-1",
            3,
        )
        .unwrap();
    runtime
        .request_decision(
            "release-choice",
            CheckpointKind::VersionConvergence,
            "release/main",
            version.artifact.blake3.as_bytes(),
            "vault://shadow/release-comparison",
            4,
        )
        .unwrap();
    runtime
        .approve_decision("release-choice", "release-manager", "verified candidate")
        .unwrap();
    runtime
        .select_authoritative(
            "release/main",
            &version.artifact.blake3,
            MergeDecision::Hitl {
                checkpoint_id: "release-choice".into(),
                confirmed_by: "release-manager".into(),
                reason: "shadow comparison approved".into(),
            },
            5,
        )
        .unwrap();
    drop(runtime);

    let restored = LongHorizonRuntime::open(root.join("control"), 999_999).unwrap();
    assert!(matches!(
        restored.task("p2-delivery").unwrap().unwrap().status,
        TaskStatus::Succeeded { .. }
    ));
    let authority = restored.vault().authority("release/main").unwrap().unwrap();
    assert_eq!(authority.artifact_hash, version.artifact.blake3);
    assert_eq!(
        restored.vault().read(&version.artifact).unwrap(),
        b"immutable release payload"
    );
    assert_eq!(restored.vault().versions("release/main").unwrap().len(), 1);
    let events = restored.blackboard_since(0).unwrap();
    assert_eq!(
        events
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        ["TaskCreated", "TaskStarted", "TaskCompleted"]
    );
    fs::remove_dir_all(root).ok();
}

#[test]
fn durable_hitl_binds_irreversible_effect_to_exact_payload() {
    let root = std::env::temp_dir().join(format!("lha_p2_effect_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let payload = br#"{"release":"v1.2.3","channel":"production"}"#;
    let runtime = LongHorizonRuntime::open(&root, 1_000).unwrap();
    runtime
        .request_decision(
            "publish-production",
            CheckpointKind::IrreversibleEffect,
            "publish release",
            payload,
            "vault://shadow/release-v1.2.3",
            1,
        )
        .unwrap();
    runtime
        .approve_decision(
            "publish-production",
            "release-manager",
            "shadow release verified",
        )
        .unwrap();
    let proposal = EffectProposal {
        proposal_id: "publish-v1.2.3".into(),
        action: "publish release".into(),
        payload_digest: effect_payload_digest(payload),
        class: EffectClass::Irreversible,
        idempotency_key: Some("release:v1.2.3:production".into()),
        compensation: None,
    };
    assert!(
        runtime
            .prepare_effect_from_checkpoint(&proposal, "publish-production", b"different payload")
            .is_err()
    );
    assert_eq!(
        runtime
            .prepare_effect_from_checkpoint(&proposal, "publish-production", payload)
            .unwrap(),
        PrepareOutcome::Prepared
    );
    drop(runtime);
    assert_eq!(
        LongHorizonRuntime::open(&root, 1_000)
            .unwrap()
            .prepare_effect_from_checkpoint(&proposal, "publish-production", payload)
            .unwrap(),
        PrepareOutcome::AlreadyPrepared
    );
    fs::remove_dir_all(root).ok();
}

#[test]
fn operator_can_list_and_reject_pending_checkpoints() {
    let root = std::env::temp_dir().join(format!("lha_p2_reject_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let runtime = LongHorizonRuntime::open(&root, 10_000).unwrap();
    runtime
        .request_decision(
            "checkpoint-1",
            CheckpointKind::Architecture,
            "database choice",
            b"postgres",
            "vault://shadow/benchmark",
            1,
        )
        .unwrap();

    let decisions = runtime.decisions().unwrap();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].checkpoint_id, "checkpoint-1");
    assert_eq!(decisions[0].state, CheckpointState::Pending);

    runtime
        .reject_decision("checkpoint-1", "operator", "benchmark regressed")
        .unwrap();
    assert!(matches!(
        runtime.decisions().unwrap()[0].state,
        CheckpointState::Rejected { .. }
    ));
    fs::remove_dir_all(root).ok();
}

#[test]
fn orchestrator_contract_lock_allows_bodies_and_blocks_public_signature_drift() {
    let root = std::env::temp_dir().join(format!("lha_p1_contract_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let source = workspace.join("lib.rs");
    fs::write(&source, "pub fn stable(input: u8) -> u8 { input + 1 }").unwrap();
    let runtime = LongHorizonRuntime::open(root.join("control"), 1_000).unwrap();
    runtime
        .capture_contract("contract-task", &workspace)
        .unwrap();

    fs::write(&source, "pub fn stable(input: u8) -> u8 { input + 2 }").unwrap();
    assert!(
        runtime
            .check_contract("contract-task", &workspace)
            .unwrap()
            .compatible()
    );
    fs::write(&source, "pub fn stable(input: u16) -> u16 { input + 2 }").unwrap();
    assert!(
        !runtime
            .check_contract("contract-task", &workspace)
            .unwrap()
            .compatible()
    );
    assert!(runtime.capture_contract("../escape", &workspace).is_err());
    fs::remove_dir_all(root).ok();
}

#[test]
fn exhausted_global_budget_persists_partial_delivery_terminal() {
    let root = std::env::temp_dir().join(format!("lha_p2_budget_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let runtime = LongHorizonRuntime::open(&root, 5).unwrap();
    runtime
        .register_provider(
            "primary",
            ProviderLimit {
                requests_per_minute: 60,
                tokens_per_minute: 600,
                request_burst: 1,
                token_burst: 100,
            },
            0,
        )
        .unwrap();
    runtime
        .submit(TaskSpec {
            task_id: "budgeted".into(),
            parent_id: None,
            dependencies: vec![],
            inputs: json!({}),
            invariants: vec![],
            expected_output_schema: json!({}),
            timeout_seconds: 60,
            max_retries: 1,
        })
        .unwrap();
    runtime.claim_next("worker", 0, 1_000).unwrap();
    assert_eq!(
        runtime.admit_llm("budgeted", "primary", 6, 1).unwrap(),
        Admission::GracefulExhaustion
    );
    assert!(matches!(
        runtime.task("budgeted").unwrap().unwrap().status,
        TaskStatus::BudgetExhausted { .. }
    ));
    drop(runtime);
    assert!(matches!(
        LongHorizonRuntime::open(&root, 1_000)
            .unwrap()
            .task("budgeted")
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::BudgetExhausted { .. }
    ));
    fs::remove_dir_all(root).ok();
}
