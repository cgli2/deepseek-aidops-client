//! UI → 运行时的反向输入通道（trait）。
//!
//! 设计约束：UI 是事件总线纯消费者（只渲染 `SessionLog`），但它需要一个把"用户在输入框敲的字"
//! 变成一次后台 agent turn 的通道。`UiInputSink` 就是这个通道的抽象：GUI/TUI 只依赖此 trait，
//! 具体实现 `SessionController`（在 `harness-runtime`）注入进 `AppContext`，二者经 `harness-core`
//! 解耦，避免引入 `harness-ui → harness-runtime` 的反向依赖。
//!
//! 以 trait object（`Arc<dyn UiInputSink>`）存入 `AppContext` 作为服务，因此本 trait 必须带
//! `Any + Send + Sync + 'static` 超 trait（与 `dyn Shell` / `dyn LlmProvider` 同理），
//! 才能满足 `Service` 约束被 `ctx.provide` / `ctx.get` 存取。

use std::any::Any;
use std::sync::RwLock;

use crate::Attachment;

pub struct AccessPolicy {
    mode: RwLock<String>,
}
impl AccessPolicy {
    pub fn new(mode: impl Into<String>) -> Self {
        Self {
            mode: RwLock::new(mode.into()),
        }
    }
    pub fn set(&self, mode: impl Into<String>) {
        if let Ok(mut value) = self.mode.write() {
            *value = mode.into();
        }
    }
    pub fn mode(&self) -> String {
        self.mode
            .read()
            .map(|v| v.clone())
            .unwrap_or_else(|_| "只读".into())
    }
    pub fn allows(&self, tool: &str, args: &serde_json::Value) -> bool {
        if self.mode() == "只读" {
            return tool == "fs"
                && args
                    .get("op")
                    .and_then(|v| v.as_str())
                    .is_some_and(|op| matches!(op, "read" | "list"));
        }
        true
    }
}

/// 把用户输入驱动成后台回合的反向通道。
pub trait UiInputSink: Any + Send + Sync + 'static {
    /// 提交一条用户输入。实现方应后台串行执行；忙碌时输入进入 FIFO 队列。
    fn submit(&self, text: String);
    /// 提交带结构化附件的输入。默认兼容旧实现，附件不会阻断纯文本通道。
    fn submit_with_attachments(&self, text: String, _attachments: Vec<Attachment>) {
        self.submit(text);
    }
    /// 是否正在跑回合或有待执行任务。
    fn busy(&self) -> bool;
    /// 是否有任意会话仍在执行。默认与当前会话状态一致；多会话控制器可覆盖为全局状态。
    fn any_busy(&self) -> bool {
        self.busy()
    }
    /// 正在等待前序任务完成的输入数（不含当前运行回合）。
    fn queued_count(&self) -> usize {
        0
    }
    /// 等待前序任务的输入快照，供 UI 展示并允许用户在真正提交前撤回。
    fn queued_inputs(&self) -> Vec<QueuedInput> {
        Vec::new()
    }
    /// 撤回一条尚未开始执行的队列输入。返回是否成功撤回。
    fn remove_queued(&self, _id: u64) -> bool {
        false
    }
    /// 请求停止当前回合。无活动回合时应安全 no-op。
    fn cancel(&self) {}
    /// 清空当前上下文，开始新会话。忙碌时应拒绝或安全 no-op。
    fn new_session(&self) {}
    fn set_permission(&self, _mode: String) {}
    /// 切换工作区根并装载对应项目的会话历史（侧栏项目切换入口）。
    /// 默认 no-op：仅 `SessionController` 等持有运行时的实现需要真正切换。
    fn switch_workspace(&self, _path: &std::path::Path) {}
}

/// 尚未开始执行的输入。ID 在队列生命周期内稳定，避免 UI 刷新时按下标误删。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedInput {
    pub id: u64,
    pub text: String,
    pub attachments: Vec<Attachment>,
}

/// GUI 可在运行时更新模型连接，无需编辑配置文件并重启。
pub trait LlmControl: Any + Send + Sync + 'static {
    fn configure_provider(
        &self,
        provider: String,
        base_url: String,
        model: String,
        api_key: String,
        reasoning_effort: Option<String>,
    ) -> Result<(), String> {
        let _ = provider;
        self.configure_deepseek(base_url, model, api_key, reasoning_effort)
    }
    fn configure_deepseek(
        &self,
        base_url: String,
        model: String,
        api_key: String,
        reasoning_effort: Option<String>,
    ) -> Result<(), String>;
    /// 从已加载的 `Config`（通常是 `.harness.toml`）把 `[llm]` 段热重载进运行时，
    /// 无需重启（cc-switch 风格 hot-reload）。key 优先取环境变量 `api_key_env`，回退字面量。
    fn reload_config(&self, cfg: &crate::Config) -> Result<(), String> {
        let l = &cfg.llm;
        let key = std::env::var(&l.api_key_env)
            .ok()
            .or_else(|| l.api_key.clone())
            .unwrap_or_default();
        self.configure_provider(
            l.provider.clone(),
            l.base_url.clone(),
            l.model.clone(),
            key,
            l.reasoning_effort.clone(),
        )
    }
    fn status(&self) -> String;

    /// 从上游拉取模型列表（OpenAI 兼容 `GET {base_url}/models`）。
    /// 返回模型 id 列表；失败返回友好错误。默认实现返回空（不支持时 UI 隐藏按钮）。
    fn fetch_models(&self, base_url: String, api_key: String) -> Result<Vec<String>, String> {
        let _ = (base_url, api_key);
        Ok(Vec::new())
    }

    fn complete_one_shot(&self, _prompt: String) -> Result<String, String> {
        Err("one-shot completion not supported by this provider".into())
    }
}
