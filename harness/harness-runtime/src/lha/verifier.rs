//! P2 independent command verifier. Commands are explicit argv vectors; no shell is used.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::process::Command;

use super::{ArtifactVerifier, FactError, FactMatrix, HardFact};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerificationKind {
    Compile,
    Test,
    Invariant { fact_key: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationSpec {
    pub check_id: String,
    pub kind: VerificationKind,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub working_directory: PathBuf,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckReport {
    pub check_id: String,
    pub kind: VerificationKind,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
}

impl CheckReport {
    pub fn passed(&self) -> bool {
        !self.timed_out && self.exit_code == Some(0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationReport {
    pub verifier_id: String,
    pub started_at_ms: u64,
    pub finished_at_ms: u64,
    pub checks: Vec<CheckReport>,
}

#[derive(Debug)]
pub enum VerifierError {
    Io(std::io::Error),
    Json(serde_json::Error),
    InvalidSpec(String),
    WorkingDirectoryOutsideRoot(PathBuf),
    ReportOutsideRoot(PathBuf),
}

impl std::fmt::Display for VerifierError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "verifier I/O error: {error}"),
            Self::Json(error) => write!(f, "verifier JSON error: {error}"),
            Self::InvalidSpec(error) => write!(f, "invalid verifier spec: {error}"),
            Self::WorkingDirectoryOutsideRoot(path) => write!(
                f,
                "verifier working directory is outside allowed root: {}",
                path.display()
            ),
            Self::ReportOutsideRoot(path) => write!(
                f,
                "verifier report path is outside allowed root: {}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for VerifierError {}

impl From<std::io::Error> for VerifierError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for VerifierError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

pub struct IndependentVerifier {
    verifier_id: String,
    allowed_root: PathBuf,
}

impl IndependentVerifier {
    pub fn new(
        verifier_id: impl Into<String>,
        allowed_root: impl AsRef<Path>,
    ) -> Result<Self, VerifierError> {
        let verifier_id = verifier_id.into();
        if verifier_id.trim().is_empty() {
            return Err(VerifierError::InvalidSpec("verifier id is required".into()));
        }
        Ok(Self {
            verifier_id,
            allowed_root: fs::canonicalize(allowed_root)?,
        })
    }

    pub async fn run(
        &self,
        specs: &[VerificationSpec],
        report_path: impl AsRef<Path>,
    ) -> Result<VerificationReport, VerifierError> {
        if specs.is_empty() {
            return Err(VerifierError::InvalidSpec(
                "at least one verification command is required".into(),
            ));
        }
        let started_at_ms = super::now_ms();
        let mut checks = Vec::with_capacity(specs.len());
        for spec in specs {
            checks.push(self.run_one(spec).await?);
        }
        let report = VerificationReport {
            verifier_id: self.verifier_id.clone(),
            started_at_ms,
            finished_at_ms: super::now_ms(),
            checks,
        };
        let report_path = self.validate_output_path(report_path.as_ref())?;
        super::storage::atomic_write(report_path, &serde_json::to_vec_pretty(&report)?)?;
        Ok(report)
    }

    pub fn write_facts(
        &self,
        report: &VerificationReport,
        report_path: impl AsRef<Path>,
        matrix: &mut FactMatrix,
    ) -> Result<(), FactError> {
        if report.verifier_id != self.verifier_id {
            return Err(FactError::InvalidFact(
                "verification report is attributed to another verifier".into(),
            ));
        }
        let report_path = fs::canonicalize(report_path)?;
        if !report_path.starts_with(&self.allowed_root) {
            return Err(FactError::EvidenceOutsideRoot(report_path));
        }
        let persisted: VerificationReport = serde_json::from_slice(&fs::read(&report_path)?)?;
        if &persisted != report {
            return Err(FactError::InvalidFact(
                "verification report does not match its persisted evidence".into(),
            ));
        }
        let verifier = ArtifactVerifier::new(&self.allowed_root, &self.verifier_id)?;
        let evidence = verifier.evidence_for(&report_path)?;
        let compile_checks: Vec<&CheckReport> = report
            .checks
            .iter()
            .filter(|check| check.kind == VerificationKind::Compile)
            .collect();
        let test_checks: Vec<&CheckReport> = report
            .checks
            .iter()
            .filter(|check| check.kind == VerificationKind::Test)
            .collect();
        let compile_passed =
            !compile_checks.is_empty() && compile_checks.iter().all(|check| check.passed());
        let test_failures = test_checks.iter().filter(|check| !check.passed()).count() as u64;
        let skipped_tests = u64::from(test_checks.is_empty());
        for (key, value) in [
            (
                "build.status".to_owned(),
                json!(if compile_passed { "passed" } else { "failed" }),
            ),
            ("tests.failed".to_owned(), json!(test_failures)),
            ("tests.skipped".to_owned(), json!(skipped_tests)),
        ] {
            matrix.write_hard(
                HardFact {
                    key,
                    value,
                    evidence: evidence.clone(),
                },
                &verifier,
            )?;
        }
        for check in &report.checks {
            if let VerificationKind::Invariant { fact_key } = &check.kind {
                matrix.write_hard(
                    HardFact {
                        key: fact_key.clone(),
                        value: json!(check.passed()),
                        evidence: evidence.clone(),
                    },
                    &verifier,
                )?;
            }
        }
        Ok(())
    }

    async fn run_one(&self, spec: &VerificationSpec) -> Result<CheckReport, VerifierError> {
        if spec.check_id.trim().is_empty() || spec.timeout_ms == 0 {
            return Err(VerifierError::InvalidSpec(
                "check id and positive timeout are required".into(),
            ));
        }
        let working_directory = fs::canonicalize(&spec.working_directory)?;
        if !working_directory.starts_with(&self.allowed_root) {
            return Err(VerifierError::WorkingDirectoryOutsideRoot(
                working_directory,
            ));
        }
        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .current_dir(working_directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let output =
            tokio::time::timeout(Duration::from_millis(spec.timeout_ms), command.output()).await;
        match output {
            Ok(result) => {
                let output = result?;
                Ok(CheckReport {
                    check_id: spec.check_id.clone(),
                    kind: spec.kind.clone(),
                    exit_code: output.status.code(),
                    timed_out: false,
                    stdout: bounded_output(&output.stdout),
                    stderr: bounded_output(&output.stderr),
                })
            }
            Err(_) => Ok(CheckReport {
                check_id: spec.check_id.clone(),
                kind: spec.kind.clone(),
                exit_code: None,
                timed_out: true,
                stdout: String::new(),
                stderr: "verification timed out".into(),
            }),
        }
    }

    fn validate_output_path<'a>(&self, path: &'a Path) -> Result<&'a Path, VerifierError> {
        let parent = path
            .parent()
            .ok_or_else(|| VerifierError::ReportOutsideRoot(path.to_path_buf()))?;
        fs::create_dir_all(parent)?;
        let parent = fs::canonicalize(parent)?;
        if !parent.starts_with(&self.allowed_root) {
            return Err(VerifierError::ReportOutsideRoot(path.to_path_buf()));
        }
        Ok(path)
    }
}

fn bounded_output(bytes: &[u8]) -> String {
    const LIMIT: usize = 64 * 1024;
    let start = bytes.len().saturating_sub(LIMIT);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn verifier_runs_without_shell_and_emits_audited_gate_facts() {
        let root = std::env::temp_dir().join(format!("lha_verifier_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let executable = std::env::current_exe().unwrap();
        let specs = vec![
            VerificationSpec {
                check_id: "compile".into(),
                kind: VerificationKind::Compile,
                program: executable.clone(),
                args: vec!["--list".into()],
                working_directory: root.clone(),
                timeout_ms: 10_000,
            },
            VerificationSpec {
                check_id: "tests".into(),
                kind: VerificationKind::Test,
                program: executable,
                args: vec!["--list".into()],
                working_directory: root.clone(),
                timeout_ms: 10_000,
            },
        ];
        let verifier = IndependentVerifier::new("verifier-1", &root).unwrap();
        let report_path = root.join("report.json");
        let report = verifier.run(&specs, &report_path).await.unwrap();
        assert!(report.checks.iter().all(CheckReport::passed));
        let mut matrix = FactMatrix::default();
        verifier
            .write_facts(&report, &report_path, &mut matrix)
            .unwrap();
        assert_eq!(
            matrix.get_hard("build.status").unwrap().value,
            json!("passed")
        );
        assert_eq!(matrix.get_hard("tests.failed").unwrap().value, json!(0));

        let mut forged = report.clone();
        forged.checks[0].exit_code = Some(1);
        assert!(matches!(
            verifier.write_facts(&forged, &report_path, &mut FactMatrix::default()),
            Err(FactError::InvalidFact(_))
        ));
        fs::remove_dir_all(root).ok();
    }
}
