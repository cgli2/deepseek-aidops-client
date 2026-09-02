//! 把用户描述编译成可执行的任务形状，避免 Agent 仅凭模糊关键词决定是否泛搜。
//!
//! **设计原则（完成 V5 未竟迁移）**：澄清门禁与意图分类**完全不依赖任何开放集合词表**。
//! 词表驱动无法枚举用户真实表达的千变万化，是"简单问题也被反复追问"的根因。
//!
//! 本模块只使用**封闭且语言/领域不变**的信号：
//! - `extract_code_symbols` / `extract_acronyms`：工作区可定位的代码符号（L0 结构信号）；
//! - `extract_exact_transformation`：用户明确写出的 `X → Y` 变更契约；
//! - `extract_navigation`：用户写明的 `A > B` 导航路径；
//! - `has_structural_action`：封闭的结构化动作集（V5 §4.1 明确允许进 L0）；
//! - 是否是"纯提问"：以句末 `?`/`？` 或封闭疑问词开头判定（不是开放的话题枚举）。
//!
//! "能否定位"交由 `GoalContract::has_locatable_signal()`（工作区区分度裁决）给出；
//! "描述与期望是否一致"交由 Phase 2 的 `inspect_diff`（运行期观察）给出。两者都不靠词表。

use crate::goal_execution::{extract_exact_transformation, GoalContract};
use crate::target_extract::{extract_code_symbols, extract_navigation};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentKind {
    AtomicRegression,
    ScopedChange,
    Investigation,
    OpenEnded,
}

/// 封闭的结构化（位置/顺序）动作集——V5 §4.1 已裁定可进 L0。
/// 这类动作跨领域恒定：不可能出现"某个新说法"击穿它，故保留为机制层唯一允许的
/// 动作词表（与 D5"开放集合迁出机制层"不冲突，因为它本就是封闭集）。
const STRUCTURAL_ACTIONS: [&str; 25] = [
    "位置", "顺序", "对调", "互换", "调换", "交换", "前后", "移到", "置顶", "反转", "移动", "挪",
    "上移", "下移", "左移", "右移", "重排",
    // 标量大小/高低调整同样是封闭的结构动作。此前“把输入框高度调小”没有代码符号、
    // 路径或 X→Y 写法，会误落到 OpenEnded，进而绕过变更任务的写入与验证门禁。
    "调小", "调大", "缩小", "放大", "降低", "提高", "减小", "增大",
];

/// 封闭的疑问引导词——只覆盖"以疑问方式开头/结尾"这一**语言结构**特征，
/// 不枚举任何话题。任一命中即视为纯提问（Investigation），交由只读诊断流程处理。
const QUESTION_LEAD: [&str; 6] = ["为什么", "为何", "怎么", "如何", "啥", "怎样"];

#[derive(Debug, Clone)]
pub struct IntentProfile {
    pub kind: IntentKind,
    /// 是否是可执行的任务（vs 纯提问/闲聊）。门禁据此决定是否进入任务追问。
    pub is_task: bool,
    pub has_code_entity: bool,
    pub has_transformation_contract: bool,
    pub has_structural_action: bool,
    pub navigation_present: bool,
    pub is_explicit_question: bool,
}

impl IntentProfile {
    /// 仅从文本推导**封闭信号**；是否"可定位"需结合工作区（`GoalContract`），此处不臆测。
    pub fn compile(input: &str) -> Self {
        let has_transformation_contract = extract_exact_transformation(input).is_some();
        let navigation_present = !extract_navigation(input).0.is_empty();
        let has_code_entity = !extract_code_symbols(input).is_empty();
        let has_structural_action = STRUCTURAL_ACTIONS
            .iter()
            .any(|word| input.contains(word));
        let is_explicit_question = input.trim_end_matches('。').trim_end().ends_with('?')
            || input.trim_end_matches('。').trim_end().ends_with('？')
            || QUESTION_LEAD.iter().any(|lead| input.trim().starts_with(lead));

        // 纯提问不进入任务闸门（Investigation 由 Solve 循环自行定位+读取，不需要
        // 任务式追问）。任何"非纯提问"都视为任务——哪怕暂时没有可定位信号，
        // 也应在 Phase 1 门禁里被问一个定位问题，而不是被静默放行到空转熔断。
        let is_task = !is_explicit_question;

        let kind = if !is_task {
            // 纯提问：只读诊断，不写代码。
            IntentKind::Investigation
        } else if has_transformation_contract || has_structural_action {
            // 单点交付，验收标准内含在描述里（X→Y 或 交换两个字段）。
            IntentKind::AtomicRegression
        } else if has_code_entity || navigation_present {
            // 已落地（符号/路径可定位）的任务，可能是多面。
            IntentKind::ScopedChange
        } else {
            // 无封闭任务信号、又非提问——盲任务，门禁会问一个定位问题。
            IntentKind::OpenEnded
        };

        // 交付面数量由 `TaskContract::acceptance_criteria` 给出（契约真实单元），
        // 不再在机制层数 UI 名词（见 `SolvePlan::for_contract`）。

        Self {
            kind,
            is_task,
            has_code_entity,
            has_transformation_contract,
            has_structural_action,
            navigation_present,
            is_explicit_question,
        }
    }

    /// 封闭指示代词集合：仅覆盖"指代一个不在文本中的对象"这一**语言结构**特征
    /// （如"这个/那个/它"），不枚举任何话题。命中即视为"盲指代"——用户没给出任何
    /// 可定位或可执行的目标，连具体症状都没描述，只能反问"你指的是哪个？"。
    /// 非盲指代（如"这个列表"已带具体名词，或根本不含指示代词的具象描述）不命中，
    /// 交给乐观默认路径直接进 `Locate→Inspect`，避免在"简单问题"上也反复追问
    /// （呼应 ADR G2"乐观默认、只问真盲"）。
    const DEICTIC_BLIND: [&str; 6] = ["这个", "那个", "这些", "那些", "它", "此"];

    /// **Phase 1 澄清门禁（信号驱动，乐观默认 + 单问）**。
    ///
    /// 判定完全不依赖任何开放集合词表：
    /// - `!is_task`：纯提问/闲聊 → 不强制任务闸门（Investigation 自行定位+读取）；
    /// - `goal.has_locatable_signal()`：目标已被工作区落地，或已给出 `X → Y` 变更契约
    ///   （含纯 `→ Y`）→ 直接 `Locate→Inspect`，不预问；
    /// - `input` 含盲指代（"这个/那个/它…"且未给任何具象目标）→ 真·盲任务，只问
    ///   **一个**带上下文的定位问题，绝不发清单；
    /// - 其余（含具象描述但暂未落地、或纯开放式故障）→ **乐观放行**到 `Locate→Inspect`，
    ///   由 agent 读代码/跑测试自行发现，不必在首轮追问。
    ///
    /// 这与 V5 的"工作区做裁判"路线一致：可定位性由 `GoalContract`（区分度裁决）给出，
    /// 而非由机制层词表猜测用户措辞。
    pub fn requires_clarification(goal: &GoalContract, is_task: bool, input: &str) -> Option<Clarification> {
        if !is_task {
            return None;
        }
        if goal.has_locatable_signal() {
            return None;
        }
        // 乐观默认：只有"任务 + 无定位信号 + 盲指代（连症状都没给）"才追问。
        // 其余具象描述（哪怕尚未落地）一律放行，杜绝"简单问题也反复追问"的体验。
        if !Self::DEICTIC_BLIND.iter().any(|d| input.contains(d)) {
            return None;
        }
        Some(Clarification::locate(goal))
    }
}

/// Phase 2 之后、正式的追问内容类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClarificationKind {
    /// 真·盲任务：没有任何可定位信号，问一个定位问题。
    Locate,
    /// Inspect 期观察到与目标不一致、但可推断：带上下文单问。
    ObserveMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clarification {
    pub question: String,
    pub kind: ClarificationKind,
}

impl Clarification {
    /// 真·盲任务的定位单问。带上下文（已有的导航/符号线索），不发清单。
    pub fn locate(goal: &GoalContract) -> Self {
        let ctx = if !goal.navigation.is_empty() {
            format!("（已从导航路径 {} 进入工作区）", goal.navigation.join(" → "))
        } else if !goal.entities.is_empty() {
            format!("（已注意到符号 {}）", goal.entities.join("、"))
        } else {
            String::new()
        };
        let question = format!(
            "我需要从描述中定位具体的代码符号、文件或界面入口{}。请给出一个文件路径、目录名或关键符号\
             （例如 src/pages/model、ModelForm、appCode），我可以直接定位，不必全仓搜索。",
            ctx
        );
        Self {
            question,
            kind: ClarificationKind::Locate,
        }
    }

    /// Inspect 期观察到与目标不一致时的带上下文单问。
    pub fn observe_mismatch(goal: &GoalContract, observed: &ObservedBehavior) -> Self {
        let anchor = observed
            .anchors
            .first()
            .cloned()
            .or_else(|| goal.entities.first().cloned())
            .unwrap_or_else(|| "目标位置".into());
        let expected = goal
            .expected_values
            .first()
            .map(|value| value.value.clone())
            .or_else(|| goal.transformation.as_ref().map(|t| t.to_value.clone()))
            .unwrap_or_default();
        let question = format!(
            "我在 {} 处核对了当前实现，与期望终态（{}）不一致。请确认：你的目标就是让它变成「{}」吗？\
             还是我理解错了当前行为？",
            anchor, expected, expected
        );
        Self {
            question,
            kind: ClarificationKind::ObserveMismatch,
        }
    }
}

/// **Phase 2**：Inspect 阶段的运行期观察结果。由 agent 定位后读取/执行/查类型产出，
/// 与 `GoalContract` 的期望终态比对，决定是否需要带上下文追问。
#[derive(Debug, Clone, Default)]
pub struct ObservedBehavior {
    /// 已定位的符号/文件。
    pub anchors: Vec<String>,
    /// 在目标处实际发现的值（用于差异说明）。
    pub observed_value: Option<String>,
    /// 是否已发现期望的终态（期望终态已存在于代码中）。
    pub found_expected: bool,
    pub notes: Vec<String>,
}

/// `inspect_diff` 的裁定结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InspectVerdict {
    /// 观察与期望一致，直接进入 Change。
    Aligned,
    /// 观察与期望不一致，但可推断——带一个上下文单问。
    InferableMismatch(Clarification),
    /// 无锚点可核对，回落 Phase 1 的定位单问。
    NoAnchor,
}

/// **Phase 2 核心**：用运行期观察取代关键词猜异常。
///
/// 旧实现靠 28 词失败词表判断"用户是否描述了偏差"；本函数改由 `ObservedBehavior`
/// （agent 实际读到的代码/跑出的结果）与 `GoalContract` 的变更契约比对：
/// - 锚点为空 → 无锚可核对，回落 Phase 1 定位（由调用方处理）；
/// - 已是期望终态（`to` 值已出现，或当前正是 `from`）→ 一致，直接 Change/Verify，不追问；
/// - 有 `from → to` 契约但当前**既不是 `from` 也不是 `to`** → 带一个上下文单问，
///   让用户在 agent 已定位、已读代码的前提下确认意图，而不是盲猜措辞；
/// - 无 `from` 的纯"改为 to"、或当前≠to 属正常待改状态 → 交给 Change/Verify，不追问。
///
/// 关键点：只有当观察**真的揭示了歧义**（本应是 `from` 却既不是 `from` 也不是 `to`）
/// 才追问；普通待改任务一律放行，绝不退化成"逢任务就问"。
pub fn inspect_diff(goal: &GoalContract, observed: &ObservedBehavior) -> InspectVerdict {
    if observed.anchors.is_empty() {
        return InspectVerdict::NoAnchor;
    }
    if let Some(transformation) = &goal.transformation {
        let to = transformation.to_value.trim();
        if !to.is_empty() && (observed.found_expected || observed.observed_value.as_deref() == Some(to))
        {
            // 代码里已经是期望的 to 值——无需修改，也不追问。
            return InspectVerdict::Aligned;
        }
        if let Some(from) = &transformation.from_value {
            let from = from.trim();
            if !from.is_empty() {
                if observed.observed_value.as_deref() == Some(from) {
                    // 当前正是 from——正常待改状态，直接 Change，不追问。
                    return InspectVerdict::Aligned;
                }
                if observed.observed_value.is_some() && observed.observed_value.as_deref() != Some(to) {
                    // 用户说"把 X 从 from 改成 to"，但当前既不是 from 也不是 to：
                    // 带一个上下文单问，确认意图，而不是盲目按 to 直接改。
                    return InspectVerdict::InferableMismatch(Clarification::observe_mismatch(
                        goal, observed,
                    ));
                }
            }
        }
        // 无 from 的纯"改为 to"：当前≠to 是正常待改状态，交给 Change/Verify，不追问。
        return InspectVerdict::Aligned;
    }
    // 无变更契约：没有可供比对的结构化期望，观察即信息，交给 Solve 循环验证，不追问。
    InspectVerdict::Aligned
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goal_execution::GoalContract;

    #[test]
    fn transformation_contract_is_atomic_regression() {
        let profile = IntentProfile::compile("后台管理->多端拼装，这个菜单名称修改为智能体装配");
        assert_eq!(profile.kind, IntentKind::AtomicRegression);
        assert!(profile.has_transformation_contract);
    }

    #[test]
    fn structural_action_is_atomic_regression() {
        // 用户已说清"交换两个字段的顺序"，验收标准内含在描述里。
        let input = "系统管理->模型管理，把模型名称字段和 API KEY 字段位置顺序互换一下";
        let profile = IntentProfile::compile(input);
        assert_eq!(profile.kind, IntentKind::AtomicRegression);
        assert!(profile.has_structural_action);
        assert!(!profile.is_explicit_question);
    }

    #[test]
    fn scalar_adjustment_is_atomic_regression() {
        let profile = IntentProfile::compile("把当前\n输入框高度调小一点，目前占用太高点");
        assert_eq!(profile.kind, IntentKind::AtomicRegression);
        assert!(profile.has_structural_action);
        assert!(profile.is_task);
    }

    #[test]
    fn code_entity_task_is_scoped_change() {
        let profile = IntentProfile::compile("修复 ModelForm 的校验规则");
        assert_eq!(profile.kind, IntentKind::ScopedChange);
        assert!(profile.has_code_entity);
    }

    #[test]
    fn navigation_task_is_scoped_change() {
        let profile = IntentProfile::compile("后台管理>多端拼装 的标题文案");
        assert_eq!(profile.kind, IntentKind::ScopedChange);
        assert!(profile.navigation_present);
    }

    #[test]
    fn explicit_question_is_investigation_and_read_only() {
        let input = "为什么会话窗口里的短文本会自动换行？请分析根因";
        let profile = IntentProfile::compile(input);
        assert_eq!(profile.kind, IntentKind::Investigation);
        assert!(profile.is_explicit_question);
        assert!(!profile.is_task);
    }

    #[test]
    fn ungrounded_natural_language_is_open_ended_task() {
        // 没有任何封闭信号、又非提问——盲任务，门禁会问一个定位问题。
        let profile = IntentProfile::compile("登录按钮点了没反应，页面也没有跳转");
        assert_eq!(profile.kind, IntentKind::OpenEnded);
        assert!(profile.is_task);
    }

    #[test]
    fn sorting_phrase_is_not_a_structural_action() {
        // "排序"不在封闭结构化动作集，"列表排序不正确"是开放式故障，不应误判为单点交付。
        let profile = IntentProfile::compile("列表排序不正确，排查一下");
        assert!(!profile.has_structural_action);
        assert_ne!(profile.kind, IntentKind::AtomicRegression);
    }

    // ---- Phase 1 门禁 ----

    #[test]
    fn grounded_goal_needs_no_clarification() {
        let mut goal = GoalContract::compile("ModelForm 的校验规则有问题");
        goal.entities.push("ModelForm".into());
        assert!(goal.has_locatable_signal());
        assert!(IntentProfile::requires_clarification(&goal, true, "ModelForm 的校验规则有问题").is_none());
    }

    #[test]
    fn blind_task_asks_a_single_locate_question() {
        let goal = GoalContract::compile("这个列表的排序逻辑有问题，帮我修一下");
        assert!(!goal.has_locatable_signal());
        let clar = IntentProfile::requires_clarification(&goal, true, "这个列表的排序逻辑有问题，帮我修一下")
            .expect("盲任务应问一个定位问题");
        assert_eq!(clar.kind, ClarificationKind::Locate);
        // 单问：不应是清单式（无编号列表、无多个问题点）。
        assert!(!clar.question.contains("1.") && !clar.question.contains("\n2"));
    }

    #[test]
    fn pure_question_asks_no_task_clarification() {
        let goal = GoalContract::compile("为什么列表排序会乱？");
        // 即便目标无法落地，纯提问也不应触发任务式追问（Investigation 自行处理）。
        assert!(IntentProfile::requires_clarification(&goal, false, "为什么列表排序会乱？").is_none());
    }

    #[test]
    fn long_tail_paraphrase_still_proceeds_when_grounded() {
        // "提交按钮点了毫无反应"——"毫无反应"不在任何失败词表里，但按钮符号可定位。
        // 落地后不应被长尾 paraphrase 击穿而反问。
        let mut goal = GoalContract::compile("提交按钮点了毫无反应");
        goal.entities.push("提交按钮".into());
        assert!(goal.has_locatable_signal());
        assert!(IntentProfile::requires_clarification(&goal, true, "提交按钮点了毫无反应").is_none());
    }

    // ---- Phase 2 Inspect 差异比对 ----

    #[test]
    fn inspect_diff_aligned_when_expected_found() {
        let goal = GoalContract::compile("把标题改为智能体装配");
        let observed = ObservedBehavior {
            anchors: vec!["Title".into()],
            observed_value: Some("智能体装配".into()),
            found_expected: true,
            notes: vec![],
        };
        assert_eq!(inspect_diff(&goal, &observed), InspectVerdict::Aligned);
    }

    #[test]
    fn inspect_diff_inferable_mismatch_when_current_is_neither_from_nor_to() {
        // 用户说"把状态从草稿改成已发布"，但代码里当前是"待审核"（既不是 from 也不是 to）
        // ——这才是有歧义、值得带上下文追问的场景。
        let mut goal = GoalContract::compile("把状态改为已发布");
        goal.transformation = Some(crate::goal_execution::ExactTransformation {
            from_value: Some("草稿".into()),
            to_value: "已发布".into(),
        });
        let observed = ObservedBehavior {
            anchors: vec!["status".into()],
            observed_value: Some("待审核".into()),
            found_expected: false,
            notes: vec![],
        };
        match inspect_diff(&goal, &observed) {
            InspectVerdict::InferableMismatch(clar) => {
                assert_eq!(clar.kind, ClarificationKind::ObserveMismatch);
                assert!(clar.question.contains("已发布"));
            }
            other => panic!("期望 InferableMismatch，实际 {other:?}"),
        }
    }

    #[test]
    fn inspect_diff_no_ask_when_current_is_from() {
        // 当前正是 from（"草稿"），正常待改状态，不应追问。
        let mut goal = GoalContract::compile("把状态改为已发布");
        goal.transformation = Some(crate::goal_execution::ExactTransformation {
            from_value: Some("草稿".into()),
            to_value: "已发布".into(),
        });
        let observed = ObservedBehavior {
            anchors: vec!["status".into()],
            observed_value: Some("草稿".into()),
            found_expected: false,
            notes: vec![],
        };
        assert_eq!(inspect_diff(&goal, &observed), InspectVerdict::Aligned);
    }

    #[test]
    fn inspect_diff_no_ask_for_plain_to_change_without_from() {
        // 纯"改为 to"、当前≠to 是正常待改状态，绝不能退化成逢任务就问。
        let goal = GoalContract::compile("把标题改为智能体装配");
        let observed = ObservedBehavior {
            anchors: vec!["Title".into()],
            observed_value: Some("旧标题".into()),
            found_expected: false,
            notes: vec![],
        };
        assert_eq!(inspect_diff(&goal, &observed), InspectVerdict::Aligned);
    }

    #[test]
    fn inspect_diff_no_anchor_when_observations_empty() {
        let goal = GoalContract::compile("把标题改为智能体装配");
        let observed = ObservedBehavior::default();
        assert_eq!(inspect_diff(&goal, &observed), InspectVerdict::NoAnchor);
    }

    #[test]
    fn inspect_diff_aligned_when_already_at_to_value() {
        // 用户说"把 X 改成 to"，但代码里已经是 to——别瞎改，也别追问。
        let mut goal = GoalContract::compile("把状态改为已发布");
        goal.transformation = Some(crate::goal_execution::ExactTransformation {
            from_value: Some("草稿".into()),
            to_value: "已发布".into(),
        });
        let observed = ObservedBehavior {
            anchors: vec!["status".into()],
            observed_value: Some("已发布".into()),
            found_expected: true,
            notes: vec![],
        };
        assert_eq!(inspect_diff(&goal, &observed), InspectVerdict::Aligned);
    }
}
