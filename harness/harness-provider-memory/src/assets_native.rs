//! 四类记忆资产的**原生（离线）** Provider 实现。
//!
//! 不依赖任何远端服务：所有资产以文件落盘（JSON / JSONL），检索用**词法打分**
//! （查询词在条目文本中的命中比例）。这是 dsh 在不接入 aidops 后端时仍能正常工作的
//! 兜底实现（满足"可选插件、可独立运行"诉求）。
//!
//! 与 aidops 后端 Provider 实现**同一组 Definition trait**（`harness_capability::assets`），
//! 因此 `harness-tool` / `harness-runtime` 等 Consumer 源码零改动即可在"原生 ↔ aidops"间切换。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harness_capability::assets::{
    ChatTurn, CodeGraph, CodeSymbol, ConversationMemory, FactKind, LifecycleLayer, MemoryFact,
    Skill, SkillLibrary, WikiLink, WikiPage, WikiStore,
};
use harness_core::error::{Error, Result};

/// 把查询拆成小写词（按空白与常见标点）。
fn tokenize(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

/// 中文无空格：把连续 CJK 段按 2/3 字滑动窗口拆成 token，用于模糊匹配。
/// 例如「请帮我规划一下这个任务」→ 「请帮」「帮我」「我规」「规划」…
/// 这样触发边界里的「规划」等关键词能与查询长句命中。
fn cjk_ngrams(s: &str, n: usize) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_alphanumeric() {
            // 非 CJK（字母数字）整段保留，避免拆散英文词。
            let start = i;
            while i < chars.len() && chars[i].is_ascii_alphanumeric() {
                i += 1;
            }
            out.push(chars[start..i].iter().collect());
        } else if ('\u{4e00}'..='\u{9fff}').contains(&c) {
            // CJK 连续段：n-gram
            let start = i;
            while i < chars.len() && ('\u{4e00}'..='\u{9fff}').contains(&chars[i]) {
                i += 1;
            }
            let seg: Vec<char> = chars[start..i].to_vec();
            if seg.len() <= n {
                out.push(seg.iter().collect());
            } else {
                for w in seg.windows(n) {
                    out.push(w.iter().collect());
                }
            }
        } else {
            i += 1;
        }
    }
    out
}

/// 查询侧分词（每个查询只需一次）：空白/标点分词 + 中文 n-gram 双通道。
/// 同一查询要对 N 个条目打分时，避免在每个条目上重复 tokenize（热路径优化）。
fn query_tokens(query: &str) -> (Vec<String>, usize) {
    let mut q: Vec<String> = Vec::new();
    for t in tokenize(query) {
        // 若该 token 是中文长句，补充 2/3-gram；短词直接保留。
        if t.chars().all(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) && t.chars().count() > 1 {
            q.push(t.clone());
            q.extend(cjk_ngrams(&t, 2));
            q.extend(cjk_ngrams(&t, 3));
        } else {
            q.push(t);
        }
    }
    q.dedup();
    // 分母用「非 n-gram 的基础词 + 命中的 n-gram 权重」：让整句匹配到关键词时
    // 分数显著 >0，同时不因 n-gram 太多而稀释。
    let base_tokens = tokenize(query)
        .into_iter()
        .filter(|t| t.chars().count() >= 2)
        .count()
        .max(1);
    (q, base_tokens)
}

/// 用预分词的查询 token 对单个文本打分。
fn lex_score_with(q: &[String], base_tokens: usize, text: &str) -> f32 {
    if q.is_empty() {
        return 0.0;
    }
    let t = text.to_lowercase();
    // 中文 n-gram 长度≥2 才参与匹配，避免单个汉字误命中。
    let hits = q
        .iter()
        .filter(|tok| tok.chars().count() >= 2 && t.contains(tok.as_str()))
        .count();
    let score = hits as f32 / (base_tokens * 2) as f32;
    // 归一化到 0.2~1.0，保证命中即被采纳。
    if score > 0.0 {
        (score * 4.0 + 0.2).min(1.0)
    } else {
        0.0
    }
}

fn now_rfc3339() -> String {
    // 不引入 chrono：用系统时钟拼一个足够排序的时间串。
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{}", secs)
}

fn ensure_dir(p: &Path) -> Result<()> {
    std::fs::create_dir_all(p).map_err(Error::Io)
}

/// 计算资产根目录：`<cwd>/.harness-memory`。
fn asset_root(cwd: &Path) -> PathBuf {
    cwd.join(".harness-memory")
}

// ---------------------------------------------------------------------------
// ConversationMemory（Chat Memory，L0~L3）原生实现
// ---------------------------------------------------------------------------

pub struct NativeConversationMemory {
    root: PathBuf,
    /// 内存缓存事实列表，避免每次 recall 都读盘（与 FileMemory 的 index 思路一致）。
    facts: Mutex<Option<Vec<MemoryFact>>>,
}

impl NativeConversationMemory {
    pub fn new(cwd: impl AsRef<Path>) -> Arc<Self> {
        let root = asset_root(cwd.as_ref());
        let _ = ensure_dir(&root);
        Arc::new(Self {
            root,
            facts: Mutex::new(None),
        })
    }

    fn conv_path(&self, session_id: &str) -> PathBuf {
        self.root
            .join("conversations")
            .join(sanitize(session_id) + ".jsonl")
    }

    fn facts_path(&self) -> PathBuf {
        self.root.join("facts.json")
    }

    fn load_facts(&self) -> Vec<MemoryFact> {
        if let Some(cached) = self.facts.lock().unwrap().clone() {
            return cached;
        }
        let v = std::fs::read_to_string(self.facts_path())
            .ok()
            .and_then(|s| serde_json::from_str::<Vec<MemoryFact>>(&s).ok())
            .unwrap_or_default();
        *self.facts.lock().unwrap() = Some(v.clone());
        v
    }

    fn save_facts(&self, facts: &[MemoryFact]) -> Result<()> {
        let _ = ensure_dir(&self.root);
        let body = serde_json::to_string_pretty(facts).map_err(Error::Serde)?;
        std::fs::write(self.facts_path(), body).map_err(Error::Io)?;
        *self.facts.lock().unwrap() = Some(facts.to_vec());
        Ok(())
    }
}

#[async_trait]
impl ConversationMemory for NativeConversationMemory {
    async fn record_turn(&self, turn: ChatTurn) -> Result<()> {
        let dir = self.root.join("conversations");
        ensure_dir(&dir)?;
        let ts = if turn.ts.is_empty() {
            now_rfc3339()
        } else {
            turn.ts.clone()
        };
        let line = serde_json::to_string(&ChatTurn {
            ts,
            session_id: turn.session_id.clone(),
            ..turn
        })
        .map_err(Error::Serde)?;
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.conv_path(&turn.session_id))
            .map_err(Error::Io)?;
        f.write_all(line.as_bytes()).map_err(Error::Io)?;
        f.write_all(b"\n").map_err(Error::Io)?;
        Ok(())
    }

    async fn consolidate(&self, session_id: &str) -> Result<Vec<MemoryFact>> {
        let path = self.conv_path(session_id);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => return Ok(vec![]),
        };
        let mut facts = self.load_facts();
        let mut produced = Vec::new();
        for (i, line) in text.lines().enumerate() {
            let turn: ChatTurn = match serde_json::from_str(line) {
                Ok(t) => t,
                Err(_) => continue,
            };
            // 朴素启发式抽取（离线、无需 LLM）：长助手回复视为候选事实；
            // 含偏好/决定/decision/prefer 等标记的可疑种类升级。
            let kind = if turn.content.contains("决定")
                || turn.content.contains("decision")
                || turn.content.contains("我们采用")
            {
                FactKind::Decision
            } else if turn.content.contains("偏好")
                || turn.content.contains("prefer")
                || turn.content.contains("喜欢")
            {
                FactKind::Preference
            } else {
                FactKind::Fact
            };
            if turn.content.trim().len() < 24 {
                continue;
            }
            let id = format!("fact:{session_id}:{i}");
            let fact = MemoryFact {
                id,
                kind,
                content: turn.content.trim().to_string(),
                layer: LifecycleLayer::L2,
                confidence: 0.7,
                source: format!("session:{session_id}"),
            };
            facts.retain(|f| f.id != fact.id);
            facts.push(fact.clone());
            produced.push(fact);
        }
        self.save_facts(&facts)?;
        Ok(produced)
    }

    async fn recall(&self, query: &str, min_layer: LifecycleLayer) -> Result<Vec<MemoryFact>> {
        let (q, base) = query_tokens(query);
        let mut scored: Vec<(f32, MemoryFact)> = self
            .load_facts()
            .into_iter()
            .filter(|f| f.layer.at_least(min_layer))
            .map(|f| (lex_score_with(&q, base, &format!("{:?}", f)), f))
            .filter(|(s, _)| *s > 0.0)
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored.into_iter().map(|(_, f)| f).take(20).collect())
    }

    async fn remember(&self, fact: MemoryFact) -> Result<()> {
        let mut facts = self.load_facts();
        facts.retain(|f| f.id != fact.id);
        facts.push(fact);
        self.save_facts(&facts)
    }

    async fn list_facts(&self) -> Result<Vec<MemoryFact>> {
        Ok(self.load_facts())
    }

    async fn recent_turns(&self, session_id: &str, n: usize) -> Result<Vec<ChatTurn>> {
        let path = self.conv_path(session_id);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => return Ok(vec![]),
        };
        let mut turns: Vec<ChatTurn> = text
            .lines()
            .filter_map(|l| serde_json::from_str::<ChatTurn>(l).ok())
            .collect();
        if turns.len() > n {
            turns = turns.split_off(turns.len() - n);
        }
        Ok(turns)
    }
}

// ---------------------------------------------------------------------------
// SkillLibrary 原生实现
// ---------------------------------------------------------------------------

pub struct NativeSkillLibrary {
    root: PathBuf,
    /// 技能列表内存缓存：避免每回合 match_skills 都全目录扫描 + 逐文件反序列化。
    /// 写路径（register/write/delete/set_enabled）统一置 None 失效。
    cache: Mutex<Option<Vec<Skill>>>,
}

impl NativeSkillLibrary {
    pub fn new(cwd: impl AsRef<Path>) -> Arc<Self> {
        let root = asset_root(cwd.as_ref()).join("skills");
        let _ = ensure_dir(&root);
        Arc::new(Self {
            root,
            cache: Mutex::new(None),
        })
    }

    fn skill_path(&self, id: &str) -> PathBuf {
        self.root.join(sanitize(id) + ".json")
    }

    fn all_skills(&self) -> Vec<Skill> {
        if let Some(cached) = self.cache.lock().unwrap().clone() {
            return cached;
        }
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.root) {
            for e in entries.flatten() {
                if let Ok(s) = std::fs::read_to_string(e.path()) {
                    if let Ok(sk) = serde_json::from_str::<Skill>(&s) {
                        out.push(sk);
                    }
                }
            }
        }
        *self.cache.lock().unwrap() = Some(out.clone());
        out
    }

    /// 只返回已启用的技能（match_skills 用）。
    fn enabled_skills(&self) -> Vec<Skill> {
        self.all_skills()
            .into_iter()
            .filter(|s| s.enabled)
            .collect()
    }

    /// 写回某个技能的完整信息（启用状态变更时用）。
    fn write_skill(&self, skill: &Skill) -> harness_core::error::Result<()> {
        let body =
            serde_json::to_string_pretty(skill).map_err(harness_core::error::Error::Serde)?;
        std::fs::write(self.skill_path(&skill.id), body).map_err(harness_core::error::Error::Io)?;
        *self.cache.lock().unwrap() = None;
        Ok(())
    }
}

#[async_trait]
impl SkillLibrary for NativeSkillLibrary {
    async fn register_skill(&self, skill: Skill) -> Result<()> {
        ensure_dir(&self.root)?;
        let body = serde_json::to_string_pretty(&skill).map_err(Error::Serde)?;
        std::fs::write(self.skill_path(&skill.id), body).map_err(Error::Io)?;
        *self.cache.lock().unwrap() = None;
        Ok(())
    }

    async fn get_skill(&self, id: &str) -> Result<Option<Skill>> {
        Ok(std::fs::read_to_string(self.skill_path(id))
            .ok()
            .and_then(|s| serde_json::from_str::<Skill>(&s).ok()))
    }

    async fn match_skills(&self, context: &str) -> Result<Vec<Skill>> {
        // 查询分词只做一次，不随技能数量重复。
        let (q, base) = query_tokens(context);
        let mut scored: Vec<(f32, Skill)> = self
            .enabled_skills()
            .into_iter()
            .map(|sk| {
                (
                    lex_score_with(&q, base, &format!("{} {}", sk.trigger_boundary, sk.name)),
                    sk,
                )
            })
            .filter(|(s, _)| *s > 0.0)
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored.into_iter().map(|(_, sk)| sk).take(10).collect())
    }

    async fn verify_skill(&self, id: &str, outcome: &str) -> Result<f32> {
        let sk = self.get_skill(id).await?;
        match sk {
            Some(sk) if !sk.verification_rules.is_empty() => {
                let met = sk
                    .verification_rules
                    .iter()
                    .filter(|r| outcome.contains(r.as_str()))
                    .count();
                Ok(met as f32 / sk.verification_rules.len() as f32)
            }
            Some(_) => Ok(1.0),
            None => Err(Error::Runtime(format!("skill not found: {id}"))),
        }
    }

    async fn list_skills(&self) -> Result<Vec<Skill>> {
        Ok(self.all_skills())
    }

    async fn delete_skill(&self, id: &str) -> Result<bool> {
        // 先取记录：若技能是约定目录包（source_path 落在技能库目录内），
        // 连带回收其包子目录——否则启动自动扫描会把它再次注册回来。
        let record = self.get_skill(id).await?;
        let p = self.skill_path(id);
        let removed = match std::fs::remove_file(&p) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(harness_core::error::Error::Io(e)),
        };
        if let Some(sk) = record {
            if !sk.source_path.is_empty() {
                // 约定包的 source_path 是相对技能库根的路径（新），旧式导入是绝对路径：
                // 相对路径解析到库根，绝对路径原样使用。
                let src = std::path::Path::new(&sk.source_path);
                let src_abs = if src.is_absolute() {
                    src.to_path_buf()
                } else {
                    self.root.join(src)
                };
                if let Some(pack_dir) = src_abs.parent() {
                    // 只回收库目录内的包子目录；外部来源（旧式绝对路径导入）不动。
                    if pack_dir != self.root && pack_dir.starts_with(&self.root) {
                        let _ = std::fs::remove_dir_all(pack_dir);
                    }
                }
            }
        }
        *self.cache.lock().unwrap() = None;
        Ok(removed)
    }

    async fn set_skill_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        let mut skill = self
            .get_skill(id)
            .await?
            .ok_or_else(|| harness_core::error::Error::Runtime(format!("skill not found: {id}")))?;
        skill.enabled = enabled;
        self.write_skill(&skill)
    }
}

// ---------------------------------------------------------------------------
// WikiStore 原生实现
// ---------------------------------------------------------------------------

pub struct NativeWikiStore {
    root: PathBuf,
    /// 页面列表内存缓存：避免每次 query_pages/list_pages 全目录扫描；写时失效。
    cache: Mutex<Option<Vec<WikiPage>>>,
}

impl NativeWikiStore {
    pub fn new(cwd: impl AsRef<Path>) -> Arc<Self> {
        let root = asset_root(cwd.as_ref()).join("wiki");
        let _ = ensure_dir(&root);
        Arc::new(Self {
            root,
            cache: Mutex::new(None),
        })
    }

    fn page_path(&self, id: &str) -> PathBuf {
        self.root.join(sanitize(id) + ".json")
    }

    fn all_pages(&self) -> Vec<WikiPage> {
        if let Some(cached) = self.cache.lock().unwrap().clone() {
            return cached;
        }
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.root) {
            for e in entries.flatten() {
                if let Ok(s) = std::fs::read_to_string(e.path()) {
                    if let Ok(p) = serde_json::from_str::<WikiPage>(&s) {
                        out.push(p);
                    }
                }
            }
        }
        *self.cache.lock().unwrap() = Some(out.clone());
        out
    }
}

#[async_trait]
impl WikiStore for NativeWikiStore {
    async fn upsert_page(&self, page: WikiPage) -> Result<()> {
        ensure_dir(&self.root)?;
        let body = serde_json::to_string_pretty(&page).map_err(Error::Serde)?;
        std::fs::write(self.page_path(&page.id), body).map_err(Error::Io)?;
        *self.cache.lock().unwrap() = None;
        Ok(())
    }

    async fn get_page(&self, id: &str) -> Result<Option<WikiPage>> {
        Ok(std::fs::read_to_string(self.page_path(id))
            .ok()
            .and_then(|s| serde_json::from_str::<WikiPage>(&s).ok()))
    }

    async fn link(&self, from: &str, to: &str, label: &str) -> Result<()> {
        let page = self.get_page(from).await?;
        let mut page = match page {
            Some(p) => p,
            None => return Err(Error::Runtime(format!("wiki page not found: {from}"))),
        };
        if !page.links.iter().any(|l| l.target == to) {
            page.links.push(WikiLink {
                target: to.to_string(),
                label: label.to_string(),
            });
            self.upsert_page(page).await?;
        }
        Ok(())
    }

    async fn query_pages(&self, query: &str) -> Result<Vec<WikiPage>> {
        let (q, base) = query_tokens(query);
        let mut scored: Vec<(f32, WikiPage)> = self
            .all_pages()
            .into_iter()
            .map(|p| {
                let text = format!("{} {}", p.title, p.blocks.join(" "));
                (lex_score_with(&q, base, &text), p)
            })
            .filter(|(s, _)| *s > 0.0)
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored.into_iter().map(|(_, p)| p).take(10).collect())
    }

    async fn list_pages(&self) -> Result<Vec<WikiPage>> {
        Ok(self.all_pages())
    }
}

// ---------------------------------------------------------------------------
// CodeGraph 原生实现
// ---------------------------------------------------------------------------

#[derive(Clone, serde::Serialize, serde::Deserialize, Default)]
struct CodeGraphStore {
    symbols: Vec<CodeSymbol>,
}

pub struct NativeCodeGraph {
    root: PathBuf,
    cache: Mutex<Option<CodeGraphStore>>,
}

impl NativeCodeGraph {
    pub fn new(cwd: impl AsRef<Path>) -> Arc<Self> {
        let root = asset_root(cwd.as_ref());
        let _ = ensure_dir(&root);
        Arc::new(Self {
            root,
            cache: Mutex::new(None),
        })
    }

    fn store_path(&self) -> PathBuf {
        self.root.join("codegraph.json")
    }

    fn load(&self) -> CodeGraphStore {
        if let Some(c) = self.cache.lock().unwrap().clone() {
            return c;
        }
        let s = std::fs::read_to_string(self.store_path())
            .ok()
            .and_then(|t| serde_json::from_str::<CodeGraphStore>(&t).ok())
            .unwrap_or_default();
        *self.cache.lock().unwrap() = Some(s.clone());
        s
    }

    fn save(&self, s: &CodeGraphStore) -> Result<()> {
        let body = serde_json::to_string_pretty(s).map_err(Error::Serde)?;
        std::fs::write(self.store_path(), body).map_err(Error::Io)?;
        *self.cache.lock().unwrap() = Some(s.clone());
        Ok(())
    }

    /// 调用者反向索引：symbol_id -> 直接调用者集合。
    fn callers_map(&self, s: &CodeGraphStore) -> HashMap<String, Vec<String>> {
        let mut m: HashMap<String, Vec<String>> = HashMap::new();
        for sym in &s.symbols {
            for callee in &sym.calls {
                m.entry(callee.clone()).or_default().push(sym.id.clone());
            }
        }
        m
    }
}

#[async_trait]
impl CodeGraph for NativeCodeGraph {
    async fn index_symbol(&self, symbol: CodeSymbol) -> Result<()> {
        let mut s = self.load();
        s.symbols.retain(|x| x.id != symbol.id);
        s.symbols.push(symbol);
        self.save(&s)
    }

    async fn get_symbol(&self, id: &str) -> Result<Option<CodeSymbol>> {
        Ok(self.load().symbols.into_iter().find(|x| x.id == id))
    }

    async fn callers_of(&self, symbol_id: &str) -> Result<Vec<String>> {
        Ok(self
            .callers_map(&self.load())
            .remove(symbol_id)
            .unwrap_or_default())
    }

    async fn callees_of(&self, symbol_id: &str) -> Result<Vec<String>> {
        Ok(self
            .load()
            .symbols
            .into_iter()
            .find(|x| x.id == symbol_id)
            .map(|x| x.calls)
            .unwrap_or_default())
    }

    async fn impact_path(&self, symbol_id: &str) -> Result<Vec<Vec<String>>> {
        // 影响传播 = 沿"调用者"方向做有限深度 BFS，收集若干条到达路径。
        let store = self.load();
        let callers = self.callers_map(&store);
        let mut paths: Vec<Vec<String>> = Vec::new();
        let mut frontier: Vec<(String, Vec<String>)> =
            vec![(symbol_id.to_string(), vec![symbol_id.to_string()])];
        for _depth in 0..5 {
            let mut next = Vec::new();
            for (node, path) in frontier {
                if let Some(parents) = callers.get(&node) {
                    for p in parents {
                        let mut np = path.clone();
                        np.push(p.clone());
                        if np.len() > 6 {
                            continue;
                        }
                        paths.push(np.clone());
                        next.push((p.clone(), np));
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
            if paths.len() >= 20 {
                break;
            }
        }
        Ok(paths.into_iter().take(20).collect())
    }

    async fn query_symbols(&self, query: &str) -> Result<Vec<CodeSymbol>> {
        let (q, base) = query_tokens(query);
        let mut scored: Vec<(f32, CodeSymbol)> = self
            .load()
            .symbols
            .into_iter()
            .map(|x| {
                let text = format!("{} {} {}", x.name, x.summary, x.file);
                (lex_score_with(&q, base, &text), x)
            })
            .filter(|(s, _)| *s > 0.0)
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored.into_iter().map(|(_, x)| x).take(20).collect())
    }

    async fn list_symbols(&self) -> Result<Vec<CodeSymbol>> {
        Ok(self.load().symbols)
    }
}

/// 与 `FileMemory::sanitize` 同款：把任意 id 规整为合法文件名。
fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 对话记忆闭环：record_turn 落盘 → consolidate 产出事实 → list_facts 可见；
    /// recent_turns 按不带扩展名的会话 id 可读到轮次（守卫面板曾恒空的回归）。
    #[tokio::test]
    async fn conversation_memory_record_consolidate_recall_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "harness-mem-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let conv = NativeConversationMemory::new(&dir);
        let sid = "sess-1";
        conv.record_turn(ChatTurn {
            ts: String::new(),
            session_id: sid.into(),
            role: "assistant".into(),
            content: "这是一段足够长的助手回复，用于验证事实抽取链路能够正常工作运转。".into(),
        })
        .await
        .unwrap();

        // recent_turns：传入不带 `.jsonl` 后缀的会话 id 必须能读到轮次。
        let turns = conv.recent_turns(sid, 10).await.unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].role, "assistant");

        // consolidate：长回复（≥ 24 字符）应被抽为事实。
        let produced = conv.consolidate(sid).await.unwrap();
        assert!(!produced.is_empty());

        // list_facts：面板浏览入口可见已沉淀事实。
        let facts = conv.list_facts().await.unwrap();
        assert!(!facts.is_empty());

        // 幂等：重复合并不产生重复事实（按 id 去重）。
        let _ = conv.consolidate(sid).await.unwrap();
        assert_eq!(conv.list_facts().await.unwrap().len(), facts.len());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 约定目录包删除语义：delete_skill 既删 JSON 记录，也回收库内的包子目录
    ///（相对路径登记的新约定与绝对路径的旧式记录都支持），避免启动自动扫描
    /// 把已删技能再注册回来；外部来源不受影响。
    #[tokio::test]
    async fn delete_skill_removes_convention_pack_dir() {
        let dir = std::env::temp_dir().join(format!(
            "harness-skill-del-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let lib = NativeSkillLibrary::new(&dir);
        let root = asset_root(&dir).join("skills");

        // 库内包：source_path 以相对技能库根的路径登记（新约定）。
        let pack_dir = root.join("my-pack");
        std::fs::create_dir_all(&pack_dir).unwrap();
        std::fs::write(pack_dir.join("SKILL.md"), "# 包内技能\n").unwrap();
        let inside = Skill {
            id: "pack:my-pack".into(),
            name: "包内技能".into(),
            version: "1.0".into(),
            trigger_boundary: "测试用".into(),
            steps: Vec::new(),
            verification_rules: Vec::new(),
            resource_files: Vec::new(),
            confidence: 0.8,
            enabled: true,
            source_path: "my-pack/SKILL.md".into(),
        };
        lib.register_skill(inside).await.unwrap();

        // 外部来源：绝对路径（旧式记录），删除时不得触碰。
        let external_dir = dir.join("external-src");
        std::fs::create_dir_all(&external_dir).unwrap();
        std::fs::write(external_dir.join("SKILL.md"), "# 外部技能\n").unwrap();
        let external = Skill {
            id: "legacy-ext".into(),
            name: "外部技能".into(),
            version: "1.0".into(),
            trigger_boundary: "测试用".into(),
            steps: Vec::new(),
            verification_rules: Vec::new(),
            resource_files: Vec::new(),
            confidence: 0.8,
            enabled: true,
            source_path: external_dir.join("SKILL.md").to_string_lossy().to_string(),
        };
        lib.register_skill(external).await.unwrap();

        assert!(lib.delete_skill("pack:my-pack").await.unwrap());
        assert!(!pack_dir.exists(), "库内包子目录应被连带回收");

        assert!(lib.delete_skill("legacy-ext").await.unwrap());
        assert!(
            external_dir.join("SKILL.md").exists(),
            "外部来源文件不得被误删"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
