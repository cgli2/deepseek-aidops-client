//! superpowers-dsh 核心能力：内置技能集（Rust 原生）。
//!
//! 参照开源项目 `superpowers-dsh`（obra/superpowers 的 DeepSeek 移植版）的核心技能，
//! 以结构化 `Skill` 资产形式内置。首次导入时幂等注册进技能库（SkillLibrary），
//! 之后可像普通技能一样被 `match_skills` 匹配、在技能管理面板启用/禁用/删除。

use harness_capability::assets::Skill;
use harness_capability::assets::SkillLibrary;

/// 内置 superpowers 核心技能（id, 名称, 版本, 触发边界, 步骤, 验证规则）。
pub fn builtin_superpowers() -> Vec<Skill> {
    vec![
        Skill {
            id: "sp-boot".into(),
            name: "Boot（会话启动）".into(),
            version: "1.0".into(),
            trigger_boundary: "会话开始、需要加载项目上下文与技能清单、梳理当前任务起点".into(),
            steps: vec![
                "读取当前工作区结构（README、docs、.harness 配置）".into(),
                "扫描可用技能清单（技能库中已启用的技能）".into(),
                "确认当前任务目标与约束，输出任务起点摘要".into(),
            ],
            verification_rules: vec!["输出了任务起点摘要".into(), "列出了已启用技能清单".into()],
            resource_files: vec![],
            confidence: 0.9,
            enabled: true,
        },
        Skill {
            id: "sp-plan".into(),
            name: "Plan（任务规划）".into(),
            version: "1.0".into(),
            trigger_boundary: "任务复杂、多步骤、需要先规划再执行；用户要求先给方案".into(),
            steps: vec![
                "拆解目标为可执行步骤并排出依赖顺序".into(),
                "为每步标注验收标准".into(),
                "输出结构化计划（含风险与回退方案）".into(),
            ],
            verification_rules: vec!["计划含步骤与验收标准".into(), "步骤间有依赖顺序".into()],
            resource_files: vec![],
            confidence: 0.9,
            enabled: true,
        },
        Skill {
            id: "sp-execute".into(),
            name: "Execute（任务执行）".into(),
            version: "1.0".into(),
            trigger_boundary: "计划已就绪、执行具体改动/命令/代码编写".into(),
            steps: vec![
                "按计划逐项执行，优先小步验证".into(),
                "每步结束后用工具校验结果（fs/shell）".into(),
                "偏离计划时记录原因并同步给用户".into(),
            ],
            verification_rules: vec!["每个步骤有结果记录".into(), "未验证的结论不写入交付".into()],
            resource_files: vec![],
            confidence: 0.85,
            enabled: true,
        },
        Skill {
            id: "sp-test".into(),
            name: "Test（测试验证）".into(),
            version: "1.0".into(),
            trigger_boundary: "代码改动完成、需要验证正确性、运行测试".into(),
            steps: vec![
                "识别受影响模块与对应测试".into(),
                "运行相关测试并收集结果".into(),
                "对失败项定位并修复，回归通过后总结".into(),
            ],
            verification_rules: vec!["测试结果已记录".into(), "失败项有原因与修复".into()],
            resource_files: vec![],
            confidence: 0.85,
            enabled: true,
        },
        Skill {
            id: "sp-commit".into(),
            name: "Commit（Git 提交）".into(),
            version: "1.0".into(),
            trigger_boundary: "改动完成且验证通过、需要提交代码".into(),
            steps: vec![
                "检查 git status/diff，确认改动范围".into(),
                "编写符合规范的提交信息（类型+摘要+正文要点）".into(),
                "提交并核对提交结果".into(),
            ],
            verification_rules: vec!["提交信息规范".into(), "提交成功后返回 commit id".into()],
            resource_files: vec![],
            confidence: 0.8,
            enabled: true,
        },
        Skill {
            id: "sp-research".into(),
            name: "Research（调研分析）".into(),
            version: "1.0".into(),
            trigger_boundary: "需要调研技术方案、排查疑难问题、对比选项".into(),
            steps: vec![
                "明确调研问题与约束".into(),
                "搜集资料（文档/代码/命令输出），保留来源".into(),
                "归纳结论与建议，标注不确定项".into(),
            ],
            verification_rules: vec!["结论有依据来源".into(), "不确定项已标注".into()],
            resource_files: vec![],
            confidence: 0.8,
            enabled: true,
        },
    ]
}

/// 幂等注册内置 superpowers 技能到技能库：同名（id）已存在则跳过，不覆盖用户改动。
pub async fn ensure_builtin_skills(skill: &dyn SkillLibrary) -> usize {
    let mut added = 0usize;
    let existing: std::collections::HashSet<String> = skill
        .list_skills()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.id)
        .collect();
    for s in builtin_superpowers() {
        if existing.contains(&s.id) {
            continue;
        }
        if skill.register_skill(s).await.is_ok() {
            added += 1;
        }
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets_native::NativeSkillLibrary;

    const EXPECTED_IDS: [&str; 6] = [
        "sp-boot",
        "sp-plan",
        "sp-execute",
        "sp-test",
        "sp-commit",
        "sp-research",
    ];

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "harness-sp-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    /// 内置技能资产本身完备：6 个、id 唯一、默认启用、都带触发边界与验证规则。
    #[test]
    fn builtin_superpowers_has_six_enabled_skills() {
        let skills = builtin_superpowers();
        assert_eq!(skills.len(), 6);
        let ids: Vec<&str> = skills.iter().map(|s| s.id.as_str()).collect();
        for id in EXPECTED_IDS {
            assert!(ids.contains(&id), "缺少内置技能 {id}");
        }
        // id 唯一
        let unique: std::collections::HashSet<&str> = ids.iter().copied().collect();
        assert_eq!(unique.len(), 6);
        // 默认启用
        assert!(skills.iter().all(|s| s.enabled), "内置技能应默认启用");
        // 触发边界与验证规则不能为空（match_skills / verify_skill 依赖它们）
        assert!(
            skills.iter().all(|s| !s.trigger_boundary.is_empty()),
            "触发边界不能为空"
        );
        assert!(
            skills.iter().all(|s| !s.verification_rules.is_empty()),
            "验证规则不能为空"
        );
    }

    /// 端到端闭环：首次注册 6 个 → 落盘 json → 幂等（第二次 0 新增）→
    /// match_skills 能命中 → verify_skill 按规则打分。
    #[tokio::test]
    async fn ensure_builtin_skills_registers_idempotently_and_matches() {
        let dir = temp_dir("reg");
        let lib = NativeSkillLibrary::new(&dir);

        // 首次注册：6 个全部新增
        assert_eq!(ensure_builtin_skills(lib.as_ref()).await, 6);
        assert_eq!(lib.list_skills().await.unwrap().len(), 6);

        // 幂等：重复注册不新增、不覆盖
        assert_eq!(ensure_builtin_skills(lib.as_ref()).await, 0);

        // 落盘检查：<dir>/.harness-memory/skills 下应有 6 个 json
        let skill_dir = dir.join(".harness-memory").join("skills");
        let files: Vec<_> = std::fs::read_dir(&skill_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
            .collect();
        assert_eq!(files.len(), 6);

        // 匹配：规划类上下文应命中 sp-plan（词法打分，中文无分词，用短查询词验证）
        let matched = lib.match_skills("规划").await.unwrap();
        assert!(!matched.is_empty());
        assert_eq!(matched[0].id, "sp-plan");

        // 验证规则打分：outcome 全部命中规则 → 1.0
        let score = lib
            .verify_skill("sp-plan", "计划含步骤与验收标准，步骤间有依赖顺序")
            .await
            .unwrap();
        assert_eq!(score, 1.0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 回归守卫：真实自然语言中文输入必须能命中内置技能。
    /// 曾出现「占坑不拉」——技能已注册但 match_skills 因中文无空格分词问题
    /// 对真实句子永远匹配不到（返回空），技能对模型零影响。此测试确保修复不回归。
    #[tokio::test]
    async fn real_chinese_inputs_match_builtin_skills() {
        let dir = temp_dir("real");
        let lib = NativeSkillLibrary::new(&dir);
        ensure_builtin_skills(lib.as_ref()).await;

        // (输入, 期望命中的技能 id 之一)。中文长句必须能命中关键词技能。
        let cases: &[(&str, &[&str])] = &[
            ("请帮我规划一下这个任务", &["sp-plan"]),
            ("请先制定计划再执行", &["sp-plan", "sp-execute"]),
            ("测试一下这段代码", &["sp-test"]),
            ("帮我提交一下代码", &["sp-commit"]),
            ("调研一下这个方案", &["sp-research"]),
            ("开始吧", &["sp-boot"]),
        ];
        for (input, expect) in cases {
            let matched = lib.match_skills(input).await.unwrap();
            let ids: Vec<&str> = matched.iter().map(|s| s.id.as_str()).collect();
            assert!(
                !ids.is_empty(),
                "输入「{input}」未命中任何技能（占坑不拉回归）"
            );
            assert!(
                expect.iter().any(|e| ids.contains(e)),
                "输入「{input}」命中了 {ids:?}，但未包含期望 {expect:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
