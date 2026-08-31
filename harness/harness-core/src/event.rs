use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::context::{Inner, Registration};

/// 类型化事件。`Output` 为该事件经 serial / waterfall 变换后的产物类型，
/// 默认各 `impl` 显式声明 `type Output = Self`（emit / parallel 仅观察，不改事件）。
/// 事件须 `Clone`（emit / parallel 逐监听器克隆）且 `'static`（跨任务传递）。
pub trait Event: Any + Send + Clone + 'static {
    type Output;
}

/// 观察者处理器（emit / parallel 桶）：`handle` 返回 `()`。
#[async_trait]
pub trait Handler<E: Event>: Any + Send + Sync {
    async fn handle(&self, e: E);
}

/// 串行处理器（serial 桶）：注册顺序执行，返回末值（事件或派生值）。
#[async_trait]
pub trait SerialHandler<E: Event>: Any + Send + Sync {
    async fn handle(&self, e: E) -> E::Output;
}

/// 瀑布处理器（waterfall 桶）：around-middleware。`next` 不调则短路（原 §5.4）。
pub trait Waterfall<E: Event>: Any + Send + Sync {
    fn call(&self, args: E, next: &dyn Fn(E) -> E::Output) -> E::Output;
}

// ---- 类型擦除容器：每事件类型一个具体（单态）泛型结构体，可经 `Arc<dyn Any>` downcast ----

pub(crate) struct EmitObj<E: Event> {
    pub h: Arc<dyn Handler<E>>,
}
pub(crate) struct SerialObj<E: Event> {
    pub h: Arc<dyn SerialHandler<E>>,
}
pub(crate) struct WaterfallObj<E: Event> {
    // 注册即入表（drop 时按 id 回滚）；`waterfall()` 的分发链由调用方
    // 经 `EventBusView::collect_waterfall` 从注册表取回后显式传入。
    pub h: Arc<dyn Waterfall<E>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Bucket {
    Emit,
    Parallel,
    Serial,
    Waterfall,
}
pub(crate) type SubId = u64;

/// 类型擦除的事件处理器表（按 `Event::TypeId` 分桶）。
pub(crate) struct HandlerTable {
    emit: Mutex<HashMap<TypeId, Vec<(SubId, Arc<dyn Any + Send + Sync>)>>>,
    parallel: Mutex<HashMap<TypeId, Vec<(SubId, Arc<dyn Any + Send + Sync>)>>>,
    serial: Mutex<HashMap<TypeId, Vec<(SubId, Arc<dyn Any + Send + Sync>)>>>,
    waterfall: Mutex<HashMap<TypeId, Vec<(SubId, Arc<dyn Any + Send + Sync>)>>>,
    next: AtomicU64,
}

impl HandlerTable {
    pub(crate) fn new() -> Self {
        Self {
            emit: Mutex::new(HashMap::new()),
            parallel: Mutex::new(HashMap::new()),
            serial: Mutex::new(HashMap::new()),
            waterfall: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
        }
    }

    /// 由父表派生子表：复制父表四个桶的全部订阅（内部处理器 `Arc` 共享，不克隆处理器本身），
    /// 使 fork 出的子上下文继承全局事件订阅（如插件经 `on_waterfall` 注入的 PreStep 中间件）。
    ///
    /// 子表拥有独立映射：子上下文后续新增/移除订阅只作用于自身，不污染父表。
    /// `next` 自增计数器从「父表最大订阅 id + 1」起步，避免与继承来的订阅 id 碰撞（碰撞会导致
    /// 子表 `remove` 误删父表继承来的订阅）。
    pub(crate) fn inherit_from(parent: &HandlerTable) -> Self {
        let clone_bucket = |b: &Mutex<HashMap<TypeId, Vec<(SubId, Arc<dyn Any + Send + Sync>)>>>| {
            b.lock()
                .map(|g| g.clone())
                .unwrap_or_default()
        };
        let emit = clone_bucket(&parent.emit);
        let parallel = clone_bucket(&parent.parallel);
        let serial = clone_bucket(&parent.serial);
        let waterfall = clone_bucket(&parent.waterfall);
        let max_id = [
            emit.values().flatten().map(|(id, _)| *id).max().unwrap_or(0),
            parallel.values().flatten().map(|(id, _)| *id).max().unwrap_or(0),
            serial.values().flatten().map(|(id, _)| *id).max().unwrap_or(0),
            waterfall.values().flatten().map(|(id, _)| *id).max().unwrap_or(0),
        ]
        .into_iter()
        .max()
        .unwrap_or(0);
        Self {
            emit: Mutex::new(emit),
            parallel: Mutex::new(parallel),
            serial: Mutex::new(serial),
            waterfall: Mutex::new(waterfall),
            next: AtomicU64::new(max_id + 1),
        }
    }

    fn bucket(
        &self,
        b: Bucket,
    ) -> &Mutex<HashMap<TypeId, Vec<(SubId, Arc<dyn Any + Send + Sync>)>>> {
        match b {
            Bucket::Emit => &self.emit,
            Bucket::Parallel => &self.parallel,
            Bucket::Serial => &self.serial,
            Bucket::Waterfall => &self.waterfall,
        }
    }

    pub(crate) fn add(&self, b: Bucket, etype: TypeId, obj: Arc<dyn Any + Send + Sync>) -> SubId {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.bucket(b)
            .lock()
            .unwrap()
            .entry(etype)
            .or_default()
            .push((id, obj));
        id
    }

    pub(crate) fn remove(&self, b: Bucket, etype: TypeId, id: SubId) {
        if let Some(v) = self.bucket(b).lock().unwrap().get_mut(&etype) {
            v.retain(|(i, _)| *i != id);
        }
    }

    /// 取出某事件类型在某桶下的所有处理器（downcast 回具体容器）。
    fn collect<E: Event, O: 'static + Send + Sync>(&self, b: Bucket) -> Vec<Arc<O>> {
        let g = self.bucket(b).lock().unwrap();
        g.get(&TypeId::of::<E>())
            .map(|l| {
                l.iter()
                    .filter_map(|(_, a)| a.clone().downcast::<O>().ok())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// 事件总线视图（dsh 类型化事件的四种分发）。UI 与工具管线均为其纯消费者。
#[derive(Clone)]
pub struct EventBusView {
    pub(crate) inner: Arc<Inner>,
}

impl EventBusView {
    // ---- 订阅（均返回可逆 Registration）----

    pub fn on<E: Event>(&self, h: Arc<dyn Handler<E>>) -> Registration {
        let etype = TypeId::of::<E>();
        let id = self
            .inner
            .handlers
            .add(Bucket::Emit, etype, Arc::new(EmitObj { h }));
        Registration::handler(Arc::downgrade(&self.inner), etype, Bucket::Emit, id)
    }

    pub fn on_parallel<E: Event>(&self, h: Arc<dyn Handler<E>>) -> Registration {
        let etype = TypeId::of::<E>();
        let id = self
            .inner
            .handlers
            .add(Bucket::Parallel, etype, Arc::new(EmitObj { h }));
        Registration::handler(Arc::downgrade(&self.inner), etype, Bucket::Parallel, id)
    }

    pub fn on_serial<E: Event>(&self, h: Arc<dyn SerialHandler<E>>) -> Registration {
        let etype = TypeId::of::<E>();
        let id = self
            .inner
            .handlers
            .add(Bucket::Serial, etype, Arc::new(SerialObj { h }));
        Registration::handler(Arc::downgrade(&self.inner), etype, Bucket::Serial, id)
    }

    pub fn on_waterfall<E: Event>(&self, h: Arc<dyn Waterfall<E>>) -> Registration {
        let etype = TypeId::of::<E>();
        let id = self
            .inner
            .handlers
            .add(Bucket::Waterfall, etype, Arc::new(WaterfallObj { h }));
        Registration::handler(Arc::downgrade(&self.inner), etype, Bucket::Waterfall, id)
    }

    /// 取回某事件类型在 waterfall 桶下的全部处理器（注册顺序）。
    ///
    /// 与 `on_waterfall` 对称：`waterfall()` 的分发链由调用方显式传入，
    /// 本方法把"注册表"与"分发点"打通——插件注册的中间件可被真正执行，
    /// 无注册时返回空 vec（分发退化为恒等，等价于旧行为）。
    pub fn collect_waterfall<E: Event>(&self) -> Vec<Arc<dyn Waterfall<E>>> {
        self.inner
            .handlers
            .collect::<E, WaterfallObj<E>>(Bucket::Waterfall)
            .into_iter()
            .map(|o| o.h.clone())
            .collect()
    }

    // ---- 分发 ----

    /// fire-and-forget：为每个监听器 spawn 任务。
    pub async fn emit<E: Event>(&self, e: E) {
        let objs: Vec<Arc<EmitObj<E>>> = self.inner.handlers.collect::<E, EmitObj<E>>(Bucket::Emit);
        for h in objs {
            let e = e.clone();
            let h = h.h.clone();
            tokio::spawn(async move {
                h.handle(e).await;
            });
        }
    }

    /// 并行观察：await 所有观察者完成。
    pub async fn parallel<E: Event>(&self, e: E) -> Vec<()> {
        let objs: Vec<Arc<EmitObj<E>>> = self
            .inner
            .handlers
            .collect::<E, EmitObj<E>>(Bucket::Parallel);
        let mut futs: Vec<Pin<Box<dyn std::future::Future<Output = ()> + Send>>> = Vec::new();
        for h in objs {
            let e = e.clone();
            let handler = h.h.clone();
            futs.push(Box::pin(async move { handler.handle(e).await }));
        }
        futures::future::join_all(futs).await
    }

    /// 串行：注册顺序执行，返回末值（无监听器时返回 `e` 本身）。
    /// 要求 `Output = E`，保证链上每个 handler 的输出能喂给下一个 handler。
    pub async fn serial<E: Event<Output = E>>(&self, e: E) -> E::Output {
        let objs: Vec<Arc<SerialObj<E>>> = self
            .inner
            .handlers
            .collect::<E, SerialObj<E>>(Bucket::Serial);
        let mut acc = e;
        for h in objs {
            acc = h.h.handle(acc).await;
        }
        acc
    }

    /// 瀑布（around-middleware，同步）：`next` 不调则短路；空链返回 `e`（终态恒等）。
    /// 要求 `Output = E`：`call` 入参与出参同型，链条类型一致。
    pub fn waterfall<E: Event<Output = E>>(
        &self,
        e: E,
        chain: &[Arc<dyn Waterfall<E>>],
    ) -> E::Output {
        fn run<E: Event<Output = E>>(e: E, chain: &[Arc<dyn Waterfall<E>>], i: usize) -> E::Output {
            if i >= chain.len() {
                return e;
            }
            let next = |e: E| run(e, chain, i + 1);
            chain[i].call(e, &next)
        }
        run(e, chain, 0)
    }
}
