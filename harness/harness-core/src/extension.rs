use std::collections::HashMap;
use std::sync::RwLock;

/// 扩展点清单（dsh 的 40+ 能力接缝的显式枚举）。
///
/// 每个产品功能都必须映射到某个文档化扩展点上的监听器（见 `extensions/EXTENSION-COOKBOOK.md`），
/// 没有一行代码修改循环本身——这是"一切皆插件"的可验证判定标准。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExtensionPoint {
    // 能力接缝（Definition / Provider / Consumer 三角色）
    Llm,
    Shell,
    Fs,
    Editor,
    Lsp,
    Subagent,
    FileWatcher,
    Compaction,
    // 生命周期事件点（工具管线 / 循环）
    PreStep,
    TurnStopping,
    ToolPreExecute,
    ToolExecute,
    ToolPostExecute,
    ToolResult,
    // Codex 借鉴能力（记忆 / 钩子 / git / worktree）
    Memory,
    Hook,
    Git,
}

/// 运行时扩展点登记表。插件在 `register()` 时 `declare` 自己服务于哪个 `ExtensionPoint`，
/// 用于审计与 cookbook 校验；不参与调度（调度仍走事件总线）。
#[derive(Default)]
pub struct ExtensionRegistry {
    served: RwLock<HashMap<ExtensionPoint, Vec<&'static str>>>,
}

impl ExtensionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn declare(&self, point: ExtensionPoint, plugin: &'static str) {
        self.served
            .write()
            .unwrap()
            .entry(point)
            .or_default()
            .push(plugin);
    }

    pub fn served_by(&self, point: ExtensionPoint) -> Vec<&'static str> {
        self.served
            .read()
            .unwrap()
            .get(&point)
            .cloned()
            .unwrap_or_default()
    }
}
