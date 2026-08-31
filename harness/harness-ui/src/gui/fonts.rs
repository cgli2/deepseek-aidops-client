//! Cross-platform UI font discovery and egui font installation.

use std::path::PathBuf;
use std::sync::Arc;

#[cfg(target_os = "windows")]
const CJK_FONT_CANDIDATES: &[&str] = &[
    "C:\\Windows\\Fonts\\msyh.ttc",
    "C:\\Windows\\Fonts\\simhei.ttf",
    "C:\\Windows\\Fonts\\simsun.ttc",
];

#[cfg(target_os = "macos")]
const CJK_FONT_CANDIDATES: &[&str] = &[
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/System/Library/Fonts/STHeiti Medium.ttc",
    "/System/Library/Fonts/STHeiti Light.ttc",
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    "/System/Library/Fonts/Supplemental/Songti.ttc",
    "/System/Library/Fonts/PingFang.ttc",
];

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const CJK_FONT_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
];

#[cfg(target_os = "macos")]
fn macos_pingfang_path() -> Option<PathBuf> {
    let root = std::path::Path::new("/System/Library/AssetsV2/com_apple_MobileAsset_Font8");
    std::fs::read_dir(root).ok()?.flatten().find_map(|entry| {
        let path = entry.path().join("AssetData/PingFang.ttc");
        path.is_file().then_some(path)
    })
}

pub(super) fn available_cjk_font() -> Option<(PathBuf, Vec<u8>)> {
    #[cfg(target_os = "macos")]
    if let Some(path) = macos_pingfang_path() {
        if let Ok(bytes) = std::fs::read(&path) {
            return Some((path, bytes));
        }
    }

    CJK_FONT_CANDIDATES.iter().find_map(|path| {
        std::fs::read(path)
            .ok()
            .map(|bytes| (PathBuf::from(path), bytes))
    })
}

pub(super) fn install_cjk_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    #[cfg(target_os = "macos")]
    for (key, path, family) in [
        (
            "mac-sf",
            "/System/Library/Fonts/SFNS.ttf",
            egui::FontFamily::Proportional,
        ),
        (
            "mac-sf-mono",
            "/System/Library/Fonts/SFNSMono.ttf",
            egui::FontFamily::Monospace,
        ),
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert(key.to_owned(), Arc::new(egui::FontData::from_owned(bytes)));
            fonts
                .families
                .entry(family)
                .or_default()
                .insert(0, key.to_owned());
            super::trace(&format!("[fonts] loaded macOS UI font: {path}"));
        }
    }

    if let Some((path, bytes)) = available_cjk_font() {
        fonts.font_data.insert(
            "cjk".to_owned(),
            Arc::new(egui::FontData::from_owned(bytes)),
        );
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            let fonts_in_family = fonts.families.entry(family).or_default();
            #[cfg(target_os = "macos")]
            fonts_in_family.insert(1.min(fonts_in_family.len()), "cjk".to_owned());
            #[cfg(not(target_os = "macos"))]
            fonts_in_family.push("cjk".to_owned());
        }
        super::trace(&format!("[fonts] loaded CJK fallback: {}", path.display()));
    } else {
        super::trace(&format!(
            "[fonts] no CJK font found; checked: {}",
            CJK_FONT_CANDIDATES.join(", ")
        ));
    }
    ctx.set_fonts(fonts);
}

#[cfg(target_os = "macos")]
pub(super) fn install_macos_ui_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style
        .text_styles
        .insert(egui::TextStyle::Heading, egui::FontId::proportional(18.0));
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(13.5));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(13.0));
    style
        .text_styles
        .insert(egui::TextStyle::Small, egui::FontId::proportional(11.5));
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.interact_size.y = 28.0;
    ctx.set_style(style);
}
