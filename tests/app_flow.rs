//! 端到端：控制器 + 视图 + 加密存储真的接在一起时，一帧一帧跑下来是什么样。
//!
//! 没有显示器也能跑：用 `egui::Context::run_ui` 推进帧，用合成的输入事件
//! 当键盘（输密码、打字、回车），焦点按 id 直接投递。
//! 断言一律落在磁盘上——"界面上写了什么"最后必须能从库里读出来。

use ToDay::app::App;
use ToDay::model::{Change, MonthKey, MonthSheet};
use ToDay::search;
use ToDay::store::StoreError;
use ToDay::store::Vault;
use ToDay::store::sqlite::SqliteVault;
use ToDay::ui::search::SEARCH_FIELD_ID;
use ToDay::ui::{diary, theme, unlock};
use chrono::{Datelike as _, Local, NaiveDate};
use egui::widgets::text_edit::TextEditState;
use egui::{Event, Key};
use rusqlite::Connection;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PASSWORD: &str = "正确的马电池订书钉";

/// 一帧要喂给 egui 的东西
#[derive(Default)]
struct Frame {
    /// 这一帧的时刻（秒）。防抖按它计时，所以由测试说了算。
    time: f64,
    events: Vec<Event>,
    /// 这一帧把焦点放到哪个控件上
    focus: Option<egui::Id>,
    /// 这一帧是不是收到了关窗请求
    close: bool,
    /// 窗口有焦点没有（还没显示、或切到后台时没有）
    focused: bool,
    /// 这一帧量到的窗口矩形与是否最大化
    window: Option<(egui::Rect, bool)>,
    /// 窗口是不是全屏（macOS 上那个绿色按钮给的就是这个）
    fullscreen: bool,
    /// 屏幕矩形（点）。窄屏布局按整窗宽度分岔，而它只由 `RawInput::screen_rect` 决定
    screen: Option<egui::Rect>,
}

fn frame(time: f64) -> Frame {
    Frame {
        time,
        // 窗口默认是有焦点的：不然 `Response::has_focus` 恒为假，跟真实运行不一样
        focused: true,
        ..Default::default()
    }
}

/// 一个按下一次的按键事件
fn key_event(key: Key, modifiers: egui::Modifiers) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

impl Frame {
    fn text(mut self, text: &str) -> Self {
        self.events.push(Event::Text(text.to_owned()));
        self
    }

    fn enter(mut self) -> Self {
        self.events
            .push(key_event(Key::Enter, egui::Modifiers::default()));
        self
    }

    /// Ctrl（macOS 上是 Cmd）+ 回车。
    ///
    /// 键盘状态是一路积下来的（`InputState::modifiers` 只跟 `ModifiersChanged` 走），
    /// 所以除了按键本身，还得说明这个修饰键正按着。
    fn command_enter(mut self) -> Self {
        self.events
            .push(Event::ModifiersChanged(egui::Modifiers::COMMAND));
        self.events
            .push(key_event(Key::Enter, egui::Modifiers::COMMAND));
        self
    }

    /// Ctrl（macOS 上是 Cmd）+ F：聚焦搜索框
    fn command_f(mut self) -> Self {
        self.events
            .push(Event::ModifiersChanged(egui::Modifiers::COMMAND));
        self.events
            .push(key_event(Key::F, egui::Modifiers::COMMAND));
        self
    }

    fn escape(mut self) -> Self {
        self.events
            .push(key_event(Key::Escape, egui::Modifiers::default()));
        self
    }

    /// 退格删掉一个字符（单行输入框里的常规写法）
    fn backspace(mut self) -> Self {
        self.events
            .push(key_event(Key::Backspace, egui::Modifiers::default()));
        self
    }

    /// 在某个位置点一下鼠标（按下 + 抬起）
    fn click(mut self, pos: egui::Pos2) -> Self {
        self.events.push(Event::PointerMoved(pos));
        for pressed in [true, false] {
            self.events.push(Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            });
        }
        self
    }

    fn focused_on(mut self, id: egui::Id) -> Self {
        self.focus = Some(id);
        self
    }

    /// 等于用户点了窗口的关闭按钮
    fn closing(mut self) -> Self {
        self.close = true;
        self
    }

    /// 窗口还没拿到焦点（刚建好、还没显示的时候就是这样）
    fn unfocused(mut self) -> Self {
        self.focused = false;
        self
    }

    /// 这一帧量到的窗口矩形；`maximized` 为真时矩形是铺满屏幕的那块
    fn window(mut self, size: (f32, f32), maximized: bool) -> Self {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(size.0, size.1));
        self.window = Some((rect, maximized));
        self
    }

    /// 窗口全屏
    fn fullscreen(mut self) -> Self {
        self.fullscreen = true;
        self
    }

    /// 屏幕尺寸（点）。egui 不设它时默认 10000×10000，既有测试跑的就是那个“宽屏”。
    fn screen(mut self, width: f32, height: f32) -> Self {
        self.screen = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(width, height),
        ));
        self
    }

    /// 返回键（Android 上 game-activity 给的 `Keycode::Back` → `Key::BrowserBack`）
    fn back(mut self) -> Self {
        self.events
            .push(key_event(Key::BrowserBack, egui::Modifiers::default()));
        self
    }
}

/// 跑一帧，把这一帧的完整输出交出来（纹理增量自己收掉）。
fn run_output(ctx: &egui::Context, app: &mut App, frame: Frame) -> egui::FullOutput {
    run_ime(ctx, app, frame, None).0
}

/// 上面那条的完整版：`ime` 是这一帧先喂给 [`App::ime_frame`] 的输入法快照
/// （`None` = 不跑输入法那一趟，与桌面运行一致）。
///
/// 顺序照抄 eframe 的 `prepare_raw_input`：**先**跑输入法那一趟（`raw_input_hook`），
/// 再把同一份 `RawInput` 交给 `egui` 画——注入的事件因此和这一帧的键盘事件走同一条路。
/// 返回这一帧的输出与 [`App::ime_frame`] 排下的间隔（`None` = 这一帧不排帧）。
fn run_ime(
    ctx: &egui::Context,
    app: &mut App,
    frame: Frame,
    ime: Option<(&str, Option<(usize, usize)>)>,
) -> (egui::FullOutput, Option<Duration>) {
    // egui 要求每个 RawInput 都带上当前视口的信息
    let mut viewports = egui::ViewportIdMap::default();
    viewports.insert(
        egui::ViewportId::ROOT,
        egui::ViewportInfo {
            events: if frame.close {
                vec![egui::ViewportEvent::Close]
            } else {
                Vec::new()
            },
            focused: Some(frame.focused),
            inner_rect: frame.window.map(|(rect, _)| rect).or(frame.screen),
            maximized: frame.window.map(|(_, maximized)| maximized),
            fullscreen: Some(frame.fullscreen),
            ..Default::default()
        },
    );

    let mut input = egui::RawInput {
        time: Some(frame.time),
        events: frame.events,
        focused: frame.focused,
        // 窄屏测试靠它定整窗宽度；不设时 egui 用 10000×10000（既有测试就是那个宽度）
        screen_rect: frame.screen,
        viewports,
        ..Default::default()
    };
    let paced = ime.and_then(|snapshot| app.ime_frame(ctx, &mut input, Some(snapshot)));

    let mut output = ctx.run_ui(input, |ui| {
        if let Some(id) = frame.focus {
            // 键盘焦点由测试投递：没法用鼠标点，就直接告诉 egui 谁拿着焦点
            ui.memory_mut(|memory| memory.request_focus(id));
        }
        app.render(ui);
    });
    output.textures_delta.clear();

    (output, paced)
}

/// 应用向根窗口提的要求（"先别关" / "可以关了"）
fn commands(output: &egui::FullOutput) -> Vec<egui::ViewportCommand> {
    output
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|viewport| viewport.commands.clone())
        .unwrap_or_default()
}

/// 跑一帧，返回这一帧里应用向窗口提的要求（比如"先别关"和"可以关了"）。
fn run(ctx: &egui::Context, app: &mut App, frame: Frame) -> Vec<egui::ViewportCommand> {
    commands(&run_output(ctx, app, frame))
}

/// 这一帧画出来的文字，拼成一份"屏幕上有什么字"的清单
fn texts(output: &egui::FullOutput) -> String {
    fn collect(shape: &egui::Shape, out: &mut String) {
        match shape {
            egui::Shape::Text(text) => {
                out.push_str(text.galley.text());
                out.push('\n');
            }
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    collect(shape, out);
                }
            }
            _ => {}
        }
    }

    let mut out = String::new();
    for clipped in &output.shapes {
        collect(&clipped.shape, &mut out);
    }
    out
}

/// 从这一帧画出来的文字里找一段，给出它在屏幕上的位置。
///
/// 同一段文字可能在正文里也出现一次：浮层卡片画在 `Foreground`，排在最后，所以取最后一个匹配。
fn last_text_pos(output: &egui::FullOutput, needle: &str) -> Option<egui::Pos2> {
    fn collect(shape: &egui::Shape, needle: &str, out: &mut Option<egui::Pos2>) {
        match shape {
            egui::Shape::Text(text) => {
                if text.galley.text() == needle {
                    *out = Some(text.pos);
                }
            }
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    collect(shape, needle, out);
                }
            }
            _ => {}
        }
    }

    let mut found = None;
    for clipped in &output.shapes {
        collect(&clipped.shape, needle, &mut found);
    }
    found
}

/// 本月的第一天与"今天"，用来定位缓存的月份与行
fn today() -> NaiveDate {
    Local::now().date_naive()
}

/// 直接问 SQLite：现在库里存着几行
fn raw_rows(path: &Path) -> i64 {
    let conn = Connection::open(path).unwrap();
    conn.query_row("SELECT COUNT(*) FROM entries", [], |row| row.get(0))
        .unwrap()
}

/// 开一个设好密码的临时库，返回目录与路径
fn vault_with_password() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("today.db");
    let mut vault = SqliteVault::open(&path).unwrap();
    vault.setup(PASSWORD).unwrap();
    drop(vault);
    (dir, path)
}

/// 画几帧解锁卡片，把焦点送到主密码框
fn open_the_unlock_screen(ctx: &egui::Context, app: &mut App) {
    run(
        ctx,
        app,
        frame(0.0).focused_on(egui::Id::new(unlock::PASSWORD_FIELD_ID)),
    );
    run(
        ctx,
        app,
        frame(0.1).focused_on(egui::Id::new(unlock::PASSWORD_FIELD_ID)),
    );
}

/// 替身在库里做过的动作，按发生顺序记
#[derive(Clone, Debug, PartialEq, Eq)]
enum Act {
    Apply(Vec<Change>),
    Search(String),
}

/// 替身里能被测试读到的部分：`App` 拿走 `Box<dyn Vault>` 的所有权，
/// 断言只能落在共享的这份记录上。
#[derive(Default)]
struct Spy {
    /// 每次 `apply` 收到的批次，按顺序记下来
    applied: Vec<Vec<Change>>,
    /// 每次 `search` 收到的查询（含失败的那几次）
    searches: Vec<String>,
    /// `apply` 与 `search` 的先后顺序：盯住"搜索之前先落盘"
    log: Vec<Act>,
    /// 接下来几次 `apply` 要失败（每失败一次减一）
    fail_next: usize,
    /// 接下来几次 `search` 要失败（每失败一次减一）
    fail_searches: usize,
    /// 接下来几次 `load_month` 要失败（每失败一次减一）
    fail_loads: usize,
    /// `lock()` 被调了几次
    locks: usize,
    /// 替身"库"里的正文
    entries: BTreeMap<NaiveDate, String>,
}

/// 内存替身：不碰磁盘，`apply` 与 `load_month` 都可以按次失败
struct FakeVault {
    spy: Rc<RefCell<Spy>>,
}

impl Vault for FakeVault {
    fn needs_setup(&self) -> Result<bool, StoreError> {
        Ok(false)
    }

    fn setup(&mut self, _password: &str) -> Result<(), StoreError> {
        Ok(())
    }

    fn unlock(&mut self, _password: &str) -> Result<(), StoreError> {
        Ok(())
    }

    fn lock(&mut self) {
        self.spy.borrow_mut().locks += 1;
    }

    fn load_month(&self, key: MonthKey) -> Result<MonthSheet, StoreError> {
        let mut spy = self.spy.borrow_mut();
        if spy.fail_loads > 0 {
            spy.fail_loads -= 1;
            return Err(StoreError::Corrupt("替身读不出来".to_owned()));
        }
        let (start, end) = key.range();
        let mut sheet = MonthSheet::empty(key);
        for (date, text) in &spy.entries {
            if *date >= start && *date < end {
                sheet.set(*date, text);
            }
        }
        Ok(sheet)
    }

    fn apply(&mut self, changes: &[Change]) -> Result<(), StoreError> {
        let mut spy = self.spy.borrow_mut();
        spy.applied.push(changes.to_vec());
        spy.log.push(Act::Apply(changes.to_vec()));
        if spy.fail_next > 0 {
            spy.fail_next -= 1;
            return Err(StoreError::Corrupt("替身装的写盘失败".to_owned()));
        }
        for change in changes {
            match change {
                Change::Set { date, text } => {
                    spy.entries.insert(*date, text.clone());
                }
                Change::Delete { date } => {
                    spy.entries.remove(date);
                }
            }
        }
        Ok(())
    }

    fn search(&self, needle: &str) -> Result<search::SearchResults, StoreError> {
        let mut spy = self.spy.borrow_mut();
        spy.searches.push(needle.to_owned());
        spy.log.push(Act::Search(needle.to_owned()));
        if spy.fail_searches > 0 {
            spy.fail_searches -= 1;
            return Err(StoreError::Corrupt("替身搜不了".to_owned()));
        }

        // 与 SQLite 侧同一套规则：按日期从新到旧，最多 `MAX_HITS` 天；
        // 多出来的那些说明被截了
        let mut hits: Vec<search::Hit> = spy
            .entries
            .iter()
            .filter_map(|(date, text)| search::hit(*date, text, needle))
            .collect();
        hits.sort_by_key(|hit| std::cmp::Reverse(hit.date));
        let truncated = hits.len() > search::MAX_HITS;
        hits.truncate(search::MAX_HITS);
        Ok(search::SearchResults { hits, truncated })
    }
}

/// 起一个跑在替身上的应用，返回（控制器、替身记录）
fn fake_app() -> (App, Rc<RefCell<Spy>>) {
    let spy = Rc::new(RefCell::new(Spy::default()));
    let vault = FakeVault {
        spy: Rc::clone(&spy),
    };
    (App::new(Box::new(vault)).unwrap(), spy)
}

/// 在替身上解锁进主界面（替身不校验密码）
fn open_the_diary(ctx: &egui::Context, app: &mut App) {
    open_the_unlock_screen(ctx, app);
    run(
        ctx,
        app,
        frame(0.2)
            .focused_on(egui::Id::new(unlock::PASSWORD_FIELD_ID))
            .text(PASSWORD),
    );
    run(ctx, app, frame(0.3).enter());
    run(ctx, app, frame(0.4));
}

#[test]
fn first_run_sets_a_password_then_writes_into_the_encrypted_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("today.db");
    let day = today();

    let ctx = egui::Context::default();
    theme::install(&ctx);

    let mut app = App::new(Box::new(SqliteVault::open(&path).unwrap())).unwrap();

    // 设密码：两次输入 + 回车
    let password_field = egui::Id::new(unlock::PASSWORD_FIELD_ID);
    let confirm_field = egui::Id::new(unlock::CONFIRM_FIELD_ID);
    run(&ctx, &mut app, frame(0.0).focused_on(password_field));
    run(&ctx, &mut app, frame(0.1).text(PASSWORD));
    run(
        &ctx,
        &mut app,
        frame(0.2).focused_on(confirm_field).text(PASSWORD),
    );
    run(&ctx, &mut app, frame(0.3).enter());

    // 回到磁盘上看：密码设上了，库里还是空的
    assert!(!SqliteVault::open(&path).unwrap().needs_setup().unwrap());
    assert_eq!(raw_rows(&path), 0, "光设密码不该写进任何一行");

    // 主界面第一帧，然后在"今天"那行打字
    let row = diary::row_id(day);
    run(&ctx, &mut app, frame(0.4));
    run(&ctx, &mut app, frame(0.5).focused_on(row));
    run(&ctx, &mut app, frame(0.6).text("今天把加密接上了"));

    // 还没到防抖的 0.4 秒：一个字都不该落盘
    run(&ctx, &mut app, frame(0.7).focused_on(row));
    assert_eq!(raw_rows(&path), 0, "防抖期间不该写盘");

    // 安静够久了：一次写进去
    run(&ctx, &mut app, frame(1.5).focused_on(row));
    assert_eq!(raw_rows(&path), 1, "安静 0.4 秒之后应该写盘");

    drop(app);

    // 换一个连接读回来：正文对得上
    let mut vault = SqliteVault::open(&path).unwrap();
    vault.unlock(PASSWORD).unwrap();
    let sheet = vault.load_month(MonthKey::from_date(day)).unwrap();
    assert_eq!(sheet.text(day), "今天把加密接上了");
}

#[test]
fn unlocking_and_typing_lands_in_the_database() {
    let (_dir, path) = vault_with_password();
    let day = today();
    let row = diary::row_id(day);

    let ctx = egui::Context::default();
    theme::install(&ctx);
    let mut app = App::new(Box::new(SqliteVault::open(&path).unwrap())).unwrap();

    open_the_unlock_screen(&ctx, &mut app);
    run(
        &ctx,
        &mut app,
        frame(0.2)
            .focused_on(egui::Id::new(unlock::PASSWORD_FIELD_ID))
            .text(PASSWORD),
    );
    run(&ctx, &mut app, frame(0.3).enter());

    run(&ctx, &mut app, frame(0.4));
    run(
        &ctx,
        &mut app,
        frame(0.5).focused_on(row).text("解锁之后写的"),
    );
    run(&ctx, &mut app, frame(1.4).focused_on(row));

    drop(app);

    let mut vault = SqliteVault::open(&path).unwrap();
    vault.unlock(PASSWORD).unwrap();
    assert_eq!(
        vault
            .load_month(MonthKey::from_date(day))
            .unwrap()
            .text(day),
        "解锁之后写的"
    );
}

#[test]
fn a_wrong_password_never_reaches_the_database() {
    let (_dir, path) = vault_with_password();

    let ctx = egui::Context::default();
    theme::install(&ctx);
    let mut app = App::new(Box::new(SqliteVault::open(&path).unwrap())).unwrap();

    open_the_unlock_screen(&ctx, &mut app);
    // 错的密码 + 回车：被拒，还在解锁界面
    run(
        &ctx,
        &mut app,
        frame(0.2)
            .focused_on(egui::Id::new(unlock::PASSWORD_FIELD_ID))
            .text("这不是我的密码")
            .enter(),
    );

    // 就算接下来往"今天"那行打字，也一个字都写不进去
    run(
        &ctx,
        &mut app,
        frame(0.3).focused_on(diary::row_id(today())),
    );
    run(
        &ctx,
        &mut app,
        frame(0.4)
            .focused_on(diary::row_id(today()))
            .text("偷偷写的"),
    );
    run(
        &ctx,
        &mut app,
        frame(1.5).focused_on(diary::row_id(today())),
    );

    assert_eq!(raw_rows(&path), 0, "没解锁就写进去等于加密白做");

    // 密码还是原来那个，数据也没被弄坏
    let mut vault = SqliteVault::open(&path).unwrap();
    vault.unlock(PASSWORD).unwrap();
    assert!(
        vault
            .load_month(MonthKey::from_date(today()))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn losing_focus_writes_right_away() {
    let (_dir, path) = vault_with_password();
    let day = today();
    let row = diary::row_id(day);

    let ctx = egui::Context::default();
    theme::install(&ctx);
    let mut app = App::new(Box::new(SqliteVault::open(&path).unwrap())).unwrap();

    open_the_unlock_screen(&ctx, &mut app);
    run(
        &ctx,
        &mut app,
        frame(0.2)
            .focused_on(egui::Id::new(unlock::PASSWORD_FIELD_ID))
            .text(PASSWORD),
    );
    run(&ctx, &mut app, frame(0.3).enter());

    run(&ctx, &mut app, frame(0.4));
    run(&ctx, &mut app, frame(0.5).focused_on(row));
    run(
        &ctx,
        &mut app,
        frame(0.6).focused_on(row).text("失焦就该写进去"),
    );
    assert_eq!(raw_rows(&path), 0);

    // 焦点离开这一行（这里没有别的输入框，就交给一个没人认领的 id）：立刻落盘，不等防抖
    run(
        &ctx,
        &mut app,
        frame(0.65).focused_on(egui::Id::new("没人认领的焦点")),
    );
    assert_eq!(raw_rows(&path), 1, "离开一行应该马上写盘");

    drop(app);
    let mut vault = SqliteVault::open(&path).unwrap();
    vault.unlock(PASSWORD).unwrap();
    assert_eq!(
        vault
            .load_month(MonthKey::from_date(day))
            .unwrap()
            .text(day),
        "失焦就该写进去"
    );
}

#[test]
fn closing_the_window_writes_before_it_closes() {
    let (_dir, path) = vault_with_password();
    let day = today();
    let row = diary::row_id(day);

    let ctx = egui::Context::default();
    theme::install(&ctx);
    let mut app = App::new(Box::new(SqliteVault::open(&path).unwrap())).unwrap();

    open_the_unlock_screen(&ctx, &mut app);
    run(
        &ctx,
        &mut app,
        frame(0.2)
            .focused_on(egui::Id::new(unlock::PASSWORD_FIELD_ID))
            .text(PASSWORD),
    );
    run(&ctx, &mut app, frame(0.3).enter());

    run(&ctx, &mut app, frame(0.4));
    run(&ctx, &mut app, frame(0.5).focused_on(row));
    run(
        &ctx,
        &mut app,
        frame(0.6).focused_on(row).text("关窗也不能丢"),
    );

    // 防抖还没到点就点了关闭：先取消关闭、写盘、再请求关闭
    let commands = run(&ctx, &mut app, frame(0.7).closing());
    assert_eq!(raw_rows(&path), 1, "关窗之前必须把攒着的写完");
    assert_eq!(
        commands,
        vec![
            egui::ViewportCommand::CancelClose,
            egui::ViewportCommand::Close
        ],
        "顺序不对要么丢数据，要么窗口关不掉"
    );

    drop(app);
    let mut vault = SqliteVault::open(&path).unwrap();
    vault.unlock(PASSWORD).unwrap();
    assert_eq!(
        vault
            .load_month(MonthKey::from_date(day))
            .unwrap()
            .text(day),
        "关窗也不能丢"
    );
}

/// 无修饰键的回车在正文里换行，而不是“跳到下一行”：正文可以多行。
#[test]
fn enter_writes_a_newline_inside_the_day() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, spy) = fake_app();
    open_the_diary(&ctx, &mut app);
    let row = diary::row_id(today());

    run(&ctx, &mut app, frame(0.5).focused_on(row).text("第一行"));
    run(
        &ctx,
        &mut app,
        frame(0.6).focused_on(row).enter().text("第二行"),
    );
    // 过防抖：整段正文（含换行）原样落到库里
    run(&ctx, &mut app, frame(1.2).focused_on(row));
    assert_eq!(spy.borrow().entries[&today()], "第一行\n第二行");
}

/// Ctrl/Cmd+Enter：焦点交给列表里的下一行，并且这批编辑不等防抖就落盘。
#[test]
fn ctrl_enter_moves_to_the_next_written_day() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, spy) = fake_app();
    let day = today();
    let next = day.succ_opt().unwrap();
    // 月末的最后一天在列表里没有“下一天”，这条用例只在有下一行时成立
    if MonthKey::from_date(next) != MonthKey::from_date(day) {
        return;
    }
    // 替身里先写下明天：明天的行才会出现在列表里
    spy.borrow_mut()
        .entries
        .insert(next, "明天要写的事".to_owned());
    open_the_diary(&ctx, &mut app);
    let row = diary::row_id(day);

    run(
        &ctx,
        &mut app,
        frame(0.5).focused_on(row).text("今天写一句"),
    );
    run(&ctx, &mut app, frame(0.6).focused_on(row).command_enter());
    assert_eq!(
        ctx.memory(|memory| memory.focused()),
        Some(diary::row_id(next)),
        "Ctrl+Enter 该把焦点交给下一天"
    );

    // 焦点离开今天那一行：下一帧就落盘，不用等防抖到点
    run(&ctx, &mut app, frame(0.7));
    assert_eq!(spy.borrow().entries[&day], "今天写一句");
}

#[test]
fn a_locked_database_still_renders_the_unlock_screen() {
    let (_dir, path) = vault_with_password();

    let ctx = egui::Context::default();
    theme::install(&ctx);
    let mut app = App::new(Box::new(SqliteVault::open(&path).unwrap())).unwrap();

    // 多画几帧：解锁界面反复重绘也不该出岔子
    for step in 0..5 {
        run(&ctx, &mut app, frame(step as f64 * 0.1));
    }
    assert_eq!(raw_rows(&path), 0);
}

/// 写失败之后不该再排帧：排了也不会写，只会把空闲 CPU 烧成每 10 ms 一帧。
#[test]
fn a_failed_write_does_not_keep_repainting() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, spy) = fake_app();
    open_the_diary(&ctx, &mut app);
    let row = diary::row_id(today());

    spy.borrow_mut().fail_next = 1;
    run(
        &ctx,
        &mut app,
        frame(0.5).focused_on(row).text("写不进去的一句"),
    );

    // 防抖到点：写一次，失败，界面上说出来
    let failed = run_output(&ctx, &mut app, frame(1.0).focused_on(row));
    assert_eq!(spy.borrow().applied.len(), 1, "过防抖就写一次");
    let printed = texts(&failed);
    assert!(printed.contains("未保存"), "{printed}");

    // 再空转几帧：一次也不该再写
    let mut idle = failed;
    for step in 0..5 {
        idle = run_output(&ctx, &mut app, frame(1.1 + step as f64 * 0.1));
    }
    assert_eq!(spy.borrow().applied.len(), 1, "写失败之后不该反复写");
    // 调用次数看不出"空转"这个病，真正的判据是下一次排帧的间隔：
    // 写失败后 `time_until_due` 恒为 0，老代码每帧都排一帧 10 ms 后的
    let delay = idle.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
    assert!(
        delay >= Duration::from_millis(400),
        "写失败之后还在一帧一帧地空转：{delay:?}"
    );
}

/// 换月失败之后，绘制路径不会再写一次：只有用户的下一个动作才值得重试。
#[test]
fn a_failed_month_switch_is_not_retried_on_every_frame() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, spy) = fake_app();
    open_the_diary(&ctx, &mut app);
    let row = diary::row_id(today());
    // 四十天之后一定在别的月份里
    let other = today() + chrono::Duration::days(40);

    // 先攒一句没落盘的：换页前写这一次会失败，页就换不过去
    run(
        &ctx,
        &mut app,
        frame(0.5).focused_on(row).text("还没落盘的一句"),
    );
    spy.borrow_mut().fail_next = 1;
    app.go_to(other);
    assert_eq!(
        spy.borrow().applied.len(),
        1,
        "换月要先把旧页写完：写这一次，失败"
    );

    // 日历显示的是别的月、缓存还停在旧月——但一帧都不该再写
    for step in 0..5 {
        run(&ctx, &mut app, frame(0.6 + step as f64 * 0.1));
    }
    assert_eq!(
        spy.borrow().applied.len(),
        1,
        "换月失败之后每一帧都会再写一次盘"
    );

    // 重试的入口是用户的动作：再跳一次就再写一次（这次真写进去了）
    app.go_to(other);
    assert_eq!(spy.borrow().applied.len(), 2, "用户再动一次就该再试一次");
}

/// 读盘失败留下的错误条不会被一次成功的写盘抹掉：那一页其实还是空的兜底页。
#[test]
fn a_load_failure_outlives_a_successful_save() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, spy) = fake_app();
    // 解锁后读当月就会失败
    spy.borrow_mut().fail_loads = 1;
    open_the_diary(&ctx, &mut app);
    let row = diary::row_id(today());

    let printed = texts(&run_output(&ctx, &mut app, frame(0.45)));
    assert!(
        printed.contains("替身读不出来"),
        "读不了要说出来：{printed}"
    );
    assert!(
        printed.contains("知道了"),
        "这条收起来不丢东西，该给“知道了”：{printed}"
    );
    assert!(
        !printed.contains("重试"),
        "没有压着的编辑，不必给重试：{printed}"
    );

    // 写一句并落盘：状态字回"已保存"，读失败的提示留着
    run(
        &ctx,
        &mut app,
        frame(0.5).focused_on(row).text("照样写进去了"),
    );
    let printed = texts(&run_output(&ctx, &mut app, frame(1.0).focused_on(row)));
    assert!(printed.contains("已保存"), "{printed}");
    assert!(
        printed.contains("替身读不出来"),
        "一次成功的写盘治不了读失败：{printed}"
    );
    assert_eq!(spy.borrow().entries[&today()], "照样写进去了");
}

/// 失败之后点"重试"：立刻再写一次；成功就把错误条收掉、状态字回到"已保存"。
#[test]
fn retrying_after_a_failure_writes_the_edit() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, spy) = fake_app();
    open_the_diary(&ctx, &mut app);
    let row = diary::row_id(today());

    spy.borrow_mut().fail_next = 1;
    run(
        &ctx,
        &mut app,
        frame(0.5).focused_on(row).text("先失败再成功"),
    );
    run(&ctx, &mut app, frame(1.0).focused_on(row));

    let printed = texts(&run_output(&ctx, &mut app, frame(1.1)));
    assert!(
        printed.contains("未保存") && printed.contains("重试"),
        "{printed}"
    );
    assert!(printed.contains("替身装的写盘失败"), "{printed}");
    assert!(
        !printed.contains("知道了"),
        "编辑还压在队里，不该给能把它收起来的按钮：{printed}"
    );

    // 重试：真的再写一次，这次写进去
    app.retry_save();
    assert_eq!(spy.borrow().applied.len(), 2, "重试要真的再写一次");
    assert_eq!(spy.borrow().entries[&today()], "先失败再成功");

    let printed = texts(&run_output(&ctx, &mut app, frame(1.2)));
    assert!(printed.contains("已保存"), "{printed}");
    assert!(
        !printed.contains("未保存") && !printed.contains("重试"),
        "写进去了就不该再报错：{printed}"
    );

    // 队列空了再点一次：幂等，不会多写一次
    app.retry_save();
    assert_eq!(spy.borrow().applied.len(), 2);
}

/// 顶栏那枚状态字跟着队列走
#[test]
fn the_save_state_word_follows_the_queue() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, spy) = fake_app();
    open_the_diary(&ctx, &mut app);
    let row = diary::row_id(today());

    let printed = texts(&run_output(&ctx, &mut app, frame(0.45)));
    assert!(printed.contains("已保存"), "没动过就是已保存：{printed}");

    run(&ctx, &mut app, frame(0.5).focused_on(row).text("写了一句"));
    let printed = texts(&run_output(&ctx, &mut app, frame(0.6).focused_on(row)));
    assert!(printed.contains("保存中"), "防抖期间是保存中：{printed}");

    // 安静够久：落盘，回到已保存
    let printed = texts(&run_output(&ctx, &mut app, frame(1.1).focused_on(row)));
    assert!(printed.contains("已保存"), "{printed}");

    // 这次写失败：未保存
    spy.borrow_mut().fail_next = 1;
    run(&ctx, &mut app, frame(1.2).focused_on(row).text("再写一句"));
    let printed = texts(&run_output(&ctx, &mut app, frame(1.9).focused_on(row)));
    assert!(printed.contains("未保存"), "{printed}");
}

/// 锁定之后 `egui` 手里那份正文副本也要丢掉
#[test]
fn locking_forgets_the_text_edits() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, spy) = fake_app();
    open_the_diary(&ctx, &mut app);
    let row = diary::row_id(today());

    run(
        &ctx,
        &mut app,
        frame(0.5).focused_on(row).text("锁定之前写的"),
    );
    assert!(
        TextEditState::load(&ctx, row).is_some(),
        "打字之后 egui 手里存着一份输入状态（撤销缓冲里有正文）"
    );

    app.lock(&ctx);
    assert_eq!(spy.borrow().locks, 1, "落盘之后才能丢密钥");
    assert_eq!(spy.borrow().entries[&today()], "锁定之前写的");
    assert!(
        TextEditState::load(&ctx, row).is_none(),
        "锁上之后不该还有正文副本"
    );
    assert!(
        TextEditState::load(&ctx, egui::Id::new(unlock::PASSWORD_FIELD_ID)).is_none(),
        "主密码那份副本一起丢"
    );

    // 回到解锁界面：再往原来那一行打字也不会写下去
    let before = spy.borrow().applied.len();
    run(
        &ctx,
        &mut app,
        frame(0.6).focused_on(row).text("锁上之后打的"),
    );
    run(&ctx, &mut app, frame(1.5).focused_on(row));
    assert_eq!(spy.borrow().applied.len(), before, "锁定之后写不进去");
}

/// 冷启动：窗口还没显形时拿不到键盘，密码框得把"要焦点"这件事留到真的能敲键盘
#[test]
fn the_unlock_field_takes_focus_once_the_window_is_focused() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, _spy) = fake_app();

    for step in 0..3 {
        run(&ctx, &mut app, frame(step as f64 * 0.1).unfocused());
    }

    // 窗口拿到焦点之后：不额外投递焦点，敲进去的密码就该被接住
    run(&ctx, &mut app, frame(0.4));
    assert_eq!(
        ctx.memory(|memory| memory.focused()),
        Some(egui::Id::new(unlock::PASSWORD_FIELD_ID)),
        "键盘该直接落在密码框里"
    );

    run(&ctx, &mut app, frame(0.5).text(PASSWORD));
    run(&ctx, &mut app, frame(0.6).enter());
    assert!(
        texts(&run_output(&ctx, &mut app, frame(0.7))).contains("本月记录"),
        "密码进了输入框、回车就该进主界面"
    );
}

/// 关窗时记下尺寸：只记尺寸与最大化，不记位置
#[test]
fn closing_remembers_the_window_size() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("window.txt");
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (app, _spy) = fake_app();
    let mut app = app.with_window_memory(path.clone());

    // 第一帧只负责显形与主题，量的是关窗那一帧
    run(&ctx, &mut app, frame(0.0));
    // 没有待写编辑：不拦关闭，只把尺寸记下来
    let commands = run(
        &ctx,
        &mut app,
        frame(0.1).window((1200.0, 800.0), false).closing(),
    );
    assert!(
        commands.is_empty(),
        "没有待写的东西就别拦关闭：{commands:?}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "1200 800 0");

    // 最大化关窗：标志翻上去，普通尺寸保持上一次的
    run(
        &ctx,
        &mut app,
        frame(0.2).window((1920.0, 1080.0), true).closing(),
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "1200 800 1");

    // 全屏关窗（macOS 上那个绿色按钮给的就是全屏；那边的 `maximized` 不是每帧都刷新）：
    // 整屏尺寸同样不能当作"普通尺寸"记下来
    run(
        &ctx,
        &mut app,
        frame(0.3)
            .window((3840.0, 2160.0), false)
            .fullscreen()
            .closing(),
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "1200 800 0");
}

/// 没开窗口记忆时，关窗不该写任何文件
#[test]
fn closing_without_window_memory_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, _spy) = fake_app();

    run(
        &ctx,
        &mut app,
        frame(0.0).window((1200.0, 800.0), false).closing(),
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

/// Ctrl/Cmd+F 聚焦搜索框；接着打字就搜。
#[test]
fn command_f_focuses_the_search_box() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, spy) = fake_app();
    spy.borrow_mut()
        .entries
        .insert(today(), "今天喝了咖啡".to_owned());
    open_the_diary(&ctx, &mut app);
    let field = egui::Id::new(SEARCH_FIELD_ID);

    run(&ctx, &mut app, frame(0.5).command_f());
    assert_eq!(
        ctx.memory(|memory| memory.focused()),
        Some(field),
        "Ctrl+F 该把焦点交给搜索框"
    );

    run(&ctx, &mut app, frame(0.6).text("咖啡"));
    let found = texts(&run_output(&ctx, &mut app, frame(0.9)));
    assert!(found.contains("找到 1 天"), "{found}");
}

/// 防抖没到点不搜；到点搜一次，卡片上出现日期戳与摘要。
#[test]
fn typing_in_the_search_box_waits_for_the_debounce_then_lists_hits() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, spy) = fake_app();
    let day = today();
    spy.borrow_mut()
        .entries
        .insert(day, "今天喝了咖啡".to_owned());
    open_the_diary(&ctx, &mut app);
    let field = egui::Id::new(SEARCH_FIELD_ID);

    // 打字：防抖期间不碰库，卡片说"搜索中…"
    run(&ctx, &mut app, frame(0.5).focused_on(field).text("咖啡"));
    assert!(spy.borrow().searches.is_empty(), "防抖没到点就不该搜");
    // 卡片第一帧只做尺寸测量（egui 的 Area 如此），下一帧才真的画出来
    let pending = texts(&run_output(&ctx, &mut app, frame(0.55).focused_on(field)));
    assert!(pending.contains("搜索中…"), "{pending}");

    // 安静 0.25 秒之后再搜：卡片给这一天，带日期戳与摘要
    let found = texts(&run_output(&ctx, &mut app, frame(0.8).focused_on(field)));
    assert_eq!(spy.borrow().searches, vec!["咖啡".to_owned()]);
    assert!(found.contains("找到 1 天"), "{found}");
    assert!(
        found.contains(&day.format("%Y.%m.%d").to_string()),
        "卡片上要有日期戳：{found}"
    );
    assert!(found.contains("今天喝了咖啡"), "卡片上要有摘要：{found}");
}

/// 点卡片上的命中：正文跳到那天、命中那一段被选上、焦点交给那一行。
#[test]
fn clicking_a_hit_selects_it_in_the_diary() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, spy) = fake_app();
    let day = today();
    spy.borrow_mut()
        .entries
        .insert(day, "今天喝了咖啡".to_owned());
    open_the_diary(&ctx, &mut app);
    let field = egui::Id::new(SEARCH_FIELD_ID);

    run(&ctx, &mut app, frame(0.5).focused_on(field).text("咖啡"));
    let card = run_output(&ctx, &mut app, frame(0.8).focused_on(field));
    let pos = last_text_pos(&card, "今天喝了咖啡").expect("卡片上该有摘要");

    // 点这一行：跳转在这一帧末尾发生，选区下一帧才种进输入框
    run(&ctx, &mut app, frame(0.9).click(pos + egui::vec2(4.0, 4.0)));
    run(&ctx, &mut app, frame(1.0));

    let row = diary::row_id(day);
    let range = TextEditState::load(&ctx, row)
        .expect("那一行该有输入状态")
        .cursor
        .char_range()
        .expect("命中那一段该被选上");
    assert_eq!(
        (range.secondary.index.0, range.primary.index.0),
        (4, 6),
        "选中 `咖啡` 这两个字"
    );
    assert_eq!(
        ctx.memory(|memory| memory.focused()),
        Some(row),
        "焦点该交给那一行"
    );
}

/// 查询像日期：卡片第一行是"跳到这一天"；回车直接跳过去（那天没写过也行）。
#[test]
fn a_date_query_offers_a_jump_row_and_enter_goes_there() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, _spy) = fake_app();
    open_the_diary(&ctx, &mut app);
    let field = egui::Id::new(SEARCH_FIELD_ID);

    run(
        &ctx,
        &mut app,
        frame(0.5).focused_on(field).text("2025-09-17"),
    );
    let card = texts(&run_output(&ctx, &mut app, frame(0.8).focused_on(field)));
    assert!(
        card.contains("跳到") && card.contains("2025.09.17"),
        "日期查询该给一行跳转：{card}"
    );
    assert!(
        !card.contains("没有找到"),
        "有跳转行就不说「没有找到」：{card}"
    );

    // 回车：跳到那天（那天没写过也行）
    run(&ctx, &mut app, frame(0.9).focused_on(field).enter());
    let after = texts(&run_output(&ctx, &mut app, frame(1.0)));
    assert!(after.contains("2025.09.17"), "顶栏该跳到那天：{after}");
    assert!(!after.contains("跳到"), "跳过去之后卡片该收起：{after}");
}

/// Esc 清空查询并收起卡片。
#[test]
fn escape_clears_the_search() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, spy) = fake_app();
    spy.borrow_mut()
        .entries
        .insert(today(), "今天喝了咖啡".to_owned());
    open_the_diary(&ctx, &mut app);
    let field = egui::Id::new(SEARCH_FIELD_ID);

    run(&ctx, &mut app, frame(0.5).focused_on(field).text("咖啡"));
    let found = texts(&run_output(&ctx, &mut app, frame(0.8).focused_on(field)));
    assert!(found.contains("找到 1 天"), "{found}");

    let cleared = texts(&run_output(
        &ctx,
        &mut app,
        frame(0.9).focused_on(field).escape(),
    ));
    assert!(
        cleared.contains("搜索正文或日期"),
        "查询该被清空：{cleared}"
    );
    assert!(!cleared.contains("找到"), "卡片该收起：{cleared}");
}

/// 锁定之后查询、命中与输入框缓冲都不留；解锁回来是空的。
#[test]
fn locking_clears_the_search() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, spy) = fake_app();
    spy.borrow_mut()
        .entries
        .insert(today(), "今天喝了咖啡".to_owned());
    open_the_diary(&ctx, &mut app);
    let field = egui::Id::new(SEARCH_FIELD_ID);

    run(&ctx, &mut app, frame(0.5).focused_on(field).text("咖啡"));
    let found = texts(&run_output(&ctx, &mut app, frame(0.8).focused_on(field)));
    assert!(found.contains("找到 1 天"), "{found}");

    app.lock(&ctx);
    assert_eq!(spy.borrow().locks, 1, "落盘之后才丢密钥");
    assert!(
        TextEditState::load(&ctx, field).is_none(),
        "锁定之后输入框的撤销缓冲（含一份查询）也要丢"
    );

    // 解锁回来：搜索框是空的，命中与卡片都不在了
    let password = egui::Id::new(unlock::PASSWORD_FIELD_ID);
    run(
        &ctx,
        &mut app,
        frame(1.0).focused_on(password).text(PASSWORD),
    );
    run(&ctx, &mut app, frame(1.1).enter());
    let printed = texts(&run_output(&ctx, &mut app, frame(1.2)));
    assert!(
        printed.contains("搜索正文或日期"),
        "搜索框该是空的：{printed}"
    );
    assert!(!printed.contains("找到"), "命中不该还在：{printed}");
}

/// 搜索失败：红条说出来、卡片说"搜索失败"；下次改动重来一次。
#[test]
fn a_failed_search_says_so_and_recovers_on_the_next_edit() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, spy) = fake_app();
    spy.borrow_mut()
        .entries
        .insert(today(), "今天喝了咖啡啊".to_owned());
    open_the_diary(&ctx, &mut app);
    let field = egui::Id::new(SEARCH_FIELD_ID);

    spy.borrow_mut().fail_searches = 1;
    run(&ctx, &mut app, frame(0.5).focused_on(field).text("咖啡"));
    let failed = texts(&run_output(&ctx, &mut app, frame(0.8).focused_on(field)));
    assert!(failed.contains("替身搜不了"), "红条要说出来：{failed}");
    assert!(failed.contains("搜索失败"), "卡片要说搜索失败：{failed}");
    assert!(
        !failed.contains("没有找到"),
        "失败了就不该冒充「没有找到」：{failed}"
    );

    // 改一个字就重来一次：这次真搜到了
    let retried = texts(&run_output(
        &ctx,
        &mut app,
        frame(0.9).focused_on(field).text("啊"),
    ));
    assert!(retried.contains("搜索中…"), "{retried}");
    let found = texts(&run_output(&ctx, &mut app, frame(1.2).focused_on(field)));
    assert_eq!(
        spy.borrow().searches,
        vec!["咖啡".to_owned(), "咖啡啊".to_owned()]
    );
    assert!(found.contains("找到 1 天"), "{found}");
}

/// 搜索之前先落盘：搜索看到的是刚敲进去的那句（没等保存防抖）。
#[test]
fn a_search_flushes_pending_edits_first() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, spy) = fake_app();
    let day = today();
    open_the_diary(&ctx, &mut app);
    let field = egui::Id::new(SEARCH_FIELD_ID);
    let row = diary::row_id(day);

    // 先敲一个还没到点的查询，再把焦点交给正文、敲一句还没落盘的话
    run(&ctx, &mut app, frame(0.5).focused_on(field).text("咖啡"));
    run(
        &ctx,
        &mut app,
        frame(0.6).focused_on(row).text("今天喝了咖啡"),
    );

    // 查询到点：搜索之前必须先把这一句写进库，否则搜不到刚敲的
    let found = texts(&run_output(&ctx, &mut app, frame(0.8)));
    assert_eq!(
        spy.borrow().log,
        vec![
            Act::Apply(vec![Change::set(day, "今天喝了咖啡")]),
            Act::Search("咖啡".to_owned()),
        ],
        "落盘要排在搜索前面"
    );
    assert!(found.contains("找到 1 天"), "搜索该看见刚敲的那句：{found}");
}

/// 收起卡片之后键盘当前行归零：再展开时 `Enter` 该打开第一行。
///
/// 场景：搜出两条命中 → 点外面收起 → 重新聚焦（不带方向键）→ 回车。
/// 收起时若把 `selected` 留着，重新展开后回车打开的就是"收起前停的那一行"。
#[test]
fn reopening_the_card_starts_from_the_first_row() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, spy) = fake_app();
    let day = today();
    {
        let mut spy = spy.borrow_mut();
        spy.entries.insert(day, "今天喝了咖啡".to_owned());
        spy.entries
            .insert(day - chrono::Duration::days(1), "昨天也喝了咖啡".to_owned());
    }
    open_the_diary(&ctx, &mut app);
    let field = egui::Id::new(SEARCH_FIELD_ID);

    // 搜出两天的命中
    run(&ctx, &mut app, frame(0.5).focused_on(field).text("咖啡"));
    let found = texts(&run_output(&ctx, &mut app, frame(0.8).focused_on(field)));
    assert!(found.contains("找到 2 天"), "{found}");

    // 点正文（卡片外面）收起
    let outside = egui::pos2(900.0, 400.0);
    run(&ctx, &mut app, frame(0.9).click(outside));
    let closed = texts(&run_output(&ctx, &mut app, frame(0.95)));
    assert!(!closed.contains("找到 2 天"), "点外面该收起卡片：{closed}");

    // 重新聚焦（`Ctrl+F`），不碰方向键：卡片展开，当前行该是第一行
    run(&ctx, &mut app, frame(1.0).command_f());
    let reopened = texts(&run_output(&ctx, &mut app, frame(1.05).focused_on(field)));
    assert!(reopened.contains("找到 2 天"), "重新聚焦该展开：{reopened}");

    // 回车打开第一行：命中按日期从新到旧，第一条是今天
    run(&ctx, &mut app, frame(1.1).focused_on(field).enter());
    run(&ctx, &mut app, frame(1.15));

    let row = diary::row_id(day);
    assert_eq!(
        ctx.memory(|memory| memory.focused()),
        Some(row),
        "回车该落到第一条命中（今天），而不是收起前停的第二条"
    );
    // `今天喝了咖啡` 里 `咖啡` 落在 4..6
    let range = TextEditState::load(&ctx, row)
        .expect("回第一行该跳到今天那一行")
        .cursor
        .char_range()
        .expect("命中那一段该被选上");
    assert_eq!(
        (range.secondary.index.0, range.primary.index.0),
        (4, 6),
        "回车该打开第一行（今天）"
    );
}

/// 删掉最后一个字：查询空了，命中与卡片一起清，且不留下一次会再扫库的计时。
///
/// 这一帧 `changed` 与 `dismissed` 同时为真（`SearchView::field` 里的 `emptied`）。
/// 清掉优先：查询、命中、防抖计时一起归零，再聚焦不会拿旧结果冒充。
#[test]
fn emptying_the_query_drops_the_hits_and_the_timer() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, spy) = fake_app();
    spy.borrow_mut()
        .entries
        .insert(today(), "今天喝了咖啡".to_owned());
    open_the_diary(&ctx, &mut app);
    let field = egui::Id::new(SEARCH_FIELD_ID);

    run(&ctx, &mut app, frame(0.5).focused_on(field).text("咖"));
    let found = texts(&run_output(&ctx, &mut app, frame(0.8).focused_on(field)));
    assert!(found.contains("找到 1 天"), "{found}");
    assert_eq!(spy.borrow().searches.len(), 1, "该搜过一次");

    // 退格删掉最后一个字：查询空了
    run(&ctx, &mut app, frame(0.9).focused_on(field).backspace());
    let emptied = texts(&run_output(&ctx, &mut app, frame(1.3).focused_on(field)));
    assert!(
        emptied.contains("搜索正文或日期"),
        "查询该空了（只剩提示语）：{emptied}"
    );
    assert!(!emptied.contains("找到"), "命中的卡片该收起：{emptied}");
    assert_eq!(
        spy.borrow().searches.len(),
        1,
        "空查询不该再去扫库（防抖计时也该归零）"
    );
}

/// 落盘失败还压在队里时又来一次搜索失败：红条该留着写盘那条，不被顶掉。
///
/// 落盘的错要用户点"重试"（编辑还在队里），搜索的错只是这一次查询没成。
/// 写盘那条更急：被顶掉之后用户会以为问题只是查询没成。
///
/// 时序：查询的防抖（0.25s）比落盘（0.4s）短，所以要让正文先写失败一次、
/// 再让查询到点——那时 `run_search` 里的 `flush` 会再失败一次，搜索也失败。
#[test]
fn a_search_failure_does_not_hide_a_save_failure() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, spy) = fake_app();
    open_the_diary(&ctx, &mut app);
    let field = egui::Id::new(SEARCH_FIELD_ID);
    let row = diary::row_id(today());

    // 正文那句一写就失败，之后一直失败（不让重试成功）；
    // 搜索也失败一次
    {
        let mut spy = spy.borrow_mut();
        spy.fail_next = 99;
        spy.fail_searches = 1;
    }

    // 先敲正文：过 0.4 秒防抖写一次，失败，编辑压回队里
    run(
        &ctx,
        &mut app,
        frame(0.4).focused_on(row).text("今天喝了咖啡"),
    );
    let stuck = texts(&run_output(&ctx, &mut app, frame(0.9).focused_on(row)));
    assert!(stuck.contains("替身装的写盘失败"), "写盘该先失败：{stuck}");
    assert!(stuck.contains("重试"), "写盘失败时错误条该给重试：{stuck}");

    // 再敲查询并等它到点：这一次 `run_search` 里 flush 还是失败，搜索也失败
    run(&ctx, &mut app, frame(1.0).focused_on(field).text("咖啡"));
    let failed = texts(&run_output(&ctx, &mut app, frame(1.5).focused_on(field)));
    assert!(
        failed.contains("替身装的写盘失败"),
        "红条该继续说写盘失败（编辑还没写进去）：{failed}"
    );
    assert!(
        !failed.contains("替身搜不了"),
        "搜索的错不该把写盘的错顶掉：{failed}"
    );
}

// ────────────────────────── 窄屏 / Android 外壳 ──────────────────────────
//
// 手机上跑的是同一套控制器与视图，只有外壳不同：侧栏收进浮层、底栏换成三枚按钮、
// 返回键逐层退。这三条都在这里用合成事件盯着（手机没有真机也能跑）。

/// 手机竖屏的屏幕尺寸（点）
const NARROW: (f32, f32) = (400.0, 800.0);

/// 窄屏的一帧
fn narrow_frame(time: f64) -> Frame {
    frame(time).screen(NARROW.0, NARROW.1)
}

/// 替身存储：验 `App::save`（手机上切后台时的落盘钩子）只走控制器那一侧，
/// egui 的持久化文件里不该出现正文，所以这里也数“写了几次”。
#[derive(Default)]
struct DummyStorage {
    writes: usize,
}

impl eframe::Storage for DummyStorage {
    fn get_string(&self, _key: &str) -> Option<String> {
        None
    }

    fn set_string(&mut self, _key: &str, _value: String) {
        self.writes += 1;
    }

    fn remove_string(&mut self, _key: &str) {}

    fn flush(&mut self) {
        self.writes += 1;
    }
}

/// 在 400×800 的屏上解锁进主界面
fn open_the_narrow_diary(ctx: &egui::Context, app: &mut App) {
    open_the_unlock_screen(ctx, app);
    run(
        ctx,
        app,
        frame(0.2)
            .focused_on(egui::Id::new(unlock::PASSWORD_FIELD_ID))
            .text(PASSWORD),
    );
    run(ctx, app, frame(0.3).enter());
    run(ctx, app, narrow_frame(0.4));
}

/// 窄屏：侧栏收进浮层，底栏换成三枚按钮；点浮层里的一天，浮层收起、正文切过去
#[test]
fn a_narrow_window_moves_the_sidebar_into_an_overlay() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, _spy) = fake_app();
    let day = today();
    open_the_narrow_diary(&ctx, &mut app);

    // 第一帧：宽屏那一列（搜索框 + 日历 + 页脚）不在画面上，底栏那三枚在
    let first = run_output(&ctx, &mut app, narrow_frame(0.5));
    let shown = texts(&first);
    assert!(!shown.contains("本月记录"), "窄屏不该摆侧栏：{shown}");
    assert!(
        last_text_pos(&first, "日历").is_some(),
        "底栏该有「日历」：{shown}"
    );
    assert!(
        last_text_pos(&first, "搜索").is_some(),
        "底栏该有「搜索」：{shown}"
    );
    assert!(shown.contains("本月已写"), "底栏该有本月统计：{shown}");

    // 点「日历」：浮层开出来，搜索框与日历都在里面。
    // 两次延迟：浮层是这一帧末尾才开的（底栏的意图排在画完之后统一处理），
    // 而 `egui::Modal` 的第一帧还没量到自己的尺寸、什么都画不出来
    let at = last_text_pos(&first, "日历").unwrap();
    run_output(&ctx, &mut app, narrow_frame(0.6).click(at));
    run_output(&ctx, &mut app, narrow_frame(0.65));
    let opened = run_output(&ctx, &mut app, narrow_frame(0.7));
    let shown = texts(&opened);
    assert!(shown.contains("本月记录"), "点「日历」该开出浮层：{shown}");
    assert!(
        shown.contains("搜索正文或日期…"),
        "浮层里也有搜索框：{shown}"
    );

    // 挑一个落在本月正中、又跟今天不同的日子（前后补位的格子只可能落在月初/月末）；
    // 点完下一帧浮层收掉，那一帧里出现的这个日号只可能来自新选中的那天
    let target = if day.day() == 17 { 16 } else { 17 };
    let cell = last_text_pos(&opened, &target.to_string()).expect("日历上该有这一天");
    run_output(&ctx, &mut app, narrow_frame(0.75).click(cell));
    let picked = run_output(&ctx, &mut app, narrow_frame(0.8));
    let shown = texts(&picked);
    assert!(!shown.contains("本月记录"), "选完日子该收起浮层：{shown}");
    assert!(
        last_text_pos(&picked, &target.to_string()).is_some(),
        "正文该切到 {target} 号：{shown}"
    );
}

/// 空页提示得指对地方：窄屏的日历收在底栏「日历」那枚按钮开的浮层里，
/// 宽屏的日历在正文左边一整列。面板自己不看宽度（见 `docs/architecture.md`），
/// 文案由外壳给的 `Chrome` 带进来。
#[test]
fn the_empty_month_hint_points_at_wherever_the_calendar_is() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, _spy) = fake_app();
    open_the_narrow_diary(&ctx, &mut app);

    // 翻到下个月：库里那个月没有记录，正文这一页就空了。
    // 浮层是点开之后第二帧才画得出来的（`egui::Modal` 要量一次尺寸）
    let calendar = last_text_pos(&run_output(&ctx, &mut app, narrow_frame(0.5)), "日历")
        .expect("底栏该有「日历」");
    run_output(&ctx, &mut app, narrow_frame(0.55).click(calendar));
    run_output(&ctx, &mut app, narrow_frame(0.6));
    let opened = run_output(&ctx, &mut app, narrow_frame(0.65));
    let next = last_text_pos(&opened, "›").expect("浮层里该有翻到下个月那枚按钮");
    run_output(&ctx, &mut app, narrow_frame(0.7).click(next));

    let narrow = texts(&run_output(&ctx, &mut app, narrow_frame(0.75)));
    assert!(narrow.contains("还没有记录"), "翻到空月该给提示：{narrow}");
    assert!(
        narrow.contains("点底栏「日历」挑一天"),
        "窄屏的日历在浮层里：{narrow}"
    );
    assert!(!narrow.contains("左边日历"), "窄屏没有「左边」：{narrow}");

    // 同一份控制器、同一屏，只换外壳：提示指回左边那一列
    // （egui 在 `screen_rect` 为 `None` 时沿用上一帧的矩形，所以这里得明写宽屏尺寸）
    let wide = texts(&run_output(
        &ctx,
        &mut app,
        frame(0.8).screen(1200.0, 800.0),
    ));
    assert!(
        wide.contains("点左边日历上的任何一天"),
        "宽屏的日历在正文左边：{wide}"
    );
    assert!(!wide.contains("点底栏"), "宽屏没有底栏浮层：{wide}");
}

/// 返回键：浮层 → 搜索 → 退出，一次一层；退到底才交给退出钩子
#[test]
fn the_back_key_unwinds_one_layer_at_a_time() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, spy) = fake_app();
    spy.borrow_mut()
        .entries
        .insert(today(), "今天喝了咖啡".to_owned());
    open_the_narrow_diary(&ctx, &mut app);

    // 浮层开着的时候，返回键只收它：不碰搜索，也不关窗
    let calendar = last_text_pos(&run_output(&ctx, &mut app, narrow_frame(0.5)), "日历")
        .expect("底栏该有「日历」");
    run_output(&ctx, &mut app, narrow_frame(0.55).click(calendar));
    run_output(&ctx, &mut app, narrow_frame(0.6));
    let opened = run_output(&ctx, &mut app, narrow_frame(0.65));
    assert!(texts(&opened).contains("本月记录"), "点「日历」该开出浮层");

    let output = run_output(&ctx, &mut app, narrow_frame(0.7).back());
    assert!(
        !texts(&output).contains("本月记录"),
        "返回键该收起浮层：{}",
        texts(&output)
    );
    assert!(commands(&output).is_empty(), "只收浮层，不该关窗");

    // 开浮层 → 打字 → 防抖到点：命中列出来
    // （点「搜索」是下一帧才开浮层的，所以后面看的是 0.85 那一帧）
    let search = last_text_pos(&run_output(&ctx, &mut app, narrow_frame(0.75)), "搜索")
        .expect("底栏该有「搜索」");
    run_output(&ctx, &mut app, narrow_frame(0.8).click(search));
    let field = egui::Id::new(SEARCH_FIELD_ID);
    run_output(
        &ctx,
        &mut app,
        narrow_frame(0.85).focused_on(field).text("咖啡"),
    );
    let shown = texts(&run_output(
        &ctx,
        &mut app,
        narrow_frame(1.2).focused_on(field),
    ));
    assert!(shown.contains("找到 1 天"), "防抖之后该列出命中：{shown}");

    // 第一下：收浮层（查询与命中都留着）
    let output = run_output(&ctx, &mut app, narrow_frame(1.3).back());
    assert!(
        !texts(&output).contains("找到 1 天"),
        "返回键该收起浮层：{}",
        texts(&output)
    );
    assert!(commands(&output).is_empty(), "还不该关窗");

    // 第二下：轮到搜索那一层
    let output = run_output(&ctx, &mut app, narrow_frame(1.4).back());
    assert!(commands(&output).is_empty(), "清搜索不该关窗");

    // 再看一眼：框里空了（提示语回来），命中也没了（浮层第二帧才画得出内容）
    let search = last_text_pos(&run_output(&ctx, &mut app, narrow_frame(1.45)), "搜索")
        .expect("底栏该有「搜索」");
    run_output(&ctx, &mut app, narrow_frame(1.5).click(search));
    run_output(&ctx, &mut app, narrow_frame(1.55));
    let shown = texts(&run_output(&ctx, &mut app, narrow_frame(1.6)));
    assert!(
        shown.contains("搜索正文或日期…"),
        "清掉搜索之后框里该是空的：{shown}"
    );
    assert!(!shown.contains("找到 1 天"), "命中也不该留着：{shown}");

    // 收掉这个浮层，再按一次：这时搜索已经空了，返回键轮到「关窗」
    // （关窗就是桌面点关闭按钮那条路：先落盘、写不成就不关，由 `handle_close_request` 管）
    let output = run_output(&ctx, &mut app, narrow_frame(1.65).back());
    assert!(commands(&output).is_empty(), "收浮层不该关窗");
    let output = run_output(&ctx, &mut app, narrow_frame(1.7).back());
    assert!(
        commands(&output).contains(&egui::ViewportCommand::Close),
        "退到底该请求关窗：{:?}",
        commands(&output)
    );
}

/// 手机切后台（eframe 的 `EventResult::Save`）走的是 `App::save`：把攒着的写完，
/// 而 egui 的内存（含输入框撤销缓冲里的一份正文）不进持久化文件
#[test]
fn going_to_the_background_writes_the_pending_edits() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, spy) = fake_app();
    let day = today();
    open_the_diary(&ctx, &mut app);

    let row = diary::row_id(day);
    run(&ctx, &mut app, frame(0.5).focused_on(row));
    run(
        &ctx,
        &mut app,
        frame(0.6).focused_on(row).text("切后台也不能丢"),
    );
    assert!(spy.borrow().applied.is_empty(), "防抖期间不该写盘");

    // 这就是 eframe 在 `Event::Suspended` 里做的事
    let mut storage = DummyStorage::default();
    eframe::App::save(&mut app, &mut storage);
    assert_eq!(
        spy.borrow().applied,
        vec![vec![Change::set(day, "切后台也不能丢")]],
        "切后台必须把攒着的写完"
    );
    assert_eq!(storage.writes, 0, "正文不该经过 egui 的持久化文件");

    // egui 手里还有一份正文，它绝不能进 `app.ron`
    assert!(!eframe::App::persist_egui_memory(&app));
}

/// 输入法上屏的字要变成这一帧的文本事件，落进当前那一行并落盘。
///
/// 这一条盯的是手写不出来的那段接线：软键盘的字在手机上根本不走键盘事件，
/// 全靠“快照差分（[`ToDay::ime`]）→ [`ToDay::app::App::apply_ime_snapshot`]
/// → `Event::Text`”这一段。快照的来源（`AndroidApp`）是 Android 独有，
/// 这里直接喂快照正文；Android 那三行读法只能装机验（见 `docs/android.md`）。
#[test]
fn ime_text_lands_in_the_focused_row() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, spy) = fake_app();
    let day = today();
    open_the_diary(&ctx, &mut app);
    let row = diary::row_id(day);

    // 焦点先落在今天那一行（真实运行时是用户点一下）
    run(&ctx, &mut app, frame(0.5).focused_on(row));

    // eframe 的一帧：先把快照差分进这一帧要注入的事件，再画
    let mut input = egui::RawInput {
        time: Some(0.6),
        focused: true,
        ..Default::default()
    };
    app.apply_ime_snapshot(&mut input, "中文", None);
    assert!(
        input
            .events
            .iter()
            .any(|event| matches!(event, Event::Text(text) if text == "中文")),
        "快照里新上屏的字该变成这一帧的文本事件：{:?}",
        input.events
    );

    let mut output = ctx.run_ui(input, |ui| {
        ui.memory_mut(|memory| memory.request_focus(row));
        app.render(ui);
    });
    output.textures_delta.clear();

    // 防抖到点：这一句应该已经落进“库”里
    run(&ctx, &mut app, frame(1.1).focused_on(row));
    assert_eq!(
        spy.borrow().entries.get(&day).map(String::as_str),
        Some("中文"),
        "输入法上屏的字该写进库"
    );

    // 输入法的退格也要走得通：快照短了一个字
    let mut input = egui::RawInput {
        time: Some(1.2),
        focused: true,
        ..Default::default()
    };
    app.apply_ime_snapshot(&mut input, "中", None);
    assert!(
        input.events.iter().any(|event| matches!(
            event,
            Event::Key {
                key: Key::Backspace,
                pressed: true,
                ..
            }
        )),
        "快照短了的字该变成退格事件：{:?}",
        input.events
    );
    let mut output = ctx.run_ui(input, |ui| {
        ui.memory_mut(|memory| memory.request_focus(row));
        app.render(ui);
    });
    output.textures_delta.clear();

    run(&ctx, &mut app, frame(1.7).focused_on(row));
    assert_eq!(
        spy.borrow().entries.get(&day).map(String::as_str),
        Some("中"),
        "退格该删掉一个字"
    );
}

/// 差分有没有产出动作：排帧不看它（判据是焦点），但这一层唯一的可观测产物就是它——
/// “快照真的推了字”与“同一份快照又喂了一遍”靠它分开。
#[test]
fn ime_injections_report_whether_something_was_committed() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, _spy) = fake_app();
    let mut input = egui::RawInput::default();

    assert!(
        app.apply_ime_snapshot(&mut input, "中文", None),
        "新上屏的字算一次动作"
    );
    assert!(
        !app.apply_ime_snapshot(&mut input, "中文", None),
        "同一份快照重复喂不产出动作"
    );
    assert!(
        !app.apply_ime_snapshot(&mut input, "中文nihao", Some((6, 11))),
        "只有组词区在变（拼音打在候选里）不算上屏"
    );
    assert!(
        app.apply_ime_snapshot(&mut input, "中", None),
        "输入法退格算一次动作"
    );
}

/// 装上排帧回调，返回它看到的那些延迟。
///
/// `eframe` 真实运行时用的就是它（回调里按 `delay` 排一次 `UserEvent::RequestRepaint`），
/// 所以"排帧请求到底发出去没有"要在这里看：`raw_input_hook` 在 pass 之前跑，
/// `viewport_output.repaint_delay` 只装得下 pass 里的那些请求。
fn repaint_requests(ctx: &egui::Context) -> Arc<Mutex<Vec<Duration>>> {
    let seen: Arc<Mutex<Vec<Duration>>> = Arc::default();
    let sink = Arc::clone(&seen);
    ctx.set_request_repaint_callback(move |info| sink.lock().unwrap().push(info.delay));
    seen
}

/// 打字时应用要自己排帧：上屏的字不给应用发事件，不排帧就一直不上屏。
///
/// 判据只有焦点一个：有输入框在焦点上就**立刻**再画一帧（为什么不是"16ms 一帧"，见
/// `ToDay::ime::next_interval` 的注释——真机上非零延迟会被帧内的排帧请求吃掉）。
#[test]
fn ime_typing_requests_another_frame_right_away() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, _spy) = fake_app();
    open_the_diary(&ctx, &mut app);
    let row = diary::row_id(today());

    // 焦点先落在今天那一行（真实运行时是用户点一下）
    run_ime(&ctx, &mut app, frame(0.5).focused_on(row), None);
    assert!(
        ctx.text_edit_focused(),
        "夹具的前提：这一行真的拿着输入焦点"
    );

    let asked = repaint_requests(&ctx);
    asked.lock().unwrap().clear();

    // 一次新上屏：这一帧就把下一帧排到"立刻"。`repaint_delay` 是 egui 给这一帧的
    // "下一帧最早什么时候"，它已经把这一帧里所有的排帧请求（含上面那次）算进去了
    let (out, paced) = run_ime(
        &ctx,
        &mut app,
        frame(0.6).focused_on(row),
        Some(("中", None)),
    );
    assert_eq!(
        paced,
        Some(Duration::ZERO),
        "有输入框在焦点上：立刻排下一帧"
    );
    assert_eq!(
        out.viewport_output[&egui::ViewportId::ROOT].repaint_delay,
        Duration::ZERO,
        "这一帧的下一帧要排到立刻"
    );

    // 停手之后照样：上屏的字随时可能到，非零延迟在真机上不可靠，所以不看"多久没输入"
    let mut asked_last = Vec::new();
    for step in 0..5 {
        asked.lock().unwrap().clear();
        let (_out, paced) = run_ime(
            &ctx,
            &mut app,
            frame(1.0 + step as f64 * 0.5).focused_on(row),
            Some(("中", None)),
        );
        assert_eq!(paced, Some(Duration::ZERO), "有焦点就一直排帧");
        asked_last = asked.lock().unwrap().clone();
    }
    assert!(
        asked_last.iter().all(|delay| *delay == Duration::ZERO),
        "排出去的也只能是立刻：{asked_last:?}"
    );
}

/// 没有输入框在焦点上时不排帧：空闲一帧都不多画（与接手前一致）。
///
/// 快照照样每帧喂（Android 那条路也是）：基准 `ime_committed` 要保持新鲜，否则重新聚焦时
/// 会把整段旧正文重新倒进输入框——不排帧 ≠ 不读快照。
#[test]
fn without_a_focused_editor_no_repaint_is_scheduled() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (mut app, _spy) = fake_app();
    open_the_diary(&ctx, &mut app);
    assert!(!ctx.text_edit_focused(), "夹具的前提：没有输入框拿到焦点");

    let asked = repaint_requests(&ctx);
    asked.lock().unwrap().clear();
    for step in 0..3 {
        // 快照喂空：既不产出注入，也没人接注入（没焦点）——纯空闲
        let (_out, paced) = run_ime(
            &ctx,
            &mut app,
            frame(0.5 + step as f64 * 0.1),
            Some(("", None)),
        );
        assert_eq!(paced, None, "没有输入框在焦点上：这一帧不排帧");
    }
    assert!(
        asked
            .lock()
            .unwrap()
            .iter()
            .all(|delay| *delay > Duration::from_millis(50)),
        "没焦点时不该出现立刻排帧的请求：{:?}",
        asked.lock().unwrap()
    );
}
