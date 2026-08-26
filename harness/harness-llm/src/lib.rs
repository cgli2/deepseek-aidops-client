//! harness-llm：`LlmProvider` trait + 消息/工具契约 + Provider 实现。
//!
//! 设计（原 §11）：流式输出经 `async_stream` 推送；工具调用由 schema → 模型 → `ToolCall` 回环。
//! `ReplayLlm` 用于 headless / 测试闭环；DeepSeek/OpenAI/Local（OpenAI 兼容）与 Anthropic
//! 均为真实 HTTP+SSE 流式实现（`sse` / `openai_compat` 模块共用解析）。

use std::any::Any;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};

pub use harness_core::error::{Error, Result};

mod anthropic;
mod deepseek;
mod local;
mod openai;
mod openai_compat;
mod replay;
mod sse;

pub mod dsml;
pub mod model_catalog;

pub use anthropic::Anthropic;
pub use deepseek::DeepSeek;
pub use local::LocalLlm;
pub use openai::OpenAI;

/// OpenAI 兼容服务的单次最大输出长度。默认 8192：旧默认 4096 在长交付/多工具宣告
/// 场景容易被截断，触发空响应重试与重复往返，反而更慢；部署方可用
/// `HARNESS_MAX_TOKENS` 在 256..=32768 范围调整，GUI「参数配置」页的显式设置优先于环境变量。
pub(crate) fn max_output_tokens() -> u64 {
    harness_core::tuning::max_output_tokens()
        .or_else(|| {
            std::env::var("HARNESS_MAX_TOKENS")
                .ok()
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or(8_192)
        .clamp(256, 32_768)
}
pub use replay::ReplayLlm;

/// 消息角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// 一次对话消息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: text.into(),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: text.into(),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }
    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: text.into(),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }

    pub fn assistant_with_tools(text: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: text.into(),
            tool_calls,
            tool_call_id: None,
        }
    }

    pub fn tool(call_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: text.into(),
            tool_calls: vec![],
            tool_call_id: Some(call_id.into()),
        }
    }
}

/// 流式分片（SSE 的一帧）。
///
/// `reasoning` 承载模型思考链增量（DeepSeek v4 `reasoning_content`）：
/// 仅用于 UI「思考中」反馈与会话日志展示，不进入模型上下文。
/// `usage` 承载一次请求的最终 token 用量（由 `stream_options.include_usage` 触发，
/// 在流末尾单独成帧），用于 AIOps 用量/成本计量，不进入模型上下文。
/// `empty_response` 表示上游正常结束却既没有正文也没有工具调用；它不是给用户展示的
/// 助手文本，Runtime 应根据 `finish_reason` 发起受限恢复重试。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Chunk {
    pub text: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default)]
    pub empty_response: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

/// 一次请求的 token 用量（AIOps 可观测性：每会话/每回合的 prompt/completion token）。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

impl Usage {
    /// 累加两条用量（按回合/会话聚合）。
    pub fn saturating_add(self, other: Usage) -> Usage {
        Usage {
            prompt_tokens: self.prompt_tokens.saturating_add(other.prompt_tokens),
            completion_tokens: self
                .completion_tokens
                .saturating_add(other.completion_tokens),
            total_tokens: self.total_tokens.saturating_add(other.total_tokens),
        }
    }
}

/// 模型请求的工具调用。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: serde_json::Value,
}

/// 工具 JSON schema（Function Calling）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub json_schema: serde_json::Value,
}

/// 工具执行结果。`continuation_debt` 控制 agent 循环续跑（原 §5.6）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub ok: bool,
    pub content: String,
    pub continuation_debt: usize,
}

/// 流式输出类型（对象安全：返回具体 `Pin<Box<...>>`，非 `impl Trait`）。
pub type ChunkStream = Pin<Box<dyn Stream<Item = Result<Chunk>> + Send>>;

/// 单次请求的运行时覆盖项。默认不改变用户配置；Runtime 仅在已识别为原子任务时
/// 收紧输出与思考，避免一个单点回归消耗完整的长推理预算。
#[derive(Debug, Clone, Default)]
pub struct RequestOptions {
    pub max_output_tokens: Option<u64>,
    pub reasoning_effort: Option<String>,
    /// Runtime 在每个执行阶段只把当前允许的工具 schema 发给模型。`None`
    /// 保持 Provider 的完整工具集，空列表则明确禁用工具调用。
    pub allowed_tools: Option<Vec<String>>,
}

/// 按运行时白名单裁剪编码工具。这里而不是 Provider 内部实现，确保所有上游
/// Provider 收到完全一致的工具面。
pub fn allowed_coding_tools(allowed: Option<&[String]>) -> Vec<ToolSchema> {
    let all = openai_compat::coding_tools();
    match allowed {
        None => all,
        Some(names) => all
            .into_iter()
            .filter(|tool| names.iter().any(|name| name == &tool.name))
            .collect(),
    }
}

/// LLM Provider 定义（能力接缝的 Definition）。Provider 可有多个（DeepSeek/OpenAI/.../replay）。
///
/// `: Any` 使 `dyn LlmProvider` 本身满足 `Service`，可作为 `Arc<dyn LlmProvider>` 注册进 `AppContext`。
#[async_trait]
pub trait LlmProvider: Any + Send + Sync {
    fn name(&self) -> &'static str;
    fn tools(&self) -> Vec<ToolSchema>;
    fn stream(&self, msgs: Vec<Message>) -> ChunkStream;
    fn stream_with_options(&self, msgs: Vec<Message>, _options: RequestOptions) -> ChunkStream {
        self.stream(msgs)
    }
}

/// 可热切换的模型 Provider，供 GUI 在运行时配置 DeepSeek 连接。
pub struct ManagedLlm {
    provider: RwLock<Arc<dyn LlmProvider>>,
    status: RwLock<String>,
    /// 最近一次成功配置的 API Key 镜像。用于在「配置文件不含密钥」的热重载场景下
    /// 兜底复用当前运行中的 key（密钥走 DPAPI，不落 TOML；对齐 cc-switch「配置 ≠ 请求路径」）。
    key: RwLock<String>,
}

impl ManagedLlm {
    pub fn new(provider: Arc<dyn LlmProvider>, status: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            provider: RwLock::new(provider),
            status: RwLock::new(status.into()),
            key: RwLock::new(String::new()),
        })
    }
}

#[async_trait]
impl LlmProvider for ManagedLlm {
    fn name(&self) -> &'static str {
        "managed"
    }

    fn tools(&self) -> Vec<ToolSchema> {
        self.provider.read().map(|p| p.tools()).unwrap_or_default()
    }

    fn stream(&self, msgs: Vec<Message>) -> ChunkStream {
        self.stream_with_options(msgs, RequestOptions::default())
    }

    fn stream_with_options(&self, msgs: Vec<Message>, options: RequestOptions) -> ChunkStream {
        match self.provider.read() {
            Ok(provider) => provider.clone().stream_with_options(msgs, options),
            Err(_) => Box::pin(futures::stream::once(async {
                Err(Error::Llm("model configuration lock poisoned".into()))
            })),
        }
    }
}

impl harness_core::LlmControl for ManagedLlm {
    fn configure_provider(
        &self,
        provider: String,
        base_url: String,
        model: String,
        key: String,
        reasoning_effort: Option<String>,
    ) -> std::result::Result<(), String> {
        let provider = provider.trim().to_string();
        if provider.is_empty() {
            return Err("厂商名称不能为空".into());
        }
        let base_url = base_url.trim().trim_end_matches('/').to_string();
        if !(base_url.starts_with("https://") || base_url.starts_with("http://")) {
            return Err("API 地址必须以 http:// 或 https:// 开头".into());
        }
        let model = model.trim().to_string();
        if model.is_empty() {
            return Err("模型名称不能为空".into());
        }
        // 优先用本次输入的 key；为空则回退运行时缓存（兼容“只在设置页配过一次”的场景）。
        let key = key.trim().to_string();
        let key = if !key.is_empty() {
            key
        } else {
            self.key.read().map(|k| k.clone()).unwrap_or_default()
        };
        if key.is_empty() {
            return Err("API Key 不能为空，请先在设置页配置".into());
        }
        // 缓存 key，供 reload_config 在文件缺密钥时兜底复用。
        if let Ok(mut k) = self.key.write() {
            *k = key.clone();
        }
        // 按厂商构造对应 provider 并真正替换运行时实例：切换模型即时生效。
        let next: Arc<dyn LlmProvider> = match provider.as_str() {
            "deepseek" => crate::DeepSeek::new(base_url, key, model.clone(), reasoning_effort),
            _ => crate::OpenAI::with_endpoint(base_url, key, model.clone()),
        };
        *self.provider.write().map_err(|_| "模型配置锁异常")? = next;
        *self.status.write().map_err(|_| "模型状态锁异常")? =
            format!("{provider} / {model} / 已配置 Key");
        Ok(())
    }

    fn configure_deepseek(
        &self,
        base_url: String,
        model: String,
        api_key: String,
        reasoning_effort: Option<String>,
    ) -> std::result::Result<(), String> {
        let base_url = base_url.trim().trim_end_matches('/').to_string();
        let model = model.trim().to_string();
        let api_key = api_key.trim().to_string();
        if !(base_url.starts_with("https://") || base_url.starts_with("http://")) {
            return Err("API 地址必须以 http:// 或 https:// 开头".into());
        }
        if model.is_empty() {
            return Err("模型名称不能为空".into());
        }
        if api_key.is_empty() {
            return Err("API Key 不能为空".into());
        }
        // 缓存 key，供 reload_config 在文件缺密钥时兜底复用。
        if let Ok(mut k) = self.key.write() {
            *k = api_key.clone();
        }
        let next = DeepSeek::new(base_url, api_key, model.clone(), reasoning_effort);
        *self.provider.write().map_err(|_| "模型配置锁异常")? = next;
        *self.status.write().map_err(|_| "模型状态锁异常")? =
            format!("DeepSeek / {model} / 已配置 Key");
        Ok(())
    }

    fn reload_config(&self, cfg: &harness_core::Config) -> std::result::Result<(), String> {
        let llm = &cfg.llm;
        if llm.base_url.trim().is_empty() || llm.model.trim().is_empty() {
            return Err("配置文件 [llm] 缺少 base_url 或 model，无法热重载".into());
        }
        // 文件缺密钥时回退到运行时已缓存的 key（DPAPI 密钥不落 TOML）。
        let key = match &llm.api_key {
            Some(k) if !k.trim().is_empty() => k.trim().to_string(),
            _ => self.key.read().map(|k| k.clone()).unwrap_or_default(),
        };
        if key.is_empty() {
            return Err("运行时无可用 API Key；请先在设置页配置密钥再热重载".into());
        }
        self.configure_provider(
            llm.provider.clone(),
            llm.base_url.clone(),
            llm.model.clone(),
            key,
            llm.reasoning_effort.clone(),
        )?;
        Ok(())
    }

    fn status(&self) -> String {
        self.status
            .read()
            .map(|s| s.clone())
            .unwrap_or_else(|_| "模型状态不可用".into())
    }

    fn fetch_models(
        &self,
        base_url: String,
        api_key: String,
    ) -> std::result::Result<Vec<String>, String> {
        crate::openai_compat::fetch_models(base_url, api_key)
    }

    fn complete_one_shot(&self, prompt: String) -> std::result::Result<String, String> {
        use futures::StreamExt;
        let provider = self
            .provider
            .read()
            .map_err(|_| "model configuration lock poisoned".to_string())?
            .clone();
        let msgs = vec![
            Message::system(
                "You are a prompt optimization assistant. Rewrite the user's input to be clear, \
                 well-structured, and friendly for an LLM to process. Output ONLY the optimized \
                 prompt text, nothing else.",
            ),
            Message::user(&prompt),
        ];
        let rt =
            tokio::runtime::Runtime::new().map_err(|e| format!("failed to create runtime: {e}"))?;
        rt.block_on(async move {
            let mut stream = provider.stream(msgs);
            let mut parts: Vec<String> = Vec::new();
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(c) => {
                        if let Some(t) = c.text {
                            parts.push(t);
                        }
                    }
                    Err(e) => return Err(format!("stream error: {e}")),
                }
            }
            let result = parts.join("").trim().to_string();
            if result.is_empty() {
                return Err("LLM returned empty response".into());
            }
            Ok(result)
        })
    }
}

#[cfg(test)]
mod managed_tests {
    use super::*;
    use harness_core::LlmControl;

    #[test]
    fn filters_tool_schemas_to_runtime_whitelist() {
        let allowed = vec!["search".to_string(), "fs".to_string()];
        let names = allowed_coding_tools(Some(&allowed))
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["fs", "search"]);
        assert!(allowed_coding_tools(Some(&[])).is_empty());
    }

    #[test]
    fn runtime_configuration_validates_and_switches_provider() {
        let managed = ManagedLlm::new(ReplayLlm::new(vec![]), "演示模式");
        assert!(managed
            .configure_deepseek("bad".into(), "deepseek-chat".into(), "key".into(), None)
            .is_err());
        assert!(managed
            .configure_deepseek(
                "https://api.deepseek.com".into(),
                "".into(),
                "key".into(),
                None
            )
            .is_err());
        assert!(managed
            .configure_deepseek(
                "https://api.deepseek.com".into(),
                "deepseek-chat".into(),
                "".into(),
                None
            )
            .is_err());
        managed
            .configure_deepseek(
                "https://api.deepseek.com/".into(),
                "deepseek-chat".into(),
                "secret".into(),
                None,
            )
            .unwrap();
        assert!(managed.status().contains("已配置 Key"));
    }

    /// 验证 configure_provider 真正替换运行时 provider 并缓存 key。
    /// 修复前它只更新 status 文字、不替换 provider（切换模型不生效）。
    #[test]
    fn configure_provider_swaps_runtime_provider() {
        let managed = ManagedLlm::new(ReplayLlm::new(vec![]), "演示模式");

        // 非法地址：拒绝
        assert!(managed
            .configure_provider(
                "openai".into(),
                "bad".into(),
                "deepseek-v4".into(),
                "k".into(),
                None
            )
            .is_err());

        // 合法 openai 厂商：成功并替换 provider
        managed
            .configure_provider(
                "openai".into(),
                "http://localhost:8000".into(),
                "deepseek-v4".into(),
                "sk-test".into(),
                None,
            )
            .unwrap();
        assert!(managed.status().contains("deepseek-v4"));
        assert!(managed.status().contains("已配置 Key"));

        // deepseek 厂商：同样成功
        managed
            .configure_provider(
                "deepseek".into(),
                "https://api.deepseek.com".into(),
                "deepseek-reasoner".into(),
                "sk-ds".into(),
                Some("high".into()),
            )
            .unwrap();
        assert!(managed.status().contains("deepseek-reasoner"));

        // key 缓存：空 key 时回退上次配置的 key，不报错
        managed
            .configure_provider(
                "openai".into(),
                "http://localhost:8000".into(),
                "other-model".into(),
                String::new(),
                None,
            )
            .unwrap();
        assert!(managed.status().contains("other-model"));
    }
}
