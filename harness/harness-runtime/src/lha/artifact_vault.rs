//! P2 BLAKE3 content-addressed MVCC artifact vault and explicit convergence decisions.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub blake3: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactVersion {
    pub logical_key: String,
    pub generation: u64,
    pub artifact: ArtifactRef,
    pub parents: Vec<String>,
    pub producer: String,
    pub created_at_ms: u64,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MergeDecision {
    Aggregator {
        aggregator_id: String,
        reason: String,
    },
    Hitl {
        checkpoint_id: String,
        confirmed_by: String,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityRecord {
    pub logical_key: String,
    pub artifact_hash: String,
    pub decision: MergeDecision,
    pub decided_at_ms: u64,
}

#[derive(Debug)]
pub enum VaultError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Busy,
    InvalidHash(String),
    MissingObject(String),
    Conflict { expected: u64, actual: u64 },
    InvalidDecision(String),
    UnknownVersion { key: String, hash: String },
}

impl std::fmt::Display for VaultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "artifact vault I/O error: {error}"),
            Self::Json(error) => write!(f, "artifact vault JSON error: {error}"),
            Self::Busy => write!(f, "artifact vault metadata is locked by another writer"),
            Self::InvalidHash(hash) => write!(f, "invalid BLAKE3 hash: {hash}"),
            Self::MissingObject(hash) => write!(f, "missing artifact object: {hash}"),
            Self::Conflict { expected, actual } => {
                write!(
                    f,
                    "MVCC generation conflict: expected {expected}, actual {actual}"
                )
            }
            Self::InvalidDecision(reason) => write!(f, "invalid merge decision: {reason}"),
            Self::UnknownVersion { key, hash } => {
                write!(f, "artifact {hash} is not a version of {key}")
            }
        }
    }
}

impl std::error::Error for VaultError {}

impl From<std::io::Error> for VaultError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for VaultError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct VaultState {
    versions: BTreeMap<String, Vec<ArtifactVersion>>,
    authorities: BTreeMap<String, AuthorityRecord>,
}

pub struct ArtifactVault {
    root: PathBuf,
}

impl ArtifactVault {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, VaultError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(root.join("objects"))?;
        super::storage::recover_atomic(&root.join("refs.json"))?;
        Ok(Self { root })
    }

    pub fn put_bytes(&self, bytes: &[u8]) -> Result<ArtifactRef, VaultError> {
        let hash = blake3::hash(bytes).to_hex().to_string();
        let target = self.object_path(&hash)?;
        if target.exists() {
            return Ok(ArtifactRef {
                blake3: hash,
                size: bytes.len() as u64,
            });
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = target.with_extension(format!("{}.tmp", Uuid::new_v4()));
        let mut file = fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        match fs::rename(&temporary, &target) {
            Ok(()) => {}
            Err(_) if target.exists() => {
                let _ = fs::remove_file(&temporary);
            }
            Err(error) => return Err(VaultError::Io(error)),
        }
        Ok(ArtifactRef {
            blake3: hash,
            size: bytes.len() as u64,
        })
    }

    pub fn put_file(&self, path: impl AsRef<Path>) -> Result<ArtifactRef, VaultError> {
        self.put_bytes(&fs::read(path)?)
    }

    pub fn read(&self, artifact: &ArtifactRef) -> Result<Vec<u8>, VaultError> {
        let bytes = fs::read(self.object_path(&artifact.blake3)?)
            .map_err(|_| VaultError::MissingObject(artifact.blake3.clone()))?;
        let actual = blake3::hash(&bytes).to_hex().to_string();
        if actual != artifact.blake3 || bytes.len() as u64 != artifact.size {
            return Err(VaultError::InvalidHash(actual));
        }
        Ok(bytes)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn publish(
        &self,
        logical_key: &str,
        artifact: ArtifactRef,
        expected_generation: u64,
        parents: Vec<String>,
        producer: &str,
        metadata: BTreeMap<String, String>,
        now_ms: u64,
    ) -> Result<ArtifactVersion, VaultError> {
        if logical_key.trim().is_empty() || producer.trim().is_empty() {
            return Err(VaultError::InvalidDecision(
                "logical key and producer are required".into(),
            ));
        }
        if !self.object_path(&artifact.blake3)?.exists() {
            return Err(VaultError::MissingObject(artifact.blake3));
        }
        // Never admit a forged size or a corrupted pre-existing object into MVCC metadata.
        self.read(&artifact)?;
        self.with_lock(|state| {
            let versions = state.versions.entry(logical_key.into()).or_default();
            let actual = versions.last().map_or(0, |version| version.generation);
            if actual != expected_generation {
                return Err(VaultError::Conflict {
                    expected: expected_generation,
                    actual,
                });
            }
            for parent in &parents {
                if !versions
                    .iter()
                    .any(|version| &version.artifact.blake3 == parent)
                {
                    return Err(VaultError::UnknownVersion {
                        key: logical_key.into(),
                        hash: parent.clone(),
                    });
                }
            }
            let version = ArtifactVersion {
                logical_key: logical_key.into(),
                generation: actual + 1,
                artifact,
                parents,
                producer: producer.into(),
                created_at_ms: now_ms,
                metadata,
            };
            versions.push(version.clone());
            Ok(version)
        })
    }

    pub fn versions(&self, logical_key: &str) -> Result<Vec<ArtifactVersion>, VaultError> {
        Ok(self
            .load_state()?
            .versions
            .remove(logical_key)
            .unwrap_or_default())
    }

    /// R5: an aggregator normally selects the authority; ambiguous or irreversible
    /// convergence can instead provide an explicit HITL checkpoint decision.
    pub fn select_authoritative(
        &self,
        logical_key: &str,
        artifact_hash: &str,
        decision: MergeDecision,
        now_ms: u64,
    ) -> Result<AuthorityRecord, VaultError> {
        validate_decision(&decision)?;
        self.with_lock(|state| {
            let known = state.versions.get(logical_key).is_some_and(|versions| {
                versions
                    .iter()
                    .any(|version| version.artifact.blake3 == artifact_hash)
            });
            if !known {
                return Err(VaultError::UnknownVersion {
                    key: logical_key.into(),
                    hash: artifact_hash.into(),
                });
            }
            let record = AuthorityRecord {
                logical_key: logical_key.into(),
                artifact_hash: artifact_hash.into(),
                decision,
                decided_at_ms: now_ms,
            };
            state.authorities.insert(logical_key.into(), record.clone());
            Ok(record)
        })
    }

    pub fn authority(&self, logical_key: &str) -> Result<Option<AuthorityRecord>, VaultError> {
        Ok(self.load_state()?.authorities.remove(logical_key))
    }

    fn object_path(&self, hash: &str) -> Result<PathBuf, VaultError> {
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(VaultError::InvalidHash(hash.into()));
        }
        Ok(self.root.join("objects").join(&hash[..2]).join(hash))
    }

    fn state_path(&self) -> PathBuf {
        self.root.join("refs.json")
    }

    fn load_state(&self) -> Result<VaultState, VaultError> {
        let path = self.state_path();
        if !path.exists() {
            return Ok(VaultState::default());
        }
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }

    fn save_state(&self, state: &VaultState) -> Result<(), VaultError> {
        super::storage::atomic_write(&self.state_path(), &serde_json::to_vec_pretty(state)?)?;
        Ok(())
    }

    fn with_lock<T>(
        &self,
        operation: impl FnOnce(&mut VaultState) -> Result<T, VaultError>,
    ) -> Result<T, VaultError> {
        let _guard = VaultLock::acquire(self.root.join(".vault.lock"))?;
        let mut state = self.load_state()?;
        let result = operation(&mut state)?;
        self.save_state(&state)?;
        Ok(result)
    }
}

fn validate_decision(decision: &MergeDecision) -> Result<(), VaultError> {
    let valid = match decision {
        MergeDecision::Aggregator {
            aggregator_id,
            reason,
        } => !aggregator_id.trim().is_empty() && !reason.trim().is_empty(),
        MergeDecision::Hitl {
            checkpoint_id,
            confirmed_by,
            reason,
        } => {
            !checkpoint_id.trim().is_empty()
                && !confirmed_by.trim().is_empty()
                && !reason.trim().is_empty()
        }
    };
    valid
        .then_some(())
        .ok_or_else(|| VaultError::InvalidDecision("decision attribution is incomplete".into()))
}

struct VaultLock {
    path: PathBuf,
}

impl VaultLock {
    fn acquire(path: PathBuf) -> Result<Self, VaultError> {
        const LOCK_TTL_MS: u64 = 30_000;
        for _ in 0..2 {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    writeln!(
                        file,
                        "{} {}",
                        std::process::id(),
                        super::now_ms().saturating_add(LOCK_TTL_MS)
                    )?;
                    file.sync_all()?;
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let expired = fs::read_to_string(&path)
                        .ok()
                        .and_then(|value| value.split_whitespace().nth(1)?.parse::<u64>().ok())
                        .is_some_and(|expires_at| expires_at <= super::now_ms());
                    if expired {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    return Err(VaultError::Busy);
                }
                Err(error) => return Err(VaultError::Io(error)),
            }
        }
        Err(VaultError::Busy)
    }
}

impl Drop for VaultLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn blake3_reader(mut reader: impl Read) -> Result<(String, u64), VaultError> {
    let mut hasher = blake3::Hasher::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        size += read as u64;
        hasher.update(&buffer[..read]);
    }
    Ok((hasher.finalize().to_hex().to_string(), size))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault(tag: &str) -> (PathBuf, ArtifactVault) {
        let root = std::env::temp_dir().join(format!("lha_vault_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        (root.clone(), ArtifactVault::open(&root).unwrap())
    }

    #[test]
    fn objects_are_immutable_and_deduplicated() {
        let (root, vault) = vault("object");
        let first = vault.put_bytes(b"artifact").unwrap();
        let second = vault.put_bytes(b"artifact").unwrap();
        assert_eq!(first, second);
        assert_eq!(vault.read(&first).unwrap(), b"artifact");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn mvcc_rejects_stale_generation_and_records_aggregator_choice() {
        let (root, vault) = vault("mvcc");
        let one = vault.put_bytes(b"one").unwrap();
        let two = vault.put_bytes(b"two").unwrap();
        vault
            .publish(
                "module",
                one.clone(),
                0,
                vec![],
                "worker-1",
                BTreeMap::new(),
                1,
            )
            .unwrap();
        assert!(matches!(
            vault.publish(
                "module",
                two.clone(),
                0,
                vec![],
                "worker-2",
                BTreeMap::new(),
                2
            ),
            Err(VaultError::Conflict { actual: 1, .. })
        ));
        vault
            .publish(
                "module",
                two.clone(),
                1,
                vec![one.blake3],
                "worker-2",
                BTreeMap::new(),
                2,
            )
            .unwrap();
        vault
            .select_authoritative(
                "module",
                &two.blake3,
                MergeDecision::Aggregator {
                    aggregator_id: "aggregator-1".into(),
                    reason: "tests and benchmark prefer generation 2".into(),
                },
                3,
            )
            .unwrap();
        assert_eq!(
            vault.authority("module").unwrap().unwrap().artifact_hash,
            two.blake3
        );
        fs::remove_dir_all(root).ok();
    }
}
