//! Runtime SF Symbols bridge for the macOS GUI renderer.
//!
//! This module intentionally exposes only egui types. AppKit objects stay local to
//! the render call, so symbol lookup and rasterization remain on the UI/render
//! thread that invokes [`paint_if_available`].

use std::ffi::c_uchar;
use std::panic::AssertUnwindSafe;
use std::slice;

use objc2::AnyThread;
use objc2::exception;
use objc2::rc::autoreleasepool;
use objc2_app_kit::{
    NSBitmapFormat, NSBitmapImageRep, NSCompositingOperation, NSDeviceRGBColorSpace,
    NSFontWeightMedium, NSGraphicsContext, NSImage, NSImageSymbolConfiguration,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

const CACHE_NAMESPACE: &str = "harness-ui.sf-symbol";
const MAX_RASTERIZED_PIXELS: usize = 1_048_576;

#[derive(Clone, Copy)]
struct RasterSize {
    width: usize,
    height: usize,
    logical_width: f64,
    logical_height: f64,
    pixels_per_point_bits: u32,
}

/// Paint `icon` with its native SF Symbol when the current macOS provides it.
///
/// Returns `false` for an unknown semantic icon, an unavailable symbol, invalid
/// geometry, or any AppKit failure. Callers can then use their vector fallback.
pub(crate) fn paint_if_available(
    painter: &egui::Painter,
    rect: egui::Rect,
    icon: super::icons::Icon,
    tint: egui::Color32,
) -> bool {
    let Some(symbol_names) = symbol_names(icon) else {
        return false;
    };

    let context = painter.ctx();
    let pixels_per_point = context.pixels_per_point();
    let max_texture_side = context.input(|input| input.max_texture_side);
    let Some(size) = raster_size(rect, pixels_per_point, max_texture_side) else {
        return false;
    };

    for &symbol_name in symbol_names {
        let cache_id = texture_cache_id(symbol_name, size);
        if let Some(texture) = context.data(|data| data.get_temp::<egui::TextureHandle>(cache_id)) {
            paint_texture(painter, rect, texture.id(), tint);
            return true;
        }

        let Some(image) = rasterize_symbol(symbol_name, size) else {
            continue;
        };

        let texture = context.load_texture(
            format!(
                "{CACHE_NAMESPACE}.{symbol_name}.{}x{}.{}",
                size.width, size.height, size.pixels_per_point_bits
            ),
            image,
            egui::TextureOptions::LINEAR,
        );
        context.data_mut(|data| data.insert_temp(cache_id, texture.clone()));
        paint_texture(painter, rect, texture.id(), tint);
        return true;
    }

    false
}

#[allow(unreachable_patterns)] // Keep the wildcard for icon variants added after this adapter.
fn symbol_names(icon: super::icons::Icon) -> Option<&'static [&'static str]> {
    use super::icons::Icon;

    match icon {
        Icon::Chat => Some(&["message", "bubble.left"]),
        Icon::Folder => Some(&["folder"]),
        Icon::GitBranch => Some(&["arrow.triangle.branch"]),
        Icon::Layers => Some(&["square.3.layers.3d"]),
        Icon::Chip => Some(&["cpu"]),
        Icon::Gear => Some(&["gearshape", "gear"]),
        Icon::Menu => Some(&["line.3.horizontal"]),
        Icon::Update | Icon::RefreshCw => Some(&["arrow.clockwise"]),
        Icon::Terminal => Some(&["terminal"]),
        Icon::Code => Some(&["chevron.left.forwardslash.chevron.right"]),
        Icon::GitDiff => Some(&["arrow.triangle.branch"]),
        Icon::Sparkles => Some(&["sparkles"]),
        Icon::Pin => Some(&["pin", "pin.fill"]),
        Icon::CheckCircle => Some(&["checkmark.circle", "checkmark.circle.fill"]),
        Icon::Search => Some(&["magnifyingglass"]),
        Icon::Clock => Some(&["clock"]),
        Icon::FileText => Some(&["doc.text", "doc"]),
        Icon::BarChart => Some(&["chart.bar", "chart.bar.fill"]),
        Icon::Activity => Some(&["waveform.path.ecg", "waveform"]),
        Icon::ExternalLink => Some(&["arrow.up.right.square"]),
        Icon::Copy => Some(&["doc.on.doc"]),
        Icon::Target => Some(&["target"]),
        Icon::ShieldCheck => Some(&["checkmark.shield", "shield"]),
        Icon::Check => Some(&["checkmark"]),
        Icon::RotateCcw => Some(&["arrow.counterclockwise"]),
        Icon::Lightbulb => Some(&["lightbulb", "lightbulb.fill"]),
        Icon::Wrench => Some(&["wrench"]),
        Icon::ListTree => Some(&["list.bullet.indent", "list.bullet"]),
        Icon::Key => Some(&["key"]),
        Icon::Bot => Some(&["cpu"]),
        Icon::User => Some(&["person"]),
        Icon::AlertTriangle => Some(&["exclamationmark.triangle", "exclamationmark.triangle.fill"]),
        Icon::ChevronRight => Some(&["chevron.right"]),
        Icon::ChevronDown => Some(&["chevron.down"]),
        Icon::ChevronUp => Some(&["chevron.up"]),
        Icon::X => Some(&["xmark"]),
        Icon::CircleDot => Some(&["record.circle", "circle.inset.filled"]),
        Icon::Circle => Some(&["circle"]),
        Icon::Brain => Some(&["brain"]),
        Icon::Plus => Some(&["plus"]),
        Icon::Pencil => Some(&["pencil"]),
        Icon::Trash => Some(&["trash"]),
        Icon::Paperclip => Some(&["paperclip"]),
        Icon::ArchiveBox => Some(&["archivebox"]),
        Icon::Send => Some(&["paperplane"]),
        Icon::Stop => Some(&["stop.fill"]),
        Icon::Sun => Some(&["sun.max"]),
        Icon::Moon => Some(&["moon"]),
        Icon::Sidebar => Some(&["sidebar.left"]),
        Icon::Inspector => Some(&["rectangle.rightthird.inset.filled", "rectangle.split.3x1"]),
        Icon::ChevronLeft => Some(&["chevron.left"]),
        Icon::WindowMinimize => Some(&["minus"]),
        Icon::WindowMaximize => Some(&["square"]),
        Icon::WindowRestore => Some(&["rectangle.on.rectangle"]),
        _ => None,
    }
}

fn raster_size(
    rect: egui::Rect,
    pixels_per_point: f32,
    max_texture_side: usize,
) -> Option<RasterSize> {
    if !rect.is_finite() || !pixels_per_point.is_finite() || pixels_per_point <= 0.0 {
        return None;
    }

    let logical_width = rect.width();
    let logical_height = rect.height();
    if logical_width <= 0.0 || logical_height <= 0.0 {
        return None;
    }

    let width = physical_extent(logical_width, pixels_per_point)?;
    let height = physical_extent(logical_height, pixels_per_point)?;
    if width > max_texture_side || height > max_texture_side {
        return None;
    }
    if width.checked_mul(height)? > MAX_RASTERIZED_PIXELS {
        return None;
    }

    Some(RasterSize {
        width,
        height,
        logical_width: f64::from(logical_width),
        logical_height: f64::from(logical_height),
        pixels_per_point_bits: pixels_per_point.to_bits(),
    })
}

fn physical_extent(logical_extent: f32, pixels_per_point: f32) -> Option<usize> {
    let physical_extent = logical_extent * pixels_per_point;
    if !physical_extent.is_finite() || physical_extent <= 0.0 {
        return None;
    }

    let rounded_extent = physical_extent.ceil();
    if rounded_extent > usize::MAX as f32 {
        return None;
    }

    Some(rounded_extent as usize)
}

fn texture_cache_id(symbol_name: &str, size: RasterSize) -> egui::Id {
    egui::Id::new((
        CACHE_NAMESPACE,
        symbol_name,
        size.width,
        size.height,
        size.pixels_per_point_bits,
    ))
}

fn paint_texture(
    painter: &egui::Painter,
    rect: egui::Rect,
    texture_id: egui::TextureId,
    tint: egui::Color32,
) {
    painter.image(
        texture_id,
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        tint,
    );
}

fn rasterize_symbol(symbol_name: &str, size: RasterSize) -> Option<egui::ColorImage> {
    autoreleasepool(|_| rasterize_symbol_in_pool(symbol_name, size))
}

fn rasterize_symbol_in_pool(symbol_name: &str, size: RasterSize) -> Option<egui::ColorImage> {
    let name = try_appkit(|| NSString::from_str(symbol_name))?;
    let image =
        try_appkit(|| NSImage::imageWithSystemSymbolName_accessibilityDescription(&name, None))??;
    let configuration = try_appkit(|| unsafe {
        NSImageSymbolConfiguration::configurationWithPointSize_weight(
            size.logical_width.max(size.logical_height),
            NSFontWeightMedium,
        )
    })?;
    let image = try_appkit(|| image.imageWithSymbolConfiguration(&configuration))??;

    let width = isize::try_from(size.width).ok()?;
    let height = isize::try_from(size.height).ok()?;
    let bitmap = try_appkit(|| unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut::<*mut c_uchar>(),
            width,
            height,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            NSBitmapFormat::AlphaFirst | NSBitmapFormat::ThirtyTwoBitBigEndian,
            0,
            0,
        )
    })??;

    // Map the physical backing store to the caller's logical egui rectangle.
    try_appkit(|| {
        bitmap.setSize(NSSize::new(size.logical_width, size.logical_height));
    })?;

    let bitmap_data = try_appkit(|| bitmap.bitmapData())?;
    let bytes_per_row = usize::try_from(try_appkit(|| bitmap.bytesPerRow())?).ok()?;
    let bytes_per_plane = usize::try_from(try_appkit(|| bitmap.bytesPerPlane())?).ok()?;
    let minimum_row_bytes = size.width.checked_mul(4)?;
    let required_bytes = bytes_per_row.checked_mul(size.height)?;
    if bitmap_data.is_null()
        || bytes_per_row < minimum_row_bytes
        || bytes_per_plane < required_bytes
    {
        return None;
    }

    // NSBitmapImageRep does not promise zero-filled storage for a newly allocated plane.
    unsafe { std::ptr::write_bytes(bitmap_data, 0, bytes_per_plane) };

    let graphics_context =
        try_appkit(|| NSGraphicsContext::graphicsContextWithBitmapImageRep(&bitmap))??;
    let previous_context = try_appkit(NSGraphicsContext::currentContext)?;
    let switched_context =
        try_appkit(|| NSGraphicsContext::setCurrentContext(Some(&graphics_context)));
    let drew_image = if switched_context.is_some() {
        try_appkit(|| {
            image.drawInRect_fromRect_operation_fraction(
                NSRect::new(
                    NSPoint::ZERO,
                    NSSize::new(size.logical_width, size.logical_height),
                ),
                NSRect::ZERO,
                NSCompositingOperation::SourceOver,
                1.0,
            );
        })
    } else {
        None
    };
    // Restore the calling AppKit context even if changing or drawing in ours failed.
    let restored_context =
        try_appkit(|| NSGraphicsContext::setCurrentContext(previous_context.as_deref()));
    if switched_context.is_none() || drew_image.is_none() || restored_context.is_none() {
        return None;
    }

    let bytes = unsafe { slice::from_raw_parts(bitmap_data, bytes_per_plane) };
    let pixel_count = size.width.checked_mul(size.height)?;
    let rgba_len = pixel_count.checked_mul(4)?;
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(rgba_len).ok()?;
    rgba.resize(rgba_len, 0);

    let mut has_ink = false;
    for (index, pixel) in rgba.chunks_exact_mut(4).enumerate() {
        let row = index / size.width;
        let column = index % size.width;
        let source_offset = row
            .checked_mul(bytes_per_row)?
            .checked_add(column.checked_mul(4)?)?;
        // The requested alpha-first, big-endian layout is ARGB, so byte zero is alpha.
        let alpha = *bytes.get(source_offset)?;
        pixel[0] = 255;
        pixel[1] = 255;
        pixel[2] = 255;
        pixel[3] = alpha;
        has_ink |= alpha != 0;
    }

    has_ink.then(|| egui::ColorImage::from_rgba_unmultiplied([size.width, size.height], &rgba))
}

/// Converts Objective-C exceptions into the fallback path instead of allowing an
/// unavailable AppKit API or symbol to unwind into Rust.
fn try_appkit<T>(operation: impl FnOnce() -> T) -> Option<T> {
    exception::catch(AssertUnwindSafe(operation)).ok()
}

#[cfg(test)]
mod tests {
    use super::super::icons::Icon;
    use super::*;

    #[test]
    fn every_semantic_icon_has_a_native_candidate() {
        for icon in Icon::ALL {
            assert!(
                symbol_names(*icon).is_some(),
                "missing SF Symbol candidate for {icon:?}"
            );
        }
    }
}
