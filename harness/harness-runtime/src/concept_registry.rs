//! S5/G4：概念注册表。
//!
//! 跨交付面共享的符号登记为概念，避免同一概念在不同位置被当作独立目标重复探索，
//! 也避免漏改：当某个交付面改了 `appCode`，其它同样引用 `appCode` 的面也该被改到。
//!
//! 概念从 **L0 标识符信号**（`GoalContract` 的候选符号 + 代码实体）自动建立，位置由
//! **L2 工作区裁决**填充（`register`）。全程不需要领域知识——机制层只认"这个符号在
//! 哪些面、哪些文件里出现过"，至于符号语义是什么交给工作区。

use std::collections::HashMap;

use crate::goal_execution::GoalContract;

/// 概念在某交付面某一文件中的出现位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConceptLocation {
    pub surface_id: String,
    pub symbol: String,
    pub path: String,
}

/// 概念注册表：符号 → 它在各交付面/文件的出现位置。
#[derive(Debug, Clone, Default)]
pub struct ConceptRegistry {
    by_symbol: HashMap<String, Vec<ConceptLocation>>,
}

impl ConceptRegistry {
    /// S5/G4：从 L0 标识符信号（候选符号 + 代码实体）自动建立概念。
    /// 单字符符号（几乎必然命中，等于噪声）被排除。
    pub fn seed_from_goal(goal: &GoalContract) -> Self {
        let mut registry = ConceptRegistry::default();
        for symbol in goal
            .candidates
            .iter()
            .chain(goal.code_entities.iter())
        {
            if symbol.chars().count() >= 2 {
                registry.by_symbol.entry(symbol.clone()).or_default();
            }
        }
        registry
    }

    /// 登记一次"符号在某交付面某文件出现"。位置来源是 L2 工作区裁决后的候选文件。
    pub fn register(&mut self, surface_id: &str, symbol: &str, path: &str) {
        if symbol.chars().count() < 2 {
            return;
        }
        self.by_symbol
            .entry(symbol.to_string())
            .or_default()
            .push(ConceptLocation {
                surface_id: surface_id.to_string(),
                symbol: symbol.to_string(),
                path: path.to_string(),
            });
    }

    pub fn symbols(&self) -> Vec<String> {
        self.by_symbol.keys().cloned().collect()
    }

    /// 某符号的全部出现位置（跨交付面）。
    pub fn coverage(&self, symbol: &str) -> Vec<&ConceptLocation> {
        self.by_symbol
            .get(symbol)
            .map(|locations| locations.iter().collect())
            .unwrap_or_default()
    }

    /// S5/G4 防漏改：返回"引用了 `symbol` 但该面尚未改动"的交付面 id 集合。
    ///
    /// `changed_surfaces` 为已经对该 `symbol` 完成改动的交付面 id。若某面出现在覆盖里
    /// 却不在 `changed_surfaces` 中，说明它本应同步改动却漏了。
    pub fn surfaces_missing_concept(
        &self,
        symbol: &str,
        changed_surfaces: &std::collections::HashSet<&str>,
    ) -> Vec<String> {
        self.by_symbol
            .get(symbol)
            .map(|locations| {
                locations
                    .iter()
                    .filter(|location| !changed_surfaces.contains(location.surface_id.as_str()))
                    .map(|location| location.surface_id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 对注册表里所有多面符号，找出"部分面已改、部分面漏改"的潜在漏改项。
    ///
    /// 返回 `(symbol, 漏改面 id)` 列表。`is_changed(surface_id)` 决定某面是否已完成改动
    /// （通常用 `WorkItem::state` 是否 `Verified`/`Changed`）。仅当某符号的覆盖跨越 >1 面、
    /// 且并非所有面都已完成时才报告，避免对单面符号误报。
    pub fn missing_coverage_report<F>(&self, mut is_changed: F) -> Vec<(String, Vec<String>)>
    where
        F: FnMut(&str) -> bool,
    {
        let mut report = Vec::new();
        // 按符号稳定排序后遍历，避免 HashMap 迭代顺序导致报告非字节确定。
        let mut symbols: Vec<&String> = self.by_symbol.keys().collect();
        symbols.sort();
        for symbol in symbols {
            let locations = &self.by_symbol[symbol];
            if locations.len() < 2 {
                continue;
            }
            let mut missing: Vec<String> = locations
                .iter()
                .filter(|location| !is_changed(&location.surface_id))
                .map(|location| location.surface_id.clone())
                .collect();
            // 交付面顺序同样稳定排序，避免 register 调用顺序带来非确定性。
            missing.sort();
            let all_done = missing.is_empty();
            if !all_done {
                report.push((symbol.clone(), missing));
            }
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_goal() -> GoalContract {
        GoalContract::compile("在列表、详情、表单三处展示 appCode 字段")
    }

    #[test]
    fn seeds_symbols_from_l0_signals() {
        let registry = ConceptRegistry::seed_from_goal(&sample_goal());
        // appCode 是 L0 标识符信号，应被自动建为概念。
        assert!(registry.symbols().iter().any(|s| s == "appCode"));
    }

    #[test]
    fn register_and_coverage_track_locations() {
        let mut registry = ConceptRegistry::seed_from_goal(&sample_goal());
        registry.register("list", "appCode", "src/pages/list.tsx");
        registry.register("detail", "appCode", "src/pages/detail.tsx");
        registry.register("form", "appCode", "src/pages/form.tsx");
        assert_eq!(registry.coverage("appCode").len(), 3);
    }

    #[test]
    fn missing_concept_flags_unmodified_surfaces() {
        let mut registry = ConceptRegistry::seed_from_goal(&sample_goal());
        registry.register("list", "appCode", "src/pages/list.tsx");
        registry.register("detail", "appCode", "src/pages/detail.tsx");
        registry.register("form", "appCode", "src/pages/form.tsx");

        let mut changed = std::collections::HashSet::new();
        changed.insert("list");
        changed.insert("detail");
        // form 引用了 appCode 却没被改到 → 漏改。
        let missing = registry.surfaces_missing_concept("appCode", &changed);
        assert_eq!(missing, vec!["form".to_string()]);
    }

    #[test]
    fn missing_coverage_report_aggregates_per_symbol() {
        let mut registry = ConceptRegistry::seed_from_goal(&sample_goal());
        registry.register("list", "appCode", "src/pages/list.tsx");
        registry.register("detail", "appCode", "src/pages/detail.tsx");
        // 仅 list 完成改动，detail 漏了。
        let report = registry.missing_coverage_report(|id| id == "list");
        let entry = report.iter().find(|(sym, _)| sym == "appCode");
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().1, vec!["detail".to_string()]);
    }

    #[test]
    fn no_false_positive_when_all_surfaces_changed() {
        let mut registry = ConceptRegistry::seed_from_goal(&sample_goal());
        registry.register("list", "appCode", "src/pages/list.tsx");
        registry.register("detail", "appCode", "src/pages/detail.tsx");
        let report = registry.missing_coverage_report(|_| true);
        assert!(report.is_empty(), "全部已改不应报漏改：{report:?}");
    }

    #[test]
    fn missing_coverage_report_is_order_independent() {
        // register 调用顺序不同（模拟 HashMap 迭代顺序差异）时报告必须一致：
        // 符号与漏改面都按字典序稳定排序，否则打包后每次运行提示文本会漂移。
        fn build(order: &[&str]) -> ConceptRegistry {
            let mut r = ConceptRegistry::seed_from_goal(&sample_goal());
            for s in order {
                r.register(s, "appCode", &format!("src/{s}.tsx"));
            }
            r
        }
        let mut changed = std::collections::HashSet::new();
        changed.insert("list");
        changed.insert("detail");
        let a = build(&["list", "detail", "form"])
            .missing_coverage_report(|id| changed.contains(id));
        let b = build(&["form", "list", "detail"])
            .missing_coverage_report(|id| changed.contains(id));
        assert_eq!(a, b, "register 顺序不应影响报告（必须排序）");
        assert_eq!(a, vec![("appCode".to_string(), vec!["form".to_string()])]);
    }
}
