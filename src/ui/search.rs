//! 侧栏搜索：一行输入框 + 一张浮在正文列上的结果卡片。
//!
//! 这里只画界面、只表达意图：查询文字与"键盘当前行"是视图状态；命中（[`Hit`]）
//! 由控制器按帧递进来（[`Results`]），什么时候真的去扫库由控制器决定。
//! 点开一行只把 [`SearchRow`] 交回去——跳到哪一天、卡片什么时候收起，都在 `app` 里做。

use crate::search::{Hit, MAX_HITS};
use crate::ui::{shell, theme};
use chrono::NaiveDate;
use egui::{
    Align, Color32, CornerRadius, Label, Layout, Margin, Rect, Sense, Shape, Stroke, StrokeKind,
    TextFormat, Ui, Vec2,
};

/// 搜索输入框的稳定 id：`egui` 靠它记住光标，测试靠它把焦点放对地方
pub const SEARCH_FIELD_ID: &str = "search_field";

/// 输入框里的提示语
const HINT: &str = "搜索正文或日期…";
/// 输入框高度与左右内边距（与解锁界面的密码框同一套画法）
const FIELD_HEIGHT: f32 = 34.0;
const FIELD_PAD_X: f32 = 11.0;
/// 一行结果的最小高度
const ROW_HEIGHT: f32 = 26.0;
/// 卡片贴着输入框下沿留的空隙
const CARD_GAP_Y: f32 = 8.0;
/// 卡片宽度：窗口再窄也不小于这个数，正常封顶 `CARD_MAX_WIDTH`
const CARD_MIN_WIDTH: f32 = 220.0;
/// 卡片尺寸上限，以及离窗口右边 / 底边留的余量
const CARD_MAX_WIDTH: f32 = 460.0;
const CARD_MAX_HEIGHT: f32 = 420.0;
const SCREEN_MARGIN_X: f32 = 24.0;
const SCREEN_MARGIN_BOTTOM: f32 = 40.0;
/// 卡片里列表之外的固定开销：边框内边距 + 状态字
const CARD_CHROME: f32 = 46.0;

/// 控制器手上关于"当前这个查询"的结果
pub enum Results<'a> {
    /// 防抖还没到点、或查询刚改过：卡片说"搜索中…"，不拿旧结果冒充
    Pending,
    /// 这次查询的命中，按日期从新到旧；`truncated` 说库里还有更老的没列出来
    Ready(&'a [Hit], bool),
    /// 这次查询失败了（某天的密文解不开、或没解锁）
    Failed,
}

/// 卡片里的一行
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchRow {
    /// "跳到这一天"：查询能解析成日期时才有
    Date(NaiveDate),
    /// 第几条命中（下标进 [`Results::Ready`] 那张表）
    Hit(usize),
}

/// 侧栏那一行输入框交上来的意图
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchAction {
    /// 查询文字被改过
    pub changed: bool,
    /// `Esc` 或被清空：控制器据此把手里的命中一起丢掉
    pub dismissed: bool,
    /// `Enter` 打开键盘当前行；没有候选行时是 `None`
    pub open: Option<SearchRow>,
}

/// 搜索视图状态。
///
/// 只有"看"的状态：查询文字、卡片开没开、键盘停在哪一行、上一帧的候选行与位置。
/// 命中的正文一个字都不在这里留。
#[derive(Default)]
pub struct SearchView {
    /// 输入框里的文字
    query: String,
    /// 下一帧把焦点给输入框（`Ctrl`/`Cmd`+`F`）
    focus_pending: bool,
    /// 结果卡片展开着没有
    open: bool,
    /// 键盘当前行在 `rows` 里的序号
    selected: usize,
    /// 上一帧卡片占的那块：判断"点了卡片外面"
    card_rect: Option<Rect>,
    /// 上一帧的候选行：`field` 处理 `Enter` / `↑` / `↓` 要用
    rows: Vec<SearchRow>,
    /// 输入框的位置：卡片贴着它的下沿
    field_rect: Option<Rect>,
}

impl SearchView {
    pub fn new() -> Self {
        Self::default()
    }

    /// 下一帧聚焦输入框并展开卡片（`Ctrl`/`Cmd`+`F`）
    pub fn focus(&mut self) {
        self.focus_pending = true;
        self.open = true;
    }

    /// 输入框里的查询原文
    pub fn query(&self) -> &str {
        &self.query
    }

    /// 结果卡片现在开着吗
    pub fn open(&self) -> bool {
        self.open
    }

    /// 收起卡片：查询与候选行留着，再聚焦就能原样展开。
    ///
    /// 键盘当前行重置回第一行。收起来之后用户看不见它停在哪，留着就是一枚
    /// 按下去会打开未知一行的 `Enter`；展开时从第一行重新开始是可预期的。
    pub fn close(&mut self) {
        self.open = false;
        self.selected = 0;
    }

    /// 查询、候选行、键盘当前行一起清（`Esc`、锁定、解锁）
    pub fn clear(&mut self) {
        self.query.clear();
        self.selected = 0;
        self.open = false;
        self.focus_pending = false;
        self.card_rect = None;
        self.rows.clear();
    }

    /// 画侧栏那一行输入框，返回这一帧的意图。
    ///
    /// 背景先占位、等焦点状态确定之后再回填（与日记行同一套路数），
    /// 这样聚焦时的底色一定垫在文字底下。`↑` / `↓` 只改视图里的当前行，不往上报。
    pub fn field(&mut self, ui: &mut Ui) -> SearchAction {
        let mut action = SearchAction::default();

        let (rect, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), FIELD_HEIGHT),
            Sense::hover(),
        );
        self.field_rect = Some(rect);
        let backdrop = ui.painter().add(Shape::Noop);

        let inner = Rect::from_min_max(
            egui::pos2(rect.left() + FIELD_PAD_X, rect.top()),
            egui::pos2(rect.right() - FIELD_PAD_X, rect.bottom()),
        );
        // 框自己画：全局样式把输入框的底和边都做成了透明
        let response = ui
            .scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| {
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.query)
                            .id(egui::Id::new(SEARCH_FIELD_ID))
                            .frame(egui::Frame::NONE)
                            .hint_text(HINT)
                            .font(theme::sans(13.5))
                            .text_color(theme::INK)
                            .margin(Margin::symmetric(0, 7))
                            .desired_width(f32::INFINITY),
                    )
                })
                .inner
            })
            .inner;

        let focused = response.has_focus();
        let fill = if focused {
            theme::SURFACE
        } else {
            theme::SIDEBAR
        };
        let edge = if focused {
            theme::mix(theme::HAIRLINE, theme::INK, 0.45)
        } else {
            theme::HAIRLINE
        };
        ui.painter().set(
            backdrop,
            Shape::Vec(vec![
                Shape::rect_filled(rect, CornerRadius::same(8), fill),
                Shape::rect_stroke(
                    rect.shrink(0.5),
                    CornerRadius::same(8),
                    Stroke::new(1.0, edge),
                    StrokeKind::Inside,
                ),
            ]),
        );

        // 聚焦 / 打字 / Ctrl+F 都会展开卡片；外面点掉之后再聚焦，命中原样回来
        if self.focus_pending {
            response.request_focus();
            self.focus_pending = false;
            self.open = true;
        }
        if response.gained_focus() || response.changed() {
            self.open = true;
        }
        action.changed = response.changed();

        if focused {
            // 上下换行：夹在候选行数以内，没有候选行时不动。
            // 只在卡片展开时响应：收着的时候 `selected` 不可见，动了就是一枚
            // 会打开未知一行的 `Enter`（收起本身已把当前行归零，见 `close`）
            let (up, down) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::ArrowDown),
                )
            });
            if self.open && up {
                self.selected = self.selected.saturating_sub(1);
            }
            if self.open && down {
                self.selected = (self.selected + 1).min(self.rows.len().saturating_sub(1));
            }
        }

        // Esc 或被清空：控制器据此把手里的命中也清掉
        let emptied = action.changed && self.query.trim().is_empty();
        let escaped = focused && ui.input(|i| i.key_pressed(egui::Key::Escape));
        action.dismissed = escaped || emptied;

        // 回车打开当前行：单行输入框按回车会失焦，与解锁界面同一判据
        if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            action.open = self.rows.get(self.selected).copied();
        }

        action
    }

    /// 画结果卡片，返回被点开（或回车打开）的那一行。
    ///
    /// `jump` 是查询解析出来的日期（没有就是 `None`）：有它时卡片第一行是"跳到这一天"。
    /// 卡片是浮层，左上角与搜索框左边缘对齐、挂在它正下方，会盖住日历上半部分。
    pub fn results(
        &mut self,
        ctx: &egui::Context,
        jump: Option<NaiveDate>,
        results: Results<'_>,
    ) -> Option<SearchRow> {
        // 空查询不画卡片，候选行也跟着清：Enter 不该打开上一次的结果
        if self.query.trim().is_empty() {
            self.rows.clear();
            self.selected = 0;
            self.open = false;
            self.card_rect = None;
            return None;
        }

        // 候选行：日期行在最上面（有的话），后面才是命中
        self.rows.clear();
        if let Some(date) = jump {
            self.rows.push(SearchRow::Date(date));
        }
        let hits = match results {
            Results::Ready(hits, _) => Some(hits),
            Results::Pending | Results::Failed => None,
        };
        if let Some(hits) = hits {
            self.rows.extend((0..hits.len()).map(SearchRow::Hit));
        }
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));

        if !self.open {
            self.card_rect = None;
            return None;
        }
        let field = self.field_rect?;

        // 点卡片外面：收起卡片，查询与候选都留着（再聚焦就能原样展开）
        if let Some(card) = self.card_rect
            && ctx.input(|i| i.pointer.any_click())
            && let Some(pos) = ctx.input(|i| i.pointer.interact_pos())
            && !card.contains(pos)
            && !field.contains(pos)
        {
            // 走 `close()` 而不是只翻 `open`：键盘当前行也要跟着回到第一行
            self.close();
            self.card_rect = None;
            return None;
        }

        let screen = ctx.input(|i| i.content_rect());
        // 挂在搜索框正下方：左边缘对齐，宽度照旧封顶
        let x = field.left();
        let y = field.bottom() + CARD_GAP_Y;
        let width = (screen.right() - x - SCREEN_MARGIN_X).clamp(CARD_MIN_WIDTH, CARD_MAX_WIDTH);
        let max_height = (screen.bottom() - y - SCREEN_MARGIN_BOTTOM).clamp(120.0, CARD_MAX_HEIGHT);
        let body_height = (max_height - CARD_CHROME).max(80.0);

        // 头一行状态字：搜索中 / 找到 N 天 / 没有找到 / 搜索失败
        let status = match &results {
            Results::Pending => Some("搜索中…".to_owned()),
            Results::Failed => Some("搜索失败".to_owned()),
            Results::Ready(hits, _) if !hits.is_empty() => Some(format!("找到 {} 天", hits.len())),
            Results::Ready(_, _) if jump.is_none() => Some("没有找到".to_owned()),
            Results::Ready(_, _) => None,
        };
        // 只有扫描真的停在上限才说“被截了”：恰好 200 条但库里就这么多时
        // 不该声称还有更老的（那得让扫描方报，不能由这里数数猜）
        let truncated = matches!(results, Results::Ready(_, true));
        let rows = self.rows.clone();
        let mut opened = None;

        let area = egui::Area::new(egui::Id::new("search_card"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(x, y))
            .show(ctx, |ui| {
                ui.set_max_width(width);
                egui::Frame::window(ui.style())
                    .inner_margin(Margin::symmetric(12, 10))
                    .show(ui, |ui| {
                        ui.set_width(width - 24.0);
                        if let Some(text) = status.as_deref() {
                            theme::line(
                                ui,
                                theme::Edge::Inline,
                                &[theme::Piece::state(text, theme::MUTED)],
                            );
                            ui.add_space(6.0);
                        }

                        egui::ScrollArea::vertical()
                            .max_height(body_height)
                            .show(ui, |ui| {
                                for (index, row) in rows.iter().enumerate() {
                                    let highlighted = self.selected == index;
                                    let Some((_rect, response)) =
                                        card_row(ui, index, *row, hits, highlighted)
                                    else {
                                        continue;
                                    };
                                    // 悬停就把当前行拉过来：键盘接着回车就是这一行
                                    if response.hovered() {
                                        self.selected = index;
                                    }
                                    if response.clicked() {
                                        opened = Some(*row);
                                    }
                                }
                            });

                        if truncated {
                            ui.add_space(6.0);
                            theme::line(
                                ui,
                                theme::Edge::Inline,
                                &[theme::Piece::state(
                                    &format!("只列最近 {MAX_HITS} 天"),
                                    theme::MUTED,
                                )],
                            );
                        }
                    });
            });

        self.card_rect = Some(area.response.rect);
        opened
    }
}

/// 卡片里的一行：日期戳 + 摘要（命中行才有）。整行可点。
///
/// 命中下标越界时返回 `None`（结果被换掉的一瞬），这一行就不画。
fn card_row(
    ui: &mut Ui,
    index: usize,
    row: SearchRow,
    hits: Option<&[Hit]>,
    highlighted: bool,
) -> Option<(Rect, egui::Response)> {
    let (date, excerpt) = match row {
        SearchRow::Date(date) => (date, None),
        SearchRow::Hit(at) => {
            let hit = hits?.get(at)?;
            (hit.date, Some(hit))
        }
    };

    let width = ui.available_width();
    let backdrop = ui.painter().add(Shape::Noop);
    let line = ui.horizontal_top(|ui| {
        ui.set_min_height(ROW_HEIGHT);
        match excerpt {
            Some(hit) => {
                theme::mixed(ui, &shell::stamp(date), theme::mono(12.0), theme::MUTED);
                ui.add_space(10.0);
                ui.add(Label::new(excerpt_job(hit)).wrap());
            }
            None => {
                theme::mixed(
                    ui,
                    &format!("跳到 {}", shell::stamp(date)),
                    theme::mono(12.0),
                    theme::INK,
                );
            }
        }
    });
    let rect = Rect::from_min_size(
        line.response.rect.min,
        Vec2::new(width, line.response.rect.height()),
    );
    let response = ui.interact(rect, egui::Id::new(("search_row", index)), Sense::click());

    let fill = if response.hovered() {
        theme::HOVER
    } else if highlighted {
        theme::PRESSED
    } else {
        Color32::TRANSPARENT
    };
    if fill != Color32::TRANSPARENT {
        ui.painter().set(
            backdrop,
            Shape::rect_filled(rect, CornerRadius::same(8), fill),
        );
    }
    Some((rect, response))
}

/// 摘要的排版：命中前后的字弱、命中的那段用深墨加粗。
fn excerpt_job(hit: &Hit) -> egui::text::LayoutJob {
    let faded = TextFormat {
        font_id: theme::sans(12.0),
        color: theme::MUTED,
        ..Default::default()
    };
    let strong = TextFormat {
        font_id: theme::sans_bold(12.0),
        color: theme::INK_DEEP,
        ..Default::default()
    };

    let mut job = egui::text::LayoutJob::default();
    if !hit.before.is_empty() {
        job.append(&hit.before, 0.0, faded.clone());
    }
    job.append(&hit.matched, 0.0, strong);
    if !hit.after.is_empty() {
        job.append(&hit.after, 0.0, faded);
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `close()` 之后键盘当前行归零。
    ///
    /// 收起来之后用户看不见当前行停在哪；留着它，再展开时 `Enter` 会打开
    /// "收起前停的那一行"，而不是第一行。这是视图自己的不变量，直接测状态。
    #[test]
    fn closing_the_card_resets_the_current_row() {
        let mut view = SearchView::new();
        view.focus();
        view.selected = 3;

        view.close();

        assert_eq!(view.selected, 0, "收起之后当前行回第一行");
        assert!(!view.open(), "卡片该收着");
        assert_eq!(view.query(), "", "收卡片不动查询");
    }

    /// 查询与候选行留着：再聚焦就能原样展开（`close` 只收卡片与当前行）
    #[test]
    fn closing_keeps_the_query_and_rows() {
        let mut view = SearchView::new();
        view.query.push_str("咖啡");
        view.rows.push(SearchRow::Hit(0));
        view.rows.push(SearchRow::Hit(1));
        view.focus();

        view.close();

        assert_eq!(view.query(), "咖啡", "查询留着");
        assert_eq!(view.rows.len(), 2, "候选行留着");
    }
}
