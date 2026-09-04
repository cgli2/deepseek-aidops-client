use std::fs;

use harness_runtime::{
    ArtifactVerifier, DurableDag, FactMatrix, HardFact, QualityGate, SandboxTx, TaskSpec,
    TaskStatus,
};
use serde_json::json;

#[test]
fn p0_transaction_to_verified_artifact_survives_restart() {
    let root = std::env::temp_dir().join(format!("lha_p0_e2e_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let workspace = root.join("workspace");
    let control = root.join("control");
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::create_dir_all(&control).unwrap();
    fs::write(workspace.join("src/lib.rs"), "pub fn value() -> u8 { 1 }").unwrap();

    let wal = control.join("dag.jsonl");
    let mut dag = DurableDag::open(&wal).unwrap();
    dag.create_task(TaskSpec {
        task_id: "implementation".into(),
        parent_id: Some("p0".into()),
        dependencies: vec![],
        inputs: json!({"workspace": workspace}),
        invariants: vec!["invariant.public_api".into()],
        expected_output_schema: json!({"type": "directory"}),
        timeout_seconds: 900,
        max_retries: 2,
    })
    .unwrap();
    dag.refresh_ready().unwrap();
    dag.schedule("implementation", "worker-1").unwrap();
    dag.start("implementation", "worker-1", 1_000, 180_000)
        .unwrap();

    let tx = SandboxTx::stage(&workspace).unwrap();
    fs::write(
        tx.shadow_path().join("src/lib.rs"),
        "pub fn value() -> u8 { 2 }",
    )
    .unwrap();
    tx.commit().unwrap();
    dag.checkpoint("implementation", "workspace-commit-1")
        .unwrap();

    let report = control.join("verification.json");
    fs::write(
        &report,
        serde_json::to_vec(&json!({
            "build": "passed",
            "failed": 0,
            "skipped": 0,
            "public_api": true
        }))
        .unwrap(),
    )
    .unwrap();
    let verifier = ArtifactVerifier::new(&control, "verifier-1").unwrap();
    let evidence = verifier.evidence_for(&report).unwrap();
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
    facts.save(control.join("facts.json")).unwrap();
    assert!(QualityGate::evaluate(&facts, &["invariant.public_api".into()]).passed);

    dag.begin_validation("implementation").unwrap();
    dag.complete("implementation", "vault://p0/workspace-commit-1")
        .unwrap();
    drop(dag);

    let restored = DurableDag::open(&wal).unwrap();
    assert!(matches!(
        restored.task("implementation").unwrap().status,
        TaskStatus::Succeeded { .. }
    ));
    assert!(
        fs::read_to_string(workspace.join("src/lib.rs"))
            .unwrap()
            .contains("{ 2 }")
    );
    fs::remove_dir_all(root).ok();
}
