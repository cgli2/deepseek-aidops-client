use std::path::PathBuf;
use std::sync::Arc;

use tokio::process::Command;

use harness_core::error::Result;

use crate::Sandbox;

/// Linux 沙箱：landlock 限 FS + seccomp-bpf 限 syscall（原 §9 / M3）。
///
/// 经 `pre_exec` 钩子在**fork 出的子进程内、exec 之前**套用，隔离只作用于被 spawn 的
/// 工具子进程，不套在 harness 自身上（原 §9）：
/// - landlock：未显式允许的路径一律拒绝——工作区可读写执行，根文件系统只读+执行
///   （动态库 / 解释器 / 命令本身需要）；
/// - seccomp：拒绝 `ptrace`（防沙箱内进程跟踪 / 注入其它进程）。
/// 内核不支持（landlock 未启用 / 老内核）时优雅降级，不让所有 shell 命令失败。
pub struct LandlockSeccomp {
    root: PathBuf,
}

impl LandlockSeccomp {
    pub fn new(root: PathBuf) -> Arc<dyn Sandbox> {
        Arc::new(Self { root })
    }
}

impl Sandbox for LandlockSeccomp {
    fn prepare(&self, cmd: &mut Command) -> Result<()> {
        let root = self.root.clone();
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.as_std_mut().pre_exec(move || {
                // 子进程内错误吞掉降级：内核缺 landlock/seccomp 支持时仍能运行命令。
                let _ = apply_landlock(&root);
                let _ = apply_seccomp();
                Ok(())
            });
        }
        Ok(())
    }
}

/// landlock：工作区读写 + 根文件系统只读执行，其余路径全部拒绝。
fn apply_landlock(root: &std::path::Path) -> std::io::Result<()> {
    use landlock::{ABI, Access, AccessFs, PathBeneath, Ruleset, RulesetAttr, RulesetCreatedAttr};

    fn io(e: impl std::fmt::Display) -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
    }

    let abi = ABI::V3;
    let ruleset = Ruleset::default()
        .handle_access(AccessFs::from_all(abi))
        .map_err(io)?;
    let created = ruleset.create().map_err(io)?;
    created
        .add_rules(PathBeneath::new(root, AccessFs::from_all(abi)))
        .map_err(io)?
        .add_rules(PathBeneath::new("/", AccessFs::from_read(abi)))
        .map_err(io)?
        .restrict_self()
        .map_err(io)?;
    Ok(())
}

/// seccomp-bpf：默认放行，`ptrace` 返回 EPERM。
fn apply_seccomp() -> std::io::Result<()> {
    use seccompiler::{BpfProgram, Filter, SeccompAction, SeccompRule, TargetArch, apply_filter};
    use std::collections::BTreeMap;

    #[cfg(target_arch = "x86_64")]
    let arch = TargetArch::x86_64;
    #[cfg(target_arch = "aarch64")]
    let arch = TargetArch::aarch64;

    let mut rules: BTreeMap<i64, SeccompRule> = BTreeMap::new();
    rules.insert(
        libc::SYS_ptrace as i64,
        SeccompRule::new(Vec::new())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?,
    );
    let filter = Filter::new(
        arch,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        rules,
    )
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
    let program: BpfProgram = filter
        .try_into()
        .map_err(|e: seccompiler::CompilationError| {
            std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
        })?;
    apply_filter(&program)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
}
