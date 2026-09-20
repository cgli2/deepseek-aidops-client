//! 唯一的终止/交付裁决入口。
//!
//! 改造前，回合终态由 `agent_loop.rs` 末尾一路 `if / else if` 拼出：七个互不相关的
//! 布尔量（取消、provider 错误、求解图自认完成、空转、软预算、硬预算、改动面）按书写
//! 顺序交织决定 `DeliveryOutcome`，同时 `ExecutionState::can_complete`、`GoalExecution`
//! 的验收门禁和 `delivery_report` 各自再解释一遍「算不算完成」。任何一处放宽都会把
//! 搜索命中或绿色构建误记成交付成功。
//!
//! 本模块把「为什么停止」和「证据是否覆盖目标」拆成两个正交输入，由一个纯函数裁决，
//! 其余模块只能消费裁决结果。公开 `DeliveryOutcome` 通过 [`DeliveryDecision::outcome`]
//! 显式映射保留，避免 UI 与历史日志出现第二套语义。

use std::collections::{HashMap, HashSet};

use crate::execution::{
    Criterion, EvidenceRequirement, RequestedOutcome, is_behavior_verification_signature,
};
use harness_session::{DeliveryCriterion, DeliveryOutcome, DeliveryReport};

/// 循环停止的原因。它与证据覆盖度正交：任何单一停止原因都不能直接宣布完成，
/// 也不能把已有完整证据的任务降级成失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopCause {
    /// 模型停止调用工具且求解图已到终态。
    Settled,
    UserCancelled,
    /// Provider 流错误：错误文本不是模型回答，无论证据多完整都不能成为 Verified。
    ProviderError(String),
    /// 软窗口结束且无可续期额度；已有证据仍然有效。
    BudgetWindowExhausted,
    /// 任务硬步数/工具调用总额到达。
    HardBudgetCeiling,
    /// 模型连续只返回纯文本，任务状态与证据均未推进。
    NoProgressStall,
    /// 验证命令通过，但本轮没有产生任何相关改动。
    BaselineVerifiedWithoutChange,
    /// 重复调用保护、模型异常等非预算类硬熔断。它与预算停止必须分开：旧链路里前者
    /// 是 `Interrupted`（回合被打断），后者是 `SystemFailure`（额度用尽）。
    HardStop(String),
    /// 确实缺少只有用户能给出的产品口径或授权。
    NeedsUserDecision(String),
    /// 不属于以上任何已识别出口。
    Abnormal,
}

impl StopCause {
    pub fn is_budget_stop(&self) -> bool {
        matches!(self, Self::BudgetWindowExhausted | Self::HardBudgetCeiling)
    }

    fn summary(&self) -> String {
        match self {
            Self::Settled => "回合结束".into(),
            Self::UserCancelled => "用户取消".into(),
            Self::ProviderError(summary) => format!("provider 错误: {summary}"),
            Self::BudgetWindowExhausted => "软预算窗口耗尽".into(),
            Self::HardBudgetCeiling => "硬预算总额到达".into(),
            Self::NoProgressStall => "模型空转无进展".into(),
            Self::BaselineVerifiedWithoutChange => "基线验证通过但零改动".into(),
            Self::HardStop(why) => format!("硬熔断: {why}"),
            Self::NeedsUserDecision(question) => format!("需要用户决策: {question}"),
            Self::Abnormal => "未识别出口".into(),
        }
    }
}

/// 单个验收项的裁决结论。证据是否覆盖该项只由工具事实决定，模型文字不参与。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CriterionVerdict {
    /// 有与该验收要求匹配、且在最后一次写入之后取得的证据。
    Verified,
    /// 只读任务的调查证据已足够支撑结论。
    Investigated,
    /// 已写入目标但缺少匹配验收要求的验证证据。
    ChangedWithoutVerification,
    /// 既未写入也无证据。
    Unverified,
}

impl CriterionVerdict {
    pub fn covers_acceptance(self) -> bool {
        matches!(self, Self::Verified | Self::Investigated)
    }
}

/// 裁决输入：回合结束时对执行投影的一次不可变快照。
#[derive(Debug, Clone)]
pub struct DeliveryFacts {
    pub requested_outcome: RequestedOutcome,
    pub evidence_requirement: EvidenceRequirement,
    /// 该任务是否必须有成功验证才能交付。一旦写入发生过，任何语言分类都不能把它
    /// 重新解释成只读直答。
    pub requires_verification: bool,
    /// 变更类任务必须观察到工作区净变化；精确幂等变换允许无写入完成。
    pub requires_workspace_change: bool,
    pub criteria: Vec<Criterion>,
    pub changed_criteria: HashSet<String>,
    /// 验收项 id -> 已被 [`evidence_covers_requirement`] 接受的验证证据。
    pub verification: HashMap<String, Vec<String>>,
    /// 只读来源（读取、诊断、用户给定材料）摘要，供回答与诊断类任务验收。
    pub read_evidence: Vec<String>,
    pub write_operations: usize,
    /// 求解图是否自认到达终态。它只能提出候选结论，不能授予完成。
    pub solver_claims_complete: bool,
    /// 运行时是否已获得可定位目标的执行证据。有证据时「实现在哪」是 Agent 的技术
    /// 问题，不得伪装成只有用户能回答的产品决策。
    pub has_execution_evidence: bool,
    pub stop: StopCause,
}

/// 裁决输出：完成状态、逐项结论和用户可见结论的唯一来源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryStatus {
    Completed,
    /// 有合法下一动作或可恢复断点，但当前证据未覆盖全部验收项。
    Paused(String),
    NeedsUserDecision(String),
    /// 运行时或外部条件使任务无法继续推进，责任不在用户输入。
    Blocked(String),
    Cancelled,
}

/// 逐项结论与其证据的配对，避免报告生成时再从别处回查证据而形成第二套真相。
#[derive(Debug, Clone)]
pub struct CriterionOutcome {
    pub id: String,
    pub description: String,
    pub verdict: CriterionVerdict,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DeliveryDecision {
    pub status: DeliveryStatus,
    pub stop: StopCause,
    pub criteria: Vec<CriterionOutcome>,
    /// 需要实机操作或人工观察才能确认的验收说明。存在任一条时不得判 Completed。
    pub unmet_observations: Vec<String>,
    pub reason: Option<String>,
}

impl DeliveryDecision {
    pub fn is_completed(&self) -> bool {
        matches!(self.status, DeliveryStatus::Completed)
    }

    pub fn unverified_criteria(&self) -> Vec<&str> {
        self.criteria
            .iter()
            .filter(|outcome| !outcome.verdict.covers_acceptance())
            .map(|outcome| outcome.id.as_str())
            .collect()
    }

    pub fn verification_summaries(&self) -> Vec<String> {
        self.criteria
            .iter()
            .filter(|outcome| outcome.verdict.covers_acceptance())
            .flat_map(|outcome| {
                outcome
                    .evidence
                    .iter()
                    .map(|item| format!("{} => {}", outcome.id, item))
            })
            .collect()
    }

    /// 裁决自带的用户可见停止说明。`Completed` 没有停止说明；其余状态的原因文本
    /// 与 [`DeliveryDecision::into_report`] 落盘的 `reason` 同源，避免下游再从别处
    /// 回查原因而形成第二套真相。
    pub fn status_reason(&self) -> Option<String> {
        match &self.status {
            DeliveryStatus::Paused(text)
            | DeliveryStatus::Blocked(text)
            | DeliveryStatus::NeedsUserDecision(text) => Some(text.clone()),
            DeliveryStatus::Cancelled => Some("用户取消了回合；未获得完整验收证据".into()),
            DeliveryStatus::Completed => None,
        }
    }

    /// 显式映射到历史公开枚举，保持 UI 与日志可读；新代码一律消费
    /// [`DeliveryStatus`]，不得重新解释 `PartialDelivery` 的含义。
    pub fn outcome(&self) -> DeliveryOutcome {
        match &self.status {
            DeliveryStatus::Completed => DeliveryOutcome::Verified,
            DeliveryStatus::Cancelled => DeliveryOutcome::Cancelled,
            DeliveryStatus::NeedsUserDecision(_) => DeliveryOutcome::NeedsUserInput,
            DeliveryStatus::Blocked(_) => DeliveryOutcome::SystemFailure,
            // 预算停止在旧出口里是 SystemFailure（运行时停止而非部分交付）。P4 统一
            // 任务级总成本时再改名，本次不顺手改用户可见语义。
            DeliveryStatus::Paused(_) if self.stop.is_budget_stop() => {
                DeliveryOutcome::SystemFailure
            }
            DeliveryStatus::Paused(_) if matches!(self.stop, StopCause::HardStop(_)) => {
                DeliveryOutcome::Interrupted
            }
            DeliveryStatus::Paused(_) => DeliveryOutcome::PartialDelivery,
        }
    }

    pub fn into_report(self) -> DeliveryReport {
        let verification = self.verification_summaries();
        let criteria = self
            .criteria
            .iter()
            .map(|outcome| DeliveryCriterion {
                id: outcome.id.clone(),
                description: outcome.description.clone(),
                satisfied: outcome.verdict.covers_acceptance(),
                evidence: outcome.evidence.clone(),
            })
            .collect();
        let outcome = self.outcome();
        let status_reason = self.status_reason();
        DeliveryReport {
            outcome,
            criteria,
            verification,
            reason: self.reason.or(status_reason),
        }
    }
}

/// 验收要求与实际观察到的检查之间是否匹配。编译与普通单元测试都不能证明界面外观
/// 或运行行为，这是「零编辑假绿」和「改了但没验证」两类事故的硬边界。
///
/// `verification_only` 表示任务本身只要求执行一个既有检查（纯核验、Git 操作），
/// 此时命令退出码即是交付物；否则必须是一次真正观察行为结果的检查。
pub fn evidence_covers_requirement(
    requirement: EvidenceRequirement,
    verification_only: bool,
    tool_signature: &str,
) -> bool {
    match requirement {
        // Runtime 目前没有可重复的视觉观察器：截图、界面树和实机操作都不在工具结果里，
        // 因此视觉类验收一律不自证完成，只能报告为待人工确认。
        EvidenceRequirement::Visual => false,
        EvidenceRequirement::Behavior => {
            verification_only || is_behavior_verification_signature(tool_signature)
        }
        EvidenceRequirement::Static
        | EvidenceRequirement::Command
        | EvidenceRequirement::Explanation => true,
    }
}

/// 单一裁决入口。相同输入必须得到相同输出：不读取时钟、不访问工作区、不写日志。
pub fn evaluate_delivery(facts: &DeliveryFacts) -> DeliveryDecision {
    let criteria = facts
        .criteria
        .iter()
        .map(|criterion| {
            let verdict = verdict_for(facts, criterion);
            let mut evidence = facts
                .verification
                .get(&criterion.id)
                .cloned()
                .unwrap_or_default();
            // 只读任务（回答/诊断/核验/Git）由已读取来源或用户给定材料验收，没有写入后
            // 验证证据。把读取证据作为该验收项的证据锚点，保证 Verified 报告不会出现
            // satisfied=true 而 evidence 为空（§3.2 报告与裁决同源一致性）。
            if evidence.is_empty() && verdict == CriterionVerdict::Investigated {
                evidence = facts.read_evidence.clone();
            }
            CriterionOutcome {
                id: criterion.id.clone(),
                description: criterion.description.clone(),
                verdict,
                evidence,
            }
        })
        .collect::<Vec<_>>();
    let covered = !criteria.is_empty()
        && criteria
            .iter()
            .all(|outcome| outcome.verdict.covers_acceptance());
    let unmet_observations = unmet_observations(facts, &criteria);
    let status = decide_status(facts, &criteria, covered, &unmet_observations);

    DeliveryDecision {
        status,
        stop: facts.stop.clone(),
        criteria,
        unmet_observations,
        reason: None,
    }
}

fn verdict_for(facts: &DeliveryFacts, criterion: &Criterion) -> CriterionVerdict {
    if !facts.requires_verification {
        // 只读路径：回答与诊断由已读取来源或用户给定材料验收，不要求写入。
        let read_only_outcome = matches!(
            facts.requested_outcome,
            RequestedOutcome::Answer
                | RequestedOutcome::Diagnose
                | RequestedOutcome::Verify
                | RequestedOutcome::RepositoryOperation
        );
        if read_only_outcome && !facts.read_evidence.is_empty() {
            return CriterionVerdict::Investigated;
        }
        // `Undetermined` 没有可自证的验收口径：缺证据时不得默认「回答即完成」。
        if facts.requested_outcome == RequestedOutcome::Undetermined {
            return CriterionVerdict::Unverified;
        }
    }
    if let Some(items) = facts.verification.get(&criterion.id) {
        if items.iter().any(|item| !item.trim().is_empty()) {
            return CriterionVerdict::Verified;
        }
    }
    if facts.changed_criteria.contains(&criterion.id) && facts.write_operations > 0 {
        return CriterionVerdict::ChangedWithoutVerification;
    }
    CriterionVerdict::Unverified
}

/// 未验证项中需要实机或人工观察的部分。存在任一条时任务不能报告为已交付。
fn unmet_observations(facts: &DeliveryFacts, criteria: &[CriterionOutcome]) -> Vec<String> {
    let has_unverified_change = criteria
        .iter()
        .any(|outcome| outcome.verdict == CriterionVerdict::ChangedWithoutVerification);
    if !has_unverified_change {
        return Vec::new();
    }
    match facts.evidence_requirement {
        EvidenceRequirement::Visual => vec![
            "界面外观验收尚未由可重复的运行时观察确认；本轮只能声明已修改并通过代码检查".into(),
        ],
        EvidenceRequirement::Behavior => vec![
            "运行行为验收尚未由可重复的运行时观察确认；本轮只能声明已修改并通过代码检查".into(),
        ],
        EvidenceRequirement::Static
        | EvidenceRequirement::Command
        | EvidenceRequirement::Explanation => Vec::new(),
    }
}

fn decide_status(
    facts: &DeliveryFacts,
    criteria: &[CriterionOutcome],
    covered: bool,
    unmet: &[String],
) -> DeliveryStatus {
    // 停止原因先于证据：取消与 provider 错误的文本都不是模型回答，证据再完整也不是交付。
    match &facts.stop {
        StopCause::UserCancelled => return DeliveryStatus::Cancelled,
        StopCause::ProviderError(summary) => {
            return DeliveryStatus::Blocked(format!(
                "llm provider error（流读取已终止，未获有效模型回答）: {summary}"
            ));
        }
        StopCause::NeedsUserDecision(question) if !facts.has_execution_evidence => {
            return DeliveryStatus::NeedsUserDecision(question.clone());
        }
        _ => {}
    }

    if !unmet.is_empty() {
        // 自动验证器无法覆盖的验收项：诚实报告为暂停，绝不因编译通过而升级。
        return DeliveryStatus::Paused(unmet.join("；"));
    }

    let write_gate = !facts.requires_workspace_change
        || facts.write_operations > 0
        || !facts.changed_criteria.is_empty();
    if covered && write_gate {
        return DeliveryStatus::Completed;
    }

    if let StopCause::NeedsUserDecision(question) = &facts.stop {
        // 已有执行证据时，「找不到实现入口」不回抛给用户，而是报告技术阻塞点。
        return DeliveryStatus::Paused(format!(
            "已定位到实现但未覆盖全部验收项，不得以追问代替技术定位：{question}"
        ));
    }

    match &facts.stop {
        StopCause::BaselineVerifiedWithoutChange => DeliveryStatus::Paused(
            "baseline_verified_without_change: 验证命令通过，但本轮没有产生代码修改".into(),
        ),
        StopCause::NoProgressStall => DeliveryStatus::Paused(
            "stalled_without_action: 模型连续两次只返回文本，任务状态与证据均未推进".into(),
        ),
        StopCause::HardBudgetCeiling => DeliveryStatus::Paused(
            "已达到任务硬预算上限（步数/工具调用总额），等待从检查点续跑".into(),
        ),
        StopCause::BudgetWindowExhausted => {
            DeliveryStatus::Paused("已达到安全探索预算，但未形成可验证的目标路径".into())
        }
        StopCause::HardStop(why) => {
            DeliveryStatus::Paused(format!("回合因 {why} 被打断；未获得完整验收证据"))
        }
        // NeedsUserDecision 已在上方按「是否已定位到实现」提前收口，这里仅为穷尽性。
        StopCause::Abnormal
        | StopCause::Settled
        | StopCause::UserCancelled
        | StopCause::ProviderError(_)
        | StopCause::NeedsUserDecision(_) => partial_status(facts, criteria),
    }
}

/// 证据未覆盖全部验收项时的结论：已有部分交付就报告部分交付，否则说明缺少哪一步。
fn partial_status(facts: &DeliveryFacts, criteria: &[CriterionOutcome]) -> DeliveryStatus {
    let changed: Vec<&str> = criteria
        .iter()
        .filter(|outcome| outcome.verdict == CriterionVerdict::ChangedWithoutVerification)
        .map(|outcome| outcome.id.as_str())
        .collect();
    if !changed.is_empty() {
        return DeliveryStatus::Paused(format!(
            "已有修改，但验收项 {} 尚未获得匹配的验证证据",
            changed.join("、")
        ));
    }
    if criteria.is_empty() {
        return DeliveryStatus::Blocked(format!(
            "回合未形成可判定的验收项（{}）",
            facts.stop.summary()
        ));
    }
    if facts.solver_claims_complete {
        return DeliveryStatus::Paused(
            "求解图已到终态，但执行证据没有覆盖全部验收项；已拒绝 Verified".into(),
        );
    }
    DeliveryStatus::Paused(format!(
        "验收项 {} 未覆盖，任务仍未完成",
        criteria
            .iter()
            .filter(|outcome| !outcome.verdict.covers_acceptance())
            .map(|outcome| outcome.id.as_str())
            .collect::<Vec<_>>()
            .join("、")
    ))
}
