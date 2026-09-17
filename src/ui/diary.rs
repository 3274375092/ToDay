//! 日记面板：一天一行，像一张按日期划线的纸。
//!
//! 只列"已经写了的"、"今天"、"日历上点中的那天"（方便补写），以及光标正停在的那一行
//! （这样清空正文后行不会闪掉）。
//!
//! 面板不认识存储：正文从 [`MonthSheet`] 读进来，改动以 [`Change`] 交出去，
//! 什么时候落盘由控制器决定。行本身没有边框，只有底下一根细线。

use crate::model::{Change, MonthSheet};
use crate::ui::calendar::weekday_name;
use crate::ui::theme;
use chrono::{Datelike, Local, NaiveDate, Weekday};
use egui::{
    self, Align2, CornerRadius, Layout, Margin, Rect, RichText, Sense, Shape, Stroke, Ui, Vec2,
};

/// 行高
const ROW_HEIGHT: f32 = 40.0;
/// 今天那一层暖底在行里的上下内缩
const ROW_PILL_INSET: f32 = 2.0;
/// 行首左边距
const ROW_PAD: f32 = 4.0;
/// 装订线宽度（日号 + 星期）
const GUTTER: f32 = 52.0;
/// 装订线与正文之间的空隙
const GUTTER_GAP: f32 = 14.0;
/// 装订线里日号与星期之间的空隙
const DAY_WEEKDAY_GAP: f32 = 5.0;
/// 正文的字号：装订线靠它算出正文压在哪条基线上
const BODY_SIZE: f32 = 15.5;
/// 撤销条显示多久（秒）
const UNDO_SECONDS: f64 = 8.0;

/// 日记面板这一帧交出来的东西：只有意图，没有副作用
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DiaryEvent {
    /// 用户点开 / 回车跳过去的那一天
    pub activated: Option<NaiveDate>,
    /// 正文被改动的那几天
    pub changes: Vec<Change>,
    /// 这批改动要立刻落盘（删除、撤销这类离散动作，不必等防抖）
    pub immediate: bool,
    /// 这一帧有行失去焦点：控制器据此立刻落盘
    pub blurred: bool,
}

/// 日记面板的视图状态。
///
/// 只有"看"的状态——正文与日期都在控制器手里，这里一个字节的领域数据都不留。
#[derive(Default)]
pub struct Diary {
    /// 上次滚动定位到的日期，避免每帧把用户滚回原处
    scrolled_to: Option<NaiveDate>,
    /// 上一帧光标停在哪一行
    focus_anchor: Option<NaiveDate>,
    /// 刚删掉的一天，用来撤销
    trash: Option<Trash>,
}

/// 删除后暂存的内容
struct Trash {
    date: NaiveDate,
    text: String,
    /// 删除发生的时刻（`InputState::time`，秒）
    at: f64,
}

/// 每一行输入框的稳定 id：`egui` 靠它记住光标，测试靠它把焦点放对地方
pub fn row_id(date: NaiveDate) -> egui::Id {
    egui::Id::new(("diary_row", date))
}

/// 撤销条上的摘要，太长就截断
fn preview(text: &str) -> String {
    const MAX_CHARS: usize = 12;
    let mut out: String = text.chars().take(MAX_CHARS).collect();
    if text.chars().nth(MAX_CHARS).is_some() {
        out.push('…');
    }
    out
}

/// 一行的绘制结果
struct RowOutcome {
    /// 正文输入框
    response: egui::Response,
    /// 点了这一行，或焦点落上来了
    activated: bool,
    /// 正文被改过
    changed: bool,
    /// 右键菜单里点了删除
    deleted: bool,
    /// 现在有焦点
    focused: bool,
    /// 这一帧失去焦点
    blurred: bool,
    /// 按了回车：焦点交给下一行
    focus_next: bool,
}

impl Diary {
    pub fn new() -> Self {
        Self::default()
    }

    /// 展示这一页，返回这一帧的意图
    pub fn show(&mut self, ui: &mut Ui, sheet: &MonthSheet, selected: NaiveDate) -> DiaryEvent {
        let key = sheet.key();
        let today = Local::now().date_naive();
        let now = ui.input(|i| i.time);
        let mut event = DiaryEvent::default();
        // 按回车后焦点该落到哪一行
        let mut focus_row: Option<usize> = None;
        // 选中日期变了才滚动，否则会把用户的滚动"钉死"
        let should_scroll = self.scrolled_to != Some(selected);
        self.scrolled_to = Some(selected);

        let days: Vec<NaiveDate> = key
            .dates()
            .filter(|date| {
                *date == today
                    || *date == selected
                    || sheet.has(*date)
                    || self.focus_anchor == Some(*date)
            })
            .collect();

        self.masthead(ui, sheet, selected);
        ui.add_space(14.0);
        theme::hairline(ui);

        // 正文压在哪条基线上：拿同样的字体量一个汉字（中文的字身最满，行框由它定）。
        // 装订线就靠这个数把自己摆到跟正文同一条线上。量一次，行行共用。
        let body_ascent = theme::baseline(&ui.painter().layout_no_wrap(
            "日".to_owned(),
            theme::sans(BODY_SIZE),
            theme::INK,
        ));

        if days.is_empty() {
            theme::empty_hint(
                ui,
                &format!("{} 年 {} 月还没有记录", key.year(), key.month()),
                "点左边日历上的任何一天，就可以开始写",
            );
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                for (row, date) in days.iter().enumerate() {
                    let date = *date;

                    // 从这一页里取文字来编辑，改动在行末交给控制器
                    let mut text = sheet.text(date).to_owned();
                    let outcome = show_row(
                        ui,
                        date,
                        &mut text,
                        today,
                        selected,
                        row + 1 == days.len(),
                        body_ascent,
                    );

                    // 点开 / 聚焦某一行 = 当前就是这天
                    if outcome.activated {
                        event.activated = Some(date);
                    }
                    // 单行 TextEdit 按回车会失焦：顺势把焦点送给下一行
                    if outcome.focus_next {
                        focus_row = Some(row + 1);
                    }
                    if focus_row == Some(row) {
                        outcome.response.request_focus();
                    }
                    // 选中的那天（比如刚在日历上点的）滚进视野
                    if date == selected && should_scroll {
                        outcome.response.scroll_to_me(None);
                    }

                    if outcome.deleted {
                        self.trash = Some(Trash {
                            date,
                            text: text.clone(),
                            at: now,
                        });
                        event.changes.push(Change::delete(date));
                        // 删除是离散动作，立刻落盘
                        event.immediate = true;
                    } else if outcome.changed {
                        // 纯空白会被归一成删除：见 `Change::set`
                        event.changes.push(Change::set(date, &text));
                    }

                    if outcome.focused {
                        self.focus_anchor = Some(date);
                    } else if outcome.blurred && self.focus_anchor == Some(date) {
                        self.focus_anchor = None;
                    }
                    if outcome.blurred {
                        // 离开一行就等于"这句写完了"，控制器会立刻落盘
                        event.blurred = true;
                    }
                }
            });

        self.undo_toast(ui, now, &mut event);
        event
    }

    /// 抬头：大号日期 + 月份 + 星期，右边一枚字数标签
    fn masthead(&self, ui: &mut Ui, sheet: &MonthSheet, selected: NaiveDate) {
        let chars = sheet.text(selected).chars().count();

        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{:02}", selected.day()))
                    .font(theme::display(42.0))
                    .color(theme::INK_DEEP),
            );
            ui.add_space(10.0);
            ui.vertical(|ui| {
                ui.add_space(9.0);
                // 数字与"月"分开排：同一个 family 里中文会被压到拉丁字框中间偏下
                theme::mixed(
                    ui,
                    &format!("{} 月", selected.month()),
                    theme::display(17.0),
                    theme::MUTED,
                );
                ui.add_space(2.0);
                ui.label(
                    RichText::new(format!("星期{}", weekday_name(selected)))
                        .font(theme::sans(11.0))
                        .color(theme::FAINT),
                );
            });

            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                // 字数只有这里看得到；"今天"交给顶栏的状态字和日历上的墨块去说，不重复第三遍
                let counted = if chars > 0 {
                    format!("{chars} 字")
                } else {
                    "空白".to_owned()
                };
                theme::line(
                    ui,
                    theme::Edge::Right,
                    &theme::mixed_pieces(&counted, theme::mono(11.0), theme::MUTED),
                );
            });
        });

        ui.add_space(4.0);
        theme::mixed(
            ui,
            &format!(
                "{} 年 {} 月 {} 日 · 第 {} 周",
                selected.year(),
                selected.month(),
                selected.day(),
                selected.iso_week().week()
            ),
            theme::mono(11.0),
            theme::FAINT,
        );
    }

    /// 撤销条：贴底浮着，到点自己消失
    fn undo_toast(&mut self, ui: &mut Ui, now: f64, event: &mut DiaryEvent) {
        let Some((date, preview, remaining)) = self.trash.as_ref().map(|trash| {
            (
                trash.date,
                preview(&trash.text),
                UNDO_SECONDS - (now - trash.at),
            )
        }) else {
            return;
        };
        if remaining <= 0.0 {
            return;
        }

        // 倒计时要走字，动画要顺，所以按帧请求重绘
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(200));

        // 从下方滑进来：只动位置，不动尺寸
        let slide = ui
            .ctx()
            .animate_bool_with_time(egui::Id::new("undo_toast"), true, 0.22);

        egui::Area::new(egui::Id::new("undo_toast"))
            .anchor(
                Align2::CENTER_BOTTOM,
                Vec2::new(0.0, -42.0 + (1.0 - slide) * 12.0),
            )
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::NONE
                    .fill(theme::YELLOW_BG)
                    .stroke(Stroke::new(
                        1.0,
                        theme::mix(theme::YELLOW_BG, theme::YELLOW_FG, 0.16),
                    ))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::symmetric(14, 9))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("已删掉 {date} 的日记「{preview}」"))
                                    .font(theme::sans(12.5))
                                    .color(theme::YELLOW_FG),
                            );
                            ui.add_space(6.0);
                            if theme::ghost_button(ui, "撤销").clicked()
                                && let Some(restored) = self.trash.take()
                            {
                                event.activated = Some(restored.date);
                                event
                                    .changes
                                    .push(Change::set(restored.date, restored.text));
                                // 撤销也是离散动作，立刻落盘
                                event.immediate = true;
                            }
                            ui.add_space(2.0);
                            ui.label(
                                RichText::new(format!("{remaining:.0}s"))
                                    .font(theme::mono(10.5))
                                    .color(theme::mix(theme::YELLOW_BG, theme::YELLOW_FG, 0.5)),
                            );
                        });
                    });
            });
    }
}

/// 一行的装订线：日号与星期压在同一条基线上，右侧对齐。
///
/// `baseline_y` 是正文那条基线（由 [`show_row`] 从输入框算出来）：日号与正文同高，
/// 这一行才能读成"一天一行"，而不是日号飘在正文上方。
/// 星期是固定一个字宽，所以日号能右对齐成一列，星期也右对齐成一列。
/// 返回这一块实际占到的位置，测试靠它盯住这个不变量。
fn gutter(
    ui: &Ui,
    box_rect: Rect,
    date: NaiveDate,
    today: NaiveDate,
    selected: NaiveDate,
    baseline_y: f32,
) -> Rect {
    let weekend = matches!(date.weekday(), Weekday::Sat | Weekday::Sun);
    let day_color = if weekend {
        theme::WEEKEND
    } else if date == today || date == selected {
        theme::INK_DEEP
    } else {
        theme::INK
    };

    let painter = ui.painter();
    let day = painter.layout_no_wrap(format!("{:02}", date.day()), theme::mono(14.0), day_color);
    let weekday = painter.layout_no_wrap(
        weekday_name(date).to_owned(),
        theme::sans(10.0),
        theme::FAINT,
    );

    let weekday_rect = Rect::from_min_size(
        egui::pos2(
            box_rect.right() - weekday.size().x,
            baseline_y - theme::baseline(&weekday),
        ),
        weekday.size(),
    );
    let day_rect = Rect::from_min_size(
        egui::pos2(
            weekday_rect.left() - DAY_WEEKDAY_GAP - day.size().x,
            baseline_y - theme::baseline(&day),
        ),
        day.size(),
    );

    painter.galley(day_rect.min, day, day_color);
    painter.galley(weekday_rect.min, weekday, theme::FAINT);

    day_rect.union(weekday_rect)
}

/// 画一行：左边日号 + 星期，右边没有边框的输入框，底下一根细线。
///
/// 背景用一个占位图形实现：先把位置占好，等悬停状态确定了再回填，
/// 这样背景永远垫在文字底下。装订线也一样：先占位，等输入框排好、
/// 算出正文那条基线之后，再把日号与星期压上去。
fn show_row(
    ui: &mut Ui,
    date: NaiveDate,
    text: &mut String,
    today: NaiveDate,
    selected: NaiveDate,
    last: bool,
    body_ascent: f32,
) -> RowOutcome {
    let is_today = date == today;

    let background = ui.painter().add(Shape::Noop);

    let row = ui.horizontal(|ui| {
        ui.set_min_height(ROW_HEIGHT);
        ui.add_space(ROW_PAD);

        let (gutter_rect, _) =
            ui.allocate_exact_size(Vec2::new(GUTTER, ROW_HEIGHT), Sense::hover());
        ui.add_space(GUTTER_GAP);

        // 正文：没有框的输入框，像直接写在纸上；id 写死，行数变化也不丢光标。
        // 框没有内边距（给了 `Frame` 之后 `margin` 就不作数了），文字贴着框顶排，
        // 所以下面算基线时不用再让开内边距。
        let response = ui.add(
            egui::TextEdit::singleline(text)
                .id(row_id(date))
                .frame(egui::Frame::NONE)
                .font(theme::sans(BODY_SIZE))
                .text_color(theme::INK)
                .desired_width(ui.available_width())
                .hint_text(if is_today {
                    "今天，写一句就好…"
                } else {
                    "补一句…"
                }),
        );
        (gutter_rect, response)
    });

    let (gutter_rect, response) = row.inner;
    let rect = row.response.rect;
    let focused = response.has_focus();

    // 正文那条基线：输入框里文字的外框顶边，往下让一个字体升部
    gutter(
        ui,
        gutter_rect,
        date,
        today,
        selected,
        response.rect.top() + body_ascent,
    );
    // 背景：今天垫一层暖底，悬停 / 聚焦再往上调一层
    let lift = ui.ctx().animate_bool_with_time(
        egui::Id::new(("row_hover", date)),
        row.response.hovered() || response.hovered() || focused,
        0.14,
    );
    let base = if is_today {
        theme::SIDEBAR
    } else {
        theme::CANVAS
    };
    let background_rect = Rect::from_min_max(
        egui::pos2(rect.left() + ROW_PAD, rect.top() + ROW_PILL_INSET),
        egui::pos2(rect.right(), rect.bottom() - ROW_PILL_INSET),
    );
    ui.painter().set(
        background,
        Shape::rect_filled(
            background_rect,
            CornerRadius::same(8),
            theme::mix(base, theme::HOVER, lift),
        ),
    );

    // 行与行之间的细线，和正文左边缘对齐
    if !last {
        let y = rect.bottom();
        ui.painter().rect_filled(
            Rect::from_min_max(
                egui::pos2(response.rect.left(), y),
                egui::pos2(rect.right(), y + 1.0),
            ),
            CornerRadius::ZERO,
            theme::HAIRLINE,
        );
    }

    // 右键 → 显式删除（有空内容才给这个菜单项）
    let mut deleted = false;
    if !text.is_empty() {
        response.context_menu(|ui| {
            if ui.button("删除这一天").clicked() {
                deleted = true;
                ui.close();
            }
        });
    }

    RowOutcome {
        activated: response.clicked() || response.gained_focus(),
        changed: response.changed(),
        deleted,
        focused,
        blurred: response.lost_focus(),
        focus_next: response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)),
        response,
    }
}

/// 给测试与冒烟用：一帧不接窗口也能画
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MonthKey;

    fn date(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2025, 9, day).unwrap()
    }

    fn frame() -> egui::Context {
        let ctx = egui::Context::default();
        crate::ui::theme::install(&ctx);
        ctx
    }

    /// 跑一帧。测试里没有渲染后端，纹理增量得自己收掉。
    fn run_frame(ctx: &egui::Context, mut add_contents: impl FnMut(&mut Ui)) {
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| add_contents(ui));
        output.textures_delta.clear();
    }

    #[test]
    fn showing_a_page_does_not_panic() {
        let ctx = frame();
        let key = MonthKey::new(2025, 9).unwrap();
        let mut sheet = MonthSheet::empty(key);
        sheet.set(date(17), "写了点东西");
        let mut diary = Diary::new();

        let mut event = None;
        run_frame(&ctx, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                event = Some(diary.show(ui, &sheet, date(17)));
            });
        });
        assert_eq!(event, Some(DiaryEvent::default()), "没人操作时不该产出意图");
    }

    #[test]
    fn an_empty_month_shows_the_empty_hint() {
        let ctx = frame();
        let key = MonthKey::new(1999, 1).unwrap();
        let mut diary = Diary::new();

        run_frame(&ctx, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                diary.show(
                    ui,
                    &MonthSheet::empty(key),
                    NaiveDate::from_ymd_opt(1999, 1, 5).unwrap(),
                );
            });
        });
    }

    /// 装订线的两份字（日号、星期）要压在给它的那条基线上，而且不能跑出行底块。
    ///
    /// 基线的位置是给进去的，不是算出来的：装订线只负责服从它。
    #[test]
    fn the_gutter_sits_on_the_body_baseline() {
        let ctx = frame();
        let day = date(17);
        let baseline_y = 26.5;
        let mut painted = None;

        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let box_rect = Rect::from_min_size(ui.cursor().min, Vec2::new(GUTTER, ROW_HEIGHT));
            painted = Some((box_rect, gutter(ui, box_rect, day, day, day, baseline_y)));
            // 一整行里只该有装订线这两段字：从图元里读回来量
        });
        output.textures_delta.clear();

        let (box_rect, content) = painted.unwrap();
        let pill = box_rect.shrink(ROW_PILL_INSET);

        assert!(
            content.top() >= pill.top() && content.bottom() <= pill.bottom(),
            "装订线跑出了行底块：content={content:?} pill={pill:?}"
        );

        let baselines: Vec<f32> = output
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
            .collect();
        assert_eq!(baselines.len(), 2, "日号与星期各画一段：{baselines:?}");
        for y in &baselines {
            assert!(
                (y - baseline_y).abs() < 0.01,
                "装订线没压在正文那条基线上：{baselines:?}，期望 {baseline_y}"
            );
        }
    }

    /// 装订线自己占位、正文排好之后才回填：日号与正文必须落在同一条基线上
    #[test]
    fn a_row_keeps_its_gutter_on_the_text_baseline() {
        let ctx = frame();
        let key = MonthKey::new(2025, 9).unwrap();
        let mut sheet = MonthSheet::empty(key);
        // 选中 17 号、写的是 18 号：这样装订线上的 "18" 只可能是那一行的
        sheet.set(date(18), "你打包");
        let mut diary = Diary::new();
        let mut painted = Vec::new();

        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                diary.show(ui, &sheet, date(17));
            });
        });
        output.textures_delta.clear();
        for clipped in &output.shapes {
            if let egui::Shape::Text(text) = &clipped.shape
                && let Some(row) = text.galley.rows.first()
                && let Some(glyph) = row.row.glyphs.first()
            {
                painted.push((
                    text.galley.text().to_owned(),
                    text.pos.y + row.pos.y + glyph.pos.y,
                ));
            }
        }

        let baseline_of = |needle: &str| {
            painted
                .iter()
                .find(|(text, _)| text == needle)
                .unwrap_or_else(|| panic!("{needle} 没画出来：{painted:?}"))
                .1
        };
        let day = baseline_of("18");
        let body = baseline_of("你打包");
        assert!(
            (day - body).abs() < 0.01,
            "日号与正文不在同一条基线上：日号 {day}，正文 {body}"
        );
    }
}
