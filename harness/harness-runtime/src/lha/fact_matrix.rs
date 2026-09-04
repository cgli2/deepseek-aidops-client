//! P0 dual-track fact matrix and R2 evidence audit.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Summary,
    HardKv,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub artifact_path: PathBuf,
    pub sha256: String,
    pub verifier_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardFact {
    pub key: String,
    pub value: serde_json::Value,
    pub evidence: EvidenceRef,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactMatrix {
    pub summary_track: Vec<String>,
    pub hard_track: BTreeMap<String, HardFact>,
}

#[derive(Debug)]
pub enum FactError {
    InvalidFact(String),
    EvidenceOutsideRoot(PathBuf),
    EvidenceHashMismatch { expected: String, actual: String },
    VerifierMismatch { expected: String, actual: String },
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl std::fmt::Display for FactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidFact(value) => write!(f, "invalid hard fact: {value}"),
            Self::EvidenceOutsideRoot(path) => {
                write!(f, "evidence is outside verifier root: {}", path.display())
            }
            Self::EvidenceHashMismatch { expected, actual } => write!(
                f,
                "evidence hash mismatch: expected {expected}, actual {actual}"
            ),
            Self::VerifierMismatch { expected, actual } => write!(
                f,
                "evidence verifier mismatch: expected {expected}, actual {actual}"
            ),
            Self::Io(error) => write!(f, "fact store I/O error: {error}"),
            Self::Json(error) => write!(f, "fact store JSON error: {error}"),
        }
    }
}

impl std::error::Error for FactError {}

impl From<std::io::Error> for FactError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for FactError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

/// Independent verifier boundary for evidence-backed hard facts.
pub struct ArtifactVerifier {
    root: PathBuf,
    verifier_id: String,
}

impl ArtifactVerifier {
    pub fn new(root: impl AsRef<Path>, verifier_id: impl Into<String>) -> Result<Self, FactError> {
        let verifier_id = verifier_id.into();
        if verifier_id.trim().is_empty() {
            return Err(FactError::InvalidFact(
                "verifier_id must not be empty".into(),
            ));
        }
        Ok(Self {
            root: fs::canonicalize(root)?,
            verifier_id,
        })
    }

    pub fn evidence_for(&self, artifact: impl AsRef<Path>) -> Result<EvidenceRef, FactError> {
        let artifact_path = fs::canonicalize(artifact)?;
        self.ensure_in_root(&artifact_path)?;
        Ok(EvidenceRef {
            sha256: sha256_file(&artifact_path)?,
            artifact_path,
            verifier_id: self.verifier_id.clone(),
        })
    }

    pub fn verify(&self, evidence: &EvidenceRef) -> Result<(), FactError> {
        if evidence.verifier_id != self.verifier_id {
            return Err(FactError::VerifierMismatch {
                expected: self.verifier_id.clone(),
                actual: evidence.verifier_id.clone(),
            });
        }
        let artifact_path = fs::canonicalize(&evidence.artifact_path)?;
        self.ensure_in_root(&artifact_path)?;
        let actual = sha256_file(&artifact_path)?;
        if actual != evidence.sha256 {
            return Err(FactError::EvidenceHashMismatch {
                expected: evidence.sha256.clone(),
                actual,
            });
        }
        Ok(())
    }

    fn ensure_in_root(&self, path: &Path) -> Result<(), FactError> {
        if path.starts_with(&self.root) {
            Ok(())
        } else {
            Err(FactError::EvidenceOutsideRoot(path.to_path_buf()))
        }
    }
}

impl FactMatrix {
    pub fn write_summary(&mut self, entry: impl Into<String>) {
        self.summary_track.push(entry.into());
    }

    /// Accept a hard fact only after the independent verifier re-hashes its source.
    pub fn write_hard(
        &mut self,
        fact: HardFact,
        verifier: &ArtifactVerifier,
    ) -> Result<(), FactError> {
        if fact.key.trim().is_empty() {
            return Err(FactError::InvalidFact("key must not be empty".into()));
        }
        verifier.verify(&fact.evidence)?;
        self.hard_track.insert(fact.key.clone(), fact);
        Ok(())
    }

    pub fn get_hard(&self, key: &str) -> Option<&HardFact> {
        self.hard_track.get(key)
    }

    /// Persist through a sibling temporary file so readers never observe partial JSON.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), FactError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        super::storage::atomic_write(path, &bytes)?;
        Ok(())
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, FactError> {
        super::storage::recover_atomic(path.as_ref())?;
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }
}

/// Compare a compact `key=value` claim with the authoritative hard track.
pub fn cross_check(matrix: &FactMatrix, claim: &str) -> Option<String> {
    let (key, raw_value) = claim.split_once('=')?;
    let fact = matrix.get_hard(key.trim())?;
    let claimed = serde_json::from_str(raw_value.trim())
        .unwrap_or_else(|_| serde_json::Value::String(raw_value.trim().to_owned()));
    (claimed != fact.value).then(|| {
        format!(
            "summary claim {}={} conflicts with evidence-backed value {}",
            key.trim(),
            raw_value.trim(),
            fact.value
        )
    })
}

pub fn sha256_file(path: impl AsRef<Path>) -> Result<String, FactError> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture(tag: &str) -> (PathBuf, ArtifactVerifier, EvidenceRef) {
        let root = std::env::temp_dir().join(format!("lha_fact_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let artifact = root.join("verification.json");
        fs::write(&artifact, br#"{"build":"passed"}"#).unwrap();
        let verifier = ArtifactVerifier::new(&root, "verifier-1").unwrap();
        let evidence = verifier.evidence_for(&artifact).unwrap();
        (root, verifier, evidence)
    }

    #[test]
    fn hard_fact_requires_live_matching_evidence() {
        let (root, verifier, evidence) = fixture("evidence");
        let mut matrix = FactMatrix::default();
        matrix
            .write_hard(
                HardFact {
                    key: "build.status".into(),
                    value: json!("passed"),
                    evidence: evidence.clone(),
                },
                &verifier,
            )
            .unwrap();
        fs::write(&evidence.artifact_path, "tampered").unwrap();
        let error = matrix
            .write_hard(
                HardFact {
                    key: "tests.failed".into(),
                    value: json!(0),
                    evidence,
                },
                &verifier,
            )
            .unwrap_err();
        assert!(matches!(error, FactError::EvidenceHashMismatch { .. }));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn cross_check_reports_only_real_conflicts() {
        let (root, verifier, evidence) = fixture("claim");
        let mut matrix = FactMatrix::default();
        matrix
            .write_hard(
                HardFact {
                    key: "tests.failed".into(),
                    value: json!(3),
                    evidence,
                },
                &verifier,
            )
            .unwrap();
        assert!(cross_check(&matrix, "tests.failed=0").is_some());
        assert_eq!(cross_check(&matrix, "tests.failed=3"), None);
        assert_eq!(cross_check(&matrix, "unknown=1"), None);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn save_load_roundtrip_is_atomic_at_the_file_boundary() {
        let (root, verifier, evidence) = fixture("save");
        let mut matrix = FactMatrix::default();
        matrix
            .write_hard(
                HardFact {
                    key: "build.status".into(),
                    value: json!("passed"),
                    evidence,
                },
                &verifier,
            )
            .unwrap();
        let path = root.join("facts.json");
        matrix.save(&path).unwrap();
        assert_eq!(FactMatrix::load(path).unwrap(), matrix);
        fs::remove_dir_all(root).ok();
    }
}
