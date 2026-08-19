//! EguiUi（egui/eframe + glow）。交互式桌面 GUI：
//! - **OS 标准标题栏**：最大化/最小化/还原/DPI/多屏自适应全部由操作系统保证，
//!   不再自绘标题栏（slint 时代 no-frame 窗口几何 API 静默失效的根源被彻底移除）；
//! - 内容区全自绘：侧栏扁平导航、气泡消息流（自动钉底）、输入区、设置弹层；
//! - 深色/浅色主题切换，持久化到 SettingsDb（`ui.theme`）；
//! - 经 `UiInputSink` 反向通道驱动后台 turn；轮询 `SessionLog` 渲染事件。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use harness_capability::assets::{
    CodeGraph, ConversationMemory, FactKind, SkillLibrary, WikiStore,
};
use harness_core::event::EventBusView;
use harness_core::ui_input::UiInputSink;
use harness_core::update::UpdateStatus;
use harness_core::Config;
use harness_core::LlmControl;
use harness_session::{SessionEvent, SessionLog, SessionMeta};

use crate::Ui;

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

#[cfg(target_os = "windows")]
const CJK_FONT_CANDIDATES: &[&str] = &[
    "C:\\Windows\\Fonts\\msyh.ttc",
    "C:\\Windows\\Fonts\\simhei.ttf",
    "C:\\Windows\\Fonts\\simsun.ttc",
];

#[cfg(target_os = "macos")]
const CJK_FONT_CANDIDATES: &[&str] = &[
    // 新版 macOS 的苹方位于 AssetsV2，运行时扫描；以下为离线/旧系统回退。
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/System/Library/Fonts/STHeiti Medium.ttc",
    "/System/Library/Fonts/STHeiti Light.ttc",
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    "/System/Library/Fonts/Supplemental/Songti.ttc",
    // 保留旧版 macOS 路径作为最后兼容项。
    "/System/Library/Fonts/PingFang.ttc",
];

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const CJK_FONT_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
];

#[cfg(target_os = "macos")]
fn macos_pingfang_path() -> Option<PathBuf> {
    let root = std::path::Path::new("/System/Library/AssetsV2/com_apple_MobileAsset_Font8");
    std::fs::read_dir(root).ok()?.flatten().find_map(|entry| {
        let path = entry.path().join("AssetData/PingFang.ttc");
        path.is_file().then_some(path)
    })
}

fn available_cjk_font() -> Option<(PathBuf, Vec<u8>)> {
    #[cfg(target_os = "macos")]
    if let Some(path) = macos_pingfang_path() {
        if let Ok(bytes) = std::fs::read(&path) {
            return Some((path, bytes));
        }
    }

    CJK_FONT_CANDIDATES.iter().find_map(|path| {
        std::fs::read(path)
            .ok()
            .map(|bytes| (PathBuf::from(path), bytes))
    })
}

/// egui 默认字体不含中文，因此把操作系统 CJK 字体注册为比例和等宽族的 fallback。
fn install_cjk_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // macOS 原生界面采用 SF Pro；中文由同一字体族中的 PingFang SC 补齐。
    // 仅在 macOS 注入到族首，Windows 仍完整保留原有字体顺序与字形度量。
    #[cfg(target_os = "macos")]
    {
        for (key, path, family) in [
            (
                "mac-sf",
                "/System/Library/Fonts/SFNS.ttf",
                egui::FontFamily::Proportional,
            ),
            (
                "mac-sf-mono",
                "/System/Library/Fonts/SFNSMono.ttf",
                egui::FontFamily::Monospace,
            ),
        ] {
            if let Ok(bytes) = std::fs::read(path) {
                fonts
                    .font_data
                    .insert(key.to_owned(), Arc::new(egui::FontData::from_owned(bytes)));
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .insert(0, key.to_owned());
                trace(&format!("[fonts] loaded macOS UI font: {path}"));
            }
        }
    }

    if let Some((path, bytes)) = available_cjk_font() {
        fonts.font_data.insert(
            "cjk".to_owned(),
            Arc::new(egui::FontData::from_owned(bytes)),
        );
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            let fonts_in_family = fonts.families.entry(family).or_default();
            #[cfg(target_os = "macos")]
            fonts_in_family.insert(1.min(fonts_in_family.len()), "cjk".to_owned());
            #[cfg(not(target_os = "macos"))]
            fonts_in_family.push("cjk".to_owned());
        }
        trace(&format!("[fonts] loaded CJK fallback: {}", path.display()));
    } else {
        trace(&format!(
            "[fonts] no CJK font found; checked: {}",
            CJK_FONT_CANDIDATES.join(", ")
        ));
    }
    ctx.set_fonts(fonts);
}

#[cfg(target_os = "macos")]
fn install_macos_ui_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style
        .text_styles
        .insert(egui::TextStyle::Heading, egui::FontId::proportional(18.0));
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(13.5));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(13.0));
    style
        .text_styles
        .insert(egui::TextStyle::Small, egui::FontId::proportional(11.5));
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.interact_size.y = 28.0;
    ctx.set_style(style);
}

/// 主题调色板（深/浅两套）。
#[derive(Clone, Copy)]
struct Palette {
    bg: egui::Color32,
    side: egui::Color32,
    panel: egui::Color32,
    /// 导航头色带：与消息区底色区分，底边另压主题色分隔线。
    head_fill: egui::Color32,
    head_border: egui::Color32,
    field: egui::Color32,
    border: egui::Color32,
    text: egui::Color32,
    dim: egui::Color32,
    accent: egui::Color32,
    hover: egui::Color32,
    btn_fill: egui::Color32,
    btn_hover: egui::Color32,
    btn_text: egui::Color32,
    btn_border: egui::Color32,
    user_bubble: egui::Color32,
    user_text: egui::Color32,
    ai_bubble: egui::Color32,
    tool_bubble: egui::Color32,
    err_bubble: egui::Color32,
    err_text: egui::Color32,
    warn: egui::Color32,
    banner_ok: egui::Color32,
    banner_warn: egui::Color32,
}

fn palette(dark: bool) -> Palette {
    use egui::Color32 as C;
    if dark {
        Palette {
            bg: C::from_rgb(0x0b, 0x0f, 0x14),
            side: C::from_rgb(0x10, 0x15, 0x1d),
            panel: C::from_rgb(0x0f, 0x15, 0x1d),
            // 导航头：深青蓝色带，与侧栏/消息区拉开层次；分隔线用主按钮同系青。
            head_fill: C::from_rgb(0x0f, 0x1e, 0x24),
            head_border: C::from_rgb(0x2f, 0x6b, 0x58),
            field: C::from_rgb(0x14, 0x1b, 0x24),
            border: C::from_rgb(0x22, 0x2c, 0x38),
            text: C::from_rgb(0xe6, 0xed, 0xf3),
            dim: C::from_rgb(0x8f, 0xa1, 0xb5),
            accent: C::from_rgb(0x5f, 0xe0, 0xb5),
            hover: C::from_rgb(0x1a, 0x25, 0x34),
            // 主按钮：低浓度青底 + 亮青文字，避免实心深底显得沉重。
            btn_fill: C::from_rgb(0x1b, 0x3a, 0x31),
            btn_hover: C::from_rgb(0x26, 0x51, 0x45),
            btn_text: C::from_rgb(0x8c, 0xec, 0xcd),
            btn_border: C::from_rgb(0x2f, 0x6b, 0x58),
            // 用户气泡：低饱和石板蓝灰，避免高饱和蓝底刺眼。
            user_bubble: C::from_rgb(0x25, 0x31, 0x42),
            user_text: C::from_rgb(0xe6, 0xed, 0xf3),
            ai_bubble: C::from_rgb(0x15, 0x1c, 0x26),
            tool_bubble: C::from_rgb(0x10, 0x16, 0x1d),
            err_bubble: C::from_rgb(0x33, 0x15, 0x1f),
            err_text: C::from_rgb(0xff, 0xa1, 0xb0),
            warn: C::from_rgb(0xff, 0xb8, 0x6b),
            banner_ok: C::from_rgb(0x16, 0x35, 0x2c),
            banner_warn: C::from_rgb(0x33, 0x2a, 0x12),
        }
    } else {
        Palette {
            // 回答区底色：纯白（浅灰识别度低，用户反馈后改为纯白）。
            bg: C::from_rgb(0xff, 0xff, 0xff),
            side: C::from_rgb(0xff, 0xff, 0xff),
            panel: C::from_rgb(0xff, 0xff, 0xff),
            // 导航头：淡青色带呼应主题绿，与纯白回答区形成层次。
            head_fill: C::from_rgb(0xe9, 0xf5, 0xef),
            head_border: C::from_rgb(0xa9, 0xd8, 0xc6),
            field: C::from_rgb(0xf2, 0xf5, 0xf9),
            border: C::from_rgb(0xd7, 0xdf, 0xe8),
            text: C::from_rgb(0x1a, 0x24, 0x30),
            dim: C::from_rgb(0x61, 0x70, 0x82),
            accent: C::from_rgb(0x0e, 0x8a, 0x67),
            hover: C::from_rgb(0xe8, 0xef, 0xf7),
            // 主按钮：淡青底 + 深青文字，轻盈不压版面。
            btn_fill: C::from_rgb(0xe2, 0xf4, 0xed),
            btn_hover: C::from_rgb(0xd0, 0xeb, 0xdf),
            btn_text: C::from_rgb(0x0c, 0x7a, 0x5b),
            btn_border: C::from_rgb(0xa9, 0xd8, 0xc6),
            // 用户气泡：淡灰蓝底 + 深色文字，与助手气泡区分但不刺眼。
            user_bubble: C::from_rgb(0xde, 0xe7, 0xf2),
            user_text: C::from_rgb(0x1a, 0x24, 0x30),
            ai_bubble: C::from_rgb(0xea, 0xef, 0xf5),
            tool_bubble: C::from_rgb(0xf0, 0xf4, 0xf9),
            err_bubble: C::from_rgb(0xfb, 0xe5, 0xe9),
            err_text: C::from_rgb(0xb3, 0x26, 0x3c),
            warn: C::from_rgb(0xc2, 0x6a, 0x00),
            banner_ok: C::from_rgb(0xe2, 0xf4, 0xed),
            banner_warn: C::from_rgb(0xfd, 0xf1, 0xdc),
        }
    }
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
            rt: UiRuntime::new("harness-ui-mem"),
        }
    }
}

#[derive(Clone)]
struct ChatMsg {
    kind: String, // user / assistant / tool / error
    label: String,
    text: String,
    /// assistant 气泡的未剥离原文累积（跨分片 strip_dsml 用），其它 kind 恒空。
    raw: String,
}

/// 插件管理列表行。
#[derive(Clone)]
struct PluginUiRow {
    id: String,
    name: String,
    /// 名称下方的简短说明（WASM 插件展示产物路径）。
    desc: String,
    /// 核心内置插件：默认启用且不可取消（系统必须保留必要能力）。
    core: bool,
    enabled: bool,
    /// 已实例化并执行 on_load，才算真正运行中。
    active: bool,
}

/// 核心内置插件（id, 名称, 描述）：默认勾选，禁止全部取消。
const BUILTIN_PLUGINS: &[(&str, &str, &str)] = &[
    ("local-files", "本地文件", "读取、搜索与写入工作区文件"),
    ("shell", "Shell", "在受限 Shell 中执行命令并回传输出"),
    ("git", "Git", "查看差异、提交历史与分支操作"),
    ("memory", "Memory", "跨会话记忆检索与沉淀"),
    ("hooks", "Hooks", "生命周期钩子：提交前检查等自动化"),
];

struct AppState {
    host: Arc<EguiUi>,
    log: Arc<SessionLog>,
    last_event: usize,
    messages: Vec<ChatMsg>,
    input: String,
    busy: bool,
    thinking: bool,
    /// 当前思考链累积文本（回合结束/正文到达时固化入气泡）。
    thinking_text: String,
    /// 本回合开始时刻：状态栏展示「已用时 Ns」，长等待不再像假死。
    turn_started: Option<std::time::Instant>,
    dark: bool,
    sidebar_expanded: bool,
    settings_open: bool,
    settings_page: String,
    // 模型设置表单
    f_provider: String,
    f_base: String,
    f_model: String,
    f_key: String,
    /// 思考档位 / 努力度（对齐 cc-switch thinkingLevelMap）：发给上游的 reasoning_effort 字符串。
    f_effort: String,
    // aidops 后端连接配置表单（对应 Config.aidops；留空则仅用本地文件记忆）
    f_aidops_base: String,
    f_aidops_key: String,
    f_aidops_project: String,
    profiles: Vec<String>,
    selected_profile: String,
    attachment: String,
    permission: String,
    /// 权限 chip 下拉菜单是否展开。用自定义 chip + Area 弹层代替默认 ComboBox，
    /// 保证与模型 chip 同 28px 高度 / 同圆角 / 同边框，水平基线对齐。
    perm_menu_open: bool,
    /// 插件管理列表（内置核心项恒启用 + 用户导入的 WASM 插件）。
    plugin_rows: Vec<PluginUiRow>,
    /// 上一帧记录的模态面板矩形：外部点击关闭时的内部误触守卫。
    modal_panel_rect: Option<egui::Rect>,
    /// 上一帧模态是否已打开：打开当帧的 press 是触发点击，不能当作“外部点击”关闭。
    modal_open_last_frame: bool,
    note: String,
    // 侧栏项目列表（上下文隔离 / 快速切换）
    projects: Vec<crate::ProjectRow>,
    active_project: String,
    // 会话历史（侧栏「历史记录」面板，跨项目聚合）
    history: Vec<SessionMeta>,
    /// 历史条目文件名 → 所属 sessions 目录（跨项目点击恢复定位用）。
    history_dirs: std::collections::HashMap<String, std::path::PathBuf>,
    history_search: String,
    /// 历史操作（精简 / 清空）反馈提示与展示起点（5 秒后自动隐去）。
    history_note: String,
    history_note_at: Option<std::time::Instant>,
    current_session: String,
    /// 会话重命名：正在编辑标题的会话文件名（弹窗编辑）。
    renaming: Option<String>,
    /// 重命名输入框缓冲。
    rename_buf: String,
    /// 版本更新状态（后台线程写、GUI 主循环读）。
    update_status: Arc<Mutex<UpdateStatus>>,
    /// 更新设置表单缓冲。
    f_update_url: String,
    f_update_channel: String,
    f_auto_check: bool,
    f_auto_install: bool,
    // 记忆面板状态（浏览本地原生记忆资产）
    mem_tab: String,
    mem_query: String,
    mem_loaded: bool,
    mem_items: Vec<MemItem>,
    /// 是否已对当前工作区执行过资产索引（首次打开记忆面板时自动执行一次）。
    mem_bootstrapped: bool,
    /// 最近一次索引/操作的反馈信息。
    mem_index_msg: String,
}

impl AppState {
    fn new(host: Arc<EguiUi>, log: Arc<SessionLog>) -> Self {
        let settings = &host.settings;
        let dark = settings.get("ui.theme").as_deref() != Some("light");
        // 活跃项目：优先 settings 持久化的上次选择，回退启动时工作区根；并确保入库。
        let active_project = settings
            .get("workspace.root")
            .filter(|p| std::path::Path::new(p).is_dir())
            .unwrap_or_else(|| host.workspace_root.clone());
        let _ = settings.add_project(std::path::Path::new(&active_project));
        let projects = settings.projects();
        // 首次升级时把旧的单模型配置迁移为模型配置列表。
        if settings.model_profiles().is_empty() {
            if let Some(key) = settings.get("llm.api_key") {
                let _ = settings.save_model_profile(&crate::ModelProfile {
                    name: format!("{} · {}", host.provider, host.model),
                    provider: host.provider.clone(),
                    base_url: host.base_url.clone(),
                    model: host.model.clone(),
                    api_key: key,
                });
            }
        }
        let mut state = Self {
            profiles: settings
                .model_profiles()
                .into_iter()
                .map(|p| p.name)
                .collect(),
            f_provider: host.provider.clone(),
            f_base: host.base_url.clone(),
            f_model: host.model.clone(),
            f_key: String::new(),
            f_effort: settings.get("llm.reasoning_effort").unwrap_or_default(),
            // aidops 后端连接：从 .harness.toml 的 [aidops] 段加载（无则空，仅用本地记忆）。
            f_aidops_base: Config::load()
                .map(|c| c.aidops.base_url)
                .unwrap_or_default(),
            f_aidops_key: Config::load()
                .ok()
                .and_then(|c| c.aidops.api_key)
                .unwrap_or_default(),
            f_aidops_project: Config::load()
                .ok()
                .and_then(|c| c.aidops.project_id)
                .map(|v| v.to_string())
                .unwrap_or_default(),
            selected_profile: String::new(),
            attachment: String::new(),
            permission: settings
                .get("permission.mode")
                .unwrap_or_else(|| "工作区写入".into()),
            plugin_rows: Self::load_plugin_rows(settings, &host.wasm_plugins),
            modal_panel_rect: None,
            modal_open_last_frame: false,
            last_event: 0,
            messages: vec![ChatMsg {
                kind: "assistant".into(),
                label: "助手".into(),
                text: READY.into(),
                raw: String::new(),
            }],
            input: String::new(),
            busy: false,
            thinking: false,
            thinking_text: String::new(),
            turn_started: None,
            dark,
            sidebar_expanded: true,
            settings_open: false,
            settings_page: "模型设置".into(),
            note: String::new(),
            projects,
            active_project,
            history: Vec::new(),
            history_dirs: std::collections::HashMap::new(),
            history_search: String::new(),
            history_note: String::new(),
            history_note_at: None,
            current_session: String::new(),
            renaming: None,
            rename_buf: String::new(),
            perm_menu_open: false,
            // 版本更新：节流 24h 的后台自动检查（manifest_url 为空时跳过）。
            update_status: {
                let s = Arc::new(Mutex::new(UpdateStatus::Idle));
                let auto_check = settings
                    .get("update.auto_check")
                    .map(|v| v == "true")
                    .unwrap_or(true);
                let last = settings
                    .get("update.last_check_ts")
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0);
                if auto_check && harness_core::update::now_secs().saturating_sub(last) > 24 * 3600 {
                    let url = settings
                        .get("update.manifest_url")
                        .unwrap_or_else(|| harness_core::update::DEFAULT_MANIFEST_URL.to_string());
                    let ch = settings
                        .get("update.channel")
                        .unwrap_or_else(|| "stable".into());
                    let skip = settings.get("update.skipped_version").unwrap_or_default();
                    harness_core::update::spawn_check(s.clone(), &url, &ch, &skip, false);
                    let _ = settings.set(
                        "update.last_check_ts",
                        &harness_core::update::now_secs().to_string(),
                    );
                }
                s
            },
            f_update_url: settings
                .get("update.manifest_url")
                .unwrap_or_else(|| harness_core::update::DEFAULT_MANIFEST_URL.to_string()),
            f_update_channel: settings
                .get("update.channel")
                .unwrap_or_else(|| "stable".into()),
            f_auto_check: settings
                .get("update.auto_check")
                .map(|v| v == "true")
                .unwrap_or(true),
            f_auto_install: settings
                .get("update.auto_install")
                .map(|v| v == "true")
                .unwrap_or(false),
            // 记忆面板状态（浏览本地原生记忆资产）
            mem_tab: String::new(),
            mem_query: String::new(),
            mem_loaded: false,
            mem_items: Vec::new(),
            mem_bootstrapped: false,
            mem_index_msg: String::new(),
            // host/log 放最后：上方字段仍需借用 host.settings，提前移入会报 E0505。
            host,
            log,
        };
        state.refresh_history();
        state
    }

    /// 经 `host.rt` 独立 runtime 查询四类资产服务，把结果填充到 `mem_items`。
    /// 查询为空时列出全部（list_*），否则按关键词匹配（match/query），保证面板默认可见。
    fn refresh_mem(&mut self) {
        let tab = self.mem_tab.clone();
        let query = self.mem_query.clone();
        // 会话 id 去掉 `.jsonl` 后缀：`recent_turns` 内部会自行拼接扩展名，
        // 带后缀会拼出 `xxx.jsonl.jsonl` 永远读不到文件，对话记忆轮次恒空。
        let session = self.current_session.trim_end_matches(".jsonl").to_string();
        let conv = self.host.conv.clone();
        let skill = self.host.skill.clone();
        let wiki = self.host.wiki.clone();
        let code = self.host.code.clone();
        // 关键修复：GUI 线程已处于 tokio 主 runtime 内，直接 block_on 会 panic 闪退。
        // 改在独立 OS 线程里 block_on（该线程无 runtime context，不重入），结果经 mpsc 回传。
        let handle = self.host.rt.handle();
        let (tx, rx) = std::sync::mpsc::channel::<Vec<MemItem>>();
        std::thread::spawn(move || {
            let items = handle.block_on(async move {
                let mut out: Vec<MemItem> = Vec::new();
                match tab.as_str() {
                    "chat" => {
                        if let Ok(facts) = conv.list_facts().await {
                            for f in facts {
                                let kind_label = match f.kind {
                                    FactKind::Preference => "偏好",
                                    FactKind::Decision => "决策",
                                    _ => "事实",
                                };
                                out.push(MemItem {
                                    title: format!("[{}] {}", f.layer.as_str(), kind_label),
                                    meta: f.id,
                                    body: f.content,
                                });
                            }
                        }
                        if let Ok(turns) = conv.recent_turns(&session, 50).await {
                            for t in turns {
                                out.push(MemItem {
                                    title: format!("{} / {}", t.role, t.session_id),
                                    meta: t.ts,
                                    body: t.content,
                                });
                            }
                        }
                    }
                    "skill" => {
                        let skills = if query.trim().is_empty() {
                            skill.list_skills().await.unwrap_or_default()
                        } else {
                            skill.match_skills(&query).await.unwrap_or_default()
                        };
                        for s in skills {
                            out.push(MemItem {
                                title: format!("{} ({})", s.name, s.version),
                                meta: s.id,
                                body: format!(
                                    "触发边界: {}\n步骤: {}",
                                    s.trigger_boundary,
                                    s.steps.join("；")
                                ),
                            });
                        }
                    }
                    "wiki" => {
                        let pages = if query.trim().is_empty() {
                            wiki.list_pages().await.unwrap_or_default()
                        } else {
                            wiki.query_pages(&query).await.unwrap_or_default()
                        };
                        for p in pages {
                            let body: String = p.blocks.join("\n");
                            let body = if body.chars().count() > 400 {
                                format!("{}…", body.chars().take(400).collect::<String>())
                            } else {
                                body
                            };
                            out.push(MemItem {
                                title: p.title,
                                meta: format!("{} 个链接", p.links.len()),
                                body,
                            });
                        }
                    }
                    "code" => {
                        let syms = if query.trim().is_empty() {
                            code.list_symbols().await.unwrap_or_default()
                        } else {
                            code.query_symbols(&query).await.unwrap_or_default()
                        };
                        for x in syms {
                            out.push(MemItem {
                                title: format!("{} @ {}", x.name, x.file),
                                meta: x.kind,
                                body: format!("{} ｜ 调用: {}", x.summary, x.calls.join(", ")),
                            });
                        }
                    }
                    _ => {}
                }
                out
            });
            let _ = tx.send(items);
        });
        if let Ok(items) = rx.recv() {
            self.mem_items = items;
        }
    }

    /// 对当前工作区执行一次资产索引（扫描 SKILL.md / *.md / 源码 → Skill/Wiki/CodeGraph），
    /// 并把已有对话文件合并为事实（consolidate 入口）。
    /// 结果通过四类资产服务落盘（原生文件实现或 aidops 后端），并刷新面板。
    fn bootstrap_mem(&mut self) {
        let conv = self.host.conv.clone();
        let skill = self.host.skill.clone();
        let wiki = self.host.wiki.clone();
        let code = self.host.code.clone();
        let ws = self.host.workspace_root.clone();
        let path = std::path::Path::new(&ws).to_path_buf();
        // 同 refresh_mem：在独立 OS 线程 block_on，避免 GUI 线程重入 runtime 导致闪退。
        let handle = self.host.rt.handle();
        let (tx, rx) = std::sync::mpsc::channel::<
            harness_core::error::Result<(harness_capability::index::IndexStats, usize)>,
        >();
        std::thread::spawn(move || {
            let res = handle.block_on(async move {
                let stats =
                    harness_capability::index::bootstrap_assets(&skill, &wiki, &code, &path)
                        .await?;
                // 事实合并：全链路无人调用 `consolidate`（对话记忆面板恒空），
                // 在 bootstrap 时对全部已有对话文件补做一次合并（按 id 去重、幂等）。
                let mut facts = 0usize;
                let conv_dir = path.join(".harness-memory").join("conversations");
                if let Ok(entries) = std::fs::read_dir(&conv_dir) {
                    for e in entries.flatten() {
                        let p = e.path();
                        if p.extension().is_some_and(|x| x == "jsonl") {
                            if let Some(sid) = p.file_stem().and_then(|s| s.to_str()) {
                                if let Ok(f) = conv.consolidate(sid).await {
                                    facts += f.len();
                                }
                            }
                        }
                    }
                }
                Ok((stats, facts))
            });
            let _ = tx.send(res);
        });
        match rx.recv() {
            Ok(Ok((stats, facts))) => {
                self.mem_index_msg = format!(
                    "已索引：{} 技能 / {} 文档 / {} 符号 / {} 事实",
                    stats.skills, stats.pages, stats.symbols, facts
                );
                self.mem_bootstrapped = true;
                self.mem_loaded = false; // 强制刷新
            }
            Ok(Err(e)) => {
                self.mem_index_msg = format!("索引失败: {e}");
            }
            Err(_) => {
                self.mem_index_msg = "索引失败: 后台任务未返回结果".into();
            }
        }
    }

    fn push(&mut self, kind: &str, label: &str, text: &str) {
        self.messages.push(ChatMsg {
            kind: kind.into(),
            label: label.into(),
            text: text.into(),
            raw: String::new(),
        });
    }

    fn append_assistant(&mut self, text: &str) {
        if text.starts_with("[error]") || text.starts_with("[已停止]") {
            self.push("error", "系统", text);
            return;
        }
        // 旧日志防御：按气泡「累积原文 + 整体重算剥离」。SSE 分片可能把 DSML 块
        // 切成多段，逐片 strip 无法移除跨片块；整体重算保证块完整后即被剥离。
        if let Some(last) = self.messages.last_mut() {
            if last.kind == "assistant" {
                last.raw.push_str(text);
                last.text = harness_llm::dsml::strip_dsml(&last.raw);
                return;
            }
        }
        let raw = text.to_string();
        let stripped = harness_llm::dsml::strip_dsml(&raw);
        self.messages.push(ChatMsg {
            kind: "assistant".into(),
            label: "助手".into(),
            text: stripped,
            raw,
        });
    }

    /// 当前思考档位（reasoning_effort）的 Option 形态：空串视作 None。
    fn effort(&self) -> Option<String> {
        let e = self.f_effort.trim();
        if e.is_empty() {
            None
        } else {
            Some(e.to_string())
        }
    }

    /// 轮询 SessionLog 真相源，把新事件转成气泡。
    fn poll_log(&mut self) {
        let (next, events) = self.log.replay_from(self.last_event);
        if events.is_empty() {
            return;
        }
        for event in &events {
            match event {
                SessionEvent::TurnStart { input, .. } => self.push("user", "你", input),
                SessionEvent::Assistant { chunk, .. } => {
                    if let Some(text) = &chunk.text {
                        self.finalize_thinking();
                        self.append_assistant(text);
                    }
                }
                SessionEvent::Thinking { text, .. } => {
                    // 思考链增量：累积全文并实时覆盖尾部「思考」气泡（只展示最近几十字，
                    // 不刷屏），长推理期用户能看到内容在滚动，而不是只剩状态栏一个标志。
                    self.thinking = true;
                    self.thinking_text.push_str(text);
                    if self.thinking_text.chars().count() > 400 {
                        let total = self.thinking_text.chars().count();
                        let keep: String = self.thinking_text.chars().skip(total - 400).collect();
                        self.thinking_text = keep;
                    }
                    self.update_thinking_bubble();
                }
                SessionEvent::ToolCall { call, .. } => {
                    self.finalize_thinking();
                    // 参数摘要（≤120 字）：agent 行为全程可见。
                    let summary: String = call.args.to_string().chars().take(120).collect();
                    self.push("tool", "工具", &format!("调用 {}: {}", call.name, summary));
                }
                SessionEvent::ToolResult { result, .. } => {
                    let preview: String = result.content.chars().take(400).collect();
                    self.push(
                        "tool",
                        "工具",
                        &format!("{} 返回: {}", if result.ok { "✓" } else { "✗" }, preview),
                    );
                }
                SessionEvent::PlanUpdate { items, .. } => {
                    let mut s = String::from("[计划]\n");
                    for (i, item) in items.iter().enumerate() {
                        let mark = match item.status.as_str() {
                            "done" => "✓",
                            "doing" => "…",
                            _ => "·",
                        };
                        s.push_str(&format!("{}. {} {}\n", i + 1, mark, item.text));
                    }
                    self.push("plan", "计划", s.trim_end());
                }
                SessionEvent::TurnEnd { .. } => {
                    self.finalize_thinking();
                    self.turn_started = None;
                    // 回合已完整落盘：刷新历史列表（mtime / 标题可能变化）。
                    self.refresh_history();
                }
                _ => {}
            }
        }
        self.last_event = next;
        trace(&format!("[log] +{} events processed", events.len()));
    }

    /// 思考直播气泡：消息流尾部增量覆盖，只展示最近几十字；
    /// 正文/工具事件到达后由 finalize_thinking 固化为完整内容。
    fn update_thinking_bubble(&mut self) {
        let preview: String = self
            .thinking_text
            .chars()
            .rev()
            .take(60)
            .collect::<Vec<char>>()
            .into_iter()
            .rev()
            .collect();
        let text = format!("💭 {preview}…");
        if let Some(last) = self.messages.last_mut() {
            if last.kind == "thinking" {
                last.text = text;
                return;
            }
        }
        self.push("thinking", "思考", &text);
    }

    /// 思考结束：把完整思考内容固化为灰色气泡（保留尾部 ≤500 字），
    /// 直接替换直播气泡避免重复；供正文/工具调用/回合结束调用。
    fn finalize_thinking(&mut self) {
        self.thinking = false;
        if self.thinking_text.is_empty() {
            return;
        }
        let mut t = std::mem::take(&mut self.thinking_text);
        let total = t.chars().count();
        if total > 500 {
            let tail: String = t.chars().skip(total - 500).collect();
            t = format!("…{tail}");
        }
        if let Some(last) = self.messages.last_mut() {
            if last.kind == "thinking" {
                last.text = t;
                return;
            }
        }
        self.push("thinking", "思考", &t);
    }

    fn submit(&mut self) {
        let mut text = self.input.trim().to_string();
        if text.is_empty() || self.busy {
            return;
        }
        if !self.attachment.trim().is_empty() {
            text.push_str(&format!("\n\n[附件: {}]", self.attachment));
        }
        let sink = self.host.sink.clone();
        sink.set_permission(self.permission.clone());
        let settings = &self.host.settings;
        let _ = settings.set("permission.mode", &self.permission);
        let _ = settings.set("llm.model", &self.f_model);
        let _ = settings.set("llm.provider", &self.f_provider);
        if let (Some(base), Some(key)) = (settings.get("llm.base_url"), settings.get("llm.api_key"))
        {
            let _ = self.host.llm_control.configure_provider(
                self.f_provider.clone(),
                base,
                self.f_model.clone(),
                key,
                self.effort(),
            );
        }
        // 用户气泡不在此本地推入：TurnStart 事件是真相源，poll_log 会渲染，
        // 本地再推一条会导致同一问题显示两次。
        trace(&format!("[send] you: {text}"));
        self.input.clear();
        self.busy = true;
        self.thinking_text.clear();
        self.turn_started = Some(std::time::Instant::now());
        sink.submit(text);
    }

    fn new_session(&mut self) {
        self.host.sink.new_session();
        self.current_session = self
            .log
            .path()
            .and_then(|p| p.file_name().map(|f| f.to_string_lossy().to_string()))
            .unwrap_or_default();
        self.last_event = 0;
        self.messages = vec![ChatMsg {
            kind: "assistant".into(),
            label: "助手".into(),
            text: READY.into(),
            raw: String::new(),
        }];
        self.thinking_text.clear();
        self.turn_started = None;
        self.refresh_history();
        trace("[session] new session");
    }

    /// 历史面板数据刷新：跨项目聚合全部会话（各项目 sessions 目录 +
    /// 当前日志目录），mtime 倒序；并记录每条所属目录与活跃会话文件名。
    fn refresh_history(&mut self) {
        let mut seen_dirs: std::collections::HashSet<std::path::PathBuf> =
            std::collections::HashSet::new();
        let mut dirs: Vec<std::path::PathBuf> = Vec::new();
        for proj in self.host.settings.projects() {
            if proj.archived {
                continue;
            }
            let d = std::path::Path::new(&proj.path)
                .join(".harness")
                .join("sessions");
            if d.is_dir() {
                if let Ok(c) = d.canonicalize() {
                    if seen_dirs.insert(c) {
                        dirs.push(d);
                    }
                }
            }
        }
        if let Some(d) = self.log.dir() {
            if let Ok(c) = d.canonicalize() {
                if seen_dirs.insert(c) {
                    dirs.push(d);
                }
            }
        }
        let mut all: Vec<SessionMeta> = Vec::new();
        let mut map = std::collections::HashMap::new();
        for d in &dirs {
            for m in harness_session::list_sessions(d) {
                map.insert(m.file.clone(), d.clone());
                all.push(m);
            }
        }
        all.sort_by(|a, b| b.mtime.cmp(&a.mtime));
        self.history = all;
        self.history_dirs = map;
        self.current_session = self
            .log
            .path()
            .and_then(|p| p.file_name().map(|f| f.to_string_lossy().to_string()))
            .unwrap_or_default();
    }

    /// 点击恢复历史会话：支持跨项目——目标会话属于其他项目时先切工作区根
    /// （对齐侧栏项目切换），再 SessionLog 切到该文件继续追加；
    /// poll_log 下帧从 0 重放全部消息流。忙碌时拒绝避免回合穿插。
    fn switch_session(&mut self, file: &str) {
        trace(&format!(
            "[session] restore attempt {file} busy={}",
            self.busy
        ));
        if self.busy {
            return;
        }
        let Some(dir) = self.history_dirs.get(file).cloned() else {
            return;
        };
        // 跨项目会话：先切工作区根（工具上下文与项目列表同步），
        // switch_workspace 会重载该目录最近会话，随后再精确定位到目标文件。
        if self.log.dir().as_ref() != Some(&dir) {
            if let Some(root) = dir.parent().and_then(|p| p.parent()) {
                let path = root.display().to_string();
                let _ = self.host.settings.set("workspace.root", &path);
                self.host.sink.switch_workspace(root);
                self.active_project = path;
                self.projects = self.host.settings.projects();
            }
        }
        if !self.log.switch_to_file(&dir, file) {
            return;
        }
        self.current_session = file.to_string();
        self.last_event = 0;
        self.messages = vec![ChatMsg {
            kind: "assistant".into(),
            label: "助手".into(),
            text: READY.into(),
            raw: String::new(),
        }];
        self.input.clear();
        self.thinking = false;
        self.thinking_text.clear();
        self.turn_started = None;
        trace(&format!("[session] restored {file}"));
    }

    /// 删除单条历史会话；删当前活跃会话时先新建空会话接替。
    fn delete_session_entry(&mut self, file: &str) {
        if self.busy {
            return;
        }
        let Some(dir) = self.history_dirs.get(file).cloned() else {
            return;
        };
        if file == self.current_session {
            self.new_session();
        }
        harness_session::delete_session(&dir, file);
        self.refresh_history();
        trace(&format!("[session] deleted {file}"));
    }

    /// 清空全部历史：删除活跃会话之外的全部会话文件（跨项目）。
    fn clear_history(&mut self) {
        if self.busy {
            return;
        }
        if self.current_session.is_empty() {
            self.new_session();
        }
        let victims: Vec<(String, std::path::PathBuf)> = self
            .history
            .iter()
            .filter(|m| m.file != self.current_session)
            .filter_map(|m| {
                self.history_dirs
                    .get(&m.file)
                    .map(|d| (m.file.clone(), d.clone()))
            })
            .collect();
        for (f, dir) in &victims {
            harness_session::delete_session(dir, f);
        }
        self.refresh_history();
        self.history_note = if victims.is_empty() {
            "没有可清空的历史会话".into()
        } else {
            format!("已清空：删除 {} 个历史会话", victims.len())
        };
        self.history_note_at = Some(std::time::Instant::now());
        trace("[session] history cleared");
    }

    /// 精简历史：每个 sessions 目录仅保留最近 30 个会话（按 mtime），当前活跃会话永不删。
    fn prune_history(&mut self) {
        if self.busy {
            return;
        }
        let keep: usize = 30;
        let before = self.history.len();
        let active = if self.current_session.is_empty() {
            None
        } else {
            Some(self.current_session.as_str())
        };
        let dirs: std::collections::HashSet<std::path::PathBuf> =
            self.history_dirs.values().cloned().collect();
        for d in &dirs {
            harness_session::prune_sessions(d, keep, active);
        }
        self.refresh_history();
        let removed = before.saturating_sub(self.history.len());
        // 明确反馈：会话数未超上限时也要告知「无需精简」，避免「点了没反应」的观感。
        self.history_note = if removed == 0 {
            "无需精简：每个项目会话均未超过 30 个".into()
        } else {
            format!("已精简：删除 {removed} 个旧会话（当前对话保留）")
        };
        self.history_note_at = Some(std::time::Instant::now());
        trace("[session] history pruned");
    }

    /// 侧栏项目切换：换工作区根 + 重载该项目会话历史 + 清空输入框 + 气泡提示
    ///（对齐 Codex/Cursor 的上下文切换交互）。忙碌时拒绝以避免回合穿插写错日志。
    fn switch_project(&mut self, path: &str) {
        if path == self.active_project || self.busy {
            return;
        }
        let p = std::path::PathBuf::from(path);
        let name = p
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("项目")
            .to_string();
        let _ = self.host.settings.set("workspace.root", path);
        let _ = self.host.settings.add_project(&p);
        // 反向通道：Workspace 换根 + SessionLog 重载新项目目录的最近会话。
        self.host.sink.switch_workspace(&p);
        self.active_project = path.to_string();
        self.projects = self.host.settings.projects();
        // 视图复位：poll_log 下帧从 0 重放，右侧消息流刷新为该项目历史。
        self.last_event = 0;
        self.messages = vec![ChatMsg {
            kind: "assistant".into(),
            label: "助手".into(),
            text: READY.into(),
            raw: String::new(),
        }];
        self.input.clear();
        self.thinking = false;
        self.thinking_text.clear();
        self.push("tool", "系统", &format!("已切换到项目 {name}"));
        self.refresh_history();
        trace(&format!("[project] switched to {path}"));
    }

    fn apply_model(&mut self) {
        let settings = &self.host.settings;
        let key = if self.f_key.trim().is_empty() {
            settings.get("llm.api_key").unwrap_or_default()
        } else {
            std::mem::take(&mut self.f_key)
        };
        let result = self.host.llm_control.configure_provider(
            self.f_provider.clone(),
            self.f_base.clone(),
            self.f_model.clone(),
            key.clone(),
            self.effort(),
        );
        match result {
            Ok(()) => {
                let _ = settings.set("llm.base_url", &self.f_base);
                let _ = settings.set("llm.model", &self.f_model);
                let _ = settings.set("llm.provider", &self.f_provider);
                let _ = settings.set_secret("llm.api_key", &key);
                let _ = settings.set("llm.reasoning_effort", &self.f_effort);
                let name = format!("{} · {}", self.f_provider, self.f_model);
                let _ = settings.save_model_profile(&crate::ModelProfile {
                    name: name.clone(),
                    provider: self.f_provider.clone(),
                    base_url: self.f_base.clone(),
                    model: self.f_model.clone(),
                    api_key: key,
                });
                self.profiles = settings
                    .model_profiles()
                    .into_iter()
                    .map(|p| p.name)
                    .collect();
                self.selected_profile = name;
                self.note = "模型配置已保存并应用".into();
                // 不自动关闭：note 在弹窗内可见，给用户明确的保存反馈。
                trace("[config] model configuration applied");
            }
            Err(error) => {
                self.note = format!("配置错误: {error}");
                trace(&format!("[config] rejected: {error}"));
            }
        }
    }

    // ── 版本更新：顶部横幅 ─────────────────────────────────────
    /// 中央面板顶部横幅：展示检查中 / 新版本提示 / 下载进度 / 待重启 / 错误。
    fn draw_update_banner(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let status = self
            .update_status
            .lock()
            .map(|g| g.clone())
            .unwrap_or(UpdateStatus::Idle);
        // 读取升级策略：自动安装开启时，「立即升级」走下载+重启；否则打开下载页。
        let auto_install = cfg!(windows)
            && self
                .host
                .settings
                .get("update.auto_install")
                .map(|v| v == "true")
                .unwrap_or(false);
        match status {
            UpdateStatus::Idle | UpdateStatus::UpToDate => {}
            UpdateStatus::Checking => {
                ui.label(
                    egui::RichText::new("正在检查更新…")
                        .size(12.0)
                        .color(pal.dim),
                );
                ui.add_space(8.0);
            }
            UpdateStatus::Error(e) => {
                ui.label(
                    egui::RichText::new(format!("更新检查失败：{e}"))
                        .size(12.0)
                        .color(pal.warn),
                );
                ui.add_space(8.0);
            }
            UpdateStatus::Downloading => {
                ui.label(
                    egui::RichText::new("正在下载新版本…")
                        .size(12.0)
                        .color(pal.accent),
                );
                ui.add_space(8.0);
            }
            UpdateStatus::ReadyToRestart { version, .. } => {
                egui::Frame::default()
                    .fill(pal.banner_ok)
                    .rounding(egui::Rounding::same(10.0))
                    .inner_margin(egui::Margin::symmetric(12.0, 8.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!("已下载 v{version}，重启后生效"))
                                    .size(12.5)
                                    .strong()
                                    .color(pal.text),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if accent_button(ui, &pal, "重启") {
                                        self.restart_to_apply_update();
                                    }
                                },
                            );
                        });
                    });
                ui.add_space(8.0);
            }
            UpdateStatus::Available(rel) => {
                let mandatory = rel.mandatory.unwrap_or(false);
                egui::Frame::default()
                    .fill(pal.banner_warn)
                    .rounding(egui::Rounding::same(10.0))
                    .inner_margin(egui::Margin::symmetric(12.0, 8.0))
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "发现新版本 v{}（当前 v{}）",
                                        rel.version,
                                        harness_core::update::CURRENT_VERSION
                                    ))
                                    .size(13.0)
                                    .strong()
                                    .color(pal.text),
                                );
                                if mandatory {
                                    ui.label(
                                        egui::RichText::new("· 必须更新")
                                            .size(11.0)
                                            .color(pal.warn),
                                    );
                                }
                            });
                            if let Some(notes) = &rel.notes {
                                ui.label(egui::RichText::new(notes).size(11.5).color(pal.dim));
                            }
                            ui.horizontal(|ui| {
                                if auto_install {
                                    if accent_button(ui, &pal, "立即升级") {
                                        harness_core::update::spawn_download(
                                            self.update_status.clone(),
                                            rel.clone(),
                                        );
                                    }
                                } else if accent_button(ui, &pal, "立即升级") {
                                    harness_core::update::open_url(&rel.url);
                                }
                                if ghost_button(ui, &pal, "查看下载页") {
                                    harness_core::update::open_url(&rel.url);
                                }
                                if !mandatory {
                                    if ghost_button(ui, &pal, "稍后") {
                                        if let Ok(mut g) = self.update_status.lock() {
                                            *g = UpdateStatus::UpToDate;
                                        }
                                    }
                                    if ghost_button(ui, &pal, "忽略此版本") {
                                        let _ = self
                                            .host
                                            .settings
                                            .set("update.skipped_version", &rel.version);
                                        if let Ok(mut g) = self.update_status.lock() {
                                            *g = UpdateStatus::UpToDate;
                                        }
                                    }
                                }
                            });
                        });
                    });
                ui.add_space(8.0);
            }
        }
    }

    /// 「更新」设置页：清单 URL / 通道 / 自动开关 / 立即检查 / 当前版本。
    fn draw_update_settings(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        field_label(
            ui,
            &pal,
            &format!("当前版本：v{}", harness_core::update::CURRENT_VERSION),
        );
        ui.add_space(6.0);

        field_label(ui, &pal, "清单 URL（manifest.json）");
        let _ = ui.text_edit_singleline(&mut self.f_update_url);
        ui.label(
            egui::RichText::new("远端返回的 JSON 含 version / url / 可选 notes·sha256·mandatory。可放任意静态托管（COS / 对象存储 / nginx / 内网文件服务）。支持简写 github:owner/repo（自动解析为 raw.githubusercontent.com 直链，无需 GitHub API 令牌）。")
                .size(11.0)
                .color(pal.dim),
        );
        ui.add_space(8.0);

        field_label(ui, &pal, "更新通道");
        egui::ComboBox::from_id_salt("update-channel")
            .width(200.0)
            .selected_text(&self.f_update_channel)
            .show_ui(ui, |ui| {
                for ch in ["stable", "beta"] {
                    ui.selectable_value(&mut self.f_update_channel, ch.to_string(), ch);
                }
            });
        ui.add_space(8.0);

        let _ = ui.checkbox(&mut self.f_auto_check, "自动检查更新（启动后节流 24 小时）");
        ui.add_enabled_ui(cfg!(windows), |ui| {
            let _ = ui.checkbox(
                &mut self.f_auto_install,
                "自动下载并安装（当前仅 Windows 支持）",
            );
        });
        ui.add_space(10.0);

        ui.horizontal(|ui| {
            if accent_button(ui, &pal, "保存更新设置") {
                let s = &self.host.settings;
                let _ = s.set("update.manifest_url", &self.f_update_url.trim());
                let _ = s.set("update.channel", &self.f_update_channel);
                let _ = s.set("update.auto_check", &self.f_auto_check.to_string());
                let _ = s.set("update.auto_install", &self.f_auto_install.to_string());
                self.note = "更新设置已保存".into();
            }
            if ghost_button(ui, &pal, "立即检查") {
                let skip = self
                    .host
                    .settings
                    .get("update.skipped_version")
                    .unwrap_or_default();
                harness_core::update::spawn_check(
                    self.update_status.clone(),
                    &self.f_update_url.trim(),
                    &self.f_update_channel,
                    &skip,
                    true,
                );
            }
        });
        ui.add_space(10.0);

        // 设置页内也展示当前更新状态与操作。
        self.draw_update_banner(ui, pal);
    }

    /// 触发待升级替换并重启（下载完成后点「重启」调用）。
    fn restart_to_apply_update(&self) {
        if let Some(exe) = std::env::current_exe().ok() {
            if let Some(dir) = exe.parent() {
                harness_core::update::try_apply_and_relaunch(dir);
            }
        }
    }

    fn load_profile(&mut self, name: &str) {
        let Some(profile) = self
            .host
            .settings
            .model_profiles()
            .into_iter()
            .find(|p| p.name == name)
        else {
            return;
        };
        self.f_provider = profile.provider.clone();
        self.f_base = profile.base_url.clone();
        self.f_model = profile.model.clone();
        match self.host.llm_control.configure_provider(
            profile.provider,
            profile.base_url,
            profile.model,
            profile.api_key,
            self.effort(),
        ) {
            Ok(()) => self.note = self.host.llm_control.status(),
            Err(error) => self.note = format!("配置错误: {error}"),
        }
    }

    fn save_preferences(&mut self) {
        let settings = &self.host.settings;
        let _ = settings.set("permission.mode", &self.permission);
        for row in &self.plugin_rows {
            let _ = settings.set_plugin_enabled(&row.id, &row.name, row.enabled);
        }
        self.host.sink.set_permission(self.permission.clone());
        self.note = "偏好已保存".into();
        // 不自动关闭：note 在弹窗内可见，给用户明确的保存反馈。
    }

    /// 构建插件列表：核心内置恒启用（忽略历史禁用记录）；WASM 插件读持久化状态。
    fn load_plugin_rows(
        settings: &crate::SettingsDb,
        runtime: &harness_provider_wasm::WasmPluginRuntime,
    ) -> Vec<PluginUiRow> {
        let mut rows: Vec<PluginUiRow> = BUILTIN_PLUGINS
            .iter()
            .map(|(id, name, desc)| PluginUiRow {
                id: (*id).into(),
                name: (*name).into(),
                desc: (*desc).into(),
                core: true,
                enabled: true,
                active: true,
            })
            .collect();
        for p in settings.plugins() {
            if let Some(path) = p.path {
                let active = runtime.is_active(&p.id);
                rows.push(PluginUiRow {
                    id: p.id,
                    name: p.name,
                    desc: path,
                    core: false,
                    enabled: p.enabled,
                    active,
                });
            }
        }
        rows
    }

    /// 导入 WASM 插件入口：先经 `harness-provider-wasm` 的 wasmtime 沙箱校验再登记。
    fn import_wasm_plugin(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("选择 WASM 插件")
            .add_filter("WASM 插件", &["wasm", "wat"])
            .pick_file()
        else {
            return;
        };
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("plugin")
            .to_string();
        let id = format!("wasm:{stem}");
        let path_s = path.display().to_string();
        // 真正启用：在零直接能力的 Wasmtime 容器中实例化并调用可选 on_load。
        if let Err(e) = self.host.wasm_plugins.activate(&id, &path) {
            self.note = format!("插件校验或启用失败: {e}");
            return;
        }
        if let Err(e) = self.host.settings.add_wasm_plugin(&id, &stem, &path_s) {
            let _ = self.host.wasm_plugins.deactivate(&id);
            self.note = format!("登记插件失败: {e}");
            return;
        }
        if let Some(row) = self.plugin_rows.iter_mut().find(|row| row.id == id) {
            row.name = stem.clone();
            row.desc = path_s;
            row.enabled = true;
            row.active = true;
        } else {
            self.plugin_rows.push(PluginUiRow {
                id,
                name: stem.clone(),
                desc: path_s,
                core: false,
                enabled: true,
                active: true,
            });
        }
        self.note = format!("插件「{stem}」已通过沙箱校验并登记，默认启用");
    }
}

/// 侧栏功能图标：矢量线条绘制（不依赖字体字形，CJK 字体缺字也不会变豆腐块）。
#[derive(Clone, Copy)]
enum Icon {
    Chat,
    Folder,
    Layers,
    Chip,
    Gear,
    Menu,
    Update,
}

fn draw_icon(painter: &egui::Painter, center: egui::Pos2, icon: Icon, color: egui::Color32) {
    let r = egui::Rect::from_center_size(center, egui::vec2(16.0, 16.0));
    let stroke = egui::Stroke::new(1.5_f32, color);
    let thin = egui::Stroke::new(1.1_f32, color);
    match icon {
        // 对话气泡 + 内部文本线
        Icon::Chat => {
            let body = egui::Rect::from_min_size(r.min, egui::vec2(r.width(), r.height() * 0.7));
            painter.rect(
                body,
                egui::Rounding::same(3.0),
                egui::Color32::TRANSPARENT,
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(body.min.x + 4.5, body.max.y),
                    egui::pos2(body.min.x + 3.0, body.max.y + 3.2),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(body.min.x + 3.0, body.max.y + 3.2),
                    egui::pos2(body.min.x + 8.0, body.max.y),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(body.min.x + 3.0, body.center().y - 1.4),
                    egui::pos2(body.max.x - 3.0, body.center().y - 1.4),
                ],
                thin,
            );
            painter.line_segment(
                [
                    egui::pos2(body.min.x + 3.0, body.center().y + 1.4),
                    egui::pos2(body.max.x - 5.5, body.center().y + 1.4),
                ],
                thin,
            );
        }
        // 文件夹（带左上标签页）
        Icon::Folder => {
            let tab = r.height() * 0.26;
            let pts = vec![
                egui::pos2(r.min.x, r.max.y - 1.0),
                egui::pos2(r.min.x, r.min.y + 1.0),
                egui::pos2(r.min.x + r.width() * 0.42, r.min.y + 1.0),
                egui::pos2(r.min.x + r.width() * 0.52, r.min.y + tab),
                egui::pos2(r.max.x, r.min.y + tab),
                egui::pos2(r.max.x, r.max.y - 1.0),
            ];
            painter.add(egui::Shape::closed_line(pts, stroke));
        }
        // 插件：两个错位叠放的卡片
        Icon::Layers => {
            let back = egui::Rect::from_min_size(
                r.min + egui::vec2(3.0, 0.0),
                egui::vec2(r.width() - 3.0, r.height() - 3.0),
            );
            let front = egui::Rect::from_min_size(
                r.min + egui::vec2(0.0, 3.0),
                egui::vec2(r.width() - 3.0, r.height() - 3.0),
            );
            painter.rect(
                back,
                egui::Rounding::same(2.5),
                egui::Color32::TRANSPARENT,
                thin,
            );
            painter.rect(
                front,
                egui::Rounding::same(2.5),
                egui::Color32::TRANSPARENT,
                stroke,
            );
        }
        // 模型：芯片（内外框 + 四边引脚）
        Icon::Chip => {
            let c = r.center();
            let outer = egui::Rect::from_center_size(c, egui::vec2(10.5, 10.5));
            let inner = egui::Rect::from_center_size(c, egui::vec2(4.5, 4.5));
            painter.rect(
                outer,
                egui::Rounding::same(2.0),
                egui::Color32::TRANSPARENT,
                stroke,
            );
            painter.rect(inner, egui::Rounding::same(1.0), color, egui::Stroke::NONE);
            for i in 0..3 {
                let off = -3.25 + i as f32 * 3.25;
                painter.line_segment(
                    [
                        egui::pos2(c.x + off, outer.min.y - 2.5),
                        egui::pos2(c.x + off, outer.min.y),
                    ],
                    thin,
                );
                painter.line_segment(
                    [
                        egui::pos2(c.x + off, outer.max.y),
                        egui::pos2(c.x + off, outer.max.y + 2.5),
                    ],
                    thin,
                );
                painter.line_segment(
                    [
                        egui::pos2(outer.min.x - 2.5, c.y + off),
                        egui::pos2(outer.min.x, c.y + off),
                    ],
                    thin,
                );
                painter.line_segment(
                    [
                        egui::pos2(outer.max.x, c.y + off),
                        egui::pos2(outer.max.x + 2.5, c.y + off),
                    ],
                    thin,
                );
            }
        }
        // 设置：齿轮（圆环 + 8 根辐条）
        Icon::Gear => {
            let c = r.center();
            painter.circle(c, 3.2, egui::Color32::TRANSPARENT, stroke);
            painter.circle(c, 1.1, color, egui::Stroke::NONE);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::TAU / 8.0;
                let (sx, sy) = (a.sin(), a.cos());
                painter.line_segment(
                    [
                        egui::pos2(c.x + sx * 4.8, c.y + sy * 4.8),
                        egui::pos2(c.x + sx * 6.8, c.y + sy * 6.8),
                    ],
                    stroke,
                );
            }
        }
        // 汉堡菜单（收起/展开侧栏）
        Icon::Menu => {
            for i in 0..3 {
                let y = r.min.y + 3.0 + i as f32 * 4.5;
                painter.line_segment(
                    [egui::pos2(r.min.x + 1.0, y), egui::pos2(r.max.x - 1.0, y)],
                    stroke,
                );
            }
        }
        // 更新：环形箭头（刷新语义）
        Icon::Update => {
            let c = r.center();
            let rad = 5.2;
            // 四段弧线围成近圆环
            for q in 0..4 {
                let a0 = q as f32 * std::f32::consts::FRAC_PI_2 + 0.4;
                let a1 = a0 + std::f32::consts::FRAC_PI_2 - 0.8;
                let steps = 8;
                for s in 0..steps {
                    let t0 = a0 + (a1 - a0) * s as f32 / steps as f32;
                    let t1 = a0 + (a1 - a0) * (s + 1) as f32 / steps as f32;
                    painter.line_segment(
                        [
                            egui::pos2(c.x + t0.sin() * rad, c.y - t0.cos() * rad),
                            egui::pos2(c.x + t1.sin() * rad, c.y - t1.cos() * rad),
                        ],
                        stroke,
                    );
                }
            }
            // 箭头头部（指向右上）
            let head = egui::pos2(c.x + 0.4_f32.sin() * rad, c.y - 0.4_f32.cos() * rad);
            painter.line_segment([egui::pos2(head.x - 2.4, head.y - 1.6), head], stroke);
            painter.line_segment([egui::pos2(head.x - 0.4, head.y - 2.6), head], stroke);
        }
    }
}

/// 历史条目「删除」小图标：垃圾桶（矢量，语义明确不会被误认成关闭）。
fn draw_trash_icon(painter: &egui::Painter, c: egui::Pos2, color: egui::Color32) {
    let s = egui::Stroke::new(1.2_f32, color);
    // 盖沿 + 提手
    painter.line_segment(
        [
            egui::pos2(c.x - 4.5, c.y - 2.8),
            egui::pos2(c.x + 4.5, c.y - 2.8),
        ],
        s,
    );
    painter.line_segment(
        [
            egui::pos2(c.x - 1.8, c.y - 4.6),
            egui::pos2(c.x + 1.8, c.y - 4.6),
        ],
        s,
    );
    // 桶身（略收底）
    painter.add(egui::Shape::closed_line(
        vec![
            egui::pos2(c.x - 3.4, c.y - 2.0),
            egui::pos2(c.x + 3.4, c.y - 2.0),
            egui::pos2(c.x + 2.7, c.y + 4.6),
            egui::pos2(c.x - 2.7, c.y + 4.6),
        ],
        s,
    ));
    // 桶身竖纹
    painter.line_segment([egui::pos2(c.x, c.y - 0.6), egui::pos2(c.x, c.y + 3.2)], s);
}

/// 历史条目「重命名」小图标：铅笔（矢量）。
fn draw_pencil_icon(painter: &egui::Painter, c: egui::Pos2, color: egui::Color32) {
    let s = egui::Stroke::new(1.2_f32, color);
    // 笔身（左下笔尖 → 右上笔尾）
    painter.line_segment(
        [
            egui::pos2(c.x - 3.8, c.y + 3.8),
            egui::pos2(c.x + 3.4, c.y - 3.4),
        ],
        s,
    );
    // 笔尾加粗端
    painter.line_segment(
        [
            egui::pos2(c.x + 2.2, c.y - 4.6),
            egui::pos2(c.x + 4.6, c.y - 2.2),
        ],
        s,
    );
    // 笔尖三角
    painter.add(egui::Shape::closed_line(
        vec![
            egui::pos2(c.x - 3.8, c.y + 3.8),
            egui::pos2(c.x - 1.6, c.y + 3.2),
            egui::pos2(c.x - 3.2, c.y + 1.6),
        ],
        s,
    ));
}

/// 附件图标：矢量回形针（嵌套 U，外圈右臂短于左臂形成"夹口"）。
/// 与 draw_pencil_icon 同一风格，不依赖字体字形。
fn draw_paperclip_icon(painter: &egui::Painter, c: egui::Pos2, color: egui::Color32) {
    let s = egui::Stroke::new(1.2_f32, color);
    // 外圈 U（左 → 下 → 右，右臂短）
    painter.line_segment(
        [
            egui::pos2(c.x - 3.6, c.y - 3.8),
            egui::pos2(c.x - 3.6, c.y + 4.2),
        ],
        s,
    );
    painter.line_segment(
        [
            egui::pos2(c.x - 3.6, c.y + 4.2),
            egui::pos2(c.x + 3.6, c.y + 4.2),
        ],
        s,
    );
    painter.line_segment(
        [
            egui::pos2(c.x + 3.6, c.y + 4.2),
            egui::pos2(c.x + 3.6, c.y - 1.4),
        ],
        s,
    );
    // 内圈 U（更短，开口向上）
    painter.line_segment(
        [
            egui::pos2(c.x - 1.6, c.y - 1.4),
            egui::pos2(c.x - 1.6, c.y + 2.0),
        ],
        s,
    );
    painter.line_segment(
        [
            egui::pos2(c.x - 1.6, c.y + 2.0),
            egui::pos2(c.x + 1.6, c.y + 2.0),
        ],
        s,
    );
    painter.line_segment(
        [
            egui::pos2(c.x + 1.6, c.y + 2.0),
            egui::pos2(c.x + 1.6, c.y - 0.6),
        ],
        s,
    );
}

/// 侧栏品牌标：几何与 `bin/assets/aidops-logo.svg` 保持一致。
/// 展开时为横向 Logo，收起时只保留脉冲结图形。
fn draw_brand_logo(ui: &egui::Ui, rect: egui::Rect, expanded: bool, pal: &Palette) {
    let blue = egui::Color32::from_rgb(0x60, 0xa5, 0xfa);
    let mint = egui::Color32::from_rgb(0x5e, 0xea, 0xd4);
    // 展开态按“图形 + 双行字标”的整体视觉宽度居中，收起态单独居中图形。
    let logo_width = if expanded { 91.0 } else { 27.0 };
    let logo_height = 27.0;
    let origin = egui::pos2(
        rect.center().x - logo_width / 2.0,
        rect.center().y - logo_height / 2.0,
    );
    let point = |x: f32, y: f32| origin + egui::vec2(x, y);
    let stroke = 2.3;
    let left = [point(0.0, 18.0), point(7.0, 4.0), point(14.0, 15.0), point(25.0, 0.0)];
    let right = [point(1.0, 23.0), point(11.0, 10.0), point(18.0, 20.0), point(27.0, 11.0)];
    ui.painter().add(egui::Shape::line(left.to_vec(), egui::Stroke::new(stroke, blue)));
    ui.painter().add(egui::Shape::line(right.to_vec(), egui::Stroke::new(stroke, mint)));
    for (pos, color) in [(left[0], blue), (left[3], blue), (right[0], mint), (right[3], mint)] {
        ui.painter().circle_filled(pos, 2.1, color);
    }
    ui.painter().circle_filled(right[1], 2.3, egui::Color32::from_rgb(0xf0, 0xf9, 0xff));

    if expanded {
        ui.painter().text(
            egui::pos2(origin.x + 38.0, origin.y + 10.0),
            egui::Align2::LEFT_CENTER,
            "AIOPS",
            egui::FontId::proportional(15.0),
            pal.text,
        );
        ui.painter().text(
            egui::pos2(origin.x + 39.0, origin.y + 23.0),
            egui::Align2::LEFT_CENTER,
            "DESKTOP",
            egui::FontId::proportional(8.5),
            pal.dim,
        );
    }
}

/// 侧栏扁平导航项：透明底、悬停微亮、矢量图标。返回是否点击。
fn nav_item(
    ui: &mut egui::Ui,
    pal: &Palette,
    icon: Icon,
    label: &str,
    expanded: bool,
    enabled: bool,
    accent: bool,
) -> bool {
    #[cfg(target_os = "macos")]
    let height = 38.0;
    #[cfg(not(target_os = "macos"))]
    let height = 36.0;
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click(),
    );
    let hovered = enabled && response.hovered();
    if hovered {
        ui.painter()
            .rect_filled(rect.shrink(2.0), egui::Rounding::same(8.0), pal.hover);
    }
    let icon_color = if accent { pal.accent } else { pal.dim };
    let text_color = if !enabled { pal.dim } else { pal.text };
    let icon_center = egui::pos2(
        rect.min.x + if expanded { 20.0 } else { rect.width() / 2.0 },
        rect.center().y,
    );
    draw_icon(ui.painter(), icon_center, icon, icon_color);
    if expanded {
        ui.painter().text(
            egui::pos2(rect.min.x + 40.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(if cfg!(target_os = "macos") {
                13.5
            } else {
                13.0
            }),
            text_color,
        );
    }
    response.clicked() && enabled
}

#[cfg(target_os = "macos")]
fn macos_theme_button(ui: &mut egui::Ui, pal: &Palette, dark: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(62.0, 26.0), egui::Sense::click());
    let fill = if response.hovered() {
        pal.hover
    } else {
        pal.field
    };
    ui.painter().rect(
        rect,
        egui::Rounding::same(6.0),
        fill,
        egui::Stroke::new(1.0, pal.border),
    );

    let icon_center = egui::pos2(rect.left() + 13.0, rect.center().y);
    let stroke = egui::Stroke::new(
        1.25,
        if response.hovered() {
            pal.text
        } else {
            pal.dim
        },
    );
    if dark {
        ui.painter()
            .circle(icon_center, 3.5, egui::Color32::TRANSPARENT, stroke);
        for i in 0..8 {
            let angle = i as f32 * std::f32::consts::TAU / 8.0;
            let direction = egui::vec2(angle.cos(), angle.sin());
            ui.painter().line_segment(
                [icon_center + direction * 5.5, icon_center + direction * 7.0],
                stroke,
            );
        }
    } else {
        ui.painter().circle_filled(icon_center, 5.5, stroke.color);
        ui.painter()
            .circle_filled(icon_center + egui::vec2(2.5, -2.0), 5.0, fill);
    }
    ui.painter().text(
        egui::pos2(rect.left() + 25.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        if dark { "浅色" } else { "深色" },
        egui::FontId::proportional(12.0),
        pal.text,
    );
    response.clicked()
}

/// 模态面板右上角关闭按钮（矢量 ✕，悬停微亮）。
fn close_button(ui: &mut egui::Ui, pal: &Palette) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(26.0, 26.0), egui::Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, egui::Rounding::same(6.0), pal.hover);
    }
    let c = rect.center();
    let d = 4.5;
    let stroke = egui::Stroke::new(1.6_f32, if resp.hovered() { pal.text } else { pal.dim });
    ui.painter().line_segment(
        [egui::pos2(c.x - d, c.y - d), egui::pos2(c.x + d, c.y + d)],
        stroke,
    );
    ui.painter().line_segment(
        [egui::pos2(c.x - d, c.y + d), egui::pos2(c.x + d, c.y - d)],
        stroke,
    );
    resp.clicked()
}

/// 主操作按钮（柔和青底、内容自适应宽度，不再占满整行）。
fn accent_button(ui: &mut egui::Ui, pal: &Palette, label: &str) -> bool {
    // 宽度按文字估算：CJK 约 13.5px、ASCII 约 7.5px，再加左右内边距。
    let text_w: f32 = label
        .chars()
        .map(|c| if c.is_ascii() { 7.5 } else { 13.5 })
        .sum();
    let w = (text_w + 44.0).max(130.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 34.0), egui::Sense::click());
    let fill = if resp.hovered() {
        pal.btn_hover
    } else {
        pal.btn_fill
    };
    ui.painter()
        .rect_filled(rect, egui::Rounding::same(8.0), fill);
    ui.painter().rect(
        rect,
        egui::Rounding::same(8.0),
        egui::Color32::TRANSPARENT,
        egui::Stroke::new(1.0_f32, pal.btn_border),
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(13.0),
        pal.btn_text,
    );
    resp.clicked()
}

/// 插件列表单行：返回（是否移除、启用状态是否变化）。
fn plugin_row_ui(ui: &mut egui::Ui, pal: &Palette, row: &mut PluginUiRow) -> (bool, bool) {
    let mut removed = false;
    let was_enabled = row.enabled;
    // 统一行宽：内容区撑满外层可用宽度（扣除左右内边距），
    // 卡片边框左右对齐且不超出面板。
    let margin = egui::Margin::symmetric(12.0, 9.0);
    let row_w = (ui.available_width() - margin.sum().x).max(200.0);
    egui::Frame::default()
        .fill(pal.field)
        .rounding(egui::Rounding::same(9.0))
        .stroke(egui::Stroke::new(1.0_f32, pal.border))
        .inner_margin(margin)
        .show(ui, |ui| {
            ui.set_min_width(row_w);
            ui.horizontal(|ui| {
                ui.add_space(2.0);
                if row.core {
                    // 核心插件恒启用：禁用态控件直观传达「不可取消勾选」。
                    let mut on = true;
                    ui.add_enabled(false, egui::Checkbox::new(&mut on, ""));
                } else {
                    ui.checkbox(&mut row.enabled, "");
                }
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&row.name).size(13.0).color(pal.text));
                        ui.label(
                            egui::RichText::new(if row.core { "核心" } else { "WASM" })
                                .size(10.0)
                                .color(pal.accent),
                        );
                        if !row.core {
                            ui.label(
                                egui::RichText::new(if row.active {
                                    "运行中"
                                } else if row.enabled {
                                    "待加载"
                                } else {
                                    "已禁用"
                                })
                                .size(10.0)
                                .color(pal.dim),
                            );
                        }
                    });
                    ui.add(
                        egui::Label::new(egui::RichText::new(&row.desc).size(11.0).color(pal.dim))
                            // 描述单行展示不换行，超长截断省略。
                            .wrap_mode(egui::TextWrapMode::Truncate),
                    );
                });
                if !row.core {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ghost_button(ui, pal, "移除") {
                            removed = true;
                        }
                    });
                }
            });
        });
    ui.add_space(6.0);
    (removed, was_enabled != row.enabled)
}

/// 次级按钮（描边幽灵风格）。
fn ghost_button(ui: &mut egui::Ui, pal: &Palette, label: &str) -> bool {
    // 高度与主操作按钮 accent_button 保持一致（34px）。
    let size = egui::vec2(label.chars().count() as f32 * 13.0 + 20.0, 34.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, egui::Rounding::same(6.0), pal.hover);
    }
    ui.painter().rect(
        rect,
        egui::Rounding::same(6.0),
        egui::Color32::TRANSPARENT,
        egui::Stroke::new(1.0_f32, pal.border),
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(12.0),
        if resp.hovered() { pal.text } else { pal.dim },
    );
    resp.clicked()
}

/// 表单字段标签（暗色小号，上方留白）。
fn field_label(ui: &mut egui::Ui, pal: &Palette, label: &str) {
    ui.add_space(8.0);
    ui.label(egui::RichText::new(label).size(12.0).color(pal.dim));
    ui.add_space(3.0);
}

// ── 记忆面板：浏览本地原生记忆资产（与 harness-provider-memory 落盘结构一致）──
// 注意：本面板读取 `<cwd>/.harness-memory` 下的本地文件，反映 dsh「不接入后端时的
// 原生记忆」。若已配置并连接 aidops 后端，后端的记忆以远端为准，此处仅展示本地副本。

#[derive(Clone)]
struct MemItem {
    title: String,
    meta: String,
    body: String,
}

impl eframe::App for AppState {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_log();
        self.busy = self.host.sink.busy();

        let pal = palette(self.dark);
        let mut visuals = if self.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        visuals.panel_fill = pal.bg;
        visuals.window_fill = pal.panel;
        visuals.extreme_bg_color = pal.field;
        visuals.widgets.noninteractive.bg_fill = pal.panel;
        visuals.widgets.inactive.bg_fill = pal.field;
        visuals.selection.bg_fill = pal.user_bubble;
        // 下拉 / 选择控件统一主题化：按钮底色、描边、悬停、弹出菜单背景与圆角全部跟主题走，
        // 不再使用 egui 默认灰块风格。
        visuals.menu_rounding = egui::Rounding::same(8.0);
        visuals.popup_shadow = egui::epaint::Shadow {
            offset: egui::vec2(0.0, 6.0),
            blur: 16.0,
            spread: 0.0,
            color: egui::Color32::from_black_alpha(90),
        };
        let w_stroke = egui::Stroke::new(1.0_f32, pal.border);
        let w_text = egui::Stroke::new(1.0_f32, pal.text);
        visuals.widgets.inactive.bg_stroke = w_stroke;
        visuals.widgets.inactive.fg_stroke = w_text;
        visuals.widgets.hovered.bg_fill = pal.hover;
        visuals.widgets.hovered.bg_stroke = w_stroke;
        visuals.widgets.hovered.fg_stroke = w_text;
        visuals.widgets.active.bg_fill = pal.hover;
        visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0_f32, pal.accent);
        visuals.widgets.active.fg_stroke = w_text;
        visuals.widgets.open.bg_fill = pal.field;
        visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0_f32, pal.accent);
        visuals.widgets.open.fg_stroke = w_text;
        ctx.set_visuals(visuals);

        // ── 侧栏导航 ─────────────────────────────────────────────
        egui::SidePanel::left("nav")
            .exact_width(if self.sidebar_expanded { 220.0 } else { 56.0 })
            .frame(egui::Frame::default().fill(pal.side).inner_margin(8.0))
            .show(ctx, |ui| {
                ui.add_space(4.0);
                let logo_height = 30.0;
                let (logo_rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), logo_height),
                    egui::Sense::hover(),
                );
                draw_brand_logo(ui, logo_rect, self.sidebar_expanded, &pal);
                ui.add_space(6.0);
                if nav_item(ui, &pal, Icon::Chat, "新建对话", self.sidebar_expanded, !self.busy, true)
                {
                    self.new_session();
                }
                if nav_item(
                    ui,
                    &pal,
                    Icon::Folder,
                    "新建项目",
                    self.sidebar_expanded,
                    true,
                    false,
                ) {
                    self.settings_page = "新建项目".into();
                    self.settings_open = true;
                }
                if nav_item(
                    ui,
                    &pal,
                    Icon::Layers,
                    "插件管理",
                    self.sidebar_expanded,
                    true,
                    false,
                ) {
                    self.settings_page = "插件管理".into();
                    self.settings_open = true;
                }
                if nav_item(
                    ui,
                    &pal,
                    Icon::Chip,
                    "模型设置",
                    self.sidebar_expanded,
                    true,
                    false,
                ) {
                    self.settings_page = "模型设置".into();
                    self.settings_open = true;
                }
                if nav_item(
                    ui,
                    &pal,
                    Icon::Gear,
                    "系统配置",
                    self.sidebar_expanded,
                    true,
                    false,
                ) {
                    self.settings_page = "系统配置".into();
                    self.settings_open = true;
                }
                if nav_item(
                    ui,
                    &pal,
                    Icon::Layers,
                    "记忆中心",
                    self.sidebar_expanded,
                    true,
                    false,
                ) {
                    self.settings_page = "记忆".into();
                    self.settings_open = true;
                }
                if nav_item(
                    ui,
                    &pal,
                    Icon::Update,
                    "检查更新",
                    self.sidebar_expanded,
                    true,
                    false,
                ) {
                    self.settings_page = "更新".into();
                    self.settings_open = true;
                }
                ui.add_space(8.0);
                if nav_item(
                    ui,
                    &pal,
                    Icon::Menu,
                    "收起侧栏",
                    self.sidebar_expanded,
                    true,
                    false,
                ) {
                    self.sidebar_expanded = !self.sidebar_expanded;
                }
                // ── 项目列表（Codex/Cursor 式：点击即切上下文）────────────
                if self.sidebar_expanded {
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("项目").size(11.0).color(pal.dim));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_sized(
                                    [20.0, 20.0],
                                    egui::Button::new(
                                        egui::RichText::new("+").size(14.0).color(pal.dim),
                                    ),
                                )
                                .on_hover_text("添加新项目")
                                .clicked()
                            {
                                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                    let s = path.display().to_string();
                                    let _ = self.host.settings.add_project(&path);
                                    self.switch_project(&s);
                                }
                            }
                        });
                    });
                    ui.add_space(4.0);
                    egui::ScrollArea::vertical()
                        .id_salt("project_list")
                        .max_height(140.0)
                        .auto_shrink(true)
                        .show(ui, |ui| {
                            let mut archive_now: Option<String> = None;
                            let mut switch_now: Option<String> = None;
                            for proj in self.projects.clone() {
                                if proj.archived {
                                    continue;
                                }
                                let is_active = proj.path == self.active_project;
                                let (rect, resp) = ui.allocate_at_least(
                                    egui::vec2(ui.available_width(), 30.0),
                                    egui::Sense::click(),
                                );
                                // 指针在本行内即保持悬停态：否则浮出的「归档」按钮会抢走
                                // hover 导致逐帧振荡抖动（与历史面板同一修复）。
                                let hovered = resp.hovered()
                                    || (ctx.input(|i| i.pointer.has_pointer())
                                        && rect.contains(
                                            ctx.input(|i| i.pointer.hover_pos())
                                                .unwrap_or(egui::pos2(-1.0, -1.0)),
                                        ));
                                if is_active || hovered {
                                    ui.painter().rect_filled(
                                        rect.shrink(1.0),
                                        egui::Rounding::same(7.0),
                                        pal.hover,
                                    );
                                }
                                if is_active {
                                    // 左侧 accent 竖条标识当前激活项目。
                                    let bar = egui::Rect::from_min_size(
                                        egui::pos2(rect.min.x + 2.0, rect.min.y + 7.0),
                                        egui::vec2(2.5, rect.height() - 14.0),
                                    );
                                    ui.painter().rect_filled(
                                        bar,
                                        egui::Rounding::same(2.0),
                                        pal.accent,
                                    );
                                }
                                draw_icon(
                                    &ui.painter(),
                                    egui::pos2(rect.min.x + 16.0, rect.center().y),
                                    Icon::Folder,
                                    if is_active { pal.accent } else { pal.dim },
                                );
                                ui.painter().text(
                                    egui::pos2(rect.min.x + 30.0, rect.center().y),
                                    egui::Align2::LEFT_CENTER,
                                    &proj.name,
                                    egui::FontId::proportional(12.5),
                                    if is_active { pal.text } else { pal.dim },
                                );
                                if hovered {
                                    // 悬停时右侧浮现归档按钮。
                                    let arch_rect = egui::Rect::from_min_size(
                                        egui::pos2(rect.max.x - 42.0, rect.center().y - 10.0),
                                        egui::vec2(38.0, 20.0),
                                    );
                                    let b = ui.put(
                                        arch_rect,
                                        egui::Button::new(
                                            egui::RichText::new("归档").size(10.0).color(pal.dim),
                                        ),
                                    );
                                    if b.clicked() {
                                        archive_now = Some(proj.path.clone());
                                    }
                                }
                                if resp.clicked() {
                                    switch_now = Some(proj.path.clone());
                                }
                            }
                            if let Some(path) = archive_now {
                                let _ = self.host.settings.archive_project(&path, true);
                                self.projects = self.host.settings.projects();
                                trace(&format!("[project] archived {path}"));
                            }
                            if let Some(path) = switch_now {
                                self.switch_project(&path);
                            }
                        });
                    // ── 历史记录（点击恢复过往会话）────────────────
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("历史 ({})", self.history.len()))
                                .size(11.0)
                                .color(pal.dim),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_sized(
                                    [36.0, 18.0],
                                    egui::Button::new(
                                        egui::RichText::new("清空").size(10.0).color(pal.dim),
                                    ),
                                )
                                .on_hover_text("删除全部历史会话（保留当前对话）")
                                .clicked()
                            {
                                self.clear_history();
                            }
                            if ui
                                .add_sized(
                                    [36.0, 18.0],
                                    egui::Button::new(
                                        egui::RichText::new("精简").size(10.0).color(pal.dim),
                                    ),
                                )
                                .on_hover_text("仅保留最近 30 个会话（当前对话不删）")
                                .clicked()
                            {
                                self.prune_history();
                            }
                        });
                    });
                    // 精简 / 清空操作的即时反馈（5 秒后隐去）。
                    if let Some(at) = self.history_note_at {
                        if at.elapsed() < std::time::Duration::from_secs(5) {
                            ui.label(
                                egui::RichText::new(&self.history_note)
                                    .size(10.5)
                                    .color(pal.accent),
                            );
                        } else {
                            self.history_note_at = None;
                        }
                    }
                    ui.add_space(4.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut self.history_search)
                            .desired_width(f32::INFINITY)
                            .hint_text(egui::RichText::new("搜索历史…").color(pal.dim)),
                    );
                    ui.add_space(4.0);
                    let history_height = (ui.available_height() - 40.0).max(90.0);
                    egui::ScrollArea::vertical()
                        .id_salt("history_list")
                        .max_height(history_height)
                        .show(ui, |ui| {
                            let kw = self.history_search.trim().to_lowercase();
                            let mut open_now: Option<String> = None;
                            let mut delete_now: Option<String> = None;
                            for meta in self.history.clone() {
                                if !kw.is_empty() && !meta.title.to_lowercase().contains(&kw) {
                                    continue;
                                }
                                let is_active = meta.file == self.current_session;
                                let (rect, resp) = ui.allocate_at_least(
                                    egui::vec2(ui.available_width(), 40.0),
                                    egui::Sense::click(),
                                );
                                // 指针在本行内即保持悬停态：否则浮出的按钮会抢走 hover
                                // 导致行失焦→按钮消失→再悬停→再浮出，逐帧振荡抖动。
                                let hovered = resp.hovered()
                                    || (ctx.input(|i| i.pointer.has_pointer())
                                        && rect.contains(
                                            ctx.input(|i| i.pointer.hover_pos())
                                                .unwrap_or(egui::pos2(-1.0, -1.0)),
                                        ));
                                if is_active || hovered {
                                    ui.painter().rect_filled(
                                        rect.shrink(1.0),
                                        egui::Rounding::same(7.0),
                                        pal.hover,
                                    );
                                }
                                if is_active {
                                    let bar = egui::Rect::from_min_size(
                                        egui::pos2(rect.min.x + 2.0, rect.min.y + 8.0),
                                        egui::vec2(2.5, rect.height() - 16.0),
                                    );
                                    ui.painter().rect_filled(
                                        bar,
                                        egui::Rounding::same(2.0),
                                        pal.accent,
                                    );
                                }
                                // 标题截断适配窄侧栏。
                                let mut title: String = meta.title.chars().take(15).collect();
                                if meta.title.chars().count() > 15 {
                                    title.push('…');
                                }
                                ui.painter().text(
                                    egui::pos2(rect.min.x + 12.0, rect.min.y + 12.0),
                                    egui::Align2::LEFT_CENTER,
                                    &title,
                                    egui::FontId::proportional(12.0),
                                    if is_active { pal.text } else { pal.dim },
                                );
                                ui.painter().text(
                                    egui::pos2(rect.min.x + 12.0, rect.max.y - 11.0),
                                    egui::Align2::LEFT_CENTER,
                                    format!("{} · {}", meta.project, relative_time(&meta.mtime)),
                                    egui::FontId::proportional(10.0),
                                    pal.dim,
                                );
                                if hovered {
                                    // 悬停时右侧浮现「重命名」与「删除」按钮：
                                    // 矢量图标（不依赖字体字形，避免豆腐块）+ 悬停提示说明功能。
                                    let rename_rect = egui::Rect::from_min_size(
                                        egui::pos2(rect.max.x - 44.0, rect.center().y - 9.0),
                                        egui::vec2(18.0, 18.0),
                                    );
                                    let rb = ui.interact(
                                        rename_rect,
                                        egui::Id::new(("hist_rename", &meta.file)),
                                        egui::Sense::click(),
                                    );
                                    if rb.hovered() {
                                        ui.painter().rect_filled(
                                            rename_rect.shrink(1.0),
                                            egui::Rounding::same(4.0),
                                            pal.hover,
                                        );
                                    }
                                    draw_pencil_icon(
                                        &ui.painter(),
                                        rename_rect.center(),
                                        if rb.hovered() { pal.text } else { pal.dim },
                                    );
                                    let rb = rb.on_hover_text("重命名此会话");
                                    if rb.clicked() {
                                        self.renaming = Some(meta.file.clone());
                                        self.rename_buf = meta.title.clone();
                                    }
                                    let del_rect = egui::Rect::from_min_size(
                                        egui::pos2(rect.max.x - 24.0, rect.center().y - 9.0),
                                        egui::vec2(18.0, 18.0),
                                    );
                                    let b = ui.interact(
                                        del_rect,
                                        egui::Id::new(("hist_delete", &meta.file)),
                                        egui::Sense::click(),
                                    );
                                    if b.hovered() {
                                        ui.painter().rect_filled(
                                            del_rect.shrink(1.0),
                                            egui::Rounding::same(4.0),
                                            pal.hover,
                                        );
                                    }
                                    draw_trash_icon(
                                        &ui.painter(),
                                        del_rect.center(),
                                        if b.hovered() { pal.text } else { pal.dim },
                                    );
                                    let b = b.on_hover_text("删除此会话");
                                    if b.clicked() {
                                        delete_now = Some(meta.file.clone());
                                    }
                                }
                                if resp.clicked() && !is_active {
                                    open_now = Some(meta.file.clone());
                                }
                            }
                            if let Some(file) = open_now {
                                self.switch_session(&file);
                            }
                            if let Some(file) = delete_now {
                                self.delete_session_entry(&file);
                            }
                        });
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    if self.sidebar_expanded {
                        ui.label(
                            egui::RichText::new(format!("工作区: {}", self.active_project))
                                .size(11.0)
                                .color(pal.dim),
                        );
                    }
                });
            });

        // ── 底部输入区：圆角卡片 + 紧凑工具栏 + 圆形发送按钮（现代输入范式） ──
        let mut send_now = false;
        egui::TopBottomPanel::bottom("composer")
            .frame(
                egui::Frame::default()
                    .fill(pal.bg)
                    .inner_margin(egui::Margin {
                        left: 10.0,
                        right: 10.0,
                        top: 4.0,
                        bottom: 6.0,
                    }),
            )
            .show(ctx, |ui| {
                let can_send = !self.busy && !self.input.trim().is_empty();
                // ── 输入卡片：圆角 + 细边框 + 阴影浮起，卡片自身提供 chrome ──
                let card_frame = egui::Frame::default()
                    .fill(pal.panel)
                    .rounding(egui::Rounding::same(14.0))
                    .stroke(egui::Stroke::new(1.0_f32, pal.border))
                    .inner_margin(egui::Margin {
                        left: 14.0,
                        right: 10.0,
                        top: 12.0,
                        bottom: 8.0,
                    })
                    .shadow(egui::epaint::Shadow {
                        offset: egui::vec2(0.0, 6.0),
                        blur: 18.0,
                        spread: 0.0,
                        color: egui::Color32::from_black_alpha(if self.dark { 0x44 } else { 0x14 }),
                    });
                card_frame.show(ui, |ui| {
                    // 文本编辑区：去掉自身边框/背景，由卡片提供 chrome。
                    let response = ui.add(
                        egui::TextEdit::multiline(&mut self.input)
                            .desired_width(f32::INFINITY)
                            .desired_rows(3)
                            .font(egui::FontId::proportional(14.0))
                            .frame(false)
                            .margin(egui::Margin::same(0.0))
                            .hint_text(
                                egui::RichText::new("描述任务、粘贴代码或提出问题…").color(pal.dim),
                            ),
                    );
                    // Enter 发送 / Shift+Enter 换行：egui 会先插入换行，这里去掉尾随 \n 再提交。
                    let enter = response.has_focus()
                        && ctx.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
                    if enter {
                        while self.input.ends_with('\n') {
                            self.input.pop();
                        }
                        send_now = true;
                    }
                    ui.add_space(6.0);

                    // ── 底部工具栏 ──
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;

                        // ── 模型 chip（点击 → 打开设置页） ──
                        let model_label = format!("{} · {}", self.f_provider, self.f_model);
                        let text_w: f32 = model_label
                            .chars()
                            .map(|c| if c.is_ascii() { 7.0 } else { 12.0 })
                            .sum();
                        let chip_w = (text_w + 40.0).max(110.0).min(240.0);
                        let (mrect, mresp) =
                            ui.allocate_exact_size(egui::vec2(chip_w, 28.0), egui::Sense::click());
                        let mfill = if mresp.hovered() {
                            pal.hover
                        } else {
                            pal.field
                        };
                        ui.painter()
                            .rect_filled(mrect, egui::Rounding::same(8.0), mfill);
                        ui.painter().rect(
                            mrect,
                            egui::Rounding::same(8.0),
                            egui::Color32::TRANSPARENT,
                            egui::Stroke::new(1.0_f32, pal.border),
                        );
                        ui.painter().text(
                            mrect.left_center() + egui::vec2(10.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            &model_label,
                            egui::FontId::proportional(12.0),
                            pal.text,
                        );
                        // 右侧 chevron
                        let cx = mrect.right() - 11.0;
                        let cy = mrect.center().y;
                        ui.painter().add(egui::Shape::convex_polygon(
                            vec![
                                egui::pos2(cx - 3.5, cy - 1.6),
                                egui::pos2(cx + 3.5, cy - 1.6),
                                egui::pos2(cx, cy + 1.8),
                            ],
                            pal.dim,
                            egui::Stroke::NONE,
                        ));
                        if mresp.clicked() {
                            self.settings_page = "模型设置".into();
                            self.settings_open = true;
                        }
                        mresp.on_hover_text("切换模型 / API 设置");

                        // ── 权限 chip（自定义 28px，与左侧模型 chip 同高/同 chrome；默认 ComboBox 不可控高度） ──
                        let label_max_w: f32 = ["只读", "工作区写入", "完全访问"]
                            .iter()
                            .map(|s| {
                                s.chars()
                                    .map(|c| if c.is_ascii() { 7.0 } else { 12.0 })
                                    .sum::<f32>()
                            })
                            .fold(0.0_f32, f32::max);
                        let perm_w = (label_max_w + 40.0).max(96.0).min(160.0);
                        let (prect, presp) =
                            ui.allocate_exact_size(egui::vec2(perm_w, 28.0), egui::Sense::click());
                        let pfill = if presp.hovered() || self.perm_menu_open {
                            pal.hover
                        } else {
                            pal.field
                        };
                        ui.painter()
                            .rect_filled(prect, egui::Rounding::same(8.0), pfill);
                        ui.painter().rect(
                            prect,
                            egui::Rounding::same(8.0),
                            egui::Color32::TRANSPARENT,
                            egui::Stroke::new(1.0_f32, pal.border),
                        );
                        ui.painter().text(
                            prect.left_center() + egui::vec2(10.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            &self.permission,
                            egui::FontId::proportional(12.0),
                            pal.text,
                        );
                        // 右侧 chevron：关闭 ▼，打开 ▲（与模型 chip 关闭态一致；打开态翻转）
                        let pcx = prect.right() - 11.0;
                        let pcy = prect.center().y;
                        if self.perm_menu_open {
                            ui.painter().add(egui::Shape::convex_polygon(
                                vec![
                                    egui::pos2(pcx - 3.5, pcy + 1.6),
                                    egui::pos2(pcx + 3.5, pcy + 1.6),
                                    egui::pos2(pcx, pcy - 1.8),
                                ],
                                pal.dim,
                                egui::Stroke::NONE,
                            ));
                        } else {
                            ui.painter().add(egui::Shape::convex_polygon(
                                vec![
                                    egui::pos2(pcx - 3.5, pcy - 1.6),
                                    egui::pos2(pcx + 3.5, pcy - 1.6),
                                    egui::pos2(pcx, pcy + 1.8),
                                ],
                                pal.dim,
                                egui::Stroke::NONE,
                            ));
                        }
                        if presp.clicked() {
                            self.perm_menu_open = !self.perm_menu_open;
                        }
                        presp.on_hover_text("切换工具权限范围");

                        // 权限下拉弹层：向上展开（chip 靠近屏幕底部，向下会被裁），
                        // 圆角面板 + 阴影，浮于前景（与 composer 卡片同款 chrome）。
                        if self.perm_menu_open {
                            // 外部点击关闭（在渲染 Area 之前判定，避免一帧闪烁）
                            let pressed = ctx.input(|i| i.pointer.any_pressed());
                            let press_origin = ctx.input(|i| i.pointer.press_origin());
                            if pressed {
                                if let Some(pos) = press_origin {
                                    let menu_rect = egui::Rect::from_min_max(
                                        egui::pos2(prect.left(), prect.top() - 96.0),
                                        egui::pos2(prect.left() + perm_w, prect.top()),
                                    );
                                    if !prect.contains(pos) && !menu_rect.contains(pos) {
                                        self.perm_menu_open = false;
                                    }
                                }
                            }
                        }
                        if self.perm_menu_open {
                            egui::Area::new(egui::Id::new("perm_menu_area"))
                                .fixed_pos(egui::pos2(prect.left(), prect.top() - 96.0))
                                .movable(false)
                                .interactable(true)
                                .order(egui::Order::Foreground)
                                .show(ctx, |ui| {
                                    egui::Frame::none()
                                        .fill(pal.panel)
                                        .rounding(egui::Rounding::same(8.0))
                                        .inner_margin(egui::Margin::same(4.0))
                                        .stroke(egui::Stroke::new(1.0_f32, pal.border))
                                        .shadow(egui::epaint::Shadow {
                                            offset: egui::vec2(0.0, 6.0),
                                            blur: 18.0,
                                            spread: 0.0,
                                            color: egui::Color32::from_black_alpha(if self.dark {
                                                0x44
                                            } else {
                                                0x14
                                            }),
                                        })
                                        .show(ui, |ui| {
                                            ui.set_min_width(perm_w);
                                            ui.spacing_mut().item_spacing.y = 2.0;
                                            for mode in ["只读", "工作区写入", "完全访问"]
                                            {
                                                let selected = self.permission == mode;
                                                let r = ui.selectable_label(
                                                    selected,
                                                    egui::RichText::new(mode)
                                                        .size(12.0)
                                                        .color(pal.text),
                                                );
                                                if r.clicked() {
                                                    self.permission = mode.to_string();
                                                    self.perm_menu_open = false;
                                                    let _ = self
                                                        .host
                                                        .settings
                                                        .set("permission.mode", &self.permission);
                                                }
                                            }
                                        });
                                });
                        }

                        // ── 附件 icon button ──
                        let (arect, aresp) =
                            ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::click());
                        if aresp.hovered() {
                            ui.painter()
                                .rect_filled(arect, egui::Rounding::same(8.0), pal.hover);
                        }
                        let acolor = if !self.attachment.is_empty() {
                            pal.accent
                        } else {
                            pal.dim
                        };
                        draw_paperclip_icon(ui.painter(), arect.center(), acolor);
                        let tip = if self.attachment.is_empty() {
                            "添加附件".to_string()
                        } else {
                            format!("附件: {}\n（点击重新选择）", self.attachment)
                        };
                        if aresp.on_hover_text(tip).clicked() {
                            let picked = if self.settings_page == "新建项目" {
                                rfd::FileDialog::new().pick_folder()
                            } else {
                                rfd::FileDialog::new().pick_file()
                            };
                            if let Some(path) = picked {
                                self.attachment = path.display().to_string();
                            }
                        }

                        // 若已有附件，紧随其后放一个紧凑的清除 ✕
                        if !self.attachment.is_empty() {
                            let (xrect, xresp) = ui
                                .allocate_exact_size(egui::vec2(20.0, 28.0), egui::Sense::click());
                            let xcolor = if xresp.hovered() { pal.accent } else { pal.dim };
                            ui.painter().text(
                                xrect.center(),
                                egui::Align2::CENTER_CENTER,
                                "✕",
                                egui::FontId::proportional(12.0),
                                xcolor,
                            );
                            if xresp.clicked() {
                                self.attachment.clear();
                            }
                            xresp.on_hover_text("清除附件");
                        }

                        // 弹性空间 → 圆形发送/停止按钮
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let btn_size = 34.0;
                            let (brect, bresp) = ui.allocate_exact_size(
                                egui::vec2(btn_size, btn_size),
                                egui::Sense::click(),
                            );
                            let center = brect.center();
                            let bfill = if self.busy {
                                egui::Color32::from_rgb(0xfb, 0xbf, 0x24)
                            } else if can_send {
                                pal.btn_fill
                            } else {
                                pal.field
                            };
                            ui.painter().circle_filled(center, btn_size / 2.0, bfill);
                            if self.busy {
                                // 停止：实心方块
                                ui.painter().rect_filled(
                                    egui::Rect::from_center_size(center, egui::vec2(8.0, 8.0)),
                                    egui::Rounding::same(1.2),
                                    egui::Color32::from_rgb(0x1a, 0x24, 0x30),
                                );
                            } else {
                                let icon_color = if can_send { pal.btn_text } else { pal.dim };
                                // 实心三角箭头
                                ui.painter().add(egui::Shape::convex_polygon(
                                    vec![
                                        egui::pos2(center.x, center.y - 4.6),
                                        egui::pos2(center.x + 4.2, center.y - 0.9),
                                        egui::pos2(center.x - 4.2, center.y - 0.9),
                                    ],
                                    icon_color,
                                    egui::Stroke::NONE,
                                ));
                                // 箭头柄
                                ui.painter().line_segment(
                                    [
                                        egui::pos2(center.x, center.y - 0.9),
                                        egui::pos2(center.x, center.y + 4.6),
                                    ],
                                    egui::Stroke::new(2.0_f32, icon_color),
                                );
                            }
                            if bresp.hovered() {
                                ui.painter().circle_stroke(
                                    center,
                                    btn_size / 2.0,
                                    egui::Stroke::new(1.5_f32, pal.accent),
                                );
                            }
                            if bresp.clicked() {
                                if self.busy {
                                    trace("[cancel] requested");
                                    self.host.sink.cancel();
                                } else if can_send {
                                    send_now = true;
                                }
                            }
                            if self.busy {
                                bresp.on_hover_text("停止生成");
                            } else if can_send {
                                bresp.on_hover_text("发送 (Enter)");
                            } else {
                                bresp.on_hover_text("输入内容后可发送");
                            }
                        });
                    });
                });

                // 忙碌时每秒心跳重绘：egui 无输入事件不自动刷新，已用时计数需心跳驱动。
                if self.busy {
                    ctx.request_repaint_after(std::time::Duration::from_secs(1));
                }
                ui.add_space(4.0);
                // 卡片下方的提示行与状态行（轻量 footer）
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Enter 发送 · Shift+Enter 换行")
                            .size(11.0)
                            .color(pal.dim),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let usage = self.log.usage_total();
                        let (dot, text): (egui::Color32, String) = if self.busy && self.thinking {
                            let secs = self
                                .turn_started
                                .map(|t| t.elapsed().as_secs())
                                .unwrap_or(0);
                            (
                                egui::Color32::from_rgb(0x81, 0x8d, 0xf8),
                                format!("● 模型思考中 · 已用时 {secs} 秒"),
                            )
                        } else if self.busy {
                            let secs = self
                                .turn_started
                                .map(|t| t.elapsed().as_secs())
                                .unwrap_or(0);
                            (
                                egui::Color32::from_rgb(0xfb, 0xbf, 0x24),
                                format!("● 正在处理 · 已用时 {secs} 秒，可随时停止"),
                            )
                        } else {
                            (
                                pal.accent,
                                format!(
                                    "● 就绪  ·  Tokens {}/{}",
                                    usage.prompt_tokens, usage.completion_tokens
                                ),
                            )
                        };
                        ui.label(egui::RichText::new(text).size(11.0).color(dot));
                    });
                });
            });

        // ── 主区：头部 + 消息流 ──────────────────────────────────
        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(pal.bg))
            .show(ctx, |ui| {
                // 导航头色带：独立底色 + 底边主题色分隔线，与消息区拉开层次。
                let head = egui::Frame::default()
                    .fill(pal.head_fill)
                    .inner_margin(egui::Margin::symmetric(
                        14.0,
                        if cfg!(target_os = "macos") { 7.0 } else { 5.0 },
                    ))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            #[cfg(target_os = "macos")]
                            ui.set_min_height(26.0);
                            // 紧凑导航头：单行小标题，不再展示模型副标题。
                            ui.label(
                                egui::RichText::new("对话工作台")
                                    .size(15.0)
                                    .strong()
                                    .color(pal.text),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    #[cfg(target_os = "macos")]
                                    let toggle_theme = macos_theme_button(ui, &pal, self.dark);
                                    #[cfg(not(target_os = "macos"))]
                                    let toggle_theme = {
                                        let theme_label = if self.dark {
                                            "☀ 浅色"
                                        } else {
                                            "🌙 深色"
                                        };
                                        ui.button(theme_label).clicked()
                                    };
                                    if toggle_theme {
                                        self.dark = !self.dark;
                                        let _ = self.host.settings.set(
                                            "ui.theme",
                                            if self.dark { "dark" } else { "light" },
                                        );
                                    }
                                    #[cfg(target_os = "macos")]
                                    ui.add_space(4.0);
                                    ui.label(
                                        egui::RichText::new(self.host.llm_control.status())
                                            .size(11.0)
                                            .color(pal.accent),
                                    );
                                },
                            );
                        });
                    });
                ui.painter().hline(
                    head.response.rect.x_range(),
                    head.response.rect.bottom(),
                    egui::Stroke::new(1.0_f32, pal.head_border),
                );
                ui.add_space(4.0);
                // ── 版本更新横幅（非 Idle 时显示）──
                self.draw_update_banner(ui, &pal);
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        let max_w = ui.available_width();
                        for msg in self.messages.clone() {
                            if msg.text.is_empty() {
                                continue; // 纯 DSML 气泡剥离后为空，不渲染空卡片
                            }
                            let (fill, text_color): (egui::Color32, egui::Color32) =
                                match msg.kind.as_str() {
                                    "user" => (pal.user_bubble, pal.user_text),
                                    "error" => (pal.err_bubble, pal.err_text),
                                    "tool" | "plan" => (pal.tool_bubble, pal.dim),
                                    "thinking" => (pal.field, pal.dim),
                                    _ => (pal.ai_bubble, pal.text),
                                };
                            // 所有气泡统一左对齐：用户消息不右对齐，阅读动线更连贯。
                            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                                let bubble = egui::Frame::default()
                                    .fill(fill)
                                    .rounding(egui::Rounding::same(10.0))
                                    .inner_margin(if cfg!(target_os = "macos") {
                                        egui::Margin::symmetric(12.0, 10.0)
                                    } else {
                                        egui::Margin::same(10.0)
                                    })
                                    .stroke(egui::Stroke::new(1.0_f32, pal.border));
                                bubble.show(ui, |ui| {
                                    ui.set_max_width(max_w * 0.78);
                                    ui.label(
                                        egui::RichText::new(&msg.label).size(10.5).color(pal.dim),
                                    );
                                    #[cfg(target_os = "macos")]
                                    ui.add_space(2.0);
                                    // selectable(true)：正文支持鼠标拖选，选中后 Ctrl+C 复制。
                                    let resp = if msg.kind == "assistant" {
                                        // Markdown 富文本渲染：标题/加粗/列表/代码块转 LayoutJob；
                                        // egui 按 job 哈希缓存 galley，同文本不重复排版。
                                        let job = crate::markdown::to_job(
                                            &msg.text,
                                            &crate::markdown::MdTheme {
                                                text: pal.text,
                                                dim: pal.dim,
                                                accent: pal.accent,
                                                code_text: pal.text,
                                                code_bg: pal.field,
                                            },
                                            max_w * 0.78 - 20.0,
                                        );
                                        ui.add(egui::Label::new(job).selectable(true))
                                    } else {
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(&msg.text)
                                                    .size(13.5)
                                                    .color(text_color),
                                            )
                                            .selectable(true),
                                        )
                                    };
                                    // 右键菜单：一键复制整条消息内容。
                                    resp.context_menu(|ui| {
                                        if ui.button("📋 复制全部内容").clicked() {
                                            ui.ctx().copy_text(msg.text.clone());
                                            ui.close_menu();
                                        }
                                    });
                                });
                            });
                            ui.add_space(6.0);
                        }
                    });
            });

        // ── 设置弹层 ─────────────────────────────────────────────
        // ── 设置模态：全屏半透明遮罩 + 居中圆角面板（替代默认 Window 标题栏样式）──
        if self.settings_open && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.settings_open = false;
        }
        if !self.settings_open {
            // 关闭后清空面板矩形，避免残留矩形影响后续遮罩误触防护。
            self.modal_panel_rect = None;
        }
        if self.settings_open {
            let page = self.settings_page.clone();
            let screen = ctx.screen_rect();
            // 面板比例：宽度收窄、高度尽量占满视口——
            // 短内容页（插件管理等）由 min_scrolled_height 保底，不会塌成扁条。
            let panel_w = (screen.width() - 320.0).clamp(520.0, 660.0);
            let scroll_h = (screen.height() - 150.0).clamp(480.0, 800.0);
            // 内容变化（插件行、提示文字、滚动条）不能影响面板位置；否则居中锚点会
            // 和自动尺寸互相反馈，在 Windows 上表现为持续抖动。
            let panel_h = scroll_h + 94.0;
            let panel_pos = egui::pos2(
                screen.left() + (screen.width() - panel_w) * 0.5,
                screen.top() + ((screen.height() - panel_h) * 0.5).max(20.0),
            );

            // 蒙层：纯装饰压暗（直接画到 Background 层，不注册任何交互控件）。
            // ⚠️ 不能用带 Sense 的 Area 做蒙层：egui 0.30 会给 interactable Area 自动注册
            // 覆盖整个区域的“置顶点击”控件（area.rs move_response），抢占面板交互并自动
            // 把蒙层提到 Foreground 最前，表现为弹窗被蒙层挡住/点不动。
            ctx.layer_painter(egui::LayerId::new(
                egui::Order::Middle,
                egui::Id::new("modal_dim"),
            ))
            .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(130));

            // 点面板外关闭：原始输入判定（本帧不注册任何全屏交互控件，不与面板抢事件）。
            // modal_open_last_frame 守卫：打开当帧的 press 是侧栏触发点击，不得误关。
            if self.modal_open_last_frame {
                let pressed = ctx.input(|i| i.pointer.any_pressed());
                let origin = ctx.input(|i| i.pointer.press_origin());
                if pressed {
                    if let Some(pos) = origin {
                        let on_panel = self.modal_panel_rect.is_some_and(|r| r.contains(pos));
                        if !on_panel {
                            self.settings_open = false;
                        }
                    }
                }
            }

            // 面板层：Foreground 层。蒙层已无交互控件，本层是前景唯一可交互层，
            // 不会被抬到更前；层内 ComboBox 下拉注册更晚 → 盖住面板。
            // 切勿用 Tooltip：下拉菜单开在 Foreground 层，面板若更高会把菜单整个盖住。
            egui::Area::new("settings_panel".into())
                .order(egui::Order::Foreground)
                .fixed_pos(panel_pos)
                .show(ctx, |ui| {
                    egui::Frame::default()
                        .fill(pal.panel)
                        .rounding(egui::Rounding::same(14.0))
                        .stroke(egui::Stroke::new(1.0_f32, pal.border))
                        .shadow(egui::epaint::Shadow {
                            offset: egui::vec2(0.0, 10.0),
                            blur: 28.0,
                            spread: 0.0,
                            color: egui::Color32::from_black_alpha(120),
                        })
                        .inner_margin(egui::Margin::symmetric(20.0, 16.0))
                        .show(ui, |ui| {
                            ui.set_width(panel_w);
                            // 头部：标题 + 关闭按钮
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(&page)
                                        .size(16.0)
                                        .strong()
                                        .color(pal.text),
                                );
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if close_button(ui, &pal) {
                                        self.settings_open = false;
                                    }
                                });
                            });
                            let sep = ui
                                .allocate_exact_size(
                                    egui::vec2(ui.available_width(), 1.0),
                                    egui::Sense::hover(),
                                )
                                .0;
                            ui.painter().rect_filled(sep, 0.0, pal.border);
                            ui.add_space(10.0);
                            if !self.note.is_empty() {
                                ui.label(
                                    egui::RichText::new(&self.note).size(12.0).color(pal.accent),
                                );
                                ui.add_space(6.0);
                            }
                            egui::ScrollArea::vertical()
                                .min_scrolled_height(scroll_h)
                                .max_height(scroll_h)
                                .show(ui, |ui| match page.as_str() {
                                    "模型设置" => {
                                        field_label(ui, &pal, "已保存的配置");
                                        ui.horizontal(|ui| {
                                            egui::ComboBox::from_id_salt("profiles")
                                                .width((panel_w - 96.0).max(260.0))
                                                .selected_text(if self.selected_profile.is_empty() {
                                                    "选择已保存的配置…"
                                                } else {
                                                    self.selected_profile.as_str()
                                                })
                                                .show_ui(ui, |ui| {
                                                    for name in self.profiles.clone() {
                                                        if ui
                                                            .selectable_value(&mut self.selected_profile, name.clone(), name.as_str())
                                                            .clicked()
                                                        {
                                                            self.load_profile(&name);
                                                        }
                                                    }
                                                });
                                            if !self.selected_profile.is_empty()
                                                && ghost_button(ui, &pal, "删除")
                                            {
                                                let _ = self
                                                    .host
                                                    .settings
                                                    .delete_model_profile(&self.selected_profile);
                                                self.profiles = self
                                                    .host
                                                    .settings
                                                    .model_profiles()
                                                    .into_iter()
                                                    .map(|p| p.name)
                                                    .collect();
                                                self.selected_profile.clear();
                                            }
                                        });
                                        field_label(ui, &pal, "模型厂商");
                                        ui.add(egui::TextEdit::singleline(&mut self.f_provider).desired_width(f32::INFINITY));
                                        field_label(ui, &pal, "API 地址");
                                        ui.add(egui::TextEdit::singleline(&mut self.f_base).desired_width(f32::INFINITY));
                                        field_label(ui, &pal, "模型名称（可自由填写）");
                                        ui.add(egui::TextEdit::singleline(&mut self.f_model).desired_width(f32::INFINITY));
                                        field_label(ui, &pal, "API Key（AES-256-GCM 加密后保存至 SQLite，跨操作系统通用）");
                                        ui.add(egui::TextEdit::singleline(&mut self.f_key).password(true).desired_width(f32::INFINITY));
                                        field_label(ui, &pal, "思考档位 reasoning_effort（可选：off/low/medium/high/xhigh/max/auto，留空=默认）");
                                        ui.add(egui::TextEdit::singleline(&mut self.f_effort).desired_width(f32::INFINITY));
                                        ui.add_space(14.0);
                                        if accent_button(ui, &pal, "添加 / 更新并应用") {
                                            self.apply_model();
                                        }
                                        ui.add_space(8.0);
                                        ui.label(
                                            egui::RichText::new(
                                                "支持采用 OpenAI Chat Completions 协议的服务；保存时以“厂商 · 模型名”建立或更新配置。",
                                            )
                                            .size(11.0)
                                            .color(pal.dim),
                                        );
                                    }
                                    "新建项目" => {
                                        ui.label(
                                            egui::RichText::new("选择项目目录后立即切换到该项目，并保存到侧栏项目列表。")
                                                .size(12.5)
                                                .color(pal.text),
                                        );
                                        ui.add_space(12.0);
                                        if accent_button(ui, &pal, "选择项目目录") {
                                            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                                let s = path.display().to_string();
                                                let _ = self.host.settings.add_project(&path);
                                                self.switch_project(&s);
                                            }
                                        }
                                        ui.add_space(8.0);
                                        ui.label(
                                            egui::RichText::new(format!("当前项目: {}", self.active_project))
                                                .size(12.0)
                                                .color(pal.dim),
                                        );
                                    }
                                    "插件管理" => {
                                        field_label(ui, &pal, "内置核心插件（默认启用，系统必要能力不可取消）");
                                        for i in 0..self.plugin_rows.len() {
                                            if self.plugin_rows[i].core {
                                                let _ = plugin_row_ui(ui, &pal, &mut self.plugin_rows[i]);
                                            }
                                        }
                                        ui.add_space(6.0);
                                        field_label(ui, &pal, "扩展插件（WASM · wasmtime 沙箱隔离，可自由启用 / 禁用或移除）");
                                        let active_plugins = self.host.wasm_plugins.active_ids();
                                        ui.label(
                                            egui::RichText::new(if active_plugins.is_empty() {
                                                "运行时：当前没有已加载的 WASM 插件".to_string()
                                            } else {
                                                format!("运行时：已加载 {}", active_plugins.join("、"))
                                            })
                                            .size(11.0)
                                            .color(pal.dim),
                                        );
                                        ui.add_space(4.0);
                                        let mut remove_ids: Vec<String> = Vec::new();
                                        let mut wasm_count = 0;
                                        for i in 0..self.plugin_rows.len() {
                                            if self.plugin_rows[i].core {
                                                continue;
                                            }
                                            wasm_count += 1;
                                            let (remove, changed) = plugin_row_ui(ui, &pal, &mut self.plugin_rows[i]);
                                            if changed {
                                                let row = &mut self.plugin_rows[i];
                                                let result = if row.enabled {
                                                    self.host.wasm_plugins.activate(&row.id, std::path::Path::new(&row.desc))
                                                } else {
                                                    self.host.wasm_plugins.deactivate(&row.id)
                                                };
                                                match result {
                                                    Ok(()) => {
                                                        row.active = row.enabled;
                                                        let _ = self.host.settings.set_plugin_enabled(&row.id, &row.name, row.enabled);
                                                        self.note = format!("插件「{}」{}", row.name, if row.enabled { "已启用并开始运行" } else { "已禁用并卸载" });
                                                    }
                                                    Err(error) => {
                                                        row.enabled = !row.enabled;
                                                        self.note = format!("插件状态未变更: {error}");
                                                    }
                                                }
                                            }
                                            if remove {
                                                remove_ids.push(self.plugin_rows[i].id.clone());
                                            }
                                        }
                                        if wasm_count == 0 {
                                            ui.label(
                                                egui::RichText::new("尚未导入 WASM 插件，点下方「＋ 添加新插件」导入 .wasm / .wat 产物。")
                                                    .size(12.0)
                                                    .color(pal.dim),
                                            );
                                            ui.add_space(6.0);
                                        }
                                        if !remove_ids.is_empty() {
                                            for id in &remove_ids {
                                                let _ = self.host.wasm_plugins.deactivate(id);
                                                let _ = self.host.settings.remove_plugin(id);
                                            }
                                            self.plugin_rows.retain(|r| !remove_ids.contains(&r.id));
                                            self.note = format!("已移除 {} 个插件", remove_ids.len());
                                        }
                                        ui.add_space(8.0);
                                        ui.horizontal(|ui| {
                                            if accent_button(ui, &pal, "保存插件设置") {
                                                self.save_preferences();
                                            }
                                            if ghost_button(ui, &pal, "＋ 添加新插件") {
                                                self.import_wasm_plugin();
                                            }
                                        });
                                        ui.add_space(6.0);
                                        ui.label(
                                            egui::RichText::new(
                                                "操作方式：点击「＋ 添加新插件」导入 .wasm/.wat；勾选即立即加载并执行可选 on_load，取消勾选即卸载。插件仅获得 host_log，默认没有 Shell、文件或网络权限；「移除」只删除登记，不删除你的原始文件。",
                                            )
                                            .size(11.0)
                                            .color(pal.dim),
                                        );
                                    }
                                    "记忆" => {
                                        // 标签切换：对话记忆 / 技能 / 知识库 / 代码图谱
                                        ui.horizontal_wrapped(|ui| {
                                            for (t, label) in [
                                                ("chat", "对话记忆"),
                                                ("skill", "技能"),
                                                ("wiki", "知识库"),
                                                ("code", "代码图谱"),
                                            ] {
                                                if ui
                                                    .selectable_value(
                                                        &mut self.mem_tab,
                                                        t.to_string(),
                                                        label,
                                                    )
                                                    .clicked()
                                                {
                                                    self.mem_loaded = false;
                                                }
                                            }
                                        });
                                        ui.add_space(6.0);
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new("搜索").size(12.0).color(pal.dim),
                                            );
                                            if ui
                                                .text_edit_singleline(&mut self.mem_query)
                                                .changed()
                                            {
                                                self.mem_loaded = false;
                                            }
                                        });
                                        ui.add_space(6.0);
                                        if self.mem_tab.is_empty() {
                                            self.mem_tab = "chat".into();
                                        }
                                        if !self.mem_loaded {
                                            // 首次打开：自动对当前工作区做一次资产索引
                                            //（扫描 SKILL.md / *.md / 源码 → Skill/Wiki/CodeGraph），
                                            // 之后记忆面板才有真实内容可见。
                                            if !self.mem_bootstrapped {
                                                self.bootstrap_mem();
                                            }
                                            self.refresh_mem();
                                            self.mem_loaded = true;
                                        }
                                        ui.horizontal(|ui| {
                                            if ghost_button(ui, &pal, "重新索引资产") {
                                                self.bootstrap_mem();
                                            }
                                            if !self.mem_index_msg.is_empty() {
                                                ui.label(
                                                    egui::RichText::new(&self.mem_index_msg)
                                                        .size(11.0)
                                                        .color(pal.dim),
                                                );
                                            }
                                        });
                                        ui.add_space(4.0);
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "共 {} 条 · 本地原生记忆（若已连接 aidops 后端，以远端为准）",
                                                self.mem_items.len()
                                            ))
                                            .size(11.0)
                                            .color(pal.dim),
                                        );
                                        ui.add_space(6.0);
                                        egui::ScrollArea::vertical().show(ui, |ui| {
                                            if self.mem_items.is_empty() {
                                                ui.label(
                                                    egui::RichText::new(
                                                        "暂无记忆。点击「重新索引资产」扫描工作区的 SKILL.md / 文档 / 源码，自动沉淀技能、知识库与代码图谱；对话中也会逐步沉淀对话记忆（L0~L3）。",
                                                    )
                                                    .size(12.0)
                                                    .color(pal.dim),
                                                );
                                            }
                                            for it in &self.mem_items {
                                                ui.group(|ui| {
                                                    ui.label(
                                                        egui::RichText::new(&it.title)
                                                            .size(13.0)
                                                            .color(pal.text)
                                                            .strong(),
                                                    );
                                                    ui.label(
                                                        egui::RichText::new(&it.meta)
                                                            .size(10.5)
                                                            .color(pal.dim),
                                                    );
                                                    ui.label(
                                                        egui::RichText::new(&it.body)
                                                            .size(12.0)
                                                            .color(pal.text),
                                                    );
                                                });
                                                ui.add_space(6.0);
                                            }
                                        });
                                    }
                                    "更新" => {
                                        self.draw_update_settings(ui, &pal);
                                    }
                                    _ => {
                                        field_label(ui, &pal, "默认访问权限");
                                        egui::ComboBox::from_id_salt("sys-perm")
                                            .width(260.0)
                                            .selected_text(&self.permission)
                                            .show_ui(ui, |ui| {
                                                for mode in ["只读", "工作区写入", "完全访问"] {
                                                    ui.selectable_value(&mut self.permission, mode.to_string(), mode);
                                                }
                                            });
                                        ui.add_space(14.0);
                                        if accent_button(ui, &pal, "保存系统设置") {
                                            self.save_preferences();
                                        }
                                        ui.add_space(10.0);
                                        field_label(ui, &pal, "aidops 后端连接（可选）");
                                        ui.label(
                                            egui::RichText::new(
                                                "配置后 dsh 把四类记忆资产同步到智程平台；留空则仅用本地文件记忆，桌面可独立工作。",
                                            )
                                            .size(11.0)
                                            .color(pal.dim),
                                        );
                                        ui.add(
                                            egui::TextEdit::singleline(&mut self.f_aidops_base)
                                                .desired_width(f32::INFINITY)
                                                .hint_text("后端地址，如 http://localhost:8000"),
                                        );
                                        ui.add(
                                            egui::TextEdit::singleline(&mut self.f_aidops_key)
                                                .desired_width(f32::INFINITY)
                                                .hint_text("API Key（可选；亦可用环境变量 AIDOPS_API_KEY）")
                                                .password(true),
                                        );
                                        ui.add(
                                            egui::TextEdit::singleline(&mut self.f_aidops_project)
                                                .desired_width(f32::INFINITY)
                                                .hint_text("项目 ID（可选，整数）"),
                                        );
                                        ui.add_space(10.0);
                                        field_label(ui, &pal, "配置文件 .harness.toml");
                                        ui.horizontal(|ui| {
                                            if ghost_button(ui, &pal, "重新加载") {
                                                match Config::load() {
                                                    Ok(cfg) => {
                                                        let _ = self.host.llm_control.reload_config(&cfg);
                                                        self.f_aidops_base = cfg.aidops.base_url;
                                                        self.f_aidops_key =
                                                            cfg.aidops.api_key.unwrap_or_default();
                                                        self.f_aidops_project = cfg
                                                            .aidops
                                                            .project_id
                                                            .map(|v| v.to_string())
                                                            .unwrap_or_default();
                                                        self.note = "已从 .harness.toml 重新加载并应用配置".into();
                                                    }
                                                    Err(e) => self.note = format!("加载失败: {e}"),
                                                }
                                            }
                                            if ghost_button(ui, &pal, "原子写入") {
                                                let mut cfg = Config::default();
                                                cfg.llm.provider = self.f_provider.clone();
                                                cfg.llm.base_url = self.f_base.clone();
                                                cfg.llm.model = self.f_model.clone();
                                                // 不写入 api_key：密钥经 AES-256-GCM 加密存储，明文落盘会泄露；
                                                // 热重载（reload_config）会回退到运行时缓存的 key。
                                                cfg.llm.reasoning_effort = self.effort();
                                                // aidops 后端连接（可选插件入口）：留空 base_url 即不启用。
                                                cfg.aidops.base_url = self.f_aidops_base.trim().to_string();
                                                cfg.aidops.api_key = if self.f_aidops_key.trim().is_empty() {
                                                    None
                                                } else {
                                                    Some(self.f_aidops_key.trim().to_string())
                                                };
                                                cfg.aidops.project_id =
                                                    self.f_aidops_project.trim().parse::<i64>().ok();
                                                match cfg.save_atomic(".harness.toml") {
                                                    Ok(()) => {
                                                        self.note = "配置已原子写入 .harness.toml（含 [aidops]，临时文件 + rename）".into()
                                                    }
                                                    Err(e) => self.note = format!("写入失败: {e}"),
                                                }
                                            }
                                        });
                                        ui.add_space(8.0);
                                        ui.label(
                                            egui::RichText::new(
                                                "「原子写入」先写临时文件再 rename，崩溃不会损坏原配置；「重新加载」把文件 [llm] 段热重载进运行时，无需重启。",
                                            )
                                            .size(11.0)
                                            .color(pal.dim),
                                        );
                                    }
                                });
                            // 记录面板矩形：供下一帧“点外部关闭”守卫判定。
                            self.modal_panel_rect = Some(ui.min_rect());
                        });
                });
            self.modal_open_last_frame = true;
        } else {
            self.modal_open_last_frame = false;
        }

        // ── 会话重命名弹窗 ───────────────────────────────────────
        if let Some(file) = self.renaming.clone() {
            egui::Window::new("重命名会话")
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(
                        "为这个会话设置便于识别的名称（写入旁挂 .title 文件，不影响日志内容）：",
                    );
                    ui.add_space(6.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut self.rename_buf)
                            .desired_width(300.0)
                            .hint_text("如：发布前的压测排障"),
                    );
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("确定").clicked() {
                            if let Some(dir) = self.history_dirs.get(&file).cloned() {
                                harness_session::rename_session(&dir, &file, &self.rename_buf);
                                self.refresh_history();
                            }
                            self.renaming = None;
                        }
                        if ui.button("取消").clicked() {
                            self.renaming = None;
                        }
                    });
                });
        }

        if send_now {
            self.submit();
        }

        // 轮询 SessionLog 需要周期重绘（egui 默认按需重绘）。
        ctx.request_repaint_after(std::time::Duration::from_millis(80));
    }
}

impl Ui for EguiUi {
    fn run(self: Arc<Self>, _bus: EventBusView, log: Arc<SessionLog>) {
        #[cfg(target_os = "macos")]
        let window_title = "";
        #[cfg(not(target_os = "macos"))]
        let window_title = "AIOPS Desktop";
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title(window_title)
                .with_inner_size([1280.0, 800.0])
                .with_min_inner_size([960.0, 600.0])
                // egui 0.30：窗口图标挂在 ViewportBuilder 上（旧版 NativeOptions.icon_data 已移除）。
                .with_icon(egui::IconData {
                    rgba: APP_ICON_RGBA.to_vec(),
                    width: APP_ICON_WIDTH,
                    height: APP_ICON_HEIGHT,
                }),
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
