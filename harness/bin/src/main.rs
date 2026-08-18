//! harness：DeepSeek-AIOps 原生 Rust 编码代理 harness 入口。
//!
//! 组合流程（dsh 插件机制）：`Profile` → `compose_plugins([HarnessPlugin])` →
//! 得到 `(AppContext, ComposeGuard)`；guard 的生命周期即插件集合生命周期（drop 即卸载，等价于
//! dsh `effect()` 自动回滚，完成文档 §8 不变量 3/5）。

// 默认启用了 gui 特性的 Windows 构建编译为 GUI 子系统：双击 exe 不再带出 CMD 黑窗。
#![cfg_attr(all(windows, feature = "gui"), windows_subsystem = "windows")]

mod compose;

use std::io::IsTerminal;
use std::sync::Arc;

use harness_core::config::Config;
use harness_core::plugin::compose_plugins;
use harness_core::types::{Profile, UserInput};
use harness_runtime::Scheduler;
use harness_session::SessionLog;
use harness_ui::Ui;

use compose::HarnessPlugin;

#[tokio::main]
async fn main() -> harness_core::error::Result<()> {
    // 自更新：若上次已下载新版本并标记，启动即替换当前 exe 并重启新进程。
    // 必须放在最前，使 GUI / TUI / Headless 所有形态都能在启动瞬间完成升级。
    if let Some(exe) = std::env::current_exe().ok() {
        if let Some(dir) = exe.parent() {
            harness_core::update::try_apply_and_relaunch(dir);
        }
    }

    // 先加载配置（含 [ui].profile 与 [llm] key），再推导运行 Profile。
    let mut config = Config::load()?;
    let settings = Arc::new(harness_ui::SettingsDb::open_default().map_err(|e| {
        harness_core::error::Error::Io(std::io::Error::new(std::io::ErrorKind::Other, e))
    })?);
    if let Some(value) = settings.get("llm.provider") {
        config.llm.provider = value;
    }
    if let Some(value) = settings.get("llm.base_url") {
        config.llm.base_url = value;
    }
    if let Some(value) = settings.get("llm.model") {
        config.llm.model = value;
    }
    if let Some(value) = settings.get("llm.api_key") {
        config.llm.api_key = Some(value);
    }
    if std::env::var_os("HARNESS_WORKSPACE").is_none() {
        if let Some(value) = settings.get("workspace.root") {
            std::env::set_var("HARNESS_WORKSPACE", value);
        }
    }
    let profile = parse_profile(&config);

    // 真正启用 GUI（profile 请求 gui 且二进制含 gui 特性）时，若当前附带了控制台，
    // 先隐藏并释放它，保证"打开即 GUI 窗口、无 CMD 黑窗"。
    let gui_active = matches!(profile, Profile::Gui) && cfg!(feature = "gui");
    #[cfg(windows)]
    detach_console_if_gui(gui_active);

    // 请求了 GUI 但当前二进制未启用 gui 特性：明确告知，避免"静默回退 CLI"的困惑。
    if matches!(profile, Profile::Gui) && !cfg!(feature = "gui") {
        eprintln!(
            "[harness] 警告：请求 GUI，但本二进制未启用 gui 特性（需 `cargo build --features gui`）。\n\
             [harness] 已回退为 Headless（终端）界面。"
        );
    }

    // 仅非 GUI（有控制台）时打印启动横幅；GUI 已无控制台，信息显示在窗口内。
    if !gui_active {
        eprintln!(
            "[harness] starting (profile={:?}, provider={}, api_key={})",
            profile,
            if cfg!(feature = "deepseek") {
                "deepseek"
            } else if cfg!(feature = "local-llm") {
                "local"
            } else {
                "replay"
            },
            if config
                .llm
                .api_key
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .is_some()
                || std::env::var(&config.llm.api_key_env)
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false)
            {
                "set"
            } else {
                "missing"
            }
        );
    }

    // 编译期组合：等价于 dsh `inject` 推导加载顺序 + 逐 effect 收集进 guard。
    let plugins: Vec<Arc<dyn harness_core::plugin::Plugin>> = vec![Arc::new(HarnessPlugin {
        profile,
        config,
        settings,
    })];
    let (ctx, _guard) = compose_plugins(plugins);

    // 取默认会话日志与 UI（均为事件总线消费者接口）。
    let log: Arc<SessionLog> = ctx.get::<SessionLog>();
    let ui: Arc<dyn Ui> = ctx.get::<dyn Ui>();

    // UI 事件循环。GUI（egui/winit）要求事件循环必须在主线程创建，因此 GUI 直接在主线程
    // 阻塞运行（后台回合由多线程 tokio runtime 的工作线程执行，不会饿死）；
    // TUI 保持旧路径 spawn_blocking，避免占用主线程。
    let bus = ctx.events();
    let ui_task = if matches!(profile, Profile::Gui) {
        Arc::clone(&ui).run(bus, log.clone());
        None
    } else {
        let log2 = log.clone();
        Some(tokio::task::spawn_blocking(move || {
            Arc::clone(&ui).run(bus, log2);
        }))
    };

    // 多任务调度器（单运行时 + 层级取消）。
    let scheduler = Scheduler::new(tokio::runtime::Handle::current());

    // GUI / TUI 由窗口内的输入框经 SessionController 驱动回合；
    // 仅 Headless 形态自动跑一条内置输入（CI / 冒烟回放闭环）。
    if matches!(profile, Profile::Headless) {
        let input = UserInput {
            text: "hello from the harness skeleton".into(),
            attachments: vec![],
        };
        let task = harness_runtime::Task {
            session: log.id(),
            input,
        };
        let sid = scheduler.spawn_session(ctx.clone(), task).await;

        // 主循环：会话跑完即退出；Ctrl-C 随时取消。不再永久阻塞在 ctrl_c，避免「黑屏卡死」。
        tokio::select! {
            _ = scheduler.wait_session(&sid) => {
                eprintln!("[harness] session finished.");
            }
            _ = tokio::signal::ctrl_c() => {
                eprintln!("[harness] interrupted, cancelling...");
                scheduler.cancel_all();
            }
        }
    }

    // 退出前等待：TUI 等窗口关闭（GUI 已在主线程同步结束）；headless 已在上一步结束，停一下等用户看结果。
    if matches!(profile, Profile::Tui) {
        if let Some(task) = ui_task {
            let _ = task.await;
        }
    } else if matches!(profile, Profile::Headless) {
        if std::io::stdin().is_terminal() {
            eprintln!("\n[harness] press Enter to exit...");
            let mut _s = String::new();
            let _ = std::io::stdin().read_line(&mut _s);
        }
    }
    Ok(())
}

/// 从命令行参数 / 配置推导运行 Profile。
///
/// 优先级：命令行 flag（`--tui`/`--gui`/`--acp`）> `default.toml` 的 `[ui].profile` > 默认 Headless。
fn parse_profile(cfg: &Config) -> Profile {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--tui") {
        return Profile::Tui;
    }
    if args.iter().any(|a| a == "--gui") {
        return Profile::Gui;
    }
    if args.iter().any(|a| a == "--acp") {
        return Profile::Acp;
    }
    if args.iter().any(|a| a == "--headless") {
        return Profile::Headless;
    }
    if let Some(p) = cfg.ui.profile.as_ref() {
        match p.to_ascii_lowercase().as_str() {
            "tui" => return Profile::Tui,
            "gui" => return Profile::Gui,
            "acp" => return Profile::Acp,
            _ => {}
        }
    }
    Profile::Headless
}

/// 若以 GUI 形态运行，先隐藏并释放可能附带的 console 窗口，确保"打开即 GUI、无 CMD 黑窗"。
///
/// 仅 Windows 生效：先 `ShowWindow(SW_HIDE)` 瞬时隐藏（避免闪一下），再 `FreeConsole` 释放。
/// 在 windows_subsystem 构建下通常无控制台，`GetConsoleWindow` 返回空，本函数安全 no-op。
#[cfg(windows)]
fn detach_console_if_gui(is_gui: bool) {
    if !is_gui {
        return;
    }
    unsafe {
        use windows_sys::Win32::System::Console::*;
        use windows_sys::Win32::UI::WindowsAndMessaging::*;
        let hwnd = GetConsoleWindow();
        if hwnd != 0 {
            ShowWindow(hwnd, SW_HIDE);
            let _ = FreeConsole();
        }
    }
}
