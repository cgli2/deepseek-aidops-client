use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use base64::Engine as _;
use futures::StreamExt;
use harness_capability::assets::{
    ChatTurn, ConversationMemory, FactKind, LifecycleLayer, MemoryFact, Skill, SkillLibrary,
};
use harness_capability::compaction::Compaction;
use harness_capability::hook::{Hook, HookDecision, HookEvent, HookPayload};
use harness_core::{error::Result, types::UserInput, AppContext};
use harness_llm::{Chunk, LlmProvider, Message, RequestOptions, Role, ToolCall, ToolResult, Usage};
use harness_session::{
    DeliveryOutcome, DeliveryReport, ExecutionTelemetry, SessionEvent, SessionLog,
    WorkItemTelemetry,
};
use harness_tool::ToolRegistry;
use tokio_util::sync::CancellationToken;

use crate::case_file::CaseFile;
use crate::events::{PreStep, TurnStopping};
use crate::execution::{
    ActionGate, ActionProposal, BudgetManager, Completion, CompletionJudge, DomainPolicy,
    ExecutionState, GateDecision, GeneralDomainPolicy, SolvePlan, TaskContract,
};
use crate::goal_execution::{ActionContract, EvidenceKind, GoalCompletion};
use crate::governor::{artifact_text, is_continuation_request, Decision, TurnGovernor};
use crate::{GoalExecution, TaskLedger, WorkspaceGrounder, WorkspaceIndex};

/// 治理路径选择（spec §5 步骤④：控制器接管后 A/B 默认 On）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GovernorMode {
    /// 旧守卫网络拥有终止权（仅逃生门，步骤⑤删除）。
    Legacy,
    /// 终止权收归 TurnGovernor；旧守卫并行运行但只产信号（默认）。
    On,
}

/// 解析 `HARNESS_GOVERNOR`：默认控制器接管（On）；仅显式 legacy/off/0 回退旧路径
/// （步骤⑤删除 Legacy 前保留一个阶段的逃生门）。
pub fn parse_governor_mode(value: Option<&str>) -> GovernorMode {
    match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("legacy" | "off" | "0") => GovernorMode::Legacy,
        _ => GovernorMode::On,
    }
}

/// Agent 循环 / Turn-Step 生命周期（原 §5.6）。
///
/// `Turn` = 0..n `Step`；`debt` 计数控制续跑；`agent/turn-stopping` 为唯一串行终止点。
pub struct AgentLoop {
    governor: GovernorMode,
}

impl Default for AgentLoop {
    fn default() -> Self {
        Self::new()
    }
}

fn goal_executor_enabled() -> bool {
    parse_goal_executor_mode(std::env::var("HARNESS_GOAL_EXECUTOR").ok().as_deref())
}

fn parse_goal_executor_mode(value: Option<&str>) -> bool {
    !matches!(
        value.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        Some("0" | "off" | "false" | "legacy" | "v3")
    )
}

/// 相同调用的结果守卫。成功调用在工作区未发生写入前不应原样重试；失败调用
/// 只允许一次定向重试。结果只保留哈希，避免控制状态复制工具原文。
#[derive(Default)]
struct ToolRepeatGuard {
    previous: Option<(String, u64, bool)>,
    recovery_attempts: HashMap<String, u8>,
    /// 同一调用（名称+参数）的累计执行次数：即使每次输出略有差异（扫描类命令
    /// 常见），超过阈值也判定为低价值重复。取证：同回合扫描脚本被跑 13 次。
    cumulative: HashMap<String, u8>,
}

/// 同回合内同一失败调用（名称+参数完全相同）最多执行两次（首次 + 一次定向重试）。
const MAX_SAME_CALLS_PER_TURN: u8 = 2;

/// 用户的“继续”不是一个新的、只有一句话的 Direct 任务。它必须接回最近一个
/// 未验证结束的根任务，否则复杂迁移会被重新分类为 Direct，并误用 36/48 这类
/// 小任务硬预算。
#[derive(Clone)]
struct ResumeState {
    objective: String,
    report: DeliveryReport,
}

/// 用户指出“刚才只给方案/没有真正修改”时，语义上仍是在推进上一条未完成任务，
/// 不能把这句纠错反馈重新编译成一个没有业务对象的新目标。否则 Grounder 会围绕
/// “修改代码”之类泛词重新定位，最终为了满足写入计数而改到无关文件。
fn is_execution_correction_request(text: &str) -> bool {
    let compact = text.split_whitespace().collect::<String>().to_lowercase();
    [
        "你只是列出来",
        "只给了方案",
        "没有落实",
        "没有真正改",
        "还是要实际改",
        "没有看到你有什么改动",
        "自己执行修改",
        "这个功能未实现",
        "右击文件没有",
        "右键没有",
        "并没有生效",
    ]
    .iter()
    .any(|marker| compact.contains(marker))
}

fn is_resumable_follow_up(text: &str) -> bool {
    is_continuation_request(text) || is_execution_correction_request(text)
}

const CLARIFICATION_REASON_PREFIX: &str = "需要补充执行信息：";

/// Fix2：搜索/扫描类调用的会话级记忆化缓存。键=工具名+归一化参数；命中即返回
/// 缓存结果、不重跑真实工具，消除单窗口重复扫描（取证：同回合扫描被跑 13 次）
/// 与续跑重扫。进程内长驻（同一 harness 会话跨多次“继续”共享），进程退出后失效；
/// 只读搜索命中不重复记证据/写入，避免污染 Fix1 的进展度量与预算计数。
static SEARCH_MEMO: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, ToolResult>>,
> = std::sync::OnceLock::new();

fn search_memo() -> &'static std::sync::Mutex<std::collections::HashMap<String, ToolResult>> {
    SEARCH_MEMO.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// 识别搜索/扫描/定位类工具（易产生重复空转调用）。
fn is_search_like(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("search")
        || n.contains("grep")
        || n == "find"
        || n.contains("where")
        || n.contains("glob")
        || n == "rg"
        || n.contains("locate")
        || n.contains("ack")
}

/// 搜索缓存键：工具名 + 归一化参数（Debug 表示即可，足以区分不同查询）。
fn search_cache_key(name: &str, args: &impl std::fmt::Debug) -> String {
    format!("{}::{:?}", name, args)
}

/// 控制器模式下澄清提问是否被允许（spec §4.2 三重前置）；Legacy 一律允许，
/// 使 A/B 两条路径的旧行为逐字一致。
fn ask_user_permitted(
    governor: Option<&TurnGovernor>,
    case: &CaseFile,
    input_text: &str,
    question: &str,
) -> bool {
    match governor {
        None => true,
        Some(gov) => gov.ask_user_allowed(case, input_text, question),
    }
}

/// 给问题补上工作区派生的候选列表（R2 硬前置）。无可派生候选时原样返回，
/// 由 `ask_user_permitted` 按「开放模板」拒绝。
fn with_candidates(question: &str, goal: &crate::GoalContract) -> String {
    let mut candidates: Vec<String> = goal.entities.clone();
    candidates.extend(goal.navigation.iter().cloned());
    candidates.sort();
    candidates.dedup();
    if candidates.is_empty() {
        return question.to_string();
    }
    format!("{}（候选：{}）", question, candidates.join("、"))
}

/// 从追加日志逆向找到最近的未完成根任务。若最近一回合本身也是“继续”，继续
/// 向前穿透，直到命中真实目标；这使连续多次续跑始终继承同一契约与策略。
fn latest_resumable_task(events: &[SessionEvent]) -> Option<ResumeState> {
    let mut end = events.len();
    // 目标文本要穿透“继续”找到根任务，但验收状态必须采用最近一次 Delivery；
    // 新版续跑回合的 TurnStart 仍记录用户原话，而其 Delivery 已是根任务的报告。
    let mut latest_report: Option<DeliveryReport> = None;
    loop {
        let delivery_index = (0..end).rev().find(|&index| {
            matches!(
                events[index],
                SessionEvent::Delivery {
                    report: DeliveryReport {
                        outcome: DeliveryOutcome::NeedsUserInput
                            | DeliveryOutcome::PartialDelivery
                            | DeliveryOutcome::SystemFailure
                            | DeliveryOutcome::Blocked
                            | DeliveryOutcome::Interrupted,
                        ..
                    },
                    ..
                }
            )
        })?;
        let report = match &events[delivery_index] {
            SessionEvent::Delivery { report, .. } => report.clone(),
            _ => unreachable!("delivery index was matched above"),
        };
        let turn_index = (0..delivery_index)
            .rev()
            .find(|&index| matches!(events[index], SessionEvent::TurnStart { .. }))?;
        let objective = match &events[turn_index] {
            SessionEvent::TurnStart { input, .. } => input.clone(),
            _ => unreachable!("turn index was matched above"),
        };
        if !is_resumable_follow_up(&objective) {
            return Some(ResumeState {
                objective,
                report: latest_report.unwrap_or(report),
            });
        }
        latest_report.get_or_insert(report);
        end = turn_index;
    }
}

/// 上一回合主动澄清时，用户的下一条自然语言通常就是补充答案而不是“继续”。
/// 将其和根请求合并重新编译，避免丢失原始意图或把答案误当全新任务。
fn awaiting_clarification(events: &[SessionEvent]) -> bool {
    events
        .iter()
        .rev()
        .find_map(|event| match event {
            SessionEvent::Delivery { report, .. } => Some(
                report
                    .reason
                    .as_deref()
                    .is_some_and(|reason| reason.starts_with(CLARIFICATION_REASON_PREFIX)),
            ),
            _ => None,
        })
        .unwrap_or(false)
}

/// 取回最近一条助手消息。用于判断"同一个澄清问题是否已经问过"。
fn last_assistant_text(events: &[SessionEvent]) -> Option<String> {
    events.iter().rev().find_map(|event| match event {
        SessionEvent::Assistant { chunk, .. } => chunk.text.clone(),
        _ => None,
    })
}

fn resume_instruction(resume: &ResumeState) -> String {
    let completed = resume
        .report
        .criteria
        .iter()
        .filter(|criterion| criterion.satisfied)
        .map(|criterion| criterion.id.as_str())
        .collect::<Vec<_>>();
    let remaining = resume
        .report
        .criteria
        .iter()
        .filter(|criterion| !criterion.satisfied)
        .map(|criterion| format!("{}={}", criterion.id, criterion.description))
        .collect::<Vec<_>>();
    format!(
        "[续跑任务]\n这是同一任务的后续执行，不要重新创建计划或按本句重新分类。\n原始目标：{}\n已验证：{}\n未完成：{}\n上次停止原因：{}\n从第一个未完成验收项继续；保留已验证项，不重复已完成的定位、修改或验证。",
        resume.objective,
        if completed.is_empty() { "无".into() } else { completed.join("、") },
        if remaining.is_empty() { "无".into() } else { remaining.join("；") },
        resume.report.reason.as_deref().unwrap_or("未提供")
    )
}

/// 单次模型响应内的受控定位门禁。它补足跨步骤状态机的时间差：首个 search 的
/// 结果尚未写入执行状态时，原子、诊断和范围受限任务都不能并行发起更多定位搜索。
/// 零先验时允许多少个定位搜索同时在飞。串行只允许一个会让模型把步数预算
/// 线性消耗在"换关键词重试"上，而这正是零先验场景唯一不该省的地方。
const ZERO_PRIOR_SEARCH_PARALLELISM: usize = 3;

struct LocateStepGate {
    search_queued: usize,
    max_parallel: usize,
}

impl Default for LocateStepGate {
    fn default() -> Self {
        Self {
            search_queued: 0,
            max_parallel: 1,
        }
    }
}

impl LocateStepGate {
    fn with_parallelism(max_parallel: usize) -> Self {
        Self {
            search_queued: 0,
            max_parallel: max_parallel.max(1),
        }
    }

    fn allows(&mut self, controlled: bool, signature: &str) -> bool {
        if !controlled || !signature.starts_with("search:") {
            return true;
        }
        if self.search_queued >= self.max_parallel {
            return false;
        }
        self.search_queued += 1;
        true
    }
}

impl ToolRepeatGuard {
    fn should_block(&self, signature: &str) -> bool {
        let repeated_success = self
            .previous
            .as_ref()
            .is_some_and(|(previous, _, ok)| previous == signature && *ok);
        let failed_retries_exhausted = self
            .cumulative
            .get(signature)
            .is_some_and(|count| *count >= MAX_SAME_CALLS_PER_TURN);
        repeated_success || failed_retries_exhausted
    }

    fn record_result(&mut self, signature: &str, result: &ToolResult) {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        result.ok.hash(&mut hasher);
        result.content.hash(&mut hasher);
        let fingerprint = hasher.finish();
        // 成功写入改变了观察对象：之后允许重新运行同一验证命令，但仍不会允许
        // 紧随其后的原样重复。其它成功读取/搜索不改变对象，必须换参数或换路径。
        let changed_workspace = result.ok
            && (signature.starts_with("edit:")
                || (signature.starts_with("fs:") && signature.contains("\"op\":\"write\"")));
        if changed_workspace {
            self.cumulative.clear();
            self.recovery_attempts.clear();
            self.previous = None;
        }
        let changed_path =
            self.previous
                .as_ref()
                .is_none_or(|(previous_signature, previous_fingerprint, _)| {
                    previous_signature != signature || *previous_fingerprint != fingerprint
                });
        if changed_path {
            // 一次真正不同的观察意味着模型已改变调查路径；旧的恢复次数不再相关。
            self.recovery_attempts.clear();
        }
        self.previous = Some((signature.to_string(), fingerprint, result.ok));
        // 累计计数：与「连续相同结果」互补，拦截输出有微小差异的空转重复。
        *self.cumulative.entry(signature.to_string()).or_default() = self
            .cumulative
            .get(signature)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
    }

    fn note_recovery(&mut self, signature: &str) -> u8 {
        let attempts = self
            .recovery_attempts
            .entry(signature.to_string())
            .or_default();
        *attempts = attempts.saturating_add(1);
        *attempts
    }
}

/// 默认确定性压缩器：复用会话重建、工具输出压缩与上下文预算规则。
/// 作为能力注册后，后续可替换为远程摘要器；默认实现绝不额外请求模型。
#[derive(Default)]
pub struct DeterministicCompaction;

#[async_trait::async_trait]
impl Compaction for DeterministicCompaction {
    async fn compact(&self, events: Vec<SessionEvent>) -> Result<Vec<Message>> {
        Ok(messages_from_events(&events))
    }
}

impl AgentLoop {
    pub fn new() -> Self {
        Self {
            governor: parse_governor_mode(std::env::var("HARNESS_GOVERNOR").ok().as_deref()),
        }
    }

    /// 显式指定治理路径：回放套件与实机 A/B 对照用它绕开进程环境变量。
    pub fn with_governor(mut self, mode: GovernorMode) -> Self {
        self.governor = mode;
        self
    }

    pub fn governor_mode(&self) -> GovernorMode {
        self.governor
    }

    /// 跑一个 turn，直到唯一终止检查点返回 `will_stop`。
    pub async fn run_turn(&self, ctx: &AppContext, input: UserInput) -> Result<()> {
        self.run_turn_cancellable(ctx, input, CancellationToken::new(), None)
            .await
    }

    /// 测试/脚本化入口：直接注入一个已构造的 `GoalExecution`（如多面 `ReadyToChange`
    /// 任务），跳过内部从 `UserInput` 现建，用于驱动并发执行器分支的端到端验证。
    pub async fn run_turn_with_goal_execution(
        &self,
        ctx: &AppContext,
        input: UserInput,
        goal: GoalExecution,
    ) -> Result<()> {
        self.run_turn_cancellable(ctx, input, CancellationToken::new(), Some(goal))
            .await
    }

    pub async fn run_turn_cancellable(
        &self,
        ctx: &AppContext,
        input: UserInput,
        cancellation: CancellationToken,
        injected_goal: Option<GoalExecution>,
    ) -> Result<()> {
        let log = ctx.get::<SessionLog>();
        let llm = ctx.get::<dyn LlmProvider>();
        let tools = ctx.get::<ToolRegistry>();
        let hook = ctx.get::<dyn Hook>();
        let bus = ctx.events();

        // 仅附文件/图片也必须形成完整任务，不能把空 user message 交给上游模型；
        // 后者常被网关当作无效输入或返回空输出，表现为会话窗口毫无响应。
        let input_text = attachment_prompt(&input);
        // 会话正文可以重放，但任务的策略/验收状态不能从一句“继续”重新推断。
        // 只在明确续跑表达式下恢复，普通新请求仍完全按其自身目标执行。
        let history = log.replay();
        let is_follow_up = is_resumable_follow_up(&input_text);
        let is_clarification_reply = !is_follow_up && awaiting_clarification(&history);
        let resume = (is_follow_up || is_clarification_reply)
            .then(|| latest_resumable_task(&history))
            .flatten();
        let task_text = match (resume.as_ref(), is_clarification_reply) {
            (Some(state), true) => {
                format!("原始需求：{}\n用户补充：{}", state.objective, input_text)
            }
            (Some(state), false) => state.objective.clone(),
            (None, _) => input_text.clone(),
        };

        // 先把自然语言请求编译成通用执行契约，再由可替换的领域策略选择执行方式。
        // 没有注册领域策略时使用通用分类器，不把代码修复等场景写死在 Agent Loop。
        let contract = TaskContract::from_input(&task_text);
        let default_policy = GeneralDomainPolicy;
        let strategy = ctx
            .try_get::<dyn DomainPolicy>()
            .map(|policy| policy.select_strategy(&contract))
            .unwrap_or_else(|| default_policy.select_strategy(&contract));
        let mut budget = BudgetManager::for_contract(&contract, strategy);
        if let Some(policy) = ctx.try_get::<dyn DomainPolicy>() {
            policy.adjust_budget(&contract, &mut budget);
        }
        // 现有 UI/env 步数设置继续作为管理员硬上限；动态预算只会进一步收紧。
        BudgetManager::cap_initial_step_window(&mut budget, max_steps_limit());
        let solve_plan = SolvePlan::for_contract(&contract, strategy);
        BudgetManager::cap_initial_step_window(&mut budget, solve_plan.initial_steps);
        BudgetManager::cap_initial_tool_window(&mut budget, solve_plan.initial_tool_calls);
        // 硬预算是总成本保险，而非“到点后再扩一轮”的软提示。任何任务都不能
        // 借自动续跑突破上限；总额内如已有写入，模型仍可用剩余调用完成验证。
        BudgetManager::cap_hard_limits(
            &mut budget,
            solve_plan.hard_max_steps,
            solve_plan.hard_max_tool_calls,
        );
        let mut execution = ExecutionState::new(contract, strategy);
        if let Some(state) = &resume {
            execution.restore_verified_criteria(&state.report);
        }
        let mut ledger = resume
            .as_ref()
            .map(|state| TaskLedger::from_delivery(&execution.contract, &state.report))
            .unwrap_or_else(|| TaskLedger::from_contract(&execution.contract));
        // S5/G1：不再传 None。用**确定性本地计划器**生成草图并回灌同一套 schema 校验，
        // 使 G1 的 `from_sketch` 路径在运行时真正运行（LLM 可用时其 JSON 会覆盖本地草图）。
        // 回灌仍经校验，失败即回落 `from_contract`，可用性零风险（D1）。
        let local_sketch_json =
            crate::solve_sketch::SolveSketch::from_contract(&execution.contract).to_json();
        let mut goal_execution = match injected_goal {
            // 注入路径（并发执行器 e2e 等）：直接使用调用方构造的执行体，
            // 不再从草图现建——调用方已保证与 contract 一致。
            Some(mut g) => {
                g.confirm_injected_targets();
                g
            }
            // 默认路径：本地确定性草图回灌同一套 schema 校验门（LLM 可用时其 JSON 覆盖）。
            None => {
                let mut ge = GoalExecution::from_input_with_sketch(
                    &execution.contract,
                    Some(&local_sketch_json),
                );
                // S5/G2 护栏：含环的草图（若有）回落静态模板，绝不推进含环 DAG。
                if ge.detect_cycle().is_some() {
                    ge = GoalExecution::from_contract(&execution.contract);
                }
                ge
            }
        };
        // S3 分面预算供给：交付面已经切分出来了，此时才知道"这个任务实际有几个面"。
        // 求解计划的硬熔断常量对面数是次线性且被截断的，多面任务的尾部面在算术上
        // 拿不到跑完四个相位的步数（V5 §2.2）。这里按 Σ 每面独立预算抬升硬熔断，
        // 让总额与面数恢复线性；抬升仍受 execution.rs 的绝对天花板约束。
        let surface_demand = goal_execution.required_budget();
        // Fix3：按求解草图估算抬升硬熔断，不再仅按“面数>1”抬升。单面但草图估算明显
        // 超固定硬预算的任务同样抬升，使需更多步的任务上限随量放大、1~2 窗口收敛，
        // 而非被固定 36/48 封顶后反复触发硬熔断。多面任务取并发需求（预算守恒，不乘
        // 并行度）；抬升仍受 execution.rs 的 ABSOLUTE_MAX_* 上限约束。
        let demand = if goal_execution.active_surfaces().len() > 1 {
            goal_execution.required_budget_parallel()
        } else {
            surface_demand
        };
        BudgetManager::provision_hard_limits(&mut budget, demand.steps, demand.tool_calls);
        let intent = crate::IntentProfile::compile(&task_text);

        // L2 裁决：**先于澄清门禁执行**。
        //
        // 旧流程把工作区扫描放在澄清之后，理由是"模糊请求不必白白读几百个文件"。
        // 但 V5 里工作区不再是可选优化，而是唯一的裁判——"用户说的东西工作区里
        // 有没有"恰恰是决定该不该问的关键信息。靠词表去猜而跳过这一步，正是
        // "需求已经很明确、agent 却反复追问"的根源。
        let workspace_root = ctx
            .try_get::<harness_core::Workspace>()
            .map(|workspace| workspace.root());
        // 精确旧值不需要先为最多 320 个源码文件构建候选词索引。直接执行字面量
        // 快速定位；只有没有旧值可搜的开放描述才支付 L1/L2 工作区裁决成本。
        let exact_literal_grounding = workspace_root
            .as_ref()
            .and_then(|root| WorkspaceGrounder::ground_exact_literal(root, &goal_execution.goal));
        let workspace_index = (exact_literal_grounding.is_none()
            && (!goal_execution.goal.candidates.is_empty()
                || !goal_execution.goal.code_entities.is_empty()
                || goal_execution.goal.transformation.is_some()))
        .then(|| {
            workspace_root
                .as_ref()
                .map(|root| WorkspaceIndex::load_or_build(root))
        })
        .flatten();
        if let Some(index) = &workspace_index {
            goal_execution.goal.resolve_against(index);
        }

        // 步骤②/④：Case File 是 SessionLog 的只读投影（不构成第二事实源）。
        // grounded 判据与 :508 索引构建条件同源，不引入新的猜测逻辑。
        // 只有控制器模式才建 TurnGovernor；Legacy 下 governor 恒为 None，
        // 三处门禁的 ask_user_permitted 直接返回 true，旧行为逐字保持。
        let case_file = CaseFile::from_replay(&history);
        let read_only = matches!(intent.kind, crate::IntentKind::Investigation);
        let grounded = exact_literal_grounding
            .as_ref()
            .is_some_and(|grounding| !grounding.literal_hits.is_empty())
            || (workspace_index.is_some() && goal_execution.goal.has_locatable_signal());
        let mut governor = (self.governor == GovernorMode::On)
            .then(|| TurnGovernor::new(&case_file, grounded, read_only));

        if let Some(clar) = crate::IntentProfile::requires_clarification(
            &goal_execution.goal,
            intent.is_task,
            &input_text,
        ) {
            // Phase 1 信号驱动门禁：已落地或纯提问都不会到这里；能到这里说明是真·盲任务，
            // 且 `clar` 已经是单个带上下文的定位问题（不发清单、不靠词表猜用户措辞）。
            let question = with_candidates(&clar.question, &goal_execution.goal);
            // 重复澄清熔断：用户已经回答过一次、而重新编译后问的还是同一个问题，
            // 说明他没有这个维度的信息（或认为原描述已足够）。继续追问只会制造
            // 死循环——此时带着已有信息直接执行，比再问一遍更有用。
            let repeated = is_clarification_reply
                && last_assistant_text(&history).as_deref() == Some(question.as_str());
            let permitted =
                ask_user_permitted(governor.as_ref(), &case_file, &input_text, &question);
            if !repeated && permitted {
                // 澄清不是失败后的补救，而是执行前门禁：不请求模型、不暴露工具、不开始
                // 目录搜索。问题被补全后，下一条用户消息会与本轮根请求合并再编译。
                log.append(SessionEvent::TurnStart {
                    id: log.gen_id(),
                    input: input_text,
                });
                log.append(SessionEvent::Assistant {
                    id: log.gen_id(),
                    chunk: Chunk {
                        text: Some(question),
                        ..Default::default()
                    },
                });
                let reason = format!(
                    "{CLARIFICATION_REASON_PREFIX}任务缺少可执行的目标、症状或验收标准，尚未调用模型或工具"
                );
                ledger.block("user-objective", reason.clone());
                log.append(SessionEvent::Delivery {
                    id: log.gen_id(),
                    report: execution
                        .delivery_report(DeliveryOutcome::NeedsUserInput, Some(reason)),
                });
                append_telemetry(
                    &log,
                    &execution,
                    &goal_execution,
                    &ledger,
                    "等待用户补充任务信息，未开始执行",
                );
                log.append(SessionEvent::TurnEnd { id: log.gen_id() });
                return Ok(());
            }
        }

        // 复用上面已经建好的索引做扫描，避免把工作区读第二遍。
        let workspace_grounding = exact_literal_grounding.or_else(|| {
            workspace_index
                .as_ref()
                .map(|index| WorkspaceGrounder::ground_with(index, &goal_execution.goal))
        });
        if let Some(grounding) = &workspace_grounding {
            goal_execution.apply_grounding(grounding);
        }

        // 方案B·Phase 2：已落地的任务，用运行期观察（静态核对已定位文件是否已含期望终态）
        // 取代关键词猜异常。若观察与目标不一致且可推断，提出单个带上下文的澄清问题，
        // 而不是盲目搜索。无落地信号时不在此处理（已由 Phase 1 门禁问定位问题）。
        if goal_execution.goal.has_locatable_signal() {
            if let Some(root) = &workspace_root {
                if let Some(clar) = goal_execution.inspect_for_clarification(root) {
                    let question = with_candidates(&clar.question, &goal_execution.goal);
                    if ask_user_permitted(governor.as_ref(), &case_file, &input_text, &question) {
                        let item_id = ledger
                            .current_item()
                            .map(|item| item.id.clone())
                            .unwrap_or_else(|| "user-objective".to_owned());
                        let reason = format!("{CLARIFICATION_REASON_PREFIX}{question}");
                        ledger.block(&item_id, reason.clone());
                        log.append(SessionEvent::TurnStart {
                            id: log.gen_id(),
                            input: input_text,
                        });
                        log.append(SessionEvent::Assistant {
                            id: log.gen_id(),
                            chunk: Chunk {
                                text: Some(format!("[需要澄清] {question}")),
                                ..Default::default()
                            },
                        });
                        log.append(SessionEvent::Delivery {
                            id: log.gen_id(),
                            report: execution
                                .delivery_report(DeliveryOutcome::NeedsUserInput, Some(reason)),
                        });
                        append_telemetry(
                            &log,
                            &execution,
                            &goal_execution,
                            &ledger,
                            "Inspect 观察与期望终态不一致，等待用户确认意图",
                        );
                        log.append(SessionEvent::TurnEnd { id: log.gen_id() });
                        return Ok(());
                    }
                }
            }
        }

        // 把本轮裁决学到的命中统计写回 `.harness/learned.json`，供后续回合复用。
        if let (Some(index), Some(root)) = (&workspace_index, &workspace_root) {
            let _ = index.save(root);
        }

        // 仅将含有明确代码实体（如 appCode）或明确旧值的请求作为硬确认门禁。
        // 普通自然语言问题仍进入精准定位流程，避免把“未命中名称”误判为无法执行；
        // 但用户逐字给出的旧值完整扫描仍缺席时，再搜索同义词只会偏离目标。
        if let Some(grounding) = &workspace_grounding {
            let has_exact_old_value = goal_execution
                .goal
                .transformation
                .as_ref()
                .and_then(|value| value.from_value.as_deref())
                .is_some();
            if (!goal_execution.goal.code_entities.is_empty() || has_exact_old_value)
                && grounding.needs_user_input()
            {
                let question = with_candidates(
                    &grounding.user_question(&goal_execution.goal),
                    &goal_execution.goal,
                );
                if ask_user_permitted(governor.as_ref(), &case_file, &input_text, &question) {
                    let item_id = ledger
                        .current_item()
                        .map(|item| item.id.clone())
                        .unwrap_or_else(|| "user-objective".to_owned());
                    let reason = format!("{CLARIFICATION_REASON_PREFIX}{question}");
                    ledger.block(&item_id, reason.clone());
                    log.append(SessionEvent::TurnStart {
                        id: log.gen_id(),
                        input: input_text,
                    });
                    log.append(SessionEvent::Assistant {
                        id: log.gen_id(),
                        chunk: Chunk {
                            text: Some(format!("[需要澄清] {question}")),
                            ..Default::default()
                        },
                    });
                    log.append(SessionEvent::Delivery {
                        id: log.gen_id(),
                        report: execution
                            .delivery_report(DeliveryOutcome::NeedsUserInput, Some(reason)),
                    });
                    append_telemetry(
                        &log,
                        &execution,
                        &goal_execution,
                        &ledger,
                        "工作区与目标实体不匹配，等待用户确认",
                    );
                    log.append(SessionEvent::TurnEnd { id: log.gen_id() });
                    return Ok(());
                }
            }
        }

        // 从追加日志重建多轮上下文；不能每个 turn 都只发送当前一句，否则 GUI 看似能聊天，
        // 实际模型完全不记得上一轮以及之前的工具结果。
        // 记忆自动沉淀（L0 工作记忆）：每个用户回合写入对话记忆（无后端则落本地文件）。
        // 失败容忍——记忆写入不影响对话流程；放后台任务，不阻塞首帧。
        if let Some(conv) = ctx.try_get::<dyn ConversationMemory>() {
            let turn = ChatTurn {
                session_id: log.id().to_string(),
                role: "user".into(),
                content: input_text.clone(),
                ts: String::new(),
            };
            tokio::spawn(async move {
                let _ = conv.record_turn(turn).await;
            });
        }
        // 预处理并行化：历史压缩与技能匹配互相独立，join 执行；此前串行排在
        // 第一次 llm.stream() 之前，每一项都直接叠加进首 token 延迟。
        let compaction = ctx.try_get::<dyn Compaction>();
        let skill_library = ctx.try_get::<dyn SkillLibrary>();
        let conversation_memory = ctx.try_get::<dyn ConversationMemory>();
        let experience_workspace = ctx
            .try_get::<harness_core::Workspace>()
            .map(|ws| ws.root().display().to_string());
        let (compacted, matched_skills, recalled_experience) = tokio::join!(
            async {
                match compaction {
                    Some(compaction) => compaction.compact(history.clone()).await.ok(),
                    None => None,
                }
            },
            async {
                match skill_library {
                    Some(skills) => skills.match_skills(&input_text).await.ok(),
                    None => None,
                }
            },
            async {
                match conversation_memory {
                    Some(memory) => memory
                        .recall(&input_text, LifecycleLayer::L2)
                        .await
                        .ok()
                        .map(|facts| {
                            facts
                                .into_iter()
                                .filter(|fact| {
                                    experience_workspace.as_ref().is_none_or(|workspace| {
                                        !fact.source.contains("workspace=")
                                            || fact
                                                .source
                                                .contains(&format!("workspace={workspace}"))
                                    })
                                })
                                .collect::<Vec<_>>()
                        }),
                    None => None,
                }
            }
        );
        // 压缩器不可用不能阻断对话；保留旧路径作为可靠回退。
        let mut messages = compacted.unwrap_or_else(|| messages_from_events(&history));

        log.append(SessionEvent::TurnStart {
            id: log.gen_id(),
            input: input_text.clone(),
        });
        // 在网络握手、排队或模型首个分片到来前立即驱动 UI 的思考气泡，避免主界面
        // 留白而被误认为卡死。Thinking 事件不会被写入下一轮模型上下文，也不消耗 token。
        log.append(SessionEvent::Thinking {
            id: log.gen_id(),
            text: "正在理解你的问题…".into(),
        });
        append_telemetry(
            &log,
            &execution,
            &goal_execution,
            &ledger,
            "任务已编译，等待首次定位",
        );

        let mut debt: usize = 1;
        // 跨步累积本轮助手最终文本，供回合结束时沉淀为 L0 记忆。
        let mut last_assistant = String::new();
        let (image_data_urls, image_notes) =
            inline_image_data_urls(&input.attachments, llm.supports_vision());
        let attachment_context = render_attachment_context(&input.attachments, &image_notes);
        messages.push(Message::user_with_images(&input_text, image_data_urls));
        if !attachment_context.is_empty() {
            messages.insert(1, Message::system(&attachment_context));
        }
        messages.insert(
            1,
            Message::system(
                execution
                    .contract
                    .render_for_model(execution.strategy, &budget),
            ),
        );
        messages.insert(1, Message::system(solve_plan.instructions.clone()));
        // 捕获目标提示文本作为后续并发执行器作用域替换的锚点（内容匹配，与插入位置无关）。
        let goal_prompt = goal_execution.render_for_model();
        messages.insert(1, Message::system(goal_prompt.clone()));
        if let Some(grounding) = &workspace_grounding {
            messages.insert(1, Message::system(grounding.render_for_model()));
        }
        if let Some(state) = &resume {
            messages.insert(1, Message::system(resume_instruction(state)));
        }
        // 技能注入点：只匹配启用的 SKILL.md 资产，并在本回合的系统上下文中
        // 提供可执行步骤与验收条件。禁用或删除后，SkillLibrary 不会返回它们，
        // 因而从下一回合起立即不再影响模型行为。匹配已在预处理阶段与压缩并行完成。
        if let Some(matched) = &matched_skills {
            if let Some(instructions) = render_skill_instructions(matched) {
                messages.insert(1, Message::system(&instructions));
            }
        }
        if let Some(facts) = recalled_experience.as_deref() {
            if let Some(instructions) = render_experience(facts) {
                messages.insert(1, Message::system(instructions));
            }
        }
        // 项目事实注入：manifest 位置/工具链/打包入口等稳定环境信息一次告知，
        // 避免模型每回合花十几次调用重新探索构建环境（取证：单回合 15 次 shell
        // 花在发现 cargo 根与 GNU→MSVC 工具链切换上）。
        if let Some(workspace) = ctx.try_get::<harness_core::Workspace>() {
            let facts = crate::facts::project_facts(&workspace.root());
            if !facts.is_empty() {
                messages.insert(1, Message::system(&facts));
            }
        }
        // 工具超时一次读取：此前每个工具调用都重新 std::env::var。
        let tool_timeout_secs: u64 = std::env::var("HARNESS_TOOL_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(300)
            .clamp(5, 3_600);
        let mut steps = 0usize;
        // 控制器模式专属状态。Legacy 下 governor 为 None，这些变量恒为初值且不被读。
        // R3 是单次执行回合的安全边界，不是会话的永久熔断器。历史 Usage 仍完整保留在
        // CaseFile 用于成本审计，但新用户回合（尤其明确的“继续”）必须获得新的执行预算；
        // 否则会话首次触顶后，后续每个回合都会在发出首个请求前重复触顶，永远无法恢复。
        // 首请求尚无本回合实际用量可作下界，发出后再以真实 Usage 驱动后续前置判顶。
        let mut turn_prompt_tokens = 0u64;
        let mut last_prompt_tokens = 0u64;
        let mut session_case = case_file.clone();
        let mut case_cursor = history.len();
        // 只有“同一调用连续得到相同结果”才被视为停滞；先要求模型换路，不立即终止。
        const MAX_LOOP_RECOVERY_PROMPTS: u8 = 2;
        let mut repeat_guard = ToolRepeatGuard::default();
        // 硬终止标记（取消/流错误/反复无视循环恢复）：阻止步末的 debt 记账复活回合，
        // 否则带着「已宣告未执行」的 tool_call 续跑会直接 400。
        let mut hard_stop = false;
        // provider 流错误文本不是模型回答：回合收口必须按系统失败处理，
        // 否则错误串会被当成最终交付（2026-09-01 实机冒烟抓出的假绿 Verified）。
        let mut provider_error_seen = false;
        let mut provider_error_summary = String::new();
        let mut cancelled = false;
        let mut delivery_verified = false;
        // 工具错误事实来自结构化 ToolResult，绝不允许模型凭空把普通补丁冲突、
        // 阶段门禁或“根本没调用写工具”改写成沙箱/权限拒绝。
        let mut sandbox_denial_observed = case_file.tried.iter().any(|entry| {
            entry.summary.contains("[sandbox denied]")
                || entry.summary.contains("sandbox policy denied")
        });
        let mut access_denial_observed = case_file
            .tried
            .iter()
            .any(|entry| entry.summary.contains("[access-policy denied]"));
        let mut claim_correction_notified = false;
        let mut budget_exhausted = false;
        let mut absolute_budget_hit = false;
        // Fix1：断点后进展基线。硬熔断时比较“写入+证据”与基线的增量，有进展才自动续跑。
        let mut hard_baseline = execution.write_operations + execution.evidence.len();
        // prompt 安全边界同样是“执行窗口”而不是人工交互边界。窗口内出现可验证进展时，
        // 先压缩为最小断点再自动续期；连续无进展或续期封顶才真正暂停并交回用户。
        // 这避免大上下文任务每 3~4 次模型往返就要求用户手工输入“继续”。
        let mut prompt_baseline = hard_baseline;
        let mut prompt_autorenews = 0u32;
        let mut convergence_notified = false;
        let mut goal_correction_notified = false;
        // 预算续期耗尽后只给一次最终收尾窗口（2 步）；窗口也用尽则强制停止。
        let mut final_window_armed = false;
        // 上游可能正常结束却没有正文/工具调用（例如网关截断、reasoning-only 帧）。
        // 这不是完成；允许有限恢复重试，避免把占位文本污染会话上下文。
        // 首次空响应后改用最小检查点重试一次。没有备用 Provider 可切换时，继续把
        // 同一目标请求第三遍只会制造截图中的“连续 3 次空响应”伪熔断。
        const MAX_EMPTY_RESPONSE_RETRIES: usize = 1;

        /// Fix1：硬熔断自动续跑硬上限，超过则强制交回用户，防止失控。
        const MAX_HARD_AUTORENEWS: u32 = 8;
        /// 单个用户请求内的 prompt 窗口续期上限；每次续期仍受 300k 窗口边界约束。
        const MAX_PROMPT_AUTORENEWS: u32 = 4;
        let mut empty_response_retries = 0usize;
        let mut empty_recovery_pending = false;
        let controlled_delivery_turn = goal_executor_enabled()
            && execution.solve_mode != crate::execution::SolveMode::OpenEnded;
        while debt > 0 {
            // R3 前置：本执行回合到顶后暂停，不再向模型发请求。下一条用户消息会新建
            // 回合预算，并通过 resume 断点继续；历史成本只做审计，不会把会话永久锁死。
            // 放在 steps 自增与 StepStart 之前，避免留下"开了步却没请求"的空步骤。
            if let Some(gov) = governor.as_ref() {
                if gov.should_stop_before_request(turn_prompt_tokens, last_prompt_tokens) {
                    let progress_now = execution.write_operations + execution.evidence.len();
                    let progress_since_window = progress_now.saturating_sub(prompt_baseline);
                    if prompt_autorenews < MAX_PROMPT_AUTORENEWS
                        && (progress_since_window > 0
                            || (controlled_delivery_turn && goal_execution.can_auto_advance()))
                    {
                        prompt_autorenews += 1;
                        prompt_baseline = progress_now;
                        turn_prompt_tokens = 0;
                        last_prompt_tokens = 0;
                        messages = compact_for_prompt_renewal(
                            messages,
                            &execution.compact_checkpoint(),
                            prompt_autorenews,
                            MAX_PROMPT_AUTORENEWS,
                        );
                        log.append(SessionEvent::Thinking {
                            id: log.gen_id(),
                            text: format!(
                                "执行上下文已压缩，预算窗口自动续期 {prompt_autorenews}/{MAX_PROMPT_AUTORENEWS}…"
                            ),
                        });
                        continue;
                    }
                    budget_exhausted = true;
                    log.append(SessionEvent::Thinking {
                        id: log.gen_id(),
                        text: format!(
                            "执行预算已到安全边界（prompt tokens {} + 预计增量 {} ≥ {}），当前回合停止；不要求用户输入“继续”。",
                            turn_prompt_tokens,
                            last_prompt_tokens,
                            crate::governor::PROMPT_CAP
                        ),
                    });
                    break;
                }
            }
            steps += 1;
            execution.steps = steps;
            debt -= 1;
            if BudgetManager::hard_exhausted(&execution, &budget) {
                // Fix1：硬熔断不再一律打断等用户。若本窗口产生了可验证进展
                // （写入或新证据），自动发放新探索窗口继续推进，避免把任务切碎成
                // 十几次人工“继续”。连续无进展或达到自动续跑上限才交回用户。
                let progress_since_window = (execution.write_operations + execution.evidence.len())
                    .saturating_sub(hard_baseline);
                let deterministic_retry = progress_since_window == 0
                    && budget.hard_autorenews == 0
                    && controlled_delivery_turn
                    && goal_execution.can_auto_advance();
                if !cancelled
                    && budget.hard_autorenews < MAX_HARD_AUTORENEWS
                    && (progress_since_window > 0 || deterministic_retry)
                {
                    BudgetManager::arm_hard_continuation(&mut budget);
                    budget.hard_autorenews += 1;
                    hard_baseline = execution.write_operations + execution.evidence.len();
                    messages.push(Message::user(&format!(
                        "[自动续跑·第{}次] 本窗口新增 {} 项可验证进展；下一步仍可直接执行，已自动发放新窗口，无需人工“继续”。围绕未满足的验收条件继续推进。",
                        budget.hard_autorenews, progress_since_window
                    )));
                    continue;
                }
                absolute_budget_hit = true;
                hard_stop = true;
                let terminal_reason =
                    goal_execution
                        .actionable_terminal_reason()
                        .unwrap_or_else(|| {
                            if goal_execution.can_auto_advance() {
                                "明确的下一步在安全执行窗口内仍未成功完成".into()
                            } else {
                                "运行时未获得足以继续定位的目标证据".into()
                            }
                        });
                log.append(SessionEvent::Thinking {
                    id: log.gen_id(),
                    text: format!(
                        "已到安全执行上限（{} 步 / {} 次工具调用）：{}",
                        budget.hard_max_steps, budget.hard_max_tool_calls, terminal_reason
                    ),
                });
                break;
            }
            log.append(SessionEvent::StepStart {
                id: log.gen_id(),
                step: steps,
            });
            append_telemetry(
                &log,
                &execution,
                &goal_execution,
                &ledger,
                "请求模型执行当前阶段",
            );

            // 瀑布前处理：可重写/拒绝消息；空链返回输入本身（终态恒等）。
            // 链从事件总线注册表收集：插件可经 `on_waterfall::<PreStep>` 注入
            // around-middleware（如 Trellis 的 spec 注入 / 任务状态机）；
            // 无注册时为空 vec = 与旧行为完全一致。
            let chain = bus.collect_waterfall::<PreStep>();
            // 空链直接短路：无插件注册 PreStep 中间件（默认场景）时省去 waterfall 分发。
            let pre_input = if chain.is_empty() {
                messages.clone()
            } else {
                bus.waterfall(
                    PreStep {
                        input: messages.clone(),
                    },
                    &chain,
                )
                .input
            };
            // PreStep 包含不断增长的完整上下文，持久化会造成 O(n²) 日志膨胀。
            // 真正的会话重建只依赖 TurnStart/Assistant/ToolResult，因此不写入 SessionLog。

            // 每一步都执行上下文预算，而非仅在回合开始时裁剪；工具循环越长，
            // 节省的重复 prompt token 越明显。
            // 原子任务以低推理、短输出请求模型：工具门禁只能减少后续回合，只有这里
            // 能抑制首个 tool call 前的隐藏长思考与 `omitted` token 消耗。非原子任务
            // 完全保留用户的模型设置和默认输出预算。
            let controlled_delivery = controlled_delivery_turn;
            let runtime_allowed_tools = if controlled_delivery {
                goal_execution.allowed_tools()
            } else {
                execution.allowed_tools()
            };
            let request_options = if execution.solve_mode
                == crate::execution::SolveMode::AtomicDelivery
                || empty_recovery_pending
            {
                RequestOptions {
                    max_output_tokens: Some(if empty_recovery_pending { 1_024 } else { 1_536 }),
                    // DeepSeek/OpenAI 兼容端使用 `none` 表示关闭；`off` 是旧目录
                    // 的内部别名，直接透传会被网关以 HTTP 400 拒绝并中断整个回合。
                    reasoning_effort: Some("none".into()),
                    allowed_tools: Some(runtime_allowed_tools.clone()),
                }
            } else {
                RequestOptions {
                    allowed_tools: Some(runtime_allowed_tools.clone()),
                    ..Default::default()
                }
            };
            // S5/G2 并发执行器（Phase 1，成本优先）：存在多个无写冲突的就绪面时，
            // 对每个写冲突组并发开一轮作用域化模型往返，融合成单一流交给现有门禁 /
            // dispatch 主体（字节不变）；单写冲突组（最常见）仍走原单一流路径，零回归。
            // 融合只改变"模型回合"的来源（N 个流并发 I/O），门禁 / 写冲突串行 /
            // repeat_guard / 预算硬上限全部沿用原串行逻辑，协议配对由后续 join_all 保证。
            // 记录本步 assistant 在持久上下文中的固定插入点。工具执行期间会先把
            // Tool 结果以及控制器的恢复提示追加到 messages；不能再用“末尾减去
            // tool_call 数量”倒推位置，因为中间只要多出一条 user 提示，就会把
            // assistant 插到 Tool 结果之后，下一轮 OpenAI 请求随即以
            // invalid_tool_call_history 拒绝。
            let assistant_history_index = messages.len();
            let mut s: harness_llm::ChunkStream = if controlled_delivery {
                let groups: Vec<Vec<String>> = goal_execution
                    .parallel_write_groups()
                    .into_iter()
                    .take(crate::goal_execution::MAX_PARALLEL_SURFACES)
                    .collect();
                if groups.len() > 1 {
                    let mut streams: Vec<harness_llm::ChunkStream> =
                        Vec::with_capacity(groups.len());
                    for group in &groups {
                        let scoped_prompt = goal_execution.render_for_model_scoped(group);
                        let mut scoped_msgs = pre_input.clone();
                        // 把全局目标提示替换为该组作用域版本；找不到（如被 PreStep 改写）
                        // 则退化为全局提示，仍正确只是聚焦弱一些。
                        if let Some(pos) = scoped_msgs
                            .iter()
                            .position(|m| m.role == Role::System && m.content == goal_prompt)
                        {
                            scoped_msgs[pos] = Message::system(scoped_prompt);
                        }
                        streams.push(llm.stream_with_options(
                            apply_context_budget(scoped_msgs),
                            request_options.clone(),
                        ));
                    }
                    Box::pin(futures::stream::select_all(streams))
                } else {
                    llm.stream_with_options(apply_context_budget(pre_input), request_options)
                }
            } else {
                llm.stream_with_options(apply_context_budget(pre_input), request_options)
            };
            let mut assistant_text = String::new();
            let mut assistant_tools = Vec::new();
            let mut assistant_reasoning = String::new();
            let mut last_leak_reminder = String::new();
            let mut step_had_tools = false;
            let mut loop_recovery_prompts = Vec::new();
            let mut empty_response_reason: Option<String> = None;
            // 本步（单次请求）的 token 用量累计（AIOps 成本计量）。
            let mut step_usage = Usage::default();
            loop {
                // S5/G2+G5：每步清空本步归属记录，使并发准入在多面任务里每步重新生效。
                goal_execution.step_attributed.clear();
                let item = tokio::select! {
                    _ = cancellation.cancelled() => {
                        log.append(SessionEvent::Assistant { id: log.gen_id(), chunk: Chunk { text: Some("[已停止]".into()), ..Default::default() } });
                        cancelled = true;
                        debt = 0;
                        hard_stop = true;
                        break;
                    }
                    item = s.next() => item,
                };
                let Some(item) = item else {
                    break;
                };
                // 错误不再上抛吞掉：写入日志可见，并终止回合（置 debt=0），
                // 否则 TurnEnd 永不写入会让 UI 轮询死循环。
                let chunk = match item {
                    Ok(c) => c,
                    Err(e) => {
                        provider_error_seen = true;
                        provider_error_summary = e.to_string();
                        log.append(SessionEvent::Assistant {
                            id: log.gen_id(),
                            chunk: Chunk {
                                text: Some(format!("[error] {e}")),
                                ..Default::default()
                            },
                        });
                        debt = 0;
                        hard_stop = true;
                        break;
                    }
                };
                // 思考链同时写 UI Thinking 事件和 Assistant 流事件。后者在模型产生
                // tool_call 时是 DeepSeek 的协议上下文，不能只留在 UI 日志里。
                if let Some(r) = &chunk.reasoning {
                    log.append(SessionEvent::Thinking {
                        id: log.gen_id(),
                        text: r.clone(),
                    });
                }
                if let Some(u) = &chunk.usage {
                    step_usage = step_usage.saturating_add(*u);
                }
                if chunk.empty_response {
                    let reason = chunk
                        .finish_reason
                        .clone()
                        .unwrap_or_else(|| "unknown".into());
                    if matches!(reason.as_str(), "tool_calls" | "function_call") {
                        log.append(SessionEvent::Assistant {
                            id: log.gen_id(),
                            chunk: Chunk {
                                text: Some("[error] 上游声明要调用工具（finish_reason=tool_calls），但没有返回可执行的工具名称或参数。运行时已停止相同请求重试，避免空跑熔断；请检查兼容网关的流式 tool_calls 格式。".into()),
                                ..Default::default()
                            },
                        });
                        debt = 0;
                        hard_stop = true;
                        break;
                    }
                    empty_response_reason = Some(reason);
                    continue;
                }
                if chunk.text.is_some() || !chunk.tool_calls.is_empty() || chunk.reasoning.is_some()
                {
                    log.append(SessionEvent::Assistant {
                        id: log.gen_id(),
                        chunk: chunk.clone(),
                    });
                }
                if let Some(text) = &chunk.text {
                    assistant_text.push_str(text);
                }
                if let Some(reasoning) = &chunk.reasoning {
                    assistant_reasoning.push_str(reasoning);
                }
                assistant_tools.extend(chunk.tool_calls.clone());
                // 工具调用并行化：门禁（循环守卫/行动门禁/访问策略/PreToolUse 钩子）
                // 串行执行以保全状态与顺序语义；通过门禁的调用收集后 join 并行分发。
                // 此前同一步的 N 个工具调用逐个 await，与系统提示词鼓励的并行调用相悖。
                let mut pending: Vec<(&ToolCall, String, ActionProposal, Option<ActionContract>)> =
                    Vec::new();
                let mut pending_signatures = HashSet::new();
                // 原子任务的第一阶段只能有一个定位动作。否则模型即使知道“后续要
                // 缩小范围”，也可能在同一响应里并发发出 N 个不同关键词的泛搜。
                // 例外：零先验（内容扫描与路径降级都没命中）时放宽到多假设并行，
                // 此时"多个不同关键词"恰恰是唯一有效的探测手段。
                let locate_parallel = match &workspace_grounding {
                    Some(grounding) if grounding.zero_prior => ZERO_PRIOR_SEARCH_PARALLELISM,
                    _ => 1,
                };
                let mut locate_step_gate = LocateStepGate::with_parallelism(locate_parallel);
                for tc in &chunk.tool_calls {
                    // 守卫与行动门禁共用归一化签名，避免仅因路径分隔符或 `cd` 前缀
                    // 不同就绕过重复判定。
                    let mut proposal = ActionProposal::from_tool_call(tc, &execution);
                    if controlled_delivery {
                        goal_execution.link_proposal(&mut proposal);
                    }
                    let action_spec = controlled_delivery
                        .then(|| goal_execution.action_spec(tc, &proposal))
                        .flatten();
                    let sig = proposal.signature.clone();
                    log.append(SessionEvent::ToolCall {
                        id: log.gen_id(),
                        call: tc.clone(),
                    });

                    if repeat_guard.should_block(&sig) {
                        let recovery = repeat_guard.note_recovery(&sig);
                        let blocked = ToolResult {
                            call_id: tc.id.clone(),
                            ok: false,
                            content: format!(
                                "[tool-loop guard] 工具 {} 的相同参数调用不会带来新信息：成功调用不得原样重试，失败调用最多首次加一次定向重试。本次未执行；请分析已有结果、换用不同参数/工具或直接基于现有证据收尾。",
                                tc.name
                            ),
                            continuation_debt: 0,
                        };
                        log.append(SessionEvent::ToolResult {
                            id: log.gen_id(),
                            result: blocked.clone(),
                        });
                        messages.push(Message::tool(tc.id.clone(), blocked.content));
                        step_had_tools = true;
                        if recovery <= MAX_LOOP_RECOVERY_PROMPTS {
                            loop_recovery_prompts.push(format!(
                                "[循环恢复] 工具 {} 的相同调用已被拦截（恢复提示 {recovery}/{MAX_LOOP_RECOVERY_PROMPTS}）。任务尚未完成：先解释现有结果，再选择不同参数、不同工具或下一项验证；禁止原样重试。",
                                tc.name
                            ));
                        }
                        continue;
                    }

                    if controlled_delivery && action_spec.is_none() {
                        let blocked = ToolResult {
                            call_id: tc.id.clone(),
                            ok: false,
                            content: "[goal-execution gate] 该调用没有关联当前工作项；请先围绕当前验收项定位、修改或验证。".into(),
                            continuation_debt: 0,
                        };
                        log.append(SessionEvent::ToolResult {
                            id: log.gen_id(),
                            result: blocked.clone(),
                        });
                        messages.push(Message::tool(tc.id.clone(), blocked.content));
                        step_had_tools = true;
                        continue;
                    }
                    if controlled_delivery {
                        if let Err(reason) = goal_execution.allows_tool_call(tc, &proposal) {
                            if let Some(action) = &action_spec {
                                goal_execution.record_gate_rejection(action, &reason);
                            }
                            let blocked = ToolResult {
                                call_id: tc.id.clone(),
                                ok: false,
                                content: format!("[target-anchor gate] {reason}"),
                                continuation_debt: 0,
                            };
                            log.append(SessionEvent::ToolResult {
                                id: log.gen_id(),
                                result: blocked.clone(),
                            });
                            messages.push(Message::tool(tc.id.clone(), blocked.content));
                            step_had_tools = true;
                            continue;
                        }
                    }
                    if let Some(action_spec) = &action_spec {
                        log.append(SessionEvent::Thinking {
                            id: log.gen_id(),
                            text: format!(
                                "动作契约 {} / {}：{}；预期：{}；命中→{}；未命中→{}",
                                action_spec.work_item_id,
                                action_spec.hypothesis_id,
                                action_spec.purpose,
                                action_spec.expected_signal,
                                action_spec.on_hit,
                                action_spec.on_miss,
                            ),
                        });
                    }

                    if !locate_step_gate.allows(true, &sig) {
                        let blocked = ToolResult {
                                call_id: tc.id.clone(),
                                ok: false,
                                content: "[controlled-delivery guard] 当前受控交付阶段只允许一个定位 search；请先使用该结果缩小到具体文件/行号，再决定下一步。".into(),
                                continuation_debt: 0,
                            };
                        log.append(SessionEvent::ToolResult {
                            id: log.gen_id(),
                            result: blocked.clone(),
                        });
                        messages.push(Message::tool(tc.id.clone(), blocked.content));
                        step_had_tools = true;
                        continue;
                    }

                    // 同一回复里出现完全相同的并行调用时，执行其中一个不会比执行全部
                    // 少任何信息，只会放大空跑成本。这里在分发前去重，仍给每个调用补齐
                    // 协议要求的 tool result。
                    if !pending_signatures.insert(sig.clone()) {
                        let blocked = ToolResult {
                            call_id: tc.id.clone(),
                            ok: false,
                            content: format!(
                                "[tool-loop guard] 工具 {} 与本步骤中已排队调用的参数完全相同，已跳过重复执行；请使用第一个结果继续。",
                                tc.name
                            ),
                            continuation_debt: 0,
                        };
                        log.append(SessionEvent::ToolResult {
                            id: log.gen_id(),
                            result: blocked.clone(),
                        });
                        messages.push(Message::tool(tc.id.clone(), blocked.content));
                        step_had_tools = true;
                        continue;
                    }

                    // 通用行动门禁：每个工具动作必须关联验收目标。调用/时间预算是软检查点，
                    // 只触发进展诊断与续期，不会因为任务耗时较长而拒绝必要动作。
                    if let GateDecision::Deny(reason) = ActionGate::authorize_with_tools(
                        &proposal,
                        &execution,
                        &budget,
                        &runtime_allowed_tools,
                    ) {
                        if let Some(action) = &action_spec {
                            goal_execution.record_gate_rejection(action, &reason);
                        }
                        let denied = ToolResult {
                            call_id: tc.id.clone(),
                            ok: false,
                            content: format!("[execution gate] {reason}"),
                            continuation_debt: 0,
                        };
                        log.append(SessionEvent::ToolResult {
                            id: log.gen_id(),
                            result: denied.clone(),
                        });
                        repeat_guard.record_result(&sig, &denied);
                        messages.push(Message::tool(tc.id.clone(), denied.content));
                        step_had_tools = true;
                        continue;
                    }
                    execution.tool_calls =
                        execution.tool_calls.saturating_add(proposal.estimated_cost);

                    if let Some(policy) = ctx.try_get::<harness_core::AccessPolicy>() {
                        if !policy.allows(&tc.name, &tc.args) {
                            access_denial_observed = true;
                            let denied = ToolResult {
                                call_id: tc.id.clone(),
                                ok: false,
                                content: format!(
                                    "[access-policy denied] 访问权限“{}”拒绝了该工具调用",
                                    policy.mode()
                                ),
                                continuation_debt: 0,
                            };
                            log.append(SessionEvent::ToolResult {
                                id: log.gen_id(),
                                result: denied.clone(),
                            });
                            repeat_guard.record_result(&sig, &denied);
                            messages.push(Message::tool(tc.id.clone(), denied.content));
                            step_had_tools = true;
                            continue;
                        }
                    }

                    // 钩子（PreToolUse）：可阻断危险工具调用（fail-closed 在 Provider 侧）。
                    let pre = hook.run(&HookPayload {
                        event: HookEvent::PreToolUse,
                        tool: Some(tc.name.clone()),
                        input: Some(format!("{:?}", tc)),
                        ..Default::default()
                    })?;
                    if let HookDecision::Block(reason) = pre {
                        let blocked = ToolResult {
                            call_id: tc.id.clone(),
                            ok: false,
                            content: format!("[blocked by hook] {reason}"),
                            continuation_debt: 0,
                        };
                        log.append(SessionEvent::ToolResult {
                            id: log.gen_id(),
                            result: blocked.clone(),
                        });
                        repeat_guard.record_result(&sig, &blocked);
                        messages.push(Message::tool(tc.id.clone(), blocked.content.clone()));
                        step_had_tools = true;
                        continue;
                    }

                    // Fix2：搜索类调用先查会话级记忆化缓存；命中直接返回缓存结果，
                    // 不重跑真实工具，消除重复扫描与续跑重扫。只读搜索不重复记证据/写入。
                    if is_search_like(&tc.name) {
                        let key = search_cache_key(&tc.name, &tc.args);
                        if let Some(cached) = search_memo().lock().unwrap().get(&key).cloned() {
                            log.append(SessionEvent::ToolResult {
                                id: log.gen_id(),
                                result: cached.clone(),
                            });
                            repeat_guard.record_result(&sig, &cached);
                            messages.push(Message::tool(tc.id.clone(), cached.content.clone()));
                            step_had_tools = true;
                            continue;
                        }
                    }
                    // 通过全部门禁；延迟到本批收集完毕后并行执行。
                    pending.push((tc, sig, proposal, action_spec));
                }
                // 并行执行阶段：只有纯 I/O 的 dispatch 并行；结果顺序与 tool_calls
                // 声明顺序一致，保证 tool 消息与 assistant 宣告一一配对。
                if !pending.is_empty() {
                    let futs = pending.iter().map(|(tc, _sig, _proposal, _action)| {
                        let tools = Arc::clone(&tools);
                        async move {
                            // 工具调用必须有超时和取消通道。此前只有模型流设置了 idle
                            // 超时，某个 shell / 插件工具卡住时 UI 会一直 busy。
                            let outcome = tokio::time::timeout(
                                std::time::Duration::from_secs(tool_timeout_secs),
                                tools.dispatch(tc),
                            )
                            .await;
                            match outcome {
                                Ok(Ok(result)) => result,
                                Ok(Err(error)) => ToolResult {
                                    call_id: tc.id.clone(),
                                    ok: false,
                                    content: format_tool_dispatch_error(error),
                                    continuation_debt: 0,
                                },
                                Err(_) => ToolResult {
                                    call_id: tc.id.clone(),
                                    ok: false,
                                    content: format!(
                                        "工具 {} 超过 {tool_timeout_secs} 秒未返回，已停止本次调用",
                                        tc.name
                                    ),
                                    continuation_debt: 0,
                                },
                            }
                        }
                    });
                    let joined = tokio::select! {
                        _ = cancellation.cancelled() => None,
                        results = futures::future::join_all(futs) => Some(results),
                    };
                    match joined {
                        Some(results) => {
                            for ((tc, sig, proposal, action), res) in pending.iter().zip(results) {
                                sandbox_denial_observed |= res.content.contains("[sandbox denied]");
                                access_denial_observed |=
                                    res.content.contains("[access-policy denied]");
                                // 钩子（PostToolUse）：审计 / 后处理挂钩点。
                                let _ = hook.run(&HookPayload {
                                    event: HookEvent::PostToolUse,
                                    tool: Some(tc.name.clone()),
                                    output: Some(format!("{:?}", res)),
                                    ..Default::default()
                                });
                                log.append(SessionEvent::ToolResult {
                                    id: log.gen_id(),
                                    result: res.clone(),
                                });
                                repeat_guard.record_result(sig, &res);
                                // Fix2：把搜索类调用结果写入会话级记忆化缓存，供后续同查询直接复用。
                                if is_search_like(&tc.name) {
                                    let key = search_cache_key(&tc.name, &tc.args);
                                    search_memo().lock().unwrap().insert(key, res.clone());
                                }
                                execution.record_tool_result(proposal, res.ok, &res.content);
                                let evidence_kind = if let Some(action) = action {
                                    goal_execution.record_action_result(
                                        action,
                                        proposal,
                                        res.ok,
                                        &res.content,
                                    )
                                } else {
                                    EvidenceKind::NoInformation
                                };
                                if evidence_kind != EvidenceKind::NoInformation {
                                    goal_correction_notified = false;
                                }
                                // S4 收敛判据分层：写入刚落盘（或读出来发现已经满足），此刻
                                // 立即以**磁盘产物**为准逐字复核。能证明的交付面免 shell 直接
                                // 置 Verified，不再让界面/字段/签名这类静态可证的面永远卡在
                                // 待验证（ADR §2.3）。复核失败则什么都不改，继续走执行验证。
                                //
                                // 两种证据都要触发：ChangeApplied 是"刚改完"，AlreadySatisfied
                                // 是"本来就对"——后者同样会把面停在 Satisfied，漏掉它就等于
                                // 只修了一半的卡死。
                                if matches!(
                                    evidence_kind,
                                    EvidenceKind::ChangeApplied | EvidenceKind::AlreadySatisfied
                                ) {
                                    if let Some(root) = &workspace_root {
                                        for (id, proof) in
                                            goal_execution.settle_static_convergence(root)
                                        {
                                            execution
                                                .record_static_verification(&id, proof.clone());
                                            ledger.add_evidence(&id, proof.clone());
                                            ledger.verify(&id);
                                            append_telemetry(
                                                &log,
                                                &execution,
                                                &goal_execution,
                                                &ledger,
                                                &format!("{id} {proof}"),
                                            );
                                        }
                                    }
                                }
                                for criterion in &proposal.supports {
                                    ledger.activate(criterion);
                                    if res.ok && execution.satisfied_criteria.contains(criterion) {
                                        ledger.add_evidence(
                                            criterion,
                                            res.content.chars().take(480).collect(),
                                        );
                                        ledger.verify(criterion);
                                    }
                                }
                                if goal_execution.read_only && goal_execution.can_conclude() {
                                    for item in goal_execution.items.values() {
                                        if let Some(evidence) = item.evidence.last() {
                                            ledger.add_evidence(&item.id, evidence.clone());
                                            ledger.verify(&item.id);
                                        }
                                    }
                                }
                                let telemetry_detail = format!(
                                    "{}；信息增益：{:?}",
                                    if res.ok {
                                        "已记录工具结果"
                                    } else {
                                        "工具结果失败，等待调整"
                                    },
                                    evidence_kind
                                );
                                append_telemetry(
                                    &log,
                                    &execution,
                                    &goal_execution,
                                    &ledger,
                                    &telemetry_detail,
                                );
                                messages.push(Message::tool(tc.id.clone(), res.content.clone()));
                            }
                            step_had_tools = true;
                            // V3：两个确定性定位探针均无命中后，立刻结束本回合并
                            // 给出可回答的工作区问题。不能再把“没有目标证据”交回
                            // 给模型进行第 N 轮同义词搜索。
                            if let Some(reason) = goal_execution.actionable_terminal_reason() {
                                if reason.contains("需要用户确认工作区或目标路径") {
                                    log.append(SessionEvent::Assistant {
                                        id: log.gen_id(),
                                        chunk: Chunk {
                                            text: Some(format!("[需要澄清] {reason}")),
                                            ..Default::default()
                                        },
                                    });
                                    debt = 0;
                                    hard_stop = true;
                                }
                            }
                        }
                        None => {
                            // 取消：仍须为每个已宣告的 tool_call 补占位结果，否则
                            // 续跑/恢复会因「已宣告未执行」的孤儿调用直接 400。
                            for (tc, _sig, _proposal, _action) in &pending {
                                messages.push(Message::tool(
                                    tc.id.clone(),
                                    format!("[已停止] 工具 {} 已取消", tc.name),
                                ));
                            }
                            step_had_tools = true;
                            cancelled = true;
                            debt = 0;
                            hard_stop = true;
                            break;
                        }
                    }
                }
            }
            // S5/G4：每步工具结果后检查跨面概念漏改，存在则向模型追加一次提醒（去重），
            // 真正关闭"改了 A 漏了 B"的跨面一致性问题。
            if !goal_execution.missing_concept_coverage().is_empty() {
                let leak = goal_execution.concept_coverage_checklist();
                if leak != last_leak_reminder {
                    last_leak_reminder = leak.clone();
                    messages.push(Message::user(leak));
                }
            }
            let mut claim_recovery_requested = false;
            let completion_ready = execution.can_complete() && goal_execution.can_conclude();
            if let Some(correction) = unsupported_runtime_claim_correction(
                &assistant_text,
                controlled_delivery || execution.write_attempts > 0,
                execution.write_operations,
                completion_ready,
                sandbox_denial_observed,
                access_denial_observed,
            ) {
                // 每次无证据声明都必须阻断完成裁决；用户可见的校正只展示一次，
                // 后续重复则继续通过内部恢复提示纠偏，避免刷屏。
                claim_recovery_requested = true;
                if !claim_correction_notified {
                    claim_correction_notified = true;
                    log.append(SessionEvent::Assistant {
                        id: log.gen_id(),
                        chunk: Chunk {
                            text: Some(format!("[事实校正] {correction}")),
                            ..Default::default()
                        },
                    });
                }
                loop_recovery_prompts.push(format!(
                    "[运行时事实校正] {correction} 不得重复无证据结论；继续调用当前阶段允许的工具，成功写盘并验证后才能声称已落实。"
                ));
            }
            let should_recover_empty = empty_response_reason.is_some()
                && assistant_text.trim().is_empty()
                && assistant_tools.is_empty();
            if !assistant_text.trim().is_empty() {
                last_assistant = assistant_text.clone();
            }
            // 本步用量落盘：Usage 事件不进模型上下文、不影响多轮重建，
            // 仅用于会话级成本计量（usage_total）。
            if step_usage.total_tokens > 0 {
                // 判顶前置需要「上一轮实际发了多少 prompt」与「本回合累计 prompt」，
                // 二者都必须在 step_usage 被 move 进事件之前取。
                last_prompt_tokens = step_usage.prompt_tokens;
                turn_prompt_tokens += step_usage.prompt_tokens;
                log.append(SessionEvent::Usage {
                    id: log.gen_id(),
                    usage: step_usage,
                });
            }
            if !should_recover_empty {
                empty_recovery_pending = false;
                insert_assistant_at_step_boundary(
                    &mut messages,
                    assistant_history_index,
                    Message::assistant_with_tools_and_reasoning(
                        assistant_text,
                        assistant_tools,
                        (!assistant_reasoning.is_empty()).then_some(assistant_reasoning),
                    ),
                );
            }
            for prompt in loop_recovery_prompts {
                messages.push(Message::user(prompt));
            }
            // 续跑记账：本步无论并行多少个工具调用只续跑一次。旧的按调用 +1
            // 会让 N 个并行调用触发 N 次额外模型往返，步数预算被成倍消耗。
            // 硬终止（取消/错误/循环守卫）时禁止复活：此时可能有「已宣告未执行」的
            // tool_call 缺对应 tool 消息，续跑必 400。
            if should_recover_empty && !hard_stop {
                let reason = empty_response_reason.as_deref().unwrap_or("unknown");
                // 任意 finish_reason 的空响应都不能原样重试。`stop` 同样可能来自网关
                // 截断或上下文污染；继续携带完整历史只会稳定复现同一个空结果。
                let retry_limit = MAX_EMPTY_RESPONSE_RETRIES;
                if empty_response_retries < retry_limit {
                    empty_response_retries += 1;
                    debt += 1;
                    messages = compact_for_empty_recovery(
                        messages,
                        &execution.compact_checkpoint(),
                        &goal_execution.render_for_model(),
                        reason,
                        empty_response_retries,
                        retry_limit,
                    );
                    empty_recovery_pending = true;
                } else {
                    log.append(SessionEvent::Assistant {
                        id: log.gen_id(),
                        chunk: Chunk {
                            text: Some(format!(
                                "[error] 模型连续 {} 次返回空响应（最后 finish_reason={reason}）。请求未被视为完成；请检查模型/网关日志、输出 token 限制或切换模型后重试。",
                                retry_limit + 1
                            )),
                            ..Default::default()
                        },
                    });
                    debt = 0;
                    hard_stop = true;
                }
            } else if (step_had_tools || claim_recovery_requested) && !hard_stop {
                debt += 1;
            }
            log.append(SessionEvent::StepEnd {
                id: log.gen_id(),
                step: steps,
            });

            if should_recover_empty {
                // 恢复重试已经重新记账，不能再被“本步没有工具”误判为完成。
            } else if claim_recovery_requested {
                // 事实校正已经发放一次续跑；本步正文不能作为完成结论继续裁决。
            } else if controlled_delivery {
                match goal_execution.evaluate_completion(step_had_tools) {
                    GoalCompletion::Complete => {
                        delivery_verified = true;
                        debt = 0;
                    }
                    GoalCompletion::Correct(hint) if !hard_stop => {
                        let correction = if goal_correction_notified {
                            format!(
                                "[自动推进] 当前任务仍未验收。{hint} 下一步不需要用户决策，直接执行并验证，不要等待用户回复。"
                            )
                        } else {
                            goal_correction_notified = true;
                            format!("[V4 目标状态校正] 当前回复没有满足求解图终态。{hint}")
                        };
                        messages.push(Message::user(correction));
                        debt += 1;
                    }
                    GoalCompletion::Terminal(reason) => {
                        log.append(SessionEvent::Assistant {
                            id: log.gen_id(),
                            chunk: Chunk {
                                text: Some(format!("[需要澄清] {reason}")),
                                ..Default::default()
                            },
                        });
                        debt = 0;
                        hard_stop = true;
                    }
                    GoalCompletion::Continue | GoalCompletion::Correct(_) => {}
                }
            } else if BudgetManager::phase(&execution, &budget)
                == crate::execution::BudgetPhase::Exhausted
            {
                budget_exhausted = true;
                match BudgetManager::diagnose_and_renew(&mut execution, &mut budget) {
                    Some(diagnosis) => {
                        convergence_notified = false;
                        messages.push(Message::user(&diagnosis));
                    }
                    None if !final_window_armed => {
                        // 续期次数用尽：进入最终收尾窗口（非截断）——给足步骤
                        // 汇总交付，但不再允许扩张探索（旧实现无限续期，
                        // 单回合可跑出 591 步）。
                        final_window_armed = true;
                        BudgetManager::arm_final_window(&execution, &mut budget);
                        messages.push(Message::user(
                            "[收尾阶段] 预算续期已用完，请在接下来 6 步内完成交付：停止新的探索与扫描，把已完成的改动固化（必要的写入/构建验证），然后输出结构化总结：1) 已完成的交付物与验证结果；2) 未完成部分及原因；3) 建议的后续步骤。请先自评：若剩余工作能在窗口内完成，就集中精力做完；若判断无法完成，立即停止新的工具调用，直接输出上述总结，不要浪费收尾窗口。",
                        ));
                    }
                    // 步骤⑤退位：收尾窗口亦耗尽时不再 [强制收敛]/[自动接续] 无限延展
                    // （那正是简单任务步数失控的放大器）。回合交由控制器栈底/R3 顶
                    // 收口，失败也带 R4 四要素资产，绝不再自动发“继续”。
                    None => {}
                }
            } else {
                match CompletionJudge::evaluate(&execution, &budget, step_had_tools) {
                    Completion::Converge(reason) if !convergence_notified => {
                        convergence_notified = true;
                        messages.push(Message::user(&format!("[系统提示] {reason}")));
                        // 收敛提示必须伴随一次新的模型请求；此前无工具调用时 debt
                        // 已归零，提示虽写入上下文却没有机会被模型执行，回合直接以
                        // Blocked 结束。只补一次，后续仍由预算/重复守卫负责收尾。
                        if !hard_stop {
                            debt += 1;
                        }
                    }
                    Completion::Complete => {
                        if execution.solve_mode == crate::execution::SolveMode::OpenEnded
                            || goal_execution.can_conclude()
                        {
                            delivery_verified = true;
                            debt = 0;
                        } else if !goal_correction_notified && !hard_stop {
                            goal_correction_notified = true;
                            messages.push(Message::user(format!(
                                "[目标状态校正] 模型准备结束，但当前工作项尚未达到可验收状态。{}",
                                goal_execution.next_action_hint()
                            )));
                            debt += 1;
                        }
                    }
                    _ => {}
                }
            }

            // 控制器观测点：把本步新事件折叠进投影，由 TurnGovernor 给出唯一决策。
            // 换路/降级只注入提示（旧守卫并行存续，其退位属步骤⑤）；
            // Terminate 是全系统唯一的回合终止来源（spec §4.1 G1）。
            if let Some(gov) = governor.as_mut() {
                let (next_cursor, fresh) = log.replay_from(case_cursor);
                case_cursor = next_cursor;
                session_case.absorb(&fresh);
                match gov.observe(&session_case, steps, execution.write_operations) {
                    Decision::SwitchStrategy => messages.push(Message::user(format!(
                        "[换路] 本窗口零增益，策略已切换至 {}。禁止重复 case file 中已尝试过的 {} 次调用。",
                        gov.strategy_hint(),
                        session_case.tried.len()
                    ))),
                    Decision::Degrade => messages.push(Message::user(
                        "[降至栈底] 请交付可验证的子目标：停止扩大探索，把已确认的部分整理为结构化交付（已完成、证据锚点、未完成原因、下一步）。",
                    )),
                    Decision::Terminate(_)
                        if controlled_delivery
                            && !hard_stop
                            && goal_execution.can_auto_advance() =>
                    {
                        if debt == 0 {
                            messages.push(Message::user(format!(
                                "[自动推进] 下一步无需用户决策：{} 直接执行并完成验证，不要等待用户回复。",
                                goal_execution.next_action_hint()
                            )));
                            debt += 1;
                        }
                    }
                    Decision::Terminate(_) => break,
                    Decision::Continue => {}
                }
            }
            // 唯一终止检查点（serial，无 next()）。
            let stop = bus
                .serial(TurnStopping {
                    will_stop: debt == 0,
                })
                .await;
            if stop.will_stop {
                break;
            }
        }

        // 记忆自动沉淀（L0）：记录本轮助手最终回复（无后端则落本地文件）。失败容忍。
        if !last_assistant.trim().is_empty() {
            if let Some(conv) = ctx.try_get::<dyn ConversationMemory>() {
                let _ = conv
                    .record_turn(ChatTurn {
                        session_id: log.id().to_string(),
                        role: "assistant".into(),
                        content: last_assistant.clone(),
                        ts: String::new(),
                    })
                    .await;
            }
        }

        let terminal_reason = goal_execution.actionable_terminal_reason();
        let (raw_outcome, raw_reason) = if provider_error_seen {
            // provider 流错误优先级最高：错误文本非模型回答，绝不可 Verified；
            // On 模式下游出口会把 SystemFailure 收口为 PartialDelivery；内部诊断资产
            // 只写遥测，用户看到的是简短的缺失步骤。
            (
                harness_session::DeliveryOutcome::SystemFailure,
                Some(format!(
                    "llm provider error（流读取已终止，未获有效模型回答）: {provider_error_summary}"
                )),
            )
        } else if delivery_verified && execution.can_complete() {
            (harness_session::DeliveryOutcome::Verified, None)
        } else if delivery_verified {
            (
                harness_session::DeliveryOutcome::PartialDelivery,
                Some("求解图已到终态，但执行证据没有覆盖全部验收项；已拒绝 Verified".into()),
            )
        } else if terminal_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("需要用户确认"))
        {
            (
                harness_session::DeliveryOutcome::NeedsUserInput,
                terminal_reason,
            )
        } else if !execution.changed_criteria.is_empty() {
            (
                harness_session::DeliveryOutcome::PartialDelivery,
                terminal_reason
                    .or_else(|| Some("已有修改，但尚未获得覆盖全部验收项的验证证据".into())),
            )
        } else if absolute_budget_hit {
            // Fix4（轻量）：把已探索证据要点并入停止原因，使下一次续跑的
            // resume_instruction 能直接展示，模型从证据前沿继续而非从零重探。
            let evidence_hint = if execution.evidence.is_empty() {
                String::new()
            } else {
                let keys: Vec<&String> = execution.evidence.keys().collect();
                format!(
                    "；已探索证据：{}",
                    keys.iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join("、")
                )
            };
            (
                harness_session::DeliveryOutcome::SystemFailure,
                terminal_reason
                    .or_else(|| Some("已达到安全探索预算，但未形成可验证的目标路径".into()))
                    .map(|reason| format!("{reason}{evidence_hint}")),
            )
        } else if cancelled {
            (
                harness_session::DeliveryOutcome::Cancelled,
                Some("用户取消了回合；未获得完整验收证据".into()),
            )
        } else if hard_stop {
            (
                harness_session::DeliveryOutcome::Interrupted,
                Some("回合因取消、模型异常或重复调用保护而中断；未获得完整验收证据".into()),
            )
        } else if budget_exhausted {
            (
                harness_session::DeliveryOutcome::SystemFailure,
                terminal_reason.or_else(|| Some("执行窗口结束前未形成完整验收证据".into())),
            )
        } else {
            (
                harness_session::DeliveryOutcome::SystemFailure,
                terminal_reason.or_else(|| Some("回合异常结束，未形成完整验收证据".into())),
            )
        };
        // 出口收口（spec §4.2）：控制器模式下只剩两个出口。Verified 即 Delivered；
        // 用户取消保持 Cancelled（强行改判会剥夺取消语义，且它不是治理失败）；
        // 其余一律收敛为 PartialDelivery。R4 四要素是内部诊断资产，写入 Telemetry；
        // 用户只看到“缺少哪一步 + 下一步”，避免把锚点、假设和门禁术语直接倾倒到 UI。
        let (outcome, reason) = if self.governor == GovernorMode::On
            && !matches!(
                raw_outcome,
                DeliveryOutcome::Verified | DeliveryOutcome::Cancelled
            ) {
            let (_, fresh) = log.replay_from(case_cursor);
            session_case.absorb(&fresh);
            let mut final_case = session_case.clone();
            if let Some(gov) = governor.as_ref() {
                final_case
                    .eliminated
                    .extend(gov.eliminated().iter().cloned());
            }
            // 只有真正缺少用户信息时才形成问项。预算暂停、系统错误和普通未完成状态
            // 都已有确定的恢复路径；把原因改写成“是否继续”会在用户已明确说“继续”后
            // 原样复读，形成无法退出的伪澄清循环。
            let candidate = (raw_outcome == DeliveryOutcome::NeedsUserInput)
                .then(|| raw_reason.as_deref().map(|r| format!("请确认：{r}")))
                .flatten();
            let artifact = artifact_text(
                &final_case,
                raw_reason.as_deref().unwrap_or(""),
                &goal_execution.next_action_hint(),
                candidate.as_deref(),
            );
            append_telemetry(
                &log,
                &execution,
                &goal_execution,
                &ledger,
                &format!("内部终止资产：{}", artifact.replace('\n', " | ")),
            );
            let status = concise_incomplete_status(
                &raw_outcome,
                raw_reason.as_deref(),
                execution.write_operations,
                execution.write_attempts,
                goal_execution.phase(),
            );
            log.append(SessionEvent::Assistant {
                id: log.gen_id(),
                chunk: Chunk {
                    text: Some(status.clone()),
                    ..Default::default()
                },
            });
            (DeliveryOutcome::PartialDelivery, Some(status))
        } else {
            (raw_outcome, raw_reason)
        };
        log.append(SessionEvent::Delivery {
            id: log.gen_id(),
            report: execution.delivery_report(outcome.clone(), reason),
        });
        // 只在 Runtime 验证通过后沉淀经验卡；模型文本或 TurnEnd 绝不触发写入，
        // 这样下一次检索到的是可复核的解决路径而不是自报完成。
        if outcome == harness_session::DeliveryOutcome::Verified {
            if let Some(conv) = ctx.try_get::<dyn ConversationMemory>() {
                let workspace = ctx
                    .try_get::<harness_core::Workspace>()
                    .map(|ws| ws.root().display().to_string())
                    .unwrap_or_else(|| "unknown-workspace".into());
                let evidence = execution
                    .verification_evidence
                    .values()
                    .flatten()
                    .take(3)
                    .map(|item| item.chars().take(220).collect::<String>())
                    .collect::<Vec<_>>()
                    .join(" | ");
                let fingerprint = stable_fingerprint(&execution.contract.objective);
                let card =
                    MemoryFact {
                        id: format!("solve-card:{fingerprint}"),
                        kind: FactKind::Decision,
                        content: format!(
                        "[SolveCard]\\n问题：{}\\n工作区：{}\\n有效验证：{}\\n结果：已验证交付。",
                        execution.contract.objective.chars().take(600).collect::<String>(),
                        workspace,
                        evidence,
                    ),
                        layer: LifecycleLayer::L2,
                        confidence: 0.9,
                        source: format!(
                            "solve-card;workspace={workspace};fingerprint={fingerprint}"
                        ),
                    };
                let _ = conv.remember(card).await;
            }
        }
        append_telemetry(
            &log,
            &execution,
            &goal_execution,
            &ledger,
            "回合结束，交付状态已落盘",
        );
        log.append(SessionEvent::TurnEnd { id: log.gen_id() });
        Ok(())
    }
}

fn concise_incomplete_status(
    outcome: &DeliveryOutcome,
    reason: Option<&str>,
    successful_writes: usize,
    write_attempts: usize,
    phase: crate::goal_execution::SolvePhase,
) -> String {
    if *outcome == DeliveryOutcome::NeedsUserInput {
        let detail = reason
            .unwrap_or("需要补充目标范围或预期结果")
            .replace(['\r', '\n'], " ");
        let detail = detail
            .trim()
            .trim_start_matches("需要用户确认")
            .trim_start_matches(['：', ':', '；', ';', ' ']);
        return format!(
            "需要你的确认：{}",
            detail.chars().take(180).collect::<String>()
        );
    }
    if reason.is_some_and(|text| text.contains("llm provider error")) {
        return "未完成：模型服务在执行过程中返回错误。\n下一步：恢复模型服务后重试本次任务。"
            .into();
    }
    if successful_writes > 0 {
        return "未完成：缺少“验证”步骤。\n下一步：运行与本次修改相关的最小构建或测试。".into();
    }
    if write_attempts > 0 {
        return "未完成：缺少“写入修改”步骤，之前的编辑没有成功落盘。\n下一步：修正编辑内容并确认目标文件确实发生变化。".into();
    }
    match phase {
        crate::goal_execution::SolvePhase::Locate => {
            "未完成：缺少“定位实现文件”步骤。\n下一步：用目标中的明确文字或符号做一次限定范围搜索。"
                .into()
        }
        crate::goal_execution::SolvePhase::Inspect => {
            "未完成：缺少“确认修改位置”步骤。\n下一步：读取已命中文件的相关代码区间。".into()
        }
        crate::goal_execution::SolvePhase::Change => {
            "未完成：缺少“写入修改”步骤。\n下一步：对已确认文件执行最小编辑。".into()
        }
        crate::goal_execution::SolvePhase::Verify => {
            "未完成：缺少“验证”步骤。\n下一步：运行与本次修改相关的最小构建或测试。".into()
        }
        crate::goal_execution::SolvePhase::Conclude => {
            "未完成：验收证据不完整。\n下一步：补充尚未通过的验收项证据。".into()
        }
    }
}

fn append_telemetry(
    log: &SessionLog,
    execution: &ExecutionState,
    goal_execution: &GoalExecution,
    ledger: &TaskLedger,
    detail: &str,
) {
    let current = ledger
        .current_item()
        .map(|item| format!("{}：{}", item.id, item.description))
        .unwrap_or_else(|| "全部验收项已处理".into());
    log.append(SessionEvent::Telemetry {
        id: log.gen_id(),
        telemetry: ExecutionTelemetry {
            executor: if goal_executor_enabled()
                && execution.solve_mode != crate::execution::SolveMode::OpenEnded
            {
                "v4".into()
            } else {
                "legacy".into()
            },
            goal: goal_execution.goal.objective.clone(),
            intent: format!(
                "{:?}",
                crate::IntentProfile::compile(&execution.contract.objective).kind
            ),
            phase: if execution.solve_mode == crate::execution::SolveMode::OpenEnded {
                execution.tool_phase().as_str()
            } else {
                goal_execution.phase_name()
            }
            .into(),
            allowed_tools: if execution.solve_mode == crate::execution::SolveMode::OpenEnded {
                execution.allowed_tools()
            } else {
                goal_execution.allowed_tools()
            },
            step: execution.steps,
            tool_calls: execution.tool_calls,
            evidence_count: execution.evidence.len(),
            verified_count: ledger.verified_count(),
            blocked_count: ledger.blocked_count(),
            active_work_item: goal_execution
                .active_item()
                .map(|item| format!("{}：{}", item.id, item.description))
                .unwrap_or_else(|| "无".into()),
            work_items: goal_execution
                .items
                .values()
                .map(|item| WorkItemTelemetry {
                    id: item.id.clone(),
                    description: item.description.clone(),
                    state: item.state.as_str().into(),
                    evidence_count: item.evidence.len(),
                })
                .collect(),
            next_action: goal_execution.next_action_hint(),
            active_hypothesis: goal_execution.active_hypothesis_summary(),
            no_information_count: goal_execution.no_information_count,
            correction_count: goal_execution.correction_count,
            detail: format!("{detail}；当前验收：{current}"),
        },
    });
}

fn render_experience(facts: &[MemoryFact]) -> Option<String> {
    let cards = facts
        .iter()
        .filter(|fact| fact.content.starts_with("[SolveCard]"))
        .take(3)
        .map(|fact| fact.content.chars().take(700).collect::<String>())
        .collect::<Vec<_>>();
    (!cards.is_empty()).then(|| {
        format!(
            "[已验证历史经验]\\n以下是检索出的经验卡，只能作为候选线索；先用当前工作区证据验证，不能把它当作当前事实：\\n{}",
            cards.join("\\n---\\n")
        )
    })
}

fn stable_fingerprint(text: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// 生成紧凑的技能系统指令，避免用户导入的长技能文档无限放大上下文。
fn render_skill_instructions(skills: &[Skill]) -> Option<String> {
    const MAX_SKILLS: usize = 4;
    const MAX_STEP_CHARS: usize = 360;
    let mut out = String::from("[已启用的匹配技能]\n");
    for skill in skills.iter().take(MAX_SKILLS) {
        let steps = skill
            .steps
            .join("；")
            .chars()
            .take(MAX_STEP_CHARS)
            .collect::<String>();
        let checks = skill
            .verification_rules
            .join("；")
            .chars()
            .take(MAX_STEP_CHARS)
            .collect::<String>();
        out.push_str(&format!(
            "- {}：适用范围：{}\n  执行：{}\n  验证：{}\n",
            skill.name,
            skill.trigger_boundary,
            if steps.is_empty() {
                "遵循技能文档的步骤"
            } else {
                &steps
            },
            if checks.is_empty() {
                "完成后进行必要验证"
            } else {
                &checks
            },
        ));
    }
    (out.lines().count() > 1).then_some(out)
}

fn messages_from_events(events: &[SessionEvent]) -> Vec<Message> {
    let mut messages = vec![Message::system(SYSTEM_PROMPT)];
    // DeepSeek can stream reasoning_content in frames before the frame containing
    // its tool call. Older logs stored those frames only as Thinking events, while
    // newer logs additionally store them in the Assistant chunk. Keep fragments
    // until the visible assistant frame is available in either representation.
    let mut pending_reasoning = String::new();
    for event in events {
        match event {
            SessionEvent::TurnStart { input, .. } => {
                // A reasoning-only response without text or a tool call is not a
                // valid reusable assistant turn. Do not attach it to a later user
                // turn (and never invent reasoning content).
                pending_reasoning.clear();
                messages.push(Message::user(input));
            }
            SessionEvent::Assistant { chunk, .. } => {
                // SSE 流式下每个文本增量都是一条 Assistant 事件；重建上下文时必须把相邻的
                // 纯文本 assistant 分片合并为一条消息，否则会向模型发送大量连续 assistant
                // 消息（OpenAI 兼容协议不接受，多轮对话直接报错）。
                let text = chunk.text.clone().unwrap_or_default();
                if let Some(reasoning) = &chunk.reasoning {
                    // 新日志顺序为 Thinking → Assistant(reasoning)，避免将同一 SSE
                    // 分片记录两次；旧日志只有 Thinking，仍可在此后工具帧完成恢复。
                    if !pending_reasoning.ends_with(reasoning) {
                        pending_reasoning.push_str(reasoning);
                    }
                }
                if text.is_empty() && chunk.tool_calls.is_empty() {
                    continue;
                }
                let reasoning_content =
                    (!pending_reasoning.is_empty()).then(|| std::mem::take(&mut pending_reasoning));
                if chunk.tool_calls.is_empty() {
                    if let Some(last) = messages.last_mut() {
                        if last.role == Role::Assistant && last.tool_calls.is_empty() {
                            last.content.push_str(&text);
                            if let Some(reasoning) = reasoning_content {
                                last.reasoning_content
                                    .get_or_insert_with(String::new)
                                    .push_str(&reasoning);
                            }
                            continue;
                        }
                    }
                }
                messages.push(Message::assistant_with_tools_and_reasoning(
                    text,
                    chunk.tool_calls.clone(),
                    reasoning_content,
                ));
            }
            SessionEvent::ToolResult { result, .. } => {
                pending_reasoning.clear();
                messages.push(Message::tool(
                    &result.call_id,
                    &compress_tool_context(&result.content),
                ));
            }
            // 旧版日志的 Thinking 事件是恢复 DeepSeek tool-call 协议的唯一来源。
            // 新版同时有 Assistant(reasoning) 会在上方去重。
            SessionEvent::Thinking { text, .. } => pending_reasoning.push_str(text),
            // Step* / PlanUpdate 仅 UI 展示，不进入模型上下文。
            _ => {}
        }
    }
    // 旧日志防御：合并后的 assistant 文本可能残留 DSML 裸标记，剥离后再发给模型。
    for m in messages.iter_mut() {
        if m.role == Role::Assistant {
            m.content = harness_llm::dsml::strip_dsml(&m.content);
        }
    }
    // 协议净化：DeepSeek/OpenAI 要求 assistant 消息的每个 tool_call 后面必须紧跟
    // 同 call_id 的 tool 消息。取消/流错误/循环守卫等异常终止会在日志里留下
    // 「已宣告未执行」的 tool_call，直接发送即 HTTP 400；剔除无响应的 tool_call
    // 与孤儿 tool 消息，保证任何日志重建出的上下文都协议合法。
    let responded: std::collections::HashSet<String> = messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .filter_map(|m| m.tool_call_id.clone())
        .collect();
    let mut announced: std::collections::HashSet<String> = std::collections::HashSet::new();
    for m in messages.iter_mut() {
        if m.role == Role::Assistant {
            m.tool_calls.retain(|tc| responded.contains(&tc.id));
            for tc in &m.tool_calls {
                announced.insert(tc.id.clone());
            }
        }
    }
    messages.retain(|m| match m.role {
        Role::Tool => m
            .tool_call_id
            .as_deref()
            .is_some_and(|id| announced.contains(id)),
        // 剥离 tool_calls 后内容为空的 assistant 消息无信息量，且部分服务端拒收。
        Role::Assistant => !(m.content.is_empty() && m.tool_calls.is_empty()),
        _ => true,
    });
    apply_context_budget(compress_stale_tool_results(messages))
}

/// 把本步 assistant 放在该步产生的 Tool 结果和控制器 follow-up 之前。
///
/// `step_boundary` 在请求模型前采集，因此不依赖本步实际产生了多少条 Tool/user
/// 消息；即使门禁、并发去重或概念覆盖提醒额外追加消息，协议顺序仍保持为
/// assistant(tool_calls) -> tool results -> follow-ups。
fn insert_assistant_at_step_boundary(
    messages: &mut Vec<Message>,
    step_boundary: usize,
    assistant: Message,
) {
    messages.insert(step_boundary.min(messages.len()), assistant);
}

/// 陈旧工具结果渐进压缩：仅最近 `RECENT_FULL` 条工具输出保留完整（已被
/// `compress_tool_context` 限长）正文，更早的输出收缩为首部摘录。
/// 取证：单步上下文不算大，但上千步 replay 堆叠的旧探索输出会淹没当前目标，
/// 模型反复重读同一文件（最高 18 次）。在保留近期交付所需细节的同时
/// 压缩 prompt 体积、把注意力拉回当前目标；真正需要旧输出时可重新调用工具。
fn compress_stale_tool_results(mut messages: Vec<Message>) -> Vec<Message> {
    const RECENT_FULL: usize = 12;
    const STALE_EXCERPT: usize = 300;
    let tool_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == Role::Tool)
        .map(|(i, _)| i)
        .collect();
    let stale_count = tool_indices.len().saturating_sub(RECENT_FULL);
    for &idx in tool_indices.iter().take(stale_count) {
        let message = &mut messages[idx];
        if message.content.chars().count() > STALE_EXCERPT {
            let excerpt: String = message.content.chars().take(STALE_EXCERPT).collect();
            message.content =
                format!("{excerpt}\n…[较早的工具输出已节选；如需完整内容请重新调用工具获取]");
        }
    }
    messages
}

/// 工具原文对用户日志仍完整保留；送回模型时按层压缩，避免一次构建/搜索输出挤掉
/// 近期对话。短内容不变，长内容保留开头、错误附近和结尾。
/// 显式上传的附件是本回合输入条件。文本附件直接提供受限摘录；二进制附件保留
/// 文件名、MIME 与路径，要求 Agent 在用户授权范围内按需调用合适工具处理。
fn render_attachment_context(
    attachments: &[harness_core::Attachment],
    image_notes: &[String],
) -> String {
    if attachments.is_empty() {
        return String::new();
    }
    let mut out = String::from("[用户已附加以下文件；必须作为任务输入条件处理]\n");
    for attachment in attachments {
        let name = attachment
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("未命名文件");
        out.push_str(&format!(
            "- {name}（{}，路径：{}）",
            attachment.mime,
            attachment.path.display()
        ));
        let ext = attachment
            .path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(
            ext.as_str(),
            "txt" | "md" | "rs" | "toml" | "json" | "yaml" | "yml" | "csv" | "log" | "xml"
        ) {
            if let Ok(text) = std::fs::read_to_string(&attachment.path) {
                let excerpt: String = text.chars().take(8_000).collect();
                out.push_str(&format!("\n  文本摘录：\n{excerpt}"));
                if text.chars().count() > 8_000 {
                    out.push_str("\n  [摘录已截断，请按需读取文件]\n");
                }
            }
        } else {
            out.push_str("\n  [二进制或富媒体附件：请基于文件类型和任务要求处理，不得忽略]\n");
        }
        out.push('\n');
    }
    if !image_notes.is_empty() {
        out.push_str("[图片处理状态]\n");
        for note in image_notes {
            out.push_str("- ");
            out.push_str(note);
            out.push('\n');
        }
    }
    out
}

/// 单张图片上限，防止一次粘贴把原图 base64 膨胀后挤爆模型上下文或请求体。
const MAX_INLINE_IMAGE_BYTES: u64 = 10 * 1024 * 1024;

fn attachment_prompt(input: &UserInput) -> String {
    if input.text.trim().is_empty() && !input.attachments.is_empty() {
        "请分析并说明我附上的附件内容；如包含图片，请识别其中可见的信息。若当前模型或文件类型不支持直接识别，请明确说明原因及下一步可行操作。".to_string()
    } else {
        input.text.clone()
    }
}

/// 把可识别的本地图片转换为 Provider 可发送的 data URL。
///
/// 视觉能力明确关闭时绝不上传原图，避免把图片传给文本模型并得到难以理解的 400；
/// 同时返回可见状态说明，保证用户不会误以为图片已经被识别。
fn inline_image_data_urls(
    attachments: &[harness_core::Attachment],
    vision_enabled: bool,
) -> (Vec<String>, Vec<String>) {
    let mut urls = Vec::new();
    let mut notes = Vec::new();
    for attachment in attachments {
        let Some(mime) = vision_image_mime(attachment) else {
            continue;
        };
        let name = attachment
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("未命名图片");
        if !vision_enabled {
            notes.push(format!("{name}：当前所选模型不支持图片识别，未发送原图。"));
            continue;
        }
        let bytes = match std::fs::read(&attachment.path) {
            Ok(bytes) if bytes.len() as u64 <= MAX_INLINE_IMAGE_BYTES => bytes,
            Ok(bytes) => {
                notes.push(format!(
                    "{name}：图片大小 {} MiB，超过 {} MiB 上限，未发送原图。",
                    bytes.len() / 1024 / 1024,
                    MAX_INLINE_IMAGE_BYTES / 1024 / 1024
                ));
                continue;
            }
            Err(error) => {
                notes.push(format!("{name}：无法读取图片（{error}），未发送原图。"));
                continue;
            }
        };
        urls.push(format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ));
        notes.push(format!("{name}：已作为图片内容发送给视觉模型。"));
    }
    (urls, notes)
}

fn vision_image_mime(attachment: &harness_core::Attachment) -> Option<&'static str> {
    match attachment
        .path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

fn compress_tool_context(content: &str) -> String {
    // 6k→8k→16k：旧阈值对编译/测试类长输出裁得过狠，模型拿不到关键尾部
    // （错误栈通常在末尾）而重复调用工具；取证进一步发现 8k 会把 fs 区间读
    // 的代码内容截掉大半，模型看不到目标区域转而自造截取脚本；16k 与
    // fs 单次区间读上限（250 行）匹配，保证按需读取的内容完整可达。
    const LIMIT: usize = 16_000;
    if content.chars().count() <= LIMIT {
        return content.into();
    }
    let lines: Vec<&str> = content.lines().collect();
    let mut selected: Vec<&str> = lines.iter().take(36).copied().collect();
    selected.extend(
        lines
            .iter()
            .filter(|line| {
                let lower = line.to_ascii_lowercase();
                lower.contains("error") || lower.contains("failed") || lower.contains("warning")
            })
            .take(24)
            .copied(),
    );
    selected.extend(lines.iter().rev().take(28).rev().copied());
    selected.dedup();
    let mut out = format!("[工具输出已压缩：原 {} 字符]\n", content.chars().count());
    for line in selected {
        if out.chars().count() + line.chars().count() + 1 > LIMIT {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// 确定性的上下文预算器：保留系统提示和最近完整回合，较早对话压成短摘要。
/// 从 User 边界裁剪，避免拆开 assistant tool_call / tool result 协议对。
fn apply_context_budget(messages: Vec<Message>) -> Vec<Message> {
    // UI「参数配置」显式设置优先；其次 `HARNESS_CONTEXT_MAX_CHARS` 环境变量；最后默认 96k 字符。
    // 旧默认 48k 对长工具会话裁剪过激：早期证据被压成短摘录后模型会重新读取同样
    // 的文件/命令输出，多余往返的耗时与 token 远超保留上下文的成本。
    let budget = harness_core::tuning::context_budget_chars()
        .or_else(|| {
            std::env::var("HARNESS_CONTEXT_MAX_CHARS")
                .ok()
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or(96_000usize)
        .clamp(12_000, 240_000);
    let total: usize = messages.iter().map(message_chars).sum();
    if total <= budget {
        return messages;
    }

    let user_starts: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| (message.role == Role::User).then_some(index))
        .collect();
    let Some(&latest_user) = user_starts.last() else {
        return messages;
    };
    let mut start = latest_user;
    let mut retained: usize = messages[start..].iter().map(message_chars).sum();
    for &candidate in user_starts.iter().rev().skip(1) {
        let added: usize = messages[candidate..start].iter().map(message_chars).sum();
        if retained.saturating_add(added) > budget.saturating_sub(4_000) {
            break;
        }
        start = candidate;
        retained += added;
    }
    if start <= 1 {
        return messages;
    }

    let mut result: Vec<Message> = messages[..start]
        .iter()
        .filter(|message| message.role == Role::System)
        .cloned()
        .collect();
    let omitted = start.saturating_sub(result.len());
    let mut summary = format!("[较早会话已按上下文预算压缩，共省略 {omitted} 条消息]\n");
    for message in messages[1..start].iter().rev() {
        if !matches!(message.role, Role::User | Role::Assistant)
            || message.content.trim().is_empty()
        {
            continue;
        }
        let role = if message.role == Role::User {
            "用户"
        } else {
            "助手"
        };
        let compact = message
            .content
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let excerpt: String = compact.chars().take(480).collect();
        let line = format!("{role}: {excerpt}\n");
        if summary.chars().count() + line.chars().count() > 4_000 {
            break;
        }
        summary.push_str(&line);
    }
    result.push(Message::system(summary));
    result.extend(messages.into_iter().skip(start));
    result
}

/// prompt 窗口续期只保留本回合仍有效的系统约束和执行状态检查点。
///
/// 旧工具原文已经折叠进 `ExecutionState` 的证据摘要；继续携带它们不仅会再次消耗
/// 近一个完整上下文，还会让模型回头重做已经完成的定位。历史压缩摘要和旧续期断点
/// 也不再保留，防止每次续期递归叠加。
fn compact_for_prompt_renewal(
    messages: Vec<Message>,
    checkpoint: &str,
    renewal: u32,
    max_renewals: u32,
) -> Vec<Message> {
    let mut seen = HashSet::new();
    let mut compacted = messages
        .into_iter()
        .filter(|message| {
            message.role == Role::System
                && !message.content.starts_with("[较早会话已按上下文预算压缩")
                && !message.content.starts_with("[预算窗口续期")
        })
        .filter(|message| seen.insert(message.content.clone()))
        .collect::<Vec<_>>();
    compacted.push(Message::system(format!(
        "[预算窗口续期 {renewal}/{max_renewals}·最小断点]\n{checkpoint}\n旧工具输出已从请求上下文移除，但其证据结论仍有效。不要重新扫描缓存、依赖或生成目录；直接推进第一个未满足验收项。"
    )));
    compacted.push(Message::user(
        "自动续期继续执行：若已有足够证据就完成修改与验证；若没有新增信息，立即基于现有证据收尾，不要重复探索。",
    ));
    compacted
}

/// 空响应恢复与普通预算续期不同：它必须把导致空响应的对话/工具历史全部移除，
/// 但保留本回合系统约束、目标图和运行时检查点。这样重试请求在语义上连续，字节上
/// 却不是原请求重放；已有写入与证据也不会丢失或被重复执行。
fn compact_for_empty_recovery(
    messages: Vec<Message>,
    checkpoint: &str,
    goal_state: &str,
    reason: &str,
    attempt: usize,
    max_attempts: usize,
) -> Vec<Message> {
    let mut seen = HashSet::new();
    let mut compacted = messages
        .into_iter()
        .filter(|message| {
            message.role == Role::System
                && !message.content.starts_with("[较早会话已按上下文预算压缩")
                && !message.content.starts_with("[预算窗口续期")
                && !message.content.starts_with("[空响应恢复")
                && !message.content.starts_with("[V4 唯一目标求解图")
        })
        .filter(|message| seen.insert(message.content.clone()))
        .collect::<Vec<_>>();
    compacted.push(Message::system(format!(
        "[空响应恢复 {attempt}/{max_attempts}·最小快照]\nfinish_reason={reason}\n{checkpoint}\n{goal_state}\n旧对话与工具原文已移除，运行时记录的证据和阶段仍有效。禁止重新规划、重复搜索或扩大范围。"
    )));
    compacted.push(Message::user(
        "继续当前唯一下一动作：需要执行时只返回一个当前阶段允许的工具调用；证据已经充分时给出简短、可验证的最终答复。不得只输出思考过程。",
    ));
    compacted
}

fn message_chars(message: &Message) -> usize {
    message.content.chars().count()
        + message
            .tool_calls
            .iter()
            .map(|call| call.name.len() + call.args.to_string().chars().count())
            .sum::<usize>()
}

/// 给工具错误打上互斥来源标签，避免模型把 edit 文本冲突、普通 IO 错误和真正的
/// 沙箱拒绝混为一谈。用户与模型都能据此审计失败来自哪一层。
fn format_tool_dispatch_error(error: harness_core::Error) -> String {
    match error {
        harness_core::Error::SandboxDenied(reason) => format!("[sandbox denied] {reason}"),
        harness_core::Error::Io(error) => format!("[tool io error] {error}"),
        other => format!("[tool error] {other}"),
    }
}

/// 检查模型正文中的运行时事实声明。这里只拦截可由 Runtime 确定真假的两类说法：
/// “沙箱/权限拒绝”必须有对应 ToolResult，“已落实/已写入”必须有成功写操作计数。
fn unsupported_runtime_claim_correction(
    text: &str,
    change_required: bool,
    write_operations: usize,
    completion_ready: bool,
    sandbox_denial_observed: bool,
    access_denial_observed: bool,
) -> Option<String> {
    let compact = text.split_whitespace().collect::<String>().to_lowercase();
    let claims_sandbox_denial = (compact.contains("沙箱") || compact.contains("sandbox"))
        && ["拦截", "拒绝", "阻止", "无法写", "denied", "blocked"]
            .iter()
            .any(|marker| compact.contains(marker));
    let claims_access_denial = [
        "权限拒绝",
        "权限拦截",
        "没有写入权限",
        "无写入权限",
        "permissiondenied",
    ]
    .iter()
    .any(|marker| compact.contains(marker));
    let claims_change_applied = [
        "已完成",
        "已经落实",
        "已落实修改",
        "修改已完成",
        "代码已写入",
        "已经写入代码",
        "成功落盘",
    ]
    .iter()
    .any(|marker| compact.contains(marker));

    let mut facts = Vec::new();
    if claims_sandbox_denial && !sandbox_denial_observed {
        facts.push("没有任何带 [sandbox denied] 标签的工具结果，不能归因为沙箱拦截");
    }
    if claims_access_denial && !access_denial_observed {
        facts.push("没有任何带 [access-policy denied] 标签的工具结果，不能归因为访问权限拒绝");
    }
    if change_required && claims_change_applied {
        if write_operations == 0 {
            facts.push("当前成功写操作计数为 0，不能声称修改已经落实或代码已经落盘");
        } else if !completion_ready {
            facts.push("虽然已有部分写操作，但全部验收项尚未完成并验证，不能声称整个任务已完成");
        }
    }
    (!facts.is_empty()).then(|| facts.join("；"))
}

/// 结构化系统提示词：语言跟随、工具契约、长周期工作流、安全边界。
const SYSTEM_PROMPT: &str = "You are a reliable desktop assistant and coding agent.\n\
\n\
## 语言与格式\n\
- 始终用与用户最新消息相同的语言回复（中文提问用中文答）。\n\
- 结论先行；需要时用简洁的 markdown 列表展开步骤，不写冗长铺垫。\n\
\n\
## 工具契约\n\
- 只允许使用提供给你的工具：fs / edit / shell / search / plan / delegate。\n\
- 定位代码/文本位置一律优先用 search（一次调用返回文件:行号:内容）；严禁用 shell findstr/dir/grep 全仓扫描或编写临时扫描脚本来找代码。\n\
- 严禁在正文里输出任何形式的工具调用标记（DSML、XML invoke、tool_calls 文本等）；调用工具必须走 function calling 通道。\n\
- 问候、提问、普通对话直接回答，不使用工具。\n\
- 不得虚构沙箱、权限、网络或工具失败原因；只有对应 ToolResult 明确返回时才能引用。execution gate、old_text 失配和 sandbox denied 是不同故障，必须按原始标签准确陈述。\n\
- 变更任务只有成功执行写工具并获得验证后才能说“已落实/已修改/已写入”；只给方案、代码块或修改建议不等于落盘。\n\
\n\
## 复杂任务工作流\n\
- 只有多个独立交付物、范围不明确或高风险任务才调用 plan；单点、范围明确的修复直接执行“定位 → 修改 → 验证”，不要为计划本身增加往返。\n\
- 相互独立的多个操作尽量在同一次回复里作为多个工具调用一起发出，减少往返。\n\
- 独立且耗时的子任务用 delegate 委托子代理，主线程只整合结果。\n\
- 回合结束前给出简洁、可读的最终总结。\n\
\n\
## 效率与收敛（必须遵守）\n\
- 最小路径优先：先用 search 定位与目标直接相关的最小文件集，再有针对性地读取；禁止全仓库递归扫描、批量试探性搜索与自造临时扫描脚本。\n\
- 读取大文件用 fs 的 start_line/end_line 按区间读取；严禁编写临时脚本（python/ps1 等）截取或提取文件内容。\n\
- 纯界面/配置类任务：search 定位到少数目标文件后集中批量编辑，全部改完再做一次构建验证，不要每改一处就编译一次。\n\
- 已读过的文件不要重复读取；成功的同一命令/同一参数不得原样重试，失败调用只允许一次带明确原因的定向重试。search 有命中后，下一次读取必须使用命中的路径和最小行区间；下一次搜索必须缩小目录或验证不同假设。\n\
- 探索（读取/搜索/列目录）应尽快收敛到写操作与验证；交付目标达成后立即停止，不做重复确认与打磨。\n\
- 步数预算有限且续期次数封顶：收到检查点/收尾提示时必须服从，基于现有证据交付总结，不要继续扩张探索。\n\
- shell 命令已在工作区根目录执行：不要重复 cd 到工作区，直接用相对路径。\n\
- 编译验证按 [项目事实] 给出的命令模板一次到位；命令失败后先读全量错误文本再换路一次，禁止用试错方式探索环境（manifest 位置、工具链、目录结构）。\n\
\n\
## 安全\n\
- 仅当用户明确要求检查/修改/构建/测试/操作工作区时才使用文件系统或 shell 工具。\n\
- 永不搜索或泄露 API key、凭据、token 等秘密。";

/// 进展检查间隔（沿用 env `HARNESS_MAX_STEPS` 保持兼容，默认 128）。
/// 这不是完成期限；到点后诊断调用价值并自动续期。失控由循环守卫、取消与 turn timeout 兜底。
fn max_steps_limit() -> usize {
    // UI 显式设置优先；其次兼容旧环境变量；最后默认 128。
    harness_core::tuning::max_steps()
        .or_else(|| {
            std::env::var("HARNESS_MAX_STEPS")
                .ok()
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(128)
        .max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v4_executor_has_an_explicit_legacy_fallback() {
        assert!(parse_goal_executor_mode(None));
        assert!(parse_goal_executor_mode(Some("v4")));
        assert!(!parse_goal_executor_mode(Some("legacy")));
        assert!(!parse_goal_executor_mode(Some("off")));
    }
    use harness_capability::assets::Skill;
    use harness_llm::ToolCall;

    #[test]
    fn renders_matched_skills_as_compact_system_instructions() {
        let skills = vec![Skill {
            id: "review".into(),
            name: "Code review".into(),
            version: "1.0".into(),
            trigger_boundary: "review code".into(),
            steps: vec!["inspect diff".into(), "run tests".into()],
            verification_rules: vec!["findings recorded".into()],
            resource_files: vec![],
            confidence: 1.0,
            enabled: true,
            source_path: String::new(),
        }];
        let rendered = render_skill_instructions(&skills).expect("matched skill should render");
        assert!(rendered.contains("Code review"));
        assert!(rendered.contains("inspect diff；run tests"));
        assert!(rendered.contains("findings recorded"));
        assert!(render_skill_instructions(&[]).is_none());
    }

    #[test]
    fn retrieval_only_injects_verified_solve_cards_as_untrusted_hints() {
        let cards = vec![
            MemoryFact {
                id: "solve-card:abc".into(),
                kind: FactKind::Decision,
                content: "[SolveCard]\n问题：刷新失败\n结果：已验证交付。".into(),
                layer: LifecycleLayer::L2,
                confidence: 0.9,
                source: "solve-card".into(),
            },
            MemoryFact {
                id: "ordinary-fact".into(),
                kind: FactKind::Fact,
                content: "这不是可执行经验".into(),
                layer: LifecycleLayer::L2,
                confidence: 0.8,
                source: "test".into(),
            },
        ];
        let rendered = render_experience(&cards).expect("solve card should be injected");
        assert!(rendered.contains("刷新失败"));
        assert!(rendered.contains("候选线索"));
        assert!(!rendered.contains("这不是可执行经验"));
    }

    /// 全链路 Demo 回归（约定目录 + 自动加载）：外部技能包落盘进约定目录 →
    /// `sync_skill_packs` 自动注册但**默认未启用** → 用户勾选启用后中文语境
    /// match_skills 命中 → render_skill_instructions 产出含触发边界/步骤/验证的系统
    /// 指令（AgentLoop 每回合写入模型上下文的内容）→ 禁用后立即不再命中，
    /// 重复同步更新内容且不产生副本（全局开关即时生效）。
    #[tokio::test]
    async fn imported_skill_pack_flows_into_agent_context() {
        use harness_capability::index;
        use harness_provider_memory::NativeSkillLibrary;

        // 1) 构造外部来源技能包（SKILL.md + 资源文件），等价于 extensions/skill-packs 示例。
        let dir = std::env::temp_dir().join(format!(
            "harness-skill-e2e-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let src = dir.join("src");
        let pack = src.join("release-checklist");
        std::fs::create_dir_all(pack.join("resources")).unwrap();
        std::fs::write(
            pack.join("SKILL.md"),
            "# 发布检查清单\n\nversion: 1.1\n\n## 触发边界\n发布新版本、上线前需要检查清单\n\n## 执行步骤\n- 跑全量测试\n- 核对版本号\n\n## 验证规则\n- 测试全部通过\n",
        )
        .unwrap();
        std::fs::write(pack.join("resources").join("checklist.txt"), "ok").unwrap();

        // 2) 落盘进约定目录 + 自动加载对账（与 GUI 导入/启动扫描同一入口）。
        let lib = NativeSkillLibrary::new(&dir);
        let packs_dir = dir.join(".harness-memory").join("skills");
        let installed = index::install_skill_packs_from(&src, &packs_dir).unwrap();
        assert_eq!(installed.len(), 1);
        let report = index::sync_skill_packs(&*lib, &packs_dir).await.unwrap();
        assert_eq!(report.added, 1);

        // 3) 默认未启用：勾选前不得参与匹配（新安全语义）。
        assert!(
            lib.match_skills("发布新版本").await.unwrap().is_empty(),
            "自动加载的技能默认不得参与匹配"
        );

        // 4) 用户在面板勾选启用后，中文自然语言输入必须命中
        //（CJK n-gram 匹配，回归「占坑不拉」缺陷）。
        let id = format!("{}release-checklist", index::SKILL_PACK_ID_PREFIX);
        lib.set_skill_enabled(&id, true).await.unwrap();
        let matched = lib
            .match_skills("明天要发布新版本，帮我过一遍检查")
            .await
            .unwrap();
        assert!(!matched.is_empty(), "发布场景中文输入未命中任何技能");
        let sk = matched
            .iter()
            .find(|s| s.name.contains("发布检查清单"))
            .expect("应命中发布检查清单技能");
        assert_eq!(sk.version, "1.1");
        assert_eq!(
            sk.resource_files,
            vec!["resources/checklist.txt".to_string()]
        );

        // 5) AgentLoop 每回合注入模型上下文的即此渲染结果：
        // 触发边界与执行步骤、验证规则都必须在场。
        let instructions = render_skill_instructions(&matched).expect("命中技能应渲染出上下文指令");
        assert!(instructions.contains("发布检查清单"));
        assert!(instructions.contains("发布新版本"));
        assert!(instructions.contains("跑全量测试"));
        assert!(instructions.contains("测试全部通过"));

        // 6) 禁用 → 立即不再命中：GUI 开关经同一 SkillLibrary 全局同步。
        lib.set_skill_enabled(&id, false).await.unwrap();
        assert!(lib.match_skills("发布新版本").await.unwrap().is_empty());

        // 7) 重复同步（更新内容）：不产生副本，禁用状态不被静默打开。
        let rep2 = index::sync_skill_packs(&*lib, &packs_dir).await.unwrap();
        assert_eq!(rep2.added, 0);
        assert_eq!(rep2.updated, 1);
        let all = lib.list_skills().await.unwrap();
        assert_eq!(all.len(), 1, "重复导入不得创建副本");
        assert!(!all[0].enabled, "重新同步不得覆盖用户的禁用状态");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tool_context_keeps_errors_and_bounds_long_output() {
        let input = format!(
            "{}\nerror: expected failure\n{}",
            "first line\n".repeat(2_000),
            "last line\n".repeat(2_000)
        );
        let compacted = compress_tool_context(&input);
        assert!(compacted.chars().count() <= 16_000);
        assert!(compacted.contains("error: expected failure"));
        assert!(compacted.contains("工具输出已压缩"));
    }

    #[test]
    fn attachment_only_input_becomes_an_explicit_analysis_request() {
        let prompt = attachment_prompt(&UserInput {
            text: String::new(),
            attachments: vec![harness_core::Attachment {
                path: "screenshot.png".into(),
                mime: "image/png".into(),
            }],
        });
        assert!(prompt.contains("分析"));
        assert!(prompt.contains("图片"));
    }

    #[test]
    fn inline_image_data_urls_only_include_supported_images() {
        let dir = std::env::temp_dir().join(format!(
            "harness-attachment-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let image = dir.join("image.png");
        std::fs::write(&image, [1_u8, 2, 3]).unwrap();
        let attachments = vec![harness_core::Attachment {
            path: image,
            mime: "image/png".into(),
        }];

        let (urls, notes) = inline_image_data_urls(&attachments, true);
        assert_eq!(urls.len(), 1);
        assert!(urls[0].starts_with("data:image/png;base64,"));
        assert!(notes[0].contains("已作为图片内容发送"));

        let (urls, notes) = inline_image_data_urls(&attachments, false);
        assert!(urls.is_empty());
        assert!(notes[0].contains("不支持图片识别"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rebuilds_multi_turn_and_tool_context() {
        let events = vec![
            SessionEvent::TurnStart {
                id: 1,
                input: "第一问".into(),
            },
            // 旧版只写 Thinking，因此也必须能恢复。
            SessionEvent::Thinking {
                id: 2,
                text: "先读取文件。".into(),
            },
            // DeepSeek may send reasoning_content in its own SSE frame before
            // the tool-call frame; 新版会再保存在 Assistant chunk，不能重复。
            SessionEvent::Assistant {
                id: 3,
                chunk: Chunk {
                    reasoning: Some("先读取文件。".into()),
                    ..Default::default()
                },
            },
            SessionEvent::Assistant {
                id: 4,
                chunk: Chunk {
                    text: Some("处理中".into()),
                    tool_calls: vec![ToolCall {
                        id: "c1".into(),
                        name: "fs".into(),
                        args: serde_json::json!({"op":"read","path":"a.txt"}),
                    }],
                    ..Default::default()
                },
            },
            SessionEvent::ToolResult {
                id: 5,
                result: ToolResult {
                    call_id: "c1".into(),
                    ok: true,
                    content: "内容".into(),
                    continuation_debt: 0,
                },
            },
            SessionEvent::TurnEnd { id: 6 },
        ];
        let messages = messages_from_events(&events);
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[1].role, Role::User);
        assert_eq!(messages[2].tool_calls[0].id, "c1");
        assert_eq!(
            messages[2].reasoning_content.as_deref(),
            Some("先读取文件。")
        );
        assert_eq!(messages[3].tool_call_id.as_deref(), Some("c1"));
    }

    #[test]
    fn sanitizes_unresponded_tool_calls_and_orphan_results() {
        // 模拟循环守卫/中断后的日志：c2 已宣告但从未执行，另有一条孤儿 tool 结果。
        let events = vec![
            SessionEvent::TurnStart {
                id: 1,
                input: "q".into(),
            },
            SessionEvent::Assistant {
                id: 2,
                chunk: Chunk {
                    text: Some("执行中".into()),
                    tool_calls: vec![
                        ToolCall {
                            id: "c1".into(),
                            name: "shell".into(),
                            args: serde_json::json!({"command":"dir"}),
                        },
                        ToolCall {
                            id: "c2".into(),
                            name: "shell".into(),
                            args: serde_json::json!({"command":"dir"}),
                        },
                    ],
                    ..Default::default()
                },
            },
            SessionEvent::ToolResult {
                id: 3,
                result: ToolResult {
                    call_id: "c1".into(),
                    ok: true,
                    content: "ok".into(),
                    continuation_debt: 0,
                },
            },
            SessionEvent::ToolResult {
                id: 4,
                result: ToolResult {
                    call_id: "ghost".into(),
                    ok: true,
                    content: "孤儿结果".into(),
                    continuation_debt: 0,
                },
            },
            SessionEvent::TurnEnd { id: 5 },
        ];
        let messages = messages_from_events(&events);
        // system + user + assistant（仅保留有响应的 c1）+ tool(c1)，孤儿结果被剔除。
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[2].tool_calls.len(), 1);
        assert_eq!(messages[2].tool_calls[0].id, "c1");
        assert_eq!(messages[3].tool_call_id.as_deref(), Some("c1"));
    }

    #[test]
    fn assistant_tool_declaration_stays_before_results_when_followups_are_appended() {
        let mut messages = vec![Message::system("system"), Message::user("change it")];
        let step_boundary = messages.len();
        messages.push(Message::tool("call-1", "ok"));
        messages.push(Message::user("[控制器提醒] 继续验证"));

        insert_assistant_at_step_boundary(
            &mut messages,
            step_boundary,
            Message::assistant_with_tools(
                "",
                vec![ToolCall {
                    id: "call-1".into(),
                    name: "fs".into(),
                    args: serde_json::json!({"op":"read","path":"a.rs"}),
                }],
            ),
        );

        assert_eq!(messages[2].role, Role::Assistant);
        assert_eq!(messages[2].tool_calls[0].id, "call-1");
        assert_eq!(messages[3].tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(messages[4].role, Role::User);
    }

    #[test]
    fn incomplete_delivery_names_only_the_missing_step() {
        let status = concise_incomplete_status(
            &DeliveryOutcome::PartialDelivery,
            Some("已有修改，但尚未获得覆盖全部验收项的验证证据"),
            1,
            1,
            crate::goal_execution::SolvePhase::Conclude,
        );
        assert_eq!(
            status,
            "未完成：缺少“验证”步骤。\n下一步：运行与本次修改相关的最小构建或测试。"
        );
        for internal_label in ["【资产】", "锚点：", "假设：", "补丁建议：", "问项："]
        {
            assert!(!status.contains(internal_label));
        }
    }

    #[test]
    fn provider_failure_is_summarized_without_dumping_gateway_details() {
        let status = concise_incomplete_status(
            &DeliveryOutcome::SystemFailure,
            Some("llm provider error: HTTP 400 invalid_tool_call_history messages[9]"),
            0,
            0,
            crate::goal_execution::SolvePhase::Change,
        );
        assert!(status.contains("模型服务"));
        assert!(!status.contains("messages[9]"));
        assert!(!status.contains("HTTP 400"));
    }

    #[test]
    fn context_budget_keeps_latest_turn_and_compacts_old_history() {
        let mut messages = vec![Message::system("system")];
        for index in 0..14 {
            messages.push(Message::user(format!("old-{index}-{}", "x".repeat(8_000))));
            messages.push(Message::assistant(format!("answer-{index}")));
        }
        messages.push(Message::user("LATEST-QUESTION"));
        let compacted = apply_context_budget(messages);
        assert!(compacted
            .iter()
            .any(|m| m.content.contains("较早会话已按上下文预算压缩")));
        assert!(compacted.iter().any(|m| m.content == "LATEST-QUESTION"));
        assert!(compacted.iter().map(message_chars).sum::<usize>() < 105_000);
    }

    #[test]
    fn prompt_renewal_keeps_constraints_but_drops_tool_history_and_old_summaries() {
        let messages = vec![
            Message::system("system-contract"),
            Message::system("system-contract"),
            Message::system("[较早会话已按上下文预算压缩，共省略 99 条消息]"),
            Message::user("old request"),
            Message::assistant("old answer"),
            Message::tool("call-1", "very large tool result"),
        ];
        let compacted = compact_for_prompt_renewal(messages, "目标：修复问题", 1, 4);
        assert_eq!(
            compacted
                .iter()
                .filter(|message| message.content == "system-contract")
                .count(),
            1,
            "重复系统约束应去重"
        );
        assert!(compacted.iter().all(|message| message.role != Role::Tool));
        assert!(compacted
            .iter()
            .all(|message| !message.content.contains("较早会话已按上下文预算压缩")));
        assert!(compacted
            .iter()
            .any(|message| message.content.contains("目标：修复问题")));
    }

    #[test]
    fn empty_recovery_replaces_the_failed_prompt_with_one_resumable_snapshot() {
        let messages = vec![
            Message::system("system-contract"),
            Message::system("[V4 唯一目标求解图]\n旧状态"),
            Message::user("old request"),
            Message::assistant("old answer"),
            Message::tool("call-1", "very large tool result"),
        ];
        let compacted = compact_for_empty_recovery(
            messages,
            "目标：修改菜单文字\n下一步：读取 composer.rs",
            "[V4 唯一目标求解图]\n新状态",
            "stop",
            1,
            1,
        );
        assert!(compacted.iter().all(|message| message.role != Role::Tool));
        assert!(compacted
            .iter()
            .all(|message| !message.content.contains("old answer")));
        assert!(compacted
            .iter()
            .all(|message| !message.content.contains("旧状态")));
        let snapshot = compacted
            .iter()
            .find(|message| message.content.contains("[空响应恢复 1/1·最小快照]"))
            .expect("应生成一次最小恢复快照");
        assert!(snapshot.content.contains("finish_reason=stop"));
        assert!(snapshot.content.contains("读取 composer.rs"));
        assert!(snapshot.content.contains("新状态"));
    }

    #[test]
    fn tool_errors_keep_sandbox_and_io_provenance_distinct() {
        let sandbox = format_tool_dispatch_error(harness_core::Error::SandboxDenied(
            "path is outside workspace".into(),
        ));
        assert!(sandbox.starts_with("[sandbox denied]"), "{sandbox}");

        let io = format_tool_dispatch_error(harness_core::Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "old_text must match exactly once",
        )));
        assert!(io.starts_with("[tool io error]"), "{io}");
        assert!(!io.contains("sandbox"), "{io}");
    }

    #[test]
    fn unsupported_runtime_claims_require_structured_evidence() {
        let correction = unsupported_runtime_claim_correction(
            "此前因为沙箱拦截，但三项修改已经落实。",
            true,
            0,
            false,
            false,
            false,
        )
        .expect("无证据沙箱归因和零写入完成声明都必须被拦截");
        assert!(correction.contains("不能归因为沙箱拦截"), "{correction}");
        assert!(correction.contains("成功写操作计数为 0"), "{correction}");

        assert!(unsupported_runtime_claim_correction(
            "沙箱明确拒绝了写入，但修改随后已经落实。",
            true,
            1,
            true,
            true,
            false,
        )
        .is_none());

        let correction = unsupported_runtime_claim_correction(
            "已完成输入框高度与内边距的紧凑化调整。",
            true,
            0,
            false,
            false,
            false,
        )
        .expect("‘已完成某项修改’也必须要求成功写操作");
        assert!(correction.contains("成功写操作计数为 0"), "{correction}");

        let correction = unsupported_runtime_claim_correction(
            "代码已经写入，任务已完成。",
            true,
            1,
            false,
            false,
            false,
        )
        .expect("只有部分写入、验收未闭环时不得宣布完成");
        assert!(correction.contains("全部验收项尚未完成"), "{correction}");
    }

    #[test]
    fn repeat_guard_blocks_success_retries_but_allows_retry_after_a_write() {
        let mut guard = ToolRepeatGuard::default();
        let sig = "shell:{\"cmd\":\"status\"}";
        let success = ToolResult {
            call_id: "c1".into(),
            ok: true,
            content: "still running".into(),
            continuation_debt: 0,
        };
        // 成功读取后，原样调用不会获得新信息，下一次调用必须被拦截。
        guard.record_result(sig, &success);
        assert!(guard.should_block(sig));

        // 成功写入改变观察对象，允许重新运行同一验证命令。
        let write = ToolResult {
            call_id: "edit-1".into(),
            ok: true,
            content: "updated".into(),
            continuation_debt: 0,
        };
        guard.record_result("edit:{\"path\":\"src/app.rs\"}", &write);
        assert!(!guard.should_block(sig));

        // 失败调用允许一次定向重试；第二次失败后不再重试。
        let failed = ToolResult {
            call_id: "c2".into(),
            ok: false,
            content: "temporary failure".into(),
            continuation_debt: 0,
        };
        guard.record_result(sig, &failed);
        assert!(!guard.should_block(sig));
        guard.record_result(sig, &failed);
        assert!(guard.should_block(sig));
        assert_eq!(guard.note_recovery(sig), 1);

        // 不同签名的调用不受影响。
        assert!(!guard.should_block("shell:{\"cmd\":\"other\"}"));
    }

    #[test]
    fn controlled_locate_gate_allows_only_one_search_per_model_response() {
        let mut gate = LocateStepGate::default();
        assert!(gate.allows(true, "search:{\"pattern\":\"optimizing\"}"));
        assert!(!gate.allows(true, "search:{\"pattern\":\"loading\"}"));
        // 读取和写入不是定位泛扫；它们由跨步骤 ActionGate 判断是否符合阶段。
        assert!(gate.allows(true, "fs:{\"op\":\"read\"}"));
        assert!(gate.allows(false, "search:{\"pattern\":\"anything\"}"));
    }

    #[test]
    fn continuation_recovers_the_root_task_instead_of_reclassifying_the_short_command() {
        let root_report = DeliveryReport {
            outcome: DeliveryOutcome::Blocked,
            criteria: vec![harness_session::DeliveryCriterion {
                id: "item-1".into(),
                description: "迁移旧配置".into(),
                satisfied: true,
                evidence: vec!["migration test passed".into()],
            }],
            verification: vec!["migration test passed".into()],
            reason: Some("预算窗口结束".into()),
        };
        // 新版续跑回合虽然记录了用户的短句，但 DeliveryReport 仍对应根任务的
        // 验收项；下一次续跑必须继承这个最新报告，而不是退回首轮报告。
        let generic_resume_report = DeliveryReport {
            outcome: DeliveryOutcome::Blocked,
            criteria: vec![harness_session::DeliveryCriterion {
                id: "item-1".into(),
                description: "迁移旧配置".into(),
                satisfied: true,
                evidence: vec!["migration test passed again".into()],
            }],
            verification: vec!["migration test passed again".into()],
            reason: Some("下一阶段预算窗口结束".into()),
        };
        let events = vec![
            SessionEvent::TurnStart {
                id: 1,
                input: "迁移配置中心\n- 迁移旧配置\n- 验证 CLI".into(),
            },
            SessionEvent::Delivery {
                id: 2,
                report: root_report,
            },
            SessionEvent::TurnEnd { id: 3 },
            SessionEvent::TurnStart {
                id: 4,
                input: "继续把第一阶段做完".into(),
            },
            SessionEvent::Delivery {
                id: 5,
                report: generic_resume_report,
            },
        ];

        let resumed = latest_resumable_task(&events).expect("should find root task");
        assert!(resumed.objective.starts_with("迁移配置中心"));
        assert_eq!(resumed.report.criteria[0].id, "item-1");
        assert_eq!(
            resumed.report.criteria[0].evidence,
            ["migration test passed again"]
        );
        assert!(resume_instruction(&resumed).contains("不要重新创建计划"));
    }

    #[test]
    fn execution_correction_recovers_the_original_unfinished_goal() {
        let incomplete = || DeliveryReport {
            outcome: DeliveryOutcome::PartialDelivery,
            criteria: vec![harness_session::DeliveryCriterion {
                id: "user-objective".into(),
                description: "文件树右键把文件加入附件".into(),
                satisfied: false,
                evidence: vec![],
            }],
            verification: vec![],
            reason: Some("尚未写入".into()),
        };
        let events = vec![
            SessionEvent::TurnStart {
                id: 1,
                input: "增加文件树右键菜单，把选定文件添加到对话框附件".into(),
            },
            SessionEvent::Delivery {
                id: 2,
                report: incomplete(),
            },
            SessionEvent::TurnEnd { id: 3 },
            SessionEvent::TurnStart {
                id: 4,
                input: "你只是列出来怎么修改，但是没有落实到真正的改动".into(),
            },
            SessionEvent::Delivery {
                id: 5,
                report: incomplete(),
            },
        ];

        assert!(is_resumable_follow_up("我提供不了，你自己执行修改。"));
        let resumed = latest_resumable_task(&events).expect("should recover original goal");
        assert_eq!(
            resumed.objective,
            "增加文件树右键菜单，把选定文件添加到对话框附件"
        );
    }

    #[test]
    fn natural_language_reply_after_clarification_keeps_the_root_task() {
        let events = vec![
            SessionEvent::TurnStart {
                id: 1,
                input: "这个有问题，帮我修一下".into(),
            },
            SessionEvent::Delivery {
                id: 2,
                report: DeliveryReport {
                    outcome: DeliveryOutcome::Blocked,
                    criteria: vec![],
                    verification: vec![],
                    reason: Some(format!("{CLARIFICATION_REASON_PREFIX}缺少具体对象")),
                },
            },
        ];

        assert!(awaiting_clarification(&events));
        assert_eq!(
            latest_resumable_task(&events).unwrap().objective,
            "这个有问题，帮我修一下"
        );
    }

    #[test]
    fn governor_mode_defaults_to_on_with_legacy_escape_hatch() {
        // 步骤④接管后默认控制器；HARNESS_GOVERNOR=legacy/off/0 是步骤⑤删除前的逃生门。
        assert_eq!(AgentLoop::new().governor_mode(), GovernorMode::On);
        assert_eq!(
            AgentLoop::new()
                .with_governor(GovernorMode::Legacy)
                .governor_mode(),
            GovernorMode::Legacy
        );
        // 解析函数独立可测，不依赖真实进程环境（edition 2024 下 env 写入是 unsafe）。
        assert_eq!(parse_governor_mode(None), GovernorMode::On);
        assert_eq!(parse_governor_mode(Some("")), GovernorMode::On);
        assert_eq!(parse_governor_mode(Some("legacy")), GovernorMode::Legacy);
        assert_eq!(parse_governor_mode(Some(" OFF ")), GovernorMode::Legacy);
        assert_eq!(parse_governor_mode(Some("0")), GovernorMode::Legacy);
        assert_eq!(parse_governor_mode(Some("on")), GovernorMode::On);
        assert_eq!(parse_governor_mode(Some(" 1 ")), GovernorMode::On);
        assert_eq!(parse_governor_mode(Some("TRUE")), GovernorMode::On);
    }

    #[test]
    fn ask_user_gate_blocks_questions_under_controller() {
        let case = CaseFile::default();
        // legacy（governor=None）一律允许——旧行为逐字保持。
        assert!(ask_user_permitted(
            None,
            &case,
            "改一下",
            "目标是谁（候选：a、b）"
        ));
        assert!(ask_user_permitted(None, &case, "继续", "目标是谁？"));

        // 控制器：栈顶不满足深度前置。
        let gov = TurnGovernor::new(&case, false, false);
        assert!(
            !ask_user_permitted(
                Some(&gov),
                &case,
                "这个问题解决了吗？",
                "目标是谁（候选：a、b）"
            ),
            "还有多层策略未试，禁止把负担丢回用户"
        );

        // 降到栈底后仍要拒续跑式回复；正常输入 + 带候选问题放行。
        let mut gov = TurnGovernor::new(&case, false, false);
        while !gov.stack.allow_ask_user() {
            gov.stack.pop();
        }
        assert!(!ask_user_permitted(
            Some(&gov),
            &case,
            "继续",
            "目标是谁（候选：a、b）"
        ));
        assert!(ask_user_permitted(
            Some(&gov),
            &case,
            "目标在哪",
            "目标是谁（候选：a、b）"
        ));
    }

    #[test]
    fn with_candidates_appends_workspace_candidates() {
        // GoalContract 无 Default（仅 compile），用 compile 后显式设字段。
        let mut goal = crate::GoalContract::compile("占位输入");
        goal.entities = vec!["GitCli".into(), "WorktreeGuard".into()];
        goal.navigation.clear();
        let q = with_candidates("工作区里没找到目标实体", &goal);
        assert!(q.contains("候选："), "{q}");
        assert!(q.contains("GitCli") && q.contains("WorktreeGuard"), "{q}");
        // 无候选可派生时不硬凑：开放模板问题会被 gate 直接拒绝（R2 禁开放模板）。
        let mut empty = crate::GoalContract::compile("占位输入");
        empty.entities.clear();
        empty.navigation.clear();
        assert_eq!(with_candidates("问题", &empty), "问题");
    }
}
