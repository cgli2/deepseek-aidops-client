//! S5/G1：计划特化——LLM 生成 `SolveSketch` 的门禁与回落。
//!
//! 设计边界（V5 §6.1 / D1 / D4）：
//! - 计划可由 LLM 生成，但**必须**经过 schema 校验；校验失败回落静态模板。
//! - 判据类别（`kind`）与收敛声明（`convergence`）必须在白名单内，否则回落 `unknown`/执行。
//! - 依赖必须构成无环 DAG；含环的草图一律拒绝（回落）。
//! - G1 失败**不得**影响可用性：任何一步失败都回到 `GoalExecution::from_contract`。
//!
//! 不引入新的 serde derive 依赖：LLM 输出是 `serde_json::Value`，手动抽取并校验，
//! 这样校验逻辑（白名单、引用完整性、环）与解析耦合在同一处，且无需改动 Cargo 依赖。

use crate::execution::TaskContract;
use crate::goal_execution::{PhaseBudget, SurfaceKind};

/// D4 收敛声明白名单（模型只能在这两个值里选）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvergenceDeclaration {
    /// 完成可用产物证据逐字复核（仍需真实复核通过，见 S4）。
    StaticallyProvable,
    /// 必须实际执行验证命令。
    NeedsExecution,
}

impl ConvergenceDeclaration {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StaticallyProvable => "statically_provable",
            Self::NeedsExecution => "needs_execution",
        }
    }
}

/// 草图中的单个交付面。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SketchSurface {
    pub id: String,
    pub kind: SurfaceKind,
    pub depends_on: Vec<String>,
    pub budget: PhaseBudget,
    pub convergence: ConvergenceDeclaration,
}

/// 任务特化的求解草图（带依赖的交付面 DAG）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolveSketch {
    pub surfaces: Vec<SketchSurface>,
}

/// 校验结果：合法则返回 `Valid`，否则 `Invalid` 并附带所有错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SketchValidation {
    Valid(SolveSketch),
    Invalid(Vec<String>),
}

impl SketchValidation {
    /// 便捷解包：`Valid` 取出草图，`Invalid` 返回 `None`（调用方据此回落）。
    pub fn into_option(self) -> Option<SolveSketch> {
        match self {
            Self::Valid(sketch) => Some(sketch),
            Self::Invalid(_) => None,
        }
    }

    pub fn errors(&self) -> Vec<String> {
        match self {
            Self::Invalid(errors) => errors.clone(),
            Self::Valid(_) => Vec::new(),
        }
    }
}

impl SolveSketch {
    /// 从 LLM 返回的 JSON 解析并校验。形态：
    /// ```json
    /// {
    ///   "surfaces": [
    ///     {"id":"s1","kind":"ui","depends_on":[],
    ///      "budget":{"locate":2,"inspect":2,"change":2,"verify":2},
    ///      "convergence":"statically_provable"}
    ///   ]
    /// }
    /// ```
    ///
    /// 校验项：唯一 id、kind 白名单、convergence 白名单、depends_on 引用存在、无环。
    /// 任一失败返回 `Invalid`（携带全部错误），调用方回落静态模板。
    pub fn from_llm_json(json: &str) -> SketchValidation {
        let value: serde_json::Value = match serde_json::from_str(json) {
            Ok(value) => value,
            Err(err) => {
                return SketchValidation::Invalid(vec![format!("SolveSketch JSON 解析失败：{err}")]);
            }
        };
        let Some(array) = value.get("surfaces").and_then(|value| value.as_array()) else {
            return SketchValidation::Invalid(vec!["SolveSketch 缺少 surfaces 数组".into()]);
        };
        let mut surfaces: Vec<SketchSurface> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (index, entry) in array.iter().enumerate() {
            let id = entry
                .get("id")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            if id.is_empty() {
                errors.push(format!("surfaces[{index}] 缺少 id"));
                continue;
            }
            if !seen.insert(id.clone()) {
                errors.push(format!("surfaces[{index}] id 重复：{id}"));
                continue;
            }
            let kind_raw = entry
                .get("kind")
                .and_then(|value| value.as_str())
                .unwrap_or("undeclared");
            // 白名单校验：非法字面值直接拒绝（不能因为"看不懂"就当未声明放行）。
            if !SurfaceKind::DECLARABLE
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(kind_raw.trim()))
            {
                errors.push(format!("surfaces[{index}] kind 非法：{kind_raw}"));
            }
            let kind = SurfaceKind::from_declared(kind_raw);
            let depends_on: Vec<String> = entry
                .get("depends_on")
                .and_then(|value| value.as_array())
                .map(|array| {
                    array
                        .iter()
                        .filter_map(|value| value.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let budget = parse_budget(entry.get("budget")).unwrap_or_default();
            let convergence_raw = entry
                .get("convergence")
                .and_then(|value| value.as_str())
                .unwrap_or("needs_execution");
            let convergence = match convergence_raw {
                "statically_provable" => ConvergenceDeclaration::StaticallyProvable,
                "needs_execution" => ConvergenceDeclaration::NeedsExecution,
                other => {
                    errors.push(format!("surfaces[{index}] convergence 非法：{other}"));
                    ConvergenceDeclaration::NeedsExecution
                }
            };
            surfaces.push(SketchSurface {
                id,
                kind,
                depends_on,
                budget,
                convergence,
            });
        }
        // 引用完整性：depends_on 必须指向已声明 id。
        for (index, surface) in surfaces.iter().enumerate() {
            for dep in &surface.depends_on {
                if !seen.contains(dep) {
                    errors.push(format!(
                        "surfaces[{index}] depends_on 引用不存在的 id：{dep}"
                    ));
                }
            }
        }
        // 环检测（基于依赖边）。
        if errors.is_empty() {
            let mut color: std::collections::HashMap<String, u8> = std::collections::HashMap::new();
            let mut stack: Vec<String> = Vec::new();
            let has_cycle = surfaces.iter().any(|surface| {
                dfs_cycle(&surface.id, &surfaces, &mut color, &mut stack).is_some()
            });
            if has_cycle {
                errors.push("SolveSketch 依赖图存在环，已拒绝".into());
            }
        }
        if errors.is_empty() {
            SketchValidation::Valid(SolveSketch { surfaces })
        } else {
            SketchValidation::Invalid(errors)
        }
    }

    /// 静态回落生成器：把契约的验收项直接映射为无依赖、未声明类别的面，
    /// 等价于当前 `GoalExecution::from_contract` 的行为。LLM 不可用或校验失败时使用。
    pub fn from_contract(contract: &TaskContract) -> Self {
        SolveSketch {
            surfaces: contract
                .acceptance_criteria
                .iter()
                .map(|criterion| SketchSurface {
                    id: criterion.id.clone(),
                    kind: SurfaceKind::default(),
                    depends_on: Vec::new(),
                    budget: PhaseBudget::default(),
                    convergence: ConvergenceDeclaration::NeedsExecution,
                })
                .collect(),
        }
    }

    /// 序列化回 `from_llm_json` 接受的形态，用于把**本地/LLM 草图**回灌进
    /// `GoalExecution::from_input_with_sketch`，形成闭环（D1：回灌仍经同一套校验，
    /// 失败即回落静态模板，零风险）。本地计划器即借此让 G1 的 `from_sketch` 路径
    /// 在运行时真正运行，而不再永远是 `None` 死路。
    pub fn to_json(&self) -> String {
        let surfaces: Vec<serde_json::Value> = self
            .surfaces
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id,
                    "kind": s.kind.declared_str(),
                    "depends_on": s.depends_on,
                    "budget": {
                        "locate": s.budget.locate,
                        "inspect": s.budget.inspect,
                        "change": s.budget.change,
                        "verify": s.budget.verify,
                    },
                    "convergence": s.convergence.as_str(),
                })
            })
            .collect();
        serde_json::json!({ "surfaces": surfaces }).to_string()
    }
}

fn parse_budget(value: Option<&serde_json::Value>) -> Option<PhaseBudget> {
    let value = value?;
    let get = |key: &str| value.get(key).and_then(|v| v.as_u64()).unwrap_or(2) as u8;
    Some(PhaseBudget {
        locate: get("locate"),
        inspect: get("inspect"),
        change: get("change"),
        verify: get("verify"),
    })
}

/// 依赖图环检测（DFS 三色标记）。命中回边返回环上的顶点序列。
fn dfs_cycle(
    id: &str,
    surfaces: &[SketchSurface],
    color: &mut std::collections::HashMap<String, u8>,
    stack: &mut Vec<String>,
) -> Option<Vec<String>> {
    match color.get(id).copied().unwrap_or(0) {
        2 => return None,
        1 => {
            if let Some(pos) = stack.iter().position(|s| s == id) {
                return Some(stack[pos..].to_vec());
            }
            return Some(vec![id.to_string()]);
        }
        _ => {}
    }
    color.insert(id.to_string(), 1);
    stack.push(id.to_string());
    let deps = surfaces
        .iter()
        .find(|s| s.id == id)
        .map(|s| s.depends_on.clone())
        .unwrap_or_default();
    for dep in &deps {
        if surfaces.iter().any(|s| s.id == *dep) && dfs_cycle(dep, surfaces, color, stack).is_some() {
            return Some(stack.clone());
        }
    }
    color.insert(id.to_string(), 2);
    stack.pop();
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_sketch_passes_validation_and_roundtrips() {
        let json = r#"{
            "surfaces": [
                {"id":"ui","kind":"ui","depends_on":[],
                 "budget":{"locate":2,"inspect":2,"change":2,"verify":2},
                 "convergence":"statically_provable"},
                {"id":"api","kind":"api","depends_on":["ui"],
                 "budget":{"locate":1,"inspect":3,"change":2,"verify":2},
                 "convergence":"needs_execution"}
            ]
        }"#;
        let validation = SolveSketch::from_llm_json(json);
        assert!(matches!(validation, SketchValidation::Valid(_)), "应校验通过");
        let sketch = validation.into_option().unwrap();
        assert_eq!(sketch.surfaces.len(), 2);
        assert_eq!(sketch.surfaces[1].depends_on, vec!["ui".to_string()]);
        assert_eq!(sketch.surfaces[0].kind, SurfaceKind::Ui);
    }

    #[test]
    fn illegal_kind_and_convergence_are_rejected() {
        let json = r#"{
            "surfaces": [
                {"id":"a","kind":"uii","depends_on":[],
                 "budget":{"locate":2,"inspect":2,"change":2,"verify":2},
                 "convergence":"magic"}
            ]
        }"#;
        let validation = SolveSketch::from_llm_json(json);
        assert!(matches!(validation, SketchValidation::Invalid(_)));
        let errors = validation.errors();
        assert!(
            errors.iter().any(|e| e.contains("kind 非法")),
            "应报非法 kind：{errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.contains("convergence 非法")),
            "应报非法 convergence：{errors:?}"
        );
    }

    #[test]
    fn duplicate_id_and_dangling_dependency_are_rejected() {
        let json = r#"{
            "surfaces": [
                {"id":"x","kind":"ui","depends_on":["missing"],
                 "budget":{"locate":2,"inspect":2,"change":2,"verify":2},
                 "convergence":"statically_provable"},
                {"id":"x","kind":"api","depends_on":[],
                 "budget":{"locate":2,"inspect":2,"change":2,"verify":2},
                 "convergence":"needs_execution"}
            ]
        }"#;
        let validation = SolveSketch::from_llm_json(json);
        assert!(matches!(validation, SketchValidation::Invalid(_)));
        let errors = validation.errors();
        assert!(errors.iter().any(|e| e.contains("id 重复")));
        assert!(errors.iter().any(|e| e.contains("引用不存在")));
    }

    #[test]
    fn cyclic_dependency_is_rejected() {
        let json = r#"{
            "surfaces": [
                {"id":"a","kind":"ui","depends_on":["b"],
                 "budget":{"locate":2,"inspect":2,"change":2,"verify":2},
                 "convergence":"statically_provable"},
                {"id":"b","kind":"api","depends_on":["a"],
                 "budget":{"locate":2,"inspect":2,"change":2,"verify":2},
                 "convergence":"needs_execution"}
            ]
        }"#;
        let validation = SolveSketch::from_llm_json(json);
        assert!(matches!(validation, SketchValidation::Invalid(_)));
        assert!(validation.errors().iter().any(|e| e.contains("环")));
    }

    #[test]
    fn malformed_json_falls_back_with_error() {
        let validation = SolveSketch::from_llm_json("not json {");
        assert!(matches!(validation, SketchValidation::Invalid(_)));
    }

    #[test]
    fn static_fallback_preserves_criterion_count() {
        let contract = TaskContract::from_input("- 列表展示\n- 详情展示\n- 新增表单");
        let sketch = SolveSketch::from_contract(&contract);
        assert_eq!(sketch.surfaces.len(), 3);
        assert!(sketch
            .surfaces
            .iter()
            .all(|s| s.depends_on.is_empty() && s.kind == SurfaceKind::Undeclared));
    }
}
