//! Phase 4：低风险灰度演进 —— 门禁、Shadow/Canary、原子激活与自动回滚（§14 / §15 / §768-782）。
//!
//! 验收对应：
//! - 候选失败自动回滚且不影响运行中任务（`Graduator::record_canary_outcome` 失败 → `rollback`）；
//! - 可追溯"为何改、谁批准、测了什么、改善多少"（`PromotionRecord`）；
//! - 无法绕过 `max_level` 和风险分类（`Governor::authorize` 双校验）。

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use super::bundle::{BundleError, ChangeKind, EvolutionLevel, PolicyBundle};

/// 演进门禁配置（§682-688 默认值）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernorConfig {
    /// off | observe | protect | evolve
    pub mode: String,
    pub max_level: EvolutionLevel,
    pub shadow_enabled: bool,
    /// 0..=100
    pub canary_percent: u8,
    pub auto_promote_low_risk: bool,
}

impl Default for GovernorConfig {
    /// 首发默认（§690）：mode=protect、max_level=E2，回放稳定后再升 E3；E4 显式开启。
    fn default() -> Self {
        Self {
            mode: "protect".to_string(),
            max_level: EvolutionLevel::E2Propose,
            shadow_enabled: true,
            canary_percent: 0,
            auto_promote_low_risk: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateError {
    /// 模式不为 evolve，禁止任何灰度操作
    ModeNotEvolve { mode: String },
    /// 变更所需等级超过 max_level
    LevelExceeded {
        required: EvolutionLevel,
        max: EvolutionLevel,
    },
    /// 代码/业务规则默认禁自动晋级
    CodeRequiresHuman,
    PromotionUnavailable,
}

impl std::fmt::Display for GateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ModeNotEvolve { mode } => write!(f, "mode={mode} not evolve"),
            Self::LevelExceeded { required, max } => {
                write!(f, "required {required:?} exceeds max_level {max:?}")
            }
            Self::CodeRequiresHuman => write!(f, "code/business-rule changes require human approval"),
            Self::PromotionUnavailable => write!(f, "activation requires an independent evaluated release; automatic promotion is not available"),
        }
    }
}
impl std::error::Error for GateError {}

/// 演进门禁：单一授权入口，所有灰度操作必须经此。
pub struct Governor {
    pub config: GovernorConfig,
}

impl Governor {
    pub fn new(config: GovernorConfig) -> Self {
        Self { config }
    }

    /// 校验一组变更是否允许进入灰度（等级 + 风险 + 模式三重闸门）。
    pub fn authorize(&self, changes: &[ChangeKind]) -> Result<(), GateError> {
        if self.config.mode != "evolve" {
            return Err(GateError::ModeNotEvolve {
                mode: self.config.mode.clone(),
            });
        }
        for ch in changes {
            // 代码/业务规则默认禁自动晋级，优先于等级判定报出（§563）。
            if matches!(ch, ChangeKind::CodeOrBusinessRule { .. }) {
                return Err(GateError::CodeRequiresHuman);
            }
            let required = ch.required_level();
            if required > self.config.max_level {
                return Err(GateError::LevelExceeded {
                    required,
                    max: self.config.max_level,
                });
            }
        }
        if changes.is_empty() || !self.config.auto_promote_low_risk {
            return Err(GateError::PromotionUnavailable);
        }
        if changes.iter().any(|ch| !matches!(ch,
            ChangeKind::Threshold { key, .. } if key == "repeat_stop" || key == "repeat_warning")) {
            return Err(GateError::PromotionUnavailable);
        }
        Ok(())
    }

    /// 给定 canary_percent 与稳定的会话哈希，判定本次任务是否落入灰度桶。
    pub fn in_canary_bucket(&self, session_id: &str) -> bool {
        if self.config.canary_percent == 0 {
            return false;
        }
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in session_id.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x1000_0000_01b3);
        }
        (h % 100) < self.config.canary_percent as u64
    }
}

/// 活动 manifest 指针（§15 step 3：原子更新）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveManifest {
    pub active_bundle_id: String,
    pub previous_stable_id: Option<String>,
    pub activated_ms: u64,
}

/// 晋升/灰度/回滚全过程的可追溯记录（§779）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionRecord {
    pub bundle_id: String,
    /// 为何改
    pub reason: String,
    /// 谁批准
    pub approved_by: String,
    /// 测了什么（回放/评测引用）
    pub tested_with: Vec<String>,
    /// 改善多少（基线指标 → 候选指标）
    pub improvement_summary: String,
    pub outcome: PromotionOutcome,
    pub recorded_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromotionOutcome {
    ShadowPassed,
    CanaryRunning { percent: u8 },
    Promoted,
    RolledBack { cause: String },
    Rejected { cause: String },
}

/// Shadow 运行模式：候选与现役并行评估，不影响生产。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GraduationStage {
    /// 候选仅在旁路观测，不影响产出
    Shadow,
    /// 候选按百分比接管新任务
    Canary { percent: u8 },
    /// 候选成为新活动版本
    Active,
    /// 已回滚
    RolledBack,
}

pub struct Graduator {
    pub state_root: PathBuf,
    pub governor: Governor,
}

#[derive(Debug)]
pub enum GradError {
    Io(String),
    Bundle(BundleError),
    Gate(GateError),
    NoActiveManifest,
    NoPreviousStable,
}

impl std::fmt::Display for GradError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Bundle(e) => write!(f, "bundle: {e}"),
            Self::Gate(e) => write!(f, "gate: {e}"),
            Self::NoActiveManifest => write!(f, "no active manifest"),
            Self::NoPreviousStable => write!(f, "no previous stable to roll back to"),
        }
    }
}
impl std::error::Error for GradError {}
impl From<BundleError> for GradError {
    fn from(e: BundleError) -> Self {
        Self::Bundle(e)
    }
}
impl From<GateError> for GradError {
    fn from(e: GateError) -> Self {
        Self::Gate(e)
    }
}

impl Graduator {
    pub fn new(state_root: PathBuf, governor: Governor) -> Self {
        Self { state_root, governor }
    }

    fn active_manifest_path(&self) -> PathBuf {
        self.state_root.join("active-manifest.json")
    }

    fn records_dir(&self) -> PathBuf {
        self.state_root.join("promotions")
    }

    pub fn read_active(&self) -> Result<ActiveManifest, GradError> {
        let p = self.active_manifest_path();
        let raw = fs::read_to_string(&p).map_err(|e| GradError::Io(format!("{e}")))?;
        serde_json::from_str(&raw).map_err(|e| GradError::Io(format!("parse: {e}")))
    }

    /// Legacy activation is denied until an independent release backend is configured.
    pub fn activate(&self, bundle: &PolicyBundle, now_ms: u64) -> Result<ActiveManifest, GradError> {
        self.governor.authorize(&bundle.manifest.changes)?;
        let verified = PolicyBundle::load_verified(&bundle.dir)?;
        if verified.manifest != bundle.manifest {
            return Err(GradError::Gate(GateError::PromotionUnavailable));
        }
        // Never promote a caller-constructed bundle on assertions alone. Until
        // the independent evaluator/approval service is installed this API is
        // deliberately unavailable, including at E5.
        let _ = now_ms;
        Err(GradError::Gate(GateError::PromotionUnavailable))
    }

    /// 自动回滚：原子切回 previous stable（§545：只切 manifest，保留证据）。
    pub fn rollback(&self, cause: &str, now_ms: u64) -> Result<ActiveManifest, GradError> {
        let _lock = super::storage::lock(&self.state_root).map_err(|e| GradError::Io(e.to_string()))?;
        self.rollback_locked(cause, now_ms)
    }

    fn rollback_locked(&self, cause: &str, now_ms: u64) -> Result<ActiveManifest, GradError> {
        let cur = self.read_active().map_err(|_| GradError::NoActiveManifest)?;
        let prev = cur
            .previous_stable_id
            .clone()
            .ok_or(GradError::NoPreviousStable)?;
        let next = ActiveManifest {
            active_bundle_id: prev.clone(),
            previous_stable_id: None,
            activated_ms: now_ms,
        };
        super::storage::atomic_json(&self.active_manifest_path(), &next)
            .map_err(|e| GradError::Io(e.to_string()))?;
        self.append_record(PromotionRecord {
            bundle_id: cur.active_bundle_id,
            reason: format!("auto rollback: {cause}"),
            approved_by: "system:auto-rollback".to_string(),
            tested_with: vec![],
            improvement_summary: "n/a (rollback)".to_string(),
            outcome: PromotionOutcome::RolledBack {
                cause: cause.to_string(),
            },
            recorded_ms: now_ms,
        })?;
        Ok(next)
    }

    /// 候选失败时的入口：记录证据并触发自动回滚（§775）。
    pub fn record_canary_outcome(
        &self,
        bundle_id: &str,
        success: bool,
        cause: &str,
        now_ms: u64,
    ) -> Result<(), GradError> {
        if success {
            return Ok(());
        }
        let _lock = super::storage::lock(&self.state_root).map_err(|e| GradError::Io(e.to_string()))?;
        if self.read_active()?.active_bundle_id != bundle_id { return Ok(()); }
        self.rollback_locked(cause, now_ms)?;
        Ok(())
    }

    /// Immutable per-decision audit records; previous decisions are never overwritten.
    pub fn append_record(&self, rec: PromotionRecord) -> Result<(), GradError> {
        let dir = self.records_dir();
        fs::create_dir_all(&dir).map_err(|e| GradError::Io(format!("{e}")))?;
        super::storage::validate_id(&rec.bundle_id).map_err(|e| GradError::Io(e.to_string()))?;
        let p = dir.join(format!("{}-{}.json", rec.bundle_id, uuid::Uuid::new_v4()));
        let body = serde_json::to_string_pretty(&rec)
            .map_err(|e| GradError::Io(format!("serde: {e}")))?;
        fs::write(p, body).map_err(|e| GradError::Io(format!("{e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gov_evolve_e4() -> Governor {
        Governor::new(GovernorConfig {
            mode: "evolve".to_string(),
            max_level: EvolutionLevel::E4LowRiskPromote,
            shadow_enabled: true,
            canary_percent: 10,
            auto_promote_low_risk: true,
        })
    }

    #[test]
    fn gate_blocks_when_not_evolve_mode() {
        let g = Governor::new(GovernorConfig::default());
        let ch = vec![ChangeKind::Threshold {
            key: "repeat_stop".into(),
            old: "3".into(),
            new: "2".into(),
        }];
        assert!(matches!(
            g.authorize(&ch),
            Err(GateError::ModeNotEvolve { .. })
        ));
    }

    #[test]
    fn gate_blocks_level_exceeded() {
        let mut g = gov_evolve_e4();
        g.config.max_level = EvolutionLevel::E3Validate;
        let ch = vec![ChangeKind::Threshold {
            key: "k".into(),
            old: "1".into(),
            new: "2".into(),
        }];
        assert!(matches!(
            g.authorize(&ch),
            Err(GateError::LevelExceeded { .. })
        ));
    }

    #[test]
    fn gate_blocks_code_without_human_flag() {
        let mut g = gov_evolve_e4();
        g.config.auto_promote_low_risk = false;
        let ch = vec![ChangeKind::CodeOrBusinessRule {
            description: "fix".into(),
        }];
        assert!(matches!(
            g.authorize(&ch),
            Err(GateError::CodeRequiresHuman)
        ));
    }

    #[test]
    fn gate_allows_low_risk_threshold_at_e4() {
        let g = gov_evolve_e4();
        let ch = vec![ChangeKind::Threshold {
            key: "repeat_stop".into(),
            old: "3".into(),
            new: "2".into(),
        }];
        assert!(g.authorize(&ch).is_ok());
    }

    #[test]
    fn canary_bucket_is_deterministic() {
        let g = gov_evolve_e4();
        let a = g.in_canary_bucket("sess-A");
        let a2 = g.in_canary_bucket("sess-A");
        assert_eq!(a, a2);
    }
}
