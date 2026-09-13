//! Implementation workflow policy. ExecutionState owns delivery facts; GoalExecution
//! supplies the work breakdown only. A phase is guidance, never write authorization.
use crate::execution::{ExecutionState, StrategyKind};

pub(crate) fn owns(state: &ExecutionState) -> bool {
    matches!(state.strategy, StrategyKind::Transformative | StrategyKind::Generative)
}

pub(crate) fn tools(state: &ExecutionState) -> Vec<String> {
    let mut tools = vec!["search", "fs", "edit", "shell"];
    if state.contract.acceptance_criteria.len() > 1 {
        tools.push("plan");
    }
    tools.into_iter().map(str::to_owned).collect()
}

pub(crate) fn instructions(state: &ExecutionState) -> String {
    let route = if state.contract.acceptance_criteria.len() > 1 {
        "先分析目标、给出方案并拆成有依赖的实施步骤。逐项实施并验证，最后运行覆盖各项的集成验证，再整体交付。"
    } else {
        "这是一个实施任务：分析目标 → 定位最短调用链 → 确认解决办法 → 编辑落盘 → 验证结果。不要为小任务建立额外审批或大型计划。"
    };
    format!("[实施工作流]\n{route}\n用户的修复/实施请求已授权范围内的代码修改，无需再询问是否写入。阶段仅指导下一步；可以先运行复现命令，也可以在验证失败后继续读代码和编辑。保留用户已有改动，修改已有文件前读取内容；需要新建测试文件时直接创建。访问策略和外部副作用审批仍然有效。没有定位根因时继续必要取证，不能为满足写入计数乱改文件。只有实际改动和之后的相关验证才能支持完成声明。面向用户使用其语言，不能把内部缺失步骤当成用户需要补充的授权。")
}

pub(crate) fn next_action(state: &ExecutionState) -> &'static str {
    if state.write_operations > 0 {
        "继续完成尚未覆盖的修改，运行对应验证；若测试失败，修正代码后重新验证。"
    } else if state.write_attempts > 0 {
        "编辑尚未成功落盘：使用工具返回的具体错误修正编辑，再验证。不要询问用户是否允许修改。"
    } else {
        "用户已授权修复。根据已有证据确定根因并执行编辑，随后验证；缺少诊断证据时先运行最小复现，不要重复总结或重新泛搜。"
    }
}
