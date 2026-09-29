//! Theme color tokens shared by GUI panels and reusable widgets.

#[derive(Clone, Copy)]
pub(super) struct Palette {
    pub(super) bg: egui::Color32,
    pub(super) side: egui::Color32,
    pub(super) panel: egui::Color32,
    pub(super) head_fill: egui::Color32,
    pub(super) head_border: egui::Color32,
    pub(super) field: egui::Color32,
    pub(super) border: egui::Color32,
    pub(super) text: egui::Color32,
    pub(super) dim: egui::Color32,
    pub(super) accent: egui::Color32,
    pub(super) hover: egui::Color32,
    pub(super) btn_fill: egui::Color32,
    pub(super) btn_hover: egui::Color32,
    pub(super) btn_text: egui::Color32,
    pub(super) btn_border: egui::Color32,
    pub(super) user_bubble: egui::Color32,
    pub(super) user_text: egui::Color32,
    pub(super) ai_bubble: egui::Color32,
    /// 工具执行气泡底色
    #[allow(dead_code)]
    pub(super) tool_bubble: egui::Color32,
    pub(super) err_bubble: egui::Color32,
    pub(super) err_text: egui::Color32,
    pub(super) warn: egui::Color32,
    pub(super) banner_ok: egui::Color32,
    pub(super) banner_warn: egui::Color32,
    pub(super) diff_add_bg: egui::Color32,
    pub(super) diff_del_bg: egui::Color32,
    pub(super) diff_hunk_bg: egui::Color32,
    pub(super) diff_sign_add: egui::Color32,
    pub(super) diff_sign_del: egui::Color32,

    // ── 工业级 macOS / Codex 设计系统核心代币 ──
    /// 1px 发丝级分割线（macOS 半透明材质层叠体系）
    pub(crate) line: egui::Color32,
    /// 浮动卡片与执行块底色（Slate 表面层）
    pub(super) card_bg: egui::Color32,
    /// 卡片精细边框
    pub(super) card_border: egui::Color32,
    /// 思考链（Chain of Thought）容器浅底色
    pub(super) thought_bg: egui::Color32,
    /// 思考链边框
    pub(super) thought_border: egui::Color32,
    /// 工具步骤标题栏微底色
    pub(super) tool_header_bg: egui::Color32,
    /// 语义成功色（翡翠绿）
    pub(super) success: egui::Color32,
    /// 语义信息色（高亮蓝/青）
    pub(super) info: egui::Color32,
    /// 思考与 Agent 专属语义紫
    pub(super) purple: egui::Color32,
    /// 状态徽标胶囊底色
    #[allow(dead_code)]
    pub(super) badge_bg: egui::Color32,
    /// 是否为深色模式
    pub(crate) is_dark: bool,
}

impl Palette {
    /// 工业级半透明悬停叠加色（微阻尼柔光，根据 hover_t 0.0..1.0 渐进呈现，避免同色套娃）
    pub(crate) fn translucent_hover(&self, hover_t: f32) -> egui::Color32 {
        if hover_t <= 0.001 {
            return egui::Color32::TRANSPARENT;
        }
        if self.is_dark {
            egui::Color32::from_white_alpha((24.0 * hover_t).min(255.0) as u8)
        } else {
            egui::Color32::from_black_alpha((18.0 * hover_t).min(255.0) as u8)
        }
    }

    /// 工业级半透明按下叠加色（清脆深度反馈）
    pub(crate) fn translucent_active(&self) -> egui::Color32 {
        if self.is_dark {
            egui::Color32::from_white_alpha(38)
        } else {
            egui::Color32::from_black_alpha(28)
        }
    }
}

pub(super) fn palette(dark: bool) -> Palette {
    use egui::Color32 as C;
    if dark {
        Palette {
            // 现代化 Slate-Black 沉浸式暗色基调
            bg: C::from_rgb(0x0c, 0x10, 0x17),
            side: C::from_rgb(0x0f, 0x14, 0x1e),
            panel: C::from_rgb(0x13, 0x19, 0x25),
            head_fill: C::from_rgb(0x0f, 0x15, 0x20),
            head_border: C::from_white_alpha(20),
            field: C::from_rgb(0x16, 0x1f, 0x2e),
            border: C::from_white_alpha(24),
            text: C::from_rgb(0xf1, 0xf5, 0xf9),
            dim: C::from_rgb(0x94, 0xa3, 0xb8),
            // 电气青 / DeepSeek 高亮绿
            accent: C::from_rgb(0x38, 0xbd, 0xf8),
            hover: C::from_rgb(0x1a, 0x24, 0x36),
            btn_fill: C::from_rgb(0x19, 0x2a, 0x3e),
            btn_hover: C::from_rgb(0x23, 0x38, 0x53),
            btn_text: C::from_rgb(0x7d, 0xd3, 0xfc),
            btn_border: C::from_white_alpha(32),
            user_bubble: C::from_rgb(0x1c, 0x27, 0x3a),
            user_text: C::from_rgb(0xf8, 0xfa, 0xfc),
            ai_bubble: C::from_rgb(0x12, 0x18, 0x24),
            tool_bubble: C::from_rgb(0x10, 0x16, 0x22),
            err_bubble: C::from_rgb(0x35, 0x14, 0x1b),
            err_text: C::from_rgb(0xf8, 0x71, 0x71),
            warn: C::from_rgb(0xfb, 0xbf, 0x24),
            banner_ok: C::from_rgb(0x12, 0x2d, 0x23),
            banner_warn: C::from_rgb(0x33, 0x26, 0x10),
            diff_add_bg: C::from_rgb(0x13, 0x33, 0x23),
            diff_del_bg: C::from_rgb(0x3d, 0x16, 0x1b),
            diff_hunk_bg: C::from_rgb(0x15, 0x24, 0x38),
            diff_sign_add: C::from_rgb(0x34, 0xd3, 0x99),
            diff_sign_del: C::from_rgb(0xf8, 0x71, 0x71),

            line: C::from_white_alpha(18),
            card_bg: C::from_rgb(0x13, 0x1a, 0x27),
            card_border: C::from_white_alpha(20),
            thought_bg: C::from_rgb(0x14, 0x19, 0x28),
            thought_border: C::from_white_alpha(24),
            tool_header_bg: C::from_rgb(0x16, 0x20, 0x30),
            success: C::from_rgb(0x34, 0xd3, 0x99),
            info: C::from_rgb(0x60, 0xa5, 0xfa),
            purple: C::from_rgb(0xa7, 0x8b, 0xfa),
            badge_bg: C::from_rgb(0x18, 0x23, 0x34),
            is_dark: true,
        }
    } else {
        Palette {
            // 现代化 Slate 浅色高质感调色板
            bg: C::from_rgb(0xf8, 0xfa, 0xfc),
            side: C::from_rgb(0xf1, 0xf5, 0xf9),
            panel: C::WHITE,
            head_fill: C::from_rgb(0xf1, 0xf5, 0xf9),
            head_border: C::from_black_alpha(20),
            field: C::from_rgb(0xf1, 0xf5, 0xf9),
            border: C::from_black_alpha(22),
            text: C::from_rgb(0x0f, 0x17, 0x2a),
            dim: C::from_rgb(0x64, 0x74, 0x8b),
            accent: C::from_rgb(0x02, 0x84, 0xc7),
            hover: C::from_rgb(0xe2, 0xe8, 0xf0),
            btn_fill: C::from_rgb(0xe0, 0xf2, 0xfe),
            btn_hover: C::from_rgb(0xba, 0xe6, 0xfd),
            btn_text: C::from_rgb(0x03, 0x69, 0xa1),
            btn_border: C::from_black_alpha(28),
            user_bubble: C::from_rgb(0xe2, 0xe8, 0xf0),
            user_text: C::from_rgb(0x0f, 0x17, 0x2a),
            ai_bubble: C::WHITE,
            tool_bubble: C::from_rgb(0xf8, 0xfa, 0xfc),
            err_bubble: C::from_rgb(0xfe, 0xf2, 0xf2),
            err_text: C::from_rgb(0xdc, 0x26, 0x26),
            warn: C::from_rgb(0xd9, 0x77, 0x06),
            banner_ok: C::from_rgb(0xdc, 0xfc, 0xe7),
            banner_warn: C::from_rgb(0xfe, 0xf3, 0xc7),
            diff_add_bg: C::from_rgb(0xdc, 0xfc, 0xe7),
            diff_del_bg: C::from_rgb(0xfe, 0xe2, 0xe2),
            diff_hunk_bg: C::from_rgb(0xf0, 0xf9, 0xff),
            diff_sign_add: C::from_rgb(0x16, 0xa3, 0x4a),
            diff_sign_del: C::from_rgb(0xdc, 0x26, 0x26),

            line: C::from_black_alpha(16),
            card_bg: C::WHITE,
            card_border: C::from_black_alpha(18),
            thought_bg: C::from_rgb(0xf5, 0xf3, 0xff),
            thought_border: C::from_black_alpha(20),
            tool_header_bg: C::from_rgb(0xf1, 0xf5, 0xf9),
            success: C::from_rgb(0x05, 0x96, 0x69),
            info: C::from_rgb(0x25, 0x63, 0xeb),
            purple: C::from_rgb(0x7c, 0x3a, 0xed),
            badge_bg: C::from_rgb(0xe2, 0xe8, 0xf0),
            is_dark: false,
        }
    }
}
