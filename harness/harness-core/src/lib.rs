//! harness-core：微内核。
//!
//! 移植 dsh 的"一切皆插件"思想：
//! - `AppContext` + `TypeMap<Arc<dyn Service>>` 服务仓库（dsh 的 `ctx.<key>`）；
//! - 类型化事件总线 `EventBusView`（emit / parallel / serial / waterfall 四种分发）；
//! - 可逆注册 `Registration`（RAII，等价于 dsh 的 `ctx.effect()` 自动回滚）；
//! - `Plugin` 抽象 + 依赖声明的拓扑组合（等价 dsh `inject` 加载顺序推导）；
//! - `ExtensionPoint` / `ExtensionRegistry`（功能→扩展点契约，见 `extensions/EXTENSION-COOKBOOK.md`）。
//!
//! 详见 `docs/system-design-completion.md`。

pub mod config;
pub mod context;
pub mod error;
pub mod event;
pub mod extension;
pub mod plugin;
pub mod tuning;
pub mod types;
pub mod ui_input;
pub mod update;
pub mod workspace;

pub use config::Config;
pub use context::{AppContext, Registration, Service};
pub use error::{Error, Result};
pub use event::{Event, EventBusView, Handler, SerialHandler, Waterfall};
pub use extension::{ExtensionPoint, ExtensionRegistry};
pub use plugin::{compose_plugins, topo_sort, ComposeGuard, Plugin};
pub use types::{ApprovalPolicy, Attachment, PermissionPreset, Profile, SandboxMode, UserInput};
pub use ui_input::{AccessPolicy, LlmControl, UiInputSink};
pub use update::{Release, UpdateStatus};
pub use workspace::Workspace;
