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
use crate::inverted::InvertedIndex;
use harness_core::error::{Error, Result};

/// 一次索引的统计结果（用于面板反馈）。
#[derive(Debug, Clone, Default)]
pub struct IndexStats {
    pub skills: usize,
    pub pages: usize,
    pub symbols: usize,
}

/// 一次技能（包）批量导入/同步的统计结果（用于 GUI/CLI 反馈）。
#[derive(Debug, Clone, Default)]
pub struct SkillImportReport {
    /// 新增数量（此前不存在同 id 技能）。
    pub added: usize,
    /// 更新数量（同 id 已存在：覆盖内容但保留原启用状态）。
    pub updated: usize,
    /// 回收数量（约定目录中子目录已消失，对应记录被清理）。
    pub deleted: usize,
    /// 导入的技能名称（供提示展示）。
    pub names: Vec<String>,
}

/// 约定目录技能包的记录 id 前缀：该前缀的记录由 [`sync_skill_packs`] 全生命周期管理
/// （目录存在 ⇒ 注册/更新；目录消失 ⇒ 记录被回收）。
pub const SKILL_PACK_ID_PREFIX: &str = "pack:";

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
const CODE_EXTS: &[&str] = &[
    "rs", "py", "ts", "tsx", "js", "jsx", "go", "java", "c", "cpp", "h",
];

/// 单次索引的全局上限，避免超大仓库卡死 UI。
const MAX_FILES: usize = 800;
const MAX_SKILLS: usize = 200;
const MAX_PAGES: usize = 400;
const MAX_SYMBOLS: usize = 6000;
const MAX_FILE_BYTES: usize = 300_000;
/// 单个技能包内最多登记的资源文件数（避免把大目录整体拖进资产）。
const MAX_PACK_RESOURCES: usize = 32;

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
        if is_skill_md(path) && skills.len() < MAX_SKILLS {
            if let Ok(content) = std::fs::read_to_string(path) {
                skills.push((rel.to_string(), skill_from_markdown(rel, &content)));
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
        if rel.ends_with(".md") && !rel.eq_ignore_ascii_case("README.md") && pages.len() < MAX_PAGES
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
    let page_ids: std::collections::HashSet<String> = pages.iter().map(|p| p.id.clone()).collect();
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

/// 文件名是否为技能入口 `SKILL.md`（大小写不敏感）。
pub fn is_skill_md(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case("SKILL.md"))
}

/// 从 `SKILL.md` 内容解析出一条 Skill。
///
/// 除工作区索引外，GUI 的“导入技能”也使用同一解析器，保证用户导入的
/// `SKILL.md` 与自动扫描的资产拥有一致的触发、步骤和验证语义。
/// `rel` 同时决定技能 id（经 `sanitize` 规整）：导入场景传入来源绝对路径，
/// 即可获得「同一路径重复导入 = 同一 id = 更新而非副本」的幂等语义。
pub fn skill_from_markdown(rel: &str, content: &str) -> Skill {
    let name = content
        .lines()
        .find_map(|l| l.trim_start().strip_prefix("# "))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| rel.to_string());
    let version = parse_skill_version(content).unwrap_or_else(|| "1.0".to_string());
    let mut trigger = String::new();
    let mut steps: Vec<String> = Vec::new();
    let mut verify: Vec<String> = Vec::new();
    let mut section: Option<&str> = None;
    for line in content.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("## ") {
            let head = rest.to_lowercase();
            section = if head.contains("触发") || head.contains("trigger") || head.contains("边界")
            {
                Some("trigger")
            } else if head.contains("步骤") || head.contains("steps") || head.contains("执行") {
                Some("steps")
            } else if head.contains("验证") || head.contains("verify") || head.contains("检验")
            {
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
        version,
        trigger_boundary: trigger,
        steps,
        verification_rules: verify,
        resource_files: vec![],
        confidence: 0.6,
        enabled: true,
        source_path: String::new(),
    }
}

/// 解析 SKILL.md 头部块（第一个 `## ` 之前）里的版本声明。
///
/// 支持 `version: 1.2` / `- version：1.2` / `版本: 1.2` 等写法（中英文冒号均可），
/// 允许技能包自带版本说明；未声明时由调用方回退默认版本。
fn parse_skill_version(content: &str) -> Option<String> {
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("## ") {
            break; // 头部块结束
        }
        let body = t.trim_start_matches(['-', '*', '>']).trim();
        for key in ["version", "Version", "VERSION", "版本"] {
            if let Some(rest) = body.strip_prefix(key) {
                let rest = rest.trim();
                if let Some(v) = rest.strip_prefix(':').or_else(|| rest.strip_prefix('：')) {
                    let v = v.trim().trim_matches('`').trim().to_string();
                    if !v.is_empty() {
                        return Some(v);
                    }
                }
            }
        }
    }
    None
}

/// 在目录 `dir` 下（大小写不敏感）定位技能入口 `SKILL.md`。
fn find_skill_md(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for e in entries.flatten() {
        let p = e.path();
        if p.is_file() && is_skill_md(&p) {
            return Some(p);
        }
    }
    None
}

/// 把 `dir` 作为一个**技能包**解析为一条 Skill：
///
/// - 包内其余文件（含 `resources/` 子目录，限两层）登记为 `resource_files`（相对包根）；
/// - 版本号来自 SKILL.md 头部块的 `version:` 声明；
/// - 解析阶段 id / source_path 暂由 SKILL.md 绝对路径规整；走约定目录链路时
///   [`sync_skill_packs`] 会将二者覆写为目录名 id 与库根相对路径。
pub fn parse_skill_pack(dir: &Path) -> Option<Skill> {
    let skill_md = find_skill_md(dir)?;
    let content = std::fs::read_to_string(&skill_md).ok()?;
    let source = norm_path_str(&skill_md);
    let mut skill = skill_from_markdown(&source, &content);
    skill.source_path = source;
    skill.resource_files = collect_pack_resources(dir, &skill_md);
    Some(skill)
}

/// 路径转 `/` 分隔的稳定字符串（跨平台 id / 展示一致）。
fn norm_path_str(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// 收集技能包内的资源文件：包根及一层子目录（如 `resources/`）下的普通文件，
/// 排除 SKILL.md 与隐藏文件，跳过常规噪声目录，上限 `MAX_PACK_RESOURCES`。
fn collect_pack_resources(dir: &Path, skill_md: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut scan = |d: &Path, depth: usize| {
        if out.len() >= MAX_PACK_RESOURCES {
            return;
        }
        let entries = match std::fs::read_dir(d) {
            Ok(e) => e,
            Err(_) => return,
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        files.sort(); // 稳定顺序，导入结果可预期
        for p in files {
            if out.len() >= MAX_PACK_RESOURCES || p.as_path() == skill_md {
                continue;
            }
            if depth == 0 {
                let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
                // 同包内的其它 Markdown 视为技能文档的一部分（如 CHANGELOG），不作为资源。
                if name.starts_with('.') || stem.eq_ignore_ascii_case("CHANGELOG") {
                    continue;
                }
            }
            if let Ok(rel) = p.strip_prefix(dir) {
                out.push(norm_path_str(rel));
            }
        }
    };
    scan(dir, 0);
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut subdirs: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        subdirs.sort();
        for sub in subdirs {
            let name = sub.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') || SKIP_DIRS.contains(&name) {
                continue;
            }
            scan(&sub, 1);
        }
    }
    out
}

/// 递归扫描 `root`，返回所有含 `SKILL.md` 的技能包目录（含 `root` 自身）。
pub fn discover_skill_packs(root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    walk_files(root, &mut |path, _rel| {
        if is_skill_md(path) && out.len() < MAX_SKILLS {
            if let Some(parent) = path.parent() {
                if !out.contains(&parent.to_path_buf()) {
                    out.push(parent.to_path_buf());
                }
            }
        }
    });
    out
}

/// 批量注册技能到库（幂等）：同 id 覆盖更新而非创建副本，
/// 并**保留已有技能的启用状态**——重复导入不会把用户禁用的技能静默重新打开。
pub async fn import_skills(
    lib: &dyn SkillLibrary,
    skills: Vec<Skill>,
) -> Result<SkillImportReport> {
    let mut report = SkillImportReport::default();
    for mut sk in skills {
        match lib.get_skill(&sk.id).await? {
            Some(old) => {
                sk.enabled = old.enabled;
                report.updated += 1;
            }
            None => {
                report.added += 1;
            }
        }
        lib.register_skill(sk.clone()).await?;
        report.names.push(sk.name);
    }
    Ok(report)
}

/// 从文件夹批量导入技能包：递归扫描 `root` 下所有 `SKILL.md`，
/// 每个所在目录按技能包解析（含资源文件）后幂等注册进技能库。
///
/// 注意：新代码建议改走「约定目录 + 自动加载」链路（`install_skill_packs_from`
/// + `sync_skill_packs`）；本函数保留以兼容既有调用与测试。
pub async fn import_skill_dir(lib: &dyn SkillLibrary, root: &Path) -> Result<SkillImportReport> {
    let packs = discover_skill_packs(root);
    let skills: Vec<Skill> = packs.iter().filter_map(|p| parse_skill_pack(p)).collect();
    import_skills(lib, skills).await
}

/// 递归复制目录（同名文件覆盖）。
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ft.is_file() {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// 将任意来源目录下发现的技能包**落盘**到约定目录 `packs_root`
/// （复制：包目录 → `packs_root/<原目录名>`）。返回实际安置的包目录列表，
/// 随后应调用 [`sync_skill_packs`] 完成注册。
///
/// 幂等：目标已存在时先移除再整体复制（等同更新）；来源已在约定目录内的包
/// 不重复复制，直接视为已安置。
pub fn install_skill_packs_from(src_root: &Path, packs_root: &Path) -> Result<Vec<PathBuf>> {
    let packs = discover_skill_packs(src_root);
    let mut installed = Vec::new();
    for pack in packs {
        let Some(name) = pack.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let target = packs_root.join(name);
        if norm_path_str(&pack) == norm_path_str(&target) {
            // 来源就是目标位置（对约定目录自身重复导入）：无需复制。
            installed.push(target);
            continue;
        }
        if target.exists() {
            std::fs::remove_dir_all(&target).map_err(Error::Io)?;
        }
        copy_dir_recursive(&pack, &target).map_err(Error::Io)?;
        installed.push(target);
    }
    Ok(installed)
}

/// 将单个 `SKILL.md`（或普通 md）安置进约定目录：落为 `packs_root/<文件名>/SKILL.md`，
/// 使其获得与其它技能包一致的生命周期。同名目录直接覆盖文件（更新语义）。
/// 返回该包所在目录。
pub fn install_skill_file_into(file: &Path, packs_root: &Path) -> Result<PathBuf> {
    let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("skill");
    let name = sanitize(stem);
    let name = if name.is_empty() {
        "skill".to_string()
    } else {
        name
    };
    let target = packs_root.join(&name);
    if let Some(parent) = file.parent() {
        if norm_path_str(parent) == norm_path_str(&target) {
            return Ok(target); // 文件已在目标包内，无需复制。
        }
    }
    std::fs::create_dir_all(&target).map_err(Error::Io)?;
    std::fs::copy(file, target.join("SKILL.md")).map_err(Error::Io)?;
    Ok(target)
}

/// 「约定目录 + 自动加载」的注册对账入口：Agent 启动时与 GUI 导入后均调用它。
///
/// 语义（`packs_dir` 通常为 `<workspace>/.harness-memory/skills`）：
/// 1. 扫描 `packs_dir` 的直接子目录，每个含 `SKILL.md` 的子目录 = 一个技能包；
/// 2. **首次注册默认未启用**（`enabled=false`），须用户在面板勾选后才参与匹配；
/// 3. 已注册的包：更新内容但**保留用户设置的 enabled**（幂等更新，不产生副本）；
/// 4. 目录已不存在的 `pack:` 前缀记录被回收（目录是唯一事实来源）；
/// 5. `packs_dir` 本身不存在时不扫描也不回收（避免首次启动误删）。
pub async fn sync_skill_packs(
    lib: &dyn SkillLibrary,
    packs_dir: &Path,
) -> Result<SkillImportReport> {
    let mut report = SkillImportReport::default();
    if !packs_dir.is_dir() {
        return Ok(report);
    }
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let entries = std::fs::read_dir(packs_dir).map_err(Error::Io)?;
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with('.') || SKIP_DIRS.contains(&name) {
            continue;
        }
        let Some(mut sk) = parse_skill_pack(&dir) else {
            continue;
        };
        sk.id = format!("{SKILL_PACK_ID_PREFIX}{}", sanitize(name));
        // 约定包落盘时 source_path 存**相对技能库根的路径**（如 `包名/SKILL.md`），
        // 不把磁盘绝对路径写进 JSON：位置由「库根 + 相对路径」决定，工作区搬迁/
        // 拷贝后依然有效；旧绝对路径记录会在本次对账重写时自动迁移。
        if let Ok(rel) = Path::new(&sk.source_path).strip_prefix(packs_dir) {
            sk.source_path = norm_path_str(rel);
        }
        seen.insert(sk.id.clone());
        match lib.get_skill(&sk.id).await? {
            Some(old) => {
                sk.enabled = old.enabled;
                report.updated += 1;
            }
            None => {
                sk.enabled = false;
                report.added += 1;
            }
        }
        lib.register_skill(sk.clone()).await?;
        report.names.push(sk.name);
    }
    // 回收：约定目录管辖的记录对应子目录已消失（如用户在资源管理器里删了包）。
    for sk in lib.list_skills().await.unwrap_or_default() {
        if sk.id.starts_with(SKILL_PACK_ID_PREFIX)
            && !seen.contains(&sk.id)
            && lib.delete_skill(&sk.id).await.unwrap_or(false)
        {
            report.deleted += 1;
        }
    }
    Ok(report)
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
                call_edges: vec![],
            });
            calls_map.push((id.clone(), std::collections::HashSet::new()));
            current_fn = Some((id, name.to_string()));
        } else if let Some((fid, fname)) = &current_fn {
            for callee in call_targets(line) {
                if callee != *fname && def_names.contains(&callee) {
                    if let Some(entry) = calls_map
                        .iter_mut()
                        .find(|(id, _)| id.as_str() == fid.as_str())
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
            .and_then(|rest| {
                rest.split(|c: char| !c.is_alphanumeric() && c != '_')
                    .next()
            })
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
                    "if" | "for"
                        | "while"
                        | "match"
                        | "return"
                        | "when"
                        | "switch"
                        | "catch"
                        | "await"
                        | "sizeof"
                        | "typeof"
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 内存 Mock 技能库：验证导入/幂等逻辑，不依赖具体 Provider。
    #[derive(Default)]
    struct MemSkillLib(Mutex<Vec<Skill>>);

    #[async_trait::async_trait]
    impl SkillLibrary for MemSkillLib {
        async fn register_skill(&self, skill: Skill) -> Result<()> {
            let mut g = self.0.lock().unwrap();
            g.retain(|s| s.id != skill.id);
            g.push(skill);
            Ok(())
        }
        async fn get_skill(&self, id: &str) -> Result<Option<Skill>> {
            Ok(self.0.lock().unwrap().iter().find(|s| s.id == id).cloned())
        }
        async fn match_skills(&self, _context: &str) -> Result<Vec<Skill>> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .iter()
                .filter(|s| s.enabled)
                .cloned()
                .collect())
        }
        async fn verify_skill(&self, _id: &str, _outcome: &str) -> Result<f32> {
            Ok(1.0)
        }
        async fn list_skills(&self) -> Result<Vec<Skill>> {
            Ok(self.0.lock().unwrap().clone())
        }
        async fn delete_skill(&self, id: &str) -> Result<bool> {
            let mut g = self.0.lock().unwrap();
            let before = g.len();
            g.retain(|s| s.id != id);
            Ok(g.len() < before)
        }
        async fn set_skill_enabled(&self, id: &str, enabled: bool) -> Result<()> {
            let mut g = self.0.lock().unwrap();
            if let Some(s) = g.iter_mut().find(|s| s.id == id) {
                s.enabled = enabled;
            }
            Ok(())
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "harness-skill-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    fn sample_skill_md() -> &'static str {
        "# 发布检查清单\n\nversion: 1.2\n\n## 触发边界\n发布新版本、上线前需要检查清单\n\n## 执行步骤\n- 跑全量测试\n- 核对版本号与变更日志\n\n## 验证规则\n- 测试全部通过\n"
    }

    /// 技能包解析：版本号、触发边界、步骤、验证、资源文件与来源路径一次到位。
    #[test]
    fn parse_skill_pack_reads_version_resources_and_source() {
        let dir = temp_dir("pack");
        write(&dir.join("SKILL.md"), sample_skill_md());
        write(&dir.join("CHANGELOG.md"), "# 变更\n");
        write(&dir.join("resources").join("checklist.txt"), "ok");
        write(&dir.join("notes.txt"), "n");
        write(&dir.join(".hidden.txt"), "h");

        let sk = parse_skill_pack(&dir).expect("技能包应可解析");
        assert_eq!(sk.name, "发布检查清单");
        assert_eq!(sk.version, "1.2");
        assert!(sk.trigger_boundary.contains("发布新版本"));
        assert_eq!(sk.steps.len(), 2);
        assert_eq!(sk.verification_rules, vec!["- 测试全部通过".to_string()]);
        // 资源：CHANGELOG.md / 隐藏文件被排除，resources/ 子目录被纳入。
        assert!(sk.resource_files.iter().any(|f| f == "notes.txt"));
        assert!(
            sk.resource_files
                .iter()
                .any(|f| f == "resources/checklist.txt")
        );
        assert!(sk.resource_files.iter().all(|f| !f.contains("CHANGELOG")));
        assert!(sk.resource_files.iter().all(|f| !f.contains(".hidden")));
        // 来源路径：绝对路径（统一 / 分隔），幂等更新的判定依据。
        assert!(sk.source_path.ends_with("SKILL.md"));
        assert!(!sk.source_path.contains('\\'));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 递归发现：根目录自身与嵌套子目录的技能包都能找到，噪声目录被跳过。
    #[test]
    fn discover_skill_packs_finds_nested_and_skips_noise() {
        let root = temp_dir("discover");
        write(&root.join("SKILL.md"), sample_skill_md());
        write(
            &root.join("packs").join("sql-review").join("SKILL.md"),
            "# SQL 审查\n",
        );
        write(
            &root.join("node_modules").join("evil").join("SKILL.md"),
            "# 不应被发现\n",
        );

        let packs = discover_skill_packs(&root);
        assert_eq!(packs.len(), 2, "根目录与嵌套包各一个，node_modules 被跳过");
        assert!(packs.iter().any(|p| p == &root));
        assert!(packs.iter().any(|p| p.ends_with("sql-review")));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 幂等与状态保护：重复导入不产生副本，且不会把已禁用的技能重新打开。
    #[tokio::test]
    async fn import_skills_is_idempotent_and_preserves_enabled() {
        let dir = temp_dir("import");
        write(&dir.join("SKILL.md"), sample_skill_md());
        let lib = MemSkillLib::default();

        let rep1 = import_skill_dir(&lib, &dir).await.unwrap();
        assert_eq!(rep1.added, 1);
        assert_eq!(rep1.updated, 0);

        // 用户禁用后，内容更新再次导入：仍只有 1 条，且保持禁用。
        let id = lib.list_skills().await.unwrap()[0].id.clone();
        lib.set_skill_enabled(&id, false).await.unwrap();
        write(
            &dir.join("SKILL.md"),
            &format!("{}\n补充步骤\n", sample_skill_md()),
        );
        let rep2 = import_skill_dir(&lib, &dir).await.unwrap();
        assert_eq!(rep2.added, 0);
        assert_eq!(rep2.updated, 1);
        let all = lib.list_skills().await.unwrap();
        assert_eq!(all.len(), 1, "同一路径重复导入不得创建副本");
        assert!(!all[0].enabled, "重新导入不得覆盖用户的禁用状态");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 版本声明兼容：无 version 行时回退 1.0；中文冒号/列表前缀均可识别。
    #[test]
    fn parse_skill_version_variants() {
        assert_eq!(
            parse_skill_version("# t\nversion: 2.0\n").as_deref(),
            Some("2.0")
        );
        assert_eq!(
            parse_skill_version("# t\n- 版本：3.1\n").as_deref(),
            Some("3.1")
        );
        assert_eq!(parse_skill_version("# t\n## 触发\nversion: 9"), None);
        assert_eq!(parse_skill_version("# t\n正文"), None);
        assert_eq!(
            skill_from_markdown("x/SKILL.md", "# 无版本\n## 触发边界\n测试\n").version,
            "1.0"
        );
    }

    /// 落盘：外部包被复制进约定目录；重复落盘不产生副本（先删后拷 = 更新）。
    #[test]
    fn install_skill_packs_copies_into_convention_dir() {
        let src = temp_dir("install-src");
        let packs_root = temp_dir("install-root");
        write(
            &src.join("release-checklist").join("SKILL.md"),
            sample_skill_md(),
        );
        write(
            &src.join("release-checklist")
                .join("resources")
                .join("c.txt"),
            "ok",
        );
        write(&src.join("sql-review").join("SKILL.md"), "# SQL 审查\n");

        let installed = install_skill_packs_from(&src, &packs_root).unwrap();
        assert_eq!(installed.len(), 2);
        assert!(
            packs_root
                .join("release-checklist")
                .join("resources")
                .join("c.txt")
                .exists()
        );
        assert!(packs_root.join("sql-review").join("SKILL.md").exists());

        // 重复落盘：目标被整体替换，仍只有两个包目录。
        let again = install_skill_packs_from(&src, &packs_root).unwrap();
        assert_eq!(again.len(), 2);
        let dirs: Vec<_> = std::fs::read_dir(&packs_root)
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_dir())
            .collect();
        assert_eq!(dirs.len(), 2, "重复导入不得产生副本目录");

        // 单文件落盘：按文件名建包目录。
        let one = temp_dir("install-one");
        let md = one.join("my-note.md");
        write(&md, "# 随手记\n");
        let pack_dir = install_skill_file_into(&md, &packs_root).unwrap();
        assert!(pack_dir.join("SKILL.md").exists());
        assert_eq!(pack_dir.file_name().unwrap(), "my-note");

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&packs_root);
        let _ = std::fs::remove_dir_all(&one);
    }

    /// 自动加载全生命周期：首注册默认未启用 → 启用后重同步保留状态 →
    /// 目录被删后记录被回收。
    #[tokio::test]
    async fn sync_skill_packs_defaults_disabled_and_reconciles() {
        let packs_root = temp_dir("sync");
        write(
            &packs_root.join("release-checklist").join("SKILL.md"),
            sample_skill_md(),
        );
        write(
            &packs_root.join("sql-review").join("SKILL.md"),
            "# SQL 审查\n",
        );
        let lib = MemSkillLib::default();

        // 1) 首启自动扫描：注册成功但默认未启用。
        let rep = sync_skill_packs(&lib, &packs_root).await.unwrap();
        assert_eq!(rep.added, 2);
        assert_eq!(rep.updated, 0);
        let all = lib.list_skills().await.unwrap();
        assert!(
            all.iter().all(|s| !s.enabled),
            "自动加载的技能默认不得参与匹配"
        );
        assert!(lib.match_skills("发布新版本").await.unwrap().is_empty());
        // 约定包以相对技能库根的路径登记，不把磁盘绝对路径写进记录。
        let rc = lib
            .get_skill(&format!("{SKILL_PACK_ID_PREFIX}release-checklist"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rc.source_path, "release-checklist/SKILL.md");

        // 2) 用户启用 + 内容变更后重同步：不产生副本，启用状态保留。
        let id = format!("{SKILL_PACK_ID_PREFIX}release-checklist");
        lib.set_skill_enabled(&id, true).await.unwrap();
        write(
            &packs_root.join("release-checklist").join("SKILL.md"),
            &format!("{}\n补充步骤\n", sample_skill_md()),
        );
        let rep2 = sync_skill_packs(&lib, &packs_root).await.unwrap();
        assert_eq!(rep2.added, 0);
        assert_eq!(rep2.updated, 2);
        assert_eq!(lib.list_skills().await.unwrap().len(), 2);
        assert!(lib.get_skill(&id).await.unwrap().unwrap().enabled);

        // 3) 目录被删（用户在资源管理器里移除包）⇒ 记录被回收。
        std::fs::remove_dir_all(packs_root.join("sql-review")).unwrap();
        let rep3 = sync_skill_packs(&lib, &packs_root).await.unwrap();
        assert_eq!(rep3.deleted, 1);
        let ids: Vec<String> = lib
            .list_skills()
            .await
            .unwrap()
            .iter()
            .map(|s| s.id.clone())
            .collect();
        assert_eq!(ids, vec![id]);

        let _ = std::fs::remove_dir_all(&packs_root);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Phase 1：知识根索引 + checkpoint 增量（坑 5：checkpoint 增量索引）
// ═══════════════════════════════════════════════════════════════════════════

/// 知识根目录名（与 `.harness-memory/` 对齐）。
pub const KNOWLEDGE_ROOT: &str = ".harness-memory";

/// checkpoint 文件名（存放于知识根内，记录每个文件的 mtime+size）。
pub const CHECKPOINT_FILE: &str = ".index-checkpoint.json";

/// 单文件 checkpoint 条目：用 mtime+size 判断内容是否变更。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct FileCheckpoint {
    /// 相对知识根的路径（统一用 `/` 分隔）。
    pub rel: String,
    /// 上次索引时的修改时间（秒，自 epoch）。
    pub mtime_secs: u64,
    /// 上次索引时的文件大小（字节）。
    pub size: u64,
}

/// checkpoint 文件整体结构。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct KnowledgeCheckpoint {
    pub files: Vec<FileCheckpoint>,
}

impl KnowledgeCheckpoint {
    /// 从知识根读取 checkpoint；不存在或损坏时返回空 checkpoint（坑 4：显式降级）。
    pub fn load(knowledge_root: &Path) -> Self {
        let path = knowledge_root.join(CHECKPOINT_FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// 把当前 checkpoint 写回知识根。
    pub fn save(&self, knowledge_root: &Path) -> Result<()> {
        let path = knowledge_root.join(CHECKPOINT_FILE);
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, text)?;
        Ok(())
    }

    /// 判断文件是否需要重新索引（新文件或 mtime/size 变更）。
    pub fn needs_reindex(&self, rel: &str, mtime_secs: u64, size: u64) -> bool {
        match self.files.iter().find(|f| f.rel == rel) {
            None => true, // 新文件
            Some(f) => f.mtime_secs != mtime_secs || f.size != size,
        }
    }

    /// 更新单个文件的 checkpoint 条目。
    pub fn update(&mut self, rel: String, mtime_secs: u64, size: u64) {
        match self.files.iter_mut().find(|f| f.rel == rel) {
            Some(f) => {
                f.mtime_secs = mtime_secs;
                f.size = size;
            }
            None => self.files.push(FileCheckpoint {
                rel,
                mtime_secs,
                size,
            }),
        }
    }
}

/// 知识根索引统计。
#[derive(Debug, Clone, Default)]
pub struct KnowledgeIndexStats {
    /// 本次新索引或更新的页面数。
    pub upserted: usize,
    /// 因 checkpoint 命中而跳过的文件数。
    pub skipped: usize,
    /// 扫描到的总 Markdown 文件数。
    pub total: usize,
    /// 因源文件消失而从 checkpoint 回收的条目数（Phase 6：删除走 `remove(id)`）。
    pub removed: usize,
    /// 非致命警告（如 front matter 解析失败，坑 4：显式回显）。
    pub warnings: Vec<String>,
}

/// 扫描 `<workspace>/.harness-memory/**/*.md`，把带 front matter 的知识条目
/// 投影到 `WikiStore`。支持 checkpoint 增量：仅处理新文件或 mtime/size 变更的文件。
///
/// 与 `bootstrap_assets` 的关系：
/// - `bootstrap_assets` 扫描整个工作区，**跳过** `.harness-memory`（SKIP_DIRS）；
/// - 本函数专门扫描知识根，与 `bootstrap_assets` 互补，不重复投影。
///
/// 幂等：同一文件重复索引按 id 覆盖，不产生重复页面。
pub async fn index_knowledge_base(
    wiki: &Arc<dyn WikiStore>,
    workspace: &Path,
) -> Result<KnowledgeIndexStats> {
    walk_knowledge_root(wiki, workspace, |_| Ok(())).await
}

/// 知识根索引事件（Phase 6：写入路径增量维护内存倒排索引的钩子）。
pub enum IndexEvent<'a> {
    /// 一个知识条目被投影（新增或更新）；`text` 为可分词的检索文本。
    Upsert { page: &'a WikiPage, text: String },
    /// 源文件已消失，其投影条目需要被移除。
    Remove { id: String },
}

/// [`index_knowledge_base`] 的实现体：扫描知识根、投影 `WikiStore`、维护 checkpoint，
/// 并把每条变更以 [`IndexEvent`] 回调给 `sink`（默认实现忽略；倒排索引接线时消费）。
async fn walk_knowledge_root<F>(
    wiki: &Arc<dyn WikiStore>,
    workspace: &Path,
    mut sink: F,
) -> Result<KnowledgeIndexStats>
where
    F: FnMut(IndexEvent<'_>) -> Result<()>,
{
    let knowledge_root = workspace.join(KNOWLEDGE_ROOT);
    let mut stats = KnowledgeIndexStats::default();

    if !knowledge_root.is_dir() {
        return Ok(stats); // 知识根不存在，零成本退出
    }

    let mut checkpoint = KnowledgeCheckpoint::load(&knowledge_root);

    // 收集所有 Markdown 文件（递归，不受 MAX_FILES 限制，知识根本身体积可控）。
    let mut md_files: Vec<(PathBuf, String)> = Vec::new();
    collect_md_files(&knowledge_root, &knowledge_root, &mut md_files);
    stats.total = md_files.len();

    for (abs_path, rel) in &md_files {
        let meta = match std::fs::metadata(abs_path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mtime_secs = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let size = meta.len();

        if !checkpoint.needs_reindex(rel, mtime_secs, size) {
            stats.skipped += 1;
            continue;
        }

        let content = match std::fs::read_to_string(abs_path) {
            Ok(c) => c,
            Err(e) => {
                stats
                    .warnings
                    .push(format!("读取失败 {rel}: {e}"));
                continue;
            }
        };

        // 用 Phase 0 的 frontmatter 模块解析（坑 4：warnings 显式回显）。
        let parsed = crate::frontmatter::parse_markdown(&content);
        for w in &parsed.warnings {
            stats.warnings.push(format!("{rel}: {w}"));
        }

        // 构造 WikiPage：id 用相对路径规整，title 优先取 front matter，
        // 其次取正文第一个 `# ` 标题，最后退化为文件名。
        let page = knowledge_page_from(rel, &parsed);
        let text = page_text(&page);
        wiki.upsert_page(page.clone()).await?;
        sink(IndexEvent::Upsert { page: &page, text })?;

        checkpoint.update(rel.clone(), mtime_secs, size);
        stats.upserted += 1;
    }

    // Phase 6：源文件消失 ⇒ 回收 checkpoint 条目，并通知 sink 移除对应倒排项
    // （倒排索引本身不落盘，这里只保证内存派生结构与知识根一致）。
    let seen: std::collections::HashSet<&str> =
        md_files.iter().map(|(_, rel)| rel.as_str()).collect();
    let stale: Vec<String> = checkpoint
        .files
        .iter()
        .filter(|f| !seen.contains(f.rel.as_str()))
        .map(|f| page_id_from_rel(&f.rel))
        .collect();
    checkpoint.files.retain(|f| seen.contains(f.rel.as_str()));
    stats.removed = stale.len();
    for id in stale {
        sink(IndexEvent::Remove { id })?;
    }

    checkpoint.save(&knowledge_root)?;
    Ok(stats)
}

/// 知识根常驻索引：`WikiStore`（事实来源）+ 内存倒排索引（派生结构）。
///
/// 坑 3 的接线点：召回先用倒排索引取候选 id，再**只对候选集** `get_page` 精排，
/// 不再 `list_pages` 全量加载。倒排索引不落盘、不作为事实来源，进程重启或
/// 索引失效时重新 [`sync`](Self::sync) 即可由知识根完整重建。
pub struct KnowledgeIndex {
    wiki: Arc<dyn WikiStore>,
    inverted: InvertedIndex,
}

/// 单次召回最多精排的候选数（护栏：候选集异常膨胀时只精排头部）。
pub const RECALL_CANDIDATE_CAP: usize = 64;

impl KnowledgeIndex {
    /// 用默认倒排容量护栏构造常驻索引。
    pub fn new(wiki: Arc<dyn WikiStore>) -> Self {
        Self {
            wiki,
            inverted: InvertedIndex::new(),
        }
    }

    /// 指定倒排分片容量护栏（大规模知识根调参 / 审计用）。
    pub fn with_shard_cap(wiki: Arc<dyn WikiStore>, shard_cap: usize) -> Self {
        Self {
            wiki,
            inverted: InvertedIndex::with_shard_cap(shard_cap),
        }
    }

    /// 事实来源（面板浏览 / 链接图谱仍走 `WikiStore`）。
    pub fn wiki(&self) -> &Arc<dyn WikiStore> {
        &self.wiki
    }

    /// 内存倒排索引（只读视图，供统计与审计）。
    pub fn inverted(&self) -> &InvertedIndex {
        &self.inverted
    }

    /// 增量同步知识根：投影 `WikiStore` 的同时维护倒排索引。
    ///
    /// 写入路径：新增/更新条目 → `insert(id, text)`；源文件消失 → `remove(id)`。
    /// 幂等：同一文件重复同步按 id 覆盖，不产生重复倒排项。
    pub async fn sync(&mut self, workspace: &Path) -> Result<KnowledgeIndexStats> {
        let wiki = Arc::clone(&self.wiki);
        let inverted = &mut self.inverted;
        walk_knowledge_root(&wiki, workspace, |event| {
            match event {
                IndexEvent::Upsert { page, text } => inverted.insert(&page.id, &text),
                IndexEvent::Remove { id } => inverted.remove(&id),
            }
            Ok(())
        })
        .await
    }

    /// 候选 id 集（已按命中 term 数降序，最多 [`RECALL_CANDIDATE_CAP`] 条）。
    pub fn candidate_ids(&self, query: &str) -> Vec<String> {
        self.inverted()
            .candidates(query)
            .into_iter()
            .take(RECALL_CANDIDATE_CAP)
            .collect()
    }

    /// 召回：倒排索引取候选集 → 仅对候选集精排（不触发全量加载）。
    ///
    /// 空查询或无命中返回空集；返回顺序即候选命中度顺序。
    pub async fn recall(&self, query: &str, limit: usize) -> Result<Vec<WikiPage>> {
        let mut out: Vec<WikiPage> = Vec::new();
        if limit == 0 {
            return Ok(out);
        }
        for id in self.candidate_ids(query) {
            if let Some(page) = self.wiki.get_page(&id).await? {
                out.push(page);
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// 倒排索引规模统计：(已索引条目数, term 数, 超容量护栏的分片数)。
    pub fn inverted_stats(&self) -> (usize, usize, usize) {
        (
            self.inverted.doc_count(),
            self.inverted.term_count(),
            self.inverted.oversized_shards().len(),
        )
    }
}

/// 递归收集知识根下所有 `.md` 文件，返回 (绝对路径, 相对知识根路径)。
/// 排除知识根顶层 `review/`（Phase 2：未审核候选事实不进入默认上下文）。
fn collect_md_files(dir: &Path, root: &Path, out: &mut Vec<(PathBuf, String)>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let ft = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if ft.is_dir() {
            // Phase 2 验收：review/ 存未审核候选事实，不进入默认上下文（跳过投影；
            // 由审核链路晋升到正式目录后才被索引）。仅排除知识根顶层 review。
            let is_top_review = dir == root
                && path.file_name().and_then(|n| n.to_str()) == Some("review");
            if !is_top_review {
                collect_md_files(&path, root, out);
            }
        } else if ft.is_file() {
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((path.clone(), rel));
            }
        }
    }
}

/// 由知识根相对路径推导页面 id（投影与删除回收必须用同一规则）。
fn page_id_from_rel(rel: &str) -> String {
    sanitize(rel.trim_end_matches(".md"))
}

/// 知识页面的可检索文本（标题 + 正文块 + 链接标签），供倒排索引分词。
fn page_text(page: &WikiPage) -> String {
    let mut text = page.title.clone();
    for block in &page.blocks {
        text.push(' ');
        text.push_str(block);
    }
    for link in &page.links {
        text.push(' ');
        text.push_str(&link.label);
    }
    text
}

/// 从解析结果构造 WikiPage（知识根投影规则）。
fn knowledge_page_from(rel: &str, parsed: &crate::frontmatter::ParsedMarkdown) -> WikiPage {
    let id = page_id_from_rel(rel);
    let title = parsed
        .front_matter
        .as_ref()
        .and_then(|fm| {
            if fm.id.is_empty() {
                None
            } else {
                Some(fm.id.clone())
            }
        })
        .or_else(|| {
            parsed
                .body
                .lines()
                .find_map(|l| l.trim_start().strip_prefix("# "))
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|| id.clone());

    // 正文按空行分段为 blocks；链接抽取与普通 Wiki 一致。
    let mut blocks: Vec<String> = Vec::new();
    let mut links: Vec<WikiLink> = Vec::new();
    let mut para = String::new();
    for line in parsed.body.lines() {
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
            if !tgt_id.is_empty() && tgt_id != id {
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
        id,
        title,
        blocks,
        links,
    }
}

#[cfg(test)]
mod knowledge_tests {
    use super::*;
    use crate::assets::{WikiPage, WikiStore};
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// 内存 WikiStore，仅用于测试。
    struct MemWiki {
        pages: Mutex<HashMap<String, WikiPage>>,
    }

    impl MemWiki {
        fn new() -> Self {
            Self {
                pages: Mutex::new(HashMap::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl WikiStore for MemWiki {
        async fn upsert_page(&self, page: WikiPage) -> Result<()> {
            self.pages.lock().unwrap().insert(page.id.clone(), page);
            Ok(())
        }
        async fn get_page(&self, id: &str) -> Result<Option<WikiPage>> {
            Ok(self.pages.lock().unwrap().get(id).cloned())
        }
        async fn list_pages(&self) -> Result<Vec<WikiPage>> {
            Ok(self.pages.lock().unwrap().values().cloned().collect())
        }
        async fn link(&self, _from: &str, _to: &str, _label: &str) -> Result<()> {
            Ok(())
        }
        async fn query_pages(&self, query: &str) -> Result<Vec<WikiPage>> {
            let q = query.to_lowercase();
            Ok(self
                .pages
                .lock()
                .unwrap()
                .values()
                .filter(|p| {
                    p.title.to_lowercase().contains(&q)
                        || p.blocks.iter().any(|b| b.to_lowercase().contains(&q))
                })
                .cloned()
                .collect())
        }
    }

    fn make_workspace() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "harness_kb_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(dir.join(".harness-memory")).unwrap();
        dir
    }

    #[tokio::test]
    async fn first_index_upserts_all() {
        let ws = make_workspace();
        let kb = ws.join(".harness-memory");
        std::fs::write(
            kb.join("rust-tips.md"),
            "---\nid: rust-tips\nkind: wiki\n---\n# Rust Tips\n\nOwnership rules.\n",
        )
        .unwrap();
        std::fs::write(
            kb.join("async-notes.md"),
            "# Async Notes\n\nTokio runtime.\n",
        )
        .unwrap();

        let wiki: Arc<dyn WikiStore> = Arc::new(MemWiki::new());
        let stats = index_knowledge_base(&wiki, &ws).await.unwrap();

        assert_eq!(stats.total, 2);
        assert_eq!(stats.upserted, 2);
        assert_eq!(stats.skipped, 0);
        assert!(stats.warnings.is_empty());

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn second_index_skips_unchanged() {
        let ws = make_workspace();
        let kb = ws.join(".harness-memory");
        let file = kb.join("stable.md");
        std::fs::write(&file, "# Stable\n\nNo change.\n").unwrap();

        let wiki: Arc<dyn WikiStore> = Arc::new(MemWiki::new());

        // 第一次：全新索引
        let s1 = index_knowledge_base(&wiki, &ws).await.unwrap();
        assert_eq!(s1.upserted, 1);
        assert_eq!(s1.skipped, 0);

        // 第二次：无变更，应全部跳过
        let s2 = index_knowledge_base(&wiki, &ws).await.unwrap();
        assert_eq!(s2.upserted, 0);
        assert_eq!(s2.skipped, 1);

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn changed_file_is_reindexed() {
        let ws = make_workspace();
        let kb = ws.join(".harness-memory");
        let file = kb.join("evolving.md");
        std::fs::write(&file, "# V1\n\nFirst version.\n").unwrap();

        let wiki: Arc<dyn WikiStore> = Arc::new(MemWiki::new());
        let s1 = index_knowledge_base(&wiki, &ws).await.unwrap();
        assert_eq!(s1.upserted, 1);

        // 修改文件（size 变化，保证 checkpoint 检测到）
        std::fs::write(&file, "# V2\n\nSecond version with more content.\n").unwrap();
        let s2 = index_knowledge_base(&wiki, &ws).await.unwrap();
        assert_eq!(s2.upserted, 1);
        assert_eq!(s2.skipped, 0);

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn missing_knowledge_root_is_noop() {
        let ws = std::env::temp_dir().join("harness_kb_noop_test");
        let wiki: Arc<dyn WikiStore> = Arc::new(MemWiki::new());
        let stats = index_knowledge_base(&wiki, &ws).await.unwrap();
        assert_eq!(stats.total, 0);
        assert_eq!(stats.upserted, 0);
    }

    #[tokio::test]
    async fn broken_frontmatter_produces_warning_not_error() {
        let ws = make_workspace();
        let kb = ws.join(".harness-memory");
        std::fs::write(
            kb.join("broken.md"),
            "---\ninvalid: [unclosed\n---\n# Broken\n\nBody.\n",
        )
        .unwrap();

        let wiki: Arc<dyn WikiStore> = Arc::new(MemWiki::new());
        let stats = index_knowledge_base(&wiki, &ws).await.unwrap();

        // 坑 4：降级不致命，warning 显式回显
        assert_eq!(stats.upserted, 1);
        assert!(!stats.warnings.is_empty());

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn review_candidates_not_projected() {
        let ws = make_workspace();
        let kb = ws.join(".harness-memory");
        std::fs::create_dir_all(kb.join("review")).unwrap();
        std::fs::write(kb.join("wiki-note.md"), "# Wiki Note\n\nBody.\n").unwrap();
        std::fs::write(
            kb.join("review").join("fact-1.md"),
            "---\nid: fact-1\nkind: fact\nstatus: candidate\n---\n# Candidate\n\nUnreviewed content.\n",
        )
        .unwrap();

        let wiki: Arc<dyn WikiStore> = Arc::new(MemWiki::new());
        let stats = index_knowledge_base(&wiki, &ws).await.unwrap();

        // Phase 2 验收：未审核候选（review/）不进入默认上下文
        assert_eq!(stats.total, 1);
        assert_eq!(stats.upserted, 1);
        let pages = wiki.list_pages().await.unwrap();
        assert_eq!(pages.len(), 1);
        assert!(!pages[0]
            .blocks
            .iter()
            .any(|b| b.contains("Unreviewed content")));

        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── Phase 6：内存倒排索引接线（坑 3：召回不再全量加载） ──────────────

    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 统计 `get_page` / `list_pages` 调用次数的 WikiStore，
    /// 用于验证召回只精排候选集、从不全量加载。
    struct CountingWiki {
        pages: Mutex<HashMap<String, WikiPage>>,
        gets: AtomicUsize,
        lists: AtomicUsize,
    }

    impl CountingWiki {
        fn new() -> Self {
            Self {
                pages: Mutex::new(HashMap::new()),
                gets: AtomicUsize::new(0),
                lists: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl WikiStore for CountingWiki {
        async fn upsert_page(&self, page: WikiPage) -> Result<()> {
            self.pages.lock().unwrap().insert(page.id.clone(), page);
            Ok(())
        }
        async fn get_page(&self, id: &str) -> Result<Option<WikiPage>> {
            self.gets.fetch_add(1, Ordering::SeqCst);
            Ok(self.pages.lock().unwrap().get(id).cloned())
        }
        async fn list_pages(&self) -> Result<Vec<WikiPage>> {
            self.lists.fetch_add(1, Ordering::SeqCst);
            Ok(self.pages.lock().unwrap().values().cloned().collect())
        }
        async fn link(&self, _from: &str, _to: &str, _label: &str) -> Result<()> {
            Ok(())
        }
        async fn query_pages(&self, _query: &str) -> Result<Vec<WikiPage>> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn sync_maintains_inverted_index_incrementally() {
        let ws = make_workspace();
        let kb = ws.join(".harness-memory");
        std::fs::write(
            kb.join("tokio-notes.md"),
            "# Tokio Notes\n\nAsync runtime tuning.\n",
        )
        .unwrap();
        std::fs::write(
            kb.join("sql-notes.md"),
            "# Sql Notes\n\nQuery planner hints.\n",
        )
        .unwrap();

        let wiki = Arc::new(CountingWiki::new());
        let mut index = KnowledgeIndex::new(wiki.clone());

        let s1 = index.sync(&ws).await.unwrap();
        assert_eq!(s1.upserted, 2);
        assert_eq!(s1.removed, 0);
        let (docs, terms, oversized) = index.inverted_stats();
        assert_eq!(docs, 2);
        assert!(terms > 0);
        assert_eq!(oversized, 0);
        assert_eq!(index.candidate_ids("tokio"), vec!["tokio-notes".to_string()]);

        // 新增一个文件：只索引新文件，倒排索引增量长到 3 条
        std::fs::write(
            kb.join("grpc-notes.md"),
            "# Grpc Notes\n\nStreaming backpressure.\n",
        )
        .unwrap();
        let s2 = index.sync(&ws).await.unwrap();
        assert_eq!(s2.upserted, 1);
        assert_eq!(s2.skipped, 2);
        assert_eq!(index.inverted_stats().0, 3);
        assert_eq!(index.candidate_ids("grpc"), vec!["grpc-notes".to_string()]);

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn deleted_file_drops_postings_and_checkpoint() {
        let ws = make_workspace();
        let kb = ws.join(".harness-memory");
        let file = kb.join("runtime-notes.md");
        std::fs::write(&file, "# Runtime Notes\n\nScheduler internals.\n").unwrap();

        let wiki = Arc::new(CountingWiki::new());
        let mut index = KnowledgeIndex::new(wiki.clone());
        index.sync(&ws).await.unwrap();
        assert_eq!(index.inverted_stats().0, 1);
        assert_eq!(
            index.candidate_ids("scheduler"),
            vec!["runtime-notes".to_string()]
        );

        std::fs::remove_file(&file).unwrap();
        let stats = index.sync(&ws).await.unwrap();
        assert_eq!(stats.removed, 1);
        assert_eq!(stats.upserted, 0);
        // 倒排项被摘除：召回不再命中已删除条目
        assert_eq!(index.inverted_stats().0, 0);
        assert!(index.candidate_ids("scheduler").is_empty());
        assert!(index.recall("scheduler", 5).await.unwrap().is_empty());
        // checkpoint 同步回收，不会无限增长
        assert!(KnowledgeCheckpoint::load(&kb).files.is_empty());

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn recall_only_fetches_candidates() {
        let ws = make_workspace();
        let kb = ws.join(".harness-memory");
        std::fs::write(kb.join("a.md"), "# A\n\nTokio runtime tuning.\n").unwrap();
        std::fs::write(kb.join("b.md"), "# B\n\nQuery planner hints.\n").unwrap();
        std::fs::write(kb.join("c.md"), "# C\n\nUnrelated gardening notes.\n").unwrap();

        let wiki = Arc::new(CountingWiki::new());
        let mut index = KnowledgeIndex::new(wiki.clone());
        index.sync(&ws).await.unwrap();

        let pages = index.recall("planner", 10).await.unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].id, "b");
        // 坑 3 验收：只精排候选集（1 次 get_page），从不全量加载（0 次 list_pages）
        assert_eq!(wiki.gets.load(Ordering::SeqCst), 1);
        assert_eq!(wiki.lists.load(Ordering::SeqCst), 0);

        // 空查询零成本：不产生任何 get_page
        assert!(index.recall("", 10).await.unwrap().is_empty());
        assert_eq!(wiki.gets.load(Ordering::SeqCst), 1);

        let _ = std::fs::remove_dir_all(&ws);
    }
}
