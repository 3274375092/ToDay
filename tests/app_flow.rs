//! 端到端：控制器 + 视图 + 加密存储真的接在一起时，一帧一帧跑下来是什么样。
//!
//! 没有显示器也能跑：用 `egui::Context::run_ui` 推进帧，用合成的输入事件
//! 当键盘（输密码、打字、回车），焦点按 id 直接投递。
//! 断言一律落在磁盘上——"界面上写了什么"最后必须能从库里读出来。

use ToDay::app::App;
use ToDay::model::MonthKey;
use ToDay::store::Vault;
use ToDay::store::sqlite::SqliteVault;
use ToDay::ui::{diary, theme, unlock};
use chrono::{Local, NaiveDate};
use egui::{Event, Key};
use rusqlite::Connection;
use std::path::Path;

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
}

fn frame(time: f64) -> Frame {
    Frame {
        time,
        ..Default::default()
    }
}

impl Frame {
    fn text(mut self, text: &str) -> Self {
        self.events.push(Event::Text(text.to_owned()));
        self
    }

    fn enter(mut self) -> Self {
        self.events.push(Event::Key {
            key: Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        });
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
}

/// 跑一帧。测试里没有渲染后端，纹理增量得自己收掉。
///
/// 返回这一帧里应用向窗口提的要求（比如"先别关"和"可以关了"）。
fn run(ctx: &egui::Context, app: &mut App, frame: Frame) -> Vec<egui::ViewportCommand> {
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
            ..Default::default()
        },
    );

    let input = egui::RawInput {
        time: Some(frame.time),
        events: frame.events,
        // 窗口是有焦点的：不然 `Response::has_focus` 恒为假，跟真实运行不一样
        focused: true,
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
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|viewport| viewport.commands.clone())
        .unwrap_or_default()
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
