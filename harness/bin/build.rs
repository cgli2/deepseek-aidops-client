//! Windows 构建时把图标资源编进 `aidops-desktop.exe`。
//!
//! 采用 `embed-resource`（tauri 同款方案）：在 MSVC 下自动经 `vswhere` / `RC` 环境变量 /
//! `PATH` 定位 `rc.exe`，在 GNU 下定位 `windres`，编译 `assets/icon.rc`。
//! 非 Windows 构建自动跳过（Linux / macOS 不产生 exe，无需图标资源）。

fn main() {
    #[cfg(windows)]
    {
        embed_resource::compile("assets/icon.rc", embed_resource::NONE);
    }
}
