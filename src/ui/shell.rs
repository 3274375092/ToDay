//! 外壳：顶栏、底栏、侧栏页脚的布局，以及错误提示条。
//!
//! 这些函数只画界面、只返回"用户想干什么"（[`ShellAction`]）；
//! 翻日期、锁定、落盘这些副作用一律由控制器执行。

use crate::model::MonthKey;
use crate::ui::{calendar::weekday_name, theme};
use chrono::NaiveDate;
use egui::{CornerRadius, Layout, Rect, RichText, Ui, Vec2};

/// 侧栏宽度：日历只需要这么多，剩下的留给正文
pub const SIDEBAR_WIDTH: f32 = 292.0;
/// 正文列的最大宽度：一行太长不好读
pub const COLUMN_WIDTH: f32 = 720.0;
/// 顶栏 / 底栏 / 错误条高度
pub const TOP_BAR_HEIGHT: f32 = 58.0;
pub const STATUS_BAR_HEIGHT: f32 = 30.0;
const ERROR_BAR_HEIGHT: f32 = 34.0;
/// 面板左右内边距
const PANEL_PADDING_X: i8 = 22;

/// 顶栏 / 底栏 / 错误条用的面板高度
pub fn error_bar_height() -> f32 {
    ERROR_BAR_HEIGHT
}

/// 面板的水平内边距
pub fn panel_padding_x() -> i8 {
    PANEL_PADDING_X
}

/// 用户在界面上表达的意图。视图不改状态，只把意图交上去。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShellAction {
    /// 点了锁定
    pub lock: bool,
    /// 点了"回到今天"
    pub goto_today: bool,
}

impl ShellAction {
    /// 合并同一帧里几处布局交上来的意图
    pub fn merge(self, other: Self) -> Self {
        Self {
            lock: self.lock || other.lock,
            goto_today: self.goto_today || other.goto_today,
        }
    }
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

/// 顶栏：品牌在左，日期与锁定在右
pub fn top_bar(ui: &mut Ui, date: NaiveDate, today: NaiveDate) -> ShellAction {
    let mut action = ShellAction::default();

    ui.horizontal_centered(|ui| {
        // 品牌：拉丁与中文压在同一条基线上
        theme::line(
            ui,
            theme::Edge::Inline,
            &[
                theme::Piece::text("ToDay", theme::display(20.0), theme::INK_DEEP),
                theme::Piece::text("一天一行", theme::sans(11.5), theme::FAINT).gap(12.0),
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

        // 键位提示：键帽在前、说明在后
        theme::line(
            ui,
            theme::Edge::Right,
            &[
                theme::Piece::key("Enter"),
                theme::Piece::text("下一行", theme::sans(11.5), theme::FAINT).gap(6.0),
                theme::Piece::key("右键").gap(18.0),
                theme::Piece::text("删除这一天", theme::sans(11.5), theme::FAINT).gap(6.0),
            ],
        );
    });
}

/// 侧栏页脚：本月进度条 + 回到今天
pub fn sidebar_footer(
    ui: &mut Ui,
    month: MonthKey,
    written: usize,
    date: NaiveDate,
    today: NaiveDate,
) -> ShellAction {
    let mut action = ShellAction::default();

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

    ui.add_space(20.0);
    if date != today && theme::ghost_button(ui, "回到今天").clicked() {
        action.goto_today = true;
    }

    action
}

/// 错误提示条：一句人话 + 一个关闭按钮。返回是否点了关闭。
///
/// 面板的红底由控制器铺，这里只画内容。
pub fn error_banner(ui: &mut Ui, message: &str) -> bool {
    let mut dismissed = false;

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
            if theme::ghost_button(ui, "知道了").clicked() {
                dismissed = true;
            }
        });
    });

    dismissed
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
            RichText::new("一天一行")
                .font(theme::sans(11.5))
                .color(theme::FAINT),
        );
    });
}
