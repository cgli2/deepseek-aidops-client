//! L2 工作区裁决层：用命中率代替词表，判断哪个候选片段是真锚点。
//!
//! 机制层只负责**提出候选，不做语义判断**；工作区才是裁判。它天然含有本项目的
//! 全部领域词汇，并随项目切换自动切换，因此不需要任何人工维护的词表。
//!
//! 裁决依据是**区分度**：一个片段在工作区中越罕见，它越能直接指向目标位置。
//! 这是 TF-IDF 的 IDF 思想用在目标定位上：
//!
//! | 命中情况 | 判定 | 理由 |
//! |---|---|---|
//! | 0 次 | 缺席 | 工作区里没有，不可能是本项目的定位锚点 |
//! | 极少 | 高价值锚点 | 区分度最高，直接指向目标位置 |
//! | 中等 | 次级锚点 | 可辅助收窄，不足以单独定位 |
//! | 近乎处处命中 | 停用词 | 无区分度，等价于噪声 |
//!
//! 由此得到三个不写一张表就自动成立的效果：
//!   - 停用词（"请"、"帮我"、"the"）因为缺乏区分度被自动淘汰；
//!   - 领域词（"订单"、"appCode"）因为**在这个项目里确实存在**而自动获得权重；
//!   - 跨语言性由机制天然获得——英文、中文、日文走同一条链路。
//!
//! 全部候选都缺席时，"工作区里没有用户说的东西"就是一个**有依据的判定**
//! （`zero_prior`），此时应当索取定位信息，而不是继续蛮力搜索。

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// 候选片段在工作区中的区分度分档。顺序即优先级：越小越有价值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AnchorGrade {
    /// 存在且稀有：区分度最高。
    HighValue = 0,
    /// 存在且有一定区分度。
    Secondary = 1,
    /// 几乎处处命中：无区分度，等价于停用词。
    Stopword = 2,
    /// 工作区里不存在。
    Absent = 3,
}

impl AnchorGrade {
    pub fn is_locatable(self) -> bool {
        matches!(self, Self::HighValue | Self::Secondary)
    }
}

/// 命中率高于此值视为无区分度。
const STOPWORD_RATIO: f64 = 0.6;
/// 命中率低于此值视为稀有。
const RARE_RATIO: f64 = 0.02;
/// 无论工作区多大，命中文件数不超过此值都算稀有——3 个文件已经足够直接去看。
const RARE_MAX_HITS: usize = 3;
/// 文件数少于此值时样本不足以做统计判别，此时"存在"本身就是信号。
const MIN_FILES_FOR_STATISTICS: usize = 5;

impl AnchorGrade {
    pub fn from_hits(hits: usize, total_files: usize) -> Self {
        if hits == 0 {
            return Self::Absent;
        }
        if total_files == 0 {
            // 冷启动：仓库尚未有任何文件，命中只可能来自内置默认画像种子，
            // 视为稀有高价值锚点，给首轮一个可提议的目标。
            return Self::HighValue;
        }
        if total_files < MIN_FILES_FOR_STATISTICS {
            return Self::HighValue;
        }
        let ratio = hits as f64 / total_files as f64;
        if ratio >= STOPWORD_RATIO {
            return Self::Stopword;
        }
        if hits <= RARE_MAX_HITS || ratio <= RARE_RATIO {
            return Self::HighValue;
        }
        Self::Secondary
    }
}

/// 一个候选片段的裁决结果。
#[derive(Debug, Clone)]
pub struct GradedCandidate {
    pub text: String,
    pub hits: usize,
    pub grade: AnchorGrade,
}

/// 一组候选的整体裁决。
pub struct Adjudication {
    /// 通过裁决的锚点，按区分度从高到低。
    pub anchors: Vec<String>,
    /// 全部候选在工作区里都不存在。这是"工作区里没有用户说的东西"的
    /// **有依据的判定**，调用方应据此索取定位信息，而不是继续蛮力搜索。
    pub all_absent: bool,
}

/// 索引时跳过的文件上限之外的单文件体积（超过则不读入内存）。
const MAX_FILE_BYTES: usize = 1 << 18;
/// 索引文件数上限。裁决只需要统计量，不需要全仓。
pub const DEFAULT_MAX_FILES: usize = 320;

pub(crate) struct IndexedFile {
    /// 相对仓库根的路径（用于结果回报）。
    pub(crate) relative: String,
    /// 压缩后的正文，用于跨分隔符与大小写不敏感的匹配。扫描阶段直接复用，
    /// 避免把每个文件从磁盘读第二遍。
    pub(crate) content: String,
    /// 压缩后的相对路径，用于目录名降级定位。
    path: String,
}

pub struct WorkspaceIndex {
    files: Vec<IndexedFile>,
    truncated: bool,
    /// 运行时学习到的命中统计：squash(term) -> 命中文件数。
    /// 持久化到 `.harness/learned.json`；命中即复用，未命中才现场扫描工作区。
    term_stats: RefCell<HashMap<String, usize>>,
    /// 本次是否从 learned.json 载入（而非现场扫描产生）。仅用于诊断/断言。
    loaded_from_cache: bool,
    /// 现场扫描被实际触发的次数（live_count 调用计数），验证"学习沉淀减少重算步数"用。
    scan_count: Cell<usize>,
}

impl WorkspaceIndex {
    pub fn build(root: &Path) -> Self {
        Self::build_with_limit(root, DEFAULT_MAX_FILES)
    }

    pub fn build_with_limit(root: &Path, max_files: usize) -> Self {
        let mut paths = Vec::new();
        let mut truncated = false;
        collect_source_files(root, &mut paths, max_files, &mut truncated);
        let files = paths
            .into_iter()
            .filter_map(|absolute| {
                let content = std::fs::read_to_string(&absolute).ok()?;
                if content.len() > MAX_FILE_BYTES {
                    return None;
                }
                let relative = absolute
                    .strip_prefix(root)
                    .unwrap_or(&absolute)
                    .display()
                    .to_string();
                Some(IndexedFile {
                    path: squash(&relative),
                    content: squash(&content),
                    relative,
                })
            })
            .collect();
        Self {
            files,
            truncated,
            term_stats: RefCell::new(HashMap::new()),
            loaded_from_cache: false,
            scan_count: Cell::new(0),
        }
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// false 表示达到了文件数上限；此时"缺席"不能作为工作区里没有的硬证据。
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// 已索引的文件列表（含相对路径与压缩正文），供扫描阶段复用，
    /// 避免把每个文件从磁盘读第二遍。
    pub(crate) fn files(&self) -> &[IndexedFile] {
        &self.files
    }

    /// 实际扫描工作区统计命中文件数（命中即计数，不限上限）。
    /// 这是"现场成本"的唯一来源；命中学习缓存时不会走到这里。
    fn live_count(&self, fragment: &str) -> usize {
        self.scan_count.set(self.scan_count.get() + 1);
        let needle = squash(fragment);
        let mut hits = 0;
        for file in &self.files {
            if file.content.contains(&needle) || file.path.contains(&needle) {
                hits += 1;
            }
        }
        hits
    }

    /// 命中文件数（学习缓存优先，未命中才现场扫描并回写缓存）。
    /// 缓存里存的是该候选的**完整**命中数，因此稀有/停用词分级不会因缓存失真。
    pub fn count(&self, fragment: &str) -> usize {
        let needle = squash(fragment);
        if needle.chars().count() < 2 {
            return 0;
        }
        if let Some(&cached) = self.term_stats.borrow().get(&needle) {
            return cached;
        }
        let hits = self.live_count(fragment);
        self.term_stats.borrow_mut().insert(needle, hits);
        hits
    }

    /// 命中文件数，最多封顶到 `cap` 后返回。裁决只关心是否超过稀有阈值，
    /// 封顶到 cap 即可；完整计数由 [`WorkspaceIndex::count`] 在学习缓存中维护。
    fn count_capped(&self, fragment: &str, cap: usize) -> usize {
        self.count(fragment).min(cap)
    }

    /// 裁决单个候选。先做 capped 计数：绝大多数候选要么缺席、要么稀有，
    /// 都能在低上限内得出结论，只有真正常见的才需要完整计数。
    pub fn grade(&self, fragment: &str) -> GradedCandidate {
        let total = self.files.len();
        let capped = self.count_capped(fragment, RARE_MAX_HITS + 1);
        if capped <= RARE_MAX_HITS {
            return GradedCandidate {
                text: fragment.to_string(),
                hits: capped,
                grade: AnchorGrade::from_hits(capped, total),
            };
        }
        let hits = self.count(fragment);
        GradedCandidate {
            text: fragment.to_string(),
            hits,
            grade: AnchorGrade::from_hits(hits, total),
        }
    }

    /// 批量裁决。利用"超集的命中数不可能超过子集"这一单调性剪枝：
    /// 任一 2 元窗口缺席 ⇒ 任何包含它的候选必然缺席，无需再扫一遍工作区。
    /// 这正是"用户说的东西工作区里没有"能被快速判定为 zero_prior 的原因。
    pub fn grade_all(&self, candidates: &[String]) -> Vec<GradedCandidate> {
        let mut prepared: Vec<(String, &String)> = candidates
            .iter()
            .map(|text| (squash(text), text))
            .filter(|(needle, _)| needle.chars().count() >= 2)
            .collect();
        prepared.sort_by(|a, b| {
            a.0.chars()
                .count()
                .cmp(&b.0.chars().count())
                .then(a.0.cmp(&b.0))
        });

        let mut absent_windows: HashSet<String> = HashSet::new();
        let mut out = Vec::with_capacity(prepared.len());
        for (needle, text) in prepared {
            let windows = two_char_windows(&needle);
            if windows.iter().any(|window| absent_windows.contains(window)) {
                out.push(GradedCandidate {
                    text: text.clone(),
                    hits: 0,
                    grade: AnchorGrade::Absent,
                });
                continue;
            }
            let graded = self.grade(text);
            if graded.grade == AnchorGrade::Absent && windows.len() == 1 {
                absent_windows.insert(needle);
            }
            out.push(graded);
        }
        out
    }

    /// 一次裁决同时给出两件事：通过裁决的锚点，以及"全部候选都缺席"这一
    /// 有依据的零先验判定。两者共用同一次扫描，避免为判定而重复读工作区。
    pub fn adjudicate(&self, candidates: &[String], limit: usize) -> Adjudication {
        let graded = self.grade_all(candidates);
        let all_absent = !candidates.is_empty()
            && graded
                .iter()
                .all(|item| item.grade == AnchorGrade::Absent);
        let mut keep: Vec<GradedCandidate> = graded
            .into_iter()
            .filter(|item| item.grade.is_locatable())
            .collect();
        // 命中越少越有价值；命中数相同时取更长的片段——它更具体，而更短的形式
        // 已经作为独立候选单独参选，不会因为取长而丢失。
        keep.sort_by(|a, b| {
            a.grade
                .cmp(&b.grade)
                .then(a.hits.cmp(&b.hits))
                .then(b.text.chars().count().cmp(&a.text.chars().count()))
                .then(a.text.cmp(&b.text))
        });
        let mut anchors: Vec<GradedCandidate> = Vec::new();
        for item in keep {
            if limit == 0 {
                break;
            }
            let subsumed = anchors
                .iter()
                .any(|kept| kept.hits == item.hits && kept.text.contains(&item.text));
            if !subsumed {
                anchors.push(item);
                if anchors.len() >= limit {
                    break;
                }
            }
        }
        Adjudication {
            anchors: anchors.into_iter().map(|item| item.text).collect(),
            all_absent,
        }
    }

    /// 挑选最终锚点：按区分度排序后去重包含关系。
    pub fn select_anchors(&self, candidates: &[String], limit: usize) -> Vec<String> {
        self.adjudicate(candidates, limit).anchors
    }

    /// 从一组候选里挑出工作区中最可能存在、且最具区分度的形式。
    /// 用于把用户说的"模型名称字段"解析成代码里实际存在的"模型名称"。
    pub fn best_variant(&self, candidates: &[String]) -> Option<String> {
        self.select_anchors(candidates, 1).into_iter().next()
    }

    /// 全部候选都缺席 —— 有依据的零先验判定，而非"没搜到"的兜底。
    pub fn all_absent(&self, candidates: &[String]) -> bool {
        !candidates.is_empty() && self.adjudicate(candidates, 0).all_absent
    }

    /// 路径降级：用压缩后的相对路径匹配，覆盖 appCode / app_code / app-code
    /// 等不同命名习惯，等价于大小写与分隔符无关的比较。
    pub fn match_paths(&self, fragments: &[String], limit: usize) -> Vec<String> {
        let needles: Vec<String> = fragments.iter().map(|text| squash(text)).collect();
        let needles: Vec<&str> = needles
            .iter()
            .filter(|needle| needle.chars().count() >= 3)
            .map(String::as_str)
            .collect();
        if needles.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for file in &self.files {
            if needles.iter().any(|needle| file.path.contains(needle)) {
                out.push(file.relative.clone());
                if out.len() >= limit {
                    break;
                }
            }
        }
        out
    }

    /// 载入已有学习沉淀，否则现场构建；并在仓库一个源码文件都没有时用内置默认画像作冷启动种子。
    ///
    /// 缓存只**加速**已见过的候选，不会替代工作区：无论是否命中缓存都仍扫描文件，
    /// 因此遇到全新表述也能正确裁决，只是要多扫一遍。删除 `.harness/learned.json`
    /// 后重跑，正确性不变、只是现场扫描次数上升。
    pub fn load_or_build(root: &Path) -> Self {
        let cache_path = learned_path(root);
        if let Some(stats) = load_learned(&cache_path) {
            let mut index = Self::build(root);
            index.term_stats = RefCell::new(stats);
            index.loaded_from_cache = true;
            index
        } else {
            let mut index = Self::build(root);
            if index.files.is_empty() {
                seed_cold_start(&mut index);
            }
            index
        }
    }

    /// 把学习沉淀写回 `.harness/learned.json`。
    ///
    /// 仅当本轮确实做过现场扫描、或本就来自缓存（可能补充了新词）时才写，
    /// 避免把"纯冷启动种子"当学习沉淀持久化污染后续运行。
    pub fn save(&self, root: &Path) {
        if self.scan_count.get() == 0 && !self.loaded_from_cache {
            return;
        }
        let dir = root.join(".harness");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("learned.json");
        let stats = self.term_stats.borrow();
        let mut term = serde_json::Map::new();
        for (key, value) in stats.iter() {
            term.insert(key.clone(), serde_json::Value::Number((*value).into()));
        }
        let payload = serde_json::json!({ "version": 1u32, "term_stats": term });
        if let Ok(text) = serde_json::to_string_pretty(&payload) {
            let _ = std::fs::write(&path, text);
        }
    }

    /// 是否来自 learned.json（而非现场扫描产生）。仅诊断用。
    pub fn loaded_from_cache(&self) -> bool {
        self.loaded_from_cache
    }

    /// 现场扫描被触发的次数。验证"学习沉淀减少重算"时对比用。
    pub fn scan_count(&self) -> usize {
        self.scan_count.get()
    }
}

fn learned_path(root: &Path) -> PathBuf {
    root.join(".harness").join("learned.json")
}

fn load_learned(path: &Path) -> Option<HashMap<String, usize>> {
    let content = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    let object = value.get("term_stats")?.as_object()?;
    let mut stats = HashMap::new();
    for (key, value) in object {
        if let Some(number) = value.as_u64() {
            stats.insert(key.clone(), number as usize);
        }
    }
    Some(stats)
}

/// 冷启动种子：仓库里一个源码文件都没有时，用内置默认画像（D5 迁出的中文词表）
/// 给最常见的领域标记一个"稀有"命中，让首轮至少有一个可提议的锚点。一旦真实文件
/// 出现，现场扫描会立即覆盖这些种子值——它们是脚手架，不是机制。
fn seed_cold_start(index: &mut WorkspaceIndex) {
    for marker in crate::builtin_profile::CJK_NOUN_MARKERS {
        let needle = squash(marker);
        if needle.chars().count() >= 2 {
            index.term_stats.get_mut().entry(needle).or_insert(1);
        }
    }
}

/// 压缩成"只保留字母数字的小写形式"。
///
/// 这一步同时解决三件跨语言/跨命名习惯的事：
///   - 大小写不敏感：`Model` 与 `model` 归一；
///   - 分隔符不敏感：`"Model Name"`、`model_name`、`modelName` 都归一成 `modelname`；
///   - CJK 不受影响：汉字本身是 alphanumeric，标点与空格被剔除。
pub(crate) fn squash(text: &str) -> String {
    text.chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

/// S4 静态收敛复核：以**磁盘上的产物**为准判断一个断言是否成立。
///
/// 索引在回合开始时构建，而写入发生在之后。所以"我改好了"绝不能拿陈旧索引内容
/// 自证——那正是虚假完成的入口（ADR §8 风险表）。这里重读单个文件，用与裁决
/// 同口径的 `squash` 匹配，保证"裁决说有"和"复核说有"不会互相打脸。
///
/// 返回 `None` 表示文件读不到（不存在/权限/非 UTF-8），调用方必须当作**未证明**
/// 处理，不得当作通过。
pub fn recheck_on_disk(root: &Path, relative: &str, needle: &str) -> Option<bool> {
    let needle = squash(needle);
    if needle.chars().count() < 2 {
        return None;
    }
    let content = fs::read_to_string(root.join(relative)).ok()?;
    Some(squash(&content).contains(&needle))
}

fn two_char_windows(needle: &str) -> Vec<String> {
    let chars: Vec<char> = needle.chars().collect();
    if chars.len() <= 2 {
        return vec![needle.to_string()];
    }
    chars
        .windows(2)
        .map(|window| window.iter().collect::<String>())
        .collect()
}

/// 收集用于索引的源码文件。与 `WorkspaceIndex` 放在一起，保证裁决与扫描
/// 看到的是同一批文件——否则"裁决说有、扫描说没有"会制造自相矛盾的证据。
pub fn collect_source_files(
    root: &Path,
    files: &mut Vec<PathBuf>,
    limit: usize,
    truncated: &mut bool,
) {
    if files.len() >= limit {
        *truncated = true;
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        *truncated = true;
        return;
    };
    let mut entries = entries.flatten().collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        if files.len() >= limit {
            *truncated = true;
            return;
        }
        let path = entry.path();
        if path.is_dir() {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if [
                ".git",
                "node_modules",
                "target",
                "dist",
                "build",
                ".harness",
            ]
            .contains(&name)
            {
                continue;
            }
            collect_source_files(&path, files, limit, truncated);
        } else if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(
                    extension,
                    "rs" | "ts"
                        | "tsx"
                        | "js"
                        | "jsx"
                        | "vue"
                        | "svelte"
                        | "py"
                        | "go"
                        | "java"
                        | "kt"
                        | "kts"
                        | "cs"
                        | "c"
                        | "h"
                        | "cpp"
                        | "hpp"
                        | "html"
                        | "css"
                        | "scss"
                        | "json"
                        | "yaml"
                        | "yml"
                        | "toml"
                )
            })
        {
            files.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("wsidx-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    /// 构造一个有"背景噪声"的工作区：多数文件不含目标词，便于检验区分度。
    fn noisy_workspace(tag: &str) -> PathBuf {
        let root = temp_root(tag);
        std::fs::create_dir_all(root.join("src/pages")).unwrap();
        for i in 0..12 {
            std::fs::write(
                root.join(format!("src/pages/noise{i}.tsx")),
                format!("export const n{i} = {{ label: 'filler{i}', value: {i} }};"),
            )
            .unwrap();
        }
        std::fs::write(
            root.join("src/pages/model.tsx"),
            "<label>模型名称</label><input name=\"apiKey\" /><input name=\"modelName\" />",
        )
        .unwrap();
        root
    }

    #[test]
    fn grading_maps_hit_rates_to_discrimination() {
        assert_eq!(AnchorGrade::from_hits(0, 100), AnchorGrade::Absent);
        assert_eq!(AnchorGrade::from_hits(2, 100), AnchorGrade::HighValue);
        assert_eq!(AnchorGrade::from_hits(20, 100), AnchorGrade::Secondary);
        assert_eq!(AnchorGrade::from_hits(95, 100), AnchorGrade::Stopword);
    }

    #[test]
    fn tiny_workspaces_treat_presence_as_signal() {
        // 3 个文件做不了统计判别，此时"存在"本身就是信号，不能因为
        // 命中率 100% 就把它当停用词丢掉。
        assert_eq!(AnchorGrade::from_hits(3, 3), AnchorGrade::HighValue);
    }

    #[test]
    fn chinese_anchor_is_picked_without_any_word_list() {
        let root = noisy_workspace("cjk");
        let index = WorkspaceIndex::build(&root);
        let candidates = vec![
            "模型名称字段".to_string(),
            "模型名称".to_string(),
            "字段".to_string(),
        ];
        let anchors = index.select_anchors(&candidates, 8);
        assert_eq!(
            anchors.first().map(String::as_str),
            Some("模型名称"),
            "实际存在于代码里的形式应胜出，实际 {anchors:?}"
        );
        assert!(
            !anchors.iter().any(|a| a == "模型名称字段"),
            "代码里不存在的整串必须被淘汰，实际 {anchors:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn english_phrase_matches_camel_case_without_a_profile() {
        let root = noisy_workspace("en");
        let index = WorkspaceIndex::build(&root);
        // "Model Name" 在源码里写作 modelName——压缩匹配把分隔符差异抹平，
        // 不需要任何语言或领域画像。
        let candidates = vec!["Model Name".to_string(), "Model".to_string()];
        let anchors = index.select_anchors(&candidates, 8);
        assert_eq!(
            anchors.first().map(String::as_str),
            Some("Model Name"),
            "实际 {anchors:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stopwords_are_eliminated_by_hit_rate_alone() {
        let root = noisy_workspace("stop");
        let index = WorkspaceIndex::build(&root);
        // 每个噪声文件都含 "export"，命中率 100% → 无区分度 → 自动淘汰。
        // 这里没有任何停用词表参与判断。
        let graded = index.grade(&"export".to_string());
        assert_eq!(graded.grade, AnchorGrade::Stopword);
        assert!(
            !index
                .select_anchors(&["export".to_string(), "apiKey".to_string()], 8)
                .contains(&"export".to_string())
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn all_absent_is_a_grounded_zero_prior_verdict() {
        let root = temp_root("absent");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/hello.ts"), "const x = 1;").unwrap();
        let index = WorkspaceIndex::build(&root);
        let candidates: Vec<String> = vec!["订单编号".into(), "创建时间".into(), "订单".into()];
        assert!(index.all_absent(&candidates));
        assert!(index.select_anchors(&candidates, 8).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn directory_names_count_as_hits() {
        let root = temp_root("dir");
        std::fs::create_dir_all(root.join("src/features/appCode")).unwrap();
        std::fs::write(root.join("src/features/appCode/editor.tsx"), "const x = 1;").unwrap();
        let index = WorkspaceIndex::build(&root);
        // 目录名能说明"这个功能在哪"，即使正文里没有该符号。
        assert!(index.count("appCode") > 0);
        let matched = index.match_paths(&["appCode".to_string()], 8);
        assert_eq!(matched.len(), 1, "实际 {matched:?}");
        assert!(matched[0].contains("appCode"), "实际 {matched:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn subset_monotonicity_prunes_impossible_candidates() {
        let root = temp_root("prune");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.ts"), "const x = 1;").unwrap();
        let index = WorkspaceIndex::build(&root);
        // "模型名称字段" 的任一 2 元窗口都不存在，因此它必然缺席。
        let graded = index.grade_all(&["模型名称字段".to_string()]);
        assert_eq!(graded.len(), 1);
        assert_eq!(graded[0].grade, AnchorGrade::Absent);
        assert_eq!(graded[0].hits, 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 性能实测（非回归用例，用 `--ignored` 手动跑）：裁决真实大仓库的开销。
    /// 这一步在澄清门禁之前执行，因此必须足够快，否则会拖慢每一次任务。
    #[test]
    #[ignore = "perf measurement, run with --ignored"]
    fn adjudication_cost_on_a_real_repository() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
            .expect("workspace root");
        let started = std::time::Instant::now();
        let index = WorkspaceIndex::build(&root);
        let build = started.elapsed();
        let started = std::time::Instant::now();
        let candidates = crate::target_extract::segment_candidates(
            "界面优化，系统管理->模型管理，把模型名称字段，API KEY字段位置顺序互换一下。",
        );
        let adjudication = index.adjudicate(&candidates, 8);
        let grade = started.elapsed();
        println!(
            "\n[perf] files={} candidates={} build={:?} adjudicate={:?} anchors={:?} all_absent={}",
            index.file_count(),
            candidates.len(),
            build,
            grade,
            adjudication.anchors,
            adjudication.all_absent
        );
    }

    #[test]
    fn longer_wins_when_hit_counts_tie() {
        let root = temp_root("tie");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/form.tsx"), "<label>模型名称</label>").unwrap();
        let index = WorkspaceIndex::build(&root);
        let candidates: Vec<String> = vec!["模型".into(), "模型名".into(), "模型名称".into()];
        // 三者命中数相同（都在同一个文件里），取最长的——它最具体。
        assert_eq!(
            index.best_variant(&candidates),
            Some("模型名称".to_string())
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// S2 验收（一）：学习沉淀加速重复裁决。
    /// 首轮现场扫描产生命中统计；第二轮命中缓存后现场扫描应为 0 且裁决一致；
    /// 删除 learned.json 后重跑正确性不变、但需再次现场扫描（步数上升）。
    #[test]
    fn learned_index_accelerates_repeat_adjudication() {
        let root = temp_root("learn");
        std::fs::create_dir_all(root.join("src/pages")).unwrap();
        for i in 0..12 {
            std::fs::write(
                root.join(format!("src/pages/noise{i}.tsx")),
                format!("export const n{i} = {{ label: 'filler{i}', value: {i} }};"),
            )
            .unwrap();
        }
        std::fs::write(
            root.join("src/pages/model.tsx"),
            "<label>模型名称</label><input name=\"apiKey\" /><input name=\"modelName\" />",
        )
        .unwrap();

        let candidates = vec![
            "模型名称字段".to_string(),
            "模型名称".to_string(),
            "字段".to_string(),
            "apiKey".to_string(),
        ];

        // 第一轮：现场扫描，产生学习沉淀。
        let first = WorkspaceIndex::load_or_build(&root);
        let first_anchors = first.select_anchors(&candidates, 8);
        let first_scans = first.scan_count();
        assert!(first_scans > 0, "首轮必须现场扫描，实际 {first_scans}");
        first.save(&root);
        assert!(root.join(".harness/learned.json").exists(), "应写出学习沉淀文件");

        // 第二轮：命中缓存，现场扫描应为 0，裁决完全一致。
        let second = WorkspaceIndex::load_or_build(&root);
        assert!(second.loaded_from_cache(), "第二轮应命中缓存");
        let second_anchors = second.select_anchors(&candidates, 8);
        assert_eq!(
            second.scan_count(),
            0,
            "命中缓存时不应再现场扫描，实际 {}",
            second.scan_count()
        );
        assert_eq!(second_anchors, first_anchors, "缓存不应改变裁决结果");

        // 删除沉淀后重跑：正确性不变，但需再次现场扫描（步数上升）。
        let _ = std::fs::remove_file(root.join(".harness/learned.json"));
        let third = WorkspaceIndex::load_or_build(&root);
        assert!(!third.loaded_from_cache());
        let third_anchors = third.select_anchors(&candidates, 8);
        assert!(
            third.scan_count() > 0,
            "删除沉淀后必须重新现场扫描，实际 {}",
            third.scan_count()
        );
        assert_eq!(third_anchors, first_anchors, "删除沉淀后裁决结果必须一致");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// S2 验收（二）：冷启动种子。
    /// 仓库一个源码文件都没有时，内置默认画像（D5 迁出的中文词表）给最常见的领域
    /// 标记一个"稀有"命中，让首轮至少有一个可提议的锚点；真正文件出现后由现场扫描覆盖。
    #[test]
    fn cold_start_seeds_builtin_profile_when_repo_is_empty() {
        let root = temp_root("cold");
        // 仓库里没有任何源码文件。
        let index = WorkspaceIndex::load_or_build(&root);
        assert_eq!(index.file_count(), 0, "空仓库不应扫到任何文件");
        // 内置默认画像（中文名词标记）应作为冷启动种子提供初始召回：
        // "字段" 这类标记被视为稀有高价值锚点（命中数=1），而非 Absent。
        let graded = index.grade(&"字段".to_string());
        assert_eq!(
            graded.grade,
            AnchorGrade::HighValue,
            "冷启动应给内置标记初始召回，实际 {:?}",
            graded.grade
        );
        // 非内置标记的随意词仍应判为 Absent（种子不会无中生有）。
        let unknown = index.grade(&"订单编号xyz".to_string());
        assert_eq!(unknown.grade, AnchorGrade::Absent);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// S2 验收（三）：冷启动种子被真实文件覆盖。
    /// 同一词表标记若在真实仓库里处处命中（停用词），不应再被种子抬成高价值。
    #[test]
    fn cold_start_seed_is_overridden_by_real_files() {
        let root = temp_root("coldover");
        std::fs::create_dir_all(root.join("src")).unwrap();
        // "字段" 在 8 个文件里都出现 —— 真实工作区判定为停用词，而非冷启动种子的稀有锚点。
        for i in 0..8 {
            std::fs::write(
                root.join(format!("src/f{i}.ts")),
                format!("const 字段 = {i}; // 字段 出现"),
            )
            .unwrap();
        }
        let index = WorkspaceIndex::load_or_build(&root);
        assert!(index.file_count() >= 8);
        let graded = index.grade(&"字段".to_string());
        assert_ne!(
            graded.grade,
            AnchorGrade::HighValue,
            "真实工作区里处处命中的词不应被冷启动种子抬成高价值，实际 {:?}",
            graded.grade
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
