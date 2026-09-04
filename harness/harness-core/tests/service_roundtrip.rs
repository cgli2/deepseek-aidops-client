//! 锁定 `AppContext::provide` / `get` 对「裸 trait object」服务的存取一致性。
//!
//! 关键不变量：`provide::<dyn Trait>(arc)` 与 `get::<dyn Trait>()` 的类型参数必须是同一个
//! 「裸 trait object」`dyn Trait`（而非 `Arc<dyn Trait>`，后者是 sized，TypeId 不同会 panic）。
//! 这是 GUI 通过 `Arc<dyn UiInputSink>` 注入反向输入通道、再在 `make_ui` 取出的同一机制，
//! 一旦不一致会在 `harness` 启动时 `ctx.get` 直接 panic。

use std::sync::Arc;

use harness_core::AppContext;
use harness_core::ui_input::UiInputSink;

struct DummySink {
    flag: bool,
}

impl UiInputSink for DummySink {
    fn submit(&self, _text: String) {}
    fn busy(&self) -> bool {
        self.flag
    }
}

#[test]
fn trait_object_service_roundtrip() {
    let ctx = AppContext::new();
    let sink: Arc<dyn UiInputSink> = Arc::new(DummySink { flag: true });
    // 以「裸 trait object」形式注册（与 harness-bin/compose.rs 一致）。
    // 注意：必须保留返回的 `Registration`，其 Drop 会从仓库移除该服务
    //（真实 `compose.rs` 里 `regs` 由 `ComposeGuard` 持有，生命周期覆盖整个运行）。
    let _reg = ctx.provide(sink);

    // 以相同「裸 trait object」取出 —— 若类型参数不一致会在此 panic。
    let got: Arc<dyn UiInputSink> = ctx.get::<dyn UiInputSink>();
    assert!(got.busy(), "取出的服务应是同一个，busy() 应返回 true");
}
