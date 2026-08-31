//! harness-provider-wasm：wasmtime 隔离的不可信用户脚本 / 工具 Provider（dsh 插件机制 §11.4）。
//!
//! 设计要点：
//! - 用户 / 第三方脚本以 WASM 字节码加载，运行在 wasmtime 线性内存沙箱中；
//! - WASM 侧**只能**调用 host 显式导入的函数，不能直接触碰文件系统 / 网络 / 进程；
//! - 所有"能力"由受信 Provider（`LocalBash` / `LocalFs` / `LocalEditor` …）经导入表暴露，
//!   因此换成 WASM Provider 时，Consumer（`BashTool` 等）源码零改动（不变量 2）。
//!
//! `WasmPluginLoader` 经 `Plugin` 机制注册为可被组合进 `ctx` 的 Provider。真实加载路径在
//! `wasm-tools` feature 下（`loader` 模块）；未开启时保留同名占位类型，便于下游无需 cfg 分支。

#[cfg(feature = "wasm-tools")]
pub mod loader;

#[cfg(feature = "wasm-tools")]
pub use loader::{WasmPluginLoader, WasmPluginRuntime};

// 未开启 `wasm-tools` 时的编译期占位：保留同名类型，避免下游 cfg 分支爆炸。
#[cfg(not(feature = "wasm-tools"))]
pub struct WasmPluginLoader;

#[cfg(not(feature = "wasm-tools"))]
impl WasmPluginLoader {
    pub fn new() -> Self {
        Self
    }

    /// 未开启 `wasm-tools` feature 时调用即报错，提示重新编译。
    pub fn load(&self, _path: &std::path::Path) -> harness_core::error::Result<()> {
        Err(harness_core::error::Error::PluginLoad(
            "wasm plugin support requires the `wasm-tools` feature".into(),
        ))
    }
}
