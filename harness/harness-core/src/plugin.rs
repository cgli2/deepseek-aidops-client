use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::context::{AppContext, Registration};

/// 插件抽象（dsh 的"一切皆插件"）。
///
/// - `deps()` 声明依赖的其它插件名，`compose_plugins` 据此做**拓扑排序**（等价 dsh `inject` 自动推导加载顺序）；
/// - `register()` 向 `ctx` 贡献服务（`ctx.provide`）与事件监听器（`ctx.events().on`），
///   二者均返回 `Registration`（可逆副作用，drop 即回滚，等价于 dsh `ctx.effect()`）；
/// - 换一个 Provider（如 `LocalBash` → `WasmShell`），Consumer 源码零改动（仅依赖 trait）。
pub trait Plugin: Send + Sync + 'static {
    fn name(&self) -> &'static str;

    /// 声明依赖的其它插件名；compose 据此拓扑排序。默认无依赖。
    fn deps(&self) -> &[&'static str] {
        &[]
    }

    /// 向 ctx 贡献服务与事件监听器；每个贡献都是可逆副作用，返回 `Registration` 集合。
    fn register(self: Arc<Self>, ctx: &AppContext) -> Vec<Registration>;
}

/// 组合守卫：持有所有 `Registration`。Drop 时逐个 drop = 卸载全部插件（不留残影）。
///
/// 不变量（完成文档 §8 不变量 3/5）：guard drop 后 `ctx.get::<S>()` 必须 fail，且事件订阅消失。
pub struct ComposeGuard {
    regs: Vec<Registration>,
}

impl ComposeGuard {
    pub fn new() -> Self {
        Self { regs: Vec::new() }
    }

    pub fn add(&mut self, r: Registration) {
        self.regs.push(r);
    }
}

impl Default for ComposeGuard {
    fn default() -> Self {
        Self::new()
    }
}

/// 按 `Plugin::deps()` 拓扑排序（Kahn 算法）。遇到环时退化为原顺序并忽略环边（不 panic）。
pub fn topo_sort(plugins: &[Arc<dyn Plugin>]) -> Vec<Arc<dyn Plugin>> {
    let by_name: HashMap<&'static str, Arc<dyn Plugin>> =
        plugins.iter().map(|p| (p.name(), p.clone())).collect();
    let mut ordered: Vec<Arc<dyn Plugin>> = Vec::new();
    let mut visited: HashSet<&'static str> = HashSet::new();
    let mut in_stack: HashSet<&'static str> = HashSet::new();
    for p in plugins {
        visit(p, &by_name, &mut visited, &mut in_stack, &mut ordered);
    }
    ordered
}

fn visit(
    p: &Arc<dyn Plugin>,
    by_name: &HashMap<&'static str, Arc<dyn Plugin>>,
    visited: &mut HashSet<&'static str>,
    in_stack: &mut HashSet<&'static str>,
    ordered: &mut Vec<Arc<dyn Plugin>>,
) {
    let n = p.name();
    if visited.contains(n) {
        return;
    }
    if in_stack.contains(n) {
        return; // 环：跳过
    }
    in_stack.insert(n);
    for d in p.deps() {
        if let Some(dp) = by_name.get(d) {
            visit(dp, by_name, visited, in_stack, ordered);
        }
    }
    in_stack.remove(n);
    visited.insert(n);
    ordered.push(p.clone());
}

/// 编译期组合入口（dsh 的 Cordis 组合层，原 §5.2）。
///
/// 等价于 dsh：按依赖顺序 register，每个 `effect` 收集进 guard。返回 `(ctx, guard)`；
/// guard 的生命周期即插件集合的生命周期。bin 的 `compose(profile)` 是此函数的特化。
pub fn compose_plugins(plugins: Vec<Arc<dyn Plugin>>) -> (AppContext, ComposeGuard) {
    let ordered = topo_sort(&plugins);
    let ctx = AppContext::new();
    let mut guard = ComposeGuard::new();
    for p in ordered {
        for r in p.register(&ctx) {
            guard.add(r);
        }
    }
    (ctx, guard)
}
