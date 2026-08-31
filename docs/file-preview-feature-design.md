# 项目文件预览功能设计文档

> 版本：v1.0（设计稿，待评审）
> 日期：2026-08-19
> 作者：harness-ui 维护者
> 关联文档：`docs/architecture.md`（分层与运行时数据流）、`docs/system-design-completion.md`（能力接缝三角色、不变量）

---

## 0. 摘要（结论先行）

本设计在**不破坏现有分层与不变量**的前提下，为 `harness-ui`（egui GUI）新增「项目文件预览」能力，整体参照 Codex 的三栏布局与交互范式：

1. **右侧分栏预览窗**：点击文件 → 右侧 `SidePanel::right` 展示文件内容；可关闭恢复主窗口全宽。
2. **对话气泡内文件高亮可点击**：助手消息中出现的文件路径（代码块 / 行内代码 / 普通文本）渲染为可点击 chip，点击即打开预览。
3. **Git 托管文件的 diff 视图**：若文件受 git 跟踪且有未提交修改，预览窗顶部提供「源码 / Diff」切换；Diff 模式以 `+/-` 着色行渲染 `git diff` 输出。
4. **最右侧项目文件树**：`SidePanel::right` 的文件树目录结构，右上角图标按钮可开可关。

所有文件读取复用既有 `Arc<dyn Fs>` 能力（`LocalFs`，沙箱根 = `Workspace`），diff 复用既有 `Arc<dyn Git>` 能力（`GitCli`）。**UI 仍是事件总线的纯消费者**，预览状态为纯 UI 本地状态，不回写核心循环、不写入 `SessionLog`，符合不变量 3。

---

## 1. 背景与目标

### 1.1 现状

当前 `harness-ui/src/gui.rs`（约 3200 行）布局为：

```
┌─────────────────────────────────────────────────┐
│ SidePanel::left("nav")  │  CentralPanel("main") │
│  侧栏导航 + 项目 + 历史  │  头部 + 消息流 + 输入  │
│  (220px / 56px)         │  (剩余宽度)            │
└─────────────────────────────────────────────────┘
```

- 消息流（`ScrollArea::vertical`）渲染 `Vec<ChatMsg>`，助手气泡经 `crate::markdown::to_job` 做 Markdown 富文本渲染。
- 已有能力接缝：`Fs`（read/write/list）、`Git`（status/diff/commit/branch/worktree），Provider 已在 `compose.rs` 注册。
- `AppState` 持有 `host: Arc<EguiUi>`，`EguiUi` 持有 `workspace_root`、`settings` 等，但**当前未持有 `Arc<dyn Fs>` / `Arc<dyn Git>`**——这是本设计需要补齐的装配点。

### 1.2 目标（对齐需求四条）

| # | 需求 | 设计落点 |
|---|------|----------|
| R1 | 点击文件 → 右侧分栏预览；可关闭恢复主窗口 | `SidePanel::right("preview")` + `preview_open: bool` |
| R2 | 对话主窗口列出的文件高亮可点击查看 | `markdown.rs` 渲染时把文件路径 token 标记为可点击；`AppState` 记录点击事件 |
| R3 | git 托管文件用 git diff 方式展示修改 | 预览窗「源码 / Diff」切换；Diff 复用 `Git::diff` 或按文件路径取 `git diff -- <path>` |
| R4 | 最右侧文件树目录结构，右上角图标可开可关 | `SidePanel::right("tree")`（在预览窗更右侧或作为预览窗的一个 tab）；`tree_open: bool` + 头部图标按钮 |

### 1.3 非目标

- 不做文件编辑（预览只读；编辑仍走 `EditTool` / `Editor` 能力，由模型驱动）。
- 不做 LSP 语义高亮（M+ 扩展，当前用等宽纯文本 + Markdown 代码块着色）。
- 不把预览状态持久化进 `SessionLog`（纯 UI 状态，重启不恢复预览窗）。
- 不引入新的外部依赖（复用 egui 既有控件；diff 解析自写轻量解析器）。

---

## 2. 架构定位（落在哪一层）

参照 `docs/architecture.md` 的分层：

```
L6 · harness-ui（表现层）  ← 本功能全部落点
L4 · harness-tool（Consumer，已有 FsTool/Git 不改）
L3 · harness-provider-local / harness-provider-git（Provider，不改）
L2 · harness-capability（Fs/Git trait，不改）
```

**关键约束**：UI 只消费能力，不反向调用核心循环。本功能在 UI 层新增：

- `EguiUi` 注入 `Arc<dyn Fs>` + `Arc<dyn Git>`（从 `AppContext` 取，与 `UiInputSink` 同路径）。
- `AppState` 新增预览相关字段（纯本地状态）。
- `markdown.rs` 扩展：文件路径 token 标记 + 点击回调。
- 新模块 `preview.rs`：文件内容渲染、diff 解析、文件树构建。

**不变量影响**：

| 不变量 | 影响 | 说明 |
|--------|------|------|
| 1 会话日志是真相源 | ✅ 无影响 | 预览状态不写入 `SessionLog`；文件内容来自 `Fs::read`，是工作区实时快照，非会话历史 |
| 2 Consumer 永不直接依赖 Provider | ✅ 无影响 | UI 经 `Arc<dyn Fs>` / `Arc<dyn Git>` 消费，与 `FsTool` 同构 |
| 3 UI 是事件总线纯消费者 | ✅ 无影响 | 预览交互不触发 agent turn、不回写核心；`Fs::read` / `Git::diff` 是只读查询，不产生 `SessionEvent` |

> 注：`Fs::read` / `Git::diff` 是同步只读能力调用，不产生会话事件，不违反「UI 不反向调用核心循环」——这与现有记忆面板（`refresh_mem` 经 `host.rt` 查询资产服务）同一模式：UI 侧只读查询能力服务，不驱动 turn。

---

## 3. 布局设计（参照 Codex）

### 3.1 三栏 + 可折叠右栏

```
┌──────────┬────────────────────────────┬─────────────┬──────────┐
│ nav      │ main (消息流 + 输入)        │ preview      │ tree     │
│ 侧栏导航  │  头部 [📁树] [◐主题]        │  文件内容     │ 文件树    │
│ 项目/历史 │  气泡（文件路径可点击）     │  [源码|Diff]  │ 目录结构  │
│          │  输入区                     │  ✕关闭       │ ✕关闭    │
│ 220px    │  flex                      │ 320~480px   │ 240px    │
└──────────┴────────────────────────────┴─────────────┴──────────┘
```

- **preview 窗**：`egui::SidePanel::right("preview")`，宽度 `resizable`，默认 380px，范围 [320, 600]。
- **tree 窗**：`egui::SidePanel::right("tree")`，在 preview 更右侧；默认 240px，范围 [180, 360]。
- 两窗独立开关：`preview_open` / `tree_open`。关闭时 `SidePanel` 不渲染，主区自动占满。
- egui 的 `SidePanel::right` 按声明顺序从右往左叠放：先声明的在最右。因此 `update()` 中声明顺序为 `tree` → `preview` → `main`（CentralPanel 最后，自动填充剩余）。

### 3.2 开关入口

| 窗 | 开启入口 | 关闭入口 |
|----|----------|----------|
| preview | 气泡内文件路径点击；文件树点击文件 | 预览窗右上角 ✕ 按钮 |
| tree | 主区头部「📁」图标按钮（`Icon::FolderTree` 新增）；侧栏导航可加「文件树」项 | 树窗右上角 ✕ 按钮 |

头部图标按钮位置：现有头部 `right_to_left` 布局中，在主题切换按钮左侧新增一个文件夹树 toggle 图标。

### 3.3 状态字段（AppState 新增）

```rust
struct AppState {
    // ... 既有字段 ...

    // ── 文件预览（纯 UI 本地状态，不持久化、不进 SessionLog）──
    /// 预览窗是否展开。
    preview_open: bool,
    /// 当前预览的文件相对路径（相对 workspace_root）。
    preview_path: Option<String>,
    /// 预览窗内容缓存：避免每帧重读磁盘。
    preview_content: Option<String>,
    /// 预览模式：源码 / Diff。
    preview_mode: PreviewMode,
    /// diff 文本缓存（切换到 Diff 模式时按需加载）。
    preview_diff: Option<String>,
    /// 文件是否受 git 跟踪（决定是否显示 Diff tab）。
    preview_tracked: bool,
    /// 预览加载错误信息（文件不存在 / 超大 / 二进制）。
    preview_error: Option<String>,

    // ── 文件树 ──
    tree_open: bool,
    /// 文件树根节点（懒构建，项目切换 / 外部修改时刷新）。
    tree_root: Option<FileTreeNode>,
    /// 文件树展开路径集合（相对路径 → 是否展开）。
    tree_expanded: std::collections::HashSet<String>,
    /// 文件树上次刷新时间（用于轮询刷新节流）。
    tree_last_refresh: Option<std::time::Instant>,
}

#[derive(Clone, Copy, PartialEq)]
enum PreviewMode {
    Source,
    Diff,
}

#[derive(Clone)]
struct FileTreeNode {
    name: String,
    path: String,       // 相对 workspace_root
    is_dir: bool,
    children: Vec<FileTreeNode>,  // 目录才有，懒填充
}
```

---

## 4. 能力装配（compose.rs 改动）

### 4.1 EguiUi 注入 Fs / Git

`EguiUi::new` 签名新增两个参数（与既有 `conv/skill/wiki/code` 注入同模式）：

```rust
pub fn new(
    sink: Arc<dyn UiInputSink>,
    llm_control: Arc<dyn LlmControl>,
    workspace_root: String,
    provider: String,
    base_url: String,
    model: String,
    settings: Arc<crate::SettingsDb>,
    conv: Arc<dyn ConversationMemory>,
    skill: Arc<dyn SkillLibrary>,
    wiki: Arc<dyn WikiStore>,
    code: Arc<dyn CodeGraph>,
    wasm_plugins: Arc<harness_provider_wasm::WasmPluginRuntime>,
    fs: Arc<dyn Fs>,      // ← 新增
    git: Arc<dyn Git>,    // ← 新增
) -> Self
```

### 4.2 compose.rs make_ui 取服务

`make_ui` 已有 `ctx: &AppContext`，新增：

```rust
let fs: Arc<dyn Fs> = ctx.get::<dyn Fs>();
let git: Arc<dyn Git> = ctx.get::<dyn Git>();
```

这两个服务在 `HarnessPlugin::register` 中已 `ctx.provide`（见 `compose.rs` 现有代码：`let fs: Arc<dyn Fs> = LocalFs::with_workspace(...)` / `let git: Arc<dyn Git> = GitCli::new(...)`），**Provider 侧零改动**。

### 4.3 异步查询模式（复用既有 UiRuntime）

文件读取 / diff 生成是异步 IO（`Fs::read` / `Git::diff` 是 `async_trait`）。为避免 GUI 线程重入 tokio runtime（既有 `refresh_mem` 已踩坑并修复），预览查询复用 `host.rt`（`UiRuntime`）的「独立 OS 线程 block_on + mpsc 回传」模式：

```rust
fn load_preview(&mut self, path: String) {
    let fs = self.host.fs.clone();
    let git = self.host.git.clone();
    let root = self.host.workspace_root.clone();
    let handle = self.host.rt.handle();
    let (tx, rx) = std::sync::mpsc::channel::<PreviewLoadResult>();
    std::thread::spawn(move || {
        let res = handle.block_on(async move {
            let p = std::path::Path::new(&path);
            let content = fs.read(p).await;
            let tracked = git.status().ok().map(|s| s.dirty).unwrap_or(false);
            let diff = if tracked { git.diff().ok() } else { None };
            PreviewLoadResult { content, diff, tracked: tracked && diff.is_some() }
        });
        let _ = tx.send(res);
    });
    if let Ok(res) = rx.recv() {
        // 填充 preview_content / preview_diff / preview_tracked / preview_error
    }
}
```

> 与 `refresh_mem` / `bootstrap_mem` 完全同一模式，已验证安全。`Fs::read` 走 `LocalFs::path()` 沙箱校验（拒绝 `..` 越界），安全。

---

## 5. 功能详细设计

### 5.1 R1：右侧分栏预览窗

#### 5.1.1 渲染结构

`update()` 中，在 `CentralPanel` 之前声明：

```rust
// 最右：文件树（独立开关）
if self.tree_open {
    egui::SidePanel::right("tree")
        .resizable(true)
        .default_width(240.0)
        .width_range(180.0..=360.0)
        .show(ctx, |ui| { self.render_tree(ui, &pal); });
}

// 次右：文件预览（独立开关）
if self.preview_open {
    egui::SidePanel::right("preview")
        .resizable(true)
        .default_width(380.0)
        .width_range(320.0..=600.0)
        .show(ctx, |ui| { self.render_preview(ui, &pal); });
}
```

#### 5.1.2 预览窗内容（render_preview）

```
┌─ preview ──────────────────────────────┐
│ 📄 src/gui.rs              [源码|Diff] ✕ │  ← 标题栏
├────────────────────────────────────────┤
│ 1  fn main() {                         │  ← 带行号的源码
│ 2      println!("hi");                 │
│ 3  }                                   │
└────────────────────────────────────────┘
```

- 标题栏：文件名（`preview_path` 的 `file_name`）+ 模式切换（仅 `preview_tracked` 时显示 Diff tab）+ 关闭按钮。
- 源码模式：`ScrollArea::vertical` + 等宽字体 + 行号列。行号与内容分两列对齐（`ui.columns(2, ...)` 或手动 `allocate`）。
- Diff 模式：见 §5.3。
- 关闭按钮：`close_button(ui, &pal)`（复用既有函数），点击置 `preview_open = false`、`preview_path = None`、清空缓存。

#### 5.1.3 文件类型处理

| 类型 | 处理 |
|------|------|
| 文本（UTF-8 可解码） | 正常渲染，行号 + 等宽 |
| 二进制（含 NUL 或非 UTF-8） | 显示「二进制文件，无法预览」 |
| 超大文件（> 512KB） | 显示前 512KB + 「文件过大，仅显示前 512KB」 |
| 不存在 | `preview_error = "文件不存在"` |

`Fs::read` 返回 `String`，`LocalFs` 用 `tokio::fs::read_to_string`，非 UTF-8 会报错 → 归入 `preview_error`。二进制检测：读到的 `String` 中检查是否含 `\0`（egui 文本渲染遇 NUL 会截断）。

### 5.2 R2：对话气泡内文件高亮可点击

#### 5.2.1 文件路径识别

在 `markdown.rs` 的 `to_job` 渲染中，对 `Event::Code(c)`（行内代码）和 `Event::Text(t)`（普通文本）做路径检测：

- **行内代码** `` `src/gui.rs` ``：高优先级识别为文件路径（最常见场景，模型常以行内代码标注文件名）。
- **普通文本**：用正则匹配相对路径模式（如 `src/xxx.rs`、`docs/xxx.md`），但**仅限在代码块外**且路径以已知扩展名结尾（`.rs/.md/.toml/.json/.js/.ts/.py` 等），避免误伤普通文本。

检测函数：

```rust
fn looks_like_file_path(s: &str, root: &Path) -> bool {
    // 1. 不含换行
    // 2. 路径分量数 >= 1，扩展名在白名单
    // 3. （可选）root.join(s).exists() —— 但这会阻塞渲染，改为点击时再校验
    s.lines().count() == 1
        && s.contains('.')
        && !s.contains(' ')
        && {
            let ext = std::path::Path::new(s).extension()
                .and_then(|e| e.to_str()).unwrap_or("");
            FILE_EXTS.contains(&ext)
        }
}
```

> 不在渲染期做 `exists()` 校验（IO 阻塞 UI 线程）；点击时若文件不存在，预览窗显示错误，不崩溃。

#### 5.2.2 可点击渲染

egui 的 `LayoutJob` 不直接支持「片段点击回调」。两种方案：

**方案 A（推荐）：Label + Sense::click，按行拆分**

对识别为文件路径的片段，不并入大 LayoutJob，而是单独渲染为一个 `egui::Label` + `ui.interact(..., Sense::click())`，样式为「等宽 + 下划线 + accent 色 + hover 变亮」，点击时设置 `preview_path` 并打开预览窗。

实现：`markdown.rs::to_job` 改为返回 `Vec<MarkdownBlock>`，其中 `MarkdownBlock::FilePath(String)` 表示一个可点击文件路径片段，GUI 侧渲染时对该 block 单独处理。

```rust
pub enum MarkdownBlock {
    Job(LayoutJob),           // 普通富文本段
    FilePath(String),         // 可点击文件路径
}

pub fn parse_blocks(md: &str, theme: &MdTheme, max_width: f32) -> Vec<MarkdownBlock>
```

**方案 B（备选）：保留 LayoutJob，用 `Label::new(job).sense(Sense::click)` + `response.clicked()` + 命中测试**

整个气泡作为一个 Label，点击时根据点击坐标反查 galley 命中的字符位置，判断是否落在文件路径区间。复杂度高，不推荐。

**采用方案 A**：改动集中在 `markdown.rs`（拆分输出）+ `gui.rs` 气泡渲染循环（对 `FilePath` block 单独渲染可点击 label）。

#### 5.2.3 点击行为

```rust
// 气泡渲染循环中
for block in crate::markdown::parse_blocks(&msg.text, &md_theme, max_w * 0.78 - 20.0) {
    match block {
        MarkdownBlock::Job(job) => {
            ui.add(egui::Label::new(job).selectable(true));
        }
        MarkdownBlock::FilePath(path) => {
            let label = egui::RichText::new(&path)
                .monospace()
                .color(pal.accent)
                .underline();
            let resp = ui.add(egui::Label::new(label).sense(egui::Sense::click()));
            if resp.hovered() { resp.on_hover_text("点击预览此文件"); }
            if resp.clicked() {
                self.open_preview(path);
            }
        }
    }
}
```

`open_preview`：

```rust
fn open_preview(&mut self, path: String) {
    self.preview_path = Some(path.clone());
    self.preview_open = true;
    self.preview_mode = PreviewMode::Source;
    self.preview_content = None;  // 触发重新加载
    self.preview_diff = None;
    self.preview_error = None;
    self.load_preview(path);
}
```

### 5.3 R3：Git Diff 视图

#### 5.3.1 Diff 数据来源

两种取 diff 的方式：

| 方式 | 命令 | 适用 |
|------|------|------|
| A. 整仓 diff | `Git::diff()`（现有，`git diff`，无路径参数） | 简单，但返回全仓 diff，需前端按文件切分 |
| B. 单文件 diff | `git diff -- <path>`（需 `Git` trait 新增方法） | 精确，推荐 |

**推荐方案 B**：`harness-capability::git::Git` trait 新增方法：

```rust
pub trait Git: Any + Send + Sync {
    // ... 既有方法 ...
    /// 指定文件的未暂存 diff（unified 格式）。无修改返回空串。
    fn diff_path(&self, path: &str) -> Result<String>;
    /// 文件是否被 git 跟踪（`git ls-files --error-unmatch <path>` 成功即跟踪）。
    fn is_tracked(&self, path: &str) -> Result<bool>;
}
```

`GitCli` 实现：

```rust
fn diff_path(&self, path: &str) -> Result<String> {
    self.run(&["diff", "--", path])
}

fn is_tracked(&self, path: &str) -> Result<bool> {
    self.run(&["ls-files", "--error-unmatch", path])
        .map(|s| !s.trim().is_empty())
}
```

> 这是**唯一需要改动 capability / provider 层**的地方。改动面极小：trait 加两方法、`GitCli` 加两实现。Consumer（UI）只依赖 trait，零硬编码。

#### 5.3.2 Diff 渲染

`git diff` unified 格式解析（轻量自写解析器，不引依赖）：

```rust
struct DiffLine {
    kind: DiffLineKind,  // Context / Add / Del / Hunk / Meta
    text: String,
}

fn parse_diff(diff: &str) -> Vec<DiffLine> {
    diff.lines().map(|line| {
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
        DiffLine { kind, text: line.to_string() }
    }).collect()
}
```

渲染着色（参照 Codex / GitHub）：

| 行类型 | 前景色 | 背景色（行底） | 前缀 |
|--------|--------|----------------|------|
| Add | 绿 | `diff_add_bg` | `+` |
| Del | 红 | `diff_del_bg` | `-` |
| Hunk | 青 | 透明 | `@@` |
| Meta | dim | 透明 | `+++`/`---` |
| Context | text | 透明 | ` ` |

Palette 新增字段：

```rust
struct Palette {
    // ... 既有 ...
    diff_add_bg: egui::Color32,
    diff_del_bg: egui::Color32,
    diff_add_text: egui::Color32,
    diff_del_text: egui::Color32,
}
```

深色：`diff_add_bg = #0f2e1a`、`diff_del_bg = #3a1414`；浅色：`#e6f4ea` / `#fce8e6`。

#### 5.3.3 模式切换

预览窗标题栏：

```
📄 src/gui.rs     [源码 | Diff]     ✕
```

- `preview_tracked == false` 时不显示 Diff tab（非 git 文件）。
- 切到 Diff 时若 `preview_diff` 为空则触发 `load_diff`（`git diff -- <path>`）。
- 无修改（diff 为空串）时 Diff 区显示「该文件无未提交修改」。

### 5.4 R4：项目文件树

#### 5.4.1 树构建

`FileTreeNode` 懒构建：首次打开 tree 窗时调 `Fs::list(workspace_root)` 构建顶层；点击目录节点展开时调 `Fs::list(dir)` 填充子节点。

```rust
fn build_tree(&mut self) {
    let fs = self.host.fs.clone();
    let root = self.host.workspace_root.clone();
    let handle = self.host.rt.handle();
    let (tx, rx) = std::sync::mpsc::channel::<Vec<FileTreeNode>>();
    std::thread::spawn(move || {
        let nodes = handle.block_on(async move {
            list_dir_recursive(&fs, std::path::Path::new(&root), 2).await
        });
        let _ = tx.send(nodes);
    });
    if let Ok(nodes) = rx.recv() {
        self.tree_root = Some(FileTreeNode {
            name: root.clone(), path: String::new(), is_dir: true, children: nodes,
        });
    }
}
```

- **深度限制**：首次只构建 2 层（顶层 + 一级子目录），更深层级点击展开时懒加载，避免大仓库卡顿。
- **忽略目录**：`.git`、`target`、`node_modules`、`.harness-memory`、`dist` 等（参照 `.gitignore` + 性能考量）。
- **刷新节流**：`tree_last_refresh` 记录上次构建时间，间隔 > 5s 才允许手动刷新按钮触发重建；外部文件变更不自动刷新（避免轮询开销）。

#### 5.4.2 树渲染

```
tree
├─ 📁 src
│  ├─ 📁 bin
│  │  └─ 📄 main.rs
│  └─ 📄 lib.rs
├─ 📁 docs
│  └─ 📄 architecture.md
└─ 📄 Cargo.toml
```

- 目录节点：点击切换 `tree_expanded`，展开时懒加载子节点。
- 文件节点：点击 → `open_preview(path)`（同时打开预览窗）。
- 图标：目录用 `Icon::Folder`（既有），文件用新增 `Icon::File`（简单文档线条）。
- 当前预览文件高亮：`preview_path == node.path` 时行底着色 `pal.hover` + 左侧 accent 竖条（与历史面板当前会话同一视觉语言）。

#### 5.4.3 开关入口

主区头部 `right_to_left` 布局中，主题切换按钮左侧新增文件树 toggle：

```rust
let tree_btn = ui.add_sized(
    [28.0, 28.0],
    egui::Button::new(egui::RichText::new("📁").size(14.0))
).on_hover_text("项目文件树");
if tree_btn.clicked() {
    self.tree_open = !self.tree_open;
    if self.tree_open && self.tree_root.is_none() {
        self.build_tree();
    }
}
```

> 用 emoji 还是矢量图标？既有 `draw_icon` 是矢量线条绘制（不依赖字体字形，避免 CJK 字体缺字变豆腐块）。为一致性，新增 `Icon::FolderTree` 矢量图标，用 `draw_icon` 绘制，而非 emoji。

---

## 6. 模块拆分

为避免 `gui.rs`（已 3200 行）继续膨胀，新增独立模块：

```
harness-ui/src/
├── gui.rs          （既有，改动：AppState 字段、update 布局、气泡渲染、头部按钮）
├── markdown.rs     （既有，改动：parse_blocks 输出 FilePath block）
├── preview.rs      （新增：PreviewMode、FileTreeNode、DiffLine、parse_diff、render_preview、render_tree）
└── lib.rs          （既有，改动：mod preview）
```

`preview.rs` 导出：

```rust
pub use crate::preview::{
    FileTreeNode, PreviewMode, DiffLine, DiffLineKind,
    parse_diff, looks_like_file_path, FILE_EXTS,
};
```

`gui.rs` 的 `AppState` 方法 `render_preview` / `render_tree` / `load_preview` / `build_tree` 可实现为 `preview.rs` 中的自由函数（接收 `&mut AppState` + `&mut egui::Ui` + `&Palette`），或作为 `AppState` 的 impl 方法。推荐后者（与既有 `draw_update_banner` 等同一风格）。

---

## 7. 数据流（一次「点击文件路径 → 预览」的完整链路）

```
用户点击气泡内 "src/gui.rs"
  │
  ▼
AppState::open_preview("src/gui.rs")
  │  设置 preview_path / preview_open=true / 清缓存
  ▼
AppState::load_preview("src/gui.rs")
  │  clone Arc<dyn Fs> + Arc<dyn Git>
  │  spawn 独立 OS 线程 → host.rt.handle().block_on(async {
  │     content = fs.read("src/gui.rs").await
  │     tracked = git.is_tracked("src/gui.rs").unwrap_or(false)
  │     diff    = if tracked { git.diff_path("src/gui.rs").ok() } else { None }
  │  })
  │  mpsc 回传 → 填充 preview_content / preview_tracked / preview_diff
  ▼
下一帧 update()
  │  preview_open == true → SidePanel::right("preview").show()
  │  render_preview: 按 preview_mode 渲染 Source 或 Diff
  ▼
用户点击 [Diff] tab
  │  preview_mode = Diff
  │  preview_diff 已有 → 直接渲染 parse_diff(diff)
  ▼
用户点击 ✕
  │  preview_open = false → SidePanel 不渲染 → 主区占满
```

**不经过 SessionLog、不触发 agent turn、不产生 SessionEvent**。符合不变量 1 / 3。

---

## 8. 边界与安全

| 场景 | 处理 |
|------|------|
| 路径越界（`../etc/passwd`） | `LocalFs::path()` 已有沙箱校验（`SandboxDenied`），`Fs::read` 返回 Err → `preview_error` |
| 超大文件 | `Fs::read` 全量读入内存；> 512KB 时截断展示 + 提示（在 `load_preview` 返回后检查 `content.len()`） |
| 二进制文件 | `String` 含 NUL → 显示「二进制文件」 |
| git 未初始化（非 git 仓库） | `Git::is_tracked` 返回 Err → `preview_tracked = false`，不显示 Diff tab |
| 文件树超大仓库 | 深度限制 2 层 + 忽略 `target`/`node_modules` 等；手动刷新按钮 |
| 并发点击多个文件 | `load_preview` 覆盖 `preview_path`，最新点击生效；旧 mpsc 结果通过比对 `preview_path` 丢弃过期回传 |
| 项目切换 | `switch_project` 时清空 `preview_*` / `tree_root`（与 `refresh_history` 同一清理点） |

---

## 9. 改动清单（评审通过后实施）

### 9.1 capability 层（L2）

**`harness-capability/src/git.rs`**：trait 新增 2 方法

```diff
 pub trait Git: Any + Send + Sync {
     fn status(&self) -> Result<GitStatus>;
     fn diff(&self) -> Result<String>;
+    fn diff_path(&self, path: &str) -> Result<String>;
+    fn is_tracked(&self, path: &str) -> Result<bool>;
     fn commit(&self, message: &str) -> Result<String>;
     // ...
 }
```

### 9.2 provider 层（L3）

**`harness-provider-git/src/lib.rs`**：`GitCli` 实现 2 方法（各 3 行）

### 9.3 UI 层（L6）

| 文件 | 改动 |
|------|------|
| `harness-ui/src/lib.rs` | `mod preview;` + pub use |
| `harness-ui/src/preview.rs` | **新增**：`PreviewMode` / `FileTreeNode` / `DiffLine` / `parse_diff` / `looks_like_file_path` / `FILE_EXTS` |
| `harness-ui/src/markdown.rs` | `to_job` → 拆为 `parse_blocks` 返回 `Vec<MarkdownBlock>`（`Job` / `FilePath`）；保留 `to_job` 向后兼容（内部调 `parse_blocks` 后取 Job 段拼接） |
| `harness-ui/src/gui.rs` | `EguiUi::new` 加 `fs`/`git` 参数；`AppState` 加预览/树字段；`update()` 加右栏布局；`render_preview`/`render_tree`/`load_preview`/`build_tree`/`open_preview` 方法；气泡渲染改用 `parse_blocks`；头部加文件树 toggle；`Palette` 加 diff 配色；`Icon` 加 `File`/`FolderTree` |
| `harness-ui/Cargo.toml` | 无新增依赖（复用 egui / pulldown-cmark） |

### 9.4 组装层（L7）

**`harness/bin/src/compose.rs`**：`make_ui` 取 `Arc<dyn Fs>` / `Arc<dyn Git>` 传入 `EguiUi::new`

### 9.5 不改动的层

- L0 `harness-core`：无改动
- L1 `harness-llm` / `harness-session`：无改动
- L4 `harness-tool` / `harness-runtime`：无改动
- L3 `harness-provider-local`：无改动（`LocalFs` 已满足）

---

## 10. 测试计划

### 10.1 单元测试

| 模块 | 测试 |
|------|------|
| `preview.rs::parse_diff` | 给定标准 unified diff 文本，断言行类型分类正确（Add/Del/Hunk/Meta/Context） |
| `preview.rs::looks_like_file_path` | 正例：`src/gui.rs`、`docs/a.md`；反例：`hello world`、`foo`、`a b/c.rs` |
| `markdown.rs::parse_blocks` | 含行内代码文件路径的 Markdown，断言产出 `FilePath` block |

### 10.2 集成测试（手动 / 截图验证）

1. 打开 GUI，助手回复含 `` `src/main.rs` `` → 路径高亮可点击 → 点击 → 右侧预览窗展示内容。
2. 预览窗点 ✕ → 关闭，主区恢复全宽。
3. 修改某 git 跟踪文件 → 预览该文件 → 切 Diff → 看到 `+/-` 着色 diff。
4. 头部点 📁 → 文件树展开 → 点击文件 → 预览窗同步打开。
5. 切换项目 → 预览窗 / 文件树清空。
6. 点击 `../outside.txt`（若气泡出现）→ 预览窗显示沙箱拒绝错误。

### 10.3 不变量回归

- `cargo test` 既有不变量断言全部通过（本功能不触碰核心循环 / SessionLog）。
- `cargo clippy -D warnings` 零警告。

---

## 11. 风险与缓解

| 风险 | 缓解 |
|------|------|
| `markdown.rs` 拆分 `parse_blocks` 破坏既有气泡渲染 | 保留 `to_job` 兼容包装；先在 `parse_blocks` 内部复用既有逻辑，`to_job` 调它后只取 `Job` 段拼接，既有非文件路径气泡渲染零变化 |
| 文件路径误识别（普通文本被当成路径） | 白名单扩展名 + 不含空格 + 单行；点击时文件不存在则报错不崩；可加 settings 开关 `ui.file_path_clickable` 默认 true |
| 大仓库文件树卡顿 | 深度限制 2 层 + 忽略目录 + 懒加载 + 手动刷新 |
| `Arc<dyn Fs>` / `Arc<dyn Git>` 注入使 `EguiUi::new` 参数更多 | 已 12 参数，加 2 个可接受；若后续继续膨胀可引入 `EguiUiDeps` struct 聚合（本次不做） |
| egui `SidePanel::right` 双栏叠放顺序 | 已验证：egui 按声明顺序从右往左叠放，`tree` 先声明在最右，`preview` 次右，`CentralPanel` 最后填充剩余 |

---

## 12. 里程碑（评审通过后）

| 阶段 | 内容 | 预计 |
|------|------|------|
| M1 | capability + provider：`diff_path` / `is_tracked` | 0.5h |
| M2 | `preview.rs` 骨架：`PreviewMode` / `FileTreeNode` / `parse_diff` / `looks_like_file_path` + 单测 | 2h |
| M3 | `gui.rs`：`AppState` 字段 + `EguiUi::new` 注入 + `compose.rs` 取服务 | 1h |
| M4 | `render_preview` + `load_preview`（源码模式） | 2h |
| M5 | `markdown.rs` `parse_blocks` + 气泡可点击文件路径 | 2h |
| M6 | Diff 模式渲染 + 模式切换 | 1.5h |
| M7 | `render_tree` + `build_tree` + 头部 toggle | 2h |
| M8 | 联调 + 截图验证 + clippy/test | 1h |

---

## 13. 评审检查清单

- [ ] 是否破坏分层（UI 是否反向调用核心循环）？→ 否，只读查询能力服务，不触发 turn。
- [ ] 是否破坏不变量 1（SessionLog 真相源）？→ 否，预览状态不进日志。
- [ ] 是否破坏不变量 2（Consumer 不依赖 Provider）？→ 否，UI 依赖 `dyn Fs` / `dyn Git` trait。
- [ ] 是否破坏不变量 3（UI 纯消费者）？→ 否，不回写核心。
- [ ] capability 改动是否最小？→ 是，仅 `Git` trait 加 2 方法。
- [ ] 是否引入新外部依赖？→ 否。
- [ ] 布局是否参照 Codex？→ 是，三栏 + 可折叠右栏 + 文件树。
- [ ] 异步查询是否复用既有安全模式？→ 是，`UiRuntime` + mpsc。
- [ ] 错误路径是否覆盖（越界 / 二进制 / 超大 / 非 git）？→ 是。

---

*本文档为设计稿，评审通过后按 §9 改动清单与 §12 里程碑实施。*
