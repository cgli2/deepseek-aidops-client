//! 真实 WASM 加载器（feature `wasm-tools`）。
//!
//! 完成"加载字节码 → 实例化 → 套用 host 导入表"的闭环，并补齐 M7 的 capability bridge：
//! guest 只能调用 host 显式导入的 `env.host_log` / `env.shell_run`，不能直接触碰
//! 文件系统 / 网络 / 进程（完成文档 §11.4 不变量：WASM 侧零直接能力）。

use std::path::Path;
use std::sync::Arc;

use harness_capability::shell::{Shell, ShellOutput, ShellRequest};
use harness_core::error::{Error, Result};
use wasmtime::{Caller, Engine, Instance, Linker, Module, Store};

/// 加载不可信 WASM 插件：解析为 `Module`，套用 host 导入表后实例化为 `Instance`。
///
/// 同时接受 `.wasm` 二进制与 `.wat` 文本（`Module::new` 两者都能解析），便于测试与脚本分发。
pub struct WasmPluginLoader {
    engine: Engine,
}

impl Default for WasmPluginLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl WasmPluginLoader {
    pub fn new() -> Self {
        Self {
            engine: Engine::default(),
        }
    }

    /// 加载并实例化一个插件（无受信能力绑定；`env.shell_run` 调用返回 -1）。
    pub fn load(&self, path: &Path) -> Result<(Store<HostState>, Instance)> {
        self.load_inner(path, HostState::default())
    }

    /// 加载并实例化一个插件，同时把受信 `Shell` 能力经导入表暴露给 guest
    /// （"换 Provider 不改 Consumer"：此处 Shell 即 `LocalBash` 等既有实现）。
    pub fn load_with_shell(
        &self,
        path: &Path,
        shell: Arc<dyn Shell>,
    ) -> Result<(Store<HostState>, Instance)> {
        let state = HostState {
            shell: Some(ShellBridge::new(shell)),
            ..Default::default()
        };
        self.load_inner(path, state)
    }

    fn load_inner(&self, path: &Path, state: HostState) -> Result<(Store<HostState>, Instance)> {
        let bytes = std::fs::read(path)?;
        let module =
            Module::new(&self.engine, &bytes).map_err(|e| Error::PluginLoad(e.to_string()))?;

        let mut store = Store::new(&self.engine, state);
        let linker = build_linker(&self.engine);
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| Error::PluginLoad(e.to_string()))?;
        Ok((store, instance))
    }
}

/// host 侧状态：持有受信能力句柄与 guest 写入的日志，供导入函数回调时使用。
#[derive(Default)]
pub struct HostState {
    shell: Option<ShellBridge>,
    /// guest 经 `env.host_log` 写入的日志行（host 同时打印）。
    pub guest_log: std::sync::Mutex<Vec<String>>,
}

/// 异步 `Shell` → 同步调用的桥：专用 OS 线程 + current-thread tokio 运行时。
///
/// wasmtime 的 host 导入函数是同步的，且可能运行在任意线程（含已在 tokio 运行时内的线程，
/// 此时直接 `block_on` 会 panic）；用独立线程上的运行时转发即可安全地在任何上下文调用。
pub struct ShellBridge {
    tx: std::sync::mpsc::Sender<ShellJob>,
}

type ShellJob = (ShellRequest, std::sync::mpsc::Sender<Result<ShellOutput>>);

impl ShellBridge {
    pub fn new(shell: Arc<dyn Shell>) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<ShellJob>();
        let _ = std::thread::Builder::new()
            .name("wasm-shell-bridge".into())
            .spawn(move || {
                let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                while let Ok((req, reply)) = rx.recv() {
                    let result = rt.block_on(shell.run(req));
                    let _ = reply.send(result);
                }
            });
        Self { tx }
    }

    /// 同步执行一次 Shell 请求（阻塞当前线程直到完成）。
    pub fn run(&self, req: ShellRequest) -> Result<ShellOutput> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.tx
            .send((req, tx))
            .map_err(|_| Error::PluginLoad("wasm shell bridge worker exited".into()))?;
        rx.recv()
            .map_err(|_| Error::PluginLoad("wasm shell bridge reply lost".into()))?
    }
}

/// 构建 host 导入表（`env.host_log` / `env.shell_run`）。
fn build_linker(engine: &Engine) -> Linker<HostState> {
    let mut linker: Linker<HostState> = Linker::new(engine);

    // guest 写日志：host_log(ptr, len) —— 从 guest 线性内存读 UTF-8 文本。
    let _ = linker.func_wrap(
        "env",
        "host_log",
        |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| {
            let Some(mem) = caller.get_export("memory").and_then(|e| e.into_memory()) else {
                return;
            };
            if ptr < 0 || len <= 0 {
                return;
            }
            let mut buf = vec![0u8; len as usize];
            if mem.read(&caller, ptr as usize, &mut buf).is_err() {
                return;
            }
            let line = String::from_utf8_lossy(&buf).to_string();
            if let Ok(mut log) = caller.data().guest_log.lock() {
                log.push(line.clone());
            }
            eprintln!("[wasm] {line}");
        },
    );

    // guest 跑命令：shell_run(cmd_ptr, cmd_len, out_ptr, out_cap) -> 写入字节数（失败 -1）。
    let _ = linker.func_wrap(
        "env",
        "shell_run",
        |mut caller: Caller<'_, HostState>,
         cmd_ptr: i32,
         cmd_len: i32,
         out_ptr: i32,
         out_cap: i32|
         -> i32 {
            let Some(mem) = caller.get_export("memory").and_then(|e| e.into_memory()) else {
                return -1;
            };
            if cmd_ptr < 0 || cmd_len <= 0 || out_ptr < 0 || out_cap <= 0 {
                return -1;
            }
            let mut cmd_buf = vec![0u8; cmd_len as usize];
            if mem.read(&caller, cmd_ptr as usize, &mut cmd_buf).is_err() {
                return -1;
            }
            let Some(bridge) = caller.data().shell.as_ref() else {
                return -1;
            };
            let req = ShellRequest {
                cmd: String::from_utf8_lossy(&cmd_buf).to_string(),
                cwd: None,
                timeout_ms: 30_000,
            };
            let output = match bridge.run(req) {
                Ok(o) => o,
                Err(_) => return -1,
            };
            let mut payload = output.stdout.into_bytes();
            if !output.stderr.is_empty() {
                payload.extend_from_slice(b"\n[stderr]\n");
                payload.extend_from_slice(output.stderr.as_bytes());
            }
            payload.truncate(out_cap as usize);
            if mem.write(&mut caller, out_ptr as usize, &payload).is_err() {
                return -1;
            }
            payload.len() as i32
        },
    );

    linker
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUEST_WAT: &str = r#"
        (module
          (import "env" "host_log" (func $log (param i32 i32)))
          (memory (export "memory") 1)
          (data (i32.const 16) "hello from wasm")
          (func (export "greet")
            (call $log (i32.const 16) (i32.const 15))))
    "#;

    fn write_temp(name: &str, content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("harness-wasm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn host_log_bridge_roundtrip() {
        let path = write_temp("guest.wat", GUEST_WAT);
        let loader = WasmPluginLoader::new();
        let (mut store, instance) = loader.load(&path).unwrap();
        let greet = instance
            .get_typed_func::<(), ()>(&mut store, "greet")
            .unwrap();
        greet.call(&mut store, ()).unwrap();
        let log = store.data().guest_log.lock().unwrap();
        assert_eq!(log.as_slice(), ["hello from wasm"]);
    }

    #[test]
    fn shell_run_without_bridge_returns_minus_one() {
        // guest 导入 shell_run 但未绑定 Shell 能力时，调用必须返回 -1（零直接能力不变量）。
        let wat = r#"
            (module
              (import "env" "shell_run" (func $run (param i32 i32 i32 i32) (result i32)))
              (memory (export "memory") 1)
              (func (export "try_run") (result i32)
                (call $run (i32.const 0) (i32.const 4) (i32.const 64) (i32.const 64))))
        "#;
        let path = write_temp("runner.wat", wat);
        let loader = WasmPluginLoader::new();
        let (mut store, instance) = loader.load(&path).unwrap();
        let try_run = instance
            .get_typed_func::<(), i32>(&mut store, "try_run")
            .unwrap();
        assert_eq!(try_run.call(&mut store, ()).unwrap(), -1);
    }
}
