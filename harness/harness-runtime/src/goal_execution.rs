//! V4 目标求解内核：唯一求解图、动作契约与证据驱动状态迁移。
//!
//! 模型只可在当前工作项的范围内提出动作；工作项状态和终态原因由运行时维护。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use crate::execution::{ActionProposal, TaskContract};
use crate::intent::{inspect_diff, Clarification, InspectVerdict, IntentKind, IntentProfile, ObservedBehavior};
use crate::target_extract::{
    extract_acronyms, extract_code_symbols, extract_form_field_order, extract_navigation,
    segment_candidates, FormFieldOrder,
};
use crate::workspace_grounder::WorkspaceGrounding;
use crate::workspace_index::WorkspaceIndex;
use harness_llm::ToolCall;

#[derive(Debug, Clone)]
pub struct GoalContract {
    pub objective: String,
    pub navigation: Vec<String>,
    /// 全部可扫描目标。
    ///
    /// 编译阶段只放 L0 结构信号（代码符号、全大写缩写）——它们自证可定位，
    /// 不需要工作区确认。语言片段走 `candidates`，经 L2 命中率裁决后才并入。
    pub entities: Vec<String>,
    /// L1 切分出的候选片段，**未经语义判断**。哪个是真锚点由工作区裁决。
    pub candidates: Vec<String>,
    /// 仅代码符号。中文名词与缩写太泛，0 命中不足以证明仓库没有目标，
    /// 因此"工作区不匹配"的严格判定只看这一类。
    pub code_entities: Vec<String>,
    /// 用户描述里的动作短语（"界面优化"）。它们不是可定位字面量，只用于理解意图。
    pub action_phrases: Vec<String>,
    /// 表单字段顺序变更。编译成功后 Agent 可免搜索直接定位。
    pub field_order: Option<FormFieldOrder>,
    pub expected_state: String,
    pub expected_values: Vec<ExpectedValue>,
    pub transformation: Option<ExactTransformation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedValue {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactTransformation {
    pub from_value: Option<String>,
    pub to_value: String,
}

impl GoalContract {
    pub fn compile(input: &str) -> Self {
        // 只有显式路径分隔符才表示导航层级。旧实现把每个普通换行都当导航词，
        // 用户给出的复现示例会因此被误拿去扫描源码并制造“工作区不匹配”。
        // 同时把"界面优化"这类动作短语剔除出导航词——它是要做的事，不是要找的界面。
        let (navigation, action_phrases) = extract_navigation(input);
        // L0 结构信号：自证可定位，无需工作区确认。
        let mut entities: Vec<String> = Vec::new();
        for candidate in extract_code_symbols(input)
            .into_iter()
            .chain(extract_acronyms(input))
        {
            if !entities.contains(&candidate) {
                entities.push(candidate);
            }
        }
        entities.truncate(8);
        // L1 候选：只切分不判断，交由 L2 用命中率裁决（见 resolve_against）。
        // 不在此处做"是不是名词"的语义判断——那是旧实现里词表越堆越大的根源。
        let candidates = segment_candidates(input);
        let code_entities = extract_code_symbols(input);
        let field_order = extract_form_field_order(input);
        let transformation = extract_exact_transformation(input);
        let expected_values = transformation
            .as_ref()
            .map(|value| {
                vec![ExpectedValue {
                    key: "目标值".into(),
                    value: value.to_value.clone(),
                }]
            })
            .unwrap_or_else(|| extract_expected_values(input));
        Self {
            objective: input.trim().to_string(),
            navigation,
            entities,
            candidates,
            code_entities,
            action_phrases,
            field_order,
            expected_state: "所有验收项在目标交付面可观察，并经验证确认".into(),
            expected_values,
            transformation,
        }
    }

    /// L2 裁决：用工作区命中率决定哪些候选是真锚点，并把通过裁决的锚点并入
    /// `entities`。
    ///
    /// 这是"工作区做裁判"的落点——机制层提出候选，工作区决定谁是锚点。
    /// 判断依据是**区分度**而非语义：工作区里罕见 = 有信息量；几乎处处命中
    /// = 停用词；完全不存在 = 不是本项目的目标。三者都不需要任何词表。
    pub fn resolve_against(&mut self, index: &WorkspaceIndex) {
        for anchor in index.select_anchors(&self.candidates, 8) {
            if !self.entities.contains(&anchor) {
                self.entities.push(anchor);
            }
        }
        self.entities.truncate(8);
        // 用户说"模型名称字段"，代码里可能是 `modelName`——由工作区决定，
        // 不由词表猜测。
        if let Some(order) = self.field_order.as_mut() {
            order.resolve_with(|candidates| index.best_variant(candidates));
        }
    }

    /// 是否存在任何可用于定位的信号。全空意味着 Agent 只能盲搜——这是空跑的
    /// 前置条件，应当在进入求解前就被识别出来，而不是等预算烧完才熔断。
    ///
    /// 注意：纯 `→ Y` 变换契约（如"把版本号修改为 0.2.2"，无 `from`）也视作可定位信号——
    /// 用户已给出明确的目标终态，agent 可据此搜索 `X` 并核对，无需在首轮反问"改哪个"。
    pub fn has_locatable_signal(&self) -> bool {
        !self.entities.is_empty()
            || !self.navigation.is_empty()
            || self
                .transformation
                .as_ref()
                .is_some_and(|value| {
                    let from = value.from_value.as_deref().unwrap_or("").trim();
                    let to = value.to_value.trim();
                    !from.is_empty() || !to.is_empty()
                })
    }

    /// **Phase 2**：对已完成定位的候选文件做一次轻量静态核对，产出 `ObservedBehavior`。
    ///
    /// 它只读磁盘、逐字检查期望终态（变更契约的 `to` 值或 `expected_values`）是否已出现在
    /// 目标产物中——这是运行期观察的最廉价形态，替代"关键词猜用户描述过什么异常"。
    /// 找不到文件/文件为空时保守返回（不制造伪观察），由调用方决定回落策略。
    pub fn static_observe(&self, target_files: &[String], root: &Path) -> ObservedBehavior {
        let mut observed = ObservedBehavior {
            anchors: self.entities.clone(),
            observed_value: None,
            found_expected: false,
            notes: Vec::new(),
        };
        let expected: Vec<String> = self
            .transformation
            .iter()
            .map(|value| value.to_value.trim().to_string())
            .chain(self.expected_values.iter().map(|value| value.value.trim().to_string()))
            .filter(|value| !value.is_empty())
            .collect();
        if expected.is_empty() || target_files.is_empty() {
            return observed;
        }
        for file in target_files {
            let full = root.join(file);
            if let Ok(text) = std::fs::read_to_string(&full) {
                if let Some(hit) = expected.iter().find(|value| text.contains(value.as_str())) {
                    observed.found_expected = true;
                    observed.observed_value = Some(hit.clone());
                    break;
                }
            }
        }
        observed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolvePhase {
    Locate,
    Inspect,
    Change,
    Verify,
    Conclude,
}

impl SolvePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Locate => "locate",
            Self::Inspect => "inspect",
            Self::Change => "change",
            Self::Verify => "verify",
            Self::Conclude => "conclude",
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PhaseAttempts {
    pub locate: u8,
    pub inspect: u8,
    pub change: u8,
    pub verify: u8,
}

impl PhaseAttempts {
    fn get(self, phase: SolvePhase) -> u8 {
        match phase {
            SolvePhase::Locate => self.locate,
            SolvePhase::Inspect => self.inspect,
            SolvePhase::Change => self.change,
            SolvePhase::Verify => self.verify,
            SolvePhase::Conclude => 0,
        }
    }

    fn increment(&mut self, phase: SolvePhase) {
        let slot = match phase {
            SolvePhase::Locate => &mut self.locate,
            SolvePhase::Inspect => &mut self.inspect,
            SolvePhase::Change => &mut self.change,
            SolvePhase::Verify => &mut self.verify,
            SolvePhase::Conclude => return,
        };
        *slot = slot.saturating_add(1);
    }

    fn reset(&mut self, phase: SolvePhase) {
        match phase {
            SolvePhase::Locate => self.locate = 0,
            SolvePhase::Inspect => self.inspect = 0,
            SolvePhase::Change => self.change = 0,
            SolvePhase::Verify => self.verify = 0,
            SolvePhase::Conclude => {}
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhaseBudget {
    pub locate: u8,
    pub inspect: u8,
    pub change: u8,
    pub verify: u8,
}

impl Default for PhaseBudget {
    fn default() -> Self {
        Self {
            locate: 2,
            inspect: 2,
            change: 2,
            verify: 2,
        }
    }
}

impl PhaseBudget {
    fn get(self, phase: SolvePhase) -> u8 {
        match phase {
            SolvePhase::Locate => self.locate,
            SolvePhase::Inspect => self.inspect,
            SolvePhase::Change => self.change,
            SolvePhase::Verify => self.verify,
            SolvePhase::Conclude => u8::MAX,
        }
    }

    /// 一个交付面跑完 locate → inspect → change → verify 所需的步数下界。
    /// 它是全局硬预算供给的计量单位，不是某个具体领域的经验值。
    pub fn total(self) -> usize {
        self.locate as usize + self.inspect as usize + self.change as usize + self.verify as usize
    }
}

/// S3：分面预算需求量。它把"有几个面、每个面要多少步"翻译成全局硬预算的下界。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceBudgetDemand {
    pub surfaces: usize,
    pub steps: usize,
    pub tool_calls: usize,
}

/// S4 交付面类别（ADR §6.3 / D4）。
///
/// 这里**不做语义分类**——"哪些说法算界面面"属于开放集合，枚举不完。类别只有两个
/// 来源：模型显式声明（S5 的 SolveSketch，经白名单校验）或保持未声明。判定完成
/// 与否靠**产物证据**，不靠这个标签：标签只能让判据更严，不能让它更松。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SurfaceKind {
    /// 尚未声明（S5 之前的默认值）：允许尝试静态复核，但能否通过完全取决于
    /// 磁盘上是否真的存在可复核的产物证据。
    #[default]
    Undeclared,
    Ui,
    Schema,
    Api,
    /// 行为面：验证命令可由模型声明，**执行不可省略**（D4 硬约束）。
    Behavior,
    /// 声明非法或显式未知：按 D4 回落，必须实际执行验证。
    Unknown,
}

impl SurfaceKind {
    /// D4 白名单。模型只能声明这些字面值。`undeclared` 也纳入白名单，使本地计划器
    /// 生成的草图能往返校验通过（未声明是合法默认态，不是非法值）。
    pub const DECLARABLE: [&'static str; 6] =
        ["ui", "schema", "api", "behavior", "unknown", "undeclared"];

    /// D4 白名单校验：**非法声明一律回落 `Unknown`**（→ 必须实际执行），
    /// 绝不因为"看不懂就当没声明"而放宽判据——那会让拼写错误变成免检通道。
    pub fn from_declared(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "ui" => Self::Ui,
            "schema" => Self::Schema,
            "api" => Self::Api,
            "behavior" => Self::Behavior,
            "undeclared" => Self::Undeclared,
            _ => Self::Unknown,
        }
    }

    /// 序列化为 D4 白名单字面值，供 `SolveSketch::to_json` 回灌校验。
    pub fn declared_str(self) -> &'static str {
        match self {
            Self::Undeclared => "undeclared",
            Self::Ui => "ui",
            Self::Schema => "schema",
            Self::Api => "api",
            Self::Behavior => "behavior",
            Self::Unknown => "unknown",
        }
    }

    /// 该类别是否**允许**走静态收敛。允许 ≠ 已收敛：仍需磁盘产物复核通过。
    pub fn allows_static_convergence(self) -> bool {
        matches!(self, Self::Undeclared | Self::Ui | Self::Schema | Self::Api)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Undeclared => "未声明",
            Self::Ui => "界面",
            Self::Schema => "数据结构",
            Self::Api => "接口",
            Self::Behavior => "运行时行为",
            Self::Unknown => "未知",
        }
    }
}

/// S5/G5：交付面风险等级，独立于任务级 `RiskLevel`，用于并发准入控制。
///
/// 与 `RiskLevel` 解耦：任务级风险是"这次改动整体有多危险"，面级风险是
/// "这个交付面在并行调度里该被如何对待"。前者决定策略，后者决定调度顺序与
/// 隔离——两者都从 `derive` 派生，不写死任何领域假设。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum SurfaceRisk {
    #[default]
    Low,
    Medium,
    High,
}

impl SurfaceRisk {
    /// 由任务级风险 + 面类别派生：
    /// - 任务 `High`（生产/部署/凭据…）一律 `High`；
    /// - 其余以任务风险为底，但 `schema`/`behavior` 面至少 `Medium`（结构性变更/需真实执行）。
    pub fn derive(contract_risk: crate::execution::RiskLevel, kind: SurfaceKind) -> Self {
        use crate::execution::RiskLevel as RL;
        let base = match contract_risk {
            RL::High => SurfaceRisk::High,
            RL::Medium => SurfaceRisk::Medium,
            RL::Low => SurfaceRisk::Low,
        };
        match base {
            SurfaceRisk::High => SurfaceRisk::High,
            SurfaceRisk::Medium => SurfaceRisk::Medium,
            SurfaceRisk::Low if matches!(kind, SurfaceKind::Schema | SurfaceKind::Behavior) => {
                SurfaceRisk::Medium
            }
            SurfaceRisk::Low => SurfaceRisk::Low,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "低风险",
            Self::Medium => "中风险",
            Self::High => "高风险",
        }
    }
}

/// S4 收敛判据裁定结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConvergenceOutcome {
    /// 存在可静态复核的断言，且已确认产物文件——可以尝试免 shell 收敛。
    StaticallyProvable {
        assertions: Vec<String>,
        targets: Vec<String>,
    },
    /// 必须实际执行验证命令。附带原因，便于在提示词里说明"为什么还要再跑一次"。
    NeedsExecution(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HypothesisState {
    Active,
    Rejected,
    Confirmed,
}

#[derive(Debug, Clone)]
pub struct Hypothesis {
    pub id: String,
    pub description: String,
    pub attempts: u8,
    pub state: HypothesisState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkItemState {
    Pending,
    Locating,
    Located,
    Inspecting,
    ReadyToChange,
    Satisfied,
    Changed,
    Verified,
    NeedsUserInput,
    Failed,
}

impl WorkItemState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "待定位",
            Self::Locating => "定位中",
            Self::Located => "已定位",
            Self::Inspecting => "检查中",
            Self::ReadyToChange => "可修改",
            Self::Satisfied => "已满足，待验证",
            Self::Changed => "已修改，待验证",
            Self::Verified => "已验证",
            Self::NeedsUserInput => "需要用户信息",
            Self::Failed => "失败",
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkItem {
    pub id: String,
    pub description: String,
    pub state: WorkItemState,
    pub locate_attempts: u8,
    /// 连续没有改变目标、假设、代码或验证状态的动作数。它在动作返回后立即
    /// 触发校正，不再等总预算接近耗尽才要求模型“收敛”。
    pub no_information_streak: u8,
    pub read_evidence: u8,
    pub evidence: Vec<String>,
    pub candidate_targets: Vec<String>,
    pub hypotheses: Vec<Hypothesis>,
    pub active_hypothesis: usize,
    pub phase_attempts: PhaseAttempts,
    /// S3：每个交付面持有**独立**的相位预算，互不挤占。多面任务里排在后面的面
    /// 不再因前面的面耗尽共享预算而饿死（V5 §2.2 的根因）。
    pub phase_budget: PhaseBudget,
    /// S4：收敛判据类别。默认 `Undeclared`，由模型在 SolveSketch 中声明后经
    /// 白名单校验写入（D4）。它只能让判据更严，不能让它更松。
    pub kind: SurfaceKind,
    /// S5/G2：依赖的其他交付面 id，构成 DAG。被依赖面 `Verified` 后本面才进入
    /// 就绪集合（`ready_surfaces`）。空表示无依赖，可立即并行。
    pub depends_on: Vec<String>,
    /// S5/G5：面级风险，用于并发准入控制（高风险面不得与其他面同时改动同一区域）。
    pub risk: SurfaceRisk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceKind {
    TargetFound,
    HypothesisRejected,
    DataFlowConfirmed,
    AlreadySatisfied,
    ChangeApplied,
    VerificationPassed,
    VerificationFailed,
    NoInformation,
}

#[derive(Debug, Clone)]
pub struct ActionContract {
    pub work_item_id: String,
    pub phase: SolvePhase,
    pub hypothesis_id: String,
    pub tool: String,
    pub target_path: Option<String>,
    pub purpose: String,
    pub expected_signal: String,
    pub on_hit: String,
    pub on_miss: String,
    pub max_cost: usize,
}

pub type ActionSpec = ActionContract;

#[derive(Debug, Clone)]
pub struct GoalExecution {
    pub goal: GoalContract,
    pub items: BTreeMap<String, WorkItem>,
    /// 首次高信号搜索命中的目录。它是共享定位证据；后续读取、编辑和搜索必须
    /// 在其中或其子目录内完成，不能回到仓库根开始另一轮猜测。
    pub anchor_dirs: Vec<String>,
    /// Grounder 或定向搜索给出的候选文件。候选存在时应直接读取，不能再要求
    /// 模型重新支付一次 search 成本。
    pub target_files: Vec<String>,
    /// 诊断/解释任务只允许定位和读取，读取证据不会把工作项推进到 Change。
    pub read_only: bool,
    pub no_information_count: usize,
    pub correction_count: usize,
    /// S5/G2+G5：本回合内已把动作归属到的交付面 id（按准入顺序）。用于让单个 step
    /// 内多个就绪面并发推进且归属不串面；每步开始时由 Agent Loop 清空。
    pub step_attributed: Vec<String>,
}

/// V4 对外名称；保留 GoalExecution 仅作为兼容别名使用方的稳定 API。
pub type SolveGraph = GoalExecution;

/// S5/G2+G5：单步并发推进的交付面上限。超过的面推迟到后续 tick（风险优先序），
/// 保证并发占用上界守恒、且不与已准入面发生同区域写冲突。
pub const MAX_PARALLEL_SURFACES: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalCompletion {
    Continue,
    Complete,
    Correct(String),
    Terminal(String),
}

impl GoalExecution {
    pub fn from_contract(contract: &TaskContract) -> Self {
        let goal = GoalContract::compile(&contract.objective);
        let items = contract
            .acceptance_criteria
            .iter()
            .map(|criterion| {
                (
                    criterion.id.clone(),
                    WorkItem {
                        id: criterion.id.clone(),
                        description: criterion.description.clone(),
                        state: WorkItemState::Pending,
                        locate_attempts: 0,
                        no_information_streak: 0,
                        read_evidence: 0,
                        evidence: Vec::new(),
                        candidate_targets: Vec::new(),
                        hypotheses: default_hypotheses(),
                        active_hypothesis: 0,
                        phase_attempts: PhaseAttempts::default(),
                        phase_budget: PhaseBudget::default(),
                        kind: SurfaceKind::default(),
                        depends_on: Vec::new(),
                        risk: SurfaceRisk::derive(contract.risk, SurfaceKind::default()),
                    },
                )
            })
            .collect();
        Self {
            goal,
            items,
            anchor_dirs: Vec::new(),
            target_files: Vec::new(),
            read_only: IntentProfile::compile(&contract.objective).kind
                == IntentKind::Investigation,
            no_information_count: 0,
            correction_count: 0,
            step_attributed: Vec::new(),
        }
    }

    /// S5/G1：按 LLM 生成的 `SolveSketch` 构建求解图。草图里每个面携带独立类别、
    /// 依赖、预算与风险；未出现在草图中的验收项保留 `from_contract` 的默认面。
    ///
    /// **不在此做校验**——调用方应先用 `SolveSketch::from_llm_json`（含 schema 校验 +
    /// 环检测）拿到合法草图，或失败回落 `from_contract`。本函数假定草图已通过校验。
    pub fn from_sketch(contract: &TaskContract, sketch: &crate::solve_sketch::SolveSketch) -> Self {
        let mut base = Self::from_contract(contract);
        let mut items = std::collections::BTreeMap::new();
        for surf in &sketch.surfaces {
            let mut item = base
                .items
                .get(&surf.id)
                .cloned()
                .unwrap_or_else(|| WorkItem {
                    id: surf.id.clone(),
                    description: surf.id.clone(),
                    state: WorkItemState::Pending,
                    locate_attempts: 0,
                    no_information_streak: 0,
                    read_evidence: 0,
                    evidence: Vec::new(),
                    candidate_targets: Vec::new(),
                    hypotheses: default_hypotheses(),
                    active_hypothesis: 0,
                    phase_attempts: PhaseAttempts::default(),
                    phase_budget: PhaseBudget::default(),
                    kind: SurfaceKind::default(),
                    depends_on: Vec::new(),
                    risk: SurfaceRisk::default(),
                });
            item.kind = surf.kind;
            item.depends_on = surf.depends_on.clone();
            item.phase_budget = surf.budget;
            item.risk = SurfaceRisk::derive(contract.risk, surf.kind);
            items.insert(surf.id.clone(), item);
        }
        base.items = items;
        base
    }

    /// S5/G1：带 LLM 草图的构建入口，内置 **schema 校验失败回落**（D1 硬约束：
    /// G1 失败不得影响可用性）。
    ///
    /// - `llm_json = None` → 直接用静态模板 `from_contract`（当前行为，零风险）；
    /// - `Some(json)` 且校验通过且无环 → 用特化计划 `from_sketch`；
    /// - `Some(json)` 但校验失败 / 含环 → 回落 `from_contract`，可用性不受影响。
    pub fn from_input_with_sketch(contract: &TaskContract, llm_json: Option<&str>) -> Self {
        if let Some(json) = llm_json {
            if let crate::solve_sketch::SketchValidation::Valid(sketch) =
                crate::solve_sketch::SolveSketch::from_llm_json(json)
            {
                let candidate = Self::from_sketch(contract, &sketch);
                // 含环是 schema 校验之外的最后一道护栏：绝不推进含环 DAG。
                if candidate.detect_cycle().is_none() {
                    return candidate;
                }
            }
            // 校验失败或含环 → 显式回落，保证可用性。
        }
        Self::from_contract(contract)
    }

    /// 将确定性工作区扫描结果提升为求解图状态，而不只是塞进提示词。
    pub fn apply_grounding(&mut self, grounding: &WorkspaceGrounding) {
        let candidates = if !grounding.literal_hits.is_empty() {
            &grounding.literal_hits
        } else if grounding.entity_hits.is_empty() {
            &grounding.navigation_hits
        } else {
            &grounding.entity_hits
        };
        for path in candidates {
            self.add_target_file(path);
        }
        if !self.target_files.is_empty() {
            for item in self.items.values_mut() {
                item.candidate_targets = self.target_files.clone();
                if matches!(item.state, WorkItemState::Pending | WorkItemState::Locating) {
                    item.state = WorkItemState::Located;
                    item.evidence.push(format!(
                        "工作区候选：{}",
                        self.target_files
                            .iter()
                            .take(4)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join("、")
                    ));
                }
            }
        }
    }

    /// **Phase 2 入口**：已落地的任务，用运行期静态观察（见 `GoalContract::static_observe`）
    /// 替代关键词猜异常。仅在确实定位到文件、且观察揭示出真实歧义（当前既不是 `from` 也
    /// 不是 `to`）时，返回单个带上下文的澄清问题；其余一律 `None`（直接求解，不追问）。
    ///
    /// 调用方（agent loop）应在定位完成后调用；返回 `Some` 时按"需要用户确认"路径交付，
    /// 不再盲搜。无落地信号 → `None`（交由 Phase 1 门禁问定位问题）。
    pub fn inspect_for_clarification(&self, root: &Path) -> Option<Clarification> {
        if !self.goal.has_locatable_signal() || self.target_files.is_empty() {
            return None;
        }
        let observed = self.goal.static_observe(&self.target_files, root);
        match inspect_diff(&self.goal, &observed) {
            InspectVerdict::InferableMismatch(clar) => Some(clar),
            InspectVerdict::Aligned | InspectVerdict::NoAnchor => None,
        }
    }

    /// S3：返回当前所有可推进的交付面（每个面持有独立预算，互不挤占）。
    /// "先不并行"阶段仍由调度器串行挑选活动面；但预算不再跨面共享，排在后面的面
    /// 不会被前面的面耗尽而饿死（V5 §2.2 的根因）。
    pub fn active_surfaces(&self) -> Vec<&WorkItem> {
        self.items
            .values()
            .filter(|item| {
                !matches!(
                    item.state,
                    WorkItemState::Verified
                        | WorkItemState::Failed
                        | WorkItemState::NeedsUserInput
                )
            })
            .collect()
    }

    /// S5/G2：依赖就绪的交付面——其 `depends_on` 全部已 `Verified`。
    ///
    /// 这是全并行调度器的入度-0 集合：未就绪（依赖未完成）的面不进入本轮并行，
    /// 必须等拓扑序解锁。它区别于 `active_surfaces`（仅按状态筛），后者用于预算供给，
    /// 仍应覆盖所有未完成面（含未就绪者）以保证总额充足。
    pub fn ready_surfaces(&self) -> Vec<&WorkItem> {
        self.items
            .values()
            .filter(|item| {
                !matches!(
                    item.state,
                    WorkItemState::Verified
                        | WorkItemState::Failed
                        | WorkItemState::NeedsUserInput
                ) && item.depends_on.iter().all(|dep| {
                    // 依赖项不存在视为已满足（容错，避免因为草图引用瑕疵而哑火）。
                    self.items
                        .get(dep)
                        .map(|d| matches!(d.state, WorkItemState::Verified))
                        .unwrap_or(true)
                })
            })
            .collect()
    }

    /// S5/G2：DAG 环检测。返回首个检测到的环（顶点 id 序列），无环返回 `None`。
    ///
    /// 调度器在按草图执行前**必须**调用：含环的 DAG 不允许推进，应当退回静态模板
    /// （G1 回落），否则会陷入无限等待依赖解锁。
    pub fn detect_cycle(&self) -> Option<Vec<String>> {
        let mut color: HashMap<String, u8> = HashMap::new(); // 0 未访问 / 1 在栈 / 2 完成
        let mut stack: Vec<String> = Vec::new();
        for start in self.items.keys() {
            if let Some(cycle) = self.dfs_cycle(start, &mut color, &mut stack) {
                return Some(cycle);
            }
        }
        None
    }

    fn dfs_cycle(
        &self,
        id: &str,
        color: &mut HashMap<String, u8>,
        stack: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        match color.get(id).copied().unwrap_or(0) {
            2 => return None,
            1 => {
                // 回边：环 = stack 中从 id 到末尾。
                if let Some(pos) = stack.iter().position(|s| s == id) {
                    return Some(stack[pos..].to_vec());
                }
                return Some(vec![id.to_string()]);
            }
            _ => {}
        }
        color.insert(id.to_string(), 1);
        stack.push(id.to_string());
        if let Some(item) = self.items.get(id) {
            for dep in &item.depends_on {
                if self.items.contains_key(dep) && self.dfs_cycle(dep, color, stack).is_some() {
                    return Some(stack.clone());
                }
            }
        }
        color.insert(id.to_string(), 2);
        stack.pop();
        None
    }

    /// S5/G2：按候选文件交集把就绪面分成"写冲突组"。同组内的面改动同一文件/区域，
    /// 必须串行化（同 tick 只准入一个），避免并行写入互相覆盖。
    pub fn parallel_write_groups(&self) -> Vec<Vec<String>> {
        let ready: Vec<&WorkItem> = self.ready_surfaces();
        let mut groups: Vec<Vec<String>> = Vec::new();
        for item in ready {
            let regions: HashSet<&String> = item.candidate_targets.iter().collect();
            if regions.is_empty() {
                // 无候选文件：各自独立，单独成组（不与其他面共享区域假设）。
                groups.push(vec![item.id.clone()]);
                continue;
            }
            let mut placed = false;
            for g in groups.iter_mut() {
                let shares = g.iter().any(|gid| {
                    self.items
                        .get(gid)
                        .map(|gitem| gitem.candidate_targets.iter().any(|p| regions.contains(p)))
                        .unwrap_or(false)
                });
                if shares {
                    g.push(item.id.clone());
                    placed = true;
                    break;
                }
            }
            if !placed {
                groups.push(vec![item.id.clone()]);
            }
        }
        groups
    }

    /// S5/G2：全并行下的预算需求（供调度器抬升硬熔断）。
    ///
    /// 与 `required_budget` 的唯一区别：**不再对 locate 做半额摊销**——串行时多面
    /// 共享一次定位，并行时各面独立定位，必须按满额计入，否则全并行瞬间打满硬预算
    /// （V5 §8 风险）。总量守恒：返回的是"并发占用上界"，不乘以并行度——即 N 面并发
    /// 占用 == Σ 每面预算，而非 Σ × 并行度。
    pub fn required_budget_parallel(&self) -> SurfaceBudgetDemand {
        let surfaces = self.active_surfaces();
        let steps: usize = surfaces
            .iter()
            .map(|item| item.phase_budget.total())
            .sum();
        SurfaceBudgetDemand {
            surfaces: surfaces.len(),
            steps,
            tool_calls: steps.saturating_add(steps / 3),
        }
    }

    /// S5/G5：并发准入。先按风险（低优先快速产出）→ 进度排序就绪面，贪心准入至
    /// `max_parallel`。高风险面不得与共享同一 `candidate_targets` 区域的其他面同 tick
    /// 改动（同区域串行/单独准入），其余面仅在区域互不冲突时同 tick 并发。
    pub fn admit_concurrent(&self, max_parallel: usize) -> Vec<String> {
        let max_parallel = max_parallel.max(1);
        let mut ready: Vec<&WorkItem> = self.ready_surfaces();
        ready.sort_by_key(|item| self.admission_rank(item));
        let mut admitted: Vec<String> = Vec::new();
        let mut admitted_regions: HashSet<String> = HashSet::new();
        for item in &ready {
            if admitted.len() >= max_parallel {
                break;
            }
            let regions: HashSet<&String> = item.candidate_targets.iter().collect();
            let conflicts_admitted = regions.iter().any(|r| admitted_regions.contains(*r));
            let shares_with_remaining = ready.iter().any(|other| {
                other.id != item.id && other.candidate_targets.iter().any(|p| regions.contains(p))
            });
            match item.risk {
                // 高风险：仅当与任何就绪面（已准入或待准入）都**不共享区域**时才并发；
                // 一旦存在同区域面（无论当前是否已准入），高风险面单独推迟，避免与
                // 同一区域的改动并发（V5 §6.5「不与其他面同时改动同一区域」）。
                SurfaceRisk::High if conflicts_admitted || shares_with_remaining => continue,
                SurfaceRisk::High => {
                    admitted.push(item.id.clone());
                    for r in regions {
                        admitted_regions.insert(r.clone());
                    }
                }
                // 低/中风险：仅与已准入面存在同区域冲突时串行（推迟到后续 tick）。
                _ if conflicts_admitted => continue,
                _ => {
                    admitted.push(item.id.clone());
                    for r in regions {
                        admitted_regions.insert(r.clone());
                    }
                }
            }
        }
        admitted
    }

    /// G5 准入排序键：(风险升序, 进度升序, id 升序)。风险低先推进快速产出，
    /// 高风险后置；同级按"能直接产出 > 仍在定位"的进度序，最后以 id 稳定排序。
    fn admission_rank(&self, item: &WorkItem) -> (u8, u8, String) {
        let risk_rank = match item.risk {
            SurfaceRisk::Low => 0,
            SurfaceRisk::Medium => 1,
            SurfaceRisk::High => 2,
        };
        let state_rank = match item.state {
            WorkItemState::ReadyToChange => 0,
            WorkItemState::Inspecting => 1,
            WorkItemState::Located => 2,
            WorkItemState::Locating => 3,
            WorkItemState::Pending => 4,
            WorkItemState::Satisfied | WorkItemState::Changed => 5,
            _ => 6,
        };
        (risk_rank, state_rank, item.id.clone())
    }

    /// S5/G2+G5：活动工作项 = 依赖就绪 + 风险优先后的首个面。替换原仅按状态排序的
    /// `active_item`，使调度既尊重 DAG 依赖，又落实风险分层（低风险先跑、高风险单独确认）。
    pub fn active_item(&self) -> Option<&WorkItem> {
        self.ready_surfaces()
            .iter()
            .min_by_key(|item| self.admission_rank(item))
            .map(|item| &self.items[&item.id])
    }

    /// S3：把"分面独立预算"换算成全局硬预算的下界。
    ///
    /// 旧模型里全局硬熔断是 `20 + min(面数-1, 4) * 2`，面数超过 5 之后总额不再增长；
    /// 而每个面独立跑完四个相位需要 `PhaseBudget::total()` 步。于是面一多，尾部的面
    /// 在**算术上**就不可能跑完——不是模型不会做（V5 §2.2）。这里按未完成面的实际
    /// 预算求和，供调度器抬升硬预算，让"面数 → 总额"回到线性关系。
    pub fn required_budget(&self) -> SurfaceBudgetDemand {
        let surfaces = self.active_surfaces();
        let steps: usize = surfaces
            .iter()
            .map(|item| item.phase_budget.total())
            .sum::<usize>()
            .saturating_sub(
                // 定位是可共享的：多个面通常由同一个高信号符号一次定位。只为第一个面
                // 计入完整 locate 预算，其余面按半额摊销，避免把共享工作重复计费。
                surfaces
                    .iter()
                    .skip(1)
                    .map(|item| item.phase_budget.locate as usize / 2)
                    .sum::<usize>(),
            );
        SurfaceBudgetDemand {
            surfaces: surfaces.len(),
            steps,
            // 一个相位步可能包含一次工具调用外加一次结果确认，按 4/3 供给。
            tool_calls: steps.saturating_add(steps / 3),
        }
    }

    /// S4：可用于静态复核的断言集 —— "改完之后，**什么东西必须出现在产物里**"。
    ///
    /// 它不是从词表推出来的：`expected_values` 来自用户明确写出的目标值，
    /// `code_entities` 与 `candidates` 是 L2 工作区裁决确认过"这个项目里确实存在"
    /// 的锚点。两者都可被机器逐字复核，因此不需要任何领域知识。
    pub fn static_assertions(&self) -> Vec<String> {
        fn push(assertions: &mut Vec<String>, value: &str) {
            let value = value.trim();
            // 单字符断言在任何文件里几乎必然命中，等于免检通道，直接排除。
            if value.chars().count() >= 2 && !assertions.iter().any(|kept| kept == value) {
                assertions.push(value.to_string());
            }
        }
        let mut assertions: Vec<String> = Vec::new();
        // 用户写明的目标值最强：它是"改成什么"的逐字定义。
        for expected in &self.goal.expected_values {
            push(&mut assertions, &expected.value);
        }
        if assertions.is_empty() {
            // 没有明确目标值时，退到"用户提到的、且工作区确认存在的符号必须出现在
            // 产物里"。这对"某几个界面要展示 appCode"这类交付面正是充分判据。
            for entity in &self.goal.code_entities {
                push(&mut assertions, entity);
            }
        }
        assertions
    }

    /// S4 收敛判据分层（ADR §6.3）：判断这个交付面能否在**不执行 shell** 的前提下收敛。
    ///
    /// 判据不问"这个面属于哪个领域类别"（那需要语义分类，属开放集合，枚举不完），
    /// 只问"它的完成有没有留下**可逐字复核的产物证据**"：
    ///   - 有 → 允许静态收敛（仍须真实复核通过，见 `settle_static_convergence`）；
    ///   - 没有 → 自动落到必须执行，无需任何分类判断。
    ///
    /// 模型声明只能**收紧**：声明 `behavior`/非法值一律强制执行；声明 `ui` 却拿不出
    /// 产物证据，依然要执行。这就是 D4 "可以声明怎么验，不得声明已经验过了"的边界。
    pub fn convergence_for(&self, item: &WorkItem) -> ConvergenceOutcome {
        if !item.kind.allows_static_convergence() {
            return ConvergenceOutcome::NeedsExecution(format!(
                "{}类交付面必须实际执行验证命令，声明不能替代执行",
                item.kind.as_str()
            ));
        }
        let assertions = self.static_assertions();
        if assertions.is_empty() {
            return ConvergenceOutcome::NeedsExecution(
                "该交付面没有可逐字复核的产物断言，只能靠实际执行证明".into(),
            );
        }
        let targets: Vec<String> = if item.candidate_targets.is_empty() {
            self.target_files.clone()
        } else {
            item.candidate_targets.clone()
        };
        if targets.is_empty() {
            return ConvergenceOutcome::NeedsExecution(
                "该交付面尚无确认的产物文件，无处复核".into(),
            );
        }
        ConvergenceOutcome::StaticallyProvable {
            assertions,
            targets,
        }
    }

    /// S4：对已改动、且判据允许静态收敛的交付面，**重读磁盘产物**逐字复核。
    /// 全部断言都在某个产物文件里命中 → 置 `Verified`（免 shell）。
    ///
    /// 关键：证据取自磁盘上的文件，不取自模型自述，也不取自回合开始时的陈旧索引。
    /// 复核不通过就什么都不改，该面继续走原本的执行验证路径——失败是安全方向。
    ///
    /// 返回本次静态收敛成功的交付面 id 与证明摘要。
    pub fn settle_static_convergence(&mut self, root: &Path) -> Vec<(String, String)> {
        let candidates: Vec<String> = self
            .items
            .iter()
            .filter(|(_, item)| {
                matches!(
                    item.state,
                    WorkItemState::Changed | WorkItemState::Satisfied
                )
            })
            .map(|(id, _)| id.clone())
            .collect();
        let mut settled = Vec::new();
        for id in candidates {
            let Some(item) = self.items.get(&id) else {
                continue;
            };
            let ConvergenceOutcome::StaticallyProvable {
                assertions,
                targets,
            } = self.convergence_for(item)
            else {
                continue;
            };
            // 每个断言都必须在**同一批产物文件**中被找到；记录命中位置作为证明。
            let mut proofs = Vec::new();
            for assertion in &assertions {
                let hit = targets.iter().find(|relative| {
                    crate::workspace_index::recheck_on_disk(root, relative, assertion)
                        .unwrap_or(false)
                });
                match hit {
                    Some(relative) => proofs.push(format!("{assertion} @ {relative}")),
                    None => {
                        proofs.clear();
                        break;
                    }
                }
            }
            if proofs.is_empty() {
                continue;
            }
            let proof = format!("静态收敛：{}", proofs.join("；"));
            if let Some(item) = self.items.get_mut(&id) {
                item.state = WorkItemState::Verified;
                item.evidence.push(proof.clone());
            }
            settled.push((id, proof));
        }
        settled
    }

    /// S4 / D4：把模型声明的判据类别写入交付面，经白名单校验。
    /// 返回是否为**合法**声明；非法声明会被记为 `Unknown`（强制执行验证）。
    pub fn declare_surface_kind(&mut self, id: &str, raw: &str) -> bool {
        let kind = SurfaceKind::from_declared(raw);
        let legal = SurfaceKind::DECLARABLE
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(raw.trim()));
        if let Some(item) = self.items.get_mut(id) {
            item.kind = kind;
            if !legal {
                item.evidence.push(format!(
                    "判据声明非法（{}），已回落为必须实际执行验证",
                    raw.trim().chars().take(40).collect::<String>()
                ));
            }
        }
        legal
    }

    /// S5/G4：从当前求解图构建概念注册表。
    ///
    /// 概念由 `goal` 的 L0 标识符信号（候选符号 + 代码实体）自动建立；位置由每个交付面
    /// 的 `candidate_targets`（L2 裁决后的候选文件）填充。机制层只认"这个符号在哪些面、
    /// 哪些文件里出现过"，不需要任何语义知识。
    pub fn build_concept_registry(&self) -> crate::concept_registry::ConceptRegistry {
        use crate::concept_registry::ConceptRegistry;
        let mut registry = ConceptRegistry::seed_from_goal(&self.goal);
        // 去重：一个符号同时出现在 `candidates` 与 `code_entities` 时不应被登记两次，
        // 否则 `missing_coverage_report` 会返回重复的交付面 id。
        let mut symbols: Vec<String> = self
            .goal
            .candidates
            .iter()
            .chain(self.goal.code_entities.iter())
            .cloned()
            .collect();
        symbols.sort();
        symbols.dedup();
        for item in self.items.values() {
            for symbol in &symbols {
                for target in &item.candidate_targets {
                    registry.register(&item.id, symbol, target);
                }
            }
        }
        registry
    }

    /// S5/G4 漏改报告：返回 `(symbol, 尚未完成改动的交付面 id)`。仅当某符号覆盖 >1 面、
    /// 且并非所有面都已完成（Verified/Changed）时才报告——同一概念在不同位置应被同步改动。
    pub fn missing_concept_coverage(&self) -> Vec<(String, Vec<String>)> {
        let registry = self.build_concept_registry();
        registry.missing_coverage_report(|id| {
            self.items
                .get(id)
                .map(|item| {
                    matches!(item.state, WorkItemState::Verified | WorkItemState::Changed)
                })
                .unwrap_or(false)
        })
    }

    /// S5/G2+G5：生成本步的并行执行计划文本，注入模型提示，让多面任务真正并发推进
    /// 且冲突安全（同文件写串行、高风险面单独确认）。
    pub fn parallel_plan(&self) -> String {
        if self.items.len() <= 1 {
            return String::new(); // 单面任务无需并行计划。
        }
        let ready: Vec<&WorkItem> = self.ready_surfaces();
        if ready.is_empty() {
            return String::new();
        }
        let admitted = self.admit_concurrent(MAX_PARALLEL_SURFACES);
        let groups = self.parallel_write_groups();
        let mut lines: Vec<String> = Vec::new();
        lines.push("[并行执行计划]".into());
        lines.push(format!(
            "本步可并发推进 {} 个交付面（上限 {}）：{}",
            admitted.len(),
            MAX_PARALLEL_SURFACES,
            admitted.join("、")
        ));
        if admitted.len() < ready.len() {
            let waiting: Vec<&str> = ready
                .iter()
                .filter(|r| !admitted.contains(&r.id))
                .map(|r| r.id.as_str())
                .collect();
            lines.push(format!(
                "其余 {} 个面待后续 tick 解锁：{}",
                waiting.len(),
                waiting.join("、")
            ));
        }
        // 写冲突组：同组改同一文件，必须串行（同 tick 只做一个）。
        let conflicting: Vec<&Vec<String>> = groups.iter().filter(|g| g.len() > 1).collect();
        if !conflicting.is_empty() {
            let desc = conflicting
                .iter()
                .map(|g| g.join("+"))
                .collect::<Vec<_>>()
                .join("；");
            lines.push(format!("写冲突（同文件，必须串行）：{desc}"));
        }
        // 风险分层：低风险先跑、高风险单独确认。
        let risks: Vec<String> = ready
            .iter()
            .map(|r| format!("{}({})", r.id, r.risk.as_str()))
            .collect();
        lines.push(format!("风险分层（低→高优先）：{}", risks.join("、")));
        lines.join("\n")
    }

    /// S5/G4：跨面概念漏改清单。返回"在多个交付面出现、但部分面尚未改动"的概念与漏改面。
    /// 注入模型提示与每步提醒，直接关闭"改了 A 漏了 B"的跨面一致性问题。
    /// S5/G4：跨面概念漏改清单。返回"在多个交付面出现、但部分面尚未改动"的概念与漏改面。
    /// 注入模型提示与每步提醒，直接关闭"改了 A 漏了 B"的跨面一致性问题。
    ///
    /// 上限保护：跨面概念可能很多（尤其中文字面值会被工作区裁决切成 n-gram），全部列出会
    /// 淹没提示。只保留覆盖交付面最多的前 `MAX_LEAK_CONCEPTS` 个概念，且每概念最多列
    /// `MAX_LEAK_SURFACES` 个漏改面，确保清单始终精炼可用。
    pub fn concept_coverage_checklist(&self) -> String {
        const MAX_LEAK_CONCEPTS: usize = 8;
        const MAX_LEAK_SURFACES: usize = 6;
        let mut leaks = self.missing_concept_coverage();
        if leaks.is_empty() {
            return "[跨面一致性] 当前无概念漏改风险。".into();
        }
        // 按漏改面数量降序，优先暴露影响面最广的概念；同数时按符号升序，保证字节确定。
        leaks.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
        let mut lines: Vec<String> = Vec::new();
        lines.push(
            "[跨面一致性·漏改预警] 以下概念在多个交付面出现，但部分面尚未改动，必须同步：".into(),
        );
        for (symbol, surfaces) in leaks.iter().take(MAX_LEAK_CONCEPTS) {
            let shown: Vec<&str> = surfaces.iter().map(String::as_str).take(MAX_LEAK_SURFACES).collect();
            let more = if surfaces.len() > shown.len() {
                format!(" 等 {} 个面", surfaces.len())
            } else {
                String::new()
            };
            lines.push(format!(
                "- `{symbol}` 还差这些交付面未改：{}{}（请一并修改，避免跨面不一致）",
                shown.join("、"),
                more
            ));
        }
        if leaks.len() > MAX_LEAK_CONCEPTS {
            lines.push(format!("（另有 {} 个概念略）", leaks.len() - MAX_LEAK_CONCEPTS));
        }
        lines.join("\n")
    }

    /// S5/G2+G5：返回本步下一个应归属动作的交付面 id——已准入、就绪、且尚未被本步
    /// 其它动作占用的第一个面（按风险/进度优先序）。无可用面返回 `None`（退回串行）。
    pub fn next_admitted_surface(&mut self) -> Option<String> {
        if self.items.len() <= 1 {
            return None; // 单面任务无需并发归属，保持旧串行行为。
        }
        let admitted = self.admit_concurrent(MAX_PARALLEL_SURFACES);
        for id in &admitted {
            if !self.step_attributed.contains(id) {
                self.step_attributed.push(id.clone());
                return Some(id.clone());
            }
        }
        None
    }

    /// S5/G2+G5：某面是否在本步并发准入集合内（与已准入面无写冲突、风险可并发）。
    fn is_admitted(&self, id: &str) -> bool {
        self.admit_concurrent(MAX_PARALLEL_SURFACES)
            .iter()
            .any(|a| a == id)
    }

    /// S5/G2+G5：把动作归属到交付面。
    /// - 共享验证动作支持所有已 Changed/Satisfied 的面（不变）。
    /// - 非验证动作优先归属到**本步已准入且尚未被本步其它动作占用的就绪面**，
    ///   实现多面并发推进；若无可用准入面则退回单一活动面（兼容旧串行行为）。
    pub fn link_proposal(&mut self, proposal: &mut ActionProposal) {
        let is_verify = is_verification(&proposal.signature);
        proposal.supports = if is_verify {
            self.items
                .values()
                .filter(|item| {
                    matches!(
                        item.state,
                        WorkItemState::Changed | WorkItemState::Satisfied
                    )
                })
                .map(|item| item.id.clone())
                .collect()
        } else {
            self.next_admitted_surface()
                .map(|id| vec![id])
                .or_else(|| self.active_item().map(|item| vec![item.id.clone()]))
                .unwrap_or_default()
        };
    }

    pub fn action_spec(&self, call: &ToolCall, proposal: &ActionProposal) -> Option<ActionSpec> {
        let item_id = proposal.supports.first()?;
        let item = self.items.get(item_id)?;
        let is_verify = is_verification(&proposal.signature);
        let active = self.active_item()?;
        // 共享验证可覆盖多个工作项；非验证动作必须服务活动面**或本步已准入的并发面**
        // （S5/G2+G5：多面并发推进，归属由 link_proposal 保证不串面）。
        if !is_verify && item.id != active.id && !self.is_admitted(&item.id) {
            return None;
        }
        if is_verify
            && self.items.values().any(|work_item| {
                matches!(
                    work_item.state,
                    WorkItemState::Pending
                        | WorkItemState::Locating
                        | WorkItemState::Inspecting
                        | WorkItemState::ReadyToChange
                )
            })
        {
            return None;
        }
        let (purpose, expected_signal) = if proposal.signature.starts_with("search:") {
            (
                "定位当前交付面的实现入口",
                "命中字段、路由、组件或其直接引用",
            )
        } else if proposal.signature.starts_with("fs:") {
            ("检查最短数据流与当前实现", "确认当前文件是否能完成该交付面")
        } else if proposal.signature.starts_with("edit:") {
            (
                "完成当前交付面的最小修改",
                "产生与该验收项关联的确定性 diff",
            )
        } else if proposal.signature.starts_with("shell:") {
            ("验证已完成的交付面", "相关构建或测试成功")
        } else {
            ("推进当前交付面", "产生可复用的新证据")
        };
        let phase = self.phase();
        let hypothesis_id = item
            .hypotheses
            .get(item.active_hypothesis)
            .map(|hypothesis| hypothesis.id.clone())
            .unwrap_or_else(|| "none".into());
        let target_path = match call.name.as_str() {
            "search" => call.args.get("dir"),
            "fs" | "edit" => call.args.get("path"),
            _ => None,
        }
        .and_then(|value| value.as_str())
        .map(str::to_string);
        Some(ActionSpec {
            work_item_id: item.id.clone(),
            phase,
            hypothesis_id,
            tool: call.name.clone(),
            target_path,
            purpose: purpose.into(),
            expected_signal: expected_signal.into(),
            on_hit: match phase {
                SolvePhase::Locate => "锁定候选文件并进入 inspect",
                SolvePhase::Inspect => "确认最短数据流并进入 change/verify",
                SolvePhase::Change => "记录 diff 并进入 verify",
                SolvePhase::Verify => "所有关联工作项进入 verified",
                SolvePhase::Conclude => "生成证据化结论",
            }
            .into(),
            on_miss: match phase {
                SolvePhase::Locate | SolvePhase::Inspect => "拒绝当前假设，切换有限候选",
                SolvePhase::Change => "保持目标不变，修正最小修改",
                SolvePhase::Verify => "把失败映射回相关工作项",
                SolvePhase::Conclude => "报告明确阻塞",
            }
            .into(),
            max_cost: 1,
        })
    }

    /// 受控任务的工具阶段只由活动工作项决定。这是 V4 的唯一阶段源；旧
    /// ExecutionState 的全局阶段仍保留给开放式兼容路径和遥测。
    pub fn allowed_tools(&self) -> Vec<String> {
        let phase = self.phase();
        if let Some(item) = self.active_item() {
            if item.phase_attempts.get(phase) >= item.phase_budget.get(phase) {
                return Vec::new();
            }
        }
        // S4：静态可证的交付面在验证阶段额外放开 `fs`。此前只给 `shell`，等于强迫
        // 界面/字段/签名这类面去跑一次它证明不了的命令——本该读一眼产物就能收敛。
        let statically_provable = self
            .active_item()
            .map(|item| {
                matches!(
                    self.convergence_for(item),
                    ConvergenceOutcome::StaticallyProvable { .. }
                )
            })
            .unwrap_or(false);
        let names: &[&str] = match self.active_item().map(|item| item.state) {
            Some(WorkItemState::Pending | WorkItemState::Locating) => &["search"],
            Some(WorkItemState::Located | WorkItemState::Inspecting) => &["fs", "search"],
            Some(WorkItemState::ReadyToChange) if self.read_only => &["fs", "search"],
            Some(WorkItemState::ReadyToChange) => &["edit", "fs"],
            Some(WorkItemState::Satisfied | WorkItemState::Changed) if statically_provable => {
                &["fs", "shell"]
            }
            Some(WorkItemState::Satisfied | WorkItemState::Changed) => &["shell"],
            Some(
                WorkItemState::Verified | WorkItemState::NeedsUserInput | WorkItemState::Failed,
            )
            | None => &[],
        };
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    pub fn phase_name(&self) -> &'static str {
        self.phase().as_str()
    }

    pub fn phase(&self) -> SolvePhase {
        match self.active_item().map(|item| item.state) {
            Some(WorkItemState::Pending | WorkItemState::Locating) => SolvePhase::Locate,
            Some(WorkItemState::Located | WorkItemState::Inspecting) => SolvePhase::Inspect,
            Some(WorkItemState::ReadyToChange) if self.read_only => SolvePhase::Inspect,
            Some(WorkItemState::ReadyToChange) => SolvePhase::Change,
            Some(WorkItemState::Satisfied | WorkItemState::Changed) => SolvePhase::Verify,
            _ => SolvePhase::Conclude,
        }
    }

    pub fn active_hypothesis_summary(&self) -> String {
        self.active_item()
            .and_then(|item| item.hypotheses.get(item.active_hypothesis))
            .map(|hypothesis| format!("{}：{}", hypothesis.id, hypothesis.description))
            .unwrap_or_else(|| "无活动假设".into())
    }

    pub fn next_action_hint(&self) -> String {
        let Some(item) = self.active_item() else {
            return "没有可继续执行的工作项，请根据已有证据生成交付结论。".into();
        };
        match item.state {
            WorkItemState::Pending | WorkItemState::Locating => {
                "执行一个与目标实体直接相关的高信号 search；不要列目录或运行 git status。".into()
            }
            WorkItemState::Located | WorkItemState::Inspecting => format!(
                "直接读取候选文件 [{}] 中与当前验收项相关的最小区间。",
                self.target_files
                    .iter()
                    .take(4)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("、")
            ),
            WorkItemState::ReadyToChange if self.read_only => {
                "基于已确认的最短调用链形成证据结论；如仍有一个关键缺口，只读取对应候选。".into()
            }
            WorkItemState::ReadyToChange => {
                "对已确认文件执行一次最小编辑；若内容已满足目标，先读取确认后进入验证。".into()
            }
            WorkItemState::Satisfied => {
                "当前内容已经满足期望值；禁止重复编辑，运行最小验证。".into()
            }
            // S4：能否免 shell 收敛由**产物证据**决定。走到这里说明静态复核没通过
            // （断言缺失、产物未确认，或该面本就是行为面），必须实际执行。
            WorkItemState::Changed => match self.convergence_for(item) {
                ConvergenceOutcome::NeedsExecution(reason) => {
                    format!("运行一次覆盖当前改动的最小构建或测试（{reason}）。")
                }
                ConvergenceOutcome::StaticallyProvable { assertions, .. } => format!(
                    "改动尚未在产物中复核到 [{}]；先读取目标文件确认写入是否真的生效，再决定是否需要执行验证。",
                    assertions
                        .iter()
                        .take(4)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("、")
                ),
            },
            _ => "基于当前工作项证据收尾。".into(),
        }
    }

    /// 最终完成仍由求解图复核。受控修改要求所有项已验证；只读诊断至少要
    /// 真正读取一次候选调用链，不能把单次搜索命中或无命中当作根因证据。
    pub fn can_conclude(&self) -> bool {
        if self.read_only {
            return !self.items.is_empty()
                && self.items.values().all(|item| item.read_evidence > 0)
                && self.items.values().all(|item| {
                    !matches!(
                        item.state,
                        WorkItemState::Pending
                            | WorkItemState::Locating
                            | WorkItemState::Located
                            | WorkItemState::NeedsUserInput
                            | WorkItemState::Failed
                    )
                });
        }
        self.items
            .values()
            .all(|item| item.state == WorkItemState::Verified)
    }

    /// 连续无信息增益的容忍上限。达到后假设空间基本耗尽，继续让模型换关键词
    /// 只是在烧剩余预算——这正是"蛮力搜索 N 次后熔断"的成因。
    const MAX_NO_INFORMATION: usize = 4;

    /// 是否陷入无信息增益的空转。已产生写入或验证的工作项说明有实质进展，
    /// 此时不因后续动作的空转而中断。
    pub fn is_stalled(&self) -> bool {
        self.no_information_count >= Self::MAX_NO_INFORMATION
            && !self.items.values().any(|item| {
                matches!(
                    item.state,
                    WorkItemState::Changed | WorkItemState::Verified
                )
            })
    }

    /// 空转终止报告。把试过的假设讲清楚，让用户能精准补充信息，
    /// 而不是笼统地说一句"执行失败"。
    pub fn stall_report(&self) -> String {
        let mut parts = Vec::new();
        for item in self.items.values() {
            let tried: Vec<String> = item
                .hypotheses
                .iter()
                .filter(|hypothesis| hypothesis.attempts > 0)
                .map(|hypothesis| match hypothesis.state {
                    HypothesisState::Rejected => format!("{}（已排除）", hypothesis.description),
                    HypothesisState::Confirmed => format!("{}（已确认）", hypothesis.description),
                    HypothesisState::Active => format!("{}（仍在进行）", hypothesis.description),
                })
                .collect();
            if !tried.is_empty() {
                parts.push(format!("[{}] {}", item.id, tried.join("；")));
            }
        }
        let tried = if parts.is_empty() {
            format!(
                "已执行 {} 次定位动作，均未产出新的候选文件或证据",
                self.no_information_count
            )
        } else {
            parts.join("\n")
        };
        format!(
            "连续 {} 次动作没有产生新的定位证据，继续搜索不会再产生新信息。\n已尝试的假设：\n{}\n请补充具体的文件路径、目录或关键符号。",
            self.no_information_count, tried
        )
    }

    /// 受控任务的唯一完成判定。旧 ExecutionState 只保留统计与 legacy 路径，
    /// 不再与 SolveGraph 竞争控制阶段或终态。
    pub fn evaluate_completion(&self, step_had_tools: bool) -> GoalCompletion {
        if let Some(reason) = self.actionable_terminal_reason() {
            if self
                .items
                .values()
                .any(|item| item.state == WorkItemState::NeedsUserInput)
            {
                return GoalCompletion::Terminal(reason);
            }
        }
        // 空转熔断必须在"继续"之前判定。旧实现只累加 no_information_count 却
        // 从不据此停止，于是模型能一路换关键词烧到硬熔断为止。
        if self.is_stalled() {
            return GoalCompletion::Terminal(self.stall_report());
        }
        if self.can_conclude() {
            return if step_had_tools {
                GoalCompletion::Continue
            } else {
                GoalCompletion::Complete
            };
        }
        if step_had_tools {
            GoalCompletion::Continue
        } else {
            GoalCompletion::Correct(self.next_action_hint())
        }
    }

    pub fn allows_tool_call(
        &self,
        call: &ToolCall,
        proposal: &ActionProposal,
    ) -> Result<(), String> {
        let allowed = self.allowed_tools();
        let phase = self.phase();
        if let Some(item) = self.active_item() {
            if item.phase_attempts.get(phase) >= item.phase_budget.get(phase) {
                return Err(format!(
                    "当前 {} 阶段预算已耗尽；必须切换假设或报告精确阻塞，禁止继续同类动作。",
                    phase.as_str()
                ));
            }
        }
        if !allowed.iter().any(|tool| tool == &call.name) {
            return Err(format!(
                "当前工作项状态只允许 [{}]；{}",
                allowed.join("、"),
                self.next_action_hint()
            ));
        }
        if call.name == "fs" && call.args.get("op").and_then(|value| value.as_str()) == Some("list")
        {
            return Err(format!("受控交付禁止目录枚举；{}", self.next_action_hint()));
        }
        if self.anchor_dirs.is_empty() || !matches!(call.name.as_str(), "search" | "fs" | "edit") {
            return Ok(());
        }
        let path = match call.name.as_str() {
            "search" => call.args.get("dir").and_then(|value| value.as_str()),
            "fs" | "edit" => call.args.get("path").and_then(|value| value.as_str()),
            _ => None,
        };
        let Some(path) = path else {
            return Err(format!(
                "已定位共享目录 [{}]；{} 调用必须显式提供该目录内的 path/dir，禁止回到工作区根泛搜。",
                self.anchor_dirs.join("、"), call.name
            ));
        };
        let normalized = path.replace('\\', "/").trim_start_matches("./").to_owned();
        let exact_target = self
            .target_files
            .iter()
            .any(|target| normalize_path(target) == normalized);
        if exact_target
            || self
                .anchor_dirs
                .iter()
                .any(|anchor| path_is_within(&normalized, anchor))
        {
            Ok(())
        } else {
            Err(format!(
                "当前工作项 {} 已定位到 [{}]，本次 {} 的路径 {} 不在已确认调用链中。请只读取/修改锚点目录，或基于现有证据说明需要切换哪个子项目。",
                proposal.supports.first().cloned().unwrap_or_default(),
                self.anchor_dirs.join("、"), call.name, path
            ))
        }
    }

    pub fn record_result(
        &mut self,
        proposal: &ActionProposal,
        ok: bool,
        summary: &str,
    ) -> EvidenceKind {
        let tool = proposal
            .signature
            .split_once(':')
            .map(|part| part.0)
            .unwrap_or("");
        let item_id = proposal.supports.first().cloned().unwrap_or_default();
        let action = ActionContract {
            work_item_id: item_id,
            phase: phase_for_signature(&proposal.signature),
            hypothesis_id: "compat".into(),
            tool: tool.into(),
            target_path: extract_signature_path(&proposal.signature),
            purpose: proposal.question.clone(),
            expected_signal: "兼容调用产生新证据".into(),
            on_hit: "推进工作项".into(),
            on_miss: "切换假设".into(),
            max_cost: proposal.estimated_cost,
        };
        self.record_action_result(&action, proposal, ok, summary)
    }

    pub fn record_action_result(
        &mut self,
        action: &ActionContract,
        proposal: &ActionProposal,
        ok: bool,
        summary: &str,
    ) -> EvidenceKind {
        let is_search = action.tool == "search";
        let is_read = action.tool == "fs" && !proposal.signature.contains("\"op\":\"write\"");
        let is_write = action.tool == "edit" || proposal.signature.contains("\"op\":\"write\"");
        let is_verify = action.phase == SolvePhase::Verify || is_verification(&proposal.signature);
        let effective_ok = ok && !proposal.is_search_miss(summary);
        let previous_target_count = self.target_files.len();
        if effective_ok && is_search {
            self.record_targets_from_search(summary);
        }
        if let Some(path) = &action.target_path {
            if effective_ok && matches!(action.tool.as_str(), "fs" | "edit") {
                self.add_target_file(path);
            }
        }
        let already_satisfied = is_read
            && effective_ok
            && !self.goal.expected_values.is_empty()
            && self
                .goal
                .expected_values
                .iter()
                .all(|expected| summary.contains(&expected.value));
        let kind = if is_search && !effective_ok {
            EvidenceKind::HypothesisRejected
        } else if is_search && self.target_files.len() > previous_target_count {
            EvidenceKind::TargetFound
        } else if is_search {
            EvidenceKind::NoInformation
        } else if already_satisfied {
            EvidenceKind::AlreadySatisfied
        } else if is_verify && effective_ok {
            EvidenceKind::VerificationPassed
        } else if is_verify {
            EvidenceKind::VerificationFailed
        } else if is_write && effective_ok {
            EvidenceKind::ChangeApplied
        } else if is_read && effective_ok {
            EvidenceKind::DataFlowConfirmed
        } else {
            EvidenceKind::NoInformation
        };
        if kind == EvidenceKind::NoInformation {
            self.no_information_count = self.no_information_count.saturating_add(1);
        }
        if matches!(
            kind,
            EvidenceKind::HypothesisRejected | EvidenceKind::VerificationFailed
        ) {
            self.correction_count = self.correction_count.saturating_add(1);
        }
        let current_targets = self.target_files.clone();
        for id in &proposal.supports {
            let Some(item) = self.items.get_mut(id) else {
                continue;
            };
            item.phase_attempts.increment(action.phase);
            if let Some(hypothesis) = item.hypotheses.get_mut(item.active_hypothesis) {
                hypothesis.attempts = hypothesis.attempts.saturating_add(1);
            }
            item.evidence.push(summary.chars().take(240).collect());
            if kind == EvidenceKind::NoInformation {
                item.no_information_streak = item.no_information_streak.saturating_add(1);
                if item.no_information_streak >= 2
                    && matches!(
                        item.state,
                        WorkItemState::Pending
                            | WorkItemState::Locating
                            | WorkItemState::Located
                            | WorkItemState::Inspecting
                    )
                {
                    advance_hypothesis(item, action.phase);
                }
                // 无信息结果不得随后被“工具 ok”覆盖成 Inspecting/ReadyToChange。
                continue;
            } else {
                item.no_information_streak = 0;
            }
            if !effective_ok {
                if is_search {
                    item.locate_attempts = item.locate_attempts.saturating_add(1);
                    advance_hypothesis(item, action.phase);
                }
                continue;
            }
            if is_read {
                item.read_evidence = item.read_evidence.saturating_add(1);
            }
            item.state = if is_verify {
                WorkItemState::Verified
            } else if is_write {
                WorkItemState::Changed
            } else if already_satisfied {
                WorkItemState::Satisfied
            } else if is_read {
                if self.read_only {
                    WorkItemState::Inspecting
                } else {
                    WorkItemState::ReadyToChange
                }
            } else if is_search {
                WorkItemState::Inspecting
            } else {
                item.state
            };
            if matches!(
                kind,
                EvidenceKind::TargetFound | EvidenceKind::DataFlowConfirmed
            ) {
                if let Some(hypothesis) = item.hypotheses.get_mut(item.active_hypothesis) {
                    hypothesis.state = HypothesisState::Confirmed;
                }
            }
            for target in &current_targets {
                if !item.candidate_targets.contains(target) {
                    item.candidate_targets.push(target.clone());
                }
            }
        }
        if kind == EvidenceKind::VerificationFailed {
            self.map_verification_failure(summary, &proposal.supports);
        }
        kind
    }

    pub fn record_gate_rejection(&mut self, action: &ActionContract, reason: &str) {
        self.no_information_count = self.no_information_count.saturating_add(1);
        self.correction_count = self.correction_count.saturating_add(1);
        if let Some(item) = self.items.get_mut(&action.work_item_id) {
            item.evidence.push(format!(
                "门禁校正：{}",
                reason.chars().take(180).collect::<String>()
            ));
            item.no_information_streak = item.no_information_streak.saturating_add(1);
            if item.no_information_streak >= 2 {
                advance_hypothesis(item, action.phase);
            }
        }
    }

    fn map_verification_failure(&mut self, summary: &str, supported: &[String]) {
        let normalized = summary.replace('\\', "/");
        let mut mapped = false;
        for item in self.items.values_mut() {
            if !supported.contains(&item.id) {
                continue;
            }
            let mentioned = item.candidate_targets.iter().any(|target| {
                normalized.contains(target)
                    || target
                        .rsplit('/')
                        .next()
                        .is_some_and(|name| normalized.contains(name))
            });
            if mentioned {
                item.state = WorkItemState::ReadyToChange;
                mapped = true;
            }
        }
        if !mapped {
            for id in supported {
                if let Some(item) = self.items.get_mut(id) {
                    item.state = WorkItemState::ReadyToChange;
                }
            }
        }
    }

    fn record_targets_from_search(&mut self, summary: &str) {
        // Local Search 的标准格式为“相对路径:行号: 内容”。只采纳代码文件的
        // 父目录；结果中的普通文本、绝对临时路径或冒号后的源码均不会成为锚点。
        for line in summary.lines().skip(1).take(12) {
            let Some((candidate, rest)) = line.split_once(':') else {
                continue;
            };
            if !rest.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
                continue;
            }
            let candidate = candidate.replace('\\', "/");
            match candidate.rsplit_once('/') {
                Some((dir, file)) if file.contains('.') && !dir.is_empty() => {
                    self.add_target_file(&candidate);
                }
                None if candidate.contains('.') => self.add_target_file(&candidate),
                _ => {}
            }
            if self.target_files.len() >= 8 {
                break;
            }
        }
    }

    fn add_target_file(&mut self, path: &str) {
        let normalized = normalize_path(path);
        if normalized.is_empty() {
            return;
        }
        if !self
            .target_files
            .iter()
            .any(|item| normalize_path(item) == normalized)
        {
            self.target_files.push(normalized.clone());
        }
        if let Some((dir, _)) = normalized.rsplit_once('/') {
            let anchor = format!("{dir}/");
            if !dir.is_empty()
                && !self
                    .anchor_dirs
                    .iter()
                    .any(|item| normalize_path(item) == normalize_path(&anchor))
            {
                self.anchor_dirs.push(anchor);
            }
        }
    }

    /// 内部渲染原语：按 `include` 过滤交付面列表，`focus_surfaces` 非空时把"当前工作项"
    /// 指向首面并加上"[本轮聚焦]"标记。`render_for_model` 与并发执行器的
    /// `render_for_model_scoped` 共用此模板，避免两份格式字符串漂移。
    fn render_with_filter(
        &self,
        include: impl Fn(&WorkItem) -> bool,
        focus_surfaces: Option<&[String]>,
    ) -> String {
        let active = match focus_surfaces.and_then(|s| s.first()) {
            Some(id) => self.items.get(id),
            None => self.active_item(),
        };
        let lines = self
            .items
            .values()
            .filter(|item| include(item))
            .map(|item| {
                format!(
                    "- {}：{}（{}）",
                    item.id,
                    item.description,
                    item.state.as_str()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        // 字段顺序目标让"交换两个字段"从需要搜索的问题变成确定性的定位问题：
        // 找到同时含这些标签的表单文件即可，不需要任何探索性搜索。
        let field_order = self
            .goal
            .field_order
            .as_ref()
            .map(|order| format!("\n{}\n", order.render_for_model()))
            .unwrap_or_default();
        // 动作短语（"界面优化"）不是可定位字面量。显式标注可避免模型拿它去
        // 源码里做 contains 搜索——那样必然零命中。
        let actions = if self.goal.action_phrases.is_empty() {
            String::new()
        } else {
            format!(
                "\n[动作描述] 以下是用户要执行的操作，不是可在源码中搜索的界面文案：{}\n",
                self.goal.action_phrases.join("、")
            )
        };
        let parallel_plan = self.parallel_plan();
        let concept_checklist = self.concept_coverage_checklist();
        let focus_label = focus_surfaces
            .map(|s| format!("·本轮聚焦 {}", s.join("/")))
            .unwrap_or_default();
        format!(
            "[V4 唯一目标求解图{focus_label}]\n目标：{}\n期望值：{}\n导航：{}\n实体：{}，代码符号：{}\n候选文件：{}\n已确认目录：{}\n工作项：\n{}\n当前工作项：{}\n当前假设：{}\n阶段预算：locate {}/{} · inspect {}/{} · change {}/{} · verify {}/{}\n下一动作：{}{}{}\n{}\n{}\n规则：每个动作必须满足 ActionContract；命中按 on_hit 迁移，未命中按 on_miss 切换有限假设；有候选文件时直接读取，不得重新泛搜。",
            self.goal.objective,
            if self.goal.expected_values.is_empty() { "未结构化".into() } else { self.goal.expected_values.iter().map(|item| format!("{}={}", item.key, item.value)).collect::<Vec<_>>().join("、") },
            if self.goal.navigation.is_empty() { "未提供".into() } else { self.goal.navigation.join(" → ") },
            if self.goal.entities.is_empty() { "未提取".into() } else { self.goal.entities.join("、") },
            if self.goal.code_entities.is_empty() { "无".into() } else { self.goal.code_entities.join("、") },
            if self.target_files.is_empty() { "尚未定位".into() } else { self.target_files.iter().take(8).cloned().collect::<Vec<_>>().join("、") },
            if self.anchor_dirs.is_empty() { "尚未定位".into() } else { self.anchor_dirs.join("、") },
            lines,
            active.map(|item| format!("{}：{}", item.id, item.description)).unwrap_or_else(|| "无".into()),
            self.active_hypothesis_summary(),
            active.map(|item| item.phase_attempts.locate).unwrap_or(0), active.map(|item| item.phase_budget.locate).unwrap_or(0),
            active.map(|item| item.phase_attempts.inspect).unwrap_or(0), active.map(|item| item.phase_budget.inspect).unwrap_or(0),
            active.map(|item| item.phase_attempts.change).unwrap_or(0), active.map(|item| item.phase_budget.change).unwrap_or(0),
            active.map(|item| item.phase_attempts.verify).unwrap_or(0), active.map(|item| item.phase_budget.verify).unwrap_or(0),
            self.next_action_hint(),
            field_order,
            actions,
            parallel_plan,
            concept_checklist,
        )
    }

    pub fn render_for_model(&self) -> String {
        self.render_with_filter(|_| true, None)
    }

    /// S5/G2 并发执行器（Phase 1，作用域化提示）：只列出 `scope` 内的交付面，并把
    /// "当前工作项"指向 scope 的首面，使该并发轮的模型专注于这组无写冲突的面。
    /// 单面作用域（`scope.len() <= 1`）退化为 `render_for_model`，保证与单流路径
    /// 字节一致；`parallel_plan` 与跨面漏改清单仍全量注入（跨面一致性需全局视角，
    /// 不能因作用域而漏警）。
    pub fn render_for_model_scoped(&self, scope: &[String]) -> String {
        if scope.len() <= 1 {
            return self.render_for_model();
        }
        self.render_with_filter(|item| scope.contains(&item.id), Some(scope))
    }

    pub fn actionable_terminal_reason(&self) -> Option<String> {
        let waiting = self
            .items
            .values()
            .filter(|item| item.state == WorkItemState::NeedsUserInput)
            .map(|item| {
                format!(
                    "{}（{}）",
                    item.description,
                    item.evidence.last().cloned().unwrap_or_default()
                )
            })
            .collect::<Vec<_>>();
        if !waiting.is_empty() {
            return Some(format!(
                "需要用户确认工作区或目标路径：{}。已停止无效搜索；请提供正确子项目、分支或页面实现目录。",
                waiting.join("；")
            ));
        }
        let changed = self
            .items
            .values()
            .filter(|item| {
                matches!(
                    item.state,
                    WorkItemState::Changed | WorkItemState::Satisfied
                )
            })
            .map(|item| item.description.clone())
            .collect::<Vec<_>>();
        if !changed.is_empty() {
            return Some(format!(
                "已完成部分交付面：{}；尚未获得完整验证证据。",
                changed.join("、")
            ));
        }
        None
    }
}

fn default_hypotheses() -> Vec<Hypothesis> {
    vec![
        Hypothesis {
            id: "H1".into(),
            description: "目标由共享字段、配置或直接符号实现".into(),
            attempts: 0,
            state: HypothesisState::Active,
        },
        Hypothesis {
            id: "H2".into(),
            description: "目标位于具体交付面或边界映射中".into(),
            attempts: 0,
            state: HypothesisState::Rejected,
        },
    ]
}

fn advance_hypothesis(item: &mut WorkItem, phase: SolvePhase) {
    if let Some(current) = item.hypotheses.get_mut(item.active_hypothesis) {
        current.state = HypothesisState::Rejected;
    }
    if item.active_hypothesis + 1 < item.hypotheses.len() {
        item.active_hypothesis += 1;
        item.hypotheses[item.active_hypothesis].state = HypothesisState::Active;
        item.no_information_streak = 0;
        item.phase_attempts.reset(phase);
    } else {
        item.state = WorkItemState::NeedsUserInput;
    }
}

fn is_verification(signature: &str) -> bool {
    signature.starts_with("shell:")
        && ["test", "check", "build", "pytest", "tsc "]
            .iter()
            .any(|marker| signature.to_ascii_lowercase().contains(marker))
}

fn phase_for_signature(signature: &str) -> SolvePhase {
    if is_verification(signature) {
        SolvePhase::Verify
    } else if signature.starts_with("search:") {
        SolvePhase::Locate
    } else if signature.starts_with("fs:") {
        SolvePhase::Inspect
    } else if signature.starts_with("edit:") {
        SolvePhase::Change
    } else {
        SolvePhase::Conclude
    }
}

fn extract_signature_path(signature: &str) -> Option<String> {
    let (_, json) = signature.split_once(':')?;
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    value
        .get("path")
        .or_else(|| value.get("dir"))
        .and_then(|path| path.as_str())
        .map(str::to_string)
}

fn extract_expected_values(input: &str) -> Vec<ExpectedValue> {
    const MARKERS: [&str; 5] = ["修改为", "设置为", "更新为", "改为", "设为"];
    for marker in MARKERS {
        let Some((before, after)) = input.split_once(marker) else {
            continue;
        };
        let value = after
            .trim()
            .split(|ch: char| ch.is_whitespace() || "，。；;、)）".contains(ch))
            .next()
            .unwrap_or("")
            .trim_matches(['`', '\'', '"', '“', '”', '‘', '’']);
        if value.is_empty() || value.chars().count() > 80 {
            continue;
        }
        let key = before
            .trim()
            .rsplit(|ch: char| ch.is_whitespace() || "，。；;、".contains(ch))
            .next()
            .unwrap_or("目标值")
            .chars()
            .rev()
            .take(24)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>();
        return vec![ExpectedValue {
            key,
            value: value.into(),
        }];
    }
    Vec::new()
}

pub(crate) fn extract_exact_transformation(input: &str) -> Option<ExactTransformation> {
    const MARKERS: [&str; 5] = ["修改为", "名称改为", "重命名为", "替换为", "改为"];
    for marker in MARKERS {
        let Some((before, after)) = input.split_once(marker) else {
            continue;
        };
        let to_value = after
            .trim()
            .split(|ch: char| ch.is_whitespace() || "，。；;、)）".contains(ch))
            .next()
            .unwrap_or_default()
            .trim_matches(['`', '\'', '"', '“', '”', '‘', '’'])
            .to_string();
        if to_value.is_empty() || to_value.chars().count() > 80 {
            continue;
        }
        let navigation_source = (before.contains('>') || before.contains('→'))
            .then(|| {
                before
                    .rsplit(['>', '→'])
                    .next()
                    .unwrap_or_default()
                    .split(['，', ',', '。', ';', '；'])
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .trim_matches(['`', '\'', '"', '“', '”', '‘', '’'])
                    .to_string()
            })
            .filter(|value| value.chars().count() >= 2 && value.chars().count() <= 80);
        return Some(ExactTransformation {
            // 只有导航末级明确给出旧值时才进行确定性 literal grounding。
            // “把版本号修改为 1.2.3”中的“版本号”是目标字段，不是旧值。
            from_value: navigation_source,
            to_value,
        });
    }
    None
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_owned()
}

fn path_is_within(path: &str, anchor: &str) -> bool {
    let path = normalize_path(path);
    let anchor = normalize_path(anchor);
    path == anchor
        || path
            .strip_prefix(&anchor)
            .is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_no_information_terminates_with_the_tried_hypotheses() {
        let mut plan =
            GoalExecution::from_contract(&TaskContract::from_input("点击登录按钮无反应"));
        plan.no_information_count = GoalExecution::MAX_NO_INFORMATION;
        match plan.evaluate_completion(false) {
            GoalCompletion::Terminal(reason) => {
                assert!(reason.contains("没有产生新的定位证据"), "{reason}");
                assert!(reason.contains("已尝试的假设"), "{reason}");
            }
            other => panic!("连续无信息增益应触发终止，实际得到 {other:?}"),
        }
    }

    #[test]
    fn stall_breaker_stays_dormant_before_the_threshold() {
        let mut plan =
            GoalExecution::from_contract(&TaskContract::from_input("点击登录按钮无反应"));
        plan.no_information_count = GoalExecution::MAX_NO_INFORMATION - 1;
        assert!(!plan.is_stalled());
        assert!(!matches!(
            plan.evaluate_completion(false),
            GoalCompletion::Terminal(_)
        ));
    }

    #[test]
    fn real_progress_disarms_the_stall_breaker() {
        let mut plan =
            GoalExecution::from_contract(&TaskContract::from_input("点击登录按钮无反应"));
        plan.no_information_count = GoalExecution::MAX_NO_INFORMATION;
        if let Some(item) = plan.items.values_mut().next() {
            item.state = WorkItemState::Changed;
        }
        assert!(
            !plan.is_stalled(),
            "已产生写入说明有实质进展，不应因后续空转而中断"
        );
    }

    #[test]
    fn locatable_signal_covers_entities_navigation_and_literals() {
        let with_entity = GoalContract::compile("系统管理->模型管理，把模型名称字段顺序调整一下");
        assert!(
            with_entity.has_locatable_signal(),
            "中文实体与导航都应算可定位信号"
        );

        let with_literal = GoalContract::compile("后台管理->多端拼装，菜单名称修改为智能体装配");
        assert!(with_literal.has_locatable_signal(), "明确旧值也是可定位信号");

        let with_nothing = GoalContract::compile("嗯");
        assert!(
            !with_nothing.has_locatable_signal(),
            "完全提取不到信号时应如实报告，避免 Agent 盲搜"
        );
    }

    #[test]
    fn field_swap_compiles_into_a_search_free_goal() {
        let plan = GoalExecution::from_contract(&TaskContract::from_input(
            "界面优化，系统管理->模型管理，把模型名称字段，API KEY字段位置顺序互换一下，即把KEY放在排在前面。",
        ));
        let order = plan
            .goal
            .field_order
            .as_ref()
            .expect("字段顺序互换应编译成结构化目标");
        assert_eq!(order.desired_order(), vec![1, 0]);
        assert!(plan.render_for_model().contains("[顺序调整目标]"));
        // 未裁决时展示用户原话；工作区裁决后由 effective_fields 给出代码里的实际形式。
        assert_eq!(
            order.effective_fields(),
            ["模型名称字段", "API KEY字段"],
            "未经工作区裁决前应原样保留用户说法"
        );
    }

    /// V5 S1：裁决发生在有工作区之后，而不是编译期靠词表猜测。
    #[test]
    fn field_labels_are_resolved_by_the_workspace_not_a_word_list() {
        let root = std::env::temp_dir().join(format!("goalexec-fields-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/model-form.tsx"),
            "<label>模型名称</label><input name=\"apiKey\" />",
        )
        .unwrap();
        let mut plan = GoalExecution::from_contract(&TaskContract::from_input(
            "界面优化，系统管理->模型管理，把模型名称字段，API KEY字段位置顺序互换一下。",
        ));
        let index = WorkspaceIndex::build(&root);
        plan.goal.resolve_against(&index);
        let order = plan.goal.field_order.as_ref().expect("应编译出顺序目标");
        assert_eq!(
            order.effective_fields(),
            ["模型名称", "API KEY"],
            "工作区裁决后应变成代码里真实存在的写法，实际 {:?}",
            order.effective_fields()
        );
        assert_eq!(order.desired_order(), vec![1, 0]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn multi_surface_contract_creates_separate_work_items() {
        let contract =
            TaskContract::from_input("应用档案列表、新增、编辑需要展示 appCode 和 subAppCode");
        let plan = GoalExecution::from_contract(&contract);
        assert_eq!(plan.items.len(), 3);
        assert!(plan.render_for_model().contains("列表展示"));
    }

    #[test]
    fn two_failed_locates_request_user_input() {
        let contract = TaskContract::from_input("修复登录按钮无反应");
        let mut plan = GoalExecution::from_contract(&contract);
        let proposal = ActionProposal {
            signature: "search:{\"pattern\":\"login\"}".into(),
            question: "locate".into(),
            supports: vec!["user-objective".into()],
            estimated_cost: 1,
        };
        plan.record_result(&proposal, false, "no match");
        plan.record_result(&proposal, false, "no match");
        assert!(plan.actionable_terminal_reason().is_some());
    }

    #[test]
    fn scheduler_moves_to_the_next_surface_after_a_change() {
        let contract = TaskContract::from_input("列表、新增、编辑都需要展示同一字段");
        let mut plan = GoalExecution::from_contract(&contract);
        let first = ActionProposal {
            signature: "edit:{\"path\":\"list.tsx\"}".into(),
            question: "修改列表".into(),
            supports: vec!["item-1".into()],
            estimated_cost: 1,
        };
        plan.record_result(&first, true, "列表已修改");
        assert_eq!(plan.active_item().unwrap().id, "item-2");
    }

    #[test]
    fn each_surface_holds_an_independent_phase_budget() {
        // S3 验收：多交付面任务里，每个面必须持有**完整且独立**的相位预算，
        // 不再因共享预算而被前面的面耗尽饿死（V5 §2.2 的根因）。
        let contract = TaskContract::from_input("列表、新增、编辑都需要展示 appCode");
        let plan = GoalExecution::from_contract(&contract);
        assert!(plan.items.len() >= 2, "应拆出多个交付面，实际 {}", plan.items.len());
        for item in plan.items.values() {
            assert_eq!(
                item.phase_budget,
                PhaseBudget::default(),
                "每个交付面应持有独立完整预算，而非共享同一份"
            );
        }
        // active_surfaces 返回当前所有可推进的面（此处均为 Pending）。
        assert_eq!(plan.active_surfaces().len(), plan.items.len());
        assert!(plan.active_item().is_some());
    }

    #[test]
    fn budget_demand_grows_linearly_with_delivery_surfaces() {
        // S3 验收（算术级）：旧模型的全局硬熔断是 `20 + min(面数-1,4)*2`，面数超过 5 之后
        // 总额不再增长，尾部面拿不到跑完四个相位的步数。供给量必须随面数单调增长，
        // 且每增加一个面至少多给该面 change + verify 的步数——否则饥饿只是被推后。
        let one = GoalExecution::from_contract(&TaskContract::from_input("把列表页展示 appCode"));
        let many = GoalExecution::from_contract(&TaskContract::from_input(
            "列表、新增、编辑、详情、导出都需要展示 appCode",
        ));
        let one_demand = one.required_budget();
        let many_demand = many.required_budget();
        assert!(
            many_demand.surfaces > one_demand.surfaces,
            "多面请求应切出更多交付面：{} vs {}",
            many_demand.surfaces,
            one_demand.surfaces
        );

        let extra_surfaces = many_demand.surfaces - one_demand.surfaces;
        let floor = PhaseBudget::default();
        let min_extra = extra_surfaces * (floor.change as usize + floor.verify as usize);
        assert!(
            many_demand.steps >= one_demand.steps + min_extra,
            "每多一个面至少要多供给 change+verify 的步数：{} 面 {} 步 vs {} 面 {} 步（下界 +{}）",
            many_demand.surfaces,
            many_demand.steps,
            one_demand.surfaces,
            one_demand.steps,
            min_extra
        );

        // 旧的截断式常量 `20 + min(面数-1, 4) * 2`：面数一多就不再增长。供给量必须在
        // 面数足够时越过它，否则尾部交付面依然在算术上不可能完成。
        let legacy_cap = |surfaces: usize| 20 + surfaces.saturating_sub(1).min(4) * 2;
        assert!(
            many_demand.surfaces >= 4,
            "五个界面的请求应至少切出 4 个面，实际 {}",
            many_demand.surfaces
        );
        assert!(
            many_demand.steps > legacy_cap(many_demand.surfaces),
            "{} 个面的供给 {} 步未越过旧截断上限 {} 步，饥饿未消除",
            many_demand.surfaces,
            many_demand.steps,
            legacy_cap(many_demand.surfaces)
        );

        // 已验证的面不再占用供给：预算按"未完成面"计量，避免长任务里总额虚高。
        let mut settled = GoalExecution::from_contract(&TaskContract::from_input(
            "列表、新增、编辑、详情、导出都需要展示 appCode",
        ));
        let before = settled.required_budget().steps;
        if let Some(id) = settled.items.keys().next().cloned() {
            settled.items.get_mut(&id).unwrap().state = WorkItemState::Verified;
        }
        let after = settled.required_budget().steps;
        assert!(
            after < before,
            "面验证完成后供给量应回落：{after} 应小于 {before}"
        );
    }

    /// S4 测试脚手架：造一个真实工作区目录，返回根路径。
    fn scratch_workspace(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("goalexec-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        root
    }

    /// S4 测试脚手架：把所有交付面推到"已修改待验证"，并绑定产物文件。
    fn park_surfaces_at_verification(plan: &mut GoalExecution, targets: &[&str]) {
        for item in plan.items.values_mut() {
            item.state = WorkItemState::Changed;
            item.candidate_targets = targets.iter().map(|path| (*path).to_string()).collect();
        }
    }

    #[test]
    fn remaining_surfaces_converge_on_disk_proof_without_shell() {
        // S4 验收（ADR §7）："剩 N 项验收"卡住场景必须消失。多个界面面都已改完、
        // 产物里也确实能逐字复核到目标，此时不该再逼 Agent 去跑一条它证明不了的命令。
        let root = scratch_workspace("s4-converge");
        for name in ["list.tsx", "create.tsx", "edit.tsx"] {
            std::fs::write(
                root.join("src").join(name),
                "<Column title=\"应用编码\" dataIndex=\"appCode\" />",
            )
            .unwrap();
        }
        let mut plan = GoalExecution::from_contract(&TaskContract::from_input(
            "列表、新增、编辑都需要展示 appCode",
        ));
        assert!(
            plan.static_assertions().iter().any(|a| a == "appCode"),
            "用户点名的代码符号应成为可逐字复核的断言，实际 {:?}",
            plan.static_assertions()
        );
        let surfaces = plan.items.len();
        assert!(surfaces >= 2, "应切出多个交付面，实际 {surfaces}");
        park_surfaces_at_verification(
            &mut plan,
            &["src/list.tsx", "src/create.tsx", "src/edit.tsx"],
        );

        // 未声明类别（默认）+ 有断言 + 有产物 → 判据允许静态收敛。
        let active = plan.active_item().unwrap().clone();
        assert!(
            matches!(
                plan.convergence_for(&active),
                ConvergenceOutcome::StaticallyProvable { .. }
            ),
            "有产物断言的交付面应允许静态收敛"
        );
        // 验证阶段必须能读产物：此前只给 shell，等于没有收敛通道。
        let tools = plan.allowed_tools();
        assert!(
            tools.iter().any(|name| name == "fs"),
            "静态可证的验证阶段应放开 fs 以复核产物，实际 {tools:?}"
        );

        let settled = plan.settle_static_convergence(&root);
        assert_eq!(
            settled.len(),
            surfaces,
            "全部面都能在产物里复核到断言，应全部收敛，实际 {settled:?}"
        );
        assert!(
            plan.items
                .values()
                .all(|item| item.state == WorkItemState::Verified),
            "静态收敛成功后交付面应置为已验证"
        );
        assert!(
            settled
                .iter()
                .all(|(_, proof)| proof.contains("appCode") && proof.contains("src/")),
            "证明摘要应指明命中的断言与产物文件，实际 {settled:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn already_satisfied_surfaces_also_converge_statically() {
        // S4 缺口回归防护：`AlreadySatisfied`（本来就对）与 `ChangeApplied`（刚改完）
        // 都会把交付面停在待验证。只处理后者等于只修了一半的卡死。
        let root = scratch_workspace("s4-satisfied");
        std::fs::write(root.join("Cargo.toml"), "[package]\nversion = \"0.2.2\"\n").unwrap();
        let mut plan =
            GoalExecution::from_contract(&TaskContract::from_input("把版本号修改为 0.2.2"));
        for item in plan.items.values_mut() {
            item.state = WorkItemState::Satisfied;
            item.candidate_targets = vec!["Cargo.toml".into()];
        }
        let settled = plan.settle_static_convergence(&root);
        assert_eq!(
            settled.len(),
            1,
            "已满足且产物可复核的面应静态收敛，实际 {settled:?}"
        );
        assert!(
            plan.items
                .values()
                .all(|item| item.state == WorkItemState::Verified)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn static_convergence_refuses_to_rubber_stamp_missing_evidence() {
        // S4 安全方向：判据"允许"静态收敛 ≠ 已经收敛。产物里复核不到就什么都不改，
        // 该面继续走实际执行验证。同时证明复核是**每次重读磁盘**，不吃陈旧索引。
        let root = scratch_workspace("s4-nostamp");
        let file = root.join("src/list.tsx");
        std::fs::write(&file, "<Column title=\"名称\" dataIndex=\"name\" />").unwrap();
        let mut plan =
            GoalExecution::from_contract(&TaskContract::from_input("列表页需要展示 appCode"));
        park_surfaces_at_verification(&mut plan, &["src/list.tsx"]);

        assert!(
            plan.settle_static_convergence(&root).is_empty(),
            "产物里没有 appCode 时不得盖章通过"
        );
        assert!(
            plan.items
                .values()
                .all(|item| item.state == WorkItemState::Changed),
            "复核失败应保持原状态，继续走执行验证"
        );

        // 真正改到磁盘之后，同一份计划立刻能复核通过 —— 证据来自磁盘当前内容。
        std::fs::write(&file, "<Column title=\"应用编码\" dataIndex=\"appCode\" />").unwrap();
        assert_eq!(
            plan.settle_static_convergence(&root).len(),
            1,
            "磁盘产物补齐后应立即复核通过，说明读的是磁盘而非陈旧索引"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn illegal_convergence_declaration_falls_back_to_execution() {
        // S4 验收（ADR §7 / D4）：非法判据必须正确回落。"看不懂就当没声明"会让
        // 一个拼写错误变成免检通道，因此非法声明一律回落 Unknown → 强制实际执行。
        let root = scratch_workspace("s4-illegal");
        std::fs::write(
            root.join("src/list.tsx"),
            "<Column dataIndex=\"appCode\" />",
        )
        .unwrap();
        let mut plan =
            GoalExecution::from_contract(&TaskContract::from_input("列表页需要展示 appCode"));
        park_surfaces_at_verification(&mut plan, &["src/list.tsx"]);
        let id = plan.items.keys().next().cloned().unwrap();

        // 白名单内的声明：合法，且只影响判据严格程度。
        assert!(plan.declare_surface_kind(&id, "UI"), "白名单值应判为合法声明");
        assert_eq!(plan.items[&id].kind, SurfaceKind::Ui);

        // 白名单外的声明：判为非法，回落 Unknown，并留下痕迹。
        assert!(
            !plan.declare_surface_kind(&id, "我看过了，没问题"),
            "白名单外的声明必须判为非法"
        );
        assert_eq!(
            plan.items[&id].kind,
            SurfaceKind::Unknown,
            "非法声明必须回落 Unknown，而不是当作未声明放宽判据"
        );
        assert!(
            plan.items[&id]
                .evidence
                .iter()
                .any(|line| line.contains("非法")),
            "非法声明应留下可审计痕迹，实际 {:?}",
            plan.items[&id].evidence
        );

        let item = plan.items[&id].clone();
        match plan.convergence_for(&item) {
            ConvergenceOutcome::NeedsExecution(reason) => {
                assert!(reason.contains("必须实际执行"), "{reason}")
            }
            other => panic!("非法声明回落后必须强制执行验证，实际 {other:?}"),
        }
        // 即使产物里查得到断言，也不许静态收敛 —— 声明只能收紧，不能放松。
        assert!(
            plan.settle_static_convergence(&root).is_empty(),
            "回落 Unknown 的面不得走静态收敛通道"
        );
        assert_eq!(plan.allowed_tools(), vec!["shell".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn behavior_surfaces_always_execute_their_verification() {
        // S4 硬约束（D4）：行为面可以声明"怎么验"，不得声明"已经验过了"。
        // 产物里查得到字面量，也不构成"点击后真的有反应"的证据。
        let root = scratch_workspace("s4-behavior");
        std::fs::write(root.join("src/login.tsx"), "onClick={handleLogin} appCode").unwrap();
        let mut plan = GoalExecution::from_contract(&TaskContract::from_input(
            "登录按钮点击后要能提交，涉及 appCode",
        ));
        park_surfaces_at_verification(&mut plan, &["src/login.tsx"]);
        let id = plan.items.keys().next().cloned().unwrap();

        assert!(
            plan.declare_surface_kind(&id, "behavior"),
            "behavior 在白名单内，属合法声明"
        );
        assert!(
            !SurfaceKind::Behavior.allows_static_convergence(),
            "行为面不得允许静态收敛"
        );
        let item = plan.items[&id].clone();
        assert!(
            matches!(
                plan.convergence_for(&item),
                ConvergenceOutcome::NeedsExecution(_)
            ),
            "行为面必须实际执行验证"
        );
        assert!(
            plan.settle_static_convergence(&root)
                .iter()
                .all(|(settled, _)| settled != &id),
            "行为面不得被静态收敛提前结案"
        );
        assert_eq!(
            plan.items[&id].state,
            WorkItemState::Changed,
            "行为面应停在待验证，等待真实执行"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn surfaces_without_verifiable_artifacts_need_execution() {
        // S4 分层的关键：不做语义分类。"能不能静态收敛"由**有没有可逐字复核的产物证据**
        // 决定 —— 没有断言或没有产物文件，自动落到必须执行，不需要任何领域词表。
        let mut plan = GoalExecution::from_contract(&TaskContract::from_input("把首页优化一下"));
        for item in plan.items.values_mut() {
            item.state = WorkItemState::Changed;
        }
        let item = plan.active_item().unwrap().clone();
        assert_eq!(item.kind, SurfaceKind::Undeclared, "默认应为未声明");
        match plan.convergence_for(&item) {
            ConvergenceOutcome::NeedsExecution(reason) => assert!(
                reason.contains("断言") || reason.contains("产物文件"),
                "缺少产物证据的原因应说清，实际 {reason}"
            ),
            other => panic!("拿不出产物断言的面不该允许静态收敛，实际 {other:?}"),
        }
        assert_eq!(
            plan.allowed_tools(),
            vec!["shell".to_string()],
            "无静态判据时验证阶段仍只给 shell"
        );
    }

    #[test]
    fn original_multi_surface_starvation_and_stuck_verification_is_fixed() {
        // V5 端到端验证：复现旧版两类"做不完 / 卡验收"失败模式，并证明已修复。
        //
        // 旧版根因（V5 §2.2 / §2.3）：
        //   1) 全局硬预算 = 20 + min(面数-1, 4)*2；5 面时只有 28 步，而 5 面各需 8 步
        //      共 40 步 → 尾部面在算术上不可能跑完（饿死，不是模型不会做）。
        //   2) 收敛只有 shell 一条路，界面/字段/签名这类静态可证面永远进不了 Verified。
        //
        // 新版断言：
        //   S3) required_budget 供水 ≥ 各面预算之和（线性），旧常数下则不足。
        //   S4) 全部静态可证面凭磁盘产物免 shell 结案；behavior 面必须实际执行。
        let root = scratch_workspace("v5-audit");
        let markers = ["GRID_LABEL", "GRID_SORTABLE", "API_TOKEN", "SCHEMA_AGE", "UI_SUBMIT"];
        let files: Vec<String> = (0..markers.len())
            .map(|i| format!("src/surface_{i}.rs"))
            .collect();
        // 每个产物文件写入全部目标字面量，模拟"交付已落地、可被逐字复核"。
        for i in 0..markers.len() {
            let body = markers
                .iter()
                .map(|m| format!("pub const {m}_MK: &str = \"{m}\";"))
                .collect::<Vec<_>>()
                .join("\n");
            std::fs::write(root.join(format!("src/surface_{i}.rs")), &body).unwrap();
        }

        let goal = GoalContract {
            objective: "多交付面任务：列标签、列可排序、接口带 token、schema 加 age、界面加提交按钮".into(),
            navigation: vec![],
            entities: vec![],
            candidates: vec![],
            code_entities: vec![],
            action_phrases: vec![],
            field_order: None,
            expected_state: String::new(),
            expected_values: markers
                .iter()
                .map(|m| ExpectedValue { key: "目标值".into(), value: (*m).to_string() })
                .collect(),
            transformation: None,
        };
        let mut plan = GoalExecution {
            goal,
            items: Default::default(),
            anchor_dirs: vec![],
            target_files: files.clone(),
            read_only: false,
            no_information_count: 0,
            correction_count: 0,
            step_attributed: vec![],
        };
        for (i, marker) in markers.iter().enumerate() {
            let id = format!("S{i}");
            plan.items.insert(
                id.clone(),
                WorkItem {
                    id,
                    description: format!("交付面 {i}: {marker}"),
                    state: WorkItemState::Changed,
                    locate_attempts: 0,
                    no_information_streak: 0,
                    read_evidence: 0,
                    evidence: vec![],
                    candidate_targets: files.clone(),
                    hypotheses: default_hypotheses(),
                    active_hypothesis: 0,
                    phase_attempts: PhaseAttempts::default(),
                    phase_budget: PhaseBudget::default(),
                    // 前 4 个面是界面/字段/签名类（静态可证）；第 5 个是运行时行为（必须执行）。
                    kind: if i < 4 { SurfaceKind::Ui } else { SurfaceKind::Behavior },
                    depends_on: vec![],
                    risk: SurfaceRisk::default(),
                },
            );
        }

        // —— S3：供水必须 ≥ 各面预算之和，否则尾部面仍会饿死。
        let demand = plan.required_budget();
        let per = PhaseBudget::default().total();
        let required_for_all = markers.len() * per;
        assert!(
            demand.steps >= required_for_all.saturating_sub(per),
            "供水不足：需约 {} 步，实际只供 {}",
            required_for_all,
            demand.steps
        );
        // 对照旧常数：5 面时只有 20 + min(4,4)*2 = 28 步，必然不足。
        let legacy_cap = 20 + (markers.len() - 1).min(4) * 2;
        assert!(
            demand.steps > legacy_cap,
            "新版供水({})应大于旧上限({})，否则尾部面仍会饿死",
            demand.steps,
            legacy_cap
        );

        // —— S4：4 个静态可证面凭磁盘产物免 shell 结案；behavior 面不结案。
        let settled = plan.settle_static_convergence(&root);
        assert_eq!(
            settled.len(),
            4,
            "应 4 个静态可证面免 shell 结案，实际 {} 个",
            settled.len()
        );
        for i in 0..4 {
            assert_eq!(
                plan.items[&format!("S{i}")].state,
                WorkItemState::Verified,
                "界面/字段/签名面应被静态收敛置 Verified"
            );
        }
        assert_eq!(
            plan.items["S4"].state,
            WorkItemState::Changed,
            "behavior 面不得免 shell 结案，必须实际执行"
        );
        match plan.convergence_for(&plan.items["S4"]) {
            ConvergenceOutcome::NeedsExecution(_) => {}
            other => panic!("behavior 面必须强制执行，实际 {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    // ===== S5：G2 DAG 全并行 + 预算守恒 + 环检测；G5 风险分层并发准入 =====

    fn three_surface_plan() -> GoalExecution {
        let contract =
            TaskContract::from_input("- 列表展示\n- 详情展示\n- 新增表单");
        GoalExecution::from_contract(&contract)
    }

    #[test]
    fn dag_cycle_is_detected_and_rejected() {
        let mut plan = three_surface_plan();
        // item-1 ↔ item-2 互相依赖构成环。
        plan.items.get_mut("item-1").unwrap().depends_on = vec!["item-2".into()];
        plan.items.get_mut("item-2").unwrap().depends_on = vec!["item-1".into()];
        let cycle = plan.detect_cycle();
        assert!(cycle.is_some(), "含环 DAG 必须被检出");
        // 含环草图回落静态模板后不应带环。
        let contract = TaskContract::from_input("- 列表展示\n- 详情展示");
        let json = r#"{"surfaces":[
            {"id":"item-1","kind":"ui","depends_on":["item-2"],
             "budget":{"locate":2,"inspect":2,"change":2,"verify":2},"convergence":"statically_provable"},
            {"id":"item-2","kind":"ui","depends_on":["item-1"],
             "budget":{"locate":2,"inspect":2,"change":2,"verify":2},"convergence":"statically_provable"}
        ]}"#;
        let plan2 = GoalExecution::from_input_with_sketch(&contract, Some(json));
        assert!(plan2.detect_cycle().is_none(), "环草图必须回落为无环静态模板");
        assert_eq!(plan2.items.len(), 2);
    }

    #[test]
    fn ready_surfaces_respect_dependency_order() {
        let mut plan = three_surface_plan();
        // item-2 依赖 item-1；item-1 尚未验证 → item-2 不应就绪。
        plan.items.get_mut("item-2").unwrap().depends_on = vec!["item-1".into()];
        let ready_before: Vec<&str> = plan
            .ready_surfaces()
            .iter()
            .map(|item| item.id.as_str())
            .collect();
        assert!(
            !ready_before.contains(&"item-2"),
            "依赖未满足的面不应进入就绪集"
        );
        // item-1 验证后，item-2 解锁。
        plan.items.get_mut("item-1").unwrap().state = WorkItemState::Verified;
        let ready_after: Vec<&str> = plan
            .ready_surfaces()
            .iter()
            .map(|item| item.id.as_str())
            .collect();
        assert!(
            ready_after.contains(&"item-2"),
            "依赖满足后 item-2 应进入就绪集"
        );
    }

    #[test]
    fn budget_conservation_under_parallel_does_not_multiply() {
        let plan = three_surface_plan();
        // 每面独立默认预算 8 步（locate2+inspect2+change2+verify2）。
        let per = PhaseBudget::default().total();
        assert_eq!(per, 8);
        let serial = plan.required_budget();
        let parallel = plan.required_budget_parallel();
        // 串行：共享定位按半额摊销 → 24 - 2*(locate/2=1) = 22。
        assert_eq!(serial.steps, 3 * per - 2, "串行应摊销共享定位");
        // 并行：满额计入，但绝不乘以并行度 → 恰好等于 Σ 每面预算 = 24。
        assert_eq!(parallel.steps, 3 * per, "并行不应乘以并行度");
        assert!(
            parallel.steps < 3 * per * 2,
            "并发占用上界必须守恒，不得放大"
        );
    }

    #[test]
    fn same_file_write_conflicts_are_serialized_into_one_group() {
        let mut plan = three_surface_plan();
        plan.items
            .get_mut("item-1")
            .unwrap()
            .candidate_targets = vec!["shared.tsx".into()];
        plan.items
            .get_mut("item-2")
            .unwrap()
            .candidate_targets = vec!["shared.tsx".into(), "other.tsx".into()];
        plan.items
            .get_mut("item-3")
            .unwrap()
            .candidate_targets = vec!["alone.tsx".into()];
        let groups = plan.parallel_write_groups();
        // item-1 与 item-2 共享 shared.tsx → 同组；item-3 独立一组。
        let shared_group = groups
            .iter()
            .find(|g| g.contains(&"item-1".to_string()))
            .expect("应有包含 item-1 的组");
        assert!(
            shared_group.contains(&"item-2".to_string()),
            "共享文件的面必须同组串行化"
        );
        assert!(
            !shared_group.contains(&"item-3".to_string()),
            "独立文件面不应被拉进冲突组"
        );
    }

    #[test]
    fn high_risk_surface_not_admitted_concurrently_with_shared_region() {
        let mut plan = three_surface_plan();
        // item-1 高风险，与 item-2 共享区域 a.tsx；item-3 独立区域。
        plan.items.get_mut("item-1").unwrap().risk = SurfaceRisk::High;
        plan.items.get_mut("item-1").unwrap().candidate_targets = vec!["a.tsx".into()];
        plan.items.get_mut("item-2").unwrap().risk = SurfaceRisk::Low;
        plan.items.get_mut("item-2").unwrap().candidate_targets = vec!["a.tsx".into()];
        plan.items.get_mut("item-3").unwrap().risk = SurfaceRisk::Low;
        plan.items.get_mut("item-3").unwrap().candidate_targets = vec!["b.tsx".into()];
        let admitted = plan.admit_concurrent(3);
        // 高风险面与共享区域的面不得同时准入（二者必居其一被推迟，不会并发）。
        assert!(
            !(admitted.contains(&"item-1".to_string()) && admitted.contains(&"item-2".to_string())),
            "高风险面不得与同区域面并发"
        );
        assert!(!admitted.is_empty(), "至少应有一个面被准入");
    }

    #[test]
    fn low_risk_surfaces_admitted_before_high_risk() {
        let mut plan = three_surface_plan();
        plan.items.get_mut("item-1").unwrap().risk = SurfaceRisk::Low;
        plan.items.get_mut("item-1").unwrap().candidate_targets = vec!["a.tsx".into()];
        plan.items.get_mut("item-2").unwrap().risk = SurfaceRisk::Low;
        plan.items.get_mut("item-2").unwrap().candidate_targets = vec!["b.tsx".into()];
        plan.items.get_mut("item-3").unwrap().risk = SurfaceRisk::High;
        plan.items.get_mut("item-3").unwrap().candidate_targets = vec!["c.tsx".into()];
        let admitted = plan.admit_concurrent(3);
        assert_eq!(admitted.len(), 3, "区域互不冲突时应全部准入");
        // 低风险（item-1/2）排在高风险（item-3）之前。
        assert_eq!(admitted[0], "item-1");
        assert_eq!(admitted[2], "item-3");
    }

    #[test]
    fn from_input_with_sketch_falls_back_on_invalid_json() {
        let contract = TaskContract::from_input("- 列表展示\n- 详情展示");
        // 非法 JSON：G1 失败不得影响可用性，回落静态模板。
        let plan = GoalExecution::from_input_with_sketch(&contract, Some("not json {"));
        assert_eq!(plan.items.len(), 2, "非法草图必须回落为静态模板");
        assert!(
            plan.items.values().all(|item| item.kind == SurfaceKind::Undeclared),
            "回落后面类别保持默认未声明"
        );
    }

    #[test]
    fn concept_registry_flags_cross_surface_missed_change() {
        let contract = TaskContract::from_input("在列表、详情、表单展示 appCode");
        let mut plan = GoalExecution::from_contract(&contract);
        // 让 appCode 进入 L0 信号，使概念注册表能建出该概念（确定性，不依赖编译抽取）。
        plan.goal.candidates.push("appCode".to_string());
        for id in ["item-1", "item-2", "item-3"] {
            plan.items.get_mut(id).unwrap().candidate_targets = vec!["src/user.tsx".into()];
        }
        // 仅 item-1 完成改动；item-2/item-3 同样引用 appCode 却没改到 → 漏改。
        plan.items.get_mut("item-1").unwrap().state = WorkItemState::Verified;
        let report = plan.missing_concept_coverage();
        let appcode = report.iter().find(|(symbol, _)| symbol == "appCode");
        assert!(appcode.is_some(), "appCode 跨面但未全改应被报漏改");
        let missing = &appcode.unwrap().1;
        assert!(missing.contains(&"item-2".to_string()), "item-2 漏改应被标记");
        assert!(missing.contains(&"item-3".to_string()), "item-3 漏改应被标记");
        assert!(
            !missing.contains(&"item-1".to_string()),
            "已完成的面不应出现在漏改列表"
        );
    }

    #[test]
    fn valid_sketch_drives_specialized_plan() {
        let contract = TaskContract::from_input("- 列表展示\n- 详情展示");
        let json = r#"{"surfaces":[
            {"id":"item-1","kind":"ui","depends_on":[],
             "budget":{"locate":2,"inspect":2,"change":2,"verify":2},"convergence":"statically_provable"},
            {"id":"item-2","kind":"behavior","depends_on":["item-1"],
             "budget":{"locate":1,"inspect":3,"change":2,"verify":2},"convergence":"needs_execution"}
        ]}"#;
        let plan = GoalExecution::from_input_with_sketch(&contract, Some(json));
        assert_eq!(plan.items.len(), 2);
        assert_eq!(plan.items["item-1"].kind, SurfaceKind::Ui);
        assert_eq!(plan.items["item-2"].kind, SurfaceKind::Behavior);
        assert_eq!(plan.items["item-2"].depends_on, vec!["item-1".to_string()]);
        // 高风险任务会让 behavior 面升到 High（schema/behavior 至少 Medium；任务默认 Low→Medium）。
        assert_eq!(plan.items["item-2"].risk, SurfaceRisk::Medium);
    }

    /// V5 §7 收尾验收：把"端到端耗时 / 漏改率 / 回落率"三项指标跑成可量化数字。
    ///
    /// 这是**脚本化**多面求解 e2e（无真实 LLM）：脚本执行器在真实临时目录写磁盘产物，
    /// 驱动真实 `GoalExecution` 状态机（from_contract → 定面/接地 → 供水 →
    /// admit_concurrent 循环 → 脚本化落盘 → settle_static_convergence 免 shell 结案）。
    /// 它坐实 S3（不饿死）、S4（可证面免 shell 结案）、S5-G4（防漏改）、S5-G5（风险准入）
    /// 在"一个完整多面任务"上协同工作，而不只是各单元各自成立。
    #[test]
    fn multi_surface_solve_e2e_quantifies_time_leak_and_fallback() {
        use crate::execution::{Criterion, RiskLevel, TaskContract};

        let contract = TaskContract {
            objective: "为 User 增加 email 字段，在 UI 列表、Schema 结构、API 响应中展示，\
                         并让行为面 on_click 后刷新 email，另含一处非法声明面"
                .to_string(),
            deliverables: vec!["email 字段端到端贯通".to_string()],
            acceptance_criteria: vec![
                Criterion { id: "item-1".into(), description: "UI 列表展示 email".into() },
                Criterion { id: "item-2".into(), description: "Schema 增加 email 字段".into() },
                Criterion { id: "item-3".into(), description: "API 响应包含 email".into() },
                Criterion { id: "item-4".into(), description: "on_click 后刷新 email".into() },
                Criterion { id: "item-5".into(), description: "legacy 面处理 email".into() },
            ],
            inferred_surface_criteria: false,
            scope: vec![],
            constraints: vec![],
            uncertainties: vec![],
            risk: RiskLevel::Medium,
        };

        // 真实磁盘产物：每个交付面一块文件。
        let root = scratch_workspace("s5-e2e");
        let files: Vec<(String, String)> = vec![
            ("item-1".into(), "src/ui.rs".into()),
            ("item-2".into(), "src/schema.rs".into()),
            ("item-3".into(), "src/api.rs".into()),
            ("item-4".into(), "src/behavior.rs".into()),
            ("item-5".into(), "src/legacy.rs".into()),
        ];
        for (_, rel) in &files {
            std::fs::write(root.join(rel), "").unwrap();
        }

        // 构建求解图（与当前 agent_loop 一致：from_contract 静态模板）。
        let mut plan = GoalExecution::from_contract(&contract);
        // 模拟 L2 裁决已确认 "email" 为锚点（真实场景由 resolve_against / compile 填充）。
        // 模拟 L2 裁决已确认 "email" 为唯一锚点（真实场景由 resolve_against / compile 填充），
        // 控制概念集以避免目标文案的 L1 切分噪声污染漏改报告。
        plan.goal.candidates = vec!["email".to_string()];
        plan.goal.code_entities = vec![];
        plan.goal.expected_values =
            vec![ExpectedValue { key: "目标值".into(), value: "email".into() }];
        // 每个面绑定自己的产物文件（机制层只认"email 在哪些文件出现"）。
        for (id, rel) in &files {
            plan.items.get_mut(id).unwrap().candidate_targets = vec![rel.clone()];
        }
        // 类别声明：ui/schema/api 允许静态收敛；behavior 必须执行；item-5 非法→Unknown。
        plan.items.get_mut("item-1").unwrap().kind = SurfaceKind::Ui;
        plan.items.get_mut("item-2").unwrap().kind = SurfaceKind::Schema;
        plan.items.get_mut("item-3").unwrap().kind = SurfaceKind::Api;
        plan.items.get_mut("item-4").unwrap().kind = SurfaceKind::Behavior;
        let legal = plan.declare_surface_kind("item-5", "telepathy");
        assert!(!legal, "非法判据声明应被拒，回落 Unknown");
        assert_eq!(plan.items["item-5"].kind, SurfaceKind::Unknown);

        // ===== 指标 3：回落率（静态，先于驱动）=====
        // 哪些面必须实际执行（不能免 shell 结案）。
        let needs_execution: Vec<&String> = plan
            .items
            .values()
            .filter(|item| {
                matches!(
                    plan.convergence_for(item),
                    ConvergenceOutcome::NeedsExecution(_)
                )
            })
            .map(|item| &item.id)
            .collect();
        let fallback_rate = needs_execution.len() as f64 / plan.items.len() as f64;
        assert_eq!(
            needs_execution.len(),
            2,
            "behavior + 非法声明面应回落执行：{needs_execution:?}"
        );
        assert!(
            (fallback_rate - 0.4).abs() < 1e-9,
            "回落率应为 2/5=0.4，实际 {fallback_rate}"
        );

        // ===== 指标 2：漏改率（部分改动 → 检出 → 全部改动 → 归零）=====
        // 仅把静态可证面（1/2/3）落盘并置已改；behavior/非法面（4/5）故意漏改。
        for (id, rel) in &files {
            if id == "item-4" || id == "item-5" {
                continue;
            }
            std::fs::write(root.join(rel), format!("email present in {rel}")).unwrap();
            plan.items.get_mut(id).unwrap().state = WorkItemState::Changed;
        }
        let leak_before = plan.missing_concept_coverage();
        let leak_rate_before =
            leak_before.iter().flat_map(|(_, ids)| ids).count() as f64 / plan.items.len() as f64;
        assert!(
            leak_before
                .iter()
                .any(|(sym, ids)| sym == "email" && ids.len() == 2),
            "email 应检出 2 个漏改面（item-4, item-5），实际 {leak_before:?}"
        );
        assert!(
            (leak_rate_before - 0.4).abs() < 1e-9,
            "漏改率应为 2/5=0.4，实际 {leak_rate_before}"
        );

        // 修正：把 4/5 也落盘并置已改 → 漏改归零。
        for (id, rel) in &files {
            if id == "item-4" || id == "item-5" {
                std::fs::write(root.join(rel), format!("email present in {rel}")).unwrap();
                plan.items.get_mut(id).unwrap().state = WorkItemState::Changed;
            }
        }
        let leak_after = plan.missing_concept_coverage();
        assert!(
            leak_after.is_empty(),
            "全部面改动后不应再报漏改：{leak_after:?}"
        );

        // ===== 指标 1：端到端耗时（驱动真实状态机直到全部 Verified）=====
        let mut run = GoalExecution::from_contract(&contract);
        run.goal.candidates = vec!["email".to_string()];
        run.goal.code_entities = vec![];
        run.goal.expected_values =
            vec![ExpectedValue { key: "目标值".into(), value: "email".into() }];
        for (id, rel) in &files {
            run.items.get_mut(id).unwrap().candidate_targets = vec![rel.clone()];
        }
        run.items.get_mut("item-1").unwrap().kind = SurfaceKind::Ui;
        run.items.get_mut("item-2").unwrap().kind = SurfaceKind::Schema;
        run.items.get_mut("item-3").unwrap().kind = SurfaceKind::Api;
        run.items.get_mut("item-4").unwrap().kind = SurfaceKind::Behavior;
        run.declare_surface_kind("item-5", "telepathy");

        // 供水（agent_loop 接线：多面时用 required_budget_parallel 抬升硬熔断）。
        let demand = run.required_budget_parallel();
        assert!(
            demand.steps >= 5 * 8,
            "全并行供水应覆盖 5 面 * 每面 8 步 = 40，实际 {}",
            demand.steps
        );
        assert!(
            demand.steps > 28,
            "全并行供水应高于旧计划常量上限 28（S3 饿死区间），实际 {}",
            demand.steps
        );

        let start = std::time::Instant::now();
        let mut ticks = 0;
        loop {
            ticks += 1;
            if ticks > 50 {
                panic!("求解循环未能在 50 tick 内终止——疑似饿死或环");
            }
            let admitted = run.admit_concurrent(2);
            if admitted.is_empty() {
                break;
            }
            for id in admitted {
                // 脚本执行器：把产物落到该面的磁盘文件（模拟 Agent 完成修改）。
                let targets = run.items[&id].candidate_targets.clone();
                for rel in &targets {
                    std::fs::write(root.join(rel), format!("email present in {rel}")).unwrap();
                }
                run.items.get_mut(&id).unwrap().state = WorkItemState::Changed;
                // 静态可证面在此免 shell 直接结案；behavior/非法面跳过（须执行）。
                run.settle_static_convergence(&root);
            }
            // 模拟执行器对"须执行面"的验证确认（真实 loop 由 LLM 跑命令后回报）。
            for item in run.items.values_mut() {
                if matches!(item.state, WorkItemState::Changed) {
                    item.state = WorkItemState::Verified;
                }
            }
            if run.items.values().all(|i| matches!(i.state, WorkItemState::Verified)) {
                break;
            }
        }
        let elapsed_ms = start.elapsed().as_millis();

        assert!(
            run.items
                .values()
                .all(|i| matches!(i.state, WorkItemState::Verified)),
            "所有交付面应走到 Verified，无饿死无卡验收"
        );
        assert!(ticks <= 4, "5 面并行度 2 应 ≤3 tick 完成，实际 {ticks}");
        assert!(
            elapsed_ms < 5_000,
            "机制层求解开销应远小于 5s，实际 {elapsed_ms}ms"
        );

        println!(
            "[S5 e2e] surfaces=5 ticks={ticks} elapsed_ms={elapsed_ms} \
             fallback_rate={fallback_rate:.2} leak_rate_before={leak_rate_before:.2} \
             leak_rate_after=0.00"
        );
    }

    /// S5/G1 e2e：合法 LLM 草图 → 特化计划（带类别与依赖 DAG）；非法草图 → 回落静态模板
    ///（D1 可用性优先）。这把"计划特化 + schema 校验 + 环检测 + 回落"在真实构建路径上跑通。
    #[test]
    fn g1_specialized_sketch_drives_plan_and_invalid_falls_back() {
        use crate::execution::{Criterion, RiskLevel, TaskContract};
        let contract = TaskContract {
            objective: "为 User 增加 email 字段，在 UI、Schema、API 展示".to_string(),
            deliverables: vec!["email 贯通".into()],
            acceptance_criteria: vec![
                Criterion { id: "item-1".into(), description: "UI 展示 email".into() },
                Criterion { id: "item-2".into(), description: "Schema 加 email".into() },
                Criterion { id: "item-3".into(), description: "API 含 email".into() },
            ],
            inferred_surface_criteria: false,
            scope: vec![],
            constraints: vec![],
            uncertainties: vec![],
            risk: RiskLevel::Medium,
        };
        let valid = r#"{
            "surfaces": [
                {"id":"item-1","kind":"ui","depends_on":[],
                 "budget":{"locate":2,"inspect":2,"change":2,"verify":2},
                 "convergence":"statically_provable"},
                {"id":"item-2","kind":"schema","depends_on":["item-1"],
                 "budget":{"locate":2,"inspect":2,"change":2,"verify":2},
                 "convergence":"statically_provable"},
                {"id":"item-3","kind":"api","depends_on":[],
                 "budget":{"locate":2,"inspect":2,"change":2,"verify":2},
                 "convergence":"statically_provable"}
            ]
        }"#;
        let specialized = GoalExecution::from_input_with_sketch(&contract, Some(valid));
        assert_eq!(specialized.items["item-1"].kind, SurfaceKind::Ui);
        assert_eq!(specialized.items["item-2"].kind, SurfaceKind::Schema);
        assert_eq!(
            specialized.items["item-2"].depends_on,
            vec!["item-1".to_string()]
        );
        assert!(
            specialized.detect_cycle().is_none(),
            "合法 DAG 不应含环，却检出 {cycle:?}",
            cycle = specialized.detect_cycle()
        );

        // 非法 kind → 校验失败 → 回落静态模板（D1 可用性优先）。
        let invalid = r#"{
            "surfaces": [
                {"id":"item-1","kind":"uii","depends_on":[],
                 "budget":{"locate":2,"inspect":2,"change":2,"verify":2},
                 "convergence":"statically_provable"}
            ]
        }"#;
        let fell_back = GoalExecution::from_input_with_sketch(&contract, Some(invalid));
        assert_eq!(
            fell_back.items["item-1"].kind,
            SurfaceKind::Undeclared,
            "非法草图必须回落为默认未声明面，不得悄悄采用特化计划"
        );
        assert!(fell_back.items["item-1"].depends_on.is_empty());
    }

    #[test]
    fn search_hit_locks_follow_up_actions_to_the_hit_directory() {
        let contract = TaskContract::from_input("列表、新增、编辑展示 appCode");
        let mut plan = GoalExecution::from_contract(&contract);
        let locate = ActionProposal {
            signature: "search:{\"pattern\":\"appCode\"}".into(),
            question: "定位字段".into(),
            supports: vec!["item-1".into()],
            estimated_cost: 1,
        };
        plan.record_result(
            &locate,
            true,
            "共 1 条命中（格式：相对路径:行号: 内容）：\nweb/src/pages/profile.tsx:42: appCode",
        );
        assert_eq!(plan.anchor_dirs, vec!["web/src/pages/"]);

        let outside = ToolCall {
            id: "read-outside".into(),
            name: "fs".into(),
            args: serde_json::json!({"op": "read", "path": "server/main.rs"}),
        };
        assert!(plan.allows_tool_call(&outside, &locate).is_err());
        let inside = ToolCall {
            id: "read-inside".into(),
            name: "fs".into(),
            args: serde_json::json!({"op": "read", "path": "web/src/pages/form.tsx"}),
        };
        assert!(plan.allows_tool_call(&inside, &locate).is_ok());
    }

    #[test]
    fn a_successful_empty_search_is_still_a_locate_miss() {
        let contract = TaskContract::from_input("修复应用档案 appCode 不显示");
        let mut plan = GoalExecution::from_contract(&contract);
        let locate = ActionProposal {
            signature: "search:{\"pattern\":\"appCode\"}".into(),
            question: "定位字段".into(),
            supports: vec!["user-objective".into()],
            estimated_cost: 1,
        };
        plan.record_result(&locate, true, "未找到匹配（pattern=\"appCode\"）。");
        assert_eq!(plan.active_item().unwrap().state, WorkItemState::Pending);
        plan.record_result(&locate, true, "未找到匹配（pattern=\"appCode\"）。");
        assert!(plan.active_item().is_none());
        assert!(plan
            .actionable_terminal_reason()
            .unwrap()
            .contains("需要用户确认"));
    }

    #[test]
    fn grounding_candidates_skip_redundant_search_and_allow_direct_read() {
        let contract = TaskContract::from_input("应用档案列表展示 appCode");
        let mut plan = GoalExecution::from_contract(&contract);
        let grounding = WorkspaceGrounding {
            status: crate::workspace_grounder::GroundingStatus::Grounded,
            scanned_files: 10,
            complete_scan: true,
            entity_hits: vec!["web/src/pages/profile.tsx".into()],
            navigation_hits: vec![],
            literal_hits: vec![],
            zero_prior: false,
        };
        plan.apply_grounding(&grounding);
        assert_eq!(plan.allowed_tools(), vec!["fs", "search"]);
        assert_eq!(plan.active_item().unwrap().state, WorkItemState::Located);

        let proposal = ActionProposal {
            signature: "fs:{\"op\":\"read\",\"path\":\"web/src/pages/profile.tsx\"}".into(),
            question: "read candidate".into(),
            supports: vec!["user-objective".into()],
            estimated_cost: 1,
        };
        let read = ToolCall {
            id: "read-candidate".into(),
            name: "fs".into(),
            args: serde_json::json!({"op": "read", "path": "web/src/pages/profile.tsx"}),
        };
        assert!(plan.allows_tool_call(&read, &proposal).is_ok());
    }

    #[test]
    fn anchor_matching_uses_directory_boundaries() {
        assert!(path_is_within("web/src/page.tsx", "web/src/"));
        assert!(!path_is_within("web/src-old/page.tsx", "web/src/"));
    }

    #[test]
    fn no_information_actions_rotate_a_finite_hypothesis_queue() {
        let contract = TaskContract::from_input("修复登录按钮无反应");
        let mut plan = GoalExecution::from_contract(&contract);
        let proposal = ActionProposal {
            signature: "search:{\"pattern\":\"login\"}".into(),
            question: "locate".into(),
            supports: vec!["user-objective".into()],
            estimated_cost: 1,
        };
        let hit = "共 1 条命中（格式：相对路径:行号: 内容）：\nweb/src/login.tsx:10: login";
        assert_eq!(
            plan.record_result(&proposal, true, hit),
            EvidenceKind::TargetFound
        );
        assert_eq!(
            plan.record_result(&proposal, true, hit),
            EvidenceKind::NoInformation
        );
        assert_eq!(
            plan.record_result(&proposal, true, hit),
            EvidenceKind::NoInformation
        );
        assert_eq!(
            plan.active_hypothesis_summary().split('：').next(),
            Some("H2")
        );
        plan.record_result(&proposal, true, hit);
        plan.record_result(&proposal, true, hit);
        assert!(plan.active_item().is_none(), "有限假设耗尽后必须停止");
        assert!(plan
            .actionable_terminal_reason()
            .unwrap()
            .contains("需要用户确认"));
    }

    #[test]
    fn changed_first_surface_does_not_lock_second_surface_in_global_change_phase() {
        let contract = TaskContract::from_input("列表、新增都要展示 appCode");
        let mut plan = GoalExecution::from_contract(&contract);
        plan.apply_grounding(&WorkspaceGrounding {
            status: crate::workspace_grounder::GroundingStatus::Grounded,
            scanned_files: 1,
            complete_scan: true,
            entity_hits: vec!["web/src/profile.tsx".into()],
            navigation_hits: vec![],
            literal_hits: vec![],
            zero_prior: false,
        });
        let inspect = ActionProposal {
            signature: "fs:{\"op\":\"read\",\"path\":\"web/src/profile.tsx\"}".into(),
            question: "inspect".into(),
            supports: vec!["item-1".into()],
            estimated_cost: 1,
        };
        plan.record_result(&inspect, true, "profile source");
        let edit = ActionProposal {
            signature: "edit:{\"path\":\"web/src/profile.tsx\"}".into(),
            question: "change".into(),
            supports: vec!["item-1".into()],
            estimated_cost: 1,
        };
        plan.record_result(&edit, true, "changed list");
        assert_eq!(plan.active_item().unwrap().id, "item-2");
        assert_eq!(plan.active_item().unwrap().state, WorkItemState::Located);
        assert_eq!(plan.allowed_tools(), vec!["fs", "search"]);
    }

    #[test]
    fn read_only_goal_requires_a_real_read_before_conclusion() {
        // 必须是"提问"形态才会被判为 Investigation（只读）。原祈使句"诊断…根因"
        // 在信号驱动模型里属于开放式任务，不触发只读约束。
        let contract = TaskContract::from_input("为什么登录按钮无反应？请诊断根因");
        let mut plan = GoalExecution::from_contract(&contract);
        let search = ActionProposal {
            signature: "search:{\"pattern\":\"login\"}".into(),
            question: "locate".into(),
            supports: vec!["user-objective".into()],
            estimated_cost: 1,
        };
        plan.record_result(
            &search,
            true,
            "共 1 条命中（格式：相对路径:行号: 内容）：\nsrc/login.tsx:10: onClick",
        );
        assert!(!plan.can_conclude(), "搜索命中不能直接充当根因证据");
        let read = ActionProposal {
            signature: "fs:{\"op\":\"read\",\"path\":\"src/login.tsx\"}".into(),
            question: "inspect".into(),
            supports: vec!["user-objective".into()],
            estimated_cost: 1,
        };
        plan.record_result(&read, true, "button has no on_click binding");
        assert!(plan.can_conclude());
        assert_eq!(plan.allowed_tools(), vec!["fs", "search"]);
    }

    #[test]
    fn expected_version_already_present_skips_edit_and_requires_verification() {
        let contract = TaskContract::from_input("把版本号修改为 0.2.2");
        let mut plan = GoalExecution::from_contract(&contract);
        plan.apply_grounding(&WorkspaceGrounding {
            status: crate::workspace_grounder::GroundingStatus::Grounded,
            scanned_files: 1,
            complete_scan: true,
            entity_hits: vec!["Cargo.toml".into()],
            navigation_hits: vec![],
            literal_hits: vec![],
            zero_prior: false,
        });
        let proposal = ActionProposal {
            signature: "fs:{\"op\":\"read\",\"path\":\"Cargo.toml\"}".into(),
            question: "read version".into(),
            supports: vec!["user-objective".into()],
            estimated_cost: 1,
        };
        assert_eq!(
            plan.record_result(&proposal, true, "[package]\nversion = \"0.2.2\""),
            EvidenceKind::AlreadySatisfied
        );
        assert_eq!(plan.active_item().unwrap().state, WorkItemState::Satisfied);
        // 已满足仍必须验证：不得直接结案。
        assert!(
            !plan.can_conclude(),
            "已满足只是免去修改，验证不能免"
        );
        // S4：版本号是可逐字复核的产物断言（0.2.2 @ Cargo.toml），因此验证阶段除 shell
        // 之外还放开 fs —— 读一眼产物就能收敛，不必去跑一条它证明不了的命令。
        assert_eq!(plan.allowed_tools(), vec!["fs", "shell"]);
    }

    #[test]
    fn action_contract_declares_both_transitions_and_cost() {
        let contract = TaskContract::from_input("修复登录按钮无反应");
        let mut plan = GoalExecution::from_contract(&contract);
        let call = ToolCall {
            id: "1".into(),
            name: "search".into(),
            args: serde_json::json!({"pattern": "login"}),
        };
        let mut proposal = ActionProposal {
            signature: "search:{\"pattern\":\"login\"}".into(),
            question: "locate".into(),
            supports: vec![],
            estimated_cost: 1,
        };
        plan.link_proposal(&mut proposal);
        let action = plan.action_spec(&call, &proposal).unwrap();
        assert_eq!(action.phase, SolvePhase::Locate);
        assert!(!action.on_hit.is_empty() && !action.on_miss.is_empty());
        assert_eq!(action.max_cost, 1);
    }

    #[test]
    fn verification_failure_reopens_only_the_mentioned_surface() {
        let contract = TaskContract::from_input("列表、新增都要展示 appCode");
        let mut plan = GoalExecution::from_contract(&contract);
        for (id, path) in [("item-1", "web/list.tsx"), ("item-2", "web/create.tsx")] {
            let item = plan.items.get_mut(id).unwrap();
            item.state = WorkItemState::Changed;
            item.candidate_targets.push(path.into());
        }
        let proposal = ActionProposal {
            signature: "shell:{\"cmd\":\"npm test\"}".into(),
            question: "verify".into(),
            supports: vec!["item-1".into(), "item-2".into()],
            estimated_cost: 1,
        };
        let action = ActionContract {
            work_item_id: "item-1".into(),
            phase: SolvePhase::Verify,
            hypothesis_id: "H1".into(),
            tool: "shell".into(),
            target_path: None,
            purpose: "verify".into(),
            expected_signal: "tests pass".into(),
            on_hit: "verified".into(),
            on_miss: "map failure".into(),
            max_cost: 1,
        };
        plan.record_action_result(
            &action,
            &proposal,
            false,
            "web/list.tsx:42 assertion failed",
        );
        assert_eq!(plan.items["item-1"].state, WorkItemState::ReadyToChange);
        assert_eq!(plan.items["item-2"].state, WorkItemState::Changed);
    }

    // ===== S5 闭环接入验证：G1 本地草图 / G2+G5 并行归属 / G4 防漏改 =====

    #[test]
    fn local_sketch_roundtrip_drives_from_sketch() {
        let contract = TaskContract::from_input("- 列表展示\n- 详情展示\n- 新增表单");
        // G1 闭环：本地计划器生成草图 → 回灌同一套校验 → from_sketch 真正运行。
        let json = crate::solve_sketch::SolveSketch::from_contract(&contract).to_json();
        let plan = GoalExecution::from_input_with_sketch(&contract, Some(&json));
        assert_eq!(plan.items.len(), 3, "草图应驱动 3 个面");
        assert!(plan.detect_cycle().is_none(), "本地草图必须无环");
        // 含依赖的草图应被 from_sketch 真正应用（证明不是回落 from_contract）。
        let dep_json = r#"{"surfaces":[
            {"id":"item-1","kind":"undeclared","depends_on":[],
             "budget":{"locate":2,"inspect":2,"change":2,"verify":2},"convergence":"needs_execution"},
            {"id":"item-2","kind":"undeclared","depends_on":["item-1"],
             "budget":{"locate":2,"inspect":2,"change":2,"verify":2},"convergence":"needs_execution"},
            {"id":"item-3","kind":"undeclared","depends_on":[],
             "budget":{"locate":2,"inspect":2,"change":2,"verify":2},"convergence":"needs_execution"}
        ]}"#;
        let plan2 = GoalExecution::from_input_with_sketch(&contract, Some(dep_json));
        assert_eq!(
            plan2.items["item-2"].depends_on,
            vec!["item-1".to_string()],
            "from_sketch 必须应用草图依赖"
        );
        assert!(plan2.detect_cycle().is_none());
    }

    #[test]
    fn parallel_link_proposal_attributes_distinct_surfaces() {
        let contract = TaskContract::from_input("- 列表展示\n- 详情展示\n- 新增表单");
        let json = crate::solve_sketch::SolveSketch::from_contract(&contract).to_json();
        let mut plan = GoalExecution::from_input_with_sketch(&contract, Some(&json));
        // 多面任务：本步内两次非验证动作应归属到不同的已准入面（并发推进、不串面）。
        let mut p1 = ActionProposal {
            signature: "edit:x".into(),
            question: String::new(),
            supports: vec![],
            estimated_cost: 1,
        };
        plan.link_proposal(&mut p1);
        let mut p2 = ActionProposal {
            signature: "edit:y".into(),
            question: String::new(),
            supports: vec![],
            estimated_cost: 1,
        };
        plan.link_proposal(&mut p2);
        assert_ne!(p1.supports, p2.supports, "两次动作应归属不同面");
        assert_eq!(p1.supports.len(), 1);
        assert_eq!(p2.supports.len(), 1);
        // 两个归属面都应在并发准入集合内（写冲突安全）。
        let admitted = plan.admit_concurrent(MAX_PARALLEL_SURFACES);
        assert!(admitted.contains(&p1.supports[0]));
        assert!(admitted.contains(&p2.supports[0]));
    }

    #[test]
    fn parallel_plan_lists_admitted_surfaces() {
        let plan = three_surface_plan();
        let text = plan.parallel_plan();
        assert!(text.contains("并行执行计划"), "应产出并行计划");
        for id in ["item-1", "item-2", "item-3"] {
            assert!(text.contains(id), "并行计划应列出 {id}");
        }
    }

    #[test]
    fn concept_checklist_flags_cross_surface_leak() {
        let mut plan = three_surface_plan();
        // 清掉描述切出的中文 n-gram 噪声，只保留真实标识符 appCode，模拟干净的工作区裁决。
        plan.goal.code_entities.clear();
        plan.goal.candidates.clear();
        // 三个面都引用 appCode；只改了两个，第三个漏了。
        for (id, path) in [
            ("item-1", "a.tsx"),
            ("item-2", "b.tsx"),
            ("item-3", "c.tsx"),
        ] {
            plan.items.get_mut(id).unwrap().candidate_targets.push(path.into());
        }
        plan.goal.code_entities.push("appCode".into());
        plan.items.get_mut("item-1").unwrap().state = WorkItemState::Verified;
        plan.items.get_mut("item-2").unwrap().state = WorkItemState::Verified;
        let checklist = plan.concept_coverage_checklist();
        assert!(checklist.contains("appCode"), "应预警 appCode 跨面漏改");
        assert!(checklist.contains("item-3"), "应点名漏改面 item-3");
    }

    #[test]
    fn concept_coverage_checklist_is_byte_deterministic() {
        // G4 漏改清单必须字节确定：同一状态多次渲染、以及两个独立构造的等价计划，
        // 其 concept_coverage_checklist 输出必须完全一致——否则打包后每次运行提示不同，
        // 体感像"没改进 / 不稳定"。修复前因 HashMap 遍历顺序而漂移。
        fn build() -> GoalExecution {
            let mut plan = three_surface_plan();
            plan.goal.code_entities.clear();
            plan.goal.candidates.clear();
            for (id, path) in [
                ("item-1", "a.tsx"),
                ("item-2", "b.tsx"),
                ("item-3", "c.tsx"),
            ] {
                plan.items.get_mut(id).unwrap().candidate_targets.push(path.into());
            }
            plan.goal.code_entities.push("appCode".into());
            plan.items.get_mut("item-1").unwrap().state = WorkItemState::Verified;
            plan.items.get_mut("item-2").unwrap().state = WorkItemState::Verified;
            plan
        }
        let a = build();
        let b = build();
        let ca1 = a.concept_coverage_checklist();
        let ca2 = a.concept_coverage_checklist();
        assert_eq!(ca1, ca2, "同一计划两次渲染必须字节一致");
        let cb = b.concept_coverage_checklist();
        assert_eq!(
            ca1, cb,
            "两个独立构造的等价计划必须字节一致（不依赖 HashMap 遍历顺序）"
        );
        assert!(ca1.contains("appCode"), "应预警 appCode 跨面漏改");
        assert!(ca1.contains("item-3"), "应点名漏改面 item-3");
    }

    #[test]
    fn render_for_model_exposes_parallel_plan_and_concept_checklist() {
        // 证明 G1/G2/G4 真正进入模型提示：打包后的运行器给模型的求解图包含并行计划
        // 与跨面一致性清单，而不再只有单一"当前工作项"串行视图。
        let contract = TaskContract::from_input("- 列表展示\n- 详情展示\n- 新增表单");
        let json = crate::solve_sketch::SolveSketch::from_contract(&contract).to_json();
        let mut plan = GoalExecution::from_input_with_sketch(&contract, Some(&json));
        plan.goal.code_entities.push("appCode".into());
        for (id, path) in [
            ("item-1", "a.tsx"),
            ("item-2", "b.tsx"),
            ("item-3", "c.tsx"),
        ] {
            plan.items.get_mut(id).unwrap().candidate_targets.push(path.into());
        }
        let rendered = plan.render_for_model();
        assert!(
            rendered.contains("[并行执行计划]"),
            "模型提示必须含并行执行计划（G2/G5）"
        );
        assert!(
            rendered.contains("跨面一致性"),
            "模型提示必须含跨面一致性清单（G4）"
        );
    }

    #[test]
    fn parallel_write_groups_serializes_shared_file_and_splits_independent() {
        let mut plan = three_surface_plan();
        // item-1 与 item-2 改同一文件 → 必须被串行化进同一写冲突组；
        // item-3 改不同文件 → 单独成组。这样并发执行器才会对其开两个并发轮。
        for id in ["item-1", "item-2", "item-3"] {
            plan.items.get_mut(id).unwrap().state = WorkItemState::ReadyToChange;
        }
        plan.items.get_mut("item-1").unwrap().candidate_targets = vec!["shared.tsx".into()];
        plan.items.get_mut("item-2").unwrap().candidate_targets = vec!["shared.tsx".into()];
        plan.items.get_mut("item-3").unwrap().candidate_targets = vec!["other.tsx".into()];
        let groups = plan.parallel_write_groups();
        // 必须多于 1 组，并发分支才会被触发。
        assert!(groups.len() > 1, "独立文件应拆成多组以触发并发执行器");
        // 同文件的两个面必须在同一组。
        let shared_group = groups
            .iter()
            .find(|g| g.len() == 2)
            .expect("应存在含两个面的写冲突组");
        assert!(shared_group.contains(&"item-1".to_string()));
        assert!(shared_group.contains(&"item-2".to_string()));
        // item-3 单独成组。
        assert!(
            groups.iter().any(|g| g == &vec!["item-3".to_string()]),
            "独立文件应单独成组"
        );
    }

    #[test]
    fn render_for_model_scoped_degenerates_for_single_surface_and_focuses_multi() {
        let mut plan = three_surface_plan();
        for id in ["item-1", "item-2", "item-3"] {
            plan.items.get_mut(id).unwrap().state = WorkItemState::ReadyToChange;
            plan.items
                .get_mut(id)
                .unwrap()
                .candidate_targets = vec![format!("{id}.tsx")];
        }
        plan.goal.code_entities.push("appCode".into());
        // 单面作用域：必须退化为全局提示（含全部交付面、并行计划、跨面清单），
        // 不得被截断为仅 scope 内面。concept_coverage_checklist 现已字节确定（见
        // concept_coverage_checklist_is_byte_deterministic），故此处用结构标记断言退化。
        let single = plan.render_for_model_scoped(&["item-2".to_string()]);
        assert!(single.contains("[V4 唯一目标求解图]"), "单面作用域应含全局标题");
        for id in ["item-1", "item-2", "item-3"] {
            assert!(single.contains(id), "单面作用域应仍列出全部交付面 {id}（未截断）");
        }
        assert!(single.contains("[并行执行计划]"), "单面作用域应含并行计划");
        assert!(single.contains("跨面一致性"), "单面作用域应含跨面清单");
        assert!(
            !single.contains("本轮聚焦"),
            "单面作用域不应带聚焦标记"
        );
        // 多面作用域：含聚焦标记、工作项列表只列 scope 内面、仍含并行计划与跨面清单。
        let scoped = plan.render_for_model_scoped(&["item-1".to_string(), "item-2".to_string()]);
        assert!(scoped.contains("本轮聚焦"), "多面作用域应含聚焦标记");
        let block = scoped
            .split("工作项：\n")
            .nth(1)
            .unwrap()
            .split("\n当前工作项")
            .next()
            .unwrap();
        assert!(block.contains("- item-1："), "scope 工作项列表应含 item-1");
        assert!(block.contains("- item-2："), "scope 工作项列表应含 item-2");
        assert!(
            !block.contains("- item-3："),
            "scope 工作项列表不应含 scope 外的 item-3"
        );
        assert!(
            scoped.contains("[并行执行计划]"),
            "作用域提示仍须含并行计划"
        );
        assert!(
            scoped.contains("跨面一致性"),
            "作用域提示仍须含跨面清单（G4 不漏警）"
        );
    }
}
