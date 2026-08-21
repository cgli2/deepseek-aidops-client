//! Lightweight GUI data models without rendering or runtime behavior.

#[derive(Clone)]
pub(super) struct ChatMsg {
    pub(super) kind: String,
    pub(super) label: String,
    pub(super) text: String,
    /// Assistant 原文累积，用于跨分片剥离 DSML；其他消息类型保持为空。
    pub(super) raw: String,
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

#[derive(Clone)]
pub(super) struct PluginUiRow {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) desc: String,
    pub(super) core: bool,
    pub(super) enabled: bool,
    pub(super) active: bool,
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
    ("superpowers", "Superpowers", "系统工作流扩展：提供规划、执行、验证等内置技能"),
];
