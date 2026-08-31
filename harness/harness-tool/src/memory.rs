//! `MemoryTool`：模型可见的"记忆/知识"工具（Consumer 侧）。
//!
//! 仅依赖 `harness_capability::assets` 的 Definition trait，不感知具体 Provider
//! （原生文件实现或 aidops 后端）—— 印证"换 Provider 不改 Consumer"（不变量 2）。
//! 同时具备离线兜底：aidops 后端不可用时由原生 Provider 接管。

use std::sync::Arc;

use async_trait::async_trait;
use harness_capability::assets::{
    CodeGraph, ConversationMemory, FactKind, LifecycleLayer, SkillLibrary, WikiStore,
};
use harness_core::error::Result;
use harness_llm::{ToolCall, ToolResult};

pub struct MemoryTool {
    conv: Arc<dyn ConversationMemory>,
    skill: Arc<dyn SkillLibrary>,
    wiki: Arc<dyn WikiStore>,
    code: Arc<dyn CodeGraph>,
}

impl MemoryTool {
    pub fn new(
        conv: Arc<dyn ConversationMemory>,
        skill: Arc<dyn SkillLibrary>,
        wiki: Arc<dyn WikiStore>,
        code: Arc<dyn CodeGraph>,
    ) -> Arc<Self> {
        Arc::new(Self {
            conv,
            skill,
            wiki,
            code,
        })
    }

    fn ok(call: &ToolCall, content: String) -> ToolResult {
        ToolResult {
            call_id: call.id.clone(),
            ok: true,
            content,
            continuation_debt: 0,
        }
    }

    fn fail(call: &ToolCall, msg: String) -> ToolResult {
        ToolResult {
            call_id: call.id.clone(),
            ok: false,
            content: msg,
            continuation_debt: 0,
        }
    }
}

#[async_trait]
impl crate::registry::DynTool for MemoryTool {
    fn name(&self) -> &'static str {
        "memory"
    }

    async fn call(&self, call: &ToolCall) -> Result<ToolResult> {
        let args = &call.args;
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("recall");

        match action {
            "recall" => {
                let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                let min_layer = args
                    .get("min_layer")
                    .and_then(|v| v.as_str())
                    .map(LifecycleLayer::parse)
                    .unwrap_or(LifecycleLayer::L2);
                let facts = self.conv.recall(query, min_layer).await?;
                Ok(Self::ok(
                    call,
                    serde_json::to_string_pretty(&facts).unwrap_or_default(),
                ))
            }
            "remember" => {
                let content = match args.get("content").and_then(|v| v.as_str()) {
                    Some(c) if !c.is_empty() => c.to_string(),
                    _ => return Ok(Self::fail(call, "content 不能为空".into())),
                };
                let kind = match args.get("kind").and_then(|v| v.as_str()) {
                    Some("preference") => FactKind::Preference,
                    Some("decision") => FactKind::Decision,
                    _ => FactKind::Fact,
                };
                let layer = args
                    .get("layer")
                    .and_then(|v| v.as_str())
                    .map(LifecycleLayer::parse)
                    .unwrap_or(LifecycleLayer::L2);
                let id = format!(
                    "fact:{}",
                    args.get("id").and_then(|v| v.as_str()).unwrap_or("manual")
                );
                self.conv
                    .remember(harness_capability::assets::MemoryFact {
                        id,
                        kind,
                        content,
                        layer,
                        confidence: 0.9,
                        source: "memory_tool".into(),
                    })
                    .await?;
                Ok(Self::ok(call, "已记住".into()))
            }
            "skills" => {
                let context = args.get("context").and_then(|v| v.as_str()).unwrap_or("");
                let skills = self.skill.match_skills(context).await?;
                Ok(Self::ok(
                    call,
                    serde_json::to_string_pretty(&skills).unwrap_or_default(),
                ))
            }
            "wiki" => {
                let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                let pages = self.wiki.query_pages(query).await?;
                Ok(Self::ok(
                    call,
                    serde_json::to_string_pretty(&pages).unwrap_or_default(),
                ))
            }
            "codegraph" => {
                // 子动作：query / callers_of / callees_of / impact_path
                let sub = args.get("op").and_then(|v| v.as_str()).unwrap_or("query");
                let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                match sub {
                    "callers_of" => {
                        let r = self.code.callers_of(id).await?;
                        Ok(Self::ok(
                            call,
                            serde_json::to_string_pretty(&r).unwrap_or_default(),
                        ))
                    }
                    "callees_of" => {
                        let r = self.code.callees_of(id).await?;
                        Ok(Self::ok(
                            call,
                            serde_json::to_string_pretty(&r).unwrap_or_default(),
                        ))
                    }
                    "impact_path" => {
                        let r = self.code.impact_path(id).await?;
                        Ok(Self::ok(
                            call,
                            serde_json::to_string_pretty(&r).unwrap_or_default(),
                        ))
                    }
                    _ => {
                        let r = self.code.query_symbols(query).await?;
                        Ok(Self::ok(
                            call,
                            serde_json::to_string_pretty(&r).unwrap_or_default(),
                        ))
                    }
                }
            }
            other => Ok(Self::fail(call, format!("未知 action: {other}"))),
        }
    }
}
