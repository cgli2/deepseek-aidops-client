use std::sync::{Arc, Mutex};

use tokio::process::Command;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

use harness_core::error::{Error, Result};

use crate::Sandbox;

/// Windows 沙箱：Job Object（原 §9 / M3）。
///
/// 所有被 spawn 的工具子进程（及其进程树）在启动后即被关联进同一个 Job Object：
/// - `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`：harness 退出（句柄关闭）时整棵进程树被回收，
///   防止 shell 工具派生的后台进程逃逸、驻留在用户机器上；
/// - Job 句柄随本结构 Drop 关闭，RAII 语义与 `effect()` 回滚一致。
pub struct JobObject {
    /// 懒创建的 Job 句柄（windows-sys 的 HANDLE = isize，0 表示未创建）。
    handle: Mutex<isize>,
}

impl JobObject {
    pub fn new() -> Arc<dyn Sandbox> {
        Arc::new(Self {
            handle: Mutex::new(0),
        })
    }

    /// 幂等地创建 Job Object 并套用限制信息，返回句柄。
    fn ensure(&self) -> Result<isize> {
        let mut slot = self
            .handle
            .lock()
            .map_err(|_| Error::SandboxDenied("job object lock poisoned".into()))?;
        if *slot != 0 {
            return Ok(*slot);
        }
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle == 0 {
            return Err(Error::SandboxDenied(format!(
                "CreateJobObject failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            let err = std::io::Error::last_os_error();
            unsafe { CloseHandle(handle) };
            return Err(Error::SandboxDenied(format!(
                "SetInformationJobObject failed: {err}"
            )));
        }
        *slot = handle;
        Ok(handle)
    }
}

impl Sandbox for JobObject {
    fn prepare(&self, _cmd: &mut Command) -> Result<()> {
        // 提前创建 Job Object：失败在 spawn 前就暴露，工具结果可见。
        self.ensure().map(|_| ())
    }

    fn post_spawn(&self, pid: u32) -> Result<()> {
        let handle = self.ensure()?;
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
        if process == 0 {
            // 进程可能已闪电退出（竞态）：降级不阻断命令本身。
            return Ok(());
        }
        let ok = unsafe { AssignProcessToJobObject(handle, process) };
        unsafe { CloseHandle(process) };
        if ok == 0 {
            // 子进程已在其它 Job 且环境不允许嵌套时（如部分 CI / 远程会话），
            // 降级为无 Job 绑定（等价 NullSandbox），而不是让所有 shell 命令失败。
            return Ok(());
        }
        Ok(())
    }
}

impl Drop for JobObject {
    fn drop(&mut self) {
        // 关闭句柄即触发 KILL_ON_JOB_CLOSE：回收所有仍存活的工具子进程树。
        if let Ok(slot) = self.handle.lock() {
            if *slot != 0 {
                unsafe { CloseHandle(*slot) };
            }
        }
    }
}
