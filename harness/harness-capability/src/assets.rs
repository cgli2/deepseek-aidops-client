//! 四类记忆资产 Definition（借鉴 TencentDB Agent Memory 的设计思想，落到 dsh 语义）。
//!
//! 与 `memory.rs` 的扁平 KV `Memory` 不同，这里定义了**结构化资产**能力，并引入
//! **L0~L3 成熟度轴**（TencentDB 思想）：
//!
//! - `ConversationMemory`：对话记忆（偏好/事实/决策/交互历史），按 L0→L3 沉淀；
//! - `SkillLibrary`：可执行经验（版本/触发边界/执行步骤/验证规则/资源文件）；
//! - `WikiStore`：结构化知识页面 + 链接图谱（Karpathy "LLM Wiki"）；
//! - `CodeGraph`：代码符号/文件/调用关系/影响路径。
//!
//! 沿用本 crate 的三角色约定（Definition / Provider / Consumer）：
//! 本文件只声明接口与类型（零实现），Provider 在 `harness-provider-*` 实现，
//! Consumer（`harness-tool` / `harness-runtime`）仅依赖本文件定义的 trait。
//! 换 Provider（原生文件实现 ↔ aidops 远程后端）时 Consumer 源码零改动。

use std::any::Any;

use async_trait::async_trait;
use harness_core::error::Result;
use serde::{Deserialize, Serialize};

/// 记忆成熟度轴（L0~L3，TencentDB 思想）。
///
/// 与 aidops 已废弃的"主题抽象轴" `layer`（`core/technical/strategic`）**正交**：
/// 本枚举描述一条记忆从"原始"到"沉淀为资产"的演化阶段，而非主题分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "UPPERCASE")]
pub enum LifecycleLayer {
    /// L0 工作记忆：当前会话原始上下文、最近若干 turns，进程内/超短期。
    L0,
    /// L1 情景记忆：单次会话提炼的 episode / 交互摘要。
    L1,
    /// L2 长期语义：合并去重后的事实、偏好、决策（对应 aidops `is_static`）。
    #[default]
    L2,
    /// L3 程序性资产：沉淀为 Skill / Wiki / CodeGraph 可执行/结构化资产。
    L3,
}

impl LifecycleLayer {
    pub fn as_str(&self) -> &'static str {
        match self {
            LifecycleLayer::L0 => "L0",
            LifecycleLayer::L1 => "L1",
            LifecycleLayer::L2 => "L2",
            LifecycleLayer::L3 => "L3",
        }
    }

    /// 解析 "L0".."L3" 字符串；非法值回落 L2。
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_uppercase().as_str() {
            "L0" => LifecycleLayer::L0,
            "L1" => LifecycleLayer::L1,
            "L3" => LifecycleLayer::L3,
            _ => LifecycleLayer::L2,
        }
    }

    /// 是否达到给定最小层（L0<L1<L2<L3）。
    pub fn at_least(&self, min: LifecycleLayer) -> bool {
        (*self as u8) >= (min as u8)
    }
}

/// 对话记忆里的"事实种类"（TencentDB Chat Memory 的偏好/事实/决策）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FactKind {
    #[default]
    Fact,
    Preference,
    Decision,
}

/// 一次原始交互片段（L0 工作记忆的写入单位）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTurn {
    pub session_id: String,
    /// "user" | "assistant" | "tool" | "system"
    pub role: String,
    pub content: String,
    /// RFC3339 时间戳；空串时由 Provider 补当前时间。
    pub ts: String,
}

/// 一条已沉淀的长期记忆（L2/L3）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryFact {
    pub id: String,
    pub kind: FactKind,
    pub content: String,
    #[serde(default)]
    pub layer: LifecycleLayer,
    #[serde(default = "default_confidence")]
    pub confidence: f32,
    #[serde(default)]
    pub source: String,
}

fn default_confidence() -> f32 {
    0.8
}

/// 一个可执行经验（Skill 资产）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub id: String,
    pub name: String,
    /// 版本号（语义化或递增）；用于版本链与覆盖升级。
    pub version: String,
    /// 触发边界：适用条件/上下文描述（用于 `match_skills` 匹配）。
    pub trigger_boundary: String,
    /// 执行步骤（有序）。
    #[serde(default)]
    pub steps: Vec<String>,
    /// 验证规则（执行后用于校验成果）。
    #[serde(default)]
    pub verification_rules: Vec<String>,
    /// 关联资源文件（相对路径或 URI）。
    #[serde(default)]
    pub resource_files: Vec<String>,
    #[serde(default = "default_confidence")]
    pub confidence: f32,
    /// 是否启用：禁用的技能不参与 match_skills 匹配（管理面板可切换）。
    /// 默认启用；旧数据无此字段时视为启用。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 导入来源（内置技能为空）。约定目录包存**相对技能库根的路径**
    ///（如 `包名/SKILL.md`，不写磁盘绝对路径，工作区搬迁后依然有效）；
    /// 旧式导入记录为绝对路径，消费端需双格式兼容。
    /// 用于删除时定位包子目录与存储位置可视化；旧数据无此字段时为空串。
    #[serde(default)]
    pub source_path: String,
}

fn default_true() -> bool {
    true
}

/// 一个知识页面（Wiki 资产的结构化块）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WikiPage {
    pub id: String,
    pub title: String,
    /// 结构化内容块（段落 / 代码 / 列表）。
    #[serde(default)]
    pub blocks: Vec<String>,
    /// 页面内链接（指向其它页面 id）。
    #[serde(default)]
    pub links: Vec<WikiLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WikiLink {
    pub target: String,
    pub label: String,
}

/// 一个代码符号（CodeGraph 资产）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeSymbol {
    pub id: String,
    pub name: String,
    /// 所在文件（相对路径）。
    pub file: String,
    /// 符号种类：function / method / class / struct / module ...
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub signature: String,
    #[serde(default)]
    pub summary: String,
    /// 该符号直接调用的其它符号 id。
    #[serde(default)]
    pub calls: Vec<String>,
}

/// 对话记忆能力（Chat Memory，L0~L3）。
///
/// `record_turn` 写 L0 原始片段；`consolidate` 把会话片段合并为 L2 事实；
/// `recall` 按查询 + 最小成熟度层检索长期记忆；`remember` 显式沉淀一条 L2 记忆。
#[async_trait]
pub trait ConversationMemory: Any + Send + Sync + 'static {
    async fn record_turn(&self, turn: ChatTurn) -> Result<()>;
    async fn consolidate(&self, session_id: &str) -> Result<Vec<MemoryFact>>;
    async fn recall(&self, query: &str, min_layer: LifecycleLayer) -> Result<Vec<MemoryFact>>;
    async fn remember(&self, fact: MemoryFact) -> Result<()>;
    /// 列出全部已沉淀事实（无查询过滤），供面板浏览。
    async fn list_facts(&self) -> Result<Vec<MemoryFact>>;
    /// 读取某会话最近的 `n` 条原始交互片段（L0 工作记忆回看）。
    async fn recent_turns(&self, session_id: &str, n: usize) -> Result<Vec<ChatTurn>>;
}

/// 可执行经验库能力（Skill）。
#[async_trait]
pub trait SkillLibrary: Any + Send + Sync + 'static {
    async fn register_skill(&self, skill: Skill) -> Result<()>;
    async fn get_skill(&self, id: &str) -> Result<Option<Skill>>;
    /// 按上下文匹配适用技能（依据 `trigger_boundary`）。
    async fn match_skills(&self, context: &str) -> Result<Vec<Skill>>;
    /// 执行后按验证规则对成果打分（0.0~1.0）。
    async fn verify_skill(&self, id: &str, outcome: &str) -> Result<f32>;
    /// 列出全部技能（无查询过滤），供面板浏览（含已禁用的）。
    async fn list_skills(&self) -> Result<Vec<Skill>>;
    /// 删除技能（移除本地资产文件）。返回是否真的删除了。
    async fn delete_skill(&self, id: &str) -> Result<bool>;
    /// 启用 / 禁用技能。禁用的技能不再被 `match_skills` 匹配。
    async fn set_skill_enabled(&self, id: &str, enabled: bool) -> Result<()>;
}

/// 知识页面库能力（Wiki，含链接图谱）。
#[async_trait]
pub trait WikiStore: Any + Send + Sync + 'static {
    async fn upsert_page(&self, page: WikiPage) -> Result<()>;
    async fn get_page(&self, id: &str) -> Result<Option<WikiPage>>;
    async fn link(&self, from: &str, to: &str, label: &str) -> Result<()>;
    async fn query_pages(&self, query: &str) -> Result<Vec<WikiPage>>;
    /// 列出全部知识页面（无查询过滤），供面板浏览。
    async fn list_pages(&self) -> Result<Vec<WikiPage>>;
}

/// 代码图谱能力（CodeGraph）。
#[async_trait]
pub trait CodeGraph: Any + Send + Sync + 'static {
    async fn index_symbol(&self, symbol: CodeSymbol) -> Result<()>;
    async fn get_symbol(&self, id: &str) -> Result<Option<CodeSymbol>>;
    /// 谁调用了 `symbol_id`（直接调用者）。
    async fn callers_of(&self, symbol_id: &str) -> Result<Vec<String>>;
    /// `symbol_id` 直接调用了谁。
    async fn callees_of(&self, symbol_id: &str) -> Result<Vec<String>>;
    /// 影响传播路径：从 `symbol_id` 出发，下游被影响符号的若干条路径。
    async fn impact_path(&self, symbol_id: &str) -> Result<Vec<Vec<String>>>;
    async fn query_symbols(&self, query: &str) -> Result<Vec<CodeSymbol>>;
    /// 列出全部代码符号（无查询过滤），供面板浏览。
    async fn list_symbols(&self) -> Result<Vec<CodeSymbol>>;
}
