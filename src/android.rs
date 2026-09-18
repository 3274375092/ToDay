//! Android 入口。
//!
//! `android_main` 由 `android-activity` 的 springboard 在一个独立线程上调用（`GameActivity`
//! 的 `onCreate` 之后），它做四件事：接上 logcat、定下数据目录、装配控制器、跑 eframe 的事件循环。
//! 界面与落盘的所有决策都在 [`crate::app::App`] 里，这里一条业务规则都不写。
//!
//! 与桌面的三处不同，都在这里吸收掉：
//!
//! * **数据目录**：`directories` 没有 Android 实现，路径从 `AndroidApp::internal_data_path()` 拿，
//!   交给 [`crate::paths::set_data_dir`]（`/data/user/0/<包名>/files`，app 私有，卸载即删）。
//! * **日志**：Android 上 `eprintln!` 进不了 logcat，日志走 `log` + `android_logger`。
//! * **退出**：手机上关不掉窗口，返回键退到底时 `App` 发 `ViewportCommand::Close`；
//!   `run_native` 返回之后本函数就返回，`android-activity` 的 springboard 会接下去调
//!   `GameActivity_finish()` 把 Activity 正常结束——**不要自己 `std::process::exit`**（见下方注释）。
//!
//! 不碰窗口尺寸记忆（[`crate::window_state`]）：手机上只有一个全屏窗口，`window.txt` 没有意义。

use crate::boot;
use crate::paths;
use crate::ui::theme;
use eframe::egui;
use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetupCreateNew, wgpu};
use winit::platform::android::activity::AndroidApp;

/// 日志里带的标签，`adb logcat -s ToDay` 按它过滤
const LOG_TAG: &str = "ToDay";

/// eframe 的持久化文件。里面只可能有窗口设置，正文一个字都不会进去（见 `persistence_path`）。
const STORAGE_FILE: &str = "app.ron";

/// Android 的入口，签名与符号名由 `android-activity` 规定（`GameActivity` 用 `dlsym`
/// 找这个符号），不能改。入参叫 `android_app` 是为了不与上面那个控制器 `app` 撞名。
///
/// 装配失败（拿不到私有目录、库打不开）时只写一条 logcat 然后返回：没有界面可报错，
/// 写了人也看不见；进程留给系统回收，用户重开一次就是重试。
#[unsafe(no_mangle)]
pub fn android_main(android_app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_tag(LOG_TAG)
            .with_max_level(log::LevelFilter::Info),
    );

    let Some(dir) = android_app.internal_data_path() else {
        log::error!("拿不到 app 私有目录，日记本没有可落盘的地方，不启动界面");
        return;
    };
    if let Err(err) = paths::set_data_dir(dir) {
        log::error!("数据目录定不下来：{err}");
        return;
    }

    let path = match paths::database_path() {
        Ok(path) => path,
        Err(err) => {
            log::error!("算不出数据库路径：{err}");
            return;
        }
    };
    let storage_path = match paths::data_dir() {
        Ok(dir) => dir.join(STORAGE_FILE),
        Err(err) => {
            log::error!("算不出持久化文件路径：{err}");
            return;
        }
    };
    // 返回键退到底之后走的就是桌面那条"关窗"路：App 发 `Close`，`handle_close_request`
    // 先把待写的写完（写不成就不关），事件循环退出，本函数返回，
    // 然后 `android-activity` 自己调 `GameActivity_finish()`。
    //
    // 真机上量过：在这里（或任何地方）用 `std::process::exit` 收尾会在错误线程上拆 JVM，
    // 直接 `JNI DETECTED ERROR` + `SIGABRT`。见 `docs/android.md` 的"返回键与退出"。
    let app = match boot::assemble(&path) {
        Ok(app) => app,
        Err(err) => {
            log::error!("打不开数据库 {}：{err}", path.display());
            return;
        }
    };

    // 适配器偏好与桌面同一套：日记本不该把独显叫醒（手机上这条只在多 GPU 的机器上有意义）
    let mut setup = WgpuSetupCreateNew::without_display_handle();
    setup.power_preference =
        wgpu::PowerPreference::from_env().unwrap_or(wgpu::PowerPreference::LowPower);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("ToDay"),
        // 与桌面同一条理由：`epaint` 自己按 1 px feathering 抗锯齿（见 docs/architecture.md）
        multisampling: 0,
        wgpu_options: WgpuConfiguration {
            wgpu_setup: setup.into(),
            ..Default::default()
        },
        android_app: Some(android_app),
        // 只为拿到切后台时的 `App::save` 钩子。文件里永远是空的：`App::persist_egui_memory`
        // 返回 false，egui 的内存（含输入框撤销缓冲里的正文）不会被写进去。
        persistence_path: Some(storage_path),
        ..Default::default()
    };

    if let Err(err) = eframe::run_native(
        "ToDay",
        options,
        Box::new(|cc| {
            theme::install(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    ) {
        log::error!("窗口起不来：{err}");
    }
}

/// 输入法上屏的文字入口，由 Java 的 `ToDayActivity.nativePushText` 调。
///
/// 符号名是 JNI 的规矩：`Java_<包名下划线化>_<类名>_<方法名>`；静态方法与实例方法
/// 第二个参数分别是 `JClass` / `JObject`——这里是静态的。改名要三处一起改
/// （Java 的 native 声明、这里的符号名、`docs/android.md`）。
///
/// 线程：Java 的 UI 线程（不是 `android_main` 那条），所以只做一次加锁入队；
/// 注入到 `egui` 是 [`crate::app::App::raw_input_hook`] 的事。
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_today_diary_ToDayActivity_nativePushText<'local>(
    mut env: jni::EnvUnowned<'local>,
    _class: jni::objects::JClass<'local>,
    text: jni::objects::JString<'local>,
) {
    // 取字符串失败只记一条日志（policy 会打），当这一手没推过——不能让输入法崩在这里
    let text: String = env
        .with_env(|env| -> jni::errors::Result<String> { text.try_to_string(env) })
        .resolve::<jni::errors::LogErrorAndDefault>();

    if !text.is_empty() {
        crate::app::push_text(text);
    }
}

/// 退格入口，由 Java 的 `ToDayActivity.nativePushBackspace` 调（输入法在光标左边删字）。
///
/// 与 [`Java_com_today_diary_ToDayActivity_nativePushText`] 同一条线程规矩：只入队。
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_today_diary_ToDayActivity_nativePushBackspace(
    _env: jni::EnvUnowned<'_>,
    _class: jni::objects::JClass<'_>,
    count: jni::sys::jint,
) {
    if count > 0 {
        crate::app::push_backspace(count as usize);
    }
}
