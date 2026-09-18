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
//! * **退出**：没有可关的窗口，返回键的"退出"由 [`crate::app::App::with_quit_hook`] 收到这里执行。
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
    let app = match boot::assemble(&path) {
        Ok(app) => app.with_quit_hook(|| {
            log::info!("返回键退出：待写的编辑已经落盘");
            // TODO: 若真机上最近任务里残留卡片，改成 JNI 调 `Activity::finish()`。
            // 之所以不是 finish()：winit 的 Android 后端忽略 `MainEvent::Destroy`
            // （源码里是 TODO，见 docs/android.md），事件循环不会因此结束，
            // 而本进程只有这一个 Activity，进程随键一起走最干净。
            std::process::exit(0);
        }),
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
