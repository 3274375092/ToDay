//! 外壳：顶栏、底栏、侧栏页脚的布局，以及错误提示条。
//!
//! 这些函数只画界面、只返回"用户想干什么"（[`ShellAction`]）；
//! 翻日期、锁定、落盘这些副作用一律由控制器执行。

use crate::model::MonthKey;
use crate::ui::{calendar::weekday_name, theme};
use chrono::NaiveDate;
use egui::{Color32, CornerRadius, Layout, Rect, RichText, Ui, Vec2};

/// 侧栏宽度：日历只需要这么多，剩下的留给正文
pub const SIDEBAR_WIDTH: f32 = 292.0;
/// 正文列的最大宽度：一行太长不好读
pub const COLUMN_WIDTH: f32 = 720.0;
/// 顶栏 / 底栏 / 错误条高度
pub const TOP_BAR_HEIGHT: f32 = 58.0;
pub const STATUS_BAR_HEIGHT: f32 = 30.0;
/// 窄屏底栏：三枚 44pt 的按钮要装得下，比宽屏那一条高
pub const MOBILE_STATUS_BAR_HEIGHT: f32 = 58.0;
/// 窄屏断点：可用宽度窄于它就换一套外壳。
///
/// 600 是手机竖屏（360~430pt）与平板/横屏之间的常用分界；桌面窗口的最小宽度是 720，
/// 所以桌面永远走宽屏那一套（见 `window_state::MIN_SIZE`）。
pub const MOBILE_BREAKPOINT: f32 = 600.0;
/// 窄屏浮层离屏幕左右各留的余量
pub const MOBILE_MARGIN_X: f32 = 24.0;
/// 窄屏浮层的宽度上下限：小于日历排不下，大于手幅就不好按了
pub const MOBILE_SHEET_MIN_WIDTH: f32 = 280.0;
pub const MOBILE_SHEET_MAX_WIDTH: f32 = 420.0;
/// 撤销条离下面那条面板留的一道缝
const TOAST_GAP: f32 = 12.0;
/// 宽屏的空页提示：日历就在正文左边那一列
const EMPTY_HINT_WIDE: &str = "点左边日历上的任何一天，就可以开始写";
/// 窄屏的空页提示：日历收在底栏「日历」那枚按钮开的浮层里
const EMPTY_HINT_NARROW: &str = "点底栏「日历」挑一天，就可以开始写";
const ERROR_BAR_HEIGHT: f32 = 34.0;
/// 面板左右内边距
const PANEL_PADDING_X: i8 = 22;
/// 底栏那枚修饰键键帽：判据是 `Modifiers::command`，macOS 上它认的是 Cmd 不是 Ctrl
const COMMAND_KEY: &str = if cfg!(target_os = "macos") {
    "Cmd"
} else {
    "Ctrl"
};

/// 顶栏 / 底栏 / 错误条用的面板高度
pub fn error_bar_height() -> f32 {
    ERROR_BAR_HEIGHT
}

/// 面板的水平内边距
pub fn panel_padding_x() -> i8 {
    PANEL_PADDING_X
}

/// 外壳交给面板的几个数：这块面板下面要让开多少、空页提示指哪儿、放不下什么。
///
/// 面板不认识"窄屏/宽屏"（见 `docs/architecture.md` 的"为什么窄屏只换外壳"），
/// 断点由 `App::diary_screen` 判、结果经 [`Chrome::of`] 传进来。
/// 宽屏那一组取值与外壳写死它们时逐值相同，桌面行为不变。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Chrome {
    /// 空页那行提示：两种布局里"日历"不在同一个地方
    pub empty_hint: &'static str,
    /// 撤销条离窗口底边多高（让开下面那条面板 + 一道缝）
    pub bottom_inset: f32,
    /// 撤销条里放不放正文摘要（360pt 的屏上放不下）
    pub undo_preview: bool,
}

impl Chrome {
    /// 按外壳是否窄屏给出一组取值。`narrow` 由 `App::diary_screen` 算好
    /// （`available_width < MOBILE_BREAKPOINT`），面板自己不看宽度。
    pub fn of(narrow: bool) -> Self {
        if narrow {
            Self {
                empty_hint: EMPTY_HINT_NARROW,
                bottom_inset: MOBILE_STATUS_BAR_HEIGHT + TOAST_GAP,
                undo_preview: false,
            }
        } else {
            Self {
                empty_hint: EMPTY_HINT_WIDE,
                bottom_inset: STATUS_BAR_HEIGHT + TOAST_GAP,
                undo_preview: true,
            }
        }
    }
}

/// 用户点了底栏上哪个按钮：两个按钮开的是同一张浮层，区别只在焦点放在哪
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayIntent {
    /// 「日历」：浮层开在日历上
    Calendar,
    /// 「搜索」：浮层开出来，焦点直接给搜索框
    Search,
}

/// 用户在界面上表达的意图。视图不改状态，只把意图交上去。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShellAction {
    /// 点了锁定
    pub lock: bool,
    /// 点了"回到今天"
    pub goto_today: bool,
    /// 窄屏底栏：开侧栏浮层（`None` = 这一帧没点）
    pub overlay: Option<OverlayIntent>,
    /// 窄屏底栏：点了"下一天"
    pub next_day: bool,
}

impl ShellAction {
    /// 合并同一帧里几处布局交上来的意图
    pub fn merge(self, other: Self) -> Self {
        Self {
            lock: self.lock || other.lock,
            goto_today: self.goto_today || other.goto_today,
            // 同一帧既点日历又点搜索是不可能的；真出现时先来的那个算数
            overlay: self.overlay.or(other.overlay),
            next_day: self.next_day || other.next_day,
        }
    }
}

/// 保存状态：顶栏那枚状态字说什么。
///
/// 文案与颜色都在这里定，三个词都是纯中文：`Piece` 里混脚本会落到另一套字体上、破坏基线。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveState {
    /// 队列空：没有待写的东西
    Saved,
    /// 攒着等防抖
    Pending,
    /// 上次写失败，编辑还压在队里，等用户点"重试"
    Failed,
}

impl SaveState {
    /// 这一状态下顶栏显示的字
    fn word(self) -> &'static str {
        match self {
            Self::Saved => "已保存",
            Self::Pending => "保存中",
            Self::Failed => "未保存",
        }
    }

    /// 状态字的颜色：只有"没写进去"必须立刻被看见
    fn color(self) -> Color32 {
        match self {
            Self::Failed => theme::RED_FG,
            Self::Saved | Self::Pending => theme::MUTED,
        }
    }

    /// 顶栏上的一枚状态字
    fn piece(self) -> theme::Piece<'static> {
        theme::Piece::state(self.word(), self.color())
    }
}

/// 错误条上用户表达的意图。视图不改状态，只把意图交上去。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ErrorAction {
    /// 点了"知道了"：收起提示，不重试
    pub dismissed: bool,
    /// 点了"重试"：立刻再写一次
    pub retry: bool,
}

/// 面板的边，用来补 1px 结构线
#[derive(Clone, Copy)]
pub enum Edge {
    Top,
    Bottom,
    Right,
}

/// 这一天的抬头文字，例如 `2025.09.17 · 周三`
pub fn stamp(date: NaiveDate) -> String {
    format!("{} · 周{}", date.format("%Y.%m.%d"), weekday_name(date))
}

/// 给面板补一条 1px 结构线
pub fn edge_line(ui: &Ui, rect: Rect, edge: Edge) {
    let line = match edge {
        Edge::Top => Rect::from_min_size(rect.left_top(), Vec2::new(rect.width(), 1.0)),
        Edge::Bottom => Rect::from_min_size(
            egui::pos2(rect.left(), rect.bottom() - 1.0),
            Vec2::new(rect.width(), 1.0),
        ),
        Edge::Right => Rect::from_min_size(
            egui::pos2(rect.right() - 1.0, rect.top()),
            Vec2::new(1.0, rect.height()),
        ),
    };
    ui.painter()
        .rect_filled(line, CornerRadius::ZERO, theme::HAIRLINE);
}

/// 顶栏：品牌与保存状态在左，日期与锁定在右
pub fn top_bar(ui: &mut Ui, date: NaiveDate, today: NaiveDate, state: SaveState) -> ShellAction {
    let mut action = ShellAction::default();

    ui.horizontal_centered(|ui| {
        // 品牌与状态：拉丁与中文压在同一条基线上
        theme::line(
            ui,
            theme::Edge::Inline,
            &[
                theme::Piece::text("ToDay", theme::display(20.0), theme::INK_DEEP),
                theme::Piece::text("一日一记", theme::sans(11.5), theme::FAINT).gap(12.0),
                state.piece().gap(14.0),
            ],
        );

        // 右边一簇：日期戳、今天 / 回到今天、锁定。`pieces` 与 `responses` 按位置一一对应
        let stamp = stamp(date);
        let mut pieces = theme::mixed_pieces(&stamp, theme::mono(12.0), theme::MUTED);
        pieces.push(theme::Piece::divider().gap(10.0));
        let today_at = pieces.len();
        pieces.push(if date == today {
            theme::Piece::state("今天", theme::INK_DEEP).gap(10.0)
        } else {
            // 按钮自带 10pt 左内边距，这里不再留缝
            theme::Piece::button("回到今天")
        });
        let lock_at = pieces.len();
        pieces.push(theme::Piece::button("锁定"));

        let responses = theme::line(ui, theme::Edge::Right, &pieces);
        if date != today
            && let Some(response) = responses.get(today_at)
            && response.clicked()
        {
            action.goto_today = true;
        }
        if let Some(response) = responses.get(lock_at) {
            response.clone().on_hover_text("锁上日记本");
            if response.clicked() {
                action.lock = true;
            }
        }
    });

    action
}

/// 底栏：本月统计在左，键位提示在右
pub fn status_bar(ui: &mut Ui, month: MonthKey, written: usize) {
    ui.horizontal_centered(|ui| {
        theme::mixed(
            ui,
            &format!(
                "{} 年 {} 月 · 已写 {written} 天",
                month.year(),
                month.month()
            ),
            theme::mono(11.5),
            theme::MUTED,
        );

        // 键位提示：键帽在前、说明在后；一枚键一枚键帽，相邻键帽之间 4px
        theme::line(
            ui,
            theme::Edge::Right,
            &[
                theme::Piece::key(COMMAND_KEY),
                theme::Piece::key("Enter").gap(4.0),
                theme::Piece::text("下一行", theme::sans(11.5), theme::FAINT).gap(6.0),
                theme::Piece::key("右键").gap(18.0),
                theme::Piece::text("删除这一天", theme::sans(11.5), theme::FAINT).gap(6.0),
                theme::Piece::key(COMMAND_KEY).gap(18.0),
                theme::Piece::key("F").gap(4.0),
                theme::Piece::text("搜索", theme::sans(11.5), theme::FAINT).gap(6.0),
            ],
        );
    });
}

/// 窄屏顶栏：品牌与保存状态在左，"回到今天"与锁定在右。
///
/// 不摆日期戳、不摆那根竖分隔线：日期在正文的装订线里已经有了，
/// 顶栏这点宽度留给两枚按钮。摆法与 [`top_bar`] 一样（[`theme::line`] 按基线摆）。
pub fn mobile_top_bar(
    ui: &mut Ui,
    date: NaiveDate,
    today: NaiveDate,
    state: SaveState,
) -> ShellAction {
    let mut action = ShellAction::default();

    ui.horizontal_centered(|ui| {
        theme::line(
            ui,
            theme::Edge::Inline,
            &[
                theme::Piece::text("ToDay", theme::display(20.0), theme::INK_DEEP),
                state.piece().gap(10.0),
            ],
        );

        // 右边一簇：今天 / 回到今天、锁定。与宽屏同一个取舍：是今天就只写"今天"，不摆一枚按不动的按钮
        let mut pieces: Vec<theme::Piece<'_>> = Vec::new();
        let mut today_at = None;
        if date == today {
            pieces.push(theme::Piece::state("今天", theme::INK_DEEP).gap(10.0));
        } else {
            today_at = Some(pieces.len());
            pieces.push(theme::Piece::button("回到今天").gap(10.0));
        }
        let lock_at = pieces.len();
        pieces.push(theme::Piece::button("锁定"));

        let responses = theme::line(ui, theme::Edge::Right, &pieces);
        if let Some(at) = today_at
            && let Some(response) = responses.get(at)
            && response.clicked()
        {
            action.goto_today = true;
        }
        if let Some(response) = responses.get(lock_at)
            && response.clicked()
        {
            action.lock = true;
        }
    });

    action
}

/// 窄屏底栏：本月统计在左，三枚按钮在右。
///
/// 不摆键位提示：手机上没有 `Ctrl`+`Enter`、没有 `Ctrl`+`F`、也没有右键，
/// 那三个动作改由这里的按钮与长按承担。
/// `next` 是列表里的下一天：没有下一天时那枚按钮不画（摆一枚按不动的按钮只会让人以为坏了）。
pub fn mobile_status_bar(ui: &mut Ui, written: usize, next: Option<NaiveDate>) -> ShellAction {
    let mut action = ShellAction::default();

    ui.horizontal_centered(|ui| {
        theme::mixed(
            ui,
            &format!("本月已写 {written} 天"),
            theme::mono(11.5),
            theme::MUTED,
        );

        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
            // 先摆的在最右边
            if next.is_some() && theme::ghost_button(ui, "下一天").clicked() {
                action.next_day = true;
            }
            if theme::ghost_button(ui, "搜索").clicked() {
                action.overlay = Some(OverlayIntent::Search);
            }
            if theme::ghost_button(ui, "日历").clicked() {
                action.overlay = Some(OverlayIntent::Calendar);
            }
        });
    });

    action
}

/// 侧栏页脚：本月记录与进度条。
///
/// 没有"回到今天"：那个动作在顶栏（选中的不是今天时才有），这里不再重复一遍。
pub fn sidebar_footer(ui: &mut Ui, month: MonthKey, written: usize) {
    ui.add_space(18.0);
    theme::hairline(ui);
    ui.add_space(14.0);

    let total = month.days().max(1) as usize;

    // 一行两端：标签在左、读数在右，压在同一条基线上
    let written_label = format!("{written} / {total}");
    theme::line(
        ui,
        theme::Edge::Spread,
        &[
            theme::Piece::text("本月记录", theme::sans(11.5), theme::MUTED),
            theme::Piece::text(&written_label, theme::mono(11.0), theme::MUTED),
        ],
    );

    ui.add_space(9.0);
    theme::meter(ui, written as f32 / total as f32);
}

/// 错误提示条：一句人话 + 一枚按钮。
///
/// `retryable` 为真时只有"重试"、没有"知道了"：编辑还压在队里，收起提示等于把
/// 唯一的重试入口一起收走。读盘失败那种收起来不丢东西的，才给"知道了"。
/// 面板的红底由控制器铺，这里只画内容。
pub fn error_banner(ui: &mut Ui, message: &str, retryable: bool) -> ErrorAction {
    let mut action = ErrorAction::default();

    ui.horizontal_centered(|ui| {
        // ⚠ 来自另一套字体，单独成段才跟正文同一条基线
        theme::line(
            ui,
            theme::Edge::Inline,
            &[
                theme::Piece::text("⚠", theme::sans(12.5), theme::RED_FG),
                theme::Piece::text(message, theme::sans(12.5), theme::RED_FG).gap(4.0),
            ],
        );
        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
            if retryable {
                if theme::ghost_button(ui, "重试").clicked() {
                    action.retry = true;
                }
            } else if theme::ghost_button(ui, "知道了").clicked() {
                action.dismissed = true;
            }
        });
    });

    action
}

/// 居中的品牌字样，解锁界面用
pub fn wordmark(ui: &mut Ui) {
    ui.vertical_centered(|ui| {
        ui.label(
            RichText::new("ToDay")
                .font(theme::display(30.0))
                .color(theme::INK_DEEP)
                .extra_letter_spacing(0.4),
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new("一日一记")
                .font(theme::sans(11.5))
                .color(theme::FAINT),
        );
    });
}
