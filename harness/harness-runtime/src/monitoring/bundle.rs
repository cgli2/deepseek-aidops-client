//! Phase 4：低风险灰度演进 —— 不可变 PolicyBundle 与演进等级门禁（§15 / §14）。
//!
//! 交付边界（设计 §768-782）：
//! - 不可变 PolicyBundle：一次写入完整目录，激活前校验 schema/哈希；
//! - 演进等级 E0..E5，禁止跨级；
//! - 仅 E4 允许低风险配置/阈值/已审核技能灰度；
//! - 代码补丁与业务规则默认停留在 E3，需人工晋级。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

use super::proposal::RiskTier;

/// 演进成熟度等级（§14）：能力逐级开放，不能跨级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EvolutionLevel {
    /// 仅记录与人工分析
    E0Observe = 0,
    /// 自动事故归并，只能写候选区
    E1Summarize = 1,
    /// 生成提案，只能写隔离区
    E2Propose = 2,
    /// 自动回放/测试/风险报告，只能操作沙箱
    E3Validate = 3,
    /// 低风险配置/阈值/已审核技能灰度，受策略限定
    E4LowRiskPromote = 4,
    /// 特定中风险策略自动晋级，需显式开启
    E5RestrictedEvolution = 5,
}

impl EvolutionLevel {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_uppercase().as_str() {
            "E0" => Some(Self::E0Observe),
            "E1" => Some(Self::E1Summarize),
            "E2" => Some(Self::E2Propose),
            "E3" => Some(Self::E3Validate),
            "E4" => Some(Self::E4LowRiskPromote),
            "E5" => Some(Self::E5RestrictedEvolution),
            _ => None,
        }
    }
}

/// Bundle 内允许灰度的变更类别（§768：仅预批准的低风险变化）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeKind {
    /// runtime-policy.toml 内的阈值（如 hot_guard.repeat_stop）
    Threshold { key: String, old: String, new: String },
    /// 低风险配置项（非门禁/非预算类的行为开关）
    ConfigToggle { key: String, old: String, new: String },
    /// 已通过审核的技能包晋升
    SkillPromotion { skill_id: String, from: String, to: String },
    /// 中风险策略变更（仅 E5）
    PolicyChange { description: String },
    /// 代码补丁 / 业务规则（默认禁自动晋级，需人工）
    CodeOrBusinessRule { description: String },
}

impl ChangeKind {
    /// 该变更所需的最低演进等级。
    pub fn required_level(&self) -> EvolutionLevel {
        match self {
            Self::Threshold { .. } | Self::ConfigToggle { .. } | Self::SkillPromotion { .. } => {
                EvolutionLevel::E4LowRiskPromote
            }
            Self::PolicyChange { .. } => EvolutionLevel::E5RestrictedEvolution,
            // 代码/业务规则默认不被任何等级自动放行（§563：默认始终停留在 E3）。
            Self::CodeOrBusinessRule { .. } => EvolutionLevel::E5RestrictedEvolution,
        }
    }

    /// 该变更映射的风险等级（用于门禁交叉校验）。
    pub fn risk_tier(&self) -> RiskTier {
        match self {
            Self::Threshold { .. } | Self::ConfigToggle { .. } => RiskTier::Low,
            Self::SkillPromotion { .. } => RiskTier::Medium,
            Self::PolicyChange { .. } => RiskTier::High,
            Self::CodeOrBusinessRule { .. } => RiskTier::Critical,
        }
    }
}

/// Bundle manifest：版本、父版本、变更清单、内容哈希。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleManifest {
    pub bundle_id: String,
    pub parent_id: Option<String>,
    pub schema_version: u32,
    pub created_ms: u64,
    pub changes: Vec<ChangeKind>,
    /// manifest.json 之外目录内容的整体哈希（sha256，hex）。
    pub content_hash: String,
    /// 创建者/批准人（§779 可追溯）。
    pub author: String,
}

impl BundleManifest {
    pub const SCHEMA_VERSION: u32 = 1;
}

/// 不可变 PolicyBundle（§568）：落盘后不再原地编辑。
pub struct PolicyBundle {
    pub dir: PathBuf,
    pub manifest: BundleManifest,
}

impl PolicyBundle {
    /// 计算 bundle 目录内容的确定性哈希（按相对路径排序后串联文件字节）。
    pub fn hash_dir(dir: &Path) -> std::io::Result<String> {
        let mut files: Vec<PathBuf> = Vec::new();
        collect_files(dir, dir, &mut files)?;
        files.sort();
        let mut hasher = Sha256::new();
        for rel in files {
            // manifest.json 在生成期尚未写入最终哈希，跳过以避免自引用。
            if rel == Path::new("manifest.json") {
                continue;
            }
            let path = rel.to_string_lossy().replace('\\', "/");
            let bytes = fs::read(super::storage::safe_path(dir, &path)?)?;
            hasher.update((path.len() as u64).to_le_bytes());
            hasher.update(path.as_bytes());
            hasher.update((bytes.len() as u64).to_le_bytes());
            hasher.update(bytes);
        }
        Ok(format!("{:x}", hasher.finalize()))
    }

    /// 在 `bundles_root/<bundle_id>/` 下物化 bundle：写入 payload 文件 + manifest。
    pub fn stage(
        bundles_root: &Path,
        bundle_id: &str,
        parent_id: Option<String>,
        author: String,
        changes: Vec<ChangeKind>,
        payload_files: &[(String, Vec<u8>)],
        created_ms: u64,
    ) -> std::io::Result<Self> {
        super::storage::validate_id(bundle_id)?;
        let dir = super::storage::safe_path(bundles_root, bundle_id)?;
        for (rel, _) in payload_files {
            super::storage::safe_path(&dir, rel)?;
            if rel == "manifest.json" { return Err(std::io::Error::other("manifest is reserved")); }
        }
        fs::create_dir_all(bundles_root)?;
        fs::create_dir(&dir)?;
        for (rel, bytes) in payload_files {
            let p = super::storage::safe_path(&dir, rel)?;
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&p, bytes)?;
        }
        let content_hash = Self::hash_dir(&dir)?;
        let manifest = BundleManifest {
            bundle_id: bundle_id.to_string(),
            parent_id,
            schema_version: BundleManifest::SCHEMA_VERSION,
            created_ms,
            changes,
            content_hash,
            author,
        };
        let mjson = serde_json::to_string_pretty(&manifest)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        fs::write(dir.join("manifest.json"), mjson)?;
        Ok(Self { dir, manifest })
    }

    /// 从磁盘加载并校验 manifest 与内容哈希一致。
    pub fn load_verified(dir: &Path) -> Result<Self, BundleError> {
        let mpath = super::storage::safe_path(dir, "manifest.json")
            .map_err(|e| BundleError::Io(e.to_string()))?;
        let mraw = fs::read_to_string(&mpath)
            .map_err(|e| BundleError::Io(format!("read manifest: {e}")))?;
        let manifest: BundleManifest = serde_json::from_str(&mraw)
            .map_err(|e| BundleError::InvalidManifest(format!("{e}")))?;
        if manifest.schema_version != BundleManifest::SCHEMA_VERSION {
            return Err(BundleError::UnsupportedSchema(manifest.schema_version));
        }
        let actual = Self::hash_dir(dir).map_err(|e| BundleError::Io(format!("hash: {e}")))?;
        if actual != manifest.content_hash {
            return Err(BundleError::HashMismatch {
                expected: manifest.content_hash.clone(),
                actual,
            });
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            manifest,
        })
    }
}

fn collect_files(root: &Path, cur: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(cur)? {
        let entry = entry?;
        let path = entry.path();
        super::storage::safe_path(root, &path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"))?;
        if path.is_dir() {
            collect_files(root, &path, out)?;
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.push(rel.to_path_buf());
        }
    }
    Ok(())
}

#[derive(Debug)]
pub enum BundleError {
    Io(String),
    InvalidManifest(String),
    UnsupportedSchema(u32),
    HashMismatch { expected: String, actual: String },
}

impl std::fmt::Display for BundleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {e}"),
            Self::InvalidManifest(e) => write!(f, "invalid manifest: {e}"),
            Self::UnsupportedSchema(v) => write!(f, "unsupported schema version: {v}"),
            Self::HashMismatch { expected, actual } => {
                write!(f, "content hash mismatch: expected {expected}, actual {actual}")
            }
        }
    }
}

impl std::error::Error for BundleError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload() -> Vec<(String, Vec<u8>)> {
        vec![
            (
                "runtime-policy.toml".to_string(),
                b"repeat_stop = 2\n".to_vec(),
            ),
            (
                "prompts/system.md".to_string(),
                b"be concise\n".to_vec(),
            ),
        ]
    }

    fn temp_root(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "harness-p4-bundle-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn stage_and_load_verified_roundtrip() {
        let root = temp_root("roundtrip");
        let b = PolicyBundle::stage(
            &root,
            "b-001",
            None,
            "tester".to_string(),
            vec![ChangeKind::Threshold {
                key: "repeat_stop".into(),
                old: "3".into(),
                new: "2".into(),
            }],
            &payload(),
            1000,
        )
        .unwrap();
        let loaded = PolicyBundle::load_verified(&b.dir).unwrap();
        assert_eq!(loaded.manifest.bundle_id, "b-001");
        assert_eq!(loaded.manifest.content_hash, b.manifest.content_hash);
    }

    #[test]
    fn tampering_breaks_verification() {
        let root = temp_root("tamper");
        let b = PolicyBundle::stage(
            &root,
            "b-002",
            None,
            "tester".to_string(),
            vec![],
            &payload(),
            1000,
        )
        .unwrap();
        // 篡改内容
        fs::write(b.dir.join("runtime-policy.toml"), b"repeat_stop = 99\n").unwrap();
        let r = PolicyBundle::load_verified(&b.dir);
        assert!(matches!(r, Err(BundleError::HashMismatch { .. })));
    }

    #[test]
    fn schema_version_gate() {
        let root = temp_root("schema");
        let b = PolicyBundle::stage(&root, "b-003", None, "t".into(), vec![], &payload(), 1)
            .unwrap();
        let mpath = b.dir.join("manifest.json");
        let mut m: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&mpath).unwrap()).unwrap();
        m["schema_version"] = serde_json::json!(999);
        fs::write(&mpath, serde_json::to_string_pretty(&m).unwrap()).unwrap();
        let r = PolicyBundle::load_verified(&b.dir);
        assert!(matches!(r, Err(BundleError::UnsupportedSchema(999))));
    }

    #[test]
    fn evolution_level_parse_and_order() {
        assert_eq!(EvolutionLevel::parse("E4"), Some(EvolutionLevel::E4LowRiskPromote));
        assert!(EvolutionLevel::E3Validate < EvolutionLevel::E4LowRiskPromote);
        assert!(EvolutionLevel::parse("E9").is_none());
    }
}
