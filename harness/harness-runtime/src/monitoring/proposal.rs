//! Phase 3：自动复盘与改进提案（ImprovementProposal）（§6.4 / §10）。
//!
//! 规则：
//! - 自动复盘分析 Phase 2 的 Incident，提炼根因与改进类型；
//! - 生成强契约的 `ImprovementProposal`；
//! - 未经评测验证与灰度审批的提案，禁止直接进入活动运行时或知识库。

use serde::{Deserialize, Serialize};
use super::incident::Incident;
use super::replay::ReplayOutcome;

/// 改进提案类型
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalKind {
    /// 调整 HotGuard 阈值参数
    ThresholdAdjustment { metric: String, old_value: u32, suggested_value: u32 },
    /// 提示词防空转引导加固
    PromptMitigation { target_agent: String, reason: String },
    /// 工具重试与降级策略优化
    ToolPolicyPatch { tool_name: String, action: String },
}

/// 风险等级分类
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RiskTier {
    Low,
    Medium,
    High,
    Critical,
}

/// 提案评估状态
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalStatus {
    Draft,
    ValidatedByReplay,
    Rejected { reason: String },
    ApprovedForCanary,
}

/// 改进提案契约
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImprovementProposal {
    pub proposal_id: String,
    pub incident_ref: String,
    pub kind: ProposalKind,
    pub risk_tier: RiskTier,
    pub hypothesis: String,
    pub status: ProposalStatus,
}

impl ImprovementProposal {
    /// 从事故自动生成初始改进提案（草案）
    pub fn generate_from_incident(incident: &Incident) -> Self {
        let (kind, risk, hypothesis) = if incident.max_repeat >= 3 {
            (
                ProposalKind::ThresholdAdjustment {
                    metric: "stagnation_repeat_limit".to_string(),
                    old_value: incident.max_repeat,
                    suggested_value: (incident.max_repeat - 1).max(2),
                },
                RiskTier::Low,
                format!("Reduce stagnation repeat limit from {} to avoid long loops on reason: {}", incident.max_repeat, incident.reason),
            )
        } else {
            (
                ProposalKind::PromptMitigation {
                    target_agent: "general_coder".to_string(),
                    reason: incident.reason.clone(),
                },
                RiskTier::Medium,
                format!("Reinforce prompt to break early when encountering: {}", incident.reason),
            )
        };

        Self {
            proposal_id: format!("prop-{}-{}", incident.session_id, incident.terminated_seq),
            incident_ref: format!("inc-{}-{}", incident.session_id, incident.terminated_seq),
            kind,
            risk_tier: risk,
            hypothesis,
            status: ProposalStatus::Draft,
        }
    }

    /// 应用回放评测结果检验提案
    pub fn apply_replay_validation(&mut self, outcome: &ReplayOutcome) {
        if outcome.success {
            // A simulated run is not independent baseline/candidate validation.
            // Keep it in the candidate area; it cannot authorize promotion.
            self.status = ProposalStatus::Draft;
        } else {
            self.status = ProposalStatus::Rejected {
                reason: outcome.failure_reason.clone().unwrap_or_else(|| "Unknown failure in replay".to_string()),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proposal_generation_and_validation() {
        let incident = Incident {
            incident_id: "sess-prop#42".to_string(),
            session_id: "sess-prop".to_string(),
            terminated_seq: 42,
            anomaly_count: 3,
            max_repeat: 3,
            reason: "repeated same conclusion".to_string(),
            ts_ms: 1000,
            state: IncidentState::Mitigated,
        };

        let mut proposal = ImprovementProposal::generate_from_incident(&incident);
        assert_eq!(proposal.risk_tier, RiskTier::Low);
        assert_eq!(proposal.status, ProposalStatus::Draft);
        match &proposal.kind {
            ProposalKind::ThresholdAdjustment { suggested_value, .. } => {
                assert_eq!(*suggested_value, 2);
            }
            _ => panic!("Expected ThresholdAdjustment"),
        }

        let outcome = ReplayOutcome {
            spec_id: "replay-inc-42".to_string(),
            completed_steps: 5,
            success: true,
            failure_reason: None,
        };

        proposal.apply_replay_validation(&outcome);
        assert_eq!(proposal.status, ProposalStatus::Draft);
    }
}
