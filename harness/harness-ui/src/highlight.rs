//! 代码预览语法高亮：syntect 分词 + egui LayoutJob 渲染。
//!
//! 目标：让源码预览达到 markdown 代码块级别的观感（关键词/字符串/注释/数字着色），
//! 而非纯文本。整段代码构建为一个 LayoutJob（行号 + 高亮 token + 换行），
//! egui 按文本哈希缓存 galley，tokenize 只做一次、渲染零成本。

use std::sync::OnceLock;

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Style, Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

/// 全局懒加载的语法集与主题集（首次调用初始化一次，进程生命周期复用）。
struct Highlighter {
    syn: SyntaxSet,
    themes: ThemeSet,
}

fn highlighter() -> &'static Highlighter {
    static INSTANCE: OnceLock<Highlighter> = OnceLock::new();
    INSTANCE.get_or_init(|| Highlighter {
        syn: SyntaxSet::load_defaults_newlines(),
        themes: ThemeSet::load_defaults(),
    })
}

fn theme(h: &'static Highlighter, dark: bool) -> &'static Theme {
    if dark {
        h.themes
            .themes
            .get("base16-ocean.dark")
            .unwrap_or_else(|| h.themes.themes.values().next().expect("syntect themes"))
    } else {
        h.themes
            .themes
            .get("InspiredGitHub")
            .unwrap_or_else(|| h.themes.themes.values().next().expect("syntect themes"))
    }
}

/// 按文件名（或扩展名）选择语法；无匹配时回退纯文本。
fn syntax(h: &'static Highlighter, file_name: &str) -> &'static SyntaxReference {
    let path = std::path::Path::new(file_name);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    // 无扩展名的知名文件名（Dockerfile / Makefile 等）按名称匹配。
    let base = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    h.syn
        .find_syntax_by_extension(&ext)
        .or_else(|| h.syn.find_syntax_by_name(base))
        .unwrap_or_else(|| h.syn.find_syntax_plain_text())
}

/// syntect 行样式 → egui TextFormat（颜色 + 斜体/粗体/下划线）。
fn text_format(style: Style, size: f32, code_bg: Color32) -> TextFormat {
    let fg = style.foreground;
    let color = Color32::from_rgb(fg.r, fg.g, fg.b);
    TextFormat {
        font_id: FontId::monospace(size),
        color,
        background: code_bg,
        italics: style
            .font_style
            .contains(syntect::highlighting::FontStyle::ITALIC),
        underline: if style
            .font_style
            .contains(syntect::highlighting::FontStyle::UNDERLINE)
        {
            egui::Stroke::new(1.0_f32, color)
        } else {
            egui::Stroke::NONE
        },
        strikethrough: egui::Stroke::NONE,
        extra_letter_spacing: if style
            .font_style
            .contains(syntect::highlighting::FontStyle::BOLD)
        {
            0.3
        } else {
            0.0
        },
        ..Default::default()
    }
}

/// 行号文本格式（dim 色，右对齐）。
fn line_no_format(size: f32, dim: Color32) -> TextFormat {
    TextFormat {
        font_id: FontId::monospace(size),
        color: dim,
        ..Default::default()
    }
}

/// 把源码高亮为 LayoutJob（行号 + 高亮 token + 换行）。
///
/// - `code`：源码全文
/// - `file_name`：用于推断语言（扩展名 / Dockerfile 等）
/// - `dark`：选择深/浅主题
/// - `code_bg`：代码行背景色（整行统一，随主题）
/// - `dim`：行号颜色
/// - `max_width`：Label 可用宽度（`f32::INFINITY` 表示不换行，配合水平滚动）
pub fn highlight_to_job(
    code: &str,
    file_name: &str,
    dark: bool,
    code_bg: Color32,
    dim: Color32,
    max_width: f32,
) -> LayoutJob {
    let h = highlighter();
    let syn = syntax(h, file_name);
    let th = theme(h, dark);
    let mut hlines = HighlightLines::new(syn, th);

    let mut job = LayoutJob::default();
    job.wrap.max_width = max_width.max(80.0);

    let line_count = code.lines().count().max(1);
    let num_w = line_count.to_string().len().max(2);
    let size = 11.5_f32;
    let mut line_no = 1usize;

    for line in LinesWithEndings::from(code) {
        // 行号列（右对齐 + 空格分隔）
        job.append(
            &format!("{line_no:>width$} ", width = num_w),
            0.0,
            line_no_format(size, dim),
        );
        // 高亮 token
        match hlines.highlight_line(line, &h.syn) {
            Ok(ranges) => {
                for (style, text) in ranges {
                    job.append(text, 0.0, text_format(style, size, code_bg));
                }
            }
            Err(_) => {
                job.append(line, 0.0, text_format(Style::default(), size, code_bg));
            }
        }
        line_no += 1;
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_rust_keywords_in_color() {
        let job = highlight_to_job(
            "fn main() {\n    let x = 1;\n}\n",
            "test.rs",
            true,
            Color32::from_rgb(0x14, 0x1b, 0x24),
            Color32::GRAY,
            f32::INFINITY,
        );
        // 应有行号 + 内容；文本包含源码。
        assert!(job.text.contains("fn main"));
        assert!(job.text.contains("let x"));
        // 多段颜色：至少 行号段 + 若干 token 段。
        assert!(job.sections.len() > 4);
    }

    #[test]
    fn plain_text_falls_back_without_panic() {
        let job = highlight_to_job(
            "some plain text\nsecond line\n",
            "unknown_ext_xyz",
            false,
            Color32::WHITE,
            Color32::GRAY,
            300.0,
        );
        assert!(job.text.contains("some plain text"));
    }
}
