//! 项目文件预览：文件内容渲染、git diff 解析、文件树构建。
//!
//! 本模块为纯 UI 辅助逻辑（解析与数据结构），不依赖 egui——渲染由 `gui.rs` 调用。
//! 文件读取 / diff 生成经 `Arc<dyn Fs>` / `Arc<dyn Git>` 能力服务（只读查询，不触发 turn）。

/// 预览模式：源码 / Diff。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PreviewMode {
    Source,
    Diff,
}

/// 文件树节点（懒构建：目录的 `children` 在展开时填充）。
#[derive(Clone, Debug)]
pub struct FileTreeNode {
    pub name: String,
    /// 相对 workspace_root 的路径（根节点为空串）。
    pub path: String,
    pub is_dir: bool,
    /// git 有未提交变化（文件树据此显示色块标记）。
    pub dirty: bool,
    pub children: Vec<FileTreeNode>,
}

/// diff 行类型（unified 格式）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiffLineKind {
    Context,
    Add,
    Del,
    Hunk,
    Meta,
}

/// 解析后的 diff 行。
#[derive(Clone, Debug)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
}

/// 文件预览加载结果（经 mpsc 从独立 OS 线程回传）。
#[derive(Debug)]
pub struct PreviewLoadResult {
    pub content: harness_core::error::Result<String>,
    pub diff: Option<String>,
    pub tracked: bool,
}

/// 文件路径识别白名单扩展名（小写，不含点）。
pub const FILE_EXTS: &[&str] = &[
    "rs", "toml", "md", "txt", "json", "yaml", "yml", "js", "ts", "tsx", "jsx", "py", "go", "java",
    "c", "cpp", "h", "hpp", "sh", "bash", "zsh", "fish", "css", "html", "xml", "sql", "proto",
    "lock", "ini", "cfg", "conf", "env", "gitignore", "cmake",
];

/// 无扩展名的知名文件名（小写匹配）。
pub const KNOWN_FILENAMES: &[&str] = &[
    "dockerfile", "makefile", "rakefile", "gemfile", "license", "readme",
    "changelog", "authors", "contributors", "procfile",
];

/// 文件树构建时忽略的目录名（性能 + 噪声考量）。
pub const TREE_IGNORED_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".harness-memory",
    "dist",
    "build",
    ".next",
    ".nuxt",
    ".cache",
    ".turbo",
    "__pycache__",
    ".venv",
    "venv",
    ".idea",
    ".vscode",
];

/// 预览内容截断阈值（字节）。超过则只展示前 N 字节 + 提示。
pub const PREVIEW_MAX_BYTES: usize = 512 * 1024;

/// 解析 unified diff 文本为带类型标注的行列表。
///
/// 规则：
/// - `+++` / `---` → Meta（文件头）
/// - `@@` → Hunk（块范围）
/// - `+`（非 `+++`）→ Add
/// - `-`（非 `---`）→ Del
/// - 其余 → Context
pub fn parse_diff(diff: &str) -> Vec<DiffLine> {
    diff.lines()
        .map(|line| {
            let kind = if line.starts_with("+++") || line.starts_with("---") {
                DiffLineKind::Meta
            } else if line.starts_with("@@") {
                DiffLineKind::Hunk
            } else if line.starts_with('+') {
                DiffLineKind::Add
            } else if line.starts_with('-') {
                DiffLineKind::Del
            } else {
                DiffLineKind::Context
            };
            DiffLine {
                kind,
                text: line.to_string(),
            }
        })
        .collect()
}

/// 判断字符串是否「看起来像文件路径」（用于气泡内文件路径高亮可点击）。
///
/// 判定标准（全部满足）：
/// 1. 单行（不含换行）；
/// 2. 不含空格 / 制表符；
/// 3. 含点号且有扩展名；
/// 4. 扩展名在白名单 `FILE_EXTS` 中。
///
/// 不做磁盘 `exists()` 校验（避免 IO 阻塞渲染线程）；点击时若文件不存在，
/// 预览窗显示错误，不崩溃。
pub fn looks_like_file_path(s: &str) -> bool {
    if s.is_empty() || s.contains('\n') || s.contains('\r') {
        return false;
    }
    if s.contains(' ') || s.contains('\t') {
        return false;
    }
    // 无扩展名的知名文件名（如 Dockerfile、Makefile）。
    let basename = s.rsplit(['/', '\\']).next().unwrap_or(s);
    if KNOWN_FILENAMES.contains(&basename.to_lowercase().as_str()) {
        return true;
    }
    // 含路径分隔符或点号才算路径候选；纯单词（如 "hello"）不算。
    let has_sep = s.contains('/') || s.contains('\\');
    let dot_idx = match s.rfind('.') {
        Some(i) => i,
        None => return false,
    };
    let ext = s[dot_idx + 1..].to_lowercase();
    if !FILE_EXTS.contains(&ext.as_str()) {
        return false;
    }
    // 扩展名前必须有至少一个字符（`.rs` 不算合法路径）。
    if dot_idx == 0 && !has_sep {
        return false;
    }
    true
}

/// 检测字符串是否为二进制内容（含 NUL 字节）。
pub fn is_binary(content: &str) -> bool {
    content.contains('\0')
}

/// 截断超长文件内容，返回 (展示文本, 是否被截断)。
pub fn truncate_content(content: &str) -> (String, bool) {
    if content.len() <= PREVIEW_MAX_BYTES {
        return (content.to_string(), false);
    }
    // 按字节截断可能切断多字节字符，回退到最后一个字符边界。
    let mut end = PREVIEW_MAX_BYTES;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    (content[..end].to_string(), true)
}

/// 候选绝对路径：气泡/树中给定的路径可能相对不同基准根。
///
/// 场景：模型文本里写的路径通常相对「仓库根」（如 `harness/ui/src/gui.rs`），
/// 而 fs 沙箱根（Workspace）可能落在仓库子目录（如 exe 位于 `harness/dist` 时
/// 根是 `.../harness`）。直接 `root.join(path)` 会拼出 `.../harness/harness/...`。
/// 因此从沙箱根开始逐级向父目录尝试拼接，返回候选列表，调用方按序探测读取。
pub fn candidate_abs_paths(ws_root: &str, path: &str) -> Vec<std::path::PathBuf> {
    use std::path::{Path, PathBuf};
    let p = Path::new(path);
    if p.is_absolute() {
        return vec![p.to_path_buf()];
    }
    // 先定位仓库根：从 ws_root 向上找最近的含 .git 的祖先目录。
    // 找不到 .git 时仓库根 = ws_root 本身（候选只生成工作区内，绝不向上越界）。
    let mut repo_root = PathBuf::from(ws_root);
    let mut probe = PathBuf::from(ws_root);
    loop {
        if probe.join(".git").exists() {
            repo_root = probe;
            break;
        }
        match probe.parent() {
            Some(parent) if parent != probe && parent.parent().is_some() => {
                probe = parent.to_path_buf();
            }
            _ => break,
        }
    }

    // 从 ws_root 向上生成候选，直到仓库根为止。
    // 仓库根边界保证候选不会跑到 workspace 之外，避免探测到上级
    // 目录的同名文件后触发 SandboxDenied（如 `F:\workspace\preview.rs`）。
    let mut out = Vec::new();
    let mut cur = PathBuf::from(ws_root);
    loop {
        out.push(cur.join(p));
        if cur == repo_root {
            break;
        }
        match cur.parent() {
            Some(parent) if parent != cur && parent.parent().is_some() => {
                cur = parent.to_path_buf();
            }
            _ => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_diff_classifies_lines() {
        let diff = "\
--- a/foo.rs
+++ b/foo.rs
@@ -1,3 +1,4 @@
 fn main() {
+    println!(\"hi\");
 }
";
        let lines = parse_diff(diff);
        assert_eq!(lines.len(), 6);
        assert_eq!(lines[0].kind, DiffLineKind::Meta);
        assert!(lines[0].text.starts_with("---"));
        assert_eq!(lines[1].kind, DiffLineKind::Meta);
        assert_eq!(lines[2].kind, DiffLineKind::Hunk);
        assert_eq!(lines[3].kind, DiffLineKind::Context);
        assert_eq!(lines[4].kind, DiffLineKind::Add);
        assert_eq!(lines[5].kind, DiffLineKind::Context);
    }

    #[test]
    fn parse_diff_empty() {
        assert!(parse_diff("").is_empty());
    }

    #[test]
    fn looks_like_file_path_positive() {
        assert!(looks_like_file_path("src/gui.rs"));
        assert!(looks_like_file_path("docs/a.md"));
        assert!(looks_like_file_path("Cargo.toml"));
        assert!(looks_like_file_path("a/b/c.json"));
        assert!(looks_like_file_path("Dockerfile"));
    }

    #[test]
    fn looks_like_file_path_negative() {
        assert!(!looks_like_file_path("hello world"));
        assert!(!looks_like_file_path("foo"));
        assert!(!looks_like_file_path(""));
        assert!(!looks_like_file_path("a b/c.rs"));
        assert!(!looks_like_file_path("noext"));
        assert!(!looks_like_file_path(".rs"));
        assert!(!looks_like_file_path("readme.txt\nsecond line"));
    }

    #[test]
    fn is_binary_detects_nul() {
        assert!(is_binary("abc\0def"));
        assert!(!is_binary("普通文本"));
    }

    #[test]
    fn truncate_content_under_limit() {
        let s = "short".to_string();
        let (out, truncated) = truncate_content(&s);
        assert_eq!(out, "short");
        assert!(!truncated);
    }

    #[test]
    fn candidate_abs_paths_goes_up_to_git_root() {
        // 临时目录：tmp/repo/sub，其中 tmp/repo 是仓库根（含 .git）。
        let base = std::env::temp_dir().join(format!("harness-cand-{}", std::process::id()));
        let repo = base.join("repo");
        let sub = repo.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(repo.join(".git"), "").unwrap();

        let ws = sub.to_string_lossy().to_string();
        let cands = candidate_abs_paths(&ws, "src/gui.rs");
        // 只在 [sub, repo]（含 .git 的仓库根）范围内探测，不越出。
        assert_eq!(cands.len(), 2);
        assert_eq!(cands[0], sub.join("src/gui.rs"));
        assert_eq!(cands[1], repo.join("src/gui.rs"));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn candidate_abs_paths_no_git_stays_in_workspace() {
        // 无 .git 目录时只生成 ws_root 内的候选，不向上探测。
        let base = std::env::temp_dir().join(format!("harness-cand2-{}", std::process::id()));
        let sub = base.join("a").join("b");
        std::fs::create_dir_all(&sub).unwrap();

        let ws = sub.to_string_lossy().to_string();
        let cands = candidate_abs_paths(&ws, "x.rs");
        assert_eq!(cands, vec![sub.join("x.rs")]);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn candidate_abs_paths_absolute_passthrough() {
        use std::path::PathBuf;
        let cands = candidate_abs_paths("ignored", r"C:\abs\path.rs");
        assert_eq!(cands, vec![PathBuf::from(r"C:\abs\path.rs")]);
    }

    #[test]
    fn truncate_content_over_limit() {
        let s = "x".repeat(PREVIEW_MAX_BYTES + 100);
        let (out, truncated) = truncate_content(&s);
        assert!(truncated);
        assert!(out.len() <= PREVIEW_MAX_BYTES);
        assert!(out.chars().all(|c| c == 'x'));
    }
}
