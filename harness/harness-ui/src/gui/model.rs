//! Lightweight GUI data models without rendering or runtime behavior.

#[derive(Clone)]
pub(super) struct ChatMsg {
    pub(super) kind: String,
    /// 预留：发送方展示名（当前气泡头部按 kind 渲染，暂未读取）。
    #[allow(dead_code)]
    pub(super) label: String,
    pub(super) text: String,
    /// Assistant 原文累积，用于跨分片剥离 DSML；其他消息类型保持为空。
    pub(super) raw: String,
}

/// Runtime 持久化的交付判定投影。模型文本中的“已完成”不写入这里。
#[derive(Clone)]
pub(super) struct DeliveryUi {
    pub(super) outcome: harness_session::DeliveryOutcome,
    pub(super) remaining: usize,
    pub(super) verification_count: usize,
    pub(super) reason: Option<String>,
}

/// Runtime 遥测的轻量 UI 投影。只消费 SessionEvent，UI 不反向改变执行状态。
#[derive(Clone)]
pub(super) struct ExecutionProjectionUi {
    pub(super) executor: String,
    pub(super) goal: String,
    pub(super) intent: String,
    pub(super) phase: String,
    pub(super) allowed_tools: Vec<String>,
    pub(super) step: usize,
    pub(super) tool_calls: usize,
    pub(super) evidence_count: usize,
    pub(super) verified_count: usize,
    pub(super) blocked_count: usize,
    pub(super) active_work_item: String,
    pub(super) work_items: Vec<harness_session::WorkItemTelemetry>,
    pub(super) next_action: String,
    pub(super) active_hypothesis: String,
    pub(super) no_information_count: usize,
    pub(super) correction_count: usize,
    pub(super) detail: String,
}

#[derive(Clone, Default)]
pub(super) struct CouncilUi {
    pub(super) id: String,
    pub(super) goal: String,
    pub(super) phase: String,
    pub(super) max_parallel: usize,
    pub(super) started_at: Option<std::time::Instant>,
    pub(super) tasks: std::collections::BTreeMap<String, CouncilTaskUi>,
    pub(super) gates: Vec<harness_session::CouncilGateResult>,
    pub(super) detail: String,
}

#[derive(Clone)]
pub(super) struct CouncilTaskUi {
    pub(super) spec: harness_session::CouncilTaskSpec,
    pub(super) state: harness_session::CouncilTaskState,
    pub(super) attempt: u32,
    pub(super) detail: String,
}

/// 插件分类：核心内置 / WASM 扩展 / Trellis（进程内数据面插件）。
///
/// 插件管理页按分类分区展示；新增插件类型时在此扩展枚举，
/// 并在 `load_plugin_rows` 中追加对应行即可接入「启用 / 停用 / 配置」。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PluginKind {
    /// 随应用发布的核心插件：恒启用，不可停用、不可移除。
    Core,
    /// 用户导入的 WASM 插件：可启用 / 停用 / 移除，经 wasmtime 沙箱隔离。
    Wasm,
    /// Trellis（spec 驱动开发）：进程内 PreStep 注入，可启用 / 停用，可配置规格与任务文件。
    Trellis,
}

#[derive(Clone)]
pub(super) struct PluginUiRow {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) desc: String,
    pub(super) kind: PluginKind,
    pub(super) enabled: bool,
    pub(super) active: bool,
    /// Trellis 插件特有：规格文件路径（空 = 不注入规格）。
    pub(super) spec_file: String,
    /// Trellis 插件特有：任务状态机文件路径（空 = 不维护任务）。
    pub(super) tasks_file: String,
}

#[derive(Clone)]
pub(super) struct MemItem {
    pub(super) title: String,
    pub(super) meta: String,
    pub(super) body: String,
}

/// 记忆面板一次刷新的完整结果：通用条目 + 代码图谱原始符号（结构化视图消费）。
#[derive(Clone)]
pub(super) struct MemRefresh {
    pub(super) items: Vec<MemItem>,
    pub(super) code_symbols: Vec<harness_capability::assets::CodeSymbol>,
}

/// 核心插件默认启用，且不可在界面中取消。
pub(super) const BUILTIN_PLUGINS: &[(&str, &str, &str)] = &[
    ("local-files", "本地文件", "读取、搜索与写入工作区文件"),
    ("shell", "Shell", "在受限 Shell 中执行命令并回传输出"),
    ("git", "Git", "查看差异、提交历史与分支操作"),
    ("memory", "Memory", "跨会话记忆检索与沉淀"),
    ("hooks", "Hooks", "生命周期钩子：提交前检查等自动化"),
    (
        "superpowers",
        "Superpowers",
        "系统工作流扩展：提供规划、执行、验证等内置技能",
    ),
];
