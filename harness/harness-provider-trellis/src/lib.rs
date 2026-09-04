//! Trellis Provider：spec 驱动开发插件（可开可关）。
//!
//! 核心思想（源自 mindfold-ai/Trellis）：把"项目规格"当作第一类对象，
//! 以 *任务状态机*（新任务 / 进行中 / 完成）驱动开发循环。
//!
//! 本插件与平台的关系（不破坏原有架构）：
//! - 复用平台既有的 `Plugin` 抽象与事件总线 **waterfall** 扩展点，不新增内核概念；
//! - 通过 `on_waterfall::<PreStep>` 注入 around-middleware：
//!   1. `spec_file` 存在时，把项目规格注入系统消息（LLM 持续可见）；
//!   2. `tasks_file` 存在时，维护 JSON 任务状态机文件（新任务 / 进行中 / 完成）。
//! - **运行时启停**：`register()` 始终注册 PreStep 监听并 `provide` 一个
//!   [`TrellisControl`] 服务；`enabled` 为 false 时中间件直接透传（零副作用），
//!   UI / 其它消费者可随时通过 `TrellisControl::set_enabled` 热启停，无需重启。
//!
//! 与平台已有能力的重叠/冲突评估（详见仓库根 `docs/trellis-eval.md`）：
//! - `harness-provider-hook` 是外部命令钩子（进程边界），本插件是**进程内**数据面注入，互不冲突；
//! - 平台的任务/会话体系（harness-runtime task/session）管"执行编排"，本插件管"开发意图",
//!   二者是不同抽象层次，通过 PreStep 消息改写桥接，不触碰执行内核；
//! - 默认关闭，按需开启，架构零改动。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use harness_core::config::TrellisConfig;
use harness_core::context::{AppContext, Registration};
use harness_core::event::Waterfall;
use harness_core::plugin::Plugin;
use harness_llm::{Message, Role};
use harness_runtime::PreStep;

/// 任务状态机中的一条任务。
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct TaskItem {
    id: String,
    title: String,
    status: String, // "new" | "in_progress" | "done"
}

/// Trellis 的运行时控制句柄（注册为 `AppContext` 服务）。
///
/// UI 的「插件管理」通过它热启用 / 停用 Trellis，并更换规格 / 任务文件；
/// 所有变更对下一轮 PreStep 瀑布立即生效（无需重启）。
pub struct TrellisControl {
    enabled: AtomicBool,
    spec_file: Mutex<String>,
    tasks_file: Mutex<String>,
}

impl TrellisControl {
    pub fn new(enabled: bool, spec_file: String, tasks_file: String) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
            spec_file: Mutex::new(spec_file),
            tasks_file: Mutex::new(tasks_file),
        }
    }

    /// 当前是否启用（下一轮 PreStep 立即生效）。
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// 热启用 / 停用：仅切开关，不注销监听；关闭时中间件零副作用透传。
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    pub fn spec_file(&self) -> String {
        self.spec_file
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn tasks_file(&self) -> String {
        self.tasks_file
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 更换规格文件路径（空字符串 = 停止注入规格）。
    pub fn set_spec_file(&self, path: String) {
        *self.spec_file.lock().unwrap_or_else(|e| e.into_inner()) = path;
    }

    /// 更换任务状态机文件路径（空字符串 = 停止维护任务）。
    pub fn set_tasks_file(&self, path: String) {
        *self.tasks_file.lock().unwrap_or_else(|e| e.into_inner()) = path;
    }
}

/// 约定路径自动推导：用户未显式配置（空字符串）时，基于工作区根目录拼接约定相对路径。
///
/// 约定：
/// - 规格文件 → `<workspace_root>/.harness/spec.md`
/// - 任务文件 → `<workspace_root>/.harness/tasks.json`
///
/// 用户显式配置的路径（绝对或相对）原样保留，不做转换。
fn resolve_default(configured: &str, workspace_root: &str, filename: &str) -> String {
    if !configured.trim().is_empty() {
        return configured.to_string();
    }
    let root = std::path::Path::new(workspace_root);
    root.join(".harness")
        .join(filename)
        .to_string_lossy()
        .into_owned()
}

/// Trellis 插件主体：持有配置与共享控制句柄，注册 PreStep 瀑布监听。
pub struct TrellisPlugin {
    config: TrellisConfig,
    control: Arc<TrellisControl>,
}

impl TrellisPlugin {
    pub fn new(config: TrellisConfig, workspace_root: &str) -> Arc<Self> {
        // 约定路径自动装配：spec_file / tasks_file 为空时，基于工作区根目录推导
        // .harness/spec.md 与 .harness/tasks.json，无需手工录入。
        let spec_file = resolve_default(&config.spec_file, workspace_root, "spec.md");
        let tasks_file = resolve_default(&config.tasks_file, workspace_root, "tasks.json");
        Arc::new(Self {
            control: Arc::new(TrellisControl::new(config.enabled, spec_file, tasks_file)),
            config,
        })
    }

    /// 暴露控制句柄（供 bin 装配注入 UI；与 `register` 内 provide 的是同一实例）。
    pub fn control(&self) -> Arc<TrellisControl> {
        self.control.clone()
    }

    /// 暴露当前配置（供 UI 展示/调试），同时消除字段 dead_code 警告。
    pub fn config(&self) -> &TrellisConfig {
        &self.config
    }
}

impl Plugin for TrellisPlugin {
    fn name(&self) -> &'static str {
        "trellis"
    }

    fn deps(&self) -> &[&'static str] {
        &[]
    }

    fn register(self: Arc<Self>, ctx: &AppContext) -> Vec<Registration> {
        let control = self.control.clone();
        let mut regs = vec![ctx.provide(control.clone())];
        regs.push(
            ctx.events()
                .on_waterfall::<PreStep>(Arc::new(TrellisMiddleware { control })),
        );
        regs
    }
}

/// PreStep 瀑布中间件：注入规格 + 维护任务状态机。关闭时直接透传（零副作用）。
struct TrellisMiddleware {
    control: Arc<TrellisControl>,
}

impl Waterfall<PreStep> for TrellisMiddleware {
    fn call(&self, args: PreStep, next: &dyn Fn(PreStep) -> PreStep) -> PreStep {
        if !self.control.enabled() {
            return next(args);
        }
        let mut messages = args.input;
        let spec_file = self.control.spec_file();
        let tasks_file = self.control.tasks_file();

        // 1) 规格注入：系统消息中附加项目规格（LLM 每步都可见）。
        if !spec_file.trim().is_empty() {
            if let Ok(spec) = fs::read_to_string(&spec_file) {
                if let Some(spec) = inject_spec(&messages, &spec) {
                    messages.push(spec);
                }
            }
        }

        // 2) 任务状态机维护：以用户最近一条消息为输入，读取/更新 JSON 任务文件。
        if !tasks_file.trim().is_empty() {
            if let Some(last_user) = last_user_text(&messages) {
                sync_tasks(&tasks_file, last_user);
            }
        }

        next(PreStep { input: messages })
    }
}

/// 若规格尚未注入过（以 "## 项目规格" 标记为幂等键），追加一条系统消息。
fn inject_spec(messages: &[Message], spec: &str) -> Option<Message> {
    const MARK: &str = "## 项目规格";
    let already = messages
        .iter()
        .any(|m| m.role == Role::System && m.content.contains(MARK));
    if already {
        return None;
    }
    Some(Message::system(format!(
        "{MARK}\n{spec}\n\n请以本规格为准推进开发，并同步维护任务状态。"
    )))
}

/// 取最后一条用户消息文本（无则返回 None）。
fn last_user_text(messages: &[Message]) -> Option<&str> {
    messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .map(|m| m.content.as_str())
}

/// 任务状态机：读取 tasks_file（缺省为空列表），若用户消息命中 "完成 X" 则标记 done，
/// 否则若出现新任务关键字则追加 new 任务；回写 JSON。任何失败静默忽略（不阻断循环）。
fn sync_tasks(tasks_file: &str, user_text: &str) {
    let path = PathBuf::from(tasks_file);
    let mut tasks: Vec<TaskItem> = match fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    for t in tasks.iter_mut() {
        let done_mark = format!("完成 {}", t.title);
        if user_text.contains(&done_mark) {
            t.status = "done".to_string();
        }
    }

    // 简单启发：包含 "任务:" 或 "- [ ]" 视为新增任务意图；未匹配现有任务则追加。
    let new_mark: Vec<&str> = user_text
        .lines()
        .filter(|l| l.contains("任务:") || l.contains("- [ ]"))
        .collect();
    for line in new_mark {
        let title = line
            .trim()
            .trim_start_matches("- [ ]")
            .trim_start_matches("任务:")
            .trim()
            .to_string();
        if title.is_empty() {
            continue;
        }
        if !tasks.iter().any(|t| t.title == title) {
            tasks.push(TaskItem {
                id: format!("t-{}", tasks.len() + 1),
                title,
                status: "new".to_string(),
            });
        }
    }

    let _ = fs::write(
        &path,
        serde_json::to_string_pretty(&tasks).unwrap_or_default(),
    );
}
