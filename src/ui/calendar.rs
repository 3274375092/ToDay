//! 侧栏日历：只负责"选哪一天"，以及这天写没写过。
//!
//! 日期格子是自己画的，没有用默认按钮皮肤——选中是墨块、今天是描边、
//! 有记录的日子底下点一个小圆点。
//!
//! 日历不认识日记本：哪些天有记录由调用方用一集合 [`BTreeSet`] 告诉它。

use crate::model::MonthKey;
use crate::ui::theme;
use chrono::{Datelike, Duration, NaiveDate, Weekday};
use egui::{self, Align2, Color32, CornerRadius, Layout, Sense, Stroke, StrokeKind, Ui, Vec2};
use std::collections::BTreeSet;

/// 一周七天，序号与 `Weekday::num_days_from_monday` 对齐
pub const WEEKDAY_NAMES: [&str; 7] = ["一", "二", "三", "四", "五", "六", "日"];

/// 这一天是星期几，返回"一"…"日"
pub fn weekday_name(date: NaiveDate) -> &'static str {
    WEEKDAY_NAMES[date.weekday().num_days_from_monday() as usize]
}

/// 日历 Widget
pub struct Calendar {
    /// 当前显示的月份（只用年和月）
    display_year: i32,
    display_month: u32,
    /// 用户选中的日期
    pub selected_date: Option<NaiveDate>,
    /// 是否显示上一月/下一月的灰显日期
    show_adjacent_months: bool,
}

impl Calendar {
    pub fn new(initial_date: NaiveDate) -> Self {
        Self {
            display_year: initial_date.year(),
            display_month: initial_date.month(),
            selected_date: None,
            show_adjacent_months: true,
        }
    }

    pub fn with_selected(mut self, date: NaiveDate) -> Self {
        self.selected_date = Some(date);
        self.display_year = date.year();
        self.display_month = date.month();
        self
    }

    /// 上一月
    fn prev_month(&mut self) {
        self.goto(self.month_key().prev());
    }

    /// 下一月
    fn next_month(&mut self) {
        self.goto(self.month_key().next());
    }

    /// 当前显示的月份
    fn month_key(&self) -> MonthKey {
        MonthKey::new(self.display_year, self.display_month)
            .unwrap_or_else(|| MonthKey::from_date(chrono::Local::now().date_naive()))
    }

    fn goto(&mut self, key: MonthKey) {
        self.display_year = key.year();
        self.display_month = key.month();
    }

    /// 当前显示的是哪年哪月
    pub fn displayed_month(&self) -> MonthKey {
        self.month_key()
    }

    /// 从外部选中某天（比如在日记里点开了某一行），顺便跳到那个月
    pub fn select(&mut self, date: NaiveDate) {
        self.selected_date = Some(date);
        self.goto(MonthKey::from_date(date));
    }
}

impl Calendar {
    /// 展示日历，返回用户点中的那一天。
    ///
    /// `marked` 是"写过东西的日子"：这些格子底下会点一个小圆点。
    pub fn show(&mut self, ui: &mut Ui, marked: &BTreeSet<NaiveDate>) -> Option<NaiveDate> {
        let today = chrono::Local::now().date_naive();
        let mut clicked = None;

        // ── 月份导航：标题在左，翻页在右 ──
        ui.horizontal(|ui| {
            theme::mixed(
                ui,
                &format!("{} 年 {} 月", self.display_year, self.display_month),
                theme::display(19.0),
                theme::INK_DEEP,
            );
            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                // 先摆的在右边
                if theme::icon_button(ui, "›", 26.0)
                    .on_hover_text("下个月")
                    .clicked()
                {
                    self.next_month();
                }
                if theme::icon_button(ui, "‹", 26.0)
                    .on_hover_text("上个月")
                    .clicked()
                {
                    self.prev_month();
                }
            });
        });

        ui.add_space(16.0);

        let cells = build_month_grid(self.month_key());
        // 格子宽度按可用宽度平分，窄窗口下也不会挤成一团
        let gap = 4.0;
        let cell_w = ((ui.available_width() - gap * 6.0) / 7.0).max(24.0);
        let cell_h = 32.0;

        ui.scope(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(gap, gap);

            // ── 星期表头 ──
            ui.horizontal(|ui| {
                for (i, name) in WEEKDAY_NAMES.iter().enumerate() {
                    let (rect, _) = ui.allocate_exact_size(Vec2::new(cell_w, 16.0), Sense::hover());
                    let color = if i >= 5 { theme::WEEKEND } else { theme::MUTED };
                    ui.painter().text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        *name,
                        theme::sans(11.0),
                        color,
                    );
                }
            });

            // ── 日期网格：6 行 × 7 列 ──
            for week in cells.chunks(7) {
                ui.horizontal(|ui| {
                    for cell in week {
                        if self.day_cell(ui, cell, cell_w, cell_h, today, marked) {
                            clicked = Some(cell.date);
                        }
                    }
                });
            }
        });

        // 点中的日子如果不在本月，顺手把日历翻过去
        if let Some(date) = clicked {
            self.select(date);
        }

        clicked
    }

    /// 画一个日期格子，返回是否被点中
    fn day_cell(
        &self,
        ui: &mut Ui,
        cell: &DayCell,
        width: f32,
        height: f32,
        today: NaiveDate,
        marked: &BTreeSet<NaiveDate>,
    ) -> bool {
        let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());

        let selected = self.selected_date == Some(cell.date);
        let is_today = cell.date == today;
        let weekend = matches!(cell.date.weekday(), Weekday::Sat | Weekday::Sun);
        let outside = !cell.is_current_month && !self.show_adjacent_months;
        let dim = !cell.is_current_month;

        // 悬停才浮出一层暖色；选中的格子不用再变
        let hover =
            ui.ctx()
                .animate_bool_with_time(response.id, response.hovered() && !selected, 0.12);
        let radius = CornerRadius::same(6);

        let painter = ui.painter();
        if selected {
            painter.rect_filled(rect, radius, theme::INK_DEEP);
        } else if hover > 0.001 {
            painter.rect_filled(
                rect,
                radius,
                theme::mix(theme::SIDEBAR, theme::HOVER, hover),
            );
        } else if is_today {
            // 今天：一圈很淡的墨线，不跟选中态抢
            painter.rect_stroke(
                rect.shrink(0.5),
                radius,
                Stroke::new(1.0, theme::mix(theme::SIDEBAR, theme::INK, 0.28)),
                StrokeKind::Inside,
            );
        }

        let color = if selected {
            Color32::WHITE
        } else if outside || dim {
            theme::FAINT
        } else if weekend {
            theme::WEEKEND
        } else {
            theme::INK
        };

        // 日号：等宽数字，6 行才能对齐
        painter.text(
            rect.center() - Vec2::new(0.0, 3.0),
            Align2::CENTER_CENTER,
            cell.date.day(),
            theme::mono(13.5),
            color,
        );

        // 写过东西的日子：底下一个小圆点
        if marked.contains(&cell.date) {
            let dot = if selected {
                Color32::from_white_alpha(150)
            } else if dim {
                theme::FAINT
            } else {
                theme::INK.gamma_multiply(0.45)
            };
            painter.circle_filled(rect.center() + Vec2::new(0.0, 9.0), 1.5, dot);
        }

        response.clicked()
    }
}

/// 一个日历格子的数据
struct DayCell {
    date: NaiveDate,
    /// 是否属于当前显示的月份
    is_current_month: bool,
}

/// 生成这个月的 6×7 日期网格
fn build_month_grid(key: MonthKey) -> Vec<DayCell> {
    let first_day = key.first_day();
    let total_days = key.days() as i64;

    // 星期偏移：chrono 中 Monday=0, Sunday=6
    // 以周一为第一列，直接用 num_days_from_monday()
    let start_offset = first_day.weekday().num_days_from_monday() as i64;

    let mut cells = Vec::with_capacity(42);

    // 前导补位（上个月的日期）
    for i in (0..start_offset).rev() {
        let date = first_day - Duration::days(i + 1);
        cells.push(DayCell {
            date,
            is_current_month: false,
        });
    }

    // 当前月的日期
    for date in key.dates() {
        cells.push(DayCell {
            date,
            is_current_month: true,
        });
    }

    // 尾部补位（下个月的日期），补齐到 42 个
    let mut next = cells.last().map_or(first_day, |cell| cell.date) + Duration::days(1);
    while cells.len() < 42 {
        cells.push(DayCell {
            date: next,
            is_current_month: false,
        });
        next += Duration::days(1);
    }

    debug_assert_eq!(total_days as usize, key.days() as usize);
    cells
}
