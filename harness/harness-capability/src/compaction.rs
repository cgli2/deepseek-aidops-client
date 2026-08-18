use std::any::Any;

use async_trait::async_trait;

use harness_core::error::Result;
use harness_llm::Message;
use harness_session::SessionEvent;

/// 压缩能力定义（Definition）。将过长的会话事件压缩为模型上下文窗口内的消息序列。
#[async_trait]
pub trait Compaction: Any + Send + Sync {
    async fn compact(&self, events: Vec<SessionEvent>) -> Result<Vec<Message>>;
}
