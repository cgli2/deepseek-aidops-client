//! EguiUi（egui/eframe + glow）。交互式桌面 GUI：
//! - **OS 标准标题栏**：最大化/最小化/还原/DPI/多屏自适应全部由操作系统保证，
//!   不再自绘标题栏（slint 时代 no-frame 窗口几何 API 静默失效的根源被彻底移除）；
//! - 内容区全自绘：侧栏扁平导航、气泡消息流（自动钉底）、输入区、设置弹层；
//! - 深色/浅色主题切换，持久化到 SettingsDb（`ui.theme`）；
//! - 经 `UiInputSink` 反向通道驱动后台 turn；轮询 `SessionLog` 渲染事件。

use std::sync::{Arc, Mutex};

use harness_capability::assets::{CodeGraph, ConversationMemory, SkillLibrary, WikiStore};
use harness_core::event::EventBusView;
use harness_core::ui_input::UiInputSink;
use harness_core::update::UpdateStatus;
use harness_core::Config;
use harness_core::LlmControl;
use harness_session::{SessionEvent, SessionLog, SessionMeta};

use crate::Ui;

mod app;
mod app_state;
mod code_graph;
mod composer;
mod fonts;
mod icons;
mod memory_panel;
mod model;
mod preview_panel;
mod settings_panel;
mod settings_view;
mod sidebar;
mod theme;
mod widgets;
mod workspace;

use app_state::AppState;
#[cfg(all(test, target_os = "macos"))]
use fonts::available_cjk_font;
use fonts::install_cjk_fonts;
#[cfg(target_os = "macos")]
use fonts::install_macos_ui_style;
use icons::{
    draw_brand_logo, draw_icon, draw_paperclip_icon, draw_pencil_icon, draw_trash_icon, Icon,
};
use model::{ChatMsg, MemItem, MemRefresh, PluginUiRow};
use theme::{palette, Palette};
use widgets::{
    accent_button, close_button, field_label, ghost_button, nav_item, plugin_row_ui,
    sidebar_control_height, sidebar_icon_button, sidebar_search_field, sidebar_text_button,
    SidebarActionIcon,
};

// 窗口 / 任务栏图标 RGBA（自动生成，见 scripts/make_icon.py）。eframe 不会自动读 exe 资源，
// 必须在此显式喂给 NativeOptions.icon_data，否则标题栏与任务栏仍是系统默认图标。
include!("icon_data.rs");

const READY: &str =
    "DeepSeek AIOps Harness 已就绪。在下方输入框输入消息，按 Enter 或点击「发送」开始对话。";

/// 历史面板相对时间展示（刚刚 / N 分钟前 / N 小时前 / N 天前）。
fn relative_time(t: &std::time::SystemTime) -> String {
    let Ok(ago) = t.elapsed() else {
        return "刚刚".into();
    };
    let secs = ago.as_secs();
    if secs < 60 {
        "刚刚".into()
    } else if secs < 3600 {
        format!("{} 分钟前", secs / 60)
    } else if secs < 86400 {
        format!("{} 小时前", secs / 3600)
    } else {
        format!("{} 天前", secs / 86400)
    }
}

/// 诊断追踪：macOS 写 Application Support，其余平台写到 exe 目录。
fn trace(line: &str) {
    if let Some(dir) = crate::settings::app_data_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("harness_gui_trace.log");
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&p)
        {
            let _ = writeln!(f, "[{}] {}", now_ms(), line);
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 桌面 GUI 渲染器（feature = "gui"，egui）。持有反向输入通道（UI → 运行时）。
pub struct EguiUi {
    sink: Arc<dyn UiInputSink>,
    llm_control: Arc<dyn LlmControl>,
    workspace_root: String,
    provider: String,
    base_url: String,
    model: String,
    settings: Arc<crate::SettingsDb>,
    /// 四类记忆资产 Definition 服务（由 compose 注入；连 aidops 后端时为远程实现，
    /// 否则为原生文件实现）。记忆面板经此查询，自动反映后端/本地。
    conv: Arc<dyn ConversationMemory>,
    skill: Arc<dyn SkillLibrary>,
    wiki: Arc<dyn WikiStore>,
    code: Arc<dyn CodeGraph>,
    /// WASM 插件运行时：导入/启用即时生效，禁用/移除立即卸载实例。
    wasm_plugins: Arc<harness_provider_wasm::WasmPluginRuntime>,
    /// 文件预览：只读查询工作区文件内容（沙箱根 = Workspace）。
    fs: Arc<dyn harness_capability::fs::Fs>,
    /// 文件预览：git diff / 跟踪状态查询（零 C 绑定，git CLI 子进程）。
    git: Arc<dyn harness_capability::git::Git>,
    /// 独立 tokio runtime（析构安全包装）：记忆面板驱动资产服务的异步查询（原生走文件 IO、
    /// 后端走网络）。注意 GUI 事件循环本身运行在 `#[tokio::main]` 的 runtime 主线程内，
    /// 不可在该线程直接 `block_on` 另一 runtime（会触发 "Cannot start a runtime from
    /// within a runtime" 硬 panic → 闪退）。因此面板改为在**独立 OS 线程**里 `block_on`，
    /// 结果经 mpsc 通道回传，GUI 线程只做同步 `recv()`，彻底规避重入 panic。
    rt: UiRuntime,
}

/// 独立 tokio runtime 的析构安全包装。
///

/// 点右上角关闭窗口时，本对象可能在主 runtime（`#[tokio::main]`）的异步上下文中被 drop；
/// tokio 禁止在该上下文 drop runtime（blocking 池无法安全关闭），直接 panic 并经
/// services RwLock 中毒引发二次 panic → abort，表现为点关闭后卡顿数秒才退出。
/// 修复：drop 时把 runtime move 到专用 OS 线程做有界关闭，任何上下文都安全。
struct UiRuntime(Option<tokio::runtime::Runtime>);

impl UiRuntime {
    fn new(thread_name: &str) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name(thread_name)
            .build()
            .expect("[harness-ui] 无法创建记忆查询 runtime");
        Self(Some(rt))
    }

    fn handle(&self) -> tokio::runtime::Handle {
        self.0
            .as_ref()
            .expect("[harness-ui] 记忆查询 runtime 已关闭")
            .handle()
            .clone()
    }
}

impl Drop for UiRuntime {
    fn drop(&mut self) {
        if let Some(rt) = self.0.take() {
            // 有界关闭（2s）：查询任务均为本地文件 IO，正常瞬间完成；
            // 移交独立线程确保不在异步上下文中 drop runtime。
            let _ = std::thread::Builder::new()
                .name("harness-ui-mem-shutdown".into())
                .spawn(move || rt.shutdown_timeout(std::time::Duration::from_secs(2)));
        }
    }
}

impl EguiUi {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sink: Arc<dyn UiInputSink>,
        llm_control: Arc<dyn LlmControl>,
        workspace_root: String,
        provider: String,
        base_url: String,
        model: String,
        settings: Arc<crate::SettingsDb>,
        conv: Arc<dyn ConversationMemory>,
        skill: Arc<dyn SkillLibrary>,
        wiki: Arc<dyn WikiStore>,
        code: Arc<dyn CodeGraph>,
        wasm_plugins: Arc<harness_provider_wasm::WasmPluginRuntime>,
        fs: Arc<dyn harness_capability::fs::Fs>,
        git: Arc<dyn harness_capability::git::Git>,
    ) -> Self {
        Self {
            sink,
            llm_control,
            workspace_root,
            provider,
            base_url,
            model,
            settings,
            conv,
            skill,
            wiki,
            code,
            wasm_plugins,
            fs,
            git,
            rt: UiRuntime::new("harness-ui-mem"),
        }
    }
}
impl Ui for EguiUi {
    fn run(self: Arc<Self>, _bus: EventBusView, log: Arc<SessionLog>) {
        let integrated_titlebar_setting = self.settings.get("ui.integrated_titlebar");
        let integrated_titlebar = crate::window_chrome::integrated_titlebar_enabled(
            integrated_titlebar_setting.as_deref(),
        );
        #[cfg(target_os = "macos")]
        let window_title = "";
        #[cfg(not(target_os = "macos"))]
        let window_title = "AIOPS Desktop";
        let mut viewport = egui::ViewportBuilder::default()
            .with_title(window_title)
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([960.0, 600.0])
            // egui 0.30：窗口图标挂在 ViewportBuilder 上（旧版 NativeOptions.icon_data 已移除）。
            .with_icon(egui::IconData {
                rgba: APP_ICON_RGBA.to_vec(),
                width: APP_ICON_WIDTH,
                height: APP_ICON_HEIGHT,
            });
        if integrated_titlebar {
            #[cfg(target_os = "macos")]
            {
                viewport = viewport
                    .with_decorations(true)
                    .with_fullsize_content_view(true)
                    .with_title_shown(false)
                    .with_titlebar_shown(false)
                    .with_titlebar_buttons_shown(true);
            }
            #[cfg(target_os = "windows")]
            {
                viewport = viewport.with_decorations(false);
            }
        }
        let options = eframe::NativeOptions {
            viewport,
            ..Default::default()
        };
        trace("EguiUi::run start");
        // GUI 子系统下 panic 无 stderr 可见：先把 panic 信息落盘到 trace 日志再交给默认钩子。
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let bt = std::backtrace::Backtrace::force_capture();
            trace(&format!("[panic] {info}\n{bt}"));
            default_hook(info);
        }));
        let app = AppState::new(self, log);
        // eframe 0.30：字体安装只能在 CreationContext 就绪后进行（App::setup 已移除）。
        if let Err(e) = eframe::run_native(
            "AIOPS Desktop",
            options,
            Box::new(move |cc| {
                install_cjk_fonts(&cc.egui_ctx);
                #[cfg(target_os = "macos")]
                install_macos_ui_style(&cc.egui_ctx);
                Ok(Box::new(app))
            }),
        ) {
            trace(&format!("eframe::run_native ERR: {e}"));
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod macos_font_tests {
    use super::*;

    #[test]
    fn system_cjk_font_is_loaded_and_contains_chinese_glyphs() {
        let (path, _) = available_cjk_font().expect("macOS system CJK font is missing");
        assert!(path.is_file());

        let ctx = egui::Context::default();
        install_cjk_fonts(&ctx);
        ctx.begin_pass(Default::default());
        let has_chinese =
            ctx.fonts(|fonts| fonts.has_glyphs(&egui::FontId::proportional(14.0), "中文界面"));
        let _ = ctx.end_pass();
        assert!(
            has_chinese,
            "loaded font cannot render Chinese: {}",
            path.display()
        );
    }
}

#[cfg(test)]
mod close_safety_tests {
    use super::*;

    /// 根因锚定：裸 tokio Runtime 在异步上下文（如 `#[tokio::main]` 的 block_on 内）
    /// 被 drop 会硬 panic。这正是旧代码点右上角关闭后卡顿/崩溃的来源。
    #[test]
    #[should_panic(expected = "Cannot drop a runtime in a context where blocking is not allowed")]
    fn raw_runtime_drop_inside_block_on_panics() {
        let outer = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        outer.block_on(async {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            drop(rt);
        });
    }

    /// 回归守卫：`UiRuntime` 在同样的异步上下文中析构必须安全
    ///（移交专用 OS 线程做有界关闭），保证关窗退出路径不再 panic。
    #[test]
    fn ui_runtime_drop_inside_async_context_is_safe() {
        let outer = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        outer.block_on(async {
            let rt = UiRuntime::new("harness-ui-mem-test");
            // 确认 runtime 可用后再析构（跨 runtime 用 spawn+await，避免嵌套 block_on）。
            let v = rt.handle().spawn(async { 1 + 1 }).await.unwrap();
            assert_eq!(v, 2);
            drop(rt);
        });
        drop(outer);
    }
}
