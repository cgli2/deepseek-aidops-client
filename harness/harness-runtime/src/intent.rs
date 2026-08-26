//! 把用户描述编译成可执行的任务形状，避免 Agent 仅凭模糊关键词决定是否泛搜。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentKind {
    AtomicRegression,
    ScopedChange,
    Investigation,
    OpenEnded,
}

#[derive(Debug, Clone)]
pub struct IntentProfile {
    pub kind: IntentKind,
    pub has_state_transition: bool,
    pub has_stale_observation: bool,
    pub has_before_after: bool,
}

impl IntentProfile {
    pub fn compile(input: &str) -> Self {
        let has_before_after = [
            "之前", "原来", "加了", "之后", "不再", "失效", "不生效", "没有变化", "回归",
        ]
        .iter()
        .any(|word| input.contains(word));
        let has_state_transition = ["删除", "添加", "新增", "保存", "更新", "切换", "重置"]
            .iter()
            .any(|word| input.contains(word));
        let has_stale_observation = ["旧", "还是", "未刷新", "不同步", "残留", "未更新", "不及时"]
            .iter()
            .any(|word| input.contains(word));
        let kind = if has_before_after || (has_state_transition && has_stale_observation) {
            IntentKind::AtomicRegression
        } else if ["排查", "调查", "为什么", "根因", "诊断"]
            .iter()
            .any(|word| input.contains(word))
        {
            IntentKind::Investigation
        } else if ["修改", "修复", "更新", "改进", "实现", "改造", "调整"]
            .iter()
            .any(|word| input.contains(word))
        {
            IntentKind::ScopedChange
        } else {
            IntentKind::OpenEnded
        };
        Self { kind, has_state_transition, has_stale_observation, has_before_after }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutation_followed_by_stale_view_is_atomic() {
        let profile = IntentProfile::compile("删除再添加配置后，下拉框还是旧状态，需同步刷新");
        assert_eq!(profile.kind, IntentKind::AtomicRegression);
    }
}
