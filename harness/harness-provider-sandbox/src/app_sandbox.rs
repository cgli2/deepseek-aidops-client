use tokio::process::Command;

use harness_core::error::Result;

use crate::Sandbox;

/// macOS 沙箱：App Sandbox / sandbox-exec（无 seccomp，原 §9 / M3）。
///
/// 仅在 `target_os = "macos"` 编译。真实实现封装 `sandbox-exec`；此处为占位骨架。
pub struct AppSandbox;

impl Sandbox for AppSandbox {
    fn prepare(&self, _cmd: &mut Command) -> Result<()> {
        // M3：sandbox-exec 封装。
        Ok(())
    }
}
