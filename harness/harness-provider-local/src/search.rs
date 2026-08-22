use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use harness_capability::search::{Search, SearchHit, SearchRequest};
use harness_core::error::Result;
use harness_core::Workspace;

/// 本地搜索 Provider（实现 `Search`）：工作区内大小写不敏感子串扫描。
///
/// 设计约束（全部服务于“输出有界、耗时可控”）：
/// - 跳过构建产物/依赖目录与二进制文件，单文件 512KB 上限；
/// - 最多扫描 `MAX_FILES` 个文件、返回 `max_results` 条命中即停；
/// - 在 blocking 线程池执行，不阻塞事件循环。
pub struct LocalSearch {
    ws: Arc<Workspace>,
}

/// 跳过与工作区代码无关的大目录：构建产物、依赖、会话/记忆资产。
const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "dist",
    ".harness",
    ".harness-memory",
    ".workbuddy",
    ".qoder",
    ".vscode",
    "__pycache__",
];

/// 常见二进制扩展名：内容搜索对它们无意义，直接跳过。
const BINARY_EXTS: &[&str] = &[
    "exe", "dll", "so", "dylib", "pdb", "ico", "icns", "png", "jpg", "jpeg", "gif", "webp",
    "bmp", "pdf", "zip", "gz", "tar", "7z", "rar", "wasm", "bin", "ttf", "otf", "woff",
    "woff2", "mp3", "mp4", "pyc", "class", "jar",
];

/// 单次搜索最多扫描的文件数：兜底超大仓库，避免一次调用变成全仓 I/O。
const MAX_FILES: usize = 20_000;
/// 单文件大小上限：超过的多半是生成物/数据文件。
const MAX_FILE_BYTES: u64 = 512 * 1024;

impl LocalSearch {
    pub fn new(root: PathBuf) -> Arc<dyn Search> {
        Self::with_workspace(Workspace::new(root))
    }

    /// 共享外部 Workspace（GUI 项目切换可动态换根）。
    pub fn with_workspace(ws: Arc<Workspace>) -> Arc<dyn Search> {
        Arc::new(Self { ws })
    }
}

#[async_trait]
impl Search for LocalSearch {
    async fn grep(&self, req: SearchRequest) -> Result<Vec<SearchHit>> {
        if req.pattern.trim().is_empty() {
            return Err(harness_core::error::Error::Runtime(
                "search.pattern 不能为空".into(),
            ));
        }
        let root = self.ws.root();
        let base = match &req.dir {
            None => root.clone(),
            Some(d) if d.is_absolute() => {
                let canon_root = root.canonicalize().unwrap_or_else(|_| root.clone());
                let canon = d.canonicalize().map_err(|e| {
                    harness_core::error::Error::Runtime(format!("search dir 不存在: {e}"))
                })?;
                if !canon.starts_with(&canon_root) {
                    return Err(harness_core::error::Error::SandboxDenied(format!(
                        "search dir is outside workspace: {}",
                        d.display()
                    )));
                }
                canon
            }
            Some(d) => {
                if d.components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
                {
                    return Err(harness_core::error::Error::SandboxDenied(
                        "parent traversal is not allowed".into(),
                    ));
                }
                root.join(d)
            }
        };
        let pattern = req.pattern.trim().to_lowercase();
        let max = req.max_results.clamp(1, 200);
        let joined = tokio::task::spawn_blocking(move || scan(&base, &root, &pattern, max))
            .await
            .map_err(|e| harness_core::error::Error::Runtime(format!("search task panicked: {e}")))?;
        Ok(joined?)
    }
}

/// 同步扫描主体（运行于 blocking 线程池）。命中路径相对 `root` 输出。
fn scan(base: &Path, root: &Path, pattern: &str, max: usize) -> Result<Vec<SearchHit>> {
    let mut hits: Vec<SearchHit> = Vec::new();
    let mut files = 0usize;
    let mut stack: Vec<PathBuf> = vec![base.to_path_buf()];
    'outer: while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            if hits.len() >= max {
                break 'outer;
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if !SKIP_DIRS.contains(&name.as_str()) && !name.starts_with('.') {
                    stack.push(path);
                }
                continue;
            }
            files += 1;
            if files > MAX_FILES {
                break 'outer;
            }
            if is_binary_name(&name) {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.len() > MAX_FILE_BYTES {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            // 前 8KB 内含 NUL 视为二进制。
            if bytes.iter().take(8_192).any(|&b| b == 0) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            for (index, line) in text.lines().enumerate() {
                if line.to_lowercase().contains(pattern) {
                    let rel = path.strip_prefix(root).unwrap_or(&path);
                    let trimmed: String = line.trim().chars().take(300).collect();
                    hits.push(SearchHit {
                        path: rel.to_path_buf(),
                        line: (index + 1) as u32,
                        text: trimmed,
                    });
                    if hits.len() >= max {
                        break 'outer;
                    }
                }
            }
        }
    }
    Ok(hits)
}

fn is_binary_name(name: &str) -> bool {
    let Some(dot) = name.rfind('.') else {
        return false;
    };
    let ext = name[dot + 1..].to_ascii_lowercase();
    BINARY_EXTS.contains(&ext.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn finds_matches_and_bounds_results() {
        let root = std::env::temp_dir().join(format!("harness-search-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "fn needle_one() {}\nfn needle_two() {}\n").unwrap();
        std::fs::write(root.join("src/b.rs"), "// NEEDLE_ONE again\n").unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("target/skip.rs"), "needle_one should be skipped\n").unwrap();

        let search = LocalSearch::new(root.clone());
        let hits = search
            .grep(SearchRequest {
                pattern: "needle_one".into(),
                dir: None,
                max_results: 10,
            })
            .await
            .unwrap();
        // 大小写不敏感；target/ 被跳过：a.rs 1 条 + b.rs 1 条。
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|h| !h.path.starts_with("target")));

        // max_results 生效。
        let bounded = search
            .grep(SearchRequest {
                pattern: "needle".into(),
                dir: None,
                max_results: 1,
            })
            .await
            .unwrap();
        assert_eq!(bounded.len(), 1);

        let _ = std::fs::remove_dir_all(&root);
    }
}
