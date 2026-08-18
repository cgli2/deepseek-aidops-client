use harness_core::event::Event;
use harness_llm::Message;

/// 工具管线前的水瀑布事件（around-middleware：重写 / 拒绝消息，原 §5.4 / §5.6）。
///
/// 监听器拿到 `(PreStep, next)`，调用 `next()` 委托，不调则短路。
#[derive(Clone)]
pub struct PreStep {
    pub input: Vec<Message>,
}

impl Event for PreStep {
    type Output = Self;
}

/// 唯一串行终止检查点（无 `next()`，原 §5.6）。`serial` 分发，返回末值。
#[derive(Clone)]
pub struct TurnStopping {
    pub will_stop: bool,
}

impl Event for TurnStopping {
    type Output = Self;
}
