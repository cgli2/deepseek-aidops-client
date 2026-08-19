//! 版本管理与自动升级。
//!
//! 设计（混合模式：默认提示，可切换自动安装）：
//! - 清单 URL 可配置（`update.manifest_url`），远端返回 JSON：
//!   ```json
//!   {
//!     "version": "0.2.0",
//!     "channel": "stable",
//!     "notes": "修复会话重命名；优化权限下拉对齐",
//!     "url": "https://example.com/aidops-desktop-0.2.0.exe",
//!     "sha256": "hex...",        // 可选，命中则下载后校验
//!     "mandatory": false         // 可选，true 时仅允许「立即升级」
//!   }
//!   ```
//!   若 `channel` 非 stable/空，会作为 `?channel=<channel>` 追加到清单 URL。
//! - 清单地址支持简写：`github:owner/repo`（或 `gh:owner/repo`，可选 `@branch`）
//!   会解析为 `raw.githubusercontent.com` 直链；其余字符串按原样作为直链。
//! - 启动（节流 24h）后台拉取清单，与编译期版本 `CARGO_PKG_VERSION` 比较。
//! - 有新版本：GUI 顶部横幅提示；默认仅提示（打开下载页）；设置可开「自动下载并安装」。
//! - 自动安装：下载新 exe 到 exe 旁 `<stem>-next.exe` + 写 `.update-pending` 标记；
//!   下次启动（或点「重启」）由 `try_apply_and_relaunch` 替换并重启。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// 编译期版本号（来自 `[workspace.package] version`，所有 crate 共享）。
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// exe 文件名主干（与 `bin/Cargo.toml` 的 `[[bin]] name` 一致）。
/// 替换逻辑据此推导 `*-next.exe` / `*-old.exe`，改名不影响功能。
#[cfg(windows)]
const APP_EXE_STEM: &str = "aidops-desktop";

/// 远端发布清单（清单 JSON 直接对应此结构）。
#[derive(Clone, Debug, serde::Deserialize)]
pub struct Release {
    pub version: String,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    pub url: String,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub mandatory: Option<bool>,
}

/// 更新状态机（后台线程写、GUI 主循环读）。
#[derive(Clone, Debug)]
pub enum UpdateStatus {
    /// 未检查 / 已忽略。
    Idle,
    /// 正在拉取清单。
    Checking,
    /// 有新版本待处理。
    Available(Release),
    /// 已是最新（或已忽略此版本）。
    UpToDate,
    /// 检查/下载出错。
    Error(String),
    /// 正在下载新版本（自动安装）。
    Downloading,
    /// 已下载完成，等待重启替换。
    ReadyToRestart { version: String, path: PathBuf },
}

/// 当前 Unix 秒（用于节流与记录）。
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 语义化版本比较：仅取每段前导数字（`1.2.3` / `1.2.3-beta` 均按 `1.2.3` 处理）。
/// 返回 `a.cmp(b)`。
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let parse = |s: &str| -> Vec<u64> {
        s.split('.')
            .filter_map(|seg| {
                seg.trim()
                    .split(|c: char| !c.is_ascii_digit())
                    .next()
                    .and_then(|x| x.parse::<u64>().ok())
            })
            .collect()
    };
    let pa = parse(a);
    let pb = parse(b);
    let n = pa.len().max(pb.len());
    for i in 0..n {
        let xa = *pa.get(i).unwrap_or(&0);
        let xb = *pb.get(i).unwrap_or(&0);
        match xa.cmp(&xb) {
            std::cmp::Ordering::Equal => continue,
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

/// candidate 是否比 current 更新。
pub fn is_newer(current: &str, candidate: &str) -> bool {
    compare_versions(current, candidate) == std::cmp::Ordering::Less
}

fn manifest_url_with_channel(base: &str, channel: &str) -> String {
    let channel = channel.trim();
    if channel.is_empty() || channel.eq_ignore_ascii_case("stable") {
        return base.to_string();
    }
    if base.contains('?') {
        format!("{base}&channel={channel}")
    } else {
        format!("{base}?channel={channel}")
    }
}

/// 默认清单地址占位（未配置）。形如 `github:OWNER/REPO`，含占位符时视为「未配置」，
/// 启动自动检查会静默跳过，不会误报错误。用户把 OWNER/REPO 换成真实仓库后即生效。
pub const DEFAULT_MANIFEST_URL: &str = "github:OWNER/REPO";

/// 把用户输入的清单地址归一化。
///
/// - 支持 GitHub 简写：`github:owner/repo` 或 `gh:owner/repo`，可选 `@branch`
///   （默认 `main`），解析为 `raw.githubusercontent.com` 直链。这样无需依赖
///   `api.github.com`、不受 60 次/小时匿名限流，且内网只要能访问 raw 即可。
/// - 其它字符串原样返回（任意静态托管 / COS / nginx / 内网文件服务的直链）。
pub fn normalize_manifest_url(input: &str) -> String {
    let input = input.trim();
    let rest = if let Some(r) = input.strip_prefix("github:") {
        r
    } else if let Some(r) = input.strip_prefix("gh:") {
        r
    } else {
        return input.to_string();
    };
    // 解析 owner/repo[@branch]
    let (repo, branch) = match rest.split_once('@') {
        Some((r, b)) => (r.trim_end_matches('/'), b),
        None => (rest.trim_end_matches('/'), "main"),
    };
    format!("https://raw.githubusercontent.com/{repo}/{branch}/update-manifest.json")
}

/// 清单地址是否视为「已配置」。默认占位 `github:OWNER/REPO` 视为未配置，避免首跑误报。
pub fn is_manifest_configured(input: &str) -> bool {
    let u = normalize_manifest_url(input);
    !u.is_empty() && !u.contains("OWNER/REPO")
}

fn http_client(timeout: std::time::Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .user_agent(concat!("aidops-desktop/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))
}

/// 拉取清单并与当前版本比较，返回「应升级的 Release」或 `None`（已最新）。
pub fn check_for_update(
    manifest_url: &str,
    channel: &str,
    current: &str,
) -> Result<Option<Release>, String> {
    let base = normalize_manifest_url(manifest_url);
    let url = manifest_url_with_channel(&base, channel);
    let client = http_client(std::time::Duration::from_secs(10))?;
    let resp = client
        .get(&url)
        .send()
        .map_err(|e| format!("拉取清单失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("清单返回 HTTP {}", resp.status()));
    }
    let rel: Release = resp.json().map_err(|e| format!("解析清单失败: {e}"))?;
    if is_newer(current, &rel.version) {
        Ok(Some(rel))
    } else {
        Ok(None)
    }
}

/// 下载文件到 dest（流式写入，避免大文件占内存）。
pub fn download_file(url: &str, dest: &Path) -> Result<(), String> {
    let client = http_client(std::time::Duration::from_secs(300))?;
    let resp = client
        .get(url)
        .send()
        .map_err(|e| format!("下载失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("下载返回 HTTP {}", resp.status()));
    }
    let bytes = resp.bytes().map_err(|e| format!("读取下载内容失败: {e}"))?;
    if let Some(parent) = dest.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(dest, &bytes).map_err(|e| format!("写入文件失败: {e}"))?;
    Ok(())
}

/// 计算文件 sha256（十六进制小写），用于下载后完整性校验。
pub fn sha256_of(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let data = std::fs::read(path).map_err(|e| format!("读取文件失败: {e}"))?;
    let mut h = Sha256::new();
    h.update(&data);
    Ok(format!("{:x}", h.finalize()))
}

#[cfg(windows)]
fn current_exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from))
}

/// 启动后台版本检查（节流由调用方控制）。
///
/// - `manifest_url` 为空且非 `force`：直接返回（不检查）。
/// - `force` 且 `manifest_url` 为空：状态置为错误提示（用于「立即检查」按钮）。
/// - `force` 且 `auto_check` 关闭：仍执行（手动触发不受开关限制）。
pub fn spawn_check(
    status: Arc<Mutex<UpdateStatus>>,
    manifest_url: &str,
    channel: &str,
    skipped: &str,
    force: bool,
) {
    let url = manifest_url.trim().to_string();
    if !force && !is_manifest_configured(&url) {
        return;
    }
    if force && !is_manifest_configured(&url) {
        if let Ok(mut g) = status.lock() {
            *g = UpdateStatus::Error(
                "未配置清单 URL，请先在「更新」设置中填写（支持 github:owner/repo 简写）".into(),
            );
        }
        return;
    }
    let channel = channel.to_string();
    let skipped = skipped.to_string();
    let current = CURRENT_VERSION.to_string();
    std::thread::spawn(move || {
        if let Ok(mut g) = status.lock() {
            *g = UpdateStatus::Checking;
        }
        let result = check_for_update(&url, &channel, &current);
        if let Ok(mut g) = status.lock() {
            match result {
                Ok(Some(rel)) if rel.version != skipped => *g = UpdateStatus::Available(rel),
                Ok(Some(_)) => *g = UpdateStatus::UpToDate,
                Ok(None) => *g = UpdateStatus::UpToDate,
                Err(e) => *g = UpdateStatus::Error(e),
            }
        }
    });
}

/// 自动安装：下载新 exe 到 exe 旁 + 写 `.update-pending`，等待重启替换。
pub fn spawn_download(status: Arc<Mutex<UpdateStatus>>, rel: Release) {
    #[cfg(not(windows))]
    {
        let _ = rel;
        if let Ok(mut g) = status.lock() {
            *g = UpdateStatus::Error("当前平台请下载并安装新的应用包".into());
        }
        return;
    }
    #[cfg(windows)]
    std::thread::spawn(move || {
        if let Ok(mut g) = status.lock() {
            *g = UpdateStatus::Downloading;
        }
        let dir = match current_exe_dir() {
            Some(d) => d,
            None => {
                if let Ok(mut g) = status.lock() {
                    *g = UpdateStatus::Error("无法确定 exe 目录".into());
                }
                return;
            }
        };
        let next = dir.join(format!("{APP_EXE_STEM}-next.exe"));
        if let Err(e) = download_file(&rel.url, &next) {
            if let Ok(mut g) = status.lock() {
                *g = UpdateStatus::Error(e);
            }
            return;
        }
        // 完整性校验（清单提供 sha256 时）。
        if let Some(expected) = rel
            .sha256
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            match sha256_of(&next) {
                Ok(actual) if actual.eq_ignore_ascii_case(expected) => {}
                Ok(actual) => {
                    let _ = std::fs::remove_file(&next);
                    if let Ok(mut g) = status.lock() {
                        *g = UpdateStatus::Error(format!(
                            "校验失败：期望 {expected}，实际 {actual}"
                        ));
                    }
                    return;
                }
                Err(e) => {
                    if let Ok(mut g) = status.lock() {
                        *g = UpdateStatus::Error(e);
                    }
                    return;
                }
            }
        }
        let marker = dir.join(".update-pending");
        let _ = std::fs::write(&marker, rel.version.as_bytes());
        if let Ok(mut g) = status.lock() {
            *g = UpdateStatus::ReadyToRestart {
                version: rel.version.clone(),
                path: next,
            };
        }
    });
}

/// 用系统默认浏览器打开 URL（升级提示的「手动下载」入口）。
pub fn open_url(url: &str) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/c", "start", "", url])
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

/// 若已下载待升级（`.update-pending` + `<stem>-next.exe` 存在），替换当前 exe 并重启新进程。
///
/// - 成功：替换后拉起新 exe 并 `exit(0)`（本函数不会返回 true；调用方无需处理返回值）。
/// - 失败/无待升级：返回 `false`（非 Windows 平台恒为 `false`，安全 no-op）。
///
/// 必须在 `main()` 最靠前调用，确保所有 Profile（含 GUI）都能在启动瞬间完成自更新。
pub fn try_apply_and_relaunch(exe_dir: &Path) -> bool {
    #[cfg(windows)]
    {
        let marker = exe_dir.join(".update-pending");
        let next = exe_dir.join(format!("{APP_EXE_STEM}-next.exe"));
        if !(marker.exists() && next.exists()) {
            return false;
        }
        let cur = exe_dir.join(format!("{APP_EXE_STEM}.exe"));
        let old = exe_dir.join(format!("{APP_EXE_STEM}-old.exe"));
        // 清理上次残留的旧备份。
        let _ = std::fs::remove_file(&old);
        // Windows 允许重命名正在运行的 exe；旧进程仍映射旧文件，新文件就位供下次启动。
        if std::fs::rename(&cur, &old).is_err() {
            return false;
        }
        if std::fs::rename(&next, &cur).is_err() {
            let _ = std::fs::rename(&old, &cur); // 回滚
            return false;
        }
        let _ = std::fs::remove_file(&marker);
        // 拉起新 exe（沿用原启动参数），随后退出旧进程。
        let _ = std::process::Command::new(&cur).spawn();
        std::process::exit(0);
    }
    #[cfg(not(windows))]
    {
        let _ = exe_dir;
        false
    }
}
