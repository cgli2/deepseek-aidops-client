//! 资产索引器：把工作区里的「静态资产」自动沉淀进四类记忆资产服务。
//!
//! 这是让 Skill / Wiki / CodeGraph 在 dsh 桌面**真正可见、可用**的关键一步——
//! 此前这些 Definition 能力虽已实现，但没有任何自动注入，面板打开是空的。本模块：
//!
//! - 扫描工作区下所有 `SKILL.md` → 注册为 [`SkillLibrary`] 中的可执行经验；
//! - 扫描 `docs/**` 与项目内 `*.md` → 结构化拆为 [`WikiStore`] 页面 + 链接图谱；
//! - 扫描源码（`*.rs`/`*.py`/`*.ts`/...）→ 抽取符号与调用关系，灌入 [`CodeGraph`]。
//!
//! 纯离线、幂等（按 id 覆盖），不依赖任何 LLM / 向量库。索引器只依赖 Definition trait，
//! 因此对「原生实现」与「aidops 后端实现」一视同仁。本模块属于 Definition 层，
//! 不耦合任何具体 Provider，Consumer（如桌面 UI）可直接调用。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::assets::{CodeGraph, CodeSymbol, Skill, SkillLibrary, WikiLink, WikiPage, WikiStore};
use harness_core::error::Result;

/// 一次索引的统计结果（用于面板反馈）。
#[derive(Debug, Clone, Default)]
pub struct IndexStats {
    pub skills: usize,
    pub pages: usize,
    pub symbols: usize,
}

/// 扫描时跳过的目录（体积大、噪声多，且与项目语义无关）。
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "vendor",
    "build",
    ".harness-memory",
    ".workbuddy",
    "__pycache__",
    ".venv",
    "venv",
];

/// 参与代码图谱抽取的文件扩展名。
const CODE_EXTS: &[&str] = &["rs", "py", "ts", "tsx", "js", "jsx", "go", "java", "c", "cpp", "h"];

/// 单次索引的全局上限，避免超大仓库卡死 UI。
const MAX_FILES: usize = 800;
const MAX_SKILLS: usize = 200;
const MAX_PAGES: usize = 400;
const MAX_SYMBOLS: usize = 6000;
const MAX_FILE_BYTES: usize = 300_000;

/// 把任意相对路径规整为合法 id。
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

/// 递归遍历 `root`，对每个**文件**调用 `f`（传入绝对路径与相对路径）。受 `MAX_FILES` 限制。
fn walk_files(root: &Path, f: &mut dyn FnMut(&Path, &str)) {
    let mut count = 0usize;
    visit(root, root, &mut count, f);
}

fn visit(abs: &Path, root: &Path, count: &mut usize, f: &mut dyn FnMut(&Path, &str)) {
    if *count >= MAX_FILES {
        return;
    }
    let entries = match std::fs::read_dir(abs) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        if *count >= MAX_FILES {
            return;
        }
        let path = entry.path();
        let ft = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if ft.is_dir() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if SKIP_DIRS.contains(&name) {
                    continue;
                }
            }
            visit(&path, root, count, f);
        } else if ft.is_file() {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let rel_s = rel.to_string_lossy().replace('\\', "/");
            *count += 1;
            f(&path, &rel_s);
        }
    }
}

/// 执行一次完整索引：扫描 `workspace` 并把静态资产灌入三个服务。
///
/// 幂等：重复调用按 id 覆盖，不产生重复。返回统计。
pub async fn bootstrap_assets(
    skill: &Arc<dyn SkillLibrary>,
    wiki: &Arc<dyn WikiStore>,
    code: &Arc<dyn CodeGraph>,
    workspace: &Path,
) -> Result<IndexStats> {
    let mut stats = IndexStats::default();

    // ── 1. 技能：所有 SKILL.md ───────────────────────────────
    let mut skills: Vec<(String, Skill)> = Vec::new();
    walk_files(workspace, &mut |path, rel| {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.eq_ignore_ascii_case("SKILL.md") && skills.len() < MAX_SKILLS {
            if let Ok(content) = std::fs::read_to_string(path) {
                skills.push((rel.to_string(), parse_skill(rel, &content)));
            }
        }
    });
    for (_, sk) in &skills {
        skill.register_skill(sk.clone()).await?;
    }
    stats.skills = skills.len();

    // ── 2. 知识库：所有 *.md ────────────────────────────────
    let mut pages: Vec<WikiPage> = Vec::new();
    walk_files(workspace, &mut |path, rel| {
        if rel.ends_with(".md")
            && !rel.eq_ignore_ascii_case("README.md")
            && pages.len() < MAX_PAGES
        {
            if let Ok(content) = std::fs::read_to_string(path) {
                pages.push(parse_wiki(rel, &content));
            }
        }
    });
    for p in &pages {
        wiki.upsert_page(p.clone()).await?;
    }
    // 链接图谱：跨页面链接关系（best-effort）。
    let page_ids: std::collections::HashSet<String> =
        pages.iter().map(|p| p.id.clone()).collect();
    for p in &pages {
        for l in &p.links {
            if page_ids.contains(&l.target) {
                let _ = wiki.link(&p.id, &l.target, &l.label).await;
            }
        }
    }
    stats.pages = pages.len();

    // ── 3. 代码图谱：源码符号与调用关系 ──────────────────────
    let mut symbols: Vec<CodeSymbol> = Vec::new();
    walk_files(workspace, &mut |path, rel| {
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if CODE_EXTS.contains(&ext) && symbols.len() < MAX_SYMBOLS {
                if let Ok(m) = std::fs::metadata(path) {
                    if m.len() as usize > MAX_FILE_BYTES {
                        return;
                    }
                }
                if let Ok(content) = std::fs::read_to_string(path) {
                    let mut syms = parse_code(rel, &content);
                    for s in syms.drain(..) {
                        if symbols.len() < MAX_SYMBOLS {
                            symbols.push(s);
                        } else {
                            break;
                        }
                    }
                }
            }
        }
    });
    for s in &symbols {
        code.index_symbol(s.clone()).await?;
    }
    stats.symbols = symbols.len();

    Ok(stats)
}

/// 从 `SKILL.md` 内容解析出一条 Skill。
fn parse_skill(rel: &str, content: &str) -> Skill {
    let name = content
        .lines()
        .find_map(|l| l.trim_start().strip_prefix("# "))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| rel.to_string());
    let mut trigger = String::new();
    let mut steps: Vec<String> = Vec::new();
    let mut verify: Vec<String> = Vec::new();
    let mut section: Option<&str> = None;
    for line in content.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("## ") {
            let head = rest.to_lowercase();
            section = if head.contains("触发") || head.contains("trigger") || head.contains("边界") {
                Some("trigger")
            } else if head.contains("步骤") || head.contains("steps") || head.contains("执行") {
                Some("steps")
            } else if head.contains("验证") || head.contains("verify") || head.contains("检验") {
                Some("verify")
            } else {
                None
            };
            continue;
        }
        if t.is_empty() {
            continue;
        }
        match section {
            Some("trigger") => {
                if !trigger.is_empty() {
                    trigger.push(' ');
                }
                trigger.push_str(t);
            }
            Some("steps") => steps.push(t.to_string()),
            Some("verify") => verify.push(t.to_string()),
            _ => {}
        }
    }
    if trigger.is_empty() {
        trigger = content
            .lines()
            .find(|l| !l.trim_start().starts_with('#') && !l.trim().is_empty())
            .unwrap_or("")
            .trim()
            .to_string();
    }
    Skill {
        id: sanitize(rel),
        name,
        version: "1.0".into(),
        trigger_boundary: trigger,
        steps,
        verification_rules: verify,
        resource_files: vec![],
        confidence: 0.6,
    }
}

/// 从 Markdown 内容解析出一个 WikiPage（标题 + 内容块 + 链接）。
fn parse_wiki(rel: &str, content: &str) -> WikiPage {
    let title = content
        .lines()
        .find_map(|l| l.trim_start().strip_prefix("# "))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| rel.trim_end_matches(".md").to_string());
    let mut blocks: Vec<String> = Vec::new();
    let mut links: Vec<WikiLink> = Vec::new();
    let mut para = String::new();
    for line in content.lines() {
        let t = line.trim();
        if t.is_empty() {
            if !para.is_empty() {
                blocks.push(para.trim().to_string());
                para.clear();
            }
            continue;
        }
        for (label, target) in extract_md_links(line) {
            let tgt_id = sanitize_target(&target);
            if !tgt_id.is_empty() && tgt_id != sanitize(rel) {
                links.push(WikiLink {
                    target: tgt_id,
                    label,
                });
            }
        }
        let clean = t
            .trim_start_matches('#')
            .trim_start()
            .trim_start_matches("- ")
            .trim_start_matches("* ")
            .trim_start_matches("> ");
        if !clean.is_empty() {
            para.push_str(clean);
            para.push(' ');
        }
    }
    if !para.is_empty() {
        blocks.push(para.trim().to_string());
    }
    WikiPage {
        id: sanitize(rel),
        title,
        blocks,
        links,
    }
}

/// 从一行里抽取所有 `[label](target)` 链接。
fn extract_md_links(line: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b']' {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b']' {
                let label = line[i + 1..j].trim().to_string();
                if j + 1 < bytes.len() && bytes[j + 1] == b'(' {
                    let mut k = j + 2;
                    while k < bytes.len() && bytes[k] != b')' {
                        k += 1;
                    }
                    if k < bytes.len() && bytes[k] == b')' {
                        let target = line[j + 2..k].trim().to_string();
                        out.push((label, target));
                        i = k + 1;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    out
}

/// 把链接 target 规整为页面 id（取文件名去扩展名）。
fn sanitize_target(target: &str) -> String {
    let path_part = target.split(['#', '?']).next().unwrap_or(target);
    let name = path_part.rsplit('/').next().unwrap_or(path_part).trim();
    let name = name.trim_end_matches(".md");
    sanitize(name)
}

/// 从源码文本抽取符号与调用关系（启发式、跨语言，足够支撑影响路径分析）。
fn parse_code(rel: &str, content: &str) -> Vec<CodeSymbol> {
    let mut def_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut symbols: Vec<CodeSymbol> = Vec::new();
    let mut calls_map: Vec<(String, std::collections::HashSet<String>)> = Vec::new();

    let mut current_fn: Option<(String, String)> = None; // (id, name)
    for line in content.lines() {
        let t = line.trim_start();
        if let Some(name) = def_name_of(t) {
            let kind = def_kind_of(t);
            let id = format!("{rel}:{name}");
            def_names.insert(name.to_string());
            symbols.push(CodeSymbol {
                id: id.clone(),
                name: name.to_string(),
                file: rel.to_string(),
                kind: kind.to_string(),
                signature: String::new(),
                summary: String::new(),
                calls: vec![],
            });
            calls_map.push((id.clone(), std::collections::HashSet::new()));
            current_fn = Some((id, name.to_string()));
        } else if let Some((fid, fname)) = &current_fn {
            for callee in call_targets(line) {
                if callee != *fname && def_names.contains(&callee) {
                    if let Some(entry) =
                        calls_map.iter_mut().find(|(id, _)| id.as_str() == fid.as_str())
                    {
                        entry.1.insert(callee);
                    }
                }
            }
        }
    }
    for sym in symbols.iter_mut() {
        if let Some((_, set)) = calls_map.iter().find(|(id, _)| id == &sym.id) {
            sym.calls = set.iter().cloned().collect();
        }
    }
    symbols
}

/// 若本行是定义行，返回定义名（fn/def/struct/class/enum/trait/interface/function）。
fn def_name_of(line: &str) -> Option<String> {
    let l = line.trim_start();
    if l.starts_with("//") || l.starts_with("*") || l.starts_with("#") {
        return None;
    }
    let take = |prefix: &str| {
        l.strip_prefix(prefix)
            .map(str::trim_start)
            .and_then(|rest| rest.split(|c: char| !c.is_alphanumeric() && c != '_').next())
            .filter(|n| !n.is_empty())
            .map(|n| n.to_string())
    };
    for kw in [
        "async fn ",
        "pub async fn ",
        "unsafe fn ",
        "pub fn ",
        "fn ",
        "struct ",
        "enum ",
        "trait ",
        "class ",
        "def ",
        "interface ",
        "function ",
    ] {
        if let Some(n) = take(kw) {
            return Some(n);
        }
    }
    None
}

/// 推断定义种类（用于面板筛选/着色）。
fn def_kind_of(line: &str) -> &'static str {
    let l = line.trim_start();
    if l.contains("fn ") || l.contains("def ") || l.starts_with("function ") {
        "function"
    } else if l.contains("struct ") {
        "struct"
    } else if l.contains("enum ") {
        "enum"
    } else if l.contains("trait ") {
        "trait"
    } else if l.contains("class ") {
        "class"
    } else if l.contains("interface ") {
        "interface"
    } else {
        "symbol"
    }
}

/// 从一行里提取所有形如 `ident(` 的调用目标名。
fn call_targets(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let ident: String = chars[start..i].iter().collect();
            if i < chars.len() && chars[i] == '(' {
                if !matches!(
                    ident.as_str(),
                    "if" | "for" | "while" | "match" | "return" | "when" | "switch" | "catch"
                        | "await" | "sizeof" | "typeof"
                ) {
                    out.push(ident);
                }
            }
        } else {
            i += 1;
        }
    }
    out
}

// 保留 `PathBuf` 引用，避免 lint 误报未用 import（扩展点）。
#[allow(dead_code)]
fn _unused(_p: PathBuf) {}
