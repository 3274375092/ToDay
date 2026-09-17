//! 日记面板：一日一记，像一张按日期划线的纸。
//!
//! 只列"已经写了的"、"今天"、"日历上点中的那天"（方便补写），以及光标正停在的那一行
//! （这样清空正文后行不会闪掉）。
//!
//! 面板不认识存储：正文从 [`MonthSheet`] 读进来，改动以 [`Change`] 交出去，
//! 什么时候落盘由控制器决定。行本身没有边框，只有底下一根细线。
//!
//! 一天是一块会长高的正文：长文自动换行、行随内容长高；`Enter` 在正文里换行，
//! `Ctrl`（macOS 上是 `Cmd`）+ `Enter` 把焦点交给列表里的下一天。

use crate::model::{Change, MonthSheet};
use crate::ui::calendar::weekday_name;
use crate::ui::theme;
use chrono::{Datelike, Local, NaiveDate, Weekday};
use egui::{
    self, Align2, CornerRadius, Layout, Margin, Rect, RichText, Sense, Shape, Stroke, Ui, Vec2,
};

/// 一行正文时的行高（空行与单行都是它；长条目行自己长高）
const ROW_HEIGHT: f32 = 40.0;
/// 正文块上下各留的空隙：算正文基线时要把它加回来
const ROW_PAD_Y: i8 = 8;
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
    /// 用户点开 / 用 `Ctrl+Enter` 跳过去的那一天
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

/// 丢掉 `egui` 替所有输入框存下的状态：撤销缓冲里有一份正文副本，光标与选区也在里头。
///
/// 日记行与密码框一起丢，下一次绘制从零开始。只在锁定之后调：控制器那侧已经把
/// 正文缓存换成空页，这里清掉 `egui` 手里的另一份。
pub fn forget_text_edits(ctx: &egui::Context) {
    ctx.memory_mut(|memory| {
        memory
            .data
            .remove_by_type::<egui::widgets::text_edit::TextEditState>();
    });
}

/// 撤销条上的摘要：换行折成空格，太长就截断。
///
/// 折掉 `\n` 是因为撤销条是一行 `ui.label`：里面带换行会把条子撑成两行高。
fn preview(text: &str) -> String {
    const MAX_CHARS: usize = 12;
    let flat: String = text
        .chars()
        .map(|ch| if ch == '\n' { ' ' } else { ch })
        .collect();
    let mut out: String = flat.chars().take(MAX_CHARS).collect();
    if flat.chars().nth(MAX_CHARS).is_some() {
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
    /// 按了 `Ctrl`/`Cmd` + 回车：焦点交给下一行
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
        // 按 Ctrl/Cmd+Enter 后焦点该落到哪一行
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
                    // Ctrl/Cmd+Enter：把焦点交给下一行。列表最后一行没有下一行，
                    // 焦点留在原地（不丢焦点，也就不必另给一条落盘路径）
                    if outcome.focus_next && row + 1 < days.len() {
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
            // 条子到点就散：暂存的正文也没用了，留着就是锁定之后还在内存里的一份副本
            self.trash = None;
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
/// 这一行才能读成"一日一记"，而不是日号飘在正文上方。
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
        // 多行，行高随内容长：`desired_rows(1)` 让空行与单行都只占一行，
        // 长文在列宽里自动换行。给了 `Frame` 之后 `margin` 就不作数了，
        // 上下那点透气由框的内边距给，所以下面算基线时要把 `ROW_PAD_Y` 加回来。
        let response = ui.add(
            egui::TextEdit::multiline(text)
                .desired_rows(1)
                .id(row_id(date))
                .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(0, ROW_PAD_Y)))
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

    // 正文那条基线：正文块第一行的外框顶边，往下让一个字体升部。
    // `response.rect` 含框的内边距，所以先让开 `ROW_PAD_Y`。
    gutter(
        ui,
        gutter_rect,
        date,
        today,
        selected,
        response.rect.top() + f32::from(ROW_PAD_Y) + body_ascent,
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
        // `Ctrl`/`Cmd`+Enter 不是 multiline 的换行键，正文里不会同时插一个换行；
        // `has_focus()` 含"窗口有焦点"，后台帧不会误触发
        focus_next: response.has_focus()
            && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command),
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

    /// 跑一帧，把 Ctrl/Cmd+Enter 喂进去，并让 `focused` 那一行拿着焦点。
    ///
    /// 键盘状态是一路积下来的（`InputState::modifiers` 只跟 `ModifiersChanged` 走），
    /// 所以除了按键本身，还得说明 Ctrl/Cmd 正按着。
    fn ctrl_enter_frame(
        ctx: &egui::Context,
        focused: egui::Id,
        mut add_contents: impl FnMut(&mut Ui),
    ) {
        let input = egui::RawInput {
            events: vec![
                egui::Event::ModifiersChanged(egui::Modifiers::COMMAND),
                egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::COMMAND,
                },
            ],
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            ui.memory_mut(|memory| memory.request_focus(focused));
            add_contents(ui);
        });
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

    /// 长正文在列宽里换行，行随内容长高，而不是被裁成一行。
    ///
    /// 窗口宽度得自己限：测试里没有真实屏幕，根 UI 拿到的是 10000px。
    #[test]
    fn a_long_entry_wraps_into_multiple_lines() {
        let ctx = frame();
        let key = MonthKey::new(2025, 9).unwrap();
        let mut sheet = MonthSheet::empty(key);
        let long = "今天的事很多。".repeat(30);
        sheet.set(date(17), &long);
        let mut diary = Diary::new();

        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                // 宽度得自己限：测试里没有真实屏幕，根 UI 拿到的是 10000px
                theme::column(ui, 320.0, |ui| diary.show(ui, &sheet, date(17)));
            });
        });
        output.textures_delta.clear();

        let galley = output
            .shapes
            .iter()
            .find_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) if text.galley.text().starts_with("今天的事很多。") => {
                    Some(text.galley.clone())
                }
                _ => None,
            })
            .expect("正文该画出来");

        assert!(
            galley.rows.len() > 1,
            "长正文该换行：{} 行",
            galley.rows.len()
        );
        let first = galley.rows.first().expect("至少一行");
        let second = galley.rows.get(1).expect("第二行");
        assert!(
            second.pos.y > first.pos.y,
            "第二行该在第一行下面：{first:?} / {second:?}"
        );
    }

    /// Ctrl/Cmd+Enter 把焦点交给列表里的下一行；最后一行没有下一行，焦点留在原地。
    ///
    /// 顺带盯住“组合键不会被正文吃掉”：Ctrl+Enter 不该算一次正文改动。
    #[test]
    fn ctrl_enter_hands_focus_to_the_next_day() {
        let ctx = frame();
        let key = MonthKey::new(2025, 9).unwrap();
        let mut sheet = MonthSheet::empty(key);
        sheet.set(date(17), "今天写一句");
        sheet.set(date(18), "明天写一句");
        let mut diary = Diary::new();

        let mut event = None;
        ctrl_enter_frame(&ctx, row_id(date(17)), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                event = Some(diary.show(ui, &sheet, date(17)));
            });
        });
        assert_eq!(
            event.expect("画过一帧").changes,
            Vec::new(),
            "Ctrl+Enter 不该往正文里插一个换行"
        );
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(row_id(date(18))),
            "焦点该落到下一天那一行"
        );

        // 18 号是列表里的最后一行（19 号没写过）：焦点留在原地
        ctrl_enter_frame(&ctx, row_id(date(18)), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                diary.show(ui, &sheet, date(17));
            });
        });
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(row_id(date(18))),
            "最后一行没有下一天，焦点不该丢"
        );
    }

    /// 空行与单行仍是一行的高度（`ROW_HEIGHT`）：装订线的纵位置、
    /// 行间细线的位置、以及“正文就在 40px 带里居中”都建立在这个数上。
    ///
    /// 缺了 `desired_rows(1)` 空行会长成 4 行；`ROW_PAD_Y` 太大也会把行撑出去。
    /// 行高从行底块反推：它是唯一圆角 8 的方块，上下各内缩 `ROW_PILL_INSET`。
    #[test]
    fn an_empty_or_single_line_row_stays_one_line_tall() {
        let ctx = frame();
        let key = MonthKey::new(2025, 9).unwrap();
        let mut sheet = MonthSheet::empty(key);
        sheet.set(date(17), "只有一行");
        let mut diary = Diary::new();

        // 选中 18 号：它没写过，但选中那天总会列出来，于是空行也在列表里
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                diary.show(ui, &sheet, date(18));
            });
        });
        output.textures_delta.clear();

        let pills: Vec<Rect> = output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Rect(rect) if rect.corner_radius == CornerRadius::same(8) => {
                    Some(rect.rect)
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            pills.len(),
            2,
            "写过的 17 号与空着的 18 号各一个底块：{pills:?}"
        );
        for pill in &pills {
            let height = pill.height() + ROW_PILL_INSET * 2.0;
            assert!(
                (height - ROW_HEIGHT).abs() < 0.01,
                "单行与空行都该占 {ROW_HEIGHT}px：{pills:?}"
            );
        }
    }

    /// 摘要折掉换行：撤销条是一行 `ui.label`，带换行会把它撑成两行高
    #[test]
    fn the_undo_preview_folds_newlines_and_truncates() {
        assert_eq!(preview("第一行\n第二行"), "第一行 第二行");
        // 12 个字以内原样，超出就截断加省略号
        assert_eq!(
            preview("一段很长很长很长的正文啊"),
            "一段很长很长很长的正文啊"
        );
        assert_eq!(
            preview("一段很长很长很长的正文啊啊"),
            "一段很长很长很长的正文啊…"
        );
    }

    /// 行画过一帧，`egui` 手里就多了一份正文副本（撤销缓冲）：
    /// 锁定之后要它连这份一起丢。
    #[test]
    fn forgetting_the_text_edits_drops_the_rows_state() {
        let ctx = frame();
        let key = MonthKey::new(2025, 9).unwrap();
        let mut sheet = MonthSheet::empty(key);
        sheet.set(date(17), "写了点东西");
        let mut diary = Diary::new();

        run_frame(&ctx, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                diary.show(ui, &sheet, date(17));
            });
        });
        let state = || egui::widgets::text_edit::TextEditState::load(&ctx, row_id(date(17)));
        assert!(state().is_some(), "画过一行之后 egui 存着这份状态");

        forget_text_edits(&ctx);
        assert!(state().is_none());
    }

    /// 撤销条到点就散：暂存的正文也跟着丢，不然它会留在内存里直到锁定
    #[test]
    fn an_expired_undo_toast_forgets_the_deleted_text() {
        let ctx = frame();
        let key = MonthKey::new(2025, 9).unwrap();
        let mut diary = Diary::new();
        // 条子是 100 秒前删的，这一帧已经过期
        diary.trash = Some(Trash {
            date: date(17),
            text: "删掉的正文".to_owned(),
            at: -100.0,
        });

        run_frame(&ctx, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                diary.show(ui, &MonthSheet::empty(key), date(17));
            });
        });
        assert!(diary.trash.is_none(), "过期之后不该还留着正文副本");
    }
}
