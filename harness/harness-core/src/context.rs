use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::{Arc, RwLock, Weak};

/// 服务标记 trait。任何 `Any + Send + Sync + 'static` 的类型都自动是 `Service`，
/// 因此 `dyn Shell` / `dyn LlmProvider` 等 trait 对象也满足 `Service`，可作为服务注册。
pub trait Service: Any + Send + Sync + 'static {}
impl<T: ?Sized + Any + Send + Sync + 'static> Service for T {}

/// 服务存储单元：把（可能非 Sized 的）trait object 服务 `Arc<X>` 包进一个 **Sized** 的单元，
/// 从而既能以 `TypeId::of::<ServiceCell<X>>()` 作为键，又能 `downcast::<ServiceCell<X>>()`
/// 取回（`Any::downcast` 只接受 Sized 目标）。`get::<X>()` 取出后 `.inner` 即 `Arc<X>`。
///
/// `ServiceCell<X>` 本身是 Sized + `'static`，因此自动满足 blanket `Service`（从而也是
/// `Any + Send + Sync`，可存入 `Arc<dyn Any + Send + Sync>` 的 TypeMap）。
pub(crate) struct ServiceCell<X: ?Sized + Service> {
    pub inner: Arc<X>,
}

/// 微内核内部状态。用 `Arc` 持有以便 `AppContext` 廉价 `Clone` 并 move 进 `tokio::spawn`（完成文档 §3 偏差说明）。
pub(crate) struct Inner {
    pub(crate) services: RwLock<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
    pub(crate) handlers: crate::event::HandlerTable,
}

/// 微内核上下文（dsh 的 Cordis Context / `ctx.<key>` 服务仓库）。
///
/// 设计文档原签名 `provide(&mut self)`，本实现有意改为 `&self`（内部 `Arc<RwLock>` 可变性），
/// 使 `AppContext: Clone`，可 move 进异步任务（完成文档 §3 / §12 偏差表）。
#[derive(Clone)]
pub struct AppContext {
    pub(crate) inner: Arc<Inner>,
}

impl Default for AppContext {
    fn default() -> Self {
        Self::new()
    }
}

impl AppContext {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                services: RwLock::new(HashMap::new()),
                handlers: crate::event::HandlerTable::new(),
            }),
        }
    }

    /// 派生一个隔离子上下文：复用已有 Provider 的 `Arc`，但服务表与事件订阅表独立。
    /// 子代理可在其中覆盖 `SessionLog` 等会话级服务而不污染父会话。
    ///
    /// 关键修复：事件订阅表（emit / parallel / serial / waterfall 四个桶）必须**继承父上下文**
    /// 的注册，否则通过 `on_waterfall`/`on_serial` 注入的全局插件中间件
    /// （如 Trellis 的 `PreStep` 规格注入）在 fork 出的会话上下文中不可见，
    /// 导致 `collect_waterfall::<PreStep>()` 永远返回空链、插件对 agent 工作流完全失效。
    /// 服务表仍独立克隆，子上下文可覆盖会话级服务而不影响父上下文。
    pub fn fork(&self) -> Self {
        let services = self
            .inner
            .services
            .read()
            .expect("AppContext services RwLock poisoned")
            .clone();
        let handlers = crate::event::HandlerTable::inherit_from(&self.inner.handlers);
        Self {
            inner: Arc::new(Inner {
                services: RwLock::new(services),
                handlers,
            }),
        }
    }

    /// 注册服务，返回 RAII `Registration`；Drop 时自动从 TypeMap 移除（可逆注册 = dsh `effect()`）。
    ///
    /// 注册 trait 对象服务时，以 trait 作为类型参数：`ctx.provide(local as Arc<dyn Shell>)`，
    /// 这样 `get::<dyn Shell>()` 取到的是 Provider 无关的接口（Consumer 永不直接依赖具体 Provider）。
    /// 内部以 `ServiceCell<X>` 作为键与存储单元，使 `X` 可为非 Sized 的 trait object。
    pub fn provide<X: ?Sized + Service>(&self, s: Arc<X>) -> Registration {
        let tid = TypeId::of::<ServiceCell<X>>();
        let cell: Arc<dyn Any + Send + Sync> = Arc::new(ServiceCell { inner: s });
        self.inner
            .services
            .write()
            .expect("AppContext services RwLock poisoned")
            .insert(tid, cell);
        Registration::service(Arc::downgrade(&self.inner), tid)
    }

    /// 取服务；未注册则 panic（结构性保证：Consumer 必被满足）。取 trait 对象：`ctx.get::<dyn Shell>()`。
    pub fn get<X: ?Sized + Service>(&self) -> Arc<X> {
        let tid = TypeId::of::<ServiceCell<X>>();
        self.inner
            .services
            .read()
            .expect("AppContext services RwLock poisoned")
            .get(&tid)
            .and_then(|a| a.clone().downcast::<ServiceCell<X>>().ok())
            .map(|cell| cell.inner.clone())
            .unwrap_or_else(|| panic!("service {} not registered", std::any::type_name::<X>()))
    }

    pub fn try_get<X: ?Sized + Service>(&self) -> Option<Arc<X>> {
        let tid = TypeId::of::<ServiceCell<X>>();
        self.inner
            .services
            .read()
            .ok()?
            .get(&tid)
            .and_then(|a| a.clone().downcast::<ServiceCell<X>>().ok())
            .map(|cell| cell.inner.clone())
    }

    /// 取事件总线视图（纯消费者接口）。
    pub fn events(&self) -> crate::event::EventBusView {
        crate::event::EventBusView {
            inner: self.inner.clone(),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn inner_weak(&self) -> Weak<Inner> {
        Arc::downgrade(&self.inner)
    }
}

/// 可逆注册句柄。服务注册与事件订阅共用此类型；Drop 即回滚（dsh 的 `effect()` 自动回滚）。
///
/// 判定标准（完成文档 §8 不变量 3/5）：`ComposeGuard` 持有的所有 `Registration` drop 后，
/// `ctx.get::<S>()` 必须失败，且对应事件订阅消失。
pub struct Registration {
    pub(crate) kind: RegistrationKind,
}

pub(crate) enum RegistrationKind {
    Service {
        inner: Weak<Inner>,
        tid: TypeId,
    },
    Handler {
        inner: Weak<Inner>,
        etype: TypeId,
        bucket: crate::event::Bucket,
        id: crate::event::SubId,
    },
}

impl Registration {
    pub(crate) fn service(inner: Weak<Inner>, tid: TypeId) -> Self {
        Self {
            kind: RegistrationKind::Service { inner, tid },
        }
    }

    pub(crate) fn handler(
        inner: Weak<Inner>,
        etype: TypeId,
        bucket: crate::event::Bucket,
        id: crate::event::SubId,
    ) -> Self {
        Self {
            kind: RegistrationKind::Handler {
                inner,
                etype,
                bucket,
                id,
            },
        }
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        match &self.kind {
            RegistrationKind::Service { inner, tid } => {
                if let Some(i) = inner.upgrade() {
                    // 退出路径防御：锁中毒（先前 panic 遗留）时恢复而非二次 panic → abort；
                    // 且先释放写锁再 drop 被移除的服务（可能是持有 runtime 的重对象，
                    // 持锁期间析构会放大任何析构异常的影响面）。
                    let removed = i
                        .services
                        .write()
                        .map(|mut g| g.remove(tid))
                        .unwrap_or_else(|e| e.into_inner().remove(tid));
                    drop(removed);
                }
            }
            RegistrationKind::Handler {
                inner,
                etype,
                bucket,
                id,
            } => {
                if let Some(i) = inner.upgrade() {
                    i.handlers.remove(*bucket, *etype, *id);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, Waterfall};
    use std::sync::Arc;

    #[derive(Clone)]
    struct TestEv {
        v: u32,
    }
    impl Event for TestEv {
        type Output = Self;
    }
    struct Doubler;
    impl Waterfall<TestEv> for Doubler {
        fn call(&self, args: TestEv, next: &dyn Fn(TestEv) -> TestEv) -> TestEv {
            next(TestEv { v: args.v * 2 })
        }
    }

    /// 回归测试：fork 出的子上下文必须继承父上下文的 waterfall 订阅，
    /// 否则 Trellis 等插件的 PreStep 中间件在会话 fork 上下文中不可见、对 agent 完全失效。
    #[test]
    fn fork_inherits_waterfall_handlers() {
        let ctx = AppContext::new();
        // 绑定 Registration 以保持订阅存活（RAII，drop 即注销）。
        let _reg = ctx.events().on_waterfall::<TestEv>(Arc::new(Doubler));

        // 父上下文本身应可见该中间件。
        let parent_chain = ctx.events().collect_waterfall::<TestEv>();
        assert_eq!(parent_chain.len(), 1, "父上下文应注册 1 个 waterfall 中间件");
        let out = ctx
            .events()
            .waterfall(TestEv { v: 1 }, &parent_chain);
        assert_eq!(out.v, 2);

        // fork 出的子上下文必须同样可见该中间件（核心修复点）。
        let child = ctx.fork();
        let child_chain = child.events().collect_waterfall::<TestEv>();
        assert_eq!(
            child_chain.len(),
            1,
            "fork 必须继承父上下文的 waterfall 订阅，否则会话内插件中间件失效"
        );
        let out2 = child.events().waterfall(TestEv { v: 1 }, &child_chain);
        assert_eq!(out2.v, 2);

        // 子上下文的新增订阅不应污染父上下文。
        let _reg2 = child.events().on_waterfall::<TestEv>(Arc::new(Doubler));
        assert_eq!(child.events().collect_waterfall::<TestEv>().len(), 2);
        assert_eq!(ctx.events().collect_waterfall::<TestEv>().len(), 1);
    }
}
