//! Safe patch validation and legacy self-repair data contracts.
//! No isolated compiler/evaluator or authenticated binary release backend is
//! configured. Evaluate/release/rollback therefore fail closed, without writes.
//! These APIs do not claim that a simulated defect removal constitutes a repair.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use super::replay::{FaultKind, ReplaySpec};

impl PatchSpec {
    /// 把补丁应用到隔离 worktree：先校验基线上下文（old_content 完全匹配），
    /// 再写入 new_content。上下文不匹配说明基线漂移，拒绝应用。
    pub fn apply_to(&self, worktree_dir: &Path) -> Result<(), SelfRepairError> {
        super::storage::validate_id(&self.repair_id).map_err(|e| SelfRepairError::Io(e.to_string()))?;
        // Validate every file before any write, so invalid paths or stale context
        // cannot cause a partially applied patch.
        for f in &self.files {
            let target = super::storage::safe_path(worktree_dir, &f.path)
                .map_err(|e| SelfRepairError::Io(e.to_string()))?;
            if fs::read_to_string(target).map_err(|e| SelfRepairError::Io(e.to_string()))? != f.old_content {
                return Err(SelfRepairError::ContextMismatch { path: f.path.clone() });
            }
        }
        for f in &self.files {
            let target = super::storage::safe_path(worktree_dir, &f.path)
                .map_err(|e| SelfRepairError::Io(e.to_string()))?;
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| SelfRepairError::Io(format!("mkdir worktree: {e}")))?;
            }
            let current = fs::read_to_string(&target)
                .map_err(|e| SelfRepairError::Io(format!("read {}: {e}", f.path)))?;
            if current != f.old_content {
                return Err(SelfRepairError::ContextMismatch { path: f.path.clone() });
            }
            fs::write(&target, &f.new_content)
                .map_err(|e| SelfRepairError::Io(format!("write {}: {e}", f.path)))?;
        }
        Ok(())
    }
}

/// 单文件补丁：以 old_content 为基线上下文，替换为 new_content。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchFile {
    pub path: String,
    pub old_content: String,
    pub new_content: String,
}

/// 修复候选：补丁 + 旧缺陷的故障模型（Phase 3 FaultKind）。
/// The defect must remain present in both baseline and candidate test inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairCandidate {
    pub patch: PatchSpec,
    pub defect: FaultKind,
}

#[derive(Debug)]
pub enum SelfRepairError {
    IndependentPipelineRequired,
    Io(String),
    /// 补丁基线上下文与 worktree 不匹配（基线漂移）
    ContextMismatch { path: String },
    /// 默认不允许无人值守代码晋级
    UnattendedReleaseDenied,
    /// 评审报告未全部通过，禁止发布
    ReportNotApproved,
    UnknownRelease(String),
}

impl std::fmt::Display for SelfRepairError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IndependentPipelineRequired => write!(f, "no isolated build/replay/approved binary release backend configured"),
            Self::Io(e) => write!(f, "io: {e}"),
            Self::ContextMismatch { path } => {
                write!(f, "patch context mismatch: {path}")
            }
            Self::UnattendedReleaseDenied => {
                write!(f, "unattended code promotion denied: human approval required")
            }
            Self::ReportNotApproved => write!(f, "review report not fully passed"),
            Self::UnknownRelease(id) => write!(f, "unknown release: {id}"),
        }
    }
}
impl std::error::Error for SelfRepairError {}

/// 旧版（修复前）/ 候选（修复后）的基线对照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineComparison {
    pub spec_id: String,
    /// 旧版必须失败（缺陷可复现）
    pub before_ok: bool,
    /// 候选必须成功（缺陷已消除）
    pub after_ok: bool,
}

/// 黄金集条目的 before/after 结果：修复不得造成任何退化。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoldenSetEntry {
    pub spec_id: String,
    pub before_ok: bool,
    pub after_ok: bool,
}

/// 故障注入演练：修复后系统对注入故障仍敏感（失败被正确捕获）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaultDrill {
    pub spec_id: String,
    pub failed_as_expected: bool,
}

/// Phase 5 评审报告：构建/回放/黄金集/故障注入的全量证据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewReport {
    pub repair_id: String,
    /// Reserved for a real isolated compiler result; file writes are not builds.
    pub build_passed: bool,
    pub baseline: BaselineComparison,
    pub golden_set: Vec<GoldenSetEntry>,
    pub fault_drill: Option<FaultDrill>,
    /// 全部门禁通过才可提交人工审批
    pub all_passed: bool,
    pub report_path: Option<String>,
}

/// 发布记录（人工批准 + 可追溯，复用 §779 字段语义）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseRecord {
    pub repair_id: String,
    pub approved_by: String,
    pub reason: String,
    /// 测了什么（回放/黄金集/故障注入的 spec 引用）
    pub tested_with: Vec<String>,
    /// 改善多少（基线对照摘要）
    pub improvement_summary: String,
    pub released_ms: u64,
    pub rolled_back: bool,
}

/// 自修复管线：评估候选 → 评审报告。
pub struct SelfRepairPipeline;

impl SelfRepairPipeline {
    /// Reject legacy simulation-only evaluation: no executable candidate is supplied.
    pub fn evaluate(
        state_root: &Path,
        candidate: &RepairCandidate,
        baseline_spec: &ReplaySpec,
        golden_specs: &[ReplaySpec],
        fault_spec: Option<&ReplaySpec>,
        now_ms: u64,
    ) -> Result<ReviewReport, SelfRepairError> {
        // This legacy API has no repository revision, executable candidate or
        // independent test runner. Reject it before any filesystem mutation.
        let _ = (state_root, candidate, baseline_spec, golden_specs, fault_spec, now_ms);
        Err(SelfRepairError::IndependentPipelineRequired)
    }

    /// Reject caller-authored reports and names as substitutes for verified release approval.
    pub fn release(
        state_root: &Path,
        report: &ReviewReport,
        approved_by: &str,
        reason: &str,
        tested_with: Vec<String>,
        improvement_summary: String,
        now_ms: u64,
    ) -> Result<ReleaseRecord, SelfRepairError> {
        if !report.all_passed {
            return Err(SelfRepairError::ReportNotApproved);
        }
        let approver = approved_by.trim();
        if approver.is_empty() {
            return Err(SelfRepairError::UnattendedReleaseDenied);
        }
        let _ = (state_root, reason, tested_with, improvement_summary, now_ms);
        // A nonempty caller-supplied name is not authenticated HITL approval.
        Err(SelfRepairError::IndependentPipelineRequired)
    }

    /// No binary backend is configured; never report a JSON flag as a successful rollback.
    pub fn rollback(state_root: &Path, repair_id: &str) -> Result<ReleaseRecord, SelfRepairError> {
        let _ = (state_root, repair_id);
        Err(SelfRepairError::IndependentPipelineRequired)
    }


}

/// 代码修复补丁：只描述隔离 worktree 内的变更，不直接指向用户工作树。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchSpec {
    pub repair_id: String,
    pub rationale: String,
    pub files: Vec<PatchFile>,
    pub created_ms: u64,
}
