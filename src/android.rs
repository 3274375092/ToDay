//! Android 入口。
//!
//! `android_main` 由 `android-activity` 的 springboard 在一个独立线程上调用（`GameActivity`
//! 的 `onCreate` 之后），它做四件事：接上 logcat、定下数据目录、装配控制器、跑 eframe 的事件循环。
//! 界面与落盘的所有决策都在 [`crate::app::App`] 里，这里一条业务规则都不写。
//!
//! 与桌面的四处不同，都在这里吸收掉：
//!
//! * **数据目录**：`directories` 没有 Android 实现，路径从 `AndroidApp::internal_data_path()` 拿，
//!   交给 [`crate::paths::set_data_dir`]（`/data/user/0/<包名>/files`，app 私有，卸载即删）。
//! * **日志**：Android 上 `eprintln!` 进不了 logcat，日志走 `log` + `android_logger`。
//! * **输入法**：软键盘上屏的字走 game-activity 的 `TextEvent`，winit 0.30.13 不消费它；
//!   这里把 `AndroidApp` 交给 [`crate::app::App::with_ime_source`]，由控制器每帧读一次
//!   快照（读法见 `ime_snapshot`，差分见 [`crate::ime`]）。
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
use jni::JavaVM;
use jni::objects::JObject;
use std::ffi::c_void;
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

    // 排在装配之前：`PhoneWindow` 会把这条设置记在名下、等 decor 装上一并应用，
    // 所以越早越好（`android_main` 本来就在 `GameActivity.onCreate` 里跑）
    pin_decor_fits(&android_app);

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
    // 输入法上屏的字从这个句柄的快照里读（winit 不消费 game-activity 的 `TextEvent`）。
    // clone 一份：下面的 `NativeOptions` 还要拿走原值，`AndroidApp` 是 `Arc` 包着的句柄。
    let app = app.with_ime_source(android_app.clone());

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

/// 请系统把内容区让开状态栏与手势条：`Window.setDecorFitsSystemWindows(true)`。
///
/// 为什么需要它：`targetSdk 35` 在 Android 15+ 被强制 edge-to-edge，而主题里那条
/// `windowOptOutEdgeToEdgeEnforcement`（`values-v35/themes.xml`）在这台真机上没有生效
/// ——窗口帧量到的是 `[0,0][1260,2800]`，58pt 的顶栏从屏幕 y=0 画，被 38pt 的状态栏盖住
/// （「锁定」与保存状态字都在那块区域里）。`winit` 0.30 不把 `MainEvent::InsetsChanged`
/// 转给应用（`safe_area()` 只有 iOS/macOS），应用拿不到安全区，所以只能请系统把内容区让开
/// ——与主题里那条 opt-out 同一个意思，只是走运行时 API。
///
/// 真机上量到的后果（vivo V2453A / Android 16）：窗口帧仍是全屏（系统不给窗口让位），
/// 但 `DecorView` 把内容视图内缩了：surface 从 y=133 起，顶栏完整可见；
/// 触摸仍对得上（事件坐标是 surface 自己的坐标系），软键盘照旧 `adjustResize`。
/// 窗口重建时 `android_main` 会再跑一次，所以不用在 resume 上重设。
///
/// 只在 API 30+ 上做：更低的系统本来就不强制 edge-to-edge，而且那个方法还不存在。
/// 读不到 API 级、或调用被拒（报错）时只写一条 log：顶栏被盖住是难看，不是不能用。
fn pin_decor_fits(app: &AndroidApp) {
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let activity = app.activity_as_ptr().cast();
    let outcome = vm.attach_current_thread(|env| -> jni::errors::Result<()> {
        // 平台类从 native 线程里找得到（应用类不行，见 `native_handle` 的注释）
        let version = env.find_class(jni::jni_str!("android/os/Build$VERSION"))?;
        let sdk = env
            .get_static_field(&version, jni::jni_str!("SDK_INT"), jni::jni_sig!("I"))?
            .i()?;
        if sdk < 30 {
            return Ok(());
        }
        // 这个指针是 AndroidApp 持有的全局引用，调用期间一直有效
        let activity = unsafe { JObject::from_raw(env, activity) };
        let window = env
            .call_method(
                &activity,
                jni::jni_str!("getWindow"),
                jni::jni_sig!("()Landroid/view/Window;"),
                &[],
            )?
            .l()?;
        env.call_method(
            &window,
            jni::jni_str!("setDecorFitsSystemWindows"),
            jni::jni_sig!("(Z)V"),
            &[jni::objects::JValue::Bool(true)],
        )?;
        Ok(())
    });
    if let Err(err) = outcome {
        log::warn!("没能让内容区避开系统栏（{err}）：顶栏可能被状态栏盖住");
    }
}

/// AGDK 的 native handle（`GameActivity` 那个静态字段 `nativeHandle`）。
///
/// 输入法快照要用它去调 AGDK 的 C 接口（见 [`ime_snapshot`]）。
///
/// **不要用 `getGameActivityNativeHandle()`**：AGDK 4.4.0 那个公开方法是
/// `if (isNativeDestroyed()) return nativeHandle; return 0;`，而 `isNativeDestroyed()` 是
/// `nativeHandleOwner != this`——当前这个 Activity 正好是 owner，于它只会返回 0
/// （真机上量到过）。这里直接读字段，JNI 不看 Java 的访问修饰符。
///
/// 线程：`android_main` 那条（本来就挂在 JVM 上，`attach_current_thread` 不会重新挂、
/// 也不会在结束时摘掉它——摘掉会把 `android-activity` 自己那份 `JNIEnv` 弄失效）。
/// 类从 Activity 对象上取（`FindClass` 在 native 线程上走的是系统类加载器，找不到应用类）。
///
/// 失败（方法/字段改名、线程挂不上）返回 `None`。**handle 为 0 也算失败**：
/// `GameActivity.initializeNativeCode` 是“先起 `android_main` 那条线程、再把返回值写进
/// 那个静态字段”，所以头几帧问到的可能就是 0——调用方会隔帧重试。
pub(crate) fn native_handle(app: &AndroidApp) -> Option<i64> {
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let activity = app.activity_as_ptr().cast();
    vm.attach_current_thread(|env| -> jni::errors::Result<i64> {
        // 这个指针是 `AndroidApp` 持有的全局引用，调用期间一直有效
        let activity = unsafe { JObject::from_raw(env, activity) };
        let class = env.get_object_class(&activity)?;
        env.get_static_field(&class, jni::jni_str!("nativeHandle"), jni::jni_sig!("J"))?
            .j()
    })
    .ok()
    .filter(|handle| *handle != 0)
}

/// AGDK 的文本状态结构（`GameActivityEvents.h` 的 `GameTextInputState`）。
///
/// 与头文件对得上：指针 + 字节长度 + 两个 `{i32, i32}` 范围。
/// 布局对不上时宁可编不过，所以这两条断言不是摆设。
#[repr(C)]
struct TextInputState {
    text_utf8: *const core::ffi::c_char,
    text_length: i32,
    selection: Span,
    composing_region: Span,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Span {
    start: i32,
    end: i32,
}

const _: () = assert!(core::mem::size_of::<TextInputState>() == 32);
const _: () = assert!(core::mem::size_of::<Span>() == 8);

unsafe extern "C" {
    /// AGDK 的 C 接口：把当前文本状态交给 `callback`。
    ///
    /// 符号来自 `android-activity` 链接进这个 .so 的那份 AGDK glue（它自己也调它）。
    fn GameActivity_getTextInputState(
        activity: *mut c_void,
        callback: Option<unsafe extern "C" fn(*mut c_void, *const TextInputState)>,
        context: *mut c_void,
    );
}

/// 一次输入法快照：正文 + 组词区（字节下标，`None` = 没有组词区）。
pub(crate) type Snapshot = (String, Option<(usize, usize)>);

/// 读一次输入法快照；还没初始化或解不出字符串时返回 `None`。
///
/// 为什么不用 `AndroidApp::text_input_state()`：`android-activity` 0.6.1 那个回调对
/// “输入法还没推过东西”的零初始化状态做 `from_raw_parts(空指针, 0)`，debug 构建里的
/// UB 检查直接 `abort`（真机上量到过，见 `docs/android.md` 的“中文输入”）。
/// 这里自己调 AGDK 的同一个 C 接口，回调里先看指针空不空。
///
/// 线程：`android_main` 那条（`android-activity` 自己也在那条线上读它）。
/// 只读：这个 C 接口只做一次结构体拷贝 + 回调，不改任何状态，也不碰 JNI。
/// `handle` 为 0（取不到）时不调，避免 C 侧把空指针当对象用。
pub(crate) fn ime_snapshot(handle: i64) -> Option<Snapshot> {
    if handle == 0 {
        return None;
    }
    let mut out: Option<Snapshot> = None;
    let context = (&mut out as *mut Option<Snapshot>).cast::<c_void>();
    unsafe { GameActivity_getTextInputState(handle as *mut c_void, Some(collect), context) };
    out
}

/// 把 C 侧给的状态拷成 Rust 的值。
///
/// `state` 的正文归 AGDK 所有，只在这次回调里有效（所以这里当场复制）。
unsafe extern "C" fn collect(context: *mut c_void, state: *const TextInputState) {
    if state.is_null() {
        return;
    }
    let out = unsafe { &mut *context.cast::<Option<Snapshot>>() };
    let state = unsafe { &*state };
    // 零初始化：输入法还没推过任何东西。长度与指针同源，不会只有一个成立
    if state.text_utf8.is_null() || state.text_length < 0 {
        return;
    }
    let bytes = unsafe {
        std::slice::from_raw_parts(state.text_utf8.cast::<u8>(), state.text_length as usize)
    };
    // AGDK 给的是 modified UTF-8：BMP 之外的字（emoji）在标准 UTF-8 里解不出来
    let Ok(text) = simd_cesu8::mutf8::decode(bytes) else {
        return;
    };
    let text = text.into_owned();
    let compose = compose_region(state.composing_region, &text);
    *out = Some((text, compose));
}

/// 组词区：AGDK 给的是 Java 的 UTF-16 码元下标（它直接搬运了 Java 的 `State`），
/// 这里换成正文里的字节下标。越界、反向、空区间都算“没有组词区”。
fn compose_region(span: Span, text: &str) -> Option<(usize, usize)> {
    if span.start < 0 || span.end < 0 {
        return None;
    }
    let start = utf16_to_byte(text, span.start as usize);
    let end = utf16_to_byte(text, span.end as usize);
    (end > start).then_some((start, end))
}

/// UTF-16 码元下标 → 字节下标；落在字符中间时收到该字符开头（`text` 是 UTF-8）。
fn utf16_to_byte(text: &str, index: usize) -> usize {
    let mut utf16 = 0;
    for (byte, ch) in text.char_indices() {
        if utf16 >= index {
            return byte;
        }
        utf16 += ch.len_utf16();
    }
    text.len()
}
