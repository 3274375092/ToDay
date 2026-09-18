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
//! 搜索走 [`SearchScheduler`]：查询安静 0.25 秒才扫一次库，扫之前先把待写的编辑落盘。
//!
//! 密码只在这里出现一瞬（从视图取出来交给存储层），控制器自己不持有密钥。

use crate::model::{Change, MonthKey, MonthSheet};
use crate::search::{self, SearchResults};
use crate::store::{StoreError, Vault};
use crate::ui::calendar::Calendar;
use crate::ui::diary::{self, Diary, DiaryEvent};
use crate::ui::search::{Results, SearchAction, SearchRow, SearchView};
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

/// 查询安静多久之后真的去搜一次
pub const SEARCH_DEBOUNCE_SECONDS: f64 = 0.25;

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

/// 搜索防抖。
///
/// 纯逻辑，不碰 IO：查询改了就重新计时，安静够久了才真的扫一次库。
/// 一帧最多搜一次；没有改动就不搜。
#[derive(Debug, Default)]
pub struct SearchScheduler {
    /// 查询改过、还没搜
    dirty: bool,
    /// 查询最后一次改动的时刻（`InputState::time`，秒）
    changed_at: f64,
}

impl SearchScheduler {
    /// 记一次查询改动
    pub fn record(&mut self, now: f64) {
        self.dirty = true;
        self.changed_at = now;
    }

    /// 到点了吗：查询改过，而且安静够久了
    pub fn is_due(&self, now: f64) -> bool {
        self.dirty && now - self.changed_at >= SEARCH_DEBOUNCE_SECONDS
    }

    /// 离到点还有多久（已经到点就是 0）
    pub fn time_until_due(&self, now: f64) -> f64 {
        (SEARCH_DEBOUNCE_SECONDS - (now - self.changed_at)).max(0.0)
    }

    /// 搜过了（或查询空到不必搜）：在下次改动之前不再到点
    pub fn mark_searched(&mut self) {
        self.dirty = false;
    }

    /// 现在排下一帧有意义吗：查询改过、还没搜。
    ///
    /// 返回 `false` 时排帧也不会发生搜索，调用方不要排帧。
    pub fn needs_wakeup(&self) -> bool {
        self.dirty
    }
}

/// 跳转之后要落下的选区。
///
/// 只有目标月的缓存真的到位了才用得上；换页失败时根本不置它（红条已经说明了原因）。
struct PendingJump {
    date: NaiveDate,
    /// 命中那一段在正文里的字符区间
    range: std::ops::Range<usize>,
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
    /// 侧栏搜索框与结果卡片
    search: SearchView,
    /// 最近一次搜索的结果（命中按日期从新到旧，附“是不是被截了”）
    hits: SearchResults,
    /// `hits` 是对哪个查询搜出来的（`None` = 还没搜过）
    hit_query: Option<String>,
    /// 最近一次搜索失败了
    hit_failed: bool,
    /// 搜索防抖
    search_scheduler: SearchScheduler,
    /// 跳转之后要落下的选区
    pending_jump: Option<PendingJump>,
    /// 最近一次失败，显示在顶栏下面
    error: Option<Fault>,
    /// 窗口尺寸记忆的落盘位置；不设就完全不碰窗口状态（测试与嵌入式用法）
    window_memory: Option<PathBuf>,
    /// 窄屏浮层（搜索框 + 日历）开着没有。宽屏侧栏一直都在，这个状态没意义
    overlay_open: bool,
    /// 退出这个进程的执行者：手机上的返回键退到底之后走它（见 [`App::with_quit_hook`]）
    quit: Option<Box<dyn Fn()>>,
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
            search: SearchView::new(),
            hits: SearchResults::default(),
            hit_query: None,
            hit_failed: false,
            search_scheduler: SearchScheduler::default(),
            pending_jump: None,
            error: None,
            window_memory: None,
            overlay_open: false,
            quit: None,
        })
    }

    /// 打开窗口尺寸记忆：关窗时把尺寸记到 `path`（见 [`crate::window_state`]）。
    ///
    /// 这是唯一开启它的入口；不开时一个字节都不会写到窗口状态文件。
    pub fn with_window_memory(mut self, path: PathBuf) -> Self {
        self.window_memory = Some(path);
        self
    }

    /// 装上"退出进程"的执行者：手机上的返回键退到底之后由它接手（见 `crate::android`）。
    ///
    /// 为什么不让 `App` 自己退出：`std::process::exit` 是进程级动作，桌面与测试都不该有它；
    /// 而"什么时候退"是导航决策，归 `App`。两边各管一半，测试里装一个探针就能验。
    /// 不装就是不退（桌面、测试、嵌入式都一样）。
    pub fn with_quit_hook(mut self, quit: impl Fn() + 'static) -> Self {
        self.quit = Some(Box::new(quit));
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
        self.clear_search();
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

    /// 到点了：先落盘，再全库搜一次。
    ///
    /// 先 [`App::flush`] 是让搜索看到磁盘上真实的样子（刚敲的那句也在里面）；
    /// 写失败不拦住读——错误条照旧显示，搜索照样跑。
    /// 搜过之后在下次改动前不再到点；空查询不碰磁盘。
    fn run_search(&mut self) {
        self.search_scheduler.mark_searched();
        let query = self.search.query().trim().to_owned();
        if query.is_empty() {
            self.drop_hits();
            return;
        }

        // `flush` 的返回值要留着判红条该归谁（见下）
        let saved = self.flush();
        match self.vault.search(&query) {
            Ok(hits) => {
                self.hits = hits;
                self.hit_failed = false;
            }
            Err(err) => {
                self.hits = SearchResults::default();
                self.hit_failed = true;
                // 这一次落盘也失败了就不改 `error`：红条上正写着"重试"，
                // 换成搜索的错会让用户以为问题只是查询没成，
                // 而正文其实还压在队里没写进去。写盘那条更急，留它
                if saved {
                    self.error = Some(Fault::Other(err.to_string()));
                }
            }
        }
        self.hit_query = Some(query);
    }

    /// 丢掉手里的命中：查询清空、锁定之后，内存里不该还留着正文摘要
    fn drop_hits(&mut self) {
        self.hits = SearchResults::default();
        self.hit_query = None;
        self.hit_failed = false;
    }

    /// 搜索状态整个清干净：查询、命中、卡片、防抖计时（锁定与解锁都走它）
    fn clear_search(&mut self) {
        self.search.clear();
        self.drop_hits();
        self.pending_jump = None;
        self.search_scheduler = SearchScheduler::default();
    }

    /// 打开卡片里的一行：日期行直接跳过去，命中行跳到那天并把那一句选上。
    ///
    /// 命中下标是某一次搜索的结果：查询改过、或上次搜索失败时它已经失效，
    /// 这时只当没点（卡片这一帧显示的也不是这些命中）。
    fn open_row(&mut self, row: SearchRow) {
        // 窄屏：点开一行就是要看那一页正文，浮层跟着收掉（宽屏侧栏本来就在，不用动）
        if self.overlay_open {
            self.close_overlay();
        }
        match row {
            SearchRow::Date(date) => {
                self.pending_jump = None;
                self.go_to(date);
                self.search.close();
            }
            SearchRow::Hit(index) => {
                let current = self.search.query().trim();
                if self.hit_failed || self.hit_query.as_deref() != Some(current) {
                    return;
                }
                let (date, range) = match self.hits.hits.get(index) {
                    Some(hit) => (hit.date, hit.char_range()),
                    None => return,
                };
                self.open_hit(date, range);
            }
        }
    }

    /// 跳到命中的那一天，并把命中那一段选上（焦点交给那一行）。
    ///
    /// 只有目标月的缓存真的换过去了才置 `pending_jump`：换页失败（写盘失败）时
    /// 红条已经说明了原因，这里不起跳，用户的下一个动作还能重来。
    fn open_hit(&mut self, date: NaiveDate, range: std::ops::Range<usize>) {
        self.go_to(date);
        self.pending_jump =
            (self.sheet.key() == MonthKey::from_date(date)).then_some(PendingJump { date, range });
        // 卡片收起、查询留着：再聚焦就能回到这张结果表
        self.search.close();
    }

    /// 把 `pending_jump` 里的选区种进那一行的输入框状态，并把焦点交给它。
    ///
    /// 必须在正文画完之后调（见 `diary_screen` 里的调用点）：`TextEdit` 重建时会把
    /// 已有的非空选区塌成一个点，先种下去会被同一帧抹掉，用户看不到选中的那句。
    ///
    /// 换页失败时 `sheet` 还是旧的那一页，这里就什么都不做（`open_hit` 本来也没置
    /// `pending_jump`；这里是第二道阑，防着日期与缓存对不上）。
    fn apply_pending_jump(&mut self, ctx: &egui::Context) {
        let Some(jump) = self.pending_jump.take() else {
            return;
        };
        if self.sheet.key() != MonthKey::from_date(jump.date) {
            return;
        }
        let id = diary::row_id(jump.date);
        let mut state = egui::widgets::text_edit::TextEditState::load(ctx, id).unwrap_or_default();
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(jump.range.start),
                egui::text::CCursor::new(jump.range.end),
            )));
        state.store(ctx, id);
        ctx.memory_mut(|memory| memory.request_focus(id));
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

    /// 把这一帧量到的窗口尺寸写进记忆文件。    ///
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

    /// 返回键：一层一层退，退到底就退出。
    ///
    /// 顺序就是用户眼里的层次：浮层 → 搜索 → 退出。退出前先落盘，写不成就留在界面上
    /// （红条已经说明了原因），不在没写完的时候走。
    ///
    /// 桌面与测试可以不装退出钩子（[`App::with_quit_hook`]）：那时退到最外层就什么都不做。
    /// 手机上这个键来自 game-activity（`KeyCode::Back` → `Key::BrowserBack`），
    /// Activity 不会自己结束：谁都不处理它就等于这个键不存在。
    fn handle_back(&mut self, ctx: &egui::Context) {
        if !ctx.input(|input| input.key_pressed(egui::Key::BrowserBack)) {
            return;
        }
        if self.overlay_open {
            self.close_overlay();
            return;
        }
        if self.search.open() || !self.search.query().trim().is_empty() {
            self.clear_search();
            return;
        }
        if !self.flush() {
            return;
        }
        if let Some(quit) = self.quit.as_ref() {
            quit();
        }
    }

    /// 开窄屏浮层。[`shell::OverlayIntent::Search`] 还要把焦点给搜索框（底栏的「搜索」按的就是它）。
    fn open_overlay(&mut self, intent: shell::OverlayIntent) {
        self.overlay_open = true;
        if intent == shell::OverlayIntent::Search {
            self.search.focus();
        }
    }

    /// 收起窄屏浮层。查询与命中留着（下次打开原样回来），但结果卡片一起收：
    /// 卡片挂在搜索框下沿，而浮层收掉之后这一帧就不画搜索框了，`field_rect` 停在上一帧的位置。
    fn close_overlay(&mut self) {
        self.overlay_open = false;
        self.search.close();
    }

    /// `Ctrl`/`Cmd`+`F`：宽屏上侧栏本来就摆着，只把焦点给搜索框；窄屏上得先把浮层开出来。
    fn focus_search(&mut self, narrow: bool) {
        if narrow {
            self.open_overlay(shell::OverlayIntent::Search);
        } else {
            self.search.focus();
        }
    }

    /// 列表里的下一天：本月写过的那些天，加上今天，按日期排。
    ///
    /// 与日记面板那份行列表同一套判据（`Ctrl`+`Enter` 走的就是它）；手机上没有 `Ctrl`+`Enter`，
    /// 底栏那枚「下一天」按的是同一个位置。当前这一天之后没有了就返回 `None`（按钮不画）。
    fn next_day(&self) -> Option<NaiveDate> {
        let today = Local::now().date_naive();
        self.month
            .dates()
            .filter(|date| *date > self.date)
            .find(|date| *date == today || self.sheet.has(*date))
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
        // 查询与命中摘要也一起清：锁上之后内存里不该留着任何一个字
        self.clear_search();
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

    /// 窄屏浮层：搜索框 + 日历 + 页脚。
    ///
    /// 与宽屏左侧栏是同一份内容（同一批 widget、同一个顺序），只是容器不同。
    /// 返回的含义与宽屏那边一样：搜索框的意图、点中的日子、日历这一帧翻没翻页。
    fn sidebar_body(&mut self, ui: &mut egui::Ui) -> (SearchAction, Option<NaiveDate>, bool) {
        // 底部一点暖色光晕，别让侧栏下半截太空
        let rect = ui.max_rect();
        theme::glow(
            ui.painter(),
            egui::pos2(rect.center().x, rect.bottom() + 40.0),
            rect.width() * 1.1,
            egui::Color32::from_rgba_unmultiplied(0xD8, 0xC7, 0x9E, 16),
        );

        // 搜索排在日历上面：
        // 日历回答"去哪一天"，搜索回答"那句话在哪"，找一句话不必先翻到那个月
        let action = self.search.field(ui);
        ui.add_space(14.0);

        let marked = self.sheet.marked();
        let before = self.calendar.displayed_month();
        let picked = self.calendar.show(ui, &marked);
        // 页脚只有本月记录与进度条：回到今天在顶栏那枚，不在左下角重复
        shell::sidebar_footer(ui, self.month, self.sheet.len());
        // 翻页键与跳月的日子都在 `show` 里改日历自己那个月：
        // 这一帧翻没翻，只有在里面比过才知道
        (action, picked, self.calendar.displayed_month() != before)
    }

    /// 主界面
    fn diary_screen(&mut self, ui: &mut egui::Ui) {
        let today = Local::now().date_naive();
        let now = ui.input(|i| i.time);
        // 窄屏：侧栏排不下，改由底栏的「日历」「搜索」开浮层。
        // 判据是整窗宽度（面板还没摆，`available_width` 就是全宽）
        let narrow = ui.available_width() < shell::MOBILE_BREAKPOINT;
        // 从窄屏转回宽屏（横屏、展开）：浮层没意义了，状态跟着收掉，
        // 免得下次转回窄屏时凭空弹出来
        if !narrow && self.overlay_open {
            self.close_overlay();
        }

        // 返回键：一层一层退（浮层 → 搜索 → 退出）。桌面没有这个键，读了也恒假
        self.handle_back(ui.ctx());

        // ── 搜索的快捷键与到点的那次扫库，都排在画界面之前 ──
        if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::F)) {
            self.focus_search(narrow);
        }
        // 卡片开着时 `Esc` 一律收起，不看焦点在不在搜索框：
        // 跳过一次之后焦点在正文那一行上，这时用户想收掉卡片也该能按 Esc。
        // 收掉的是查询、命中与卡片，再 Ctrl+F 是从空白开始。
        // 浮层开着的时候 `Esc` 归浮层（它自己会收，见 `Modal::should_close`）：一次按键只做一件事
        if self.search.open()
            && !self.overlay_open
            && ui.input(|i| i.key_pressed(egui::Key::Escape))
        {
            self.clear_search();
        }
        if self.search_scheduler.is_due(now) {
            self.run_search();
        }

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
                if narrow {
                    shell::mobile_top_bar(ui, self.date, today, self.save_state())
                } else {
                    shell::top_bar(ui, self.date, today, self.save_state())
                }
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
        // 窄屏那条要放三枚 44pt 的按钮，比宽屏高；左右内边距也收窄一点，给按钮让位
        let next = self.next_day();
        let bar = egui::Panel::bottom("status_bar")
            .exact_size(if narrow {
                shell::MOBILE_STATUS_BAR_HEIGHT
            } else {
                shell::STATUS_BAR_HEIGHT
            })
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::NONE.fill(theme::SURFACE).inner_margin(
                    egui::Margin::symmetric(if narrow { 12 } else { shell::panel_padding_x() }, 0),
                ),
            )
            .show(ui, |ui| {
                if narrow {
                    shell::mobile_status_bar(ui, self.sheet.len(), next)
                } else {
                    shell::status_bar(ui, self.month, self.sheet.len());
                    ShellAction::default()
                }
            });
        shell::edge_line(ui, bar.response.rect, Edge::Top);
        action = action.merge(bar.inner);

        // ── 侧栏：搜索框 + 日历 ──
        // 宽屏占左边一整列；窄屏没这个宽度，同一份内容排成浮层，由底栏的「日历」「搜索」开
        let (search_action, picked, turned) = if !narrow {
            let bar = egui::Panel::left("sidebar")
                .exact_size(shell::SIDEBAR_WIDTH)
                .resizable(false)
                .show_separator_line(false)
                .frame(
                    egui::Frame::NONE
                        .fill(theme::SIDEBAR)
                        .inner_margin(egui::Margin::symmetric(20, 18)),
                )
                .show(ui, |ui| self.sidebar_body(ui));
            shell::edge_line(ui, bar.response.rect, Edge::Right);
            bar.inner
        } else if self.overlay_open {
            // 浮层排在结果卡片前面：卡片挂在搜索框下沿，位置来自这一帧刚画出来的那个框
            let width = (ui.available_width() - shell::MOBILE_MARGIN_X * 2.0)
                .clamp(shell::MOBILE_SHEET_MIN_WIDTH, shell::MOBILE_SHEET_MAX_WIDTH);
            let modal = egui::Modal::new(egui::Id::new("sidebar_overlay"))
                .frame(
                    egui::Frame::window(ui.style())
                        .fill(theme::SIDEBAR)
                        .inner_margin(egui::Margin::symmetric(18, 16)),
                )
                .show(ui.ctx(), |ui| {
                    // 日历要的宽度是固定的，先定死再排版
                    ui.set_width(width);
                    self.sidebar_body(ui)
                });
            // 点浮层外面、按 Esc 都收（`Esc` 只在它是上面那层时被吃）
            if modal.should_close() {
                self.close_overlay();
            }
            modal.inner
        } else {
            // 窄屏、浮层收着：这一帧既没有搜索框也没有日历。
            // 把它们叫回来是底栏那两枚按钮的事，这里什么都不画
            (SearchAction::default(), None, false)
        };

        // 查询改了 / 被清掉：命中的存废与防抖计时都跟着走
        // 被清掉优先于被改过：删掉最后一个字时 `changed` 与 `dismissed` 会同时为真
        // （见 `SearchView::field` 里的 `emptied`），而清掉是更强的意图——
        // 先走它，下面那个空查询分支就不必再兼顾“查询没了”这一种情况
        if search_action.dismissed {
            self.search.clear();
            self.drop_hits();
        } else if search_action.changed {
            if self.search.query().trim().is_empty() {
                // 查询空了就没什么可搜的，手里的命中也不留
                self.search_scheduler.mark_searched();
                self.drop_hits();
            } else {
                self.search_scheduler.record(now);
            }
        }
        if let Some(row) = search_action.open {
            self.open_row(row);
        }

        if let Some(date) = picked {
            // 窄屏：选完就该回正文，浮层收掉（宽屏侧栏本来就在，不用管）
            if narrow {
                self.close_overlay();
            }
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

        // ── 跳到命中：把选区种进那一行的输入框状态，焦点也交给它 ──
        // 必须排在正文画完之后：`TextEdit` 每帧重建时会把已有的非空选区塌成
        // 一个点（`builder.rs` 里进事件处理之前那段），先种下去会被同一帧抹掉。
        // 放在这里，种的选区下一帧才被读，重建反而看不见它。
        // egui 只在输入框有焦点时画选区，所以焦点必须过去；
        // 接着打字就是替换选中的那句，与编辑器 / 浏览器查找一致
        self.apply_pending_jump(ui.ctx());

        // ── 搜索结果卡片：正文画完再浮上来 ──
        // 命中行里的摘要是按卡片宽度排的，所以它排在正文后面
        let query = self.search.query().trim();
        let jump = search::date_query(query);
        let results =
            if self.search_scheduler.needs_wakeup() || self.hit_query.as_deref() != Some(query) {
                // 防抖期间、或查询刚改过：不拿旧结果冒充
                Results::Pending
            } else if self.hit_failed {
                Results::Failed
            } else {
                Results::Ready(&self.hits.hits, self.hits.truncated)
            };
        if let Some(row) = self.search.results(ui.ctx(), jump, results) {
            self.open_row(row);
        }

        // ── 顶栏 / 底栏表达的意图 ──
        if action.goto_today {
            self.go_to(today);
        }
        if action.lock {
            self.lock(ui.ctx());
        }
        if let Some(intent) = action.overlay {
            self.open_overlay(intent);
        }
        if action.next_day
            && let Some(date) = self.next_day()
        {
            self.go_to(date);
        }

        // 查询刚改过：排到点那一帧去搜（那次搜索在下一帧帧首）；
        // 没改动就一帧都不多排
        if self.search_scheduler.needs_wakeup() {
            let wait = self.search_scheduler.time_until_due(now).max(0.01);
            ui.ctx()
                .request_repaint_after(Duration::from_secs_f64(wait));
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.render(ui);
    }

    /// 落盘钩子：手机上切后台（`Event::Suspended`）、eframe 的自动保存、退出前都会走到这里。
    ///
    /// 手机上没有"关窗"这个动作，它就是"关窗前写完"的等价物；防抖（0.4 秒）不变。
    /// 写失败不在这里报——错误已经存进 `error`，回到前台那一帧的红条会说出来。
    /// 桌面没开 `persistence`，这个钩子一次都不会被调到（见 `docs/android.md`）。
    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        self.flush();
    }

    /// 不把 egui 的内存写进持久化文件。
    ///
    /// 默认是 `true`，那会把 `TextEditState` 一起写出去：它的撤销缓冲里有一份正文，
    /// 与"锁定即丢正文"直接冲突。Android 上 `app.ron` 因此永远是空的，只为拿到
    /// [`eframe::App::save`] 这个钩子。
    fn persist_egui_memory(&self) -> bool {
        false
    }
}

impl App {
    /// 画一帧。`eframe` 每帧调它，测试也靠它——所以不接 `Frame`，只要一个 `Ui`。
    pub fn render(&mut self, ui: &mut egui::Ui) {
        self.handle_close_request(ui.ctx());
        self.flush_when_due(ui.ctx());
        // 解锁 / 设密码界面上按返回键就是"离开这个应用"：那里没有上一层可退
        if self.stage != Stage::Ready {
            self.handle_back(ui.ctx());
        }

        match self.stage {
            Stage::Setup | Stage::Unlock => self.password_screen(ui),
            Stage::Ready => self.diary_screen(ui),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_waits_for_the_debounce() {
        let mut scheduler = SearchScheduler::default();
        assert!(!scheduler.needs_wakeup(), "没改过就不用排帧");
        assert!(!scheduler.is_due(100.0), "没改过就谈不上到点");

        scheduler.record(10.0);
        assert!(scheduler.needs_wakeup(), "改了就要排帧等到点");
        assert!(!scheduler.is_due(10.0), "刚敲完不搜");
        assert!(!scheduler.is_due(10.0 + SEARCH_DEBOUNCE_SECONDS - 0.01));
        assert!(scheduler.is_due(10.0 + SEARCH_DEBOUNCE_SECONDS));

        scheduler.mark_searched();
        assert!(!scheduler.needs_wakeup(), "搜过了就不用再排帧");
        assert!(!scheduler.is_due(999.0), "搜过之后在下次改动前不再到点");
    }

    #[test]
    fn typing_again_restarts_the_timer() {
        let mut scheduler = SearchScheduler::default();
        scheduler.record(10.0);
        scheduler.record(10.2);

        assert!(!scheduler.is_due(10.3), "又敲了一个字，重新计时");
        assert!(scheduler.is_due(10.45));
    }

    #[test]
    fn the_remaining_wait_counts_down() {
        let mut scheduler = SearchScheduler::default();
        scheduler.record(10.0);

        assert!((scheduler.time_until_due(10.1) - (SEARCH_DEBOUNCE_SECONDS - 0.1)).abs() < 1e-9);
        assert_eq!(scheduler.time_until_due(99.0), 0.0, "已经到点就不要再等");
    }
}
