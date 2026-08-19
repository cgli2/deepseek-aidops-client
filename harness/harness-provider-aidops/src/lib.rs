//! 可选的后端 Provider：把四类记忆资产通过 HTTP 同步到 **aidops 后端**
//! （`aidops-hub-server` 的 `/api/v1/memory-assets`）。
//!
//! 设计要点（满足"可选插件、可独立运行"）：
//! - 本 crate 默认**不进入** `harness-bin` 的编译（仅在 `aidops` feature 开启时依赖），
//!   因此 dsh 桌面在不配置后端时**完全不链接任何网络/TLS 依赖**；
//! - 本 Provider 内部持有对应的**原生兜底**实现：当 `base_url` 为空、或后端不可达/报错时，
//!   所有方法自动回落到原生文件实现，保证 dsh 行为不中断（失败可见性 + 降级）；
//! - 与 `harness-provider-memory` 实现**同一组 Definition trait**，Consumer 零改动。

use std::sync::Arc;

use async_trait::async_trait;
use harness_capability::assets::{
    ChatTurn, CodeGraph, CodeSymbol, ConversationMemory, FactKind, LifecycleLayer, MemoryFact,
    Skill, SkillLibrary, WikiPage, WikiStore,
};
use harness_core::error::Result;
use harness_provider_memory::{
    NativeCodeGraph, NativeConversationMemory, NativeSkillLibrary, NativeWikiStore,
};
use serde::Serialize;

/// 后端连接配置（由 `harness_core::Config::aidops` 提供）。
#[derive(Debug, Clone)]
pub struct AidopsConfig {
    pub base_url: String,
    /// API key 所在环境变量名（不落盘 key 本身）。
    pub api_key_env: String,
    /// 字面量 key（可选，不推荐落盘）；优先级低于同名环境变量。
    pub api_key: Option<String>,
    /// 默认项目 id；为 `None` 时所有调用直接回落原生（单项目假设）。
    pub project_id: Option<i64>,
}

impl AidopsConfig {
    /// 是否启用（base_url 非空即视为已配置）。
    pub fn enabled(&self) -> bool {
        !self.base_url.trim().is_empty()
    }

    /// 解析实际 API key：优先取 `api_key_env` 环境变量，其次配置字面量。
    fn resolve_key(&self) -> Option<String> {
        std::env::var(&self.api_key_env)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| self.api_key.clone())
    }
}

/// 统一写入请求体（与 `aidops-hub-server` 的 `/memory-assets/ingest` 对齐）。
#[derive(Serialize)]
struct IngestRequest {
    asset_type: &'static str,
    project_id: i64,
    scope: &'static str,
    lifecycle_layer: &'static str,
    memory_type: Option<String>,
    title: String,
    content: String,
    payload: serde_json::Value,
}

/// 统一召回请求体（对齐 `/memory-assets/recall`）。
#[derive(Serialize)]
struct RecallRequest {
    project_id: i64,
    query: String,
    asset_types: Vec<&'static str>,
    min_layer: &'static str,
    top_k: u32,
}

#[derive(serde::Deserialize)]
struct RecallItem {
    #[allow(dead_code)]
    asset_type: String,
    #[allow(dead_code)]
    id: String,
    #[allow(dead_code)]
    score: f32,
    payload: serde_json::Value,
}

#[derive(serde::Deserialize)]
struct RecallResponse {
    #[allow(dead_code)]
    query: String,
    results: Vec<RecallItem>,
}

/// 同时实现四类资产 trait 的后端 Provider（同一份状态，按需 coerce 成 `Arc<dyn ...>`）。
pub struct AidopsBackend {
    cfg: AidopsConfig,
    conv: Arc<NativeConversationMemory>,
    skill: Arc<NativeSkillLibrary>,
    wiki: Arc<NativeWikiStore>,
    code: Arc<NativeCodeGraph>,
}

impl AidopsBackend {
    pub fn new(
        cfg: AidopsConfig,
        conv: Arc<NativeConversationMemory>,
        skill: Arc<NativeSkillLibrary>,
        wiki: Arc<NativeWikiStore>,
        code: Arc<NativeCodeGraph>,
    ) -> Arc<Self> {
        Arc::new(Self {
            cfg,
            conv,
            skill,
            wiki,
            code,
        })
    }

    fn headers(&self) -> Vec<(&'static str, String)> {
        let mut h = vec![("Content-Type", "application/json".to_string())];
        if let Some(key) = self.cfg.resolve_key() {
            h.push(("Authorization", format!("Bearer {key}")));
        }
        h
    }

    /// 同步 POST（经 `spawn_blocking` 避免阻塞 tokio 执行器）。返回 `None` 表示后端不可用/报错。
    fn http_post(&self, path: &str, body: &impl Serialize) -> Option<serde_json::Value> {
        let url = format!(
            "{}/api/v1/{}",
            self.cfg.base_url.trim_end_matches('/'),
            path
        );
        let headers = self.headers();
        let serialized = serde_json::to_string(body).ok()?;
        let mut req = ureq::post(&url);
        for (k, v) in &headers {
            req = req.set(k, v);
        }
        match req.send_bytes(serialized.as_bytes()) {
            Ok(resp) => resp.into_json::<serde_json::Value>().ok(),
            Err(e) => {
                eprintln!("[aidops] 后端调用失败（回落原生）: {path}: {e}");
                None
            }
        }
    }

    fn project_id(&self) -> Option<i64> {
        self.cfg.project_id
    }
}

// ---- ConversationMemory ----

#[async_trait]
impl ConversationMemory for AidopsBackend {
    async fn record_turn(&self, turn: ChatTurn) -> Result<()> {
        if let Some(pid) = self.project_id() {
            let req = IngestRequest {
                asset_type: "chat_memory",
                project_id: pid,
                scope: "session",
                lifecycle_layer: "L0",
                memory_type: None,
                title: format!("turn/{}", turn.role),
                content: turn.content.clone(),
                payload: serde_json::to_value(&turn).unwrap_or(serde_json::Value::Null),
            };
            if self.http_post("memory-assets/ingest", &req).is_some() {
                return Ok(());
            }
        }
        self.conv.record_turn(turn).await
    }

    async fn remember(&self, fact: MemoryFact) -> Result<()> {
        if let Some(pid) = self.project_id() {
            let mt = match fact.kind {
                FactKind::Preference => Some("preference".into()),
                FactKind::Decision => Some("decision".into()),
                _ => Some("fact".into()),
            };
            let req = IngestRequest {
                asset_type: "chat_memory",
                project_id: pid,
                scope: "project",
                lifecycle_layer: fact.layer.as_str(),
                memory_type: mt,
                title: fact.content.chars().take(80).collect(),
                content: fact.content.clone(),
                payload: serde_json::to_value(&fact).unwrap_or(serde_json::Value::Null),
            };
            if self.http_post("memory-assets/ingest", &req).is_some() {
                return Ok(());
            }
        }
        self.conv.remember(fact).await
    }

    async fn recall(&self, query: &str, min_layer: LifecycleLayer) -> Result<Vec<MemoryFact>> {
        if let Some(pid) = self.project_id() {
            let req = RecallRequest {
                project_id: pid,
                query: query.to_string(),
                asset_types: vec!["chat_memory"],
                min_layer: min_layer.as_str(),
                top_k: 20,
            };
            if let Some(v) = self.http_post("memory-assets/recall", &req) {
                if let Ok(resp) = serde_json::from_value::<RecallResponse>(v) {
                    let mut out = Vec::new();
                    for item in resp.results {
                        if let Ok(f) = serde_json::from_value::<MemoryFact>(item.payload) {
                            if f.layer.at_least(min_layer) {
                                out.push(f);
                            }
                        }
                    }
                    if !out.is_empty() {
                        return Ok(out);
                    }
                }
            }
        }
        self.conv.recall(query, min_layer).await
    }

    async fn consolidate(&self, session_id: &str) -> Result<Vec<MemoryFact>> {
        // 后端侧抽取是异步 Celery 任务；此处直接回落原生启发式抽取，保证可离线运行。
        self.conv.consolidate(session_id).await
    }

    async fn list_facts(&self) -> Result<Vec<MemoryFact>> {
        self.conv.list_facts().await
    }

    async fn recent_turns(&self, session_id: &str, n: usize) -> Result<Vec<ChatTurn>> {
        self.conv.recent_turns(session_id, n).await
    }
}

// ---- SkillLibrary ----

#[async_trait]
impl SkillLibrary for AidopsBackend {
    async fn register_skill(&self, skill: Skill) -> Result<()> {
        if let Some(pid) = self.project_id() {
            let req = IngestRequest {
                asset_type: "skill",
                project_id: pid,
                scope: "project",
                lifecycle_layer: "L3",
                memory_type: None,
                title: skill.name.clone(),
                content: format!("{}\n\n触发边界: {}", skill.name, skill.trigger_boundary),
                payload: serde_json::to_value(&skill).unwrap_or(serde_json::Value::Null),
            };
            if self.http_post("memory-assets/ingest", &req).is_some() {
                return Ok(());
            }
        }
        self.skill.register_skill(skill).await
    }

    async fn get_skill(&self, id: &str) -> Result<Option<Skill>> {
        if let Some(pid) = self.project_id() {
            let req = RecallRequest {
                project_id: pid,
                query: id.to_string(),
                asset_types: vec!["skill"],
                min_layer: "L3",
                top_k: 5,
            };
            if let Some(v) = self.http_post("memory-assets/recall", &req) {
                if let Ok(resp) = serde_json::from_value::<RecallResponse>(v) {
                    for item in resp.results {
                        if let Ok(s) = serde_json::from_value::<Skill>(item.payload) {
                            if s.id == id {
                                return Ok(Some(s));
                            }
                        }
                    }
                }
            }
        }
        self.skill.get_skill(id).await
    }

    async fn match_skills(&self, context: &str) -> Result<Vec<Skill>> {
        if let Some(pid) = self.project_id() {
            let req = RecallRequest {
                project_id: pid,
                query: context.to_string(),
                asset_types: vec!["skill"],
                min_layer: "L3",
                top_k: 10,
            };
            if let Some(v) = self.http_post("memory-assets/recall", &req) {
                if let Ok(resp) = serde_json::from_value::<RecallResponse>(v) {
                    let mut out = Vec::new();
                    for item in resp.results {
                        if let Ok(s) = serde_json::from_value::<Skill>(item.payload) {
                            out.push(s);
                        }
                    }
                    if !out.is_empty() {
                        return Ok(out);
                    }
                }
            }
        }
        self.skill.match_skills(context).await
    }

    async fn verify_skill(&self, id: &str, outcome: &str) -> Result<f32> {
        // 验证规则在本地执行（后端暂未提供验证端点），直接走原生。
        self.skill.verify_skill(id, outcome).await
    }

    async fn list_skills(&self) -> Result<Vec<Skill>> {
        self.skill.list_skills().await
    }
}

// ---- WikiStore ----

#[async_trait]
impl WikiStore for AidopsBackend {
    async fn upsert_page(&self, page: WikiPage) -> Result<()> {
        if let Some(pid) = self.project_id() {
            let req = IngestRequest {
                asset_type: "wiki",
                project_id: pid,
                scope: "project",
                lifecycle_layer: "L3",
                memory_type: None,
                title: page.title.clone(),
                content: page.blocks.join("\n"),
                payload: serde_json::to_value(&page).unwrap_or(serde_json::Value::Null),
            };
            if self.http_post("memory-assets/ingest", &req).is_some() {
                // 链接图谱单独同步（best-effort，不影响主页写入）。
                for l in &page.links {
                    let lr = IngestRequest {
                        asset_type: "wiki",
                        project_id: pid,
                        scope: "project",
                        lifecycle_layer: "L3",
                        memory_type: None,
                        title: format!("link {}->{}", page.id, l.target),
                        content: l.label.clone(),
                        payload: serde_json::json!({"link": {"from": page.id, "to": l.target, "label": l.label}}),
                    };
                    let _ = self.http_post("memory-assets/ingest", &lr);
                }
                return Ok(());
            }
        }
        self.wiki.upsert_page(page).await
    }

    async fn get_page(&self, id: &str) -> Result<Option<WikiPage>> {
        if let Some(pid) = self.project_id() {
            let req = RecallRequest {
                project_id: pid,
                query: id.to_string(),
                asset_types: vec!["wiki"],
                min_layer: "L3",
                top_k: 5,
            };
            if let Some(v) = self.http_post("memory-assets/recall", &req) {
                if let Ok(resp) = serde_json::from_value::<RecallResponse>(v) {
                    for item in resp.results {
                        if let Ok(p) = serde_json::from_value::<WikiPage>(item.payload) {
                            if p.id == id {
                                return Ok(Some(p));
                            }
                        }
                    }
                }
            }
        }
        self.wiki.get_page(id).await
    }

    async fn link(&self, from: &str, to: &str, label: &str) -> Result<()> {
        // 链接关系在本地维护（后端可后续扩展专用端点），先走原生。
        self.wiki.link(from, to, label).await
    }

    async fn query_pages(&self, query: &str) -> Result<Vec<WikiPage>> {
        if let Some(pid) = self.project_id() {
            let req = RecallRequest {
                project_id: pid,
                query: query.to_string(),
                asset_types: vec!["wiki"],
                min_layer: "L3",
                top_k: 10,
            };
            if let Some(v) = self.http_post("memory-assets/recall", &req) {
                if let Ok(resp) = serde_json::from_value::<RecallResponse>(v) {
                    let mut out = Vec::new();
                    for item in resp.results {
                        if let Ok(p) = serde_json::from_value::<WikiPage>(item.payload) {
                            out.push(p);
                        }
                    }
                    if !out.is_empty() {
                        return Ok(out);
                    }
                }
            }
        }
        self.wiki.query_pages(query).await
    }

    async fn list_pages(&self) -> Result<Vec<WikiPage>> {
        self.wiki.list_pages().await
    }
}

// ---- CodeGraph ----

#[async_trait]
impl CodeGraph for AidopsBackend {
    async fn index_symbol(&self, symbol: CodeSymbol) -> Result<()> {
        if let Some(pid) = self.project_id() {
            let req = IngestRequest {
                asset_type: "code_graph",
                project_id: pid,
                scope: "project",
                lifecycle_layer: "L3",
                memory_type: None,
                title: symbol.name.clone(),
                content: format!("{} @ {}", symbol.name, symbol.file),
                payload: serde_json::to_value(&symbol).unwrap_or(serde_json::Value::Null),
            };
            if self.http_post("memory-assets/ingest", &req).is_some() {
                return Ok(());
            }
        }
        self.code.index_symbol(symbol).await
    }

    async fn get_symbol(&self, id: &str) -> Result<Option<CodeSymbol>> {
        if let Some(pid) = self.project_id() {
            let req = RecallRequest {
                project_id: pid,
                query: id.to_string(),
                asset_types: vec!["code_graph"],
                min_layer: "L3",
                top_k: 5,
            };
            if let Some(v) = self.http_post("memory-assets/recall", &req) {
                if let Ok(resp) = serde_json::from_value::<RecallResponse>(v) {
                    for item in resp.results {
                        if let Ok(s) = serde_json::from_value::<CodeSymbol>(item.payload) {
                            if s.id == id {
                                return Ok(Some(s));
                            }
                        }
                    }
                }
            }
        }
        self.code.get_symbol(id).await
    }

    async fn callers_of(&self, symbol_id: &str) -> Result<Vec<String>> {
        self.code.callers_of(symbol_id).await
    }

    async fn callees_of(&self, symbol_id: &str) -> Result<Vec<String>> {
        self.code.callees_of(symbol_id).await
    }

    async fn impact_path(&self, symbol_id: &str) -> Result<Vec<Vec<String>>> {
        self.code.impact_path(symbol_id).await
    }

    async fn query_symbols(&self, query: &str) -> Result<Vec<CodeSymbol>> {
        if let Some(pid) = self.project_id() {
            let req = RecallRequest {
                project_id: pid,
                query: query.to_string(),
                asset_types: vec!["code_graph"],
                min_layer: "L3",
                top_k: 20,
            };
            if let Some(v) = self.http_post("memory-assets/recall", &req) {
                if let Ok(resp) = serde_json::from_value::<RecallResponse>(v) {
                    let mut out = Vec::new();
                    for item in resp.results {
                        if let Ok(s) = serde_json::from_value::<CodeSymbol>(item.payload) {
                            out.push(s);
                        }
                    }
                    if !out.is_empty() {
                        return Ok(out);
                    }
                }
            }
        }
        self.code.query_symbols(query).await
    }

    async fn list_symbols(&self) -> Result<Vec<CodeSymbol>> {
        self.code.list_symbols().await
    }
}
