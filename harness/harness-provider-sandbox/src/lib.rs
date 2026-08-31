//! harness-provider-sandbox：跨平台沙箱（landlock+seccomp / App Sandbox / Job Object / Null）。
//!
//! 沙箱作用于**被 spawn 的子进程**，不套在 harness 自身上（原 §9）。平台实现差异见完成文档 §16。
//! `Sandbox` 定义带 `Any` 超 trait，使 `Arc<dyn Sandbox>` 可作为服务注册。

use std::any::Any;
use std::sync::Arc;

use tokio::process::Command;

use harness_core::error::Result;

/// 沙箱定义（Definition）。`prepare` 在 spawn 前套用平台隔离原语；
/// `post_spawn` 在拿到子进程 pid 后补全绑定（如 Windows JobObject 关联）。
pub trait Sandbox: Any + Send + Sync {
    fn prepare(&self, cmd: &mut Command) -> Result<()>;

    /// 子进程 spawn 成功后的钩子（默认 no-op）。实现方应容忍竞态（进程可能已退出），
    /// 隔离绑定失败时自行决定降级或报错。
    fn post_spawn(&self, _pid: u32) -> Result<()> {
        Ok(())
    }
}

/// 无隔离占位（不支持的平台 / 测试）。Windows / Linux 已分别由
/// `JobObject` / `LandlockSeccomp` 真实实现替换（见 `bin/src/compose.rs` 的平台选择）。
pub struct NullSandbox;

impl Sandbox for NullSandbox {
    fn prepare(&self, _cmd: &mut Command) -> Result<()> {
        Ok(())
    }
}

impl NullSandbox {
    pub fn new() -> Arc<dyn Sandbox> {
        Arc::new(NullSandbox)
    }
}

#[cfg(target_os = "linux")]
mod landlock_seccomp;
#[cfg(target_os = "linux")]
pub use landlock_seccomp::LandlockSeccomp;

#[cfg(target_os = "macos")]
mod app_sandbox;
#[cfg(target_os = "macos")]
pub use app_sandbox::AppSandbox;

#[cfg(target_os = "windows")]
mod job_object;
#[cfg(target_os = "windows")]
pub use job_object::JobObject;
