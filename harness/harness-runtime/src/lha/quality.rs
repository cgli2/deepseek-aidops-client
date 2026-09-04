//! Deterministic P0 compile/test/invariant gate over audited hard facts.

use serde_json::Value;

use super::FactMatrix;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateFailure {
    pub key: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateResult {
    pub passed: bool,
    pub failures: Vec<GateFailure>,
}

pub struct QualityGate;

impl QualityGate {
    /// P0 gate: compilation must pass, tests must have zero failures, and every required
    /// invariant key must be the boolean value `true`. Missing facts fail closed.
    pub fn evaluate(matrix: &FactMatrix, invariant_keys: &[String]) -> GateResult {
        let mut failures = Vec::new();
        require_value(
            matrix,
            "build.status",
            &Value::String("passed".into()),
            &mut failures,
        );
        require_value(matrix, "tests.failed", &Value::from(0), &mut failures);
        require_value(matrix, "tests.skipped", &Value::from(0), &mut failures);
        for key in invariant_keys {
            require_value(matrix, key, &Value::Bool(true), &mut failures);
        }
        GateResult {
            passed: failures.is_empty(),
            failures,
        }
    }
}

fn require_value(
    matrix: &FactMatrix,
    key: &str,
    expected: &Value,
    failures: &mut Vec<GateFailure>,
) {
    match matrix.get_hard(key) {
        Some(fact) if &fact.value == expected => {}
        Some(fact) => failures.push(GateFailure {
            key: key.into(),
            message: format!("expected {expected}, got {}", fact.value),
        }),
        None => failures.push(GateFailure {
            key: key.into(),
            message: "required audited fact is missing".into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;
    use crate::lha::{ArtifactVerifier, HardFact};

    #[test]
    fn missing_or_failed_facts_fail_closed() {
        let matrix = FactMatrix::default();
        let result = QualityGate::evaluate(&matrix, &["invariant.api".into()]);
        assert!(!result.passed);
        assert_eq!(result.failures.len(), 4);
    }

    #[test]
    fn audited_green_facts_pass() {
        let root = std::env::temp_dir().join(format!("lha_gate_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let report = root.join("report.json");
        fs::write(&report, "green").unwrap();
        let verifier = ArtifactVerifier::new(&root, "verifier").unwrap();
        let evidence = verifier.evidence_for(&report).unwrap();
        let mut matrix = FactMatrix::default();
        for (key, value) in [
            ("build.status", json!("passed")),
            ("tests.failed", json!(0)),
            ("tests.skipped", json!(0)),
            ("invariant.api", json!(true)),
        ] {
            matrix
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
        assert!(QualityGate::evaluate(&matrix, &["invariant.api".into()]).passed);
        fs::remove_dir_all(root).ok();
    }
}
