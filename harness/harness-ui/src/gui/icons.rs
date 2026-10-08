//! Semantic UI icons and the AIOPS brand mark.
//!
//! macOS renders the semantic catalog with system SF Symbols (see `sf_symbols`),
//! while this module provides a polished, tintable vector fallback everywhere else.
//! Fallback paths share a 24×24 logical grid, regular optical margins, round caps,
//! and round joins so the interface stays coherent on every supported platform.

use super::theme::Palette;

/// Semantic UI icon catalog.
///
/// Names describe intent rather than a drawing recipe so the macOS renderer can map
/// them to SF Symbols without leaking platform artwork into UI call sites.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[allow(dead_code)]
pub(crate) enum Icon {
    Chat,
    Folder,
    GitBranch,
    Layers,
    Chip,
    Gear,
    Menu,
    Update,
    Terminal,
    Code,
    GitDiff,
    Sparkles,
    Pin,
    CheckCircle,
    Search,
    Clock,
    FileText,
    BarChart,
    Activity,
    ExternalLink,
    Copy,
    RefreshCw,
    Target,
    ShieldCheck,
    Check,
    RotateCcw,
    Lightbulb,
    Wrench,
    ListTree,
    Key,
    Bot,
    User,
    AlertTriangle,
    ChevronRight,
    ChevronDown,
    ChevronUp,
    X,
    CircleDot,
    Circle,
    Brain,
    Plus,
    Pencil,
    Trash,
    Paperclip,
    ArchiveBox,
    Send,
    Stop,
    Sun,
    Moon,
    Sidebar,
    Inspector,
    ChevronLeft,
    WindowMinimize,
    WindowMaximize,
    WindowRestore,
}

impl Icon {
    #[cfg(test)]
    pub(crate) const ALL: &[Self] = &[
        Self::Chat,
        Self::Folder,
        Self::GitBranch,
        Self::Layers,
        Self::Chip,
        Self::Gear,
        Self::Menu,
        Self::Update,
        Self::Terminal,
        Self::Code,
        Self::GitDiff,
        Self::Sparkles,
        Self::Pin,
        Self::CheckCircle,
        Self::Search,
        Self::Clock,
        Self::FileText,
        Self::BarChart,
        Self::Activity,
        Self::ExternalLink,
        Self::Copy,
        Self::RefreshCw,
        Self::Target,
        Self::ShieldCheck,
        Self::Check,
        Self::RotateCcw,
        Self::Lightbulb,
        Self::Wrench,
        Self::ListTree,
        Self::Key,
        Self::Bot,
        Self::User,
        Self::AlertTriangle,
        Self::ChevronRight,
        Self::ChevronDown,
        Self::ChevronUp,
        Self::X,
        Self::CircleDot,
        Self::Circle,
        Self::Brain,
        Self::Plus,
        Self::Pencil,
        Self::Trash,
        Self::Paperclip,
        Self::ArchiveBox,
        Self::Send,
        Self::Stop,
        Self::Sun,
        Self::Moon,
        Self::Sidebar,
        Self::Inspector,
        Self::ChevronLeft,
        Self::WindowMinimize,
        Self::WindowMaximize,
        Self::WindowRestore,
    ];
}

const GRID: f32 = 24.0;
const REGULAR: f32 = 1.75;
const THIN: f32 = 1.35;
const EMPHASIS: f32 = 2.05;

/// Compatibility entry point for the existing 16pt icon call sites.
pub(crate) fn draw_icon(
    painter: &egui::Painter,
    center: egui::Pos2,
    icon: Icon,
    color: egui::Color32,
) {
    draw_icon_sized(painter, center, icon, color, 16.0);
}

/// Paint a semantic icon centered at `center` with an explicit visual size.
pub(crate) fn draw_icon_sized(
    painter: &egui::Painter,
    center: egui::Pos2,
    icon: Icon,
    color: egui::Color32,
    size: f32,
) {
    draw_icon_in_rect(
        painter,
        egui::Rect::from_center_size(center, egui::Vec2::splat(size.max(1.0))),
        icon,
        color,
    );
}

/// Paint a semantic icon into a target rectangle.
///
/// On macOS this first delegates to the cached, system-provided SF Symbol renderer.
/// Any unavailable symbol or rendering failure safely falls through to the vector
/// catalog below, preserving every control on older macOS releases and other OSes.
pub(crate) fn draw_icon_in_rect(
    painter: &egui::Painter,
    rect: egui::Rect,
    icon: Icon,
    color: egui::Color32,
) {
    #[cfg(target_os = "macos")]
    if super::sf_symbols::paint_if_available(painter, rect, icon, color) {
        return;
    }

    draw_vector_icon(painter, rect, icon, color);
}

#[derive(Clone, Copy)]
struct Glyph {
    rect: egui::Rect,
    scale: f32,
}

impl Glyph {
    fn new(rect: egui::Rect) -> Self {
        let side = rect.width().min(rect.height()).max(1.0);
        Self {
            rect,
            scale: side / GRID,
        }
    }

    fn p(self, x: f32, y: f32) -> egui::Pos2 {
        egui::pos2(
            self.rect.center().x + (x - GRID / 2.0) * self.scale,
            self.rect.center().y + (y - GRID / 2.0) * self.scale,
        )
    }

    fn rect(self, min_x: f32, min_y: f32, max_x: f32, max_y: f32) -> egui::Rect {
        egui::Rect::from_min_max(self.p(min_x, min_y), self.p(max_x, max_y))
    }

    fn stroke(self, weight: f32, color: egui::Color32) -> egui::Stroke {
        egui::Stroke::new((weight * self.scale).max(0.75), color)
    }

    fn radius(self, value: f32) -> f32 {
        value * self.scale
    }
}

/// Draw a segment with explicit circular caps. epaint's generic strokes do not
/// prescribe a cross-platform round-cap style, so the caps are part of our icon
/// design system instead of an accidental backend detail.
fn round_line(painter: &egui::Painter, from: egui::Pos2, to: egui::Pos2, stroke: egui::Stroke) {
    painter.line_segment([from, to], stroke);
    let cap = stroke.width * 0.5;
    painter.circle_filled(from, cap, stroke.color);
    painter.circle_filled(to, cap, stroke.color);
}

fn round_polyline(
    painter: &egui::Painter,
    points: &[egui::Pos2],
    closed: bool,
    stroke: egui::Stroke,
) {
    if points.len() < 2 {
        return;
    }
    for pair in points.windows(2) {
        round_line(painter, pair[0], pair[1], stroke);
    }
    if closed {
        round_line(
            painter,
            *points.last().expect("checked length"),
            points[0],
            stroke,
        );
    }
    let radius = stroke.width * 0.5;
    for point in points {
        painter.circle_filled(*point, radius, stroke.color);
    }
}

fn round_cubic(painter: &egui::Painter, points: [egui::Pos2; 4], stroke: egui::Stroke) {
    painter.add(egui::Shape::CubicBezier(
        egui::epaint::CubicBezierShape::from_points_stroke(
            points,
            false,
            egui::Color32::TRANSPARENT,
            stroke,
        ),
    ));
    let radius = stroke.width * 0.5;
    painter.circle_filled(points[0], radius, stroke.color);
    painter.circle_filled(points[3], radius, stroke.color);
}

fn round_arc(
    painter: &egui::Painter,
    g: Glyph,
    cx: f32,
    cy: f32,
    radius: f32,
    start: f32,
    end: f32,
    stroke: egui::Stroke,
) {
    let span = (end - start).abs();
    let segments = ((span / std::f32::consts::TAU * 32.0).ceil() as usize).max(5);
    let mut points = Vec::with_capacity(segments + 1);
    for index in 0..=segments {
        let t = index as f32 / segments as f32;
        let angle = start + (end - start) * t;
        points.push(g.p(cx + radius * angle.cos(), cy + radius * angle.sin()));
    }
    round_polyline(painter, &points, false, stroke);
}

fn draw_vector_icon(painter: &egui::Painter, rect: egui::Rect, icon: Icon, color: egui::Color32) {
    let g = Glyph::new(rect);
    let regular = g.stroke(REGULAR, color);
    let thin = g.stroke(THIN, color);
    let emphasis = g.stroke(EMPHASIS, color);

    match icon {
        Icon::Chat => {
            painter.rect(
                g.rect(3.5, 4.5, 20.5, 17.0),
                egui::Rounding::same(g.radius(4.0)),
                egui::Color32::TRANSPARENT,
                regular,
            );
            round_polyline(
                painter,
                &[g.p(8.5, 17.0), g.p(6.6, 20.0), g.p(11.1, 17.0)],
                false,
                regular,
            );
            for x in [8.5, 12.0, 15.5] {
                painter.circle_filled(g.p(x, 10.8), g.radius(1.05), color);
            }
        }
        Icon::Folder => {
            let outline = [
                g.p(3.3, 7.2),
                g.p(3.3, 18.4),
                g.p(20.7, 18.4),
                g.p(20.7, 7.2),
                g.p(12.5, 7.2),
                g.p(10.4, 4.8),
                g.p(3.3, 4.8),
            ];
            round_polyline(painter, &outline, true, regular);
        }
        Icon::GitBranch => {
            painter.circle_stroke(g.p(6.0, 5.0), g.radius(2.0), regular);
            painter.circle_stroke(g.p(6.0, 19.0), g.radius(2.0), regular);
            painter.circle_stroke(g.p(18.0, 6.4), g.radius(2.0), regular);
            round_line(painter, g.p(6.0, 7.0), g.p(6.0, 17.0), regular);
            round_cubic(
                painter,
                [
                    g.p(6.0, 13.0),
                    g.p(9.0, 13.0),
                    g.p(10.4, 6.4),
                    g.p(16.0, 6.4),
                ],
                regular,
            );
        }
        Icon::Layers => {
            let upper = [
                g.p(4.0, 8.0),
                g.p(12.0, 4.0),
                g.p(20.0, 8.0),
                g.p(12.0, 12.0),
            ];
            let lower = [g.p(4.0, 13.0), g.p(12.0, 17.0), g.p(20.0, 13.0)];
            round_polyline(painter, &upper, true, regular);
            round_polyline(painter, &lower, false, regular);
        }
        Icon::Chip => {
            painter.rect(
                g.rect(6.7, 6.7, 17.3, 17.3),
                egui::Rounding::same(g.radius(2.0)),
                egui::Color32::TRANSPARENT,
                regular,
            );
            painter.rect(
                g.rect(9.5, 9.5, 14.5, 14.5),
                egui::Rounding::same(g.radius(0.9)),
                egui::Color32::TRANSPARENT,
                thin,
            );
            for offset in [9.2, 14.8] {
                round_line(painter, g.p(offset, 3.8), g.p(offset, 6.7), thin);
                round_line(painter, g.p(offset, 17.3), g.p(offset, 20.2), thin);
                round_line(painter, g.p(3.8, offset), g.p(6.7, offset), thin);
                round_line(painter, g.p(17.3, offset), g.p(20.2, offset), thin);
            }
        }
        Icon::Gear => draw_gear(painter, g, regular),
        Icon::Menu => {
            for y in [6.5, 12.0, 17.5] {
                round_line(painter, g.p(4.5, y), g.p(19.5, y), regular);
            }
        }
        Icon::Update | Icon::RefreshCw => draw_refresh(painter, g, regular),
        Icon::Terminal => {
            painter.rect(
                g.rect(3.3, 4.2, 20.7, 19.8),
                egui::Rounding::same(g.radius(3.0)),
                egui::Color32::TRANSPARENT,
                thin,
            );
            round_polyline(
                painter,
                &[g.p(7.0, 9.0), g.p(10.5, 12.0), g.p(7.0, 15.0)],
                false,
                regular,
            );
            round_line(painter, g.p(13.2, 15.0), g.p(17.1, 15.0), regular);
        }
        Icon::Code => {
            round_polyline(
                painter,
                &[g.p(9.4, 5.5), g.p(4.4, 12.0), g.p(9.4, 18.5)],
                false,
                regular,
            );
            round_polyline(
                painter,
                &[g.p(14.6, 5.5), g.p(19.6, 12.0), g.p(14.6, 18.5)],
                false,
                regular,
            );
            round_line(painter, g.p(13.2, 4.4), g.p(10.8, 19.6), thin);
        }
        Icon::GitDiff => {
            painter.circle_stroke(g.p(6.0, 5.5), g.radius(1.9), regular);
            painter.circle_stroke(g.p(6.0, 18.5), g.radius(1.9), regular);
            painter.circle_stroke(g.p(18.0, 12.0), g.radius(1.9), regular);
            round_line(painter, g.p(6.0, 7.4), g.p(6.0, 16.6), regular);
            round_cubic(
                painter,
                [
                    g.p(6.0, 12.0),
                    g.p(10.0, 12.0),
                    g.p(12.0, 12.0),
                    g.p(16.1, 12.0),
                ],
                regular,
            );
        }
        Icon::Sparkles => {
            let star = [
                g.p(12.0, 3.8),
                g.p(13.8, 10.2),
                g.p(20.2, 12.0),
                g.p(13.8, 13.8),
                g.p(12.0, 20.2),
                g.p(10.2, 13.8),
                g.p(3.8, 12.0),
                g.p(10.2, 10.2),
            ];
            round_polyline(painter, &star, true, regular);
        }
        Icon::Pin => {
            round_line(painter, g.p(8.0, 5.0), g.p(16.0, 5.0), regular);
            round_line(painter, g.p(9.0, 5.0), g.p(9.0, 10.0), regular);
            round_line(painter, g.p(15.0, 5.0), g.p(15.0, 10.0), regular);
            round_line(painter, g.p(6.6, 10.0), g.p(17.4, 10.0), regular);
            round_line(painter, g.p(12.0, 10.0), g.p(12.0, 19.2), regular);
        }
        Icon::CheckCircle => {
            painter.circle_stroke(g.p(12.0, 12.0), g.radius(8.0), regular);
            draw_check(painter, g, regular);
        }
        Icon::Search => {
            painter.circle_stroke(g.p(10.2, 10.2), g.radius(5.4), regular);
            round_line(painter, g.p(14.0, 14.0), g.p(19.2, 19.2), regular);
        }
        Icon::Clock => {
            painter.circle_stroke(g.p(12.0, 12.0), g.radius(8.0), regular);
            round_line(painter, g.p(12.0, 7.0), g.p(12.0, 12.0), regular);
            round_line(painter, g.p(12.0, 12.0), g.p(16.0, 14.5), regular);
        }
        Icon::FileText => {
            let outline = [
                g.p(6.0, 3.5),
                g.p(14.0, 3.5),
                g.p(19.0, 8.5),
                g.p(19.0, 20.5),
                g.p(6.0, 20.5),
            ];
            round_polyline(painter, &outline, true, regular);
            round_polyline(
                painter,
                &[g.p(14.0, 3.5), g.p(14.0, 8.5), g.p(19.0, 8.5)],
                false,
                thin,
            );
            for (left, right, y) in [(8.4, 16.6, 12.2), (8.4, 16.6, 15.4), (8.4, 13.6, 18.0)] {
                round_line(painter, g.p(left, y), g.p(right, y), thin);
            }
        }
        Icon::BarChart => {
            round_line(painter, g.p(5.0, 19.0), g.p(19.0, 19.0), thin);
            for (x, top) in [(7.0, 13.5), (12.0, 9.0), (17.0, 5.0)] {
                round_line(painter, g.p(x, 19.0), g.p(x, top), emphasis);
            }
        }
        Icon::Activity => {
            round_polyline(
                painter,
                &[
                    g.p(3.5, 12.0),
                    g.p(7.0, 12.0),
                    g.p(9.0, 6.5),
                    g.p(12.3, 18.0),
                    g.p(14.5, 9.0),
                    g.p(16.0, 12.0),
                    g.p(20.5, 12.0),
                ],
                false,
                regular,
            );
        }
        Icon::ExternalLink => {
            round_polyline(
                painter,
                &[
                    g.p(10.2, 5.0),
                    g.p(5.0, 5.0),
                    g.p(5.0, 19.0),
                    g.p(19.0, 19.0),
                    g.p(19.0, 13.8),
                ],
                false,
                regular,
            );
            round_line(painter, g.p(10.0, 14.0), g.p(19.0, 5.0), regular);
            round_polyline(
                painter,
                &[g.p(13.3, 5.0), g.p(19.0, 5.0), g.p(19.0, 10.7)],
                false,
                regular,
            );
        }
        Icon::Copy => draw_copy(painter, g, regular),
        Icon::Target => {
            painter.circle_stroke(g.p(12.0, 12.0), g.radius(8.0), regular);
            painter.circle_stroke(g.p(12.0, 12.0), g.radius(4.0), thin);
            painter.circle_filled(g.p(12.0, 12.0), g.radius(1.4), color);
        }
        Icon::ShieldCheck => {
            let outline = [
                g.p(5.0, 5.0),
                g.p(12.0, 2.8),
                g.p(19.0, 5.0),
                g.p(19.0, 11.8),
                g.p(12.0, 20.5),
                g.p(5.0, 11.8),
            ];
            round_polyline(painter, &outline, true, regular);
            draw_check(painter, g, thin);
        }
        Icon::Check => draw_check(painter, g, emphasis),
        Icon::RotateCcw => {
            round_arc(
                painter,
                g,
                12.0,
                12.0,
                7.0,
                -0.45,
                std::f32::consts::TAU * 0.78,
                regular,
            );
            round_polyline(
                painter,
                &[g.p(5.0, 7.5), g.p(5.0, 3.8), g.p(8.8, 3.8)],
                false,
                regular,
            );
        }
        Icon::Lightbulb => {
            painter.circle_stroke(g.p(12.0, 10.2), g.radius(6.2), regular);
            round_line(painter, g.p(9.0, 16.0), g.p(15.0, 16.0), regular);
            round_line(painter, g.p(9.7, 19.0), g.p(14.3, 19.0), regular);
        }
        Icon::Wrench => {
            round_line(painter, g.p(7.0, 17.0), g.p(16.0, 8.0), emphasis);
            round_cubic(
                painter,
                [
                    g.p(15.0, 4.0),
                    g.p(19.0, 3.2),
                    g.p(20.5, 6.0),
                    g.p(18.0, 9.0),
                ],
                regular,
            );
            painter.circle_stroke(g.p(6.0, 18.0), g.radius(2.2), regular);
        }
        Icon::ListTree => {
            round_line(painter, g.p(6.0, 5.0), g.p(6.0, 19.0), thin);
            for (y, right) in [(5.0, 19.0), (12.0, 17.0), (19.0, 19.0)] {
                painter.circle_filled(g.p(6.0, y), g.radius(1.25), color);
                round_line(painter, g.p(8.5, y), g.p(right, y), regular);
            }
        }
        Icon::Key => {
            painter.circle_stroke(g.p(8.0, 9.0), g.radius(3.7), regular);
            round_line(painter, g.p(10.7, 11.7), g.p(19.4, 20.4), regular);
            round_line(painter, g.p(15.6, 16.6), g.p(18.2, 14.0), regular);
            round_line(painter, g.p(17.8, 18.8), g.p(20.2, 16.4), regular);
        }
        Icon::Bot => {
            round_line(painter, g.p(12.0, 4.0), g.p(12.0, 6.0), thin);
            painter.circle_filled(g.p(12.0, 3.2), g.radius(1.0), color);
            painter.rect(
                g.rect(4.5, 6.2, 19.5, 18.5),
                egui::Rounding::same(g.radius(3.4)),
                egui::Color32::TRANSPARENT,
                regular,
            );
            painter.circle_filled(g.p(9.0, 12.2), g.radius(1.2), color);
            painter.circle_filled(g.p(15.0, 12.2), g.radius(1.2), color);
            round_line(painter, g.p(9.0, 15.2), g.p(15.0, 15.2), thin);
        }
        Icon::User => {
            painter.circle_stroke(g.p(12.0, 7.5), g.radius(3.5), regular);
            round_cubic(
                painter,
                [
                    g.p(4.5, 20.0),
                    g.p(5.5, 14.0),
                    g.p(18.5, 14.0),
                    g.p(19.5, 20.0),
                ],
                regular,
            );
        }
        Icon::AlertTriangle => {
            let points = [g.p(12.0, 3.6), g.p(21.0, 19.5), g.p(3.0, 19.5)];
            round_polyline(painter, &points, true, regular);
            round_line(painter, g.p(12.0, 9.0), g.p(12.0, 14.0), regular);
            painter.circle_filled(g.p(12.0, 17.0), g.radius(1.0), color);
        }
        Icon::ChevronRight => round_polyline(
            painter,
            &[g.p(8.5, 5.5), g.p(15.5, 12.0), g.p(8.5, 18.5)],
            false,
            emphasis,
        ),
        Icon::ChevronDown => round_polyline(
            painter,
            &[g.p(5.5, 8.5), g.p(12.0, 15.5), g.p(18.5, 8.5)],
            false,
            emphasis,
        ),
        Icon::ChevronUp => round_polyline(
            painter,
            &[g.p(5.5, 15.5), g.p(12.0, 8.5), g.p(18.5, 15.5)],
            false,
            emphasis,
        ),
        Icon::X => {
            round_line(painter, g.p(6.0, 6.0), g.p(18.0, 18.0), regular);
            round_line(painter, g.p(18.0, 6.0), g.p(6.0, 18.0), regular);
        }
        Icon::CircleDot => {
            painter.circle_stroke(g.p(12.0, 12.0), g.radius(7.2), regular);
            painter.circle_filled(g.p(12.0, 12.0), g.radius(3.1), color);
        }
        Icon::Circle => {
            painter.circle_stroke(g.p(12.0, 12.0), g.radius(7.5), regular);
        }
        Icon::Brain => draw_brain(painter, g, regular, thin),
        Icon::Plus => {
            round_line(painter, g.p(5.5, 12.0), g.p(18.5, 12.0), regular);
            round_line(painter, g.p(12.0, 5.5), g.p(12.0, 18.5), regular);
        }
        Icon::Pencil => draw_pencil(painter, g, regular, thin),
        Icon::Trash => draw_trash(painter, g, regular, thin),
        Icon::Paperclip => draw_paperclip(painter, g, regular),
        Icon::ArchiveBox => draw_archive_box(painter, g, regular, thin),
        Icon::Send => draw_send(painter, g, regular),
        Icon::Stop => {
            painter.rect_filled(
                g.rect(7.0, 7.0, 17.0, 17.0),
                egui::Rounding::same(g.radius(2.3)),
                color,
            );
        }
        Icon::Sun => draw_sun(painter, g, regular),
        Icon::Moon => draw_moon(painter, g, regular),
        Icon::Sidebar => draw_sidebar(painter, g, regular, color),
        Icon::Inspector => draw_inspector(painter, g, regular, color),
        Icon::ChevronLeft => round_polyline(
            painter,
            &[g.p(15.5, 5.5), g.p(8.5, 12.0), g.p(15.5, 18.5)],
            false,
            emphasis,
        ),
        Icon::WindowMinimize => {
            round_line(painter, g.p(6.5, 15.0), g.p(17.5, 15.0), regular);
        }
        Icon::WindowMaximize => {
            painter.rect(
                g.rect(6.5, 6.5, 17.5, 17.5),
                egui::Rounding::same(g.radius(0.8)),
                egui::Color32::TRANSPARENT,
                regular,
            );
        }
        Icon::WindowRestore => {
            painter.rect(
                g.rect(5.0, 5.0, 15.4, 15.4),
                egui::Rounding::same(g.radius(0.8)),
                egui::Color32::TRANSPARENT,
                thin,
            );
            painter.rect(
                g.rect(8.6, 8.6, 19.0, 19.0),
                egui::Rounding::same(g.radius(0.8)),
                egui::Color32::TRANSPARENT,
                regular,
            );
        }
    }
}

fn draw_gear(painter: &egui::Painter, g: Glyph, stroke: egui::Stroke) {
    let mut points = Vec::with_capacity(32);
    for tooth in 0..8 {
        let base = tooth as f32 * std::f32::consts::TAU / 8.0 - std::f32::consts::FRAC_PI_2;
        for (offset, radius) in [(-0.30, 5.1), (-0.17, 7.5), (0.17, 7.5), (0.30, 5.1)] {
            let angle = base + offset;
            points.push(g.p(12.0 + radius * angle.cos(), 12.0 + radius * angle.sin()));
        }
    }
    round_polyline(painter, &points, true, stroke);
    painter.circle_stroke(g.p(12.0, 12.0), g.radius(2.7), stroke);
}

fn draw_refresh(painter: &egui::Painter, g: Glyph, stroke: egui::Stroke) {
    round_arc(painter, g, 12.0, 12.0, 7.1, -2.55, 0.35, stroke);
    round_arc(painter, g, 12.0, 12.0, 7.1, 0.60, 3.50, stroke);
    round_polyline(
        painter,
        &[g.p(18.4, 6.0), g.p(20.0, 8.2), g.p(17.0, 8.5)],
        false,
        stroke,
    );
    round_polyline(
        painter,
        &[g.p(5.6, 18.0), g.p(4.0, 15.8), g.p(7.0, 15.5)],
        false,
        stroke,
    );
}

fn draw_check(painter: &egui::Painter, g: Glyph, stroke: egui::Stroke) {
    round_polyline(
        painter,
        &[g.p(6.5, 12.2), g.p(10.2, 15.8), g.p(17.8, 8.0)],
        false,
        stroke,
    );
}

fn draw_copy(painter: &egui::Painter, g: Glyph, stroke: egui::Stroke) {
    painter.rect(
        g.rect(8.0, 3.8, 19.2, 15.0),
        egui::Rounding::same(g.radius(2.2)),
        egui::Color32::TRANSPARENT,
        stroke,
    );
    painter.rect(
        g.rect(4.8, 8.8, 16.0, 20.0),
        egui::Rounding::same(g.radius(2.2)),
        egui::Color32::TRANSPARENT,
        stroke,
    );
}

fn draw_brain(painter: &egui::Painter, g: Glyph, stroke: egui::Stroke, thin: egui::Stroke) {
    round_cubic(
        painter,
        [
            g.p(12.0, 19.4),
            g.p(6.1, 20.3),
            g.p(3.2, 16.7),
            g.p(4.8, 11.4),
        ],
        stroke,
    );
    round_cubic(
        painter,
        [g.p(4.8, 11.4), g.p(3.7, 6.5), g.p(7.8, 3.4), g.p(12.0, 6.1)],
        stroke,
    );
    round_cubic(
        painter,
        [
            g.p(12.0, 19.4),
            g.p(17.9, 20.3),
            g.p(20.8, 16.7),
            g.p(19.2, 11.4),
        ],
        stroke,
    );
    round_cubic(
        painter,
        [
            g.p(19.2, 11.4),
            g.p(20.3, 6.5),
            g.p(16.2, 3.4),
            g.p(12.0, 6.1),
        ],
        stroke,
    );
    round_line(painter, g.p(12.0, 6.1), g.p(12.0, 19.4), thin);
    round_cubic(
        painter,
        [
            g.p(7.0, 10.0),
            g.p(9.3, 8.0),
            g.p(10.0, 13.5),
            g.p(8.4, 15.4),
        ],
        thin,
    );
    round_cubic(
        painter,
        [
            g.p(17.0, 10.0),
            g.p(14.7, 8.0),
            g.p(14.0, 13.5),
            g.p(15.6, 15.4),
        ],
        thin,
    );
}

fn draw_pencil(painter: &egui::Painter, g: Glyph, stroke: egui::Stroke, thin: egui::Stroke) {
    round_line(painter, g.p(5.0, 19.0), g.p(16.8, 7.2), stroke);
    round_line(painter, g.p(14.7, 5.1), g.p(18.9, 9.3), stroke);
    round_polyline(
        painter,
        &[g.p(5.0, 19.0), g.p(5.7, 15.7), g.p(8.3, 18.3)],
        true,
        thin,
    );
}

fn draw_trash(painter: &egui::Painter, g: Glyph, stroke: egui::Stroke, thin: egui::Stroke) {
    round_line(painter, g.p(5.2, 6.3), g.p(18.8, 6.3), stroke);
    round_line(painter, g.p(9.2, 3.9), g.p(14.8, 3.9), stroke);
    let body = [
        g.p(7.0, 8.3),
        g.p(8.0, 20.0),
        g.p(16.0, 20.0),
        g.p(17.0, 8.3),
    ];
    round_polyline(painter, &body, true, stroke);
    for x in [10.3, 13.7] {
        round_line(painter, g.p(x, 11.0), g.p(x, 17.3), thin);
    }
}

fn draw_paperclip(painter: &egui::Painter, g: Glyph, stroke: egui::Stroke) {
    round_cubic(
        painter,
        [
            g.p(8.0, 19.2),
            g.p(3.8, 15.0),
            g.p(8.8, 5.0),
            g.p(14.4, 5.4),
        ],
        stroke,
    );
    round_cubic(
        painter,
        [
            g.p(14.4, 5.4),
            g.p(20.5, 5.8),
            g.p(20.0, 15.5),
            g.p(13.6, 19.2),
        ],
        stroke,
    );
    round_cubic(
        painter,
        [
            g.p(13.6, 19.2),
            g.p(8.7, 22.0),
            g.p(6.6, 17.5),
            g.p(9.7, 13.4),
        ],
        stroke,
    );
}

fn draw_archive_box(painter: &egui::Painter, g: Glyph, stroke: egui::Stroke, thin: egui::Stroke) {
    painter.rect(
        g.rect(3.8, 7.5, 20.2, 19.8),
        egui::Rounding::same(g.radius(2.4)),
        egui::Color32::TRANSPARENT,
        stroke,
    );
    painter.rect(
        g.rect(2.8, 4.2, 21.2, 8.2),
        egui::Rounding::same(g.radius(1.8)),
        egui::Color32::TRANSPARENT,
        stroke,
    );
    round_line(painter, g.p(9.0, 13.4), g.p(15.0, 13.4), thin);
}

fn draw_send(painter: &egui::Painter, g: Glyph, stroke: egui::Stroke) {
    let plane = [
        g.p(3.5, 4.5),
        g.p(20.6, 12.0),
        g.p(3.5, 19.5),
        g.p(7.3, 12.0),
    ];
    round_polyline(painter, &plane, true, stroke);
    round_line(painter, g.p(7.3, 12.0), g.p(14.3, 12.0), stroke);
}

fn draw_sun(painter: &egui::Painter, g: Glyph, stroke: egui::Stroke) {
    painter.circle_stroke(g.p(12.0, 12.0), g.radius(4.3), stroke);
    for angle in (0..8).map(|i| i as f32 * std::f32::consts::TAU / 8.0) {
        round_line(
            painter,
            g.p(12.0 + angle.cos() * 7.2, 12.0 + angle.sin() * 7.2),
            g.p(12.0 + angle.cos() * 9.0, 12.0 + angle.sin() * 9.0),
            stroke,
        );
    }
}

fn draw_moon(painter: &egui::Painter, g: Glyph, stroke: egui::Stroke) {
    round_cubic(
        painter,
        [
            g.p(17.8, 4.8),
            g.p(10.0, 4.4),
            g.p(5.1, 9.8),
            g.p(6.1, 15.2),
        ],
        stroke,
    );
    round_cubic(
        painter,
        [
            g.p(6.1, 15.2),
            g.p(7.1, 21.0),
            g.p(14.8, 22.0),
            g.p(19.2, 16.2),
        ],
        stroke,
    );
    round_cubic(
        painter,
        [
            g.p(19.2, 16.2),
            g.p(14.2, 17.5),
            g.p(10.4, 14.0),
            g.p(11.3, 9.1),
        ],
        stroke,
    );
    round_cubic(
        painter,
        [
            g.p(11.3, 9.1),
            g.p(12.2, 5.9),
            g.p(15.0, 4.5),
            g.p(17.8, 4.8),
        ],
        stroke,
    );
}

fn draw_sidebar(painter: &egui::Painter, g: Glyph, stroke: egui::Stroke, color: egui::Color32) {
    let body = g.rect(3.5, 4.5, 20.5, 19.5);
    painter.rect(
        body,
        egui::Rounding::same(g.radius(2.7)),
        egui::Color32::TRANSPARENT,
        stroke,
    );
    round_line(painter, g.p(9.0, 5.6), g.p(9.0, 18.4), stroke);
    painter.rect_filled(
        g.rect(5.0, 7.0, 7.5, 17.0),
        egui::Rounding::same(g.radius(0.8)),
        color.gamma_multiply(0.38),
    );
}

fn draw_inspector(painter: &egui::Painter, g: Glyph, stroke: egui::Stroke, color: egui::Color32) {
    let body = g.rect(3.5, 4.5, 20.5, 19.5);
    painter.rect(
        body,
        egui::Rounding::same(g.radius(2.7)),
        egui::Color32::TRANSPARENT,
        stroke,
    );
    round_line(painter, g.p(15.0, 5.6), g.p(15.0, 18.4), stroke);
    painter.rect_filled(
        g.rect(16.5, 7.0, 19.0, 17.0),
        egui::Rounding::same(g.radius(0.8)),
        color.gamma_multiply(0.38),
    );
}

/// 60FPS continuous circular spinner, shared by asynchronous UI states.
pub(crate) fn draw_smooth_spinner(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    color: egui::Color32,
    time: f64,
) {
    let r = radius.max(1.0);
    let base = (time as f32 * 5.4) % std::f32::consts::TAU;
    let steps = 24;
    let mut points = Vec::with_capacity(steps + 1);
    for index in 0..=steps {
        let t = index as f32 / steps as f32;
        let angle = base + t * std::f32::consts::PI * 1.55;
        points.push(center + egui::vec2(angle.cos() * r, angle.sin() * r));
    }
    let stroke = egui::Stroke::new((r * 0.25).clamp(1.1, 1.8), color);
    round_polyline(painter, &points, false, stroke);
}

/// AIOPS brand mark. This is intentionally separate from generic toolbar glyphs.
pub(super) fn draw_brand_logo(ui: &egui::Ui, rect: egui::Rect, expanded: bool, pal: &Palette) {
    let blue = egui::Color32::from_rgb(0x60, 0xa5, 0xfa);
    let mint = egui::Color32::from_rgb(0x5e, 0xea, 0xd4);
    let white = egui::Color32::from_rgb(0xf0, 0xf9, 0xff);
    let logo_width = if expanded { 91.0 } else { 27.0 };
    let logo_height = 27.0;
    let origin = egui::pos2(
        rect.center().x - logo_width / 2.0,
        rect.center().y - logo_height / 2.0,
    );
    let point = |x: f32, y: f32| origin + egui::vec2(x, y);
    let stroke = egui::Stroke::new(2.3, blue);
    let mint_stroke = egui::Stroke::new(2.3, mint);
    let left = [
        point(0.0, 18.0),
        point(7.0, 4.0),
        point(14.0, 15.0),
        point(25.0, 0.0),
    ];
    let right = [
        point(1.0, 23.0),
        point(11.0, 10.0),
        point(18.0, 20.0),
        point(27.0, 11.0),
    ];
    round_polyline(ui.painter(), &left, false, stroke);
    round_polyline(ui.painter(), &right, false, mint_stroke);
    for (pos, color) in [
        (left[0], blue),
        (left[3], blue),
        (right[0], mint),
        (right[3], mint),
    ] {
        ui.painter().circle_filled(pos, 2.1, color);
    }
    ui.painter().circle_filled(right[1], 2.3, white);
    ui.painter()
        .circle_stroke(right[1], 2.3, egui::Stroke::new(0.8, pal.side));

    if expanded {
        ui.painter().text(
            egui::pos2(origin.x + 38.0, origin.y + 10.0),
            egui::Align2::LEFT_CENTER,
            "AIOPS",
            egui::FontId::proportional(15.0),
            pal.text,
        );
        ui.painter().text(
            egui::pos2(origin.x + 39.0, origin.y + 23.0),
            egui::Align2::LEFT_CENTER,
            "DESKTOP",
            egui::FontId::proportional(super::fonts::FONT_MICRO),
            pal.dim,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_renders_at_standard_and_compact_sizes() {
        let ctx = egui::Context::default();
        ctx.begin_pass(Default::default());
        let layer = egui::LayerId::background();
        let painter = egui::Painter::new(ctx.clone(), layer, egui::Rect::EVERYTHING);
        for (index, icon) in Icon::ALL.iter().copied().enumerate() {
            draw_icon_sized(
                &painter,
                egui::pos2(16.0 + index as f32 * 2.0, 16.0),
                icon,
                egui::Color32::WHITE,
                12.0,
            );
            draw_icon_sized(
                &painter,
                egui::pos2(16.0 + index as f32 * 2.0, 38.0),
                icon,
                egui::Color32::WHITE,
                20.0,
            );
        }
        let _ = ctx.end_pass();
    }
}
