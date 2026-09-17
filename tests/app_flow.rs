//! 端到端：控制器 + 视图 + 加密存储真的接在一起时，一帧一帧跑下来是什么样。
//!
//! 没有显示器也能跑：用 `egui::Context::run_ui` 推进帧，用合成的输入事件
//! 当键盘（输密码、打字、回车），焦点按 id 直接投递。
//! 断言一律落在磁盘上——"界面上写了什么"最后必须能从库里读出来。

use ToDay::app::App;
use ToDay::model::{Change, MonthKey, MonthSheet};
use ToDay::store::StoreError;
use ToDay::store::Vault;
use ToDay::store::sqlite::SqliteVault;
use ToDay::ui::{diary, theme, unlock};
use chrono::{Local, NaiveDate};
use egui::widgets::text_edit::TextEditState;
use egui::{Event, Key};
use rusqlite::Connection;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;
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
}

/// 跑一帧，把这一帧的完整输出交出来（纹理增量自己收掉）。
fn run_output(ctx: &egui::Context, app: &mut App, frame: Frame) -> egui::FullOutput {
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
            inner_rect: frame.window.map(|(rect, _)| rect),
            maximized: frame.window.map(|(_, maximized)| maximized),
            fullscreen: Some(frame.fullscreen),
            ..Default::default()
        },
    );

    let input = egui::RawInput {
        time: Some(frame.time),
        events: frame.events,
        focused: frame.focused,
        viewports,
        ..Default::default()
    };
    let mut output = ctx.run_ui(input, |ui| {
        if let Some(id) = frame.focus {
            // 键盘焦点由测试投递：没法用鼠标点，就直接告诉 egui 谁拿着焦点
            ui.memory_mut(|memory| memory.request_focus(id));
        }
        app.render(ui);
    });
    output.textures_delta.clear();

    output
}

/// 跑一帧，返回这一帧里应用向窗口提的要求（比如"先别关"和"可以关了"）。
fn run(ctx: &egui::Context, app: &mut App, frame: Frame) -> Vec<egui::ViewportCommand> {
    run_output(ctx, app, frame)
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|viewport| viewport.commands.clone())
        .unwrap_or_default()
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

/// 替身里能被测试读到的部分：`App` 拿走 `Box<dyn Vault>` 的所有权，
/// 断言只能落在共享的这份记录上。
#[derive(Default)]
struct Spy {
    /// 每次 `apply` 收到的批次，按顺序记下来
    applied: Vec<Vec<Change>>,
    /// 接下来几次 `apply` 要失败（每失败一次减一）
    fail_next: usize,
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
