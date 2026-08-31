use thiserror::Error;

/// 全 workspace 统一错误类型。各 crate 不自定义 `Error` 枚举，统一复用本类型。
#[derive(Debug, Error)]
pub enum Error {
    #[error("service not registered: {0}")]
    ServiceMissing(&'static str),

    #[error("event handler registration conflict")]
    HandlerConflict,

    #[error("tool execution cancelled")]
    Cancelled,

    #[error("sandbox policy denied: {0}")]
    SandboxDenied(String),

    #[error("llm provider error: {0}")]
    Llm(String),

    #[error("plugin dependency cycle involving: {0}")]
    PluginCycle(String),

    #[error("plugin load error: {0}")]
    PluginLoad(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("toml parse error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("toml serialize error: {0}")]
    TomlSer(#[from] toml::ser::Error),

    #[error("git error: {0}")]
    Git(String),

    #[error("lsp error: {0}")]
    Lsp(String),

    #[error("file watcher error: {0}")]
    Watcher(String),

    #[error("subagent error: {0}")]
    Subagent(String),

    #[error("runtime error: {0}")]
    Runtime(String),
}

pub type Result<T> = std::result::Result<T, Error>;
