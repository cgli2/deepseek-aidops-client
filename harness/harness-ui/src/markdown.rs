//! 轻量 Markdown 渲染：pulldown-cmark 解析事件流 → egui `LayoutJob` 富文本。
//!
//! 仅助手气泡启用；用户/工具/计划气泡保持纯文本。支持：标题、段落、粗体（egui
//! 无字重，以强调色+字距模拟）、斜体、删除线、行内代码、围栏代码块、有序/
//! 无序列表、引用、分隔线、链接。
//!
//! 换行策略用「延迟分隔」：块结束时只置标志，下一块开始前再补空行，
//! 避免文档结尾残留空行。

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Stroke};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

const BODY_SIZE: f32 = 13.5;
const CODE_SIZE: f32 = 12.5;
const BLOCK_GAP_SIZE: f32 = 7.0;
const HEADING_SIZES: [f32; 3] = [18.0, 16.0, 14.5];

/// 渲染所需的主题色（从 gui `Palette` 拷贝，避免模块间循环依赖）。
#[derive(Clone, Copy)]
pub struct MdTheme {
    pub text: Color32,
    pub dim: Color32,
    pub accent: Color32,
    pub code_text: Color32,
    pub code_bg: Color32,
}

/// 把 Markdown 源文渲染为 LayoutJob。`max_width` 为气泡内容可用宽度。
#[allow(unused_assignments)]
pub fn to_job(md: &str, theme: &MdTheme, max_width: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = max_width.max(80.0);

    let mut bold = 0u32;
    let mut italic = 0u32;
    let mut strike = 0u32;
    let mut underline = false;
    let mut in_code_block = false;
    let mut heading: Option<u8> = None;
    let mut quote_depth = 0u32;
    // 列表栈：None=无序，Some(n)=有序且 n 为下一个序号。
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut pending_break = false;

    // 段间距（延迟分隔）：仅当已有内容且不在列表项内时补空行。
    macro_rules! block_break {
        () => {
            if pending_break && !job.text.is_empty() && lists.is_empty() {
                if !job.text.ends_with('\n') {
                    append_str(&mut job, "\n", &plain(theme));
                }
                append_str(&mut job, "\n", &spacer_fmt(theme));
            }
            pending_break = false;
        };
    }
    // 行首保证：列表项标记/代码块行之前确保位于新行。
    macro_rules! line_break {
        () => {
            if !job.text.is_empty() && !job.text.ends_with('\n') {
                append_str(&mut job, "\n", &plain(theme));
            }
        };
    }

    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);

    for event in Parser::new_ext(md, opts) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    block_break!();
                    heading = Some(match level {
                        HeadingLevel::H1 => 1,
                        HeadingLevel::H2 => 2,
                        _ => 3,
                    });
                }
                Tag::Paragraph => {
                    block_break!();
                }
                Tag::List(start) => {
                    block_break!();
                    lists.push(start);
                }
                Tag::Item => {
                    line_break!();
                    let depth = lists.len().saturating_sub(1);
                    let marker = match lists.last_mut() {
                        Some(Some(n)) => {
                            let m = format!("{n}. ");
                            *n += 1;
                            m
                        }
                        _ => "• ".into(),
                    };
                    let indent = "  ".repeat(depth + 1);
                    append_str(&mut job, &format!("{indent}{marker}"), &list_marker_fmt(theme));
                }
                Tag::CodeBlock(_) => {
                    block_break!();
                    in_code_block = true;
                }
                Tag::BlockQuote => quote_depth += 1,
                Tag::Emphasis => italic += 1,
                Tag::Strong => bold += 1,
                Tag::Strikethrough => strike += 1,
                Tag::Link { .. } => underline = true,
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Heading(_) => {
                    heading = None;
                    pending_break = true;
                }
                TagEnd::Paragraph => pending_break = true,
                TagEnd::CodeBlock => {
                    in_code_block = false;
                    pending_break = true;
                }
                TagEnd::BlockQuote => {
                    quote_depth = quote_depth.saturating_sub(1);
                    pending_break = true;
                }
                TagEnd::List(_) => {
                    lists.pop();
                    pending_break = true;
                }
                TagEnd::Item => {
                    line_break!();
                }
                TagEnd::Emphasis => italic = italic.saturating_sub(1),
                TagEnd::Strong => bold = bold.saturating_sub(1),
                TagEnd::Strikethrough => strike = strike.saturating_sub(1),
                TagEnd::Link => underline = false,
                _ => {}
            },
            Event::Text(t) => {
                if in_code_block {
                    line_break!();
                    append_str(
                        &mut job,
                        &t,
                        &fmt(
                            CODE_SIZE,
                            theme.code_text,
                            true,
                            theme.code_bg,
                            false,
                            false,
                            false,
                            false,
                        ),
                    );
                } else if let Some(level) = heading {
                    let size = HEADING_SIZES[(level.saturating_sub(1) as usize).min(2)];
                    append_str(
                        &mut job,
                        &t,
                        &fmt(
                            size,
                            heading_color(theme),
                            false,
                            Color32::TRANSPARENT,
                            false,
                            italic > 0,
                            strike > 0,
                            false,
                        ),
                    );
                } else {
                    let color = if quote_depth > 0 {
                        theme.dim
                    } else {
                        theme.text
                    };
                    append_str(
                        &mut job,
                        &t,
                        &fmt(
                            BODY_SIZE,
                            color,
                            false,
                            Color32::TRANSPARENT,
                            bold > 0,
                            italic > 0,
                            strike > 0,
                            underline,
                        ),
                    );
                }
            }
            Event::Code(c) => append_str(
                &mut job,
                &c,
                &fmt(
                    CODE_SIZE,
                    theme.code_text,
                    true,
                    theme.code_bg,
                    false,
                    false,
                    strike > 0,
                    false,
                ),
            ),
            Event::SoftBreak => append_str(&mut job, " ", &plain(theme)),
            Event::HardBreak => append_str(&mut job, "\n", &plain(theme)),
            Event::Rule => {
                block_break!();
                line_break!();
                append_str(&mut job, &"─".repeat(48), &dim_fmt(theme));
                pending_break = true;
            }
            _ => {}
        }
    }
    job
}

/// Markdown 渲染块：普通富文本段或可点击文件路径。
#[derive(Clone)]
pub enum MarkdownBlock {
    /// 普通富文本段（标题/段落/列表/代码块等）。
    #[allow(dead_code)]
    Job(LayoutJob),
    /// 可点击的文件路径（行内代码或普通文本中识别出的路径）。
    FilePath(String),
}

/// 把 Markdown 源文解析为渲染块列表。
///
/// 策略：先用 `to_job` 生成完整 LayoutJob，再扫描原文中的行内代码（`` `path` ``），
/// 若内容是文件路径则拆出为 `FilePath` block。普通文本中的路径因误伤风险高，
/// 仅在行内代码场景识别。
pub fn parse_blocks(md: &str, theme: &MdTheme, max_width: f32) -> Vec<MarkdownBlock> {
    use pulldown_cmark::{Event, Options, Parser};

    let mut blocks: Vec<MarkdownBlock> = Vec::new();
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);

    // 事件流遍历：普通文本/非路径行内代码累积进 pending，
    // 遇到文件路径行内代码时先把 pending 刷成 Job，再输出 FilePath chip。
    // 这样非文件路径的行内代码保持在文本原位（不会因拆块导致不合理换行）。
    let mut pending = String::new();
    for event in Parser::new_ext(md, opts) {
        match event {
            Event::Code(c) => {
                let code_str = c.to_string();
                if crate::preview::looks_like_file_path(&code_str) {
                    if !pending.trim().is_empty() {
                        blocks.push(MarkdownBlock::Job(to_job(&pending, theme, max_width)));
                        pending.clear();
                    }
                    blocks.push(MarkdownBlock::FilePath(code_str));
                } else {
                    // 非文件路径的行内代码：保留 backtick 原样，由 to_job 渲染为行内代码。
                    pending.push('`');
                    pending.push_str(&code_str);
                    pending.push('`');
                }
            }
            Event::Text(t) => pending.push_str(&t),
            Event::SoftBreak | Event::HardBreak => pending.push('\n'),
            _ => {}
        }
    }
    if !pending.trim().is_empty() {
        blocks.push(MarkdownBlock::Job(to_job(&pending, theme, max_width)));
    }
    if blocks.is_empty() {
        blocks.push(MarkdownBlock::Job(to_job(md, theme, max_width)));
    }
    blocks
}

fn append_str(job: &mut LayoutJob, s: &str, f: &TextFormat) {
    if !s.is_empty() {
        job.append(s, 0.0, f.clone());
    }
}

/// egui `TextFormat` 无独立字重，使用非常克制的字距表达正文强调；标题只靠
/// 小幅字号差和柔化强调色区分，避免中文标题显得粗重。
fn fmt(
    size: f32,
    color: Color32,
    mono: bool,
    bg: Color32,
    bold: bool,
    italic: bool,
    strike: bool,
    underline: bool,
) -> TextFormat {
    TextFormat {
        font_id: if mono {
            FontId::monospace(size)
        } else {
            FontId::proportional(size)
        },
        color,
        background: bg,
        italics: italic,
        underline: if underline {
            Stroke::new(1.0_f32, color)
        } else {
            Stroke::NONE
        },
        strikethrough: if strike {
            Stroke::new(1.0_f32, color)
        } else {
            Stroke::NONE
        },
        extra_letter_spacing: if bold { 0.15 } else { 0.0 },
        ..Default::default()
    }
}

fn plain(theme: &MdTheme) -> TextFormat {
    fmt(
        BODY_SIZE,
        theme.text,
        false,
        Color32::TRANSPARENT,
        false,
        false,
        false,
        false,
    )
}

fn dim_fmt(theme: &MdTheme) -> TextFormat {
    fmt(
        BODY_SIZE,
        theme.dim,
        false,
        Color32::TRANSPARENT,
        false,
        false,
        false,
        false,
    )
}

fn list_marker_fmt(theme: &MdTheme) -> TextFormat {
    fmt(
        BODY_SIZE,
        heading_color(theme),
        false,
        Color32::TRANSPARENT,
        true,
        false,
        false,
        false,
    )
}

fn spacer_fmt(theme: &MdTheme) -> TextFormat {
    fmt(
        BLOCK_GAP_SIZE,
        theme.text,
        false,
        Color32::TRANSPARENT,
        false,
        false,
        false,
        false,
    )
}

fn heading_color(theme: &MdTheme) -> Color32 {
    // 适当混入次级文字色，降低高饱和强调色在大段技术内容中的视觉冲击。
    let mix = |accent: u8, dim: u8| ((accent as u16 * 4 + dim as u16) / 5) as u8;
    Color32::from_rgb(
        mix(theme.accent.r(), theme.dim.r()),
        mix(theme.accent.g(), theme.dim.g()),
        mix(theme.accent.b(), theme.dim.b()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> MdTheme {
        MdTheme {
            text: Color32::WHITE,
            dim: Color32::GRAY,
            accent: Color32::GREEN,
            code_text: Color32::LIGHT_GRAY,
            code_bg: Color32::BLACK,
        }
    }

    #[test]
    fn renders_heading_list_and_code_as_sections() {
        let job = to_job(
            "# 标题\n\n- 项目一\n- 项目二\n\n```\ndir\n```\n",
            &theme(),
            300.0,
        );
        assert!(job.text.contains("标题"));
        assert!(job.text.contains("• 项目一"));
        assert!(job.text.contains("dir"));
        // 至少：标题 / 列表标记 / 两项 / 代码 = 多个分段。
        assert!(job.sections.len() >= 5);
    }

    #[test]
    fn plain_text_keeps_single_section_and_no_trailing_blank() {
        let job = to_job("普通段落文字", &theme(), 300.0);
        assert_eq!(job.sections.len(), 1);
        assert!(!job.text.ends_with('\n'));
    }

    #[test]
    fn heading_scale_stays_compact_and_hierarchical() {
        let job = to_job("# 一级\n\n## 二级\n\n### 三级\n\n正文", &theme(), 300.0);
        let sizes: Vec<f32> = job
            .sections
            .iter()
            .map(|section| section.format.font_id.size)
            .collect();
        for expected in HEADING_SIZES {
            assert!(sizes.iter().any(|size| (*size - expected).abs() < 0.01));
        }
        assert!(sizes.iter().any(|size| (*size - BODY_SIZE).abs() < 0.01));
        assert!(HEADING_SIZES[0] - BODY_SIZE <= 5.0, "一级标题比正文大 4.5px，属合理层次");
    }
    /// 回归：非文件路径的行内代码不拆分（保持文本原位），
    /// 只有文件路径才拆成 FilePath chip。避免行内代码被拆成独立块导致不合理换行。
    #[test]
    fn parse_blocks_only_splits_file_path_inline_code() {
        let t = theme();
        // 含非文件路径行内代码 + 文件路径
        let md = "编译通过，见 `OK` 和 `exit 0`；文件在 `src/gui.rs`。";
        let blocks = parse_blocks(md, &t, 300.0);
        // 非文件路径的 `OK`/`exit 0` 应留在文本内，不被拆成独立 block；
        // 文件路径 src/gui.rs 应被拆成 FilePath。
        let file_paths: Vec<&str> = blocks
            .iter()
            .filter_map(|b| match b {
                MarkdownBlock::FilePath(p) => Some(p.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(file_paths, vec!["src/gui.rs"], "should only extract file path");
        // 所有 Job block 的文本拼起来应包含行内代码内容（未被丢弃）
        let job_text: String = blocks
            .iter()
            .filter_map(|b| match b {
                MarkdownBlock::Job(j) => Some(j.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(job_text.contains("OK"), "inline code OK should stay in text");
        assert!(job_text.contains("exit 0"), "inline code exit 0 should stay in text");
        assert!(job_text.contains("文件在"), "prefix text should be kept");
    }

    /// 回归：全是普通文本（无文件路径）时返回单个 Job block。
    #[test]
    fn parse_blocks_no_file_path_returns_single_job() {
        let t = theme();
        let blocks = parse_blocks("你好，世界 `code` 内容", &t, 300.0);
        assert_eq!(blocks.len(), 1, "no file path should yield one job");
        if let MarkdownBlock::Job(j) = &blocks[0] {
            assert!(j.text.contains("code"));
        } else {
            panic!("expected Job block");
        }
    }
}
