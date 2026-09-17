//! 视觉基线：暖调单色 + 极少量低饱和点色。
//!
//! 颜色、字体、间距、组件小件全部收在这里，其它模块只引用这里的常量与函数；
//! 想整体换风格，改这一个文件就够。

use egui::{
    Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Galley, Margin,
    Mesh, Pos2, Rect, RichText, Shape, Stroke, StrokeKind, TextStyle, Ui, Vec2,
};
use std::sync::Arc;

// ────────────────────────────── 色板 ──────────────────────────────

/// 页面底色：暖白，比纯白柔一点
pub const CANVAS: Color32 = Color32::from_rgb(0xFB, 0xFB, 0xFA);
/// 面板 / 卡片底色
pub const SURFACE: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
/// 侧栏底色：暖骨白
pub const SIDEBAR: Color32 = Color32::from_rgb(0xF7, 0xF6, 0xF3);
/// 悬停时浮起的暖底
pub const HOVER: Color32 = Color32::from_rgb(0xF1, 0xF0, 0xEB);
/// 按下 / 选中时的暖底
pub const PRESSED: Color32 = Color32::from_rgb(0xE7, 0xE6, 0xE0);
/// 结构线：1px 分隔与描边
pub const HAIRLINE: Color32 = Color32::from_rgb(0xEA, 0xEA, 0xEA);

/// 正文墨色（不用纯黑，屏幕上看久一点不刺眼）
pub const INK: Color32 = Color32::from_rgb(0x2F, 0x34, 0x37);
/// 强调墨色：选中块、主按钮
pub const INK_DEEP: Color32 = Color32::from_rgb(0x11, 0x11, 0x11);
/// 次要文字（4.5:1 以上，够 AA）；再浅就不该承载信息了
pub const MUTED: Color32 = Color32::from_rgb(0x6E, 0x6D, 0x69);
/// 最弱的文字：只给装饰用（字距、星期缩写、空态说明），不承载必须看清的信息
/// （它的对比度只有 1.9:1，放正文就是在难为人）
pub const FAINT: Color32 = Color32::from_rgb(0xB9, 0xB7, 0xB2);

// 点色：全应用只有两个，都只用于"必须立刻被看见"的状态，不做装饰
//
//   柔和红 —— 出错（提示条）；周末数字是它的深色版，属于日历惯例
//   柔和黄 —— 刚删掉、可撤销
//
// 其余状态一律用墨色深浅表达（见 `state`）：
// 想加点色之前先问一句，这是不是"出错"？不是就别加。
/// 柔和红：错误提示条 / 周末的日期数字
pub const RED_BG: Color32 = Color32::from_rgb(0xFD, 0xEB, 0xEC);
pub const RED_FG: Color32 = Color32::from_rgb(0x9F, 0x2F, 0x2D);
/// 柔和黄：撤销提示条
pub const YELLOW_BG: Color32 = Color32::from_rgb(0xFB, 0xF3, 0xDB);
pub const YELLOW_FG: Color32 = Color32::from_rgb(0x95, 0x64, 0x00);

/// 选中文字的高亮：暖沙色，像纸上的荧光笔；暖系里本来就有这个位置，不必引第五个色系
pub const SELECTION_BG: Color32 = Color32::from_rgb(0xF3, 0xE8, 0xCE);
/// 周末的日期数字
pub const WEEKEND: Color32 = RED_FG;

// ────────────────────────────── 字体 ──────────────────────────────
//
// 三种角色，各给一串候选路径，取系统上第一个存在的文件：
//   display —— 衬线，只给日期、月份、品牌这类大字号用，负责"编辑感"
//   sans    —— 无衬线，正文与界面文字
//   mono    —— 等宽，日号、字数、键位这类"仪表读数"
//
// 每个角色都配了中文字体兜底，避免中文变成方块。

/// 界面无衬线（拉丁部分）
const SANS_LATIN: &[&str] = &[
    "C:/Windows/Fonts/segoeui.ttf",
    "/System/Library/Fonts/SFNS.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
];
/// 界面无衬线加粗（拉丁部分）
const SANS_BOLD_LATIN: &[&str] = &[
    "C:/Windows/Fonts/segoeuib.ttf",
    "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
];
/// 衬线（拉丁部分）：Georgia 的字母有点老派编辑部的味道
const SERIF_LATIN: &[&str] = &[
    "C:/Windows/Fonts/georgia.ttf",
    "/System/Library/Fonts/Supplemental/Georgia.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf",
];
/// 等宽
const MONO: &[&str] = &[
    "C:/Windows/Fonts/consola.ttf",
    "/System/Library/Fonts/SFNSMono.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
];
/// 中文无衬线
const CJK_SANS: &[&str] = &[
    "C:/Windows/Fonts/msyh.ttc",
    "/System/Library/Fonts/PingFang.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
];
/// 中文无衬线加粗
const CJK_SANS_BOLD: &[&str] = &[
    "C:/Windows/Fonts/msyhbd.ttc",
    "/System/Library/Fonts/PingFang.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc",
];
/// 中文衬线：思源宋体 → 华文中宋 → 宋体
const CJK_SERIF: &[&str] = &[
    "C:/Windows/Fonts/Source Han Serif SC Heavy (TrueType).ttf",
    "C:/Windows/Fonts/STZHONGS.TTF",
    "C:/Windows/Fonts/simsun.ttc",
    "/System/Library/Fonts/Supplemental/Songti.ttc",
    "/usr/share/fonts/opentype/noto/NotoSerifCJK-Regular.ttc",
];

/// 衬线家族名：日期、月份这类大字号走它
const FAMILY_DISPLAY: &str = "display";
/// 加粗无衬线家族名：按钮、标签走它（egui 不会合成粗体，得单独加载粗体文件）
const FAMILY_SANS_BOLD: &str = "sans-bold";

/// 衬线：给大字号和日期用
pub fn display(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FAMILY_DISPLAY.into()))
}

/// 无衬线：正文
pub fn sans(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// 无衬线加粗：按钮、小标签
pub fn sans_bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FAMILY_SANS_BOLD.into()))
}

/// 等宽：日号、字数、键位
pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// 按候选路径顺序读字体文件，读到的第一个作为该角色注册。
fn load_role(fonts: &mut FontDefinitions, key: &str, candidates: &[&str]) -> Vec<String> {
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert(key.to_owned(), FontData::from_owned(bytes).into());
            return vec![key.to_owned()];
        }
    }
    Vec::new()
}

/// 装字体：挑中的字体排在最前，egui 自带的留在后面兜底。
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    let mut sans = load_role(&mut fonts, "sans", SANS_LATIN);
    sans.extend(load_role(&mut fonts, "sans-cjk", CJK_SANS));

    let mut sans_bold = load_role(&mut fonts, "sans-bold", SANS_BOLD_LATIN);
    sans_bold.extend(load_role(&mut fonts, "sans-bold-cjk", CJK_SANS_BOLD));

    let mut serif = load_role(&mut fonts, "serif", SERIF_LATIN);
    serif.extend(load_role(&mut fonts, "serif-cjk", CJK_SERIF));

    let mut mono = load_role(&mut fonts, "mono", MONO);
    mono.extend(load_role(&mut fonts, "mono-cjk", CJK_SANS));

    if !sans.iter().any(|key| key.ends_with("cjk")) {
        eprintln!("未找到中文字体文件，中文可能显示为方块");
    }

    for (family, keys) in [
        (FontFamily::Proportional, sans),
        (FontFamily::Monospace, mono),
    ] {
        let list = fonts.families.entry(family).or_default();
        for key in keys.into_iter().rev() {
            list.insert(0, key);
        }
    }

    for (family, keys) in [
        (FontFamily::Name(FAMILY_DISPLAY.into()), serif),
        (FontFamily::Name(FAMILY_SANS_BOLD.into()), sans_bold),
    ] {
        if !keys.is_empty() {
            fonts.families.insert(family, keys);
        }
    }

    ctx.set_fonts(fonts);
}

// ────────────────────────────── 样式 ──────────────────────────────

/// 装主题：字体 + 全局样式。启动时调一次。
pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);

    // 只用浅色那一套，不跟随系统的深色模式
    ctx.set_theme(egui::ThemePreference::Light);

    // 两套主题（浅 / 深）共用同一份样式：系统切到深色时界面外观不变
    ctx.all_styles_mut(|style| {
        // 排版：字号梯度拉开，标题用衬线，读起来有"文档感"
        style.text_styles = [
            (TextStyle::Heading, display(26.0)),
            (TextStyle::Body, sans(15.0)),
            (TextStyle::Button, sans_bold(14.0)),
            (TextStyle::Small, sans(12.0)),
            (TextStyle::Monospace, mono(13.0)),
        ]
        .into();

        // 间距：整体松一点，留白当结构用
        style.spacing.item_spacing = Vec2::new(10.0, 8.0);
        style.spacing.button_padding = Vec2::new(12.0, 5.0);
        style.spacing.interact_size = Vec2::new(26.0, 24.0);
        style.spacing.menu_margin = Margin::same(8);
        style.spacing.window_margin = Margin::same(18);
        style.spacing.indent = 18.0;
        style.spacing.scroll = egui::style::ScrollStyle {
            floating: true,
            bar_width: 9.0,
            floating_width: 3.0,
            handle_min_length: 32.0,
            bar_inner_margin: 3.0,
            bar_outer_margin: 4.0,
            foreground_color: false,
            dormant_background_opacity: 0.0,
            ..Default::default()
        };
        style.animation_time = 0.18;
        style.interaction.selectable_labels = false;

        style.visuals = visuals();
    });
}

/// 全局配色与控件状态
fn visuals() -> egui::Visuals {
    let mut v = egui::Visuals::light();

    v.panel_fill = CANVAS;
    v.window_fill = SURFACE;
    v.window_stroke = Stroke::new(1.0, HAIRLINE);
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_shadow = soft_shadow();
    v.popup_shadow = soft_shadow();

    v.faint_bg_color = SIDEBAR;
    v.extreme_bg_color = SURFACE;
    v.text_edit_bg_color = Some(Color32::TRANSPARENT);
    v.code_bg_color = SIDEBAR;

    // 全应用没有链接，超链接色就归到墨色，免得点色表里多出一个用不上的蓝
    v.hyperlink_color = INK;
    v.warn_fg_color = YELLOW_FG;
    v.error_fg_color = RED_FG;

    // 次要文字给一个确定的灰，而不是靠透明度算
    v.weak_text_color = Some(MUTED);

    // 选中：暖沙底 + 墨色文字
    v.selection = egui::style::Selection {
        bg_fill: SELECTION_BG,
        stroke: Stroke::new(1.0, INK),
    };
    v.text_cursor = egui::style::TextCursorStyle {
        stroke: Stroke::new(1.5, INK_DEEP),
        preview: true,
        blink: true,
        ..Default::default()
    };

    // 控件：默认完全扁平，只有悬停 / 按下才浮出一层暖色
    let flat = |fill: Color32, fg: Color32, radius: u8| egui::style::WidgetVisuals {
        bg_fill: fill,
        weak_bg_fill: fill,
        bg_stroke: Stroke::NONE,
        corner_radius: CornerRadius::same(radius),
        fg_stroke: Stroke::new(1.0, fg),
        expansion: 0.0,
    };
    v.widgets.noninteractive = flat(SURFACE, INK, 8);
    v.widgets.inactive = flat(Color32::TRANSPARENT, INK, 6);
    v.widgets.hovered = flat(HOVER, INK_DEEP, 6);
    v.widgets.active = flat(PRESSED, INK_DEEP, 6);
    v.widgets.open = flat(HOVER, INK_DEEP, 6);

    v.indent_has_left_vline = false;
    v.striped = false;
    v.interact_cursor = Some(egui::CursorIcon::PointingHand);

    v
}

/// 极淡的投影：只用来说清"这层浮在上面"，几乎看不见影子
fn soft_shadow() -> egui::epaint::Shadow {
    egui::epaint::Shadow {
        offset: [0, 2],
        blur: 12,
        spread: 0,
        color: Color32::from_rgba_premultiplied(0, 0, 0, 10),
    }
}

// ────────────────────────────── 小件 ──────────────────────────────

/// 把 `top` 以 `t` 的不透明度调进 `base`，得到不透明结果。
///
/// 比直接叠两层半透明更可控：底色已知，不用怕叠出来发灰。
pub fn mix(base: Color32, top: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0) * (top.a() as f32 / 255.0);
    let f = |b: u8, o: u8| (b as f32 * (1.0 - t) + o as f32 * t).round() as u8;
    Color32::from_rgb(
        f(base.r(), top.r()),
        f(base.g(), top.g()),
        f(base.b(), top.b()),
    )
}

/// 画一条 1px 结构线，并占掉 1px 的高度
pub fn hairline(ui: &mut Ui) -> Rect {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 1.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::ZERO, HAIRLINE);
    rect
}

/// 带字距的文字：小字号拉开一点更好认
fn tracked(ui: &Ui, text: &str, font: FontId, tracking: f32) -> Arc<Galley> {
    let mut job =
        egui::text::LayoutJob::simple_singleline(text.to_owned(), font, Color32::PLACEHOLDER);
    for section in &mut job.sections {
        section.format.extra_letter_spacing = tracking;
    }
    ui.painter().layout_job(job)
}

/// 一条 galley 的基线离它的顶边多远：第一行的第一个字形落在哪。
///
/// `egui` 不直接给这个数，而按基线摆文字恰恰需要它（见 [`line`]）；
/// 日记的装订线也要靠它跟正文压到同一条线上。
pub(crate) fn baseline(galley: &Galley) -> f32 {
    galley.rows.first().map_or(0.0, |row| {
        row.pos.y + row.row.glyphs.first().map_or(0.0, |glyph| glyph.pos.y)
    })
}

// ────────────────────────────── 一行里的几段 ──────────────────────────────
//
// 一行里的小件一律按基线摆，不用 `ui.horizontal`：它按外框居中，而拉丁与中文的
// 外框比例不同，混排或字号不同就会错开 2~3pt（取舍见 `docs/architecture.md`
// 的"为什么一行小件按基线摆"）。

/// 一行里的一段：文字、键帽、按钮或竖分隔线。用 [`Piece::text`] 这类构造函数拼。
///
/// **一段里只放一种脚本。** 一段里混排时，中文会兜底到同一 family 里的另一套字体，
/// 两套字体的外框比例不同，仍然会错开——中英混排先切段（[`mixed_pieces`]）。
pub struct Piece<'a> {
    text: &'a str,
    font: FontId,
    color: Color32,
    /// 与上一段之间的空隙（第一段看不见它）
    gap: f32,
    /// 字距：只有状态字要拉开一点
    tracking: f32,
    skin: Skin,
}

/// 一段画成什么样子
#[derive(Clone, Copy, PartialEq, Eq)]
enum Skin {
    /// 就是文字，没有底也没有边
    Text,
    /// 键帽：实体按键的样子，给快捷键提示用
    Key,
    /// 幽灵按钮：平时看不见，悬停浮出一层暖色，可点
    Button,
    /// 竖分隔线，1px 宽（不看 `text`）
    Divider,
}

/// 整行贴哪边
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    /// 只占自己那点宽度，后面还能接着摆东西（正文里的标签、日历标题）
    Inline,
    /// 占满可用宽度、内容贴右（顶栏、底栏的右侧那一簇）
    Right,
    /// 占满可用宽度，第一段贴左、最后一段贴右，空隙均分（侧栏页脚）
    Spread,
}

/// 键帽的左右内边距
const KEY_PAD_X: f32 = 6.0;
/// 键帽相对基线的高度：上沿 1.19em、下沿 0.53em，正好包住拉丁的大写字身
const KEY_ABOVE: f32 = 1.19;
const KEY_BELOW: f32 = 0.53;
/// 幽灵按钮的左右内边距
const BUTTON_PAD_X: f32 = 10.0;
/// 幽灵按钮的上下内边距
const BUTTON_PAD_Y: f32 = 6.0;
/// 状态字（[`Piece::state`]）的字距
const STATE_TRACKING: f32 = 0.6;
/// 竖分隔线的半高
const DIVIDER_ABOVE: f32 = 8.0;

impl<'a> Piece<'a> {
    /// 一段普通文字
    pub fn text(text: &'a str, font: FontId, color: Color32) -> Self {
        Self {
            text,
            font,
            color,
            gap: 0.0,
            tracking: 0.0,
            skin: Skin::Text,
        }
    }

    /// 小号状态字：等宽、略拉字距，没有底、没有边、没有胶囊。
    ///
    /// 状态就是文字本身：深浅定轻重（`INK_DEEP` 是"当前"，`MUTED` 是次要读数），
    /// 字号与字距把它和旁边的正文、日期戳分开。
    pub fn state(text: &'a str, color: Color32) -> Self {
        Self {
            tracking: STATE_TRACKING,
            ..Self::text(text, mono(11.0), color)
        }
    }

    /// 键帽：快捷键提示里的那个键
    pub fn key(text: &'a str) -> Self {
        Self {
            skin: Skin::Key,
            ..Self::text(text, mono(10.5), MUTED)
        }
    }

    /// 幽灵按钮：可点，悬停时文字变深、浮出一层暖色
    pub fn button(text: &'a str) -> Self {
        Self {
            skin: Skin::Button,
            ..Self::text(text, sans_bold(12.5), INK)
        }
    }

    /// 竖分隔线
    pub fn divider() -> Self {
        Self {
            skin: Skin::Divider,
            ..Self::text("", mono(10.5), HAIRLINE)
        }
    }

    /// 与上一段之间的空隙
    pub fn gap(mut self, gap: f32) -> Self {
        self.gap = gap;
        self
    }
}

/// 摆一行，返回每一段的响应（顺序与 `pieces` 一致）。
///
/// 每段先量一遍：宽度，以及画出来那一块在基线上下各伸多少
/// （键帽含它那圈底、按钮含内边距、文字就是它自己的外框）。
/// 再把这些块压到同一条基线上；整行的高度取它们的并集，由调用方所在的布局（比如顶栏）去居中。
pub fn line(ui: &mut Ui, edge: Edge, pieces: &[Piece<'_>]) -> Vec<egui::Response> {
    /// 量出来的一段：宽度、基线以上多少、以下多少
    struct Measured {
        galley: Option<Arc<Galley>>,
        width: f32,
        above: f32,
        below: f32,
    }

    let measured: Vec<Measured> = pieces
        .iter()
        .map(|piece| {
            if piece.skin == Skin::Divider {
                return Measured {
                    galley: None,
                    width: 1.0,
                    above: DIVIDER_ABOVE,
                    below: DIVIDER_ABOVE,
                };
            }
            let galley = if piece.tracking == 0.0 {
                ui.painter().layout_no_wrap(
                    piece.text.to_owned(),
                    piece.font.clone(),
                    Color32::PLACEHOLDER,
                )
            } else {
                tracked(ui, piece.text, piece.font.clone(), piece.tracking)
            };
            let base = baseline(&galley);
            let (pad_x, above, below) = match piece.skin {
                Skin::Key => (
                    KEY_PAD_X,
                    piece.font.size * KEY_ABOVE,
                    piece.font.size * KEY_BELOW,
                ),
                Skin::Button => (
                    BUTTON_PAD_X,
                    base + BUTTON_PAD_Y,
                    galley.size().y - base + BUTTON_PAD_Y,
                ),
                _ => (0.0, base, galley.size().y - base),
            };
            Measured {
                width: galley.size().x + pad_x * 2.0,
                above,
                below,
                galley: Some(galley),
            }
        })
        .collect();

    let above = measured.iter().map(|m| m.above).fold(0.0, f32::max);
    let below = measured.iter().map(|m| m.below).fold(0.0, f32::max);
    let height = above + below;
    let gaps: f32 = pieces.iter().skip(1).map(|piece| piece.gap).sum();
    let content: f32 = measured.iter().map(|m| m.width).sum();

    // 两端分开时，把多出来的宽度均分进每个缝里
    let extra = if edge == Edge::Spread && pieces.len() > 1 {
        ((ui.available_width() - content - gaps) / (pieces.len() - 1) as f32).max(0.0)
    } else {
        0.0
    };
    let span = content + gaps + extra * pieces.len().saturating_sub(1) as f32;
    let width = if edge == Edge::Inline {
        span
    } else {
        ui.available_width().max(span)
    };
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::hover());

    let baseline_y = rect.top() + above;
    let mut x = rect.left() + (width - span).max(0.0);
    let mut responses = Vec::with_capacity(pieces.len());

    for (index, (piece, size)) in pieces.iter().zip(&measured).enumerate() {
        if index > 0 {
            x += piece.gap + extra;
        }
        // 这一段画出来的那一块，也是可交互的范围
        let drawn = Rect::from_min_max(
            egui::pos2(x, baseline_y - size.above),
            egui::pos2(x + size.width, baseline_y + size.below),
        );
        let sense = if piece.skin == Skin::Button {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        };
        let response = ui.interact(
            drawn,
            ui.id().with(("line_piece", index, piece.text)),
            sense,
        );

        let painter = ui.painter();
        // 文字都按自己的基线落位；只有键帽与按钮的底需要先铺（见上：它们的盒比字大）
        if piece.skin == Skin::Key {
            painter.rect_filled(drawn, CornerRadius::same(4), SIDEBAR);
            painter.rect_stroke(
                drawn.shrink(0.5),
                CornerRadius::same(4),
                Stroke::new(1.0, HAIRLINE),
                StrokeKind::Inside,
            );
        }
        if piece.skin == Skin::Button {
            let lift = ui
                .ctx()
                .animate_bool_with_time(response.id, response.hovered(), 0.12);
            if lift > 0.001 {
                painter.rect_filled(drawn, CornerRadius::same(6), HOVER.gamma_multiply(lift));
            }
        }

        if let Some(galley) = &size.galley {
            // 有底的那两种要把字摆进自己的盒里，普通文字就贴着段首
            let text_x = match piece.skin {
                Skin::Key => x + (size.width - galley.size().x) / 2.0,
                Skin::Button => x + BUTTON_PAD_X,
                _ => x,
            };
            let color = if piece.skin == Skin::Button && response.hovered() {
                INK_DEEP
            } else {
                piece.color
            };
            painter.galley(
                egui::pos2(text_x, baseline_y - baseline(galley)),
                galley.clone(),
                color,
            );
        } else {
            painter.rect_filled(drawn, CornerRadius::ZERO, HAIRLINE);
        }

        x += size.width;
        responses.push(response);
    }

    responses
}

/// 中英混排的一行文字：拉丁与中文各自成段，压在同一条基线上。
///
/// 等于把 [`mixed_pieces`] 交给 [`line`]（贴左，只占自己那点宽度）；
/// 正文里的 `年 / 月 / 日`、日期戳这类标签用它替代 `ui.label`。
pub fn mixed(ui: &mut Ui, text: &str, font: FontId, color: Color32) {
    line(ui, Edge::Inline, &mixed_pieces(text, font, color));
}

/// 把一段文本切成若干段：连续的中文归一段，其余（数字、英文、空格、标点）归另一段。
///
/// 段与段之间不留空隙——间隙来自文本里的空格，切段本身不改变排版。
/// 需要和键帽、分隔线拼成一行时，用 [`line`] 把切出来的段接在后面。
pub fn mixed_pieces<'a>(text: &'a str, font: FontId, color: Color32) -> Vec<Piece<'a>> {
    let mut pieces: Vec<Piece<'a>> = Vec::new();
    let mut start = 0;
    let mut current: Option<bool> = None;
    for (index, ch) in text.char_indices() {
        let cjk = is_cjk(ch);
        match current {
            Some(script) if script == cjk => {}
            Some(_) => {
                pieces.push(Piece::text(&text[start..index], font.clone(), color));
                start = index;
                current = Some(cjk);
            }
            None => current = Some(cjk),
        }
    }
    if current.is_some() {
        pieces.push(Piece::text(&text[start..], font, color));
    }
    pieces
}

/// 是不是汉字（含中文标点）：这段文字该走中文字体，
/// 也就是在同一个 family 里要单独成段、单独对齐基线。
fn is_cjk(ch: char) -> bool {
    matches!(ch as u32,
        0x3000..=0x303F   // 中文标点
        | 0x3040..=0x30FF // 假名
        | 0x3400..=0x4DBF // 扩展 A
        | 0x4E00..=0x9FFF // 基本区
        | 0xF900..=0xFAFF // 兼容汉字
        | 0xFF00..=0xFFEF // 全角
        | 0x20000..=0x2FA1F // 扩展 B 及以后
    )
}

// ────────────────────────────── 圆角与层次 ──────────────────────────────
//
// 一套尺子，容器越大越圆，不混用：
//   6  —— 控件与格子（按钮、日历格、键帽）
//   8  —— 行高亮
//   10 —— 浮动提示条
//   12 —— 卡片 / 窗口
//
// 没有胶囊：状态不用框包起来，就是文字本身（见 `Piece::state`）。
// 想强调"当前"就用实心墨块（日历上选中的那天、主按钮、光标墨条），
// 不要靠加色，也不要靠加形状。

/// 只能文字的小按钮：平时看不见，悬停时浮出一层暖色。
///
/// 就是 [`Piece::button`] 单独摆一行（见 [`line`]）；要和别的文字拼成一行时，
/// 直接把它放进 [`line`]，免得基线对不上。
pub fn ghost_button(ui: &mut Ui, text: &str) -> egui::Response {
    line(ui, Edge::Inline, &[Piece::button(text)])
        .pop()
        .unwrap_or_else(|| ui.allocate_response(Vec2::ZERO, egui::Sense::click()))
}

/// 方形图标按钮：同样无底色，悬停才浮出一层暖色
pub fn icon_button(ui: &mut Ui, glyph: &str, size: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::click());
    let t = ui
        .ctx()
        .animate_bool_with_time(response.id, response.hovered(), 0.12);
    let painter = ui.painter();
    if t > 0.001 {
        painter.rect_filled(rect, CornerRadius::same(6), HOVER.gamma_multiply(t));
    }
    let color = if response.hovered() { INK_DEEP } else { MUTED };
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        glyph,
        display(size * 0.62),
        color,
    );
    response
}

/// 主按钮：墨底白字，全应用里最重的一个控件，一屏只该有一个
pub fn primary_button(ui: &mut Ui, text: &str) -> egui::Response {
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_owned(), sans_bold(13.0), Color32::PLACEHOLDER);
    let (rect, response) =
        ui.allocate_exact_size(galley.size() + Vec2::new(30.0, 16.0), egui::Sense::click());
    let lift = ui
        .ctx()
        .animate_bool_with_time(response.id, response.hovered(), 0.12);
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        CornerRadius::same(6),
        mix(INK_DEEP, Color32::WHITE, lift * 0.16),
    );
    painter.galley(rect.center() - galley.size() / 2.0, galley, Color32::WHITE);
    response
}

/// 细进度条：2px 轨道 + 墨色填充
pub fn meter(ui: &mut Ui, ratio: f32) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 3.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(2), HAIRLINE);
    let ratio = ratio.clamp(0.0, 1.0);
    if ratio > 0.0 {
        let filled = Rect::from_min_size(
            rect.min,
            Vec2::new((rect.width() * ratio).max(3.0), rect.height()),
        );
        painter.rect_filled(filled, CornerRadius::same(2), INK);
    }
}

/// 极淡的暖色光晕：给大面积留白一点层次，不制造噪点
pub fn glow(painter: &egui::Painter, center: Pos2, radius: f32, color: Color32) {
    const SEGMENTS: usize = 48;
    let mut mesh = Mesh::default();
    mesh.colored_vertex(center, color);
    for i in 0..=SEGMENTS {
        let angle = std::f32::consts::TAU * i as f32 / SEGMENTS as f32;
        let pos = center + Vec2::new(angle.cos(), angle.sin()) * radius;
        mesh.colored_vertex(pos, Color32::TRANSPARENT);
        if i > 0 {
            mesh.add_triangle(0, i as u32, (i + 1) as u32);
        }
    }
    painter.add(Shape::mesh(mesh));
}

/// 居中的正文列：把内容限制在一条固定宽度的栏里，两侧留白
pub fn column<R>(ui: &mut Ui, max_width: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let avail = ui.available_width();
    // 窗口再窄也留一点页边距，正文不要贴着窗边
    let pad = ((avail - max_width) / 2.0).max(26.0);
    let rect = Rect::from_min_size(
        ui.cursor().min + Vec2::new(pad, 0.0),
        Vec2::new((avail - pad * 2.0).max(0.0), ui.available_height()),
    );
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), add)
        .inner
}

/// 空状态：一句衬线大字 + 一句说明，都居中
///
/// 两行都用 `MUTED`：这是空页面里唯一的信息，得读得清；
/// 层次交给字号差（19 对 12），不靠把字调淡。
pub fn empty_hint(ui: &mut Ui, title: &str, subtitle: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space(52.0);
        ui.label(RichText::new(title).font(display(19.0)).color(MUTED));
        ui.add_space(8.0);
        ui.label(RichText::new(subtitle).font(sans(12.0)).color(MUTED));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 画一帧，把每一段文字落在屏幕上的基线读出来。
    ///
    /// 测试里没有渲染后端，只能从录入的文字图元反推：`Shape::Text` 带位置，
    /// 位置加上 galley 自己的基线偏移就是这条基线。
    fn baselines(ctx: &egui::Context, add: impl FnMut(&mut Ui)) -> Vec<f32> {
        let mut output = ctx.run_ui(egui::RawInput::default(), add);
        output.textures_delta.clear();
        output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) => {
                    let row = text.galley.rows.first()?;
                    let glyph = row.row.glyphs.first()?;
                    Some(text.pos.y + row.pos.y + glyph.pos.y)
                }
                _ => None,
            })
            .collect()
    }

    /// 一行里的每段文字都压在同一条基线上：拉丁、中文、等宽、衬线、键帽、按钮，各是一种字体
    #[test]
    fn every_piece_shares_one_baseline() {
        let ctx = egui::Context::default();
        install(&ctx);

        let found = baselines(&ctx, |ui| {
            line(
                ui,
                Edge::Inline,
                &[
                    Piece::text("15", mono(14.0), INK),
                    Piece::text("二", sans(10.0), FAINT).gap(4.0),
                    Piece::key("Enter").gap(10.0),
                    Piece::text("你打包", sans(15.5), INK).gap(10.0),
                    Piece::divider().gap(10.0),
                    Piece::button("锁定").gap(6.0),
                ],
            );
        });

        assert_eq!(found.len(), 5, "五段文字都该画出来，分隔线不算：{found:?}");
        for (index, y) in found.iter().enumerate() {
            assert!(
                (y - found[0]).abs() < 0.01,
                "第 {index} 段没压在同一条基线上：{found:?}"
            );
        }
    }
}
