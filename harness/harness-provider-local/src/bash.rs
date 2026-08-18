use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::process::Command;

use harness_capability::shell::{Shell, ShellOutput, ShellRequest};
use harness_core::error::Result;
use harness_core::Workspace;
use harness_provider_sandbox::Sandbox;

/// 本地 bash / pwsh Provider（实现 `Shell`）。返回 `Arc<dyn Shell>`，可直接 `ctx.provide`。
/// 根目录共享 `Arc<Workspace>`：项目切换后命令立即在新工作区执行。
pub struct LocalBash {
    sandbox: Arc<dyn Sandbox>,
    ws: Arc<Workspace>,
}

impl LocalBash {
    pub fn new(sandbox: Arc<dyn Sandbox>, root: std::path::PathBuf) -> Arc<dyn Shell> {
        Self::with_workspace(sandbox, Workspace::new(root))
    }

    /// 共享外部 Workspace（GUI 项目切换可动态换根）。
    pub fn with_workspace(sandbox: Arc<dyn Sandbox>, ws: Arc<Workspace>) -> Arc<dyn Shell> {
        Arc::new(Self { sandbox, ws })
    }
}

#[async_trait]
impl Shell for LocalBash {
    async fn run(&self, req: ShellRequest) -> Result<ShellOutput> {
        let (shell, flag) = if cfg!(windows) {
            ("cmd", "/c")
        } else {
            ("sh", "-c")
        };
        let mut cmd = Command::new(shell);
        cmd.arg(flag).arg(&req.cmd);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW：GUI 调用命令时不弹 CMD 窗口。
        let ws_root = self.ws.root();
        let cwd = req
            .cwd
            .as_ref()
            .map(|p| {
                if p.is_absolute() {
                    p.clone()
                } else {
                    ws_root.join(p)
                }
            })
            .unwrap_or_else(|| ws_root.clone());
        let cwd = cwd.canonicalize()?;
        let root = ws_root
            .canonicalize()
            .unwrap_or(ws_root);
        if !cwd.starts_with(&root) {
            return Err(harness_core::error::Error::SandboxDenied(format!(
                "cwd is outside workspace: {}",
                cwd.display()
            )));
        }
        cmd.current_dir(cwd);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        // 沙箱作用于被 spawn 的子进程（原 §9）：pre-exec 套用平台隔离原语（M3）。
        self.sandbox.prepare(&mut cmd)?;

        let child = cmd.spawn()?;
        // spawn 后钩子：Windows JobObject 等需要 pid 才能完成隔离绑定（M3）。
        if let Some(pid) = child.id() {
            self.sandbox.post_spawn(pid)?;
        }
        let output = match tokio::time::timeout(
            std::time::Duration::from_millis(req.timeout_ms.max(1)),
            child.wait_with_output(),
        )
        .await
        {
            Ok(result) => result?,
            Err(_) => {
                return Ok(ShellOutput {
                    stdout: String::new(),
                    stderr: format!("command timed out after {} ms", req.timeout_ms),
                    exit_code: -1,
                })
            }
        };
        Ok(ShellOutput {
            stdout: decode_console(&output.stdout),
            stderr: decode_console(&output.stderr),
            exit_code: output.status.code().unwrap_or(-1),
        })
    }
}

/// 控制台输出解码：中文 Windows 的 cmd 子进程按 GBK 码页（CP936）输出，
/// 直接 `from_utf8_lossy` 会把中文变成替换符乱码。策略：合法 UTF-8 直接用；
/// 否则回退 GB18030（GBK 超集）；仍失败才兜底 lossy。
fn decode_console(bytes: &[u8]) -> String {
    if std::str::from_utf8(bytes).is_ok() {
        return String::from_utf8_lossy(bytes).to_string();
    }
    let (decoded, _enc, had_errors) = encoding_rs::GB18030.decode(bytes);
    if !had_errors {
        return decoded.into_owned();
    }
    String::from_utf8_lossy(bytes).to_string()
}

#[cfg(test)]
mod tests {
    use super::decode_console;

    #[test]
    fn utf8_passes_through() {
        assert_eq!(decode_console("hello 你好".as_bytes()), "hello 你好");
    }

    #[test]
    fn gbk_fallback_decodes_chinese_console_output() {
        // 「预测」的 GB2312/GBK 编码（0xD4A4 0xB2E2）：中文 Windows 控制台的典型输出。
        let bytes = [0xD4, 0xA4, 0xB2, 0xE2];
        assert_eq!(decode_console(&bytes), "预测");
    }
}
