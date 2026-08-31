//! 临时诊断：验证 to_job 对真实 markdown 回复的段落/换行渲染。
use harness_ui::markdown::{to_job, MdTheme};
use egui::Color32;

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
fn real_reply_has_paragraph_breaks() {
    let md = "第一段：介绍背景。\n\n第二段 **加粗** 和 `code`。\n\n- 项一\n- 项二\n\n```\nfn main(){}\n```\n\n结语。";
    let job = to_job(md, &theme(), 400.0);
    let nl = job.text.matches('\n').count();
    eprintln!("newlines={nl} sections={} text={:?}", job.sections.len(), job.text);
    assert!(nl >= 3, "expected paragraph breaks, got {nl}");
    assert!(job.sections.len() > 2, "expected styled sections");
}

#[test]
fn no_file_path_reply_uses_full_markdown() {
    use harness_ui::markdown::parse_blocks;
    let md = "标题\n\n正文段落一。\n\n正文段落二，带 **重点**。";
    let blocks = parse_blocks(md, &theme(), 400.0);
    assert_eq!(blocks.len(), 1, "should be single job block");
    if let harness_ui::markdown::MarkdownBlock::Job(j) = &blocks[0] {
        let nl = j.text.matches('\n').count();
        eprintln!("no-fp newlines={nl} text={:?}", j.text);
        assert!(nl >= 2, "paragraph breaks missing, got {nl}");
    } else {
        panic!("expected job");
    }
}
