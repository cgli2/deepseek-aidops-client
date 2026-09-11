//! 统一 front matter（Phase 0）：为 `.harness-memory/` 下的 Markdown 资产
//! 提供可解析、可校验的元数据头。与第 4 章、第 12.2 节设计一一对应。
//!
//! 设计要点：
//! - 仅依赖 `serde` / `serde_yaml`，不引入新重型依赖；
//! - 与既有 `WikiPage` / `Skill` 解耦：本模块只产出 `FrontMatter` 值对象，
//!   由 `index.rs` 在投影时合并进目标结构；
//! - 宽容解析：缺字段给默认值，非法字段不致命（返回 `warnings`），
//!   避免一个坏文件阻塞整批索引（坑 4：显式回显，不静默降级）。

use serde::{Deserialize, Serialize};

/// 资产生命周期层级（与 `LifecycleLayer` 对齐，但独立定义避免循环依赖）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Layer {
    L0,
    L1,
    L2,
    L3,
}

impl Default for Layer {
    fn default() -> Self {
        Layer::L2
    }
}

/// 资产种类（与 `FactKind` / Wiki / Skill 并集对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Fact,
    Preference,
    Decision,
    Wiki,
    Skill,
    Journal,
    Session,
}

impl Default for Kind {
    fn default() -> Self {
        Kind::Wiki
    }
}

/// 作用域。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Session,
    Project,
    User,
    Shared,
}

impl Default for Scope {
    fn default() -> Self {
        Scope::Project
    }
}

/// 状态机。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Candidate,
    Active,
    Superseded,
    Archived,
}

impl Default for Status {
    fn default() -> Self {
        Status::Candidate
    }
}

/// 统一 front matter 值对象。所有字段带默认值，允许部分缺失。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrontMatter {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub kind: Kind,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub layer: Layer,
    #[serde(default)]
    pub status: Status,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default = "default_confidence")]
    pub confidence: f32,
    #[serde(default = "default_importance")]
    pub importance: f32,
    #[serde(default)]
    pub freshness_days: u32,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default)]
    pub source_refs: Vec<String>,
    #[serde(default)]
    pub supersedes: Vec<String>,
}

fn default_confidence() -> f32 {
    0.7
}
fn default_importance() -> f32 {
    0.5
}

impl Default for FrontMatter {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: Kind::Wiki,
            scope: Scope::Project,
            project_id: String::new(),
            layer: Layer::L2,
            status: Status::Candidate,
            created_at: String::new(),
            updated_at: String::new(),
            confidence: default_confidence(),
            importance: default_importance(),
            freshness_days: 0,
            tags: Vec::new(),
            entities: Vec::new(),
            source_refs: Vec::new(),
            supersedes: Vec::new(),
        }
    }
}

/// 解析结果：成功时给出 front matter 与正文；失败时给出警告与裸正文。
#[derive(Debug, Clone)]
pub struct ParsedMarkdown {
    pub front_matter: Option<FrontMatter>,
    /// 去掉 YAML 头后的 Markdown 正文。
    pub body: String,
    /// 非致命警告（例如 YAML 语法错误但已容错降级）。
    pub warnings: Vec<String>,
}

/// 从 Markdown 文本中解析 YAML front matter（`---` 包裹的头部）。
///
/// 规则：
/// - 文件以 `---\n` 开头才视为有 front matter；
/// - 结束符为单独一行 `---`；
/// - YAML 解析失败不致命：返回 `warnings`，`front_matter=None`，`body=原文`。
pub fn parse_markdown(text: &str) -> ParsedMarkdown {
    let mut warnings = Vec::new();
    if !text.starts_with("---\n") {
        return ParsedMarkdown {
            front_matter: None,
            body: text.to_string(),
            warnings,
        };
    }
    // 找结束分隔行
    let rest = &text[4..];
    let end = match rest.find("\n---") {
        Some(i) => i,
        None => {
            warnings.push("front matter 未闭合（缺少结束 ---），按无头处理".into());
            return ParsedMarkdown {
                front_matter: None,
                body: text.to_string(),
                warnings,
            };
        }
    };
    let yaml_str = &rest[..end];
    let body_start = 4 + end + "\n---".len();
    let body = if body_start < text.len() {
        text[body_start..]
            .trim_start_matches('\n')
            .to_string()
    } else {
        String::new()
    };
    match serde_yaml::from_str::<FrontMatter>(yaml_str) {
        Ok(fm) => ParsedMarkdown {
            front_matter: Some(fm),
            body,
            warnings,
        },
        Err(e) => {
            warnings.push(format!("front matter YAML 解析失败：{e}；按无头处理"));
            ParsedMarkdown {
                front_matter: None,
                body: text.to_string(),
                warnings,
            }
        }
    }
}

/// 把 front matter 与正文重新序列化为 Markdown（用于写回/规范化）。
pub fn render_markdown(fm: &FrontMatter, body: &str) -> String {
    let yaml = serde_yaml::to_string(fm).unwrap_or_default();
    format!("---\n{yaml}---\n\n{body}")
}

/// 生成 `.harness-memory/` 的目录模板（Phase 0 交付物）。
/// 返回 (相对路径, 文件内容) 列表；目录用 `.gitkeep` 占位。
pub fn harness_memory_template() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for dir in [
        "wiki",
        "skills/project",
        "skills/shared",
        "code",
        "review",
        "archive",
        "index",
        "conversations",
    ] {
        out.push((format!(".harness-memory/{dir}/.gitkeep"), String::new()));
    }
    // 一个最小 wiki 示例，含合法 front matter
    let sample_fm = FrontMatter {
        id: "wiki.sample-001".into(),
        kind: Kind::Wiki,
        scope: Scope::Project,
        status: Status::Active,
        confidence: 0.9,
        importance: 0.6,
        tags: vec!["sample".into()],
        ..Default::default()
    };
    out.push((
        ".harness-memory/wiki/sample.md".into(),
        render_markdown(
            &sample_fm,
            "# 示例 Wiki\n\n这是一个符合统一 front matter 规范的示例页面。\n",
        ),
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_with_front_matter() {
        let text = "---\nid: decision.memory-storage-001\nkind: decision\nscope: project\nlayer: L2\nstatus: active\nconfidence: 0.9\nimportance: 0.8\ntags: [memory, architecture]\n---\n\n# 标题\n正文\n";
        let p = parse_markdown(text);
        let fm = p.front_matter.expect("应有 front matter");
        assert_eq!(fm.id, "decision.memory-storage-001");
        assert_eq!(fm.kind, Kind::Decision);
        assert_eq!(fm.scope, Scope::Project);
        assert_eq!(fm.layer, Layer::L2);
        assert_eq!(fm.status, Status::Active);
        assert!((fm.confidence - 0.9).abs() < 1e-6);
        assert_eq!(fm.tags, vec!["memory", "architecture"]);
        assert!(p.body.contains("# 标题"));
        assert!(p.warnings.is_empty());
    }

    #[test]
    fn parse_without_front_matter() {
        let p = parse_markdown("# 普通 Markdown\n没有头。\n");
        assert!(p.front_matter.is_none());
        assert!(p.body.contains("普通 Markdown"));
    }

    #[test]
    fn parse_broken_yaml_is_tolerated() {
        let text = "---\n: : : not yaml\n---\n\nbody\n";
        let p = parse_markdown(text);
        assert!(p.front_matter.is_none());
        assert!(!p.warnings.is_empty(), "应产生警告而非静默");
    }

    #[test]
    fn template_contains_expected_dirs() {
        let t = harness_memory_template();
        let paths: Vec<&str> = t.iter().map(|(p, _)| p.as_str()).collect();
        assert!(paths.contains(&".harness-memory/wiki/.gitkeep"));
        assert!(paths.contains(&".harness-memory/index/.gitkeep"));
        assert!(paths.iter().any(|p| p.ends_with("wiki/sample.md")));
    }
}
