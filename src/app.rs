//! 控制器：把界面意图翻译成存储动作。
//!
//! 这里是唯一知道"什么时候落盘"的地方：
//!
//! * 打字按 [`SAVE_DEBOUNCE_SECONDS`] 防抖，一次写一批；
//! * 失焦、切日期、切月份、删除、撤销、锁定、关窗 —— 立刻写；
//! * 写失败不 panic：把编辑放回队里，把错误显示在顶栏下面，然后停下来
//!   （不再排帧、不再自动重试）。再试一次的入口都是用户的动作：点"重试"、
//!   再敲一个字、换日期或翻月份、关窗与锁定时的落盘。
//!
//! 密码只在这里出现一瞬（从视图取出来交给存储层），控制器自己不持有密钥。

use crate::model::{Change, MonthKey, MonthSheet};
use crate::store::{StoreError, Vault};
use crate::ui::calendar::Calendar;
use crate::ui::diary::{self, Diary, DiaryEvent};
use crate::ui::shell::{self, Edge, SaveState, ShellAction};
use crate::ui::theme;
use crate::ui::unlock::{UnlockMode, UnlockView};
use crate::window_state;
use chrono::{Local, NaiveDate};
use eframe::egui;
use std::collections::BTreeMap;
use std::path::PathBuf;
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

    /// 现在排下一帧有意义吗：队里有东西、而且上次没写失败。
    ///
    /// 返回 `false` 时排帧也不会发生任何写入（[`SaveScheduler::is_stuck`] 正是如此），
    /// 调用方不要排帧——否则写失败会变成每 10 ms 一帧的空转。
    pub fn needs_wakeup(&self) -> bool {
        !self.pending.is_empty() && !self.stuck
    }

    /// 上次写失败过、编辑还压在队里：在用户点"重试"或下一次编辑之前，不自动重试。
    pub fn is_stuck(&self) -> bool {
        self.stuck
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

/// 顶栏下面那条错误：一句话，加上它是怎么来的——决定它什么时候消失。
#[derive(Clone, Debug)]
enum Fault {
    /// 写盘失败：跟着队列走，写成功就收掉；队列里还有东西时错误条给"重试"
    Save(String),
    /// 读盘失败、密码不对：与队列无关，留到用户点"知道了"
    Other(String),
}

impl Fault {
    fn message(&self) -> &str {
        match self {
            Self::Save(message) | Self::Other(message) => message,
        }
    }
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
    error: Option<Fault>,
    /// 窗口尺寸记忆的落盘位置；不设就完全不碰窗口状态（测试与嵌入式用法）
    window_memory: Option<PathBuf>,
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
            window_memory: None,
        })
    }

    /// 打开窗口尺寸记忆：关窗时把尺寸记到 `path`（见 [`crate::window_state`]）。
    ///
    /// 这是唯一开启它的入口；不开时一个字节都不会写到窗口状态文件。
    pub fn with_window_memory(mut self, path: PathBuf) -> Self {
        self.window_memory = Some(path);
        self
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
                self.error = Some(Fault::Other(err.to_string()));
                MonthSheet::empty(key)
            }
        };
    }

    /// 换页：先把旧页写完再读新页。
    ///
    /// 只从用户的动作里调（点日历上的日子、翻月份、点开某一行）。**不要放进每帧的绘制路径**：
    /// 写失败时它每次都会再写一次盘，磁盘坏了就从"停下来"变成按帧写盘。
    /// 写不成就只是不换页——编辑留在队里，用户的下一个动作会再试一次。
    fn switch_month(&mut self, key: MonthKey) {
        if key == self.month {
            return;
        }
        // 旧页上还没写的东西先落盘，否则换页会把它们从缓存里挤掉
        if !self.flush() {
            return;
        }
        self.load_month(key);
    }

    /// 跳到某一天：日历跟着走，月份缓存按需换页。
    ///
    /// 换页会先把旧页写完，写不成就不换页；`self.date` 与日历照样跳过去
    /// ——跳是用户已经做过的动作，拦下来只会让三处状态互相不一致。
    pub fn go_to(&mut self, date: NaiveDate) {
        self.date = date;
        self.calendar.select(date);
        self.switch_month(MonthKey::from_date(date));
    }

    /// 把攒着的编辑写进去。失败时编辑不丢，留在队里等下次，并把错误写进 `error`。
    ///
    /// 返回 `false` 表示这次没写成功：队列非空、[`SaveScheduler::is_stuck`] 为真。
    /// 成功（或本来就没东西可写）返回 `true`，并收掉自己上一次留下的那条错误；
    /// [`Fault::Other`]（读盘失败、密码不对）与队列无关，不在这里收。
    fn flush(&mut self) -> bool {
        if self.scheduler.is_empty() {
            return true;
        }
        let changes = self.scheduler.take();
        match self.vault.apply(&changes) {
            Ok(()) => {
                // 只收写盘失败留下的那条：`load_month` 的读错误跟队列无关，
                // 一次成功的写入不该让它消失（那一页其实还是空的兜底页）
                if matches!(self.error, Some(Fault::Save(_))) {
                    self.error = None;
                }
                true
            }
            Err(err) => {
                self.scheduler.requeue(changes);
                self.error = Some(Fault::Save(err.to_string()));
                false
            }
        }
    }

    /// 用户点了错误条上的"重试"：立刻再写一次。
    ///
    /// 空队列是幂等的（什么都不做）。成功之后错误条消失、状态字回到"已保存"。
    pub fn retry_save(&mut self) {
        if self.scheduler.is_empty() {
            return;
        }
        self.flush();
    }

    /// 顶栏那枚保存状态字该显示什么
    fn save_state(&self) -> SaveState {
        if self.scheduler.is_empty() {
            SaveState::Saved
        } else if self.scheduler.is_stuck() {
            SaveState::Failed
        } else {
            SaveState::Pending
        }
    }

    /// 到点了就写
    fn flush_when_due(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        if self.scheduler.is_due(now) {
            self.flush();
        }
        // 攒着东西就得保证还有下一帧，否则"到点"永远等不到；
        // 上次写失败时不排帧——再排也不会写，只会空转
        if self.scheduler.needs_wakeup() {
            let wait = self.scheduler.time_until_due(now).max(0.01);
            ctx.request_repaint_after(Duration::from_secs_f64(wait));
        }
    }

    /// 关窗请求：先记下窗口尺寸，再把攒着的东西写完，最后放行。
    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        // 尺寸与正文无关，有没有待写的编辑都要记
        self.remember_window(ctx);
        if self.scheduler.is_empty() {
            return;
        }
        // 还有没落盘的：先取消这次关闭
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        if !self.flush() {
            // 写不进去就别关，把错误留在屏幕上让人看见
            return;
        }
        // 写完立刻再请求一次关闭
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// 把这一帧量到的窗口尺寸写进记忆文件。
    ///
    /// 没开记忆、或写不进去时只报一句——记不下尺寸不该影响关窗。
    fn remember_window(&self, ctx: &egui::Context) {
        let Some(path) = self.window_memory.as_deref() else {
            return;
        };
        let (rect, maximized, fullscreen) = ctx.input(|i| {
            let viewport = i.viewport();
            (
                viewport.inner_rect,
                viewport.maximized.unwrap_or(false),
                viewport.fullscreen.unwrap_or(false),
            )
        });
        // 最大化与全屏时量到的都是整屏尺寸，不能当作"普通尺寸"记下来。
        // 全屏也要看：`maximized` 不是每帧都刷新（macOS 上 `egui_winit` 只在窗口创建
        // 那一次采样它），而 macOS 那个绿色按钮给的正是全屏；`fullscreen` 每帧都更新。
        let size = if maximized || fullscreen {
            None
        } else {
            rect.map(|rect| (rect.width(), rect.height()))
        };
        if let Err(err) = window_state::remember(path, size, maximized) {
            eprintln!("窗口尺寸记不下来：{err}");
        }
    }

    /// 锁定：落盘之后才丢密钥。
    ///
    /// 写不成就拒绝锁定并把错误留在屏幕上（编辑还在队里，不会丢）。
    pub fn lock(&mut self, ctx: &egui::Context) {
        if !self.flush() {
            return;
        }
        self.vault.lock();
        self.stage = Stage::Unlock;
        self.error = None;
        // 锁上之后内存里也不留正文：缓存、视图状态，以及 `egui` 给输入框留下的那份副本
        self.sheet = MonthSheet::empty(self.month);
        self.diary = Diary::new();
        self.scheduler = SaveScheduler::default();
        self.unlock.clear();
        diary::forget_text_edits(ctx);
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
                self.error = Some(Fault::Other(err.to_string()));
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
        // 离散动作（删除、撤销）与离开一行都立刻写
        if event.immediate || event.blurred {
            self.flush();
        }
    }

    /// 解锁 / 设密码界面
    fn password_screen(&mut self, ui: &mut egui::Ui) {
        let mode = match self.stage {
            Stage::Setup => UnlockMode::Setup,
            _ => UnlockMode::Unlock,
        };
        let error = self.error.as_ref().map(|fault| fault.message().to_owned());
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
            .show(ui, |ui| {
                shell::top_bar(ui, self.date, today, self.save_state())
            });
        shell::edge_line(ui, bar.response.rect, Edge::Bottom);
        action = action.merge(bar.inner);

        // ── 错误条 ──
        if let Some(fault) = self.error.clone() {
            let retryable = self.scheduler.is_stuck();
            let bar = egui::Panel::top("error_bar")
                .exact_size(shell::error_bar_height())
                .resizable(false)
                .show_separator_line(false)
                .frame(
                    egui::Frame::NONE
                        .fill(theme::RED_BG)
                        .inner_margin(egui::Margin::symmetric(shell::panel_padding_x(), 0)),
                )
                .show(ui, |ui| shell::error_banner(ui, fault.message(), retryable));
            shell::edge_line(ui, bar.response.rect, Edge::Bottom);
            if bar.inner.dismissed {
                self.error = None;
            }
            if bar.inner.retry {
                self.retry_save();
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
        let (month, written, marked, calendar) = (
            self.month,
            self.sheet.len(),
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

                let before = calendar.displayed_month();
                let picked = calendar.show(ui, &marked);
                // 页脚只有本月记录与进度条：回到今天在顶栏那枚，不在左下角重复
                shell::sidebar_footer(ui, month, written);
                // 翻页键与跨月的日子都在 `show` 里改日历自己那个月：
                // 这一帧翻没翻，只有在里面比过才知道
                (picked, calendar.displayed_month() != before)
            });
        shell::edge_line(ui, bar.response.rect, Edge::Right);

        let (picked, turned) = bar.inner;
        if let Some(date) = picked {
            // 点中的日子可能不在本月：换页交给 `go_to`（它先把旧页写完）
            self.go_to(date);
        } else if turned {
            // 只有用户翻了页才换页：每帧都试一次会让写失败变成按帧写盘
            self.switch_month(self.calendar.displayed_month());
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
            self.lock(ui.ctx());
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
