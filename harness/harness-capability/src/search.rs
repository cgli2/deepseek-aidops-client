use std::any::Any;
use std::path::PathBuf;

use async_trait::async_trait;

use harness_core::error::Result;

/// 内容搜索请求：`pattern` 为大小写不敏感的子串匹配；
/// `dir` 限定相对子目录；`max_results` 限制命中条数，防止输出爆炸撑大上下文。
#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub pattern: String,
    pub dir: Option<PathBuf>,
    pub max_results: usize,
}

/// 单条命中：`path` 为相对工作区根的路径（紧凑、可直接回读）。
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub path: PathBuf,
    pub line: u32,
    pub text: String,
}

/// 搜索能力定义（Definition）。Provider：`LocalSearch`。
///
/// 存在动机（取证）：没有专用搜索通道时，模型会用 shell 全仓 findstr + 自造
/// 临时扫描脚本定位代码（单回合 758 次 shell、100+ 个临时脚本），
/// 是步数失控的最大来源。本能力提供输出有界的一次性定位手段。
#[async_trait]
pub trait Search: Any + Send + Sync {
    async fn grep(&self, req: SearchRequest) -> Result<Vec<SearchHit>>;
}
