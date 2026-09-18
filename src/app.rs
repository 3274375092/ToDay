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
//! 输入法上屏的字（Android 独有）每帧读一次 game-activity 的快照，差分后注入 `egui`：
//! 时机在 [`eframe::App::raw_input_hook`]，差分与边界在 [`crate::ime`]，读法在 `crate::android`。
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
    /// 输入法快照的读入口；桌面是 `None`（`App::with_ime_source` 只在 Android 入口被调）
    #[cfg(target_os = "android")]
    ime_source: Option<winit::platform::android::activity::AndroidApp>,
    /// AGDK 的 native handle：读输入法快照要用；`None` = 还没拿到
    #[cfg(target_os = "android")]
    ime_handle: Option<i64>,
    /// 取 handle 试过几次了（到上限就不再试，见 [`App::ensure_ime_handle`]）
    #[cfg(target_os = "android")]
    ime_handle_tries: u32,
    /// 已经告诉输入法的编辑器大类；`None` = 还没说过（见 [`App::sync_ime_editor_info`]）
    #[cfg(target_os = "android")]
    ime_kind: Option<ImeKind>,
    /// 上一次快照里“已经上屏”的正文（组词区不算），下一次差分拿它当基准
    ime_committed: String,
    /// 只在 debug 构建里存在的量尺（见 `ImeStats`）；release 下整个字段不存在
    #[cfg(all(target_os = "android", debug_assertions))]
    ime_stats: ImeStats,
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
            #[cfg(target_os = "android")]
            ime_source: None,
            #[cfg(target_os = "android")]
            ime_handle: None,
            #[cfg(target_os = "android")]
            ime_handle_tries: 0,
            #[cfg(target_os = "android")]
            ime_kind: None,
            ime_committed: String::new(),
            #[cfg(all(target_os = "android", debug_assertions))]
            ime_stats: ImeStats::default(),
        })
    }

    /// 打开窗口尺寸记忆：关窗时把尺寸记到 `path`（见 [`crate::window_state`]）。
    ///
    /// 这是唯一开启它的入口；不开时一个字节都不会写到窗口状态文件。
    pub fn with_window_memory(mut self, path: PathBuf) -> Self {
        self.window_memory = Some(path);
        self
    }

    /// 接上输入法快照的来源（只有 Android 入口会调，见 `src/android.rs`）。
    ///
    /// 接了之后 [`eframe::App::raw_input_hook`] 每帧读一次输入法快照，把差量注入成
    /// 这一帧的 `egui` 事件。不接（桌面、测试）时那条路根本不存在。
    ///
    /// native handle 不在这里取：`initializeNativeCode` 先起 `android_main` 再写 Java 的
    /// `nativeHandle`，这一步问到的可能就是 0。头几帧由 `App::ensure_ime_handle` 补上。
    #[cfg(target_os = "android")]
    pub fn with_ime_source(mut self, app: winit::platform::android::activity::AndroidApp) -> Self {
        self.ime_source = Some(app);
        self
    }

    /// 把一份输入法快照差分进这一帧要注入的事件里。
    ///
    /// `text` 是输入法眼里的完整正文（含组词区），`compose` 是组词区的字节下标；
    /// 差分规则与边界在 [`crate::ime::injections`]。基准更新在 `ime_committed`。
    ///
    /// 返回**这一次快照差分出了动作没有**（新上屏 / 退格，哪怕随后被退格去重挡掉）：
    /// 测试用它区分“输入法真的推了东西”与“只是同一份快照又喂了一遍”。排帧不看它——
    /// 判据是“有没有输入框在焦点上”，见 [`crate::ime::next_interval`] 里量到的理由。
    ///
    /// 调用时序由 `eframe` 定：每帧画之前调一次（[`eframe::App::raw_input_hook`]），
    /// 所以这里塞进去的事件与同一帧的键盘事件走同一条路。同一段字只在它变化的那一帧
    /// 产出一次注入（拿着焦点的那一个输入框收到它们）。
    ///
    /// 是 `pub` 因为 Android 那条读法（`GameActivity` 的 C 接口）只能在 `android_main`
    /// 里拿到，集成测试只能直接喂快照正文；桌面运行时没有第二个人调它。
    pub fn apply_ime_snapshot(
        &mut self,
        raw_input: &mut egui::RawInput,
        text: &str,
        compose: Option<(usize, usize)>,
    ) -> bool {
        let (actions, committed) = crate::ime::injections(&self.ime_committed, text, compose);
        self.ime_committed = committed;
        let injected = !actions.is_empty();

        // 输入法删字可能既走 `sendKeyEvent`（→ winit → `Key::Backspace`）又改快照，
        // 两边都注入就一按删两个字。按**条数**合并：键盘那几次本来就删了字，只补够
        // `count` 的差额——少补的次数不会自己回来（基准已经推到新快照，下一帧差不出这部分）
        let mut keyboard_backspaces = raw_input
            .events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    egui::Event::Key {
                        key: egui::Key::Backspace,
                        pressed: true,
                        ..
                    }
                )
            })
            .count();
        for action in actions {
            match action {
                crate::ime::Injection::Text(text) => raw_input.events.push(egui::Event::Text(text)),
                // 退格造一个按键事件：文本编辑器认的就是它（输入法删字走的是
                // `deleteSurroundingText`，不是键盘事件）
                crate::ime::Injection::Backspace(count) => {
                    let already = count.min(keyboard_backspaces);
                    keyboard_backspaces -= already;
                    for _ in 0..count - already {
                        raw_input.events.push(egui::Event::Key {
                            key: egui::Key::Backspace,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::default(),
                        });
                    }
                }
            }
        }
        injected
    }

    /// 输入法这一帧：把一份快照差分进 `raw_input`，再决定这一帧要不要接着画。
    ///
    /// Android 上每帧由 [`eframe::App::raw_input_hook`] 喂一次真实快照
    /// （读法在 `crate::android::ime_snapshot`，这里是它唯一的正式调用方；那个模块只在
    /// Android 目标上编译，所以这里不写成文档链接）。
    /// 桌面没有输入法源，`tests/app_flow.rs` 的合成帧测试直接调这个方法——走的是
    /// 同一条路，只是快照由测试造。
    ///
    /// `snapshot` 为 `None` = 这一帧没读到（句柄还没到手、输入法还没推过东西、
    /// 或不是 Android）：基准 `ime_committed` 一个字都不动——**不要**因为没读到
    /// 就把基准清空，那会在下一次读到时把整段旧正文重新倒进输入框。
    ///
    /// 返回这一帧排下的间隔（`None` = 这一帧不排帧）；规则与它的真机依据在
    /// [`crate::ime::next_interval`]。
    pub fn ime_frame(
        &mut self,
        ctx: &egui::Context,
        raw_input: &mut egui::RawInput,
        snapshot: Option<(&str, Option<(usize, usize)>)>,
    ) -> Option<Duration> {
        if let Some((text, compose)) = snapshot {
            self.apply_ime_snapshot(raw_input, text, compose);
        }
        let interval = crate::ime::next_interval(ctx.text_edit_focused());
        if let Some(delay) = interval {
            ctx.request_repaint_after(delay);
        }
        interval
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

    /// 把这一帧量到的窗口尺寸写进记忆文件。
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

    /// 返回键：一层一层退，退到底就把窗口关掉。
    ///
    /// 顺序就是用户眼里的层次：浮层 → 搜索 → 关窗。
    ///
    /// 退到底时发的是 [`egui::ViewportCommand::Close`]，也就是桌面点关闭按钮那一条路：
    /// 落盘、写失败就不关，全由 [`App::handle_close_request`] 管。手机上事件循环因此退出、
    /// `android_main` 返回，由它收掉整个进程（见 `src/android.rs` 的 `end_process`）。
    /// **不要在这里自己退出进程**（真机上的栈见 `docs/android.md` 的"返回键与退出"）。
    ///
    /// 桌面基本碰不到这个键（有些键盘有），碰到了也当"关闭窗口"，语义一致。
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
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
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
        let bar =
            egui::Panel::bottom("status_bar")
                .exact_size(if narrow {
                    shell::MOBILE_STATUS_BAR_HEIGHT
                } else {
                    shell::STATUS_BAR_HEIGHT
                })
                .resizable(false)
                .show_separator_line(false)
                .frame(egui::Frame::NONE.fill(theme::SURFACE).inner_margin(
                    egui::Margin::symmetric(if narrow { 12 } else { shell::panel_padding_x() }, 0),
                ))
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
                    diary.show(ui, sheet, selected, shell::Chrome::of(narrow))
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
    /// 一帧的绘制。外面只多一层 `ImeStats` 的量尺（只在 debug 构建的 Android 上存在）。
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        #[cfg(all(target_os = "android", debug_assertions))]
        let started = std::time::Instant::now();

        self.render(ui);

        #[cfg(all(target_os = "android", debug_assertions))]
        self.ime_stats.record_draw(started.elapsed(), ui.ctx());
    }

    /// 落盘钩子：手机上切后台（`Event::Suspended`）、eframe 的自动保存、退出前都会走到这里。
    ///
    /// 手机上没有"关窗"这个动作，它就是"关窗前写完"的等价物；防抖（0.4 秒）不变。
    /// 写失败不在这里报——错误已经存进 `error`，回到前台那一帧的红条会说出来。
    /// 桌面没开 `persistence`，这个钩子一次都不会被调到（见 `docs/android.md`）。
    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        self.flush();
    }

    /// 输入法上屏的字：Android 的软键盘不走 `KeyEvent`，它的 `TextEvent` 又不被 winit 消费，
    /// 所以这里每帧读一次 game-activity 的文本快照，交给 [`App::ime_frame`] 差分、注入、排帧。
    ///
    /// `eframe` 每帧画之前调它（`prepare_raw_input`），所以注入的字和这一帧的键盘事件
    /// 走同一条路：该落的输入框、该动的光标都跟手敲的一样。
    /// 桌面没有输入法源（`App::with_ime_source` 是 Android 独有），这里是空转。
    fn raw_input_hook(&mut self, _ctx: &egui::Context, _raw_input: &mut egui::RawInput) {
        #[cfg(target_os = "android")]
        {
            self.cap_render_scale(_ctx);
            self.exit_if_activity_gone();
            if self.ime_source.is_some() {
                self.read_ime_snapshot(_ctx, _raw_input);
            }
        }
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

/// Android 独有：从 game-activity 读输入法快照、封顶渲染倍率。
///
/// 差分、注入与排帧节奏在 [`App::ime_frame`]（平台无关），这里负责“把快照读出来”、
/// “告诉输入法当前是什么输入框”与“别用屏幕原生的像素密度画”。
#[cfg(target_os = "android")]
impl App {
    /// 渲染倍率的上限（每 pt 多少物理像素）。
    ///
    /// 屏幕原生是 3.5（1260×2800 / density 560），一帧要画 1260×2800 像素。
    ///
    /// **封顶是整界面的缩放，不是"只降光栅化"**：`egui` 只给了
    /// `set_pixels_per_point`，它落成 `zoom_factor = 封顶值 / native`，而
    /// `zoom_factor` 缩放全部尺寸（字号、间距、触摸目标）。所以这台机器
    /// （native 恰好 3.5，封顶是空操作）上看不出差别，native 4.0 的机器上
    /// 封到 3.5 就是整个界面小 12.5%。真要"降帧预算又不动布局"，得先在 `egui`
    /// 那一层把光栅化倍率与缩放拆开。
    ///
    /// 3.5 = **不封顶**（见上：测试机的 native 就是它），真机上量过才定下的：3.5 时
    /// 连着打字与弹浮层都是 121 帧/2s（60Hz 满帧）、没有一帧超过一个 vsync，所以不用降。
    const MAX_PIXELS_PER_POINT: f32 = 3.5;

    /// 取 native handle 的重试上限（帧数）。前面的 SQLite 打开与迁移已经把
    /// `initializeNativeCode` 那点“先起 `android_main`、后写静态字段”的窗口盖过去了，
    /// 实际上第一次就拿到；这个上限只是别让它在异常情况下每帧去问 JNI。
    const IME_HANDLE_TRIES: u32 = 120;

    /// 把渲染倍率封到 [`Self::MAX_PIXELS_PER_POINT`]，下一帧生效。
    ///
    /// 每帧调都安全：值已经等于上限时 `egui` 自己短路（`Context::set_pixels_per_point`
    /// 第一句就是拿当前值比一下），值变了才落成 `zoom_factor`。**不要在帧中间改**：
    /// 那会让这一帧的布局与光栅化用两个倍率（所以放在 `raw_input_hook` 里——
    /// 它在下一次 `begin_pass` 之前）。
    fn cap_render_scale(&self, ctx: &egui::Context) {
        let native = ctx.native_pixels_per_point().unwrap_or(1.0);
        let capped = native.min(Self::MAX_PIXELS_PER_POINT);
        if capped < native {
            ctx.set_pixels_per_point(capped);
        }
    }

    /// 发现 Activity 被系统销毁就落盘、然后自己收掉进程。
    ///
    /// 为什么要：Activity 没了之后 winit 0.30 不结束事件循环（它忽略 `MainEvent::Destroy`），
    /// 进程会变成"没有窗口还在跑"的僵尸——解锁后的密钥留在内存里，而且下次启动只能撞
    /// `EventLoopError::RecreationAttempt`（见 `crate::android::end_process`）。
    /// 自己收掉之后，用户下次点开拿到的是干净的进程。
    ///
    /// 顺序是**先落盘再收**：真到这一步用户已经看不到界面，写不成也只记一条 error——
    /// 留着进程并不会把编辑写进去（下次启动会先把它收掉），只会把密钥多留一会儿。
    ///
    /// 只在有帧的时候跑到（`raw_input_hook` 每帧一次）：这一帧要画才会问
    /// `isDestroyed()`；没帧的时候也没什么可落的盘。读不到答案（`None`）时什么都不做，
    /// 绝不把还活着的 Activity 误判成僵尸。
    fn exit_if_activity_gone(&mut self) {
        // clone 一份句柄：下面 `flush` 要 `&mut self`，而 `AndroidApp` 只是个 `Arc` 包着的句柄
        let Some(source) = self.ime_source.clone() else {
            return;
        };
        if crate::android::activity_gone(&source) != Some(true) {
            return;
        }
        log::info!("Activity 已经销毁：落盘后收掉这个进程");
        if !self.flush() {
            log::error!("Activity 销毁后落盘失败：这次编辑会随进程一起没");
        }
        crate::android::end_process(&source);
    }

    /// 读一次输入法快照，交给 [`App::ime_frame`] 差分、注入并排下一帧。
    ///
    /// 快照的读法在 [`crate::android::ime_snapshot`]（只读、不改状态；为什么不用
    /// `AndroidApp::text_input_state()` 见那里）。
    /// 取不到 handle 或这一次读不出来就什么都不做，下一帧再试（只读不清标志，不会丢字）；
    /// 排帧只看焦点，与这一次读到没读到无关（见 [`App::ime_frame`]）。
    fn read_ime_snapshot(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.ensure_ime_handle();
        self.sync_ime_editor_info();

        let snapshot = self.ime_handle.and_then(crate::android::ime_snapshot);
        #[cfg(debug_assertions)]
        self.ime_stats.record_snapshot();
        let interval = self.ime_frame(
            ctx,
            raw_input,
            snapshot
                .as_ref()
                .map(|(text, compose)| (text.as_str(), *compose)),
        );
        #[cfg(debug_assertions)]
        self.ime_stats.record_tier(interval);
    }

    /// 拿到 AGDK 的 native handle（一次就够，同一个进程里它是不变的）。
    ///
    /// 取不到就前 [`Self::IME_HANDLE_TRIES`] 帧反复试（见 [`App::with_ime_source`] 的时序；
    /// 0 也算取不到，`native_handle` 会把它滤掉）；到上限只记一条 warn 并放弃——
    /// 中文上屏不工作，应用照常跑。
    fn ensure_ime_handle(&mut self) {
        if self.ime_handle.is_some() || self.ime_handle_tries > Self::IME_HANDLE_TRIES {
            return;
        }
        let Some(source) = self.ime_source.as_ref() else {
            return;
        };
        self.ime_handle = crate::android::native_handle(source);
        if self.ime_handle.is_some() {
            return;
        }
        self.ime_handle_tries += 1;
        if self.ime_handle_tries > Self::IME_HANDLE_TRIES {
            log::warn!("拿不到 game-activity 的 native handle：手机上中文上屏不会工作");
        }
    }

    /// 当前这一屏该给输入法哪种编辑器。
    ///
    /// 不给的话 `inputType` 是 0（`TYPE_NULL`），输入法只能摆一个数字/符号键盘——
    /// 真机上量到过，拼音一个字都打不出来。
    fn wanted_ime_kind(&self) -> ImeKind {
        match self.stage {
            Stage::Setup | Stage::Unlock => ImeKind::Password,
            Stage::Ready if self.overlay_open => ImeKind::Text,
            Stage::Ready => ImeKind::Multiline,
        }
    }

    /// 编辑器的大类变了就告诉输入法。
    ///
    /// `set_ime_editor_info` 只改 AGDK 手里那份 `EditorInfo`，输入法下次建连接时才看得到；
    /// 而这恰好就是它被叫起来的时候：`egui` 那边给焦点时会走 `show_soft_input` → AGDK 内部
    /// `restartInput` → 框架重新问一次连接，新的 `EditorInfo` 就在那一次交给它。
    ///
    /// **不要在这里自己调 `show_soft_input`**：真机上量到过——紧接着 `restartInput` 的那次
    /// 改动会让系统把刚排上的那次显示取消掉（`ImeTracker: onCancelled at
    /// PHASE_CLIENT_ON_CONTROLS_CHANGED`），键盘反而弹不出来。
    fn sync_ime_editor_info(&mut self) {
        let Some(source) = self.ime_source.as_ref() else {
            return;
        };
        let want = self.wanted_ime_kind();
        if self.ime_kind == Some(want) {
            return;
        }
        source.set_ime_editor_info(want.input_type(), ImeKind::ACTION, ImeKind::OPTIONS);
        self.ime_kind = Some(want);
    }
}

/// 只在 debug 构建的 Android 上存在的量尺（`adb logcat -s ToDay` 每 [`Self::WINDOW_SECONDS`]
/// 秒一行汇总）。
///
/// 量的是这几条判据：
///
/// * egui 这一趟画了多久（p50 / p95 / 最长）与**两帧之间的间隔**（p50 / p95 / 最长、
///   超过一个 vsync 的帧数）——渲染倍率要不要封顶看后者（`dumpsys gfxinfo` 认不出
///   SurfaceView 里 Vulkan 画的帧，真机上它只数到个位数，所以只有应用自己量）；
/// * 连续两次读输入法快照的**最大间隔**——上屏延迟的上界：字进了快照之后最多等这么久
///   才被读走、才算进 `egui`；
/// * 当前排帧档位（画 / 停）与窗口尺寸变化的次数；
/// * 这一帧的焦点与 IME 请求（窗口焦点 / 控件焦点 / 给 eframe 的 `ime`）——
///   真机上"键盘弹不出来"到底是应用没要还是系统收了，看这一行。
///
/// 另有三类单帧日志：窗口尺寸变化那一帧、绘制 ≥ [`Self::SLOW_FRAME_MS`] 的帧、
/// 间隔 ≥ [`Self::LONG_INTERVAL_MS`] 的帧——“长帧与尺寸变化那一帧重合”就是键盘 resize
/// 那一下的代价（要不要改成不 resize 看它，结论在 `docs/android.md` 的“已知取舍”）。
///
/// release 构建里这个结构整个不存在（字段上的 `cfg` 与它一致），一条日志都不留。
#[cfg(all(target_os = "android", debug_assertions))]
#[derive(Default)]
struct ImeStats {
    /// 汇总窗口的起点（`None` = 还没开始）
    window_start: Option<std::time::Instant>,
    /// 本窗口每一帧的绘制耗时（毫秒）
    draws_ms: Vec<f32>,
    /// 本窗口每一帧与上一帧的间隔（毫秒）
    intervals_ms: Vec<f32>,
    /// 上一帧的时刻（量间隔用）
    last_frame: Option<std::time::Instant>,
    /// 上一次读快照的时刻
    last_read: Option<std::time::Instant>,
    /// 本窗口里两次读快照的最大间隔（毫秒）。`0.0` = 还没量到两次
    max_gap_ms: f32,
    /// 本窗口里最后一次排帧决策
    tier: &'static str,
    /// 上一帧的窗口矩形（点）
    last_rect: Option<egui::Rect>,
    /// 本窗口里窗口尺寸变了几次
    rect_changes: u32,
    /// 最近一帧的焦点 / IME 状态（“键盘为什么不弹”看这一行）
    focus_note: String,
}

#[cfg(all(target_os = "android", debug_assertions))]
impl ImeStats {
    /// 汇总周期（秒）
    const WINDOW_SECONDS: f32 = 2.0;

    /// 算“慢帧”的门槛（毫秒）：这台设备的 vsync 周期是 60Hz（16.7ms），
    /// 1.5 倍以上就是一眼能看出的一次顿。
    const SLOW_FRAME_MS: f32 = 25.0;

    /// 算“长间隔”的门槛（毫秒）：超过一个 vsync 周期（16.7ms）就没接上趟。
    /// 汇总里按 [`Self::DROPPED_AFTER_MS`] 算掉帧，单帧日志只记更长的
    /// （免得打字时每帧都写一行）。
    const DROPPED_AFTER_MS: f32 = 33.0;

    /// 单帧记一条的间隔门槛（毫秒）
    const LONG_INTERVAL_MS: f32 = 100.0;

    /// 记一帧的绘制耗时与它距上一帧的间隔；窗口尺寸变了或这一帧长得刺眼时额外记一行。
    fn record_draw(&mut self, draw: Duration, ctx: &egui::Context) {
        let now = std::time::Instant::now();
        let draw_ms = draw.as_secs_f32() * 1000.0;
        self.draws_ms.push(draw_ms);

        let interval_ms = self
            .last_frame
            .map(|prev| now.duration_since(prev).as_secs_f32() * 1000.0);
        if let Some(interval_ms) = interval_ms {
            self.intervals_ms.push(interval_ms);
        }
        self.last_frame = Some(now);

        // 窗口矩形（点）：键盘弹出/收起会改它（`adjustResize`），所以尺寸变化也能当判据
        let rect = ctx.input(|input| input.viewport_rect());
        let resized = self
            .last_rect
            .is_some_and(|prev| prev.size() != rect.size());
        if resized {
            self.rect_changes += 1;
            log::info!(
                "窗口尺寸变化：{:.0}×{:.0}pt → {:.0}×{:.0}pt；这一帧画了 {draw_ms:.1}ms，距上一帧 {:?}",
                self.last_rect.unwrap_or(rect).width(),
                self.last_rect.unwrap_or(rect).height(),
                rect.width(),
                rect.height(),
                interval_ms.map(|ms| format!("{ms:.0}ms")),
            );
        } else if draw_ms >= Self::SLOW_FRAME_MS {
            log::info!(
                "慢帧：画了 {draw_ms:.1}ms（距上一帧 {:?}）",
                interval_ms.map(|ms| format!("{ms:.0}ms"))
            );
        } else if interval_ms.is_some_and(|ms| ms >= Self::LONG_INTERVAL_MS) {
            log::info!(
                "长间隔：距上一帧 {:.0}ms（这一帧画了 {draw_ms:.1}ms，档位 {}）",
                interval_ms.unwrap_or_default(),
                self.tier,
            );
        }
        self.last_rect = Some(rect);

        // 应用这一侧到底在不在要键盘：窗口焦点来自 winit，控件焦点来自 egui，
        // `ime` 是给 eframe 的那份平台输出（`Some` = 请求显示/保持软键盘）
        self.focus_note = format!(
            "窗口焦点={} 控件焦点={} ime={:?}",
            ctx.input(|input| input.focused),
            ctx.text_edit_focused(),
            ctx.output(|output| output.ime),
        );

        if self.window_start.is_none() {
            self.window_start = Some(now);
        }
        self.report_if_due();
    }

    /// 记一次读快照；两次读之间的间隔就是“字在快照里最多躺多久”。
    fn record_snapshot(&mut self) {
        let now = std::time::Instant::now();
        if let Some(last) = self.last_read {
            self.max_gap_ms = self
                .max_gap_ms
                .max(now.duration_since(last).as_secs_f32() * 1000.0);
        }
        self.last_read = Some(now);
    }

    /// 记下这一帧排帧决策落在哪一档。
    fn record_tier(&mut self, interval: Option<Duration>) {
        self.tier = if interval.is_some() { "画" } else { "停" };
    }

    /// 到点就写一行汇总，然后清空窗口。
    fn report_if_due(&mut self) {
        let Some(started) = self.window_start else {
            return;
        };
        let elapsed = started.elapsed();
        if elapsed.as_secs_f32() < Self::WINDOW_SECONDS {
            return;
        }

        let mut sorted = std::mem::take(&mut self.draws_ms);
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        // 样本少时取最后一个（不是插值），够用：这是量尺，不是报表
        let pick = |values: &[f32], q: f32| -> f32 {
            if values.is_empty() {
                return 0.0;
            }
            let index = ((values.len() - 1) as f32 * q).round() as usize;
            values[index]
        };
        let mut intervals = std::mem::take(&mut self.intervals_ms);
        intervals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let dropped = intervals
            .iter()
            .filter(|ms| **ms >= Self::DROPPED_AFTER_MS)
            .count();
        log::info!(
            "量尺 {:.1}s：帧 {}；绘制 p50 {:.1}ms / p95 {:.1}ms / 最长 {:.1}ms；\
             间隔 p50 {:.0}ms / p95 {:.0}ms / 最长 {:.0}ms（≥{:.0}ms {} 帧）；\
             读快照最大间隔 {:.1}ms；档位 {}；尺寸变化 {} 次；{}",
            elapsed.as_secs_f32(),
            sorted.len(),
            pick(&sorted, 0.5),
            pick(&sorted, 0.95),
            sorted.last().copied().unwrap_or(0.0),
            pick(&intervals, 0.5),
            pick(&intervals, 0.95),
            intervals.last().copied().unwrap_or(0.0),
            Self::DROPPED_AFTER_MS,
            dropped,
            self.max_gap_ms,
            self.tier,
            self.rect_changes,
            self.focus_note,
        );

        self.max_gap_ms = 0.0;
        self.rect_changes = 0;
        self.window_start = Some(std::time::Instant::now());
    }
}

/// 输入框的大类——决定给输入法的 `inputType`。
///
/// 编辑器的细类（密码变体、多行旗标）由 `egui` 那边的 `TextEdit` 决定，
/// 这里只按“当前是哪一屏、浮层开着没有”分类：
/// 密码那两屏不弹候选、不记词；正文要多行且**要**留中文候选。
#[cfg(target_os = "android")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImeKind {
    /// 主密码：普通文本 + 不要候选
    Password,
    /// 单行文本（搜索框）
    Text,
    /// 正文：多行 + 允许候选
    Multiline,
}

#[cfg(target_os = "android")]
impl ImeKind {
    /// 回车键不交给输入法处理：`egui` 那边只把回车当普通按键（搜索是防抖的，不用回车确认）
    const ACTION: winit::platform::android::activity::input::TextInputAction =
        winit::platform::android::activity::input::TextInputAction::None;

    /// 不许输入法进“全屏提取”模式。
    ///
    /// 不给这一条时真机上是这个现象：IME 窗口请求整屏高度、键盘一个像素都画不出来
    /// （系统里 Settings 的搜索框就是带着这一条才弹得出来的）。
    const OPTIONS: winit::platform::android::activity::input::ImeOptions =
        winit::platform::android::activity::input::ImeOptions::IME_FLAG_NO_FULLSCREEN;

    fn input_type(self) -> winit::platform::android::activity::input::InputType {
        use winit::platform::android::activity::input::InputType;
        match self {
            // **不要**用 `TYPE_TEXT_VARIATION_PASSWORD`：vivo 会把带密码变体的编辑器路由给自家的
            // 安全输入法（`com.vivo.secime.service`），那套在这台机器上每次都是
            // `TYPE_SHOW - STATUS_FAIL`，键盘根本弹不出来（真机上量到过）。
            // `NO_SUGGESTIONS` 不是密码变体，不触发那条路由；它挡的是“输入法把主密码学进词库”。
            Self::Password => InputType::TYPE_CLASS_TEXT | InputType::TYPE_TEXT_FLAG_NO_SUGGESTIONS,
            Self::Text => InputType::TYPE_CLASS_TEXT,
            // 多行：拼音候选要留着（日记正文）
            Self::Multiline => InputType::TYPE_CLASS_TEXT | InputType::TYPE_TEXT_FLAG_MULTI_LINE,
        }
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
