use serde::{Deserialize, Serialize};

/// 用户输入（一次 turn 的入口）。
#[derive(Debug, Clone)]
pub struct UserInput {
    pub text: String,
    pub attachments: Vec<Attachment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub path: std::path::PathBuf,
    pub mime: String,
}

/// 编译期组合入口选择（原 §5.2 / 完成文档 §1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    Tui,
    Headless,
    Acp,
    Gui,
}

/// 沙箱模式（原 §8 三轴之一）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum SandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

/// 审批策略（原 §8 三轴之二）。默认 fail-closed。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum ApprovalPolicy {
    Ask,
    Never,
    Unavailable,
}

/// 权限预设（原 §8 三轴之三，捆绑上两轴的用户友好层）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionPreset {
    Minimal,
    Balanced,
    Permissive,
}
