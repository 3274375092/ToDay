//! 控制器：把界面意图翻译成存储动作。
//!
//! 这里是唯一知道"什么时候落盘"的地方：
//!
//! * 打字按 [`SAVE_DEBOUNCE_SECONDS`] 防抖，一次写一批；
//! * 失焦、切日期、切月份、删除、撤销、锁定、关窗 —— 立刻写；
//! * 写失败不 panic：把编辑放回队里，把错误显示在顶栏下面。
//!
//! 密码只在这里出现一瞬（从视图取出来交给存储层），控制器自己不持有密钥。

use crate::model::{Change, MonthKey, MonthSheet};
use crate::store::{StoreError, Vault};
use crate::ui::calendar::Calendar;
use crate::ui::diary::{Diary, DiaryEvent};
use crate::ui::shell::{self, Edge, ShellAction};
use crate::ui::theme;
use crate::ui::unlock::{UnlockMode, UnlockView};
use chrono::{Local, NaiveDate};
use eframe::egui;
use std::collections::BTreeMap;
use std::time::Duration;

/// 打字安静多久之后落盘
pub const SAVE_DEBOUNCE_SECONDS: f64 = 0.4;

/// 落盘节流。
///
/// 纯逻辑，不碰 IO：编辑先攒着，安静够久了再一次性交给存储层。
/// 同一天被反复改只留最后一次——中间那些状态没有落盘的价值。
#[derive(Debug, Default)]
pub struct SaveScheduler {
    /// 还没落盘的编辑，按日期归并
    pending: BTreeMap<NaiveDate, Change>,
    /// 最后一次编辑的时刻（`InputState::time`，秒）
    last_edit: f64,
    /// 上次写盘失败过：别再自动重试，等下一次明确的动作
    stuck: bool,
}

impl SaveScheduler {
    /// 记一次编辑
    pub fn record(&mut self, change: Change, now: f64) {
        self.last_edit = now;
        self.stuck = false;
        self.pending.insert(change.date(), change);
    }

    /// 到点了吗：攒着东西、没卡住，而且安静够久了
    pub fn is_due(&self, now: f64) -> bool {
        !self.pending.is_empty() && !self.stuck && now - self.last_edit >= SAVE_DEBOUNCE_SECONDS
    }

    /// 离到点还有多久（已经到点就是 0）
    pub fn time_until_due(&self, now: f64) -> f64 {
        (SAVE_DEBOUNCE_SECONDS - (now - self.last_edit)).max(0.0)
    }

    /// 把攒着的编辑取走（取走之后就不再"到点"了）
    pub fn take(&mut self) -> Vec<Change> {
        self.stuck = false;
        std::mem::take(&mut self.pending).into_values().collect()
    }

    /// 写盘失败：把没写成的编辑放回队里。
    ///
    /// 不覆盖期间新攒下的编辑，也不重置计时——重试要等下一次明确的动作，
    /// 否则磁盘坏了会变成每 0.4 秒一次的忙转。
    pub fn requeue(&mut self, changes: impl IntoIterator<Item = Change>) {
        for change in changes {
            self.pending.entry(change.date()).or_insert(change);
        }
        self.stuck = true;
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

/// 界面现在停在哪一步
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// 首次启动：设主密码
    Setup,
    /// 有密码但还没解开
    Unlock,
    /// 已解锁：主界面
    Ready,
}

/// 应用控制器
pub struct App {
    vault: Box<dyn Vault>,
    stage: Stage,
    /// 解锁 / 设密码卡片
    unlock: UnlockView,
    /// 正在编辑的那一天
    date: NaiveDate,
    /// 缓存里这一页是哪个月（跟着日历显示的那个月走）
    month: MonthKey,
    /// 当前这一页
    sheet: MonthSheet,
    calendar: Calendar,
    diary: Diary,
    scheduler: SaveScheduler,
    /// 最近一次失败，显示在顶栏下面
    error: Option<String>,
}

impl App {
    /// 装好控制器：数据库由 `main` 打开，这里只问一句"设过密码没有"
    pub fn new(vault: Box<dyn Vault>) -> Result<Self, StoreError> {
        let stage = if vault.needs_setup()? {
            Stage::Setup
        } else {
            Stage::Unlock
        };
        let today = Local::now().date_naive();
        let month = MonthKey::from_date(today);

        Ok(Self {
            vault,
            stage,
            unlock: UnlockView::new(),
            date: today,
            month,
            sheet: MonthSheet::empty(month),
            calendar: Calendar::new(today).with_selected(today),
            diary: Diary::new(),
            scheduler: SaveScheduler::default(),
            error: None,
        })
    }

    /// 解锁成功之后进入主界面：读今天所在的那一页
    fn enter_ready(&mut self) {
        let today = Local::now().date_naive();
        self.stage = Stage::Ready;
        self.error = None;
        self.unlock.clear();
        self.diary = Diary::new();
        self.scheduler = SaveScheduler::default();
        self.date = today;
        self.calendar.select(today);
        self.load_month(MonthKey::from_date(today));
    }

    /// 读一页进缓存（不落盘，初始化与换页之后都用它）
    fn load_month(&mut self, key: MonthKey) {
        self.month = key;
        self.sheet = match self.vault.load_month(key) {
            Ok(sheet) => sheet,
            Err(err) => {
                self.error = Some(err.to_string());
                MonthSheet::empty(key)
            }
        };
    }

    /// 换页：先把旧页写完再读新页
    fn switch_month(&mut self, key: MonthKey) {
        if key == self.month {
            return;
        }
        // 旧页上还没写的东西先落盘，否则换页会把它们从缓存里挤掉
        if let Err(message) = self.flush() {
            self.error = Some(message);
            return;
        }
        self.load_month(key);
    }

    /// 跳到某一天：日历跟着走，月份缓存按需换页
    fn go_to(&mut self, date: NaiveDate) {
        self.date = date;
        self.calendar.select(date);
        self.switch_month(MonthKey::from_date(date));
    }

    /// 把攒着的编辑写进去。失败时编辑不丢，留在队里等下次。
    fn flush(&mut self) -> Result<(), String> {
        if self.scheduler.is_empty() {
            return Ok(());
        }
        let changes = self.scheduler.take();
        match self.vault.apply(&changes) {
            Ok(()) => Ok(()),
            Err(err) => {
                self.scheduler.requeue(changes);
                Err(err.to_string())
            }
        }
    }

    /// 到点了就写
    fn flush_when_due(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        if self.scheduler.is_due(now)
            && let Err(message) = self.flush()
        {
            self.error = Some(message);
        }
        // 攒着东西就要保证还有下一帧，否则"到点"永远等不到
        if !self.scheduler.is_empty() {
            let wait = self.scheduler.time_until_due(now).max(0.01);
            ctx.request_repaint_after(Duration::from_secs_f64(wait));
        }
    }

    /// 关窗请求：先把攒着的东西写完，再放行
    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) || self.scheduler.is_empty() {
            return;
        }
        // 还有没落盘的：先取消这次关闭
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        match self.flush() {
            // 写完立刻再请求一次关闭
            Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            // 写不进去就别关，把错误留在屏幕上让人看见
            Err(message) => self.error = Some(message),
        }
    }

    /// 锁定：落盘之后才丢密钥
    fn lock(&mut self) {
        if let Err(message) = self.flush() {
            // 没写进去的编辑不能就这么扔掉，先别锁
            self.error = Some(message);
            return;
        }
        self.vault.lock();
        self.stage = Stage::Unlock;
        self.error = None;
        // 锁上之后内存里也不留正文
        self.sheet = MonthSheet::empty(self.month);
        self.diary = Diary::new();
        self.scheduler = SaveScheduler::default();
        self.unlock.clear();
    }

    /// 提交主密码
    fn submit_password(&mut self, mode: UnlockMode) {
        let outcome = {
            let password = self.unlock.password();
            match mode {
                UnlockMode::Setup => self.vault.setup(password),
                UnlockMode::Unlock => self.vault.unlock(password),
            }
        };

        match outcome {
            Ok(()) => self.enter_ready(),
            Err(err) => {
                self.error = Some(err.to_string());
                // 输入框清掉：错的密码不该留在屏幕上
                self.unlock.clear();
            }
        }
    }

    /// 把日记面板交上来的意图落到存储与缓存上
    fn handle_diary_event(&mut self, event: DiaryEvent, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);

        // 先落到缓存：界面这一帧就要看到结果
        if !event.changes.is_empty() {
            for change in &event.changes {
                self.sheet.apply(change);
            }
            for change in event.changes {
                self.scheduler.record(change, now);
            }
        }

        // 换天之前先落盘（`go_to` 内部会 flush）
        if let Some(date) = event.activated {
            self.go_to(date);
        }
        if (event.immediate || event.blurred)
            && let Err(message) = self.flush()
        {
            self.error = Some(message);
        }
    }

    /// 解锁 / 设密码界面
    fn password_screen(&mut self, ui: &mut egui::Ui) {
        let mode = match self.stage {
            Stage::Setup => UnlockMode::Setup,
            _ => UnlockMode::Unlock,
        };
        let error = self.error.clone();
        let mut submitted = false;

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::CANVAS))
            .show(ui, |ui| {
                submitted = self.unlock.show(ui, mode, error.as_deref());
            });

        if submitted {
            self.submit_password(mode);
        }
    }

    /// 主界面
    fn diary_screen(&mut self, ui: &mut egui::Ui) {
        let today = Local::now().date_naive();

        // 日历翻页了就把缓存换过来（点 ‹/› 只动日历，不动选中的日子）
        self.switch_month(self.calendar.displayed_month());

        let mut action = ShellAction::default();

        // ── 顶栏 ──
        let bar = egui::Panel::top("top_bar")
            .exact_size(shell::TOP_BAR_HEIGHT)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::NONE
                    .fill(theme::SURFACE)
                    .inner_margin(egui::Margin::symmetric(shell::panel_padding_x(), 0)),
            )
            .show(ui, |ui| shell::top_bar(ui, self.date, today));
        shell::edge_line(ui, bar.response.rect, Edge::Bottom);
        action = action.merge(bar.inner);

        // ── 错误条 ──
        if let Some(message) = self.error.clone() {
            let bar = egui::Panel::top("error_bar")
                .exact_size(shell::error_bar_height())
                .resizable(false)
                .show_separator_line(false)
                .frame(
                    egui::Frame::NONE
                        .fill(theme::RED_BG)
                        .inner_margin(egui::Margin::symmetric(shell::panel_padding_x(), 0)),
                )
                .show(ui, |ui| shell::error_banner(ui, &message));
            shell::edge_line(ui, bar.response.rect, Edge::Bottom);
            if bar.inner {
                self.error = None;
            }
        }

        // ── 底栏 ──
        let bar = egui::Panel::bottom("status_bar")
            .exact_size(shell::STATUS_BAR_HEIGHT)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::NONE
                    .fill(theme::SURFACE)
                    .inner_margin(egui::Margin::symmetric(shell::panel_padding_x(), 0)),
            )
            .show(ui, |ui| shell::status_bar(ui, self.month, self.sheet.len()));
        shell::edge_line(ui, bar.response.rect, Edge::Top);

        // ── 侧栏：日历 ──
        let (month, written, date, marked, calendar) = (
            self.month,
            self.sheet.len(),
            self.date,
            self.sheet.marked(),
            &mut self.calendar,
        );
        let bar = egui::Panel::left("sidebar")
            .exact_size(shell::SIDEBAR_WIDTH)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::NONE
                    .fill(theme::SIDEBAR)
                    .inner_margin(egui::Margin::symmetric(20, 18)),
            )
            .show(ui, |ui| {
                // 底部一点暖色光晕，别让侧栏下半截太空
                let rect = ui.max_rect();
                theme::glow(
                    ui.painter(),
                    egui::pos2(rect.center().x, rect.bottom() + 40.0),
                    rect.width() * 1.1,
                    egui::Color32::from_rgba_unmultiplied(0xD8, 0xC7, 0x9E, 16),
                );

                let picked = calendar.show(ui, &marked);
                let footer = shell::sidebar_footer(ui, month, written, date, today);
                (picked, footer)
            });
        shell::edge_line(ui, bar.response.rect, Edge::Right);

        let (picked, footer) = bar.inner;
        action = action.merge(footer);
        if let Some(date) = picked {
            self.go_to(date);
        }

        // ── 正文 ──
        let (sheet, diary, selected) = (&self.sheet, &mut self.diary, self.date);
        let event = egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::CANVAS))
            .show(ui, |ui| {
                // 两片很淡的暖光，给大片留白一点层次（看不见光，只觉得纸不冷）
                let rect = ui.max_rect();
                let warm =
                    |alpha: u8| egui::Color32::from_rgba_unmultiplied(0xE6, 0xD9, 0xB8, alpha);
                theme::glow(
                    ui.painter(),
                    rect.right_top() - egui::vec2(rect.width() * 0.12, rect.height() * 0.12),
                    rect.width() * 0.95,
                    warm(22),
                );
                theme::glow(
                    ui.painter(),
                    egui::pos2(
                        rect.left() + rect.width() * 0.12,
                        rect.bottom() + rect.height() * 0.3,
                    ),
                    rect.width() * 0.8,
                    warm(12),
                );

                theme::column(ui, shell::COLUMN_WIDTH, |ui| {
                    diary.show(ui, sheet, selected)
                })
            })
            .inner;

        self.handle_diary_event(event, ui.ctx());

        // ── 顶栏 / 页脚表达的意图 ──
        if action.goto_today {
            self.go_to(today);
        }
        if action.lock {
            self.lock();
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.render(ui);
    }
}

impl App {
    /// 画一帧。`eframe` 每帧调它，测试也靠它——所以不接 `Frame`，只要一个 `Ui`。
    pub fn render(&mut self, ui: &mut egui::Ui) {
        self.handle_close_request(ui.ctx());
        self.flush_when_due(ui.ctx());

        match self.stage {
            Stage::Setup | Stage::Unlock => self.password_screen(ui),
            Stage::Ready => self.diary_screen(ui),
        }
    }
}
