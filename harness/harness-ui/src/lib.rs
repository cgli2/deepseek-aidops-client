//! harness-ui：UI 入口（trait）。默认仅 `Ui` trait + `NullUi`；`tui` / `gui` 为 feature 门控实现。
//!
//! UI 是事件总线的纯消费者：在独立 tokio 任务运行，仅订阅 `SessionEvent` 渲染，不反向调用核心（原 §10）。

use std::any::Any;
use std::sync::Arc;

use harness_core::event::EventBusView;
use harness_session::SessionLog;

/// UI 入口定义（Definition）。实现：TUI（默认产品形态）/ GUI（egui）/ headless `NullUi`。
///
/// 带 `Any` 超 trait，使 `Arc<dyn Ui>` 满足 `harness_core::Service`，可作为服务注册进
/// `AppContext`（与 `Arc<dyn Shell>` 等 trait 对象同构，见完成文档 §11.3）。
pub trait Ui: Any + Send + Sync + 'static {
    fn run(self: Arc<Self>, bus: EventBusView, log: Arc<SessionLog>);
}

/// headless / 测试默认 UI（no-op 消费者）。保留给测试与无渲染场景使用。
pub struct NullUi;

impl Ui for NullUi {
    fn run(self: Arc<Self>, _bus: EventBusView, _log: Arc<SessionLog>) {}
}

/// 终端可见渲染器（Headless 默认）：轮询 `SessionLog` 打印 transcript。
pub mod console;
pub use console::ConsoleUi;
pub mod settings;
pub use settings::{ModelProfile, PluginRow, ProjectRow, SettingsDb};

#[cfg(feature = "tui")]
mod tui;
#[cfg(feature = "tui")]
pub use tui::TuiUi;

#[cfg(feature = "gui")]
mod gui;
#[cfg(feature = "gui")]
mod markdown;
#[cfg(feature = "gui")]
mod window_chrome;
#[cfg(feature = "gui")]
pub use gui::EguiUi;
