use std::sync::Arc;

use async_stream::stream;
use async_trait::async_trait;

use crate::{Chunk, ChunkStream, LlmProvider, Message, ToolSchema};

/// 回放 Provider：流式产出预录的 `Chunk`，用于 headless / CI / 测试闭环（原 §5.6 / M1）。
///
/// 运行时不变量（完成文档 §8 不变量 1）：到达模型的输入必有对应日志事件——
/// `ReplayLlm` 的输入由 `SessionLog::replay()` 重建，不经过真实网络。
pub struct ReplayLlm {
    chunks: Vec<Chunk>,
    tools: Vec<ToolSchema>,
}

impl ReplayLlm {
    /// 用预录分片构造一个 `Arc<dyn LlmProvider>`。
    pub fn new(chunks: Vec<Chunk>) -> Arc<dyn LlmProvider> {
        Arc::new(Self {
            chunks,
            tools: vec![],
        })
    }

    /// 从预录分片构造（M1 的 JSONL fixture 加载走此入口）。
    pub fn from_chunks(chunks: Vec<Chunk>) -> Arc<dyn LlmProvider> {
        Self::new(chunks)
    }
}

#[async_trait]
impl LlmProvider for ReplayLlm {
    fn name(&self) -> &'static str {
        "replay"
    }

    fn tools(&self) -> Vec<ToolSchema> {
        self.tools.clone()
    }

    fn stream(&self, _msgs: Vec<Message>) -> ChunkStream {
        let chunks = self.chunks.clone();
        Box::pin(stream! {
            for c in chunks {
                yield Ok(c);
            }
        })
    }
}
