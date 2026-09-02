//! V3 工作区确认：用少量确定性本地证据确认当前仓库是否可能包含目标。
//!
//! V5 之后这里承担两件事，缺一不可：
//!   1. **L2 裁决**——用工作区命中率决定哪些候选是真锚点（见 `WorkspaceIndex`）；
//!   2. 用裁决后的锚点扫描，把命中文件提升为求解图的执行候选。
//!
//! 顺序很重要：先裁决再扫描。旧实现反过来——先用词表判断"这是不是名词"再拿去
//! 扫，于是"用户说的形式在工作区里不存在"就退化成零命中，然后靠放宽子串一遍遍
//! 重试。现在候选由 L1 无语义切分提出，工作区一次性裁决出真正存在的那种形式。

use std::path::Path;

use crate::goal_execution::GoalContract;
use crate::workspace_index::WorkspaceIndex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroundingStatus {
    Grounded,
    NavigationOnly,
    Mismatch,
    Unavailable,
}

#[derive(Debug, Clone)]
pub struct WorkspaceGrounding {
    pub status: GroundingStatus,
    pub scanned_files: usize,
    /// false 表示命中了扫描上限；此时“无命中”不能作为工作区不匹配的硬证据。
    pub complete_scan: bool,
    pub entity_hits: Vec<String>,
    pub navigation_hits: Vec<String>,
    /// 明确旧值（如导航末级名称）的字面命中。确定性微任务优先复用此候选，
    /// 完整扫描无命中时可在调用模型前准确报告工作区不匹配。
    pub literal_hits: Vec<String>,
    /// 内容扫描与路径降级都没有命中。**当且仅当候选在工作区里全部缺席时**才置位
    /// ——这是一个有依据的判定（工作区里没有用户说的东西），而不是"没搜到"的兜底。
    /// 此时 Agent 是在零先验下开工，调用方应当索取定位信息或放宽并行度做多假设
    /// 并行探测，而不是串行换关键词烧步数。
    pub zero_prior: bool,
}

impl WorkspaceGrounding {
    pub fn render_for_model(&self) -> String {
        // 零先验时明确告知模型：本轮已放宽为多假设并行探测，应一次覆盖多个
        // 候选关键词，而不是逐个串行重试——后者正是空跑的典型形态。
        let zero_prior_note = self.zero_prior.then(|| {
            "\n[零先验] 内容扫描与路径降级均未命中。已放宽为多假设并行探测：请在一次响应中提出多个不同关键词的搜索，不要逐个串行重试。"
        }).unwrap_or_default();
        format!(
            "[工作区确认]\n状态：{:?}\n已扫描源码文件：{}（{}）\n精确旧值命中：{}\n字段/实体命中：{}\n导航命中：{}\n命中文件已作为执行候选；有候选时直接读取，不要重复搜索或目录枚举。{}",
            self.status,
            self.scanned_files,
            if self.complete_scan { "完整扫描" } else { "达到扫描上限，仅作候选" },
            if self.literal_hits.is_empty() { "无".into() } else { self.literal_hits.join("、") },
            if self.entity_hits.is_empty() { "无".into() } else { self.entity_hits.join("、") },
            if self.navigation_hits.is_empty() { "无".into() } else { self.navigation_hits.join("、") },
            zero_prior_note,
        )
    }

    pub fn needs_user_input(&self) -> bool {
        self.status == GroundingStatus::Mismatch
    }

    pub fn user_question(&self, goal: &GoalContract) -> String {
        format!(
            "当前工作区扫描了 {} 个源码文件，未找到目标实体 [{}] 或导航入口 [{}]。请确认正确的项目、子目录或分支；在确认前不继续进行泛搜。",
            self.scanned_files,
            if goal.entities.is_empty() { "未提取".into() } else { goal.entities.join("、") },
            if goal.navigation.is_empty() { "未提取".into() } else { goal.navigation.join(" → ") },
        )
    }
}

pub struct WorkspaceGrounder;

impl WorkspaceGrounder {
    /// 精确变换快速通道：用户已经给出旧值时，直接逐文件查找该字面量，不构建
    /// n-gram 候选索引、也不计算工作区词频。命中即得到可读取候选；完整扫描仍无
    /// 命中则形成可靠 mismatch，交给调用方询问工作区/分支，而不是再换关键词。
    pub fn ground_exact_literal(root: &Path, goal: &GoalContract) -> Option<WorkspaceGrounding> {
        let literal = goal
            .transformation
            .as_ref()
            .and_then(|value| value.from_value.as_deref())?;
        if !root.is_dir() {
            return Some(Self::empty());
        }

        let needle = crate::workspace_index::squash(literal);
        if needle.chars().count() < 2 {
            return Some(Self::empty());
        }
        let mut paths = Vec::new();
        let mut truncated = false;
        crate::workspace_index::collect_source_files(
            root,
            &mut paths,
            crate::workspace_index::DEFAULT_MAX_FILES,
            &mut truncated,
        );
        let mut literal_hits = Vec::new();
        let mut scanned_files = 0usize;
        for absolute in paths {
            let Ok(content) = std::fs::read_to_string(&absolute) else {
                continue;
            };
            scanned_files += 1;
            if crate::workspace_index::squash(&content).contains(&needle) {
                literal_hits.push(
                    absolute
                        .strip_prefix(root)
                        .unwrap_or(&absolute)
                        .display()
                        .to_string(),
                );
                // 原子替换先读取首个权威候选确认上下文；若它不是目标，Inspect 阶段
                // 才在已命中目录内继续。不要为了统计所有重复文案预读完整工作区。
                break;
            }
        }
        let status = if !literal_hits.is_empty() {
            GroundingStatus::Grounded
        } else if !truncated {
            GroundingStatus::Mismatch
        } else {
            GroundingStatus::Unavailable
        };
        Some(WorkspaceGrounding {
            status,
            scanned_files,
            // 命中后主动停止不是完整扫描；只有零命中时该标记才参与 mismatch 解释。
            complete_scan: !truncated && literal_hits.is_empty(),
            entity_hits: Vec::new(),
            navigation_hits: Vec::new(),
            zero_prior: literal_hits.is_empty(),
            literal_hits,
        })
    }

    pub fn ground(root: &Path, goal: &GoalContract) -> WorkspaceGrounding {
        if !root.is_dir() {
            return Self::empty();
        }
        // 复用/更新学习沉淀：命中缓存即跳过现场扫描，未命中才扫并把结果写回
        // `.harness/learned.json`，下一轮同表述直接命中。
        let index = WorkspaceIndex::load_or_build(root);
        let grounding = Self::ground_with(&index, goal);
        let _ = index.save(root);
        grounding
    }

    /// 用已有索引确认。当调用方已经为澄清门禁建过索引时走这条路，
    /// 避免把整个工作区读两遍。
    pub fn ground_with(index: &WorkspaceIndex, goal: &GoalContract) -> WorkspaceGrounding {
        if index.is_empty() {
            return Self::empty();
        }
        let source_literal = goal
            .transformation
            .as_ref()
            .and_then(|value| value.from_value.as_deref());

        // L2 裁决：L0 结构信号自证可定位，语言候选须经工作区命中率认可。
        // 合并是幂等的——若调用方已裁决过一次，这里不会产生重复项。
        let adjudication = index.adjudicate(&goal.candidates, 8);
        let mut entities = goal.entities.clone();
        for anchor in &adjudication.anchors {
            if !entities.contains(anchor) {
                entities.push(anchor.clone());
            }
        }
        entities.truncate(8);

        // 第一轮：用裁决后的锚点扫描文件内容。直接复用索引已读入的 `content`，
        // 不再把每个文件从磁盘读第二遍（性能实测在 318 文件仓库上省去约一次全量读盘）。
        let mut hits = Self::scan_content(index, &entities, &goal.navigation, source_literal);

        // 第二轮降级：内容没命中时用锚点匹配文件路径。目录名往往比正文更能说明
        // "这个功能在哪"，且不需要再读一遍文件。
        if hits.is_empty() {
            hits.entity_hits = index.match_paths(&entities, 8);
        }

        // 零先验只在"候选确实全部缺席"时成立。这与"扫描没命中"是两回事：
        // 后者可能只是命名习惯不同（camelCase vs 下划线），前者才是真的没有。
        let zero_prior =
            hits.is_empty() && (goal.candidates.is_empty() || adjudication.all_absent);
        let status = if !hits.literal_hits.is_empty() || !hits.entity_hits.is_empty() {
            GroundingStatus::Grounded
        } else if !hits.navigation_hits.is_empty() {
            GroundingStatus::NavigationOnly
        } else if !index.truncated()
            && (!goal.code_entities.is_empty() || source_literal.is_some())
        {
            // 严格判定只看代码符号与明确旧值。中文片段和缩写太泛——"模型管理"没命中
            // 不代表仓库里没有这个页面，据此报"工作区不匹配"会误伤中文项目。
            GroundingStatus::Mismatch
        } else {
            GroundingStatus::Unavailable
        };
        WorkspaceGrounding {
            status,
            scanned_files: index.file_count(),
            complete_scan: !index.truncated(),
            entity_hits: hits.entity_hits,
            navigation_hits: hits.navigation_hits,
            literal_hits: hits.literal_hits,
            zero_prior,
        }
    }

    fn empty() -> WorkspaceGrounding {
        WorkspaceGrounding {
            status: GroundingStatus::Unavailable,
            scanned_files: 0,
            complete_scan: false,
            entity_hits: vec![],
            navigation_hits: vec![],
            literal_hits: vec![],
            zero_prior: true,
        }
    }

    fn scan_content(
        index: &WorkspaceIndex,
        entities: &[String],
        navigation: &[String],
        source_literal: Option<&str>,
    ) -> RawHits {
        // 用与裁决一致的 squash 匹配（大小写与分隔符无关），并直接复用索引已读入的
        // `content`，不再读盘。这让扫描阶段与裁决阶段看到完全相同的正文，杜绝
        // "裁决说有、扫描说没有"的自相矛盾证据。
        let entity_needles: Vec<String> = entities
            .iter()
            .map(|entity| crate::workspace_index::squash(entity))
            .collect();
        let nav_needles: Vec<String> = navigation
            .iter()
            .filter(|nav| nav.chars().count() >= 2)
            .map(|nav| crate::workspace_index::squash(nav))
            .collect();
        let literal = source_literal.map(crate::workspace_index::squash);
        let mut hits = RawHits::default();
        for file in index.files() {
            if !entity_needles.is_empty()
                && entity_needles.iter().any(|needle| file.content.contains(needle))
            {
                hits.entity_hits.push(file.relative.clone());
            }
            if !nav_needles.is_empty()
                && nav_needles.iter().any(|needle| file.content.contains(needle))
            {
                hits.navigation_hits.push(file.relative.clone());
            }
            if literal.as_ref().is_some_and(|needle| file.content.contains(needle)) {
                hits.literal_hits.push(file.relative.clone());
            }
            if hits.entity_hits.len() >= 8
                && hits.navigation_hits.len() >= 8
                && hits.literal_hits.len() >= 8
            {
                break;
            }
        }
        hits
    }

}

#[derive(Default, Debug, Clone)]
struct RawHits {
    entity_hits: Vec<String>,
    navigation_hits: Vec<String>,
    literal_hits: Vec<String>,
}

impl RawHits {
    fn is_empty(&self) -> bool {
        self.entity_hits.is_empty()
            && self.navigation_hits.is_empty()
            && self.literal_hits.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// V5 S1 的验收标准之一：**英文任务不得依赖中文词表定位**。
    /// 这条用例在旧实现下必然失败——`CJK_NOUN_MARKERS` 里没有任何一个词能
    /// 命中 "Model Name" / "API Key"。
    #[test]
    fn english_task_grounds_without_any_chinese_word_list() {
        let root = std::env::temp_dir().join(format!("grounder-en-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src/pages")).unwrap();
        fs::write(
            root.join("src/pages/model.tsx"),
            "<label>Model Name</label><input name=\"modelName\" /><input name=\"apiKey\" />",
        )
        .unwrap();
        let goal = GoalContract::compile("swap the order of Model Name and API Key fields");
        let grounding = WorkspaceGrounder::ground(&root, &goal);
        assert_eq!(
            grounding.status,
            GroundingStatus::Grounded,
            "英文任务应靠工作区命中率定位，实际 {:?}",
            grounding
        );
        assert!(grounding.entity_hits.iter().any(|hit| hit.contains("model.tsx")));
        let _ = fs::remove_dir_all(&root);
    }

    /// 用户说的形式（"模型名称字段"）与代码里的形式（"模型名称"）不同时，
    /// 由工作区裁决出真正存在的那一种，而不是靠词表猜。
    #[test]
    fn workspace_picks_the_form_that_actually_exists() {
        let root = std::env::temp_dir().join(format!("grounder-form-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/form.tsx"), "<label>模型名称</label>").unwrap();
        let mut goal = GoalContract::compile("把模型名称字段的显示调整一下");
        let index = WorkspaceIndex::build(&root);
        goal.resolve_against(&index);
        assert!(
            goal.entities.iter().any(|entity| entity == "模型名称"),
            "应裁决出代码里真实存在的写法，实际 {:?}",
            goal.entities
        );
        assert!(
            !goal.entities.iter().any(|entity| entity == "模型名称字段"),
            "代码里不存在的整串必须被淘汰，实际 {:?}",
            goal.entities
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn grounds_a_workspace_with_an_entity_hit() {
        let root = std::env::temp_dir().join(format!("grounder-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/page.tsx"), "const appCode = form.appCode;").unwrap();
        let goal = GoalContract::compile("应用档案列表展示 appCode");
        let grounding = WorkspaceGrounder::ground(&root, &goal);
        assert_eq!(grounding.status, GroundingStatus::Grounded);
        assert!(grounding.complete_scan);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn falls_back_to_path_matching_when_content_scan_misses() {
        let root = std::env::temp_dir().join(format!("grounder-path-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        // 目录名能说明"这个功能在哪"，即使文件内容里没有该符号。
        fs::create_dir_all(root.join("src/features/appCode")).unwrap();
        fs::write(root.join("src/features/appCode/editor.tsx"), "const x = 1;").unwrap();
        let goal = GoalContract::compile("appCode 的校验规则有问题");
        let grounding = WorkspaceGrounder::ground(&root, &goal);
        assert_eq!(grounding.status, GroundingStatus::Grounded);
        assert!(
            grounding.entity_hits.iter().any(|hit| hit.contains("appCode")),
            "应通过路径降级命中，实际 {:?}",
            grounding.entity_hits
        );
        assert!(!grounding.zero_prior);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn chinese_subterm_relaxation_finds_partial_labels() {
        let root = std::env::temp_dir().join(format!("grounder-sub-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        // 整串"模型名称字段"不出现，但"模型名称"出现——整串匹配对中文太严格。
        fs::write(root.join("src/form.tsx"), "<label>模型名称</label>").unwrap();
        let goal = GoalContract::compile("调整模型名称字段的显示");
        let grounding = WorkspaceGrounder::ground(&root, &goal);
        assert_eq!(grounding.status, GroundingStatus::Grounded);
        assert!(grounding.entity_hits.iter().any(|hit| hit.contains("form.tsx")));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn marks_zero_prior_when_every_pass_misses() {
        let root = std::env::temp_dir().join(format!("grounder-zero-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/hello.ts"), "const x = 1;").unwrap();
        let goal = GoalContract::compile("把订单编号字段和创建时间字段位置互换");
        let grounding = WorkspaceGrounder::ground(&root, &goal);
        assert_eq!(grounding.status, GroundingStatus::Unavailable);
        assert!(
            grounding.zero_prior,
            "三级降级都没命中时必须标记零先验，以便调用方放宽并行度"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn chinese_entities_do_not_trigger_a_false_mismatch() {
        let root = std::env::temp_dir().join(format!("grounder-nomismatch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/hello.ts"), "const x = 1;").unwrap();
        // 只有中文实体、没有代码符号时，"没命中"不构成工作区不匹配的证据。
        let goal = GoalContract::compile("系统管理->模型管理，把模型名称字段顺序调整一下");
        let grounding = WorkspaceGrounder::ground(&root, &goal);
        assert_ne!(
            grounding.status,
            GroundingStatus::Mismatch,
            "中文实体太泛，据此报不匹配会误伤中文项目"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn exact_transformation_grounds_old_literal_without_model_search() {
        let root = std::env::temp_dir().join(format!("grounder-literal-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/menu.ts"), "name: '多端拼装'").unwrap();
        let goal = GoalContract::compile("后台管理->多端拼装，菜单名称修改为智能体装配");
        let grounding = WorkspaceGrounder::ground(&root, &goal);
        assert_eq!(grounding.status, GroundingStatus::Grounded);
        assert_eq!(grounding.literal_hits, vec!["src\\menu.ts"]);
        let _ = fs::remove_dir_all(&root);
    }
    #[test]
    fn quoted_shortening_grounds_the_old_literal_without_bruteforce() {
        let root = std::env::temp_dir().join(format!(
            "grounder-quoted-literal-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/composer.rs"),
            "ui.button(\"添加到对话框附件\");",
        )
        .unwrap();
        let goal = GoalContract::compile(
            "弹出菜单应包含“📎 添加到对话框附件”，文字精简一下，“添加到对话”",
        );
        let grounding = WorkspaceGrounder::ground_exact_literal(&root, &goal)
            .expect("双引号旧值应进入精确字面量通道");
        assert_eq!(grounding.status, GroundingStatus::Grounded);
        assert_eq!(grounding.literal_hits, vec!["src\\composer.rs"]);
        let _ = fs::remove_dir_all(&root);
    }
}
