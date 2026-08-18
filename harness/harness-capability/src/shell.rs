use std::any::Any;
use std::path::PathBuf;

use async_trait::async_trait;

use harness_core::error::Result;

/// Shell 能力请求（bash / pwsh）。
#[derive(Debug, Clone)]
pub struct ShellRequest {
    pub cmd: String,
    pub cwd: Option<PathBuf>,
    pub timeout_ms: u64,
}

/// Shell 能力输出。
#[derive(Debug, Clone)]
pub struct ShellOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Shell 能力定义（Definition）。Provider：`LocalBash` / `WasmShell` / `BashSandbox`。
#[async_trait]
pub trait Shell: Any + Send + Sync {
    async fn run(&self, req: ShellRequest) -> Result<ShellOutput>;
}
