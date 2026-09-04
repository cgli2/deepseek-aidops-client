//! 项目事实注入：会话级生成「构建入口 / manifest 位置 / 工具链」等稳定事实，
//! 注入系统上下文，避免模型每回合花大量调用重新探索环境。
//!
//! 取证依据：简单定位任务单回合 30 次调用中 15 次 shell 花在
//! 「发现 cargo 根在 harness/ + 默认 GNU 工具链缺 gcc 改投 MSVC」这类
//! 对同一机器/工作区完全稳定的事实上——应一次告知、永久免探索。

use std::path::Path;
use std::sync::Mutex;

/// 事实缓存：工作区根切换（项目切换）时自动失效重算。
static CACHE: Mutex<Option<(std::path::PathBuf, String)>> = Mutex::new(None);

pub fn project_facts(root: &Path) -> String {
    if let Ok(mut guard) = CACHE.lock() {
        if let Some((cached_root, text)) = &*guard {
            if cached_root == root {
                return text.clone();
            }
        }
        let text = build(root);
        *guard = Some((root.to_path_buf(), text.clone()));
        return text;
    }
    build(root)
}

fn build(root: &Path) -> String {
    let mut lines: Vec<String> = Vec::new();

    // 1) manifest 位置：仓库根无 Cargo.toml 时扫描一层子目录，直接告知 cargo 根。
    let cargo_root = if root.join("Cargo.toml").is_file() {
        None
    } else {
        scan_cargo_subroot(root)
    };
    if let Some(sub) = &cargo_root {
        lines.push(format!(
            "- cargo 工作区位于 `{sub}/`（仓库根没有 Cargo.toml）：所有 cargo 命令必须带 `--manifest-path {sub}/Cargo.toml`，不要在仓库根直接跑 cargo 再猜测报错原因。"
        ));
    }

    // 2) 打包/交付入口。
    #[cfg(windows)]
    if root.join("scripts/build.bat").is_file() {
        lines.push(
            "- 打包交付入口：在仓库根执行 `scripts/build.bat package`（自动配置 MSVC 编译环境）。"
                .into(),
        );
    }
    #[cfg(not(windows))]
    if root.join("scripts/build.sh").is_file() {
        lines.push("- 打包交付入口：`scripts/build.sh`。".into());
    }

    // 3) 工具链：默认 GNU 且存在 MSVC 时直接告知，避免「缺 gcc」的无效重试。
    #[cfg(windows)]
    if let Some(msvc) = msvc_toolchain_name() {
        let manifest = match &cargo_root {
            Some(sub) => format!(" --manifest-path {sub}/Cargo.toml"),
            None => String::new(),
        };
        lines.push(format!(
            "- 本机默认 cargo 工具链为 GNU（缺 gcc，ring/cc 等依赖 C 编译器的 crate 必然失败）；编译验证统一用 `cargo +{msvc} check{manifest} -p <crate>`，失败一次后不要重试默认工具链。"
        ));
    }

    if lines.is_empty() {
        return String::new();
    }
    format!(
        "[项目事实（稳定环境信息，直接使用，禁止重新探索）]\n{}",
        lines.join("\n")
    )
}

/// 扫描一层子目录寻找 Cargo.toml（跳过隐藏/产物目录），返回首个命中目录名。
fn scan_cargo_subroot(root: &Path) -> Option<String> {
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || matches!(name.as_str(), "target" | "node_modules" | "dist") {
            continue;
        }
        if path.join("Cargo.toml").is_file() {
            return Some(name);
        }
    }
    None
}

/// 默认工具链为 GNU 且装有 MSVC 工具链时返回 MSVC 工具链名；否则 None。
#[cfg(windows)]
fn msvc_toolchain_name() -> Option<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("rustup")
        .args(["toolchain", "list"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut default_is_gnu = false;
    let mut msvc: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        let name = line.split_whitespace().next().unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let is_default = line.contains("(default)") || line.contains("(active/default)");
        if is_default && name.contains("-gnu") {
            default_is_gnu = true;
        }
        if name.contains("-msvc") && msvc.is_none() {
            msvc = Some(name.to_string());
        }
    }
    if default_is_gnu { msvc } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_cargo_subroot_and_renders_manifest_fact() {
        let tmp = std::env::temp_dir().join(format!("harness_facts_{}", std::process::id()));
        let sub = tmp.join("harness");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("Cargo.toml"), "[workspace]\n").unwrap();
        // 产物/隐藏目录不应被误判为 cargo 根。
        std::fs::create_dir_all(tmp.join("target")).unwrap();
        std::fs::write(tmp.join("target/Cargo.toml"), "").unwrap();

        let text = build(&tmp);
        assert!(text.contains("cargo 工作区位于 `harness/`"));
        assert!(text.contains("--manifest-path harness/Cargo.toml"));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn root_with_manifest_yields_no_manifest_fact() {
        let tmp = std::env::temp_dir().join(format!("harness_facts_root_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("Cargo.toml"), "[workspace]\n").unwrap();
        let text = build(&tmp);
        assert!(!text.contains("cargo 工作区位于"));
        std::fs::remove_dir_all(&tmp).ok();
    }
}
