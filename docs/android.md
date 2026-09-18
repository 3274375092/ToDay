# Android

同一个 crate 多编一个 `cdylib`（`libToDay.so`，入口是里面的 `android_main`）：界面、加密、
`Vault` 接口、SQLite 表结构一行不改，桌面那套代码就是手机跑的那套。数据落在 app 私有目录，
卸载即删，没有导出。

桌面与手机是**两个互相独立的库**：同一个主密码也打不开对方的 `today.db`（每个库自带盐与参数）。

## 现状

| 项 | 状态 |
| --- | --- |
| 编译与打包 | 本机验证过：`cargo ndk` + `gradlew assembleDebug` 出 `app-debug.apk`（16~29 MB；`lib/arm64-v8a/libToDay.so` 与两个 dex 都在包里，manifest 合并结果里的包名 / Activity / `lib_name` / `allowBackup=false` / `adjustResize` 都对了） |
| 静态检查 | 本机验证过：桌面三道门禁 + `cargo ndk -t arm64-v8a clippy --lib -- -D warnings` 与 `cargo ndk doc` 都干净 |
| 真机（vivo V2453A / OriginOS，Android 16） | 启动、渲染（Vulkan / Adreno 825）、中文字体、系统栏、软键盘弹出、`adjustResize`、返回键退出、无崩溃 —— 都验证过，见下方"真机上验过什么" |
| 中文（拼音）输入 | 应用侧已接：每帧读一次 game-activity 的文本快照，差分后注入 `egui`（见"中文输入"）。差分逻辑在 `src/ime.rs`（有单测）。**排帧**：有输入框在焦点上时每帧再排一帧（真机上把上屏延迟从 480ms 档压到一帧档，数在"叫醒"一节），没焦点时一帧都不多排 |

## 真机上验过什么

设备：vivo V2453A（OriginOS，Android 16，1260×2800 / density 560）。

| 项 | 结论 |
| --- | --- |
| 启动与渲染 | ✓ `egui_wgpu` 选到 Vulkan / Adreno 825（备选 GLES 3.2），首帧不闪白底 |
| 中文字体 | ✓ 无方块：`/system/fonts/NotoSansCJK-Regular.ttc`（无衬线）与 `NotoSerifCJK-Regular.ttc`（衬线）都在，两条候选路径都补上了 |
| 系统栏 | ✓ 内容从状态栏下沿起画（顶栏完整、「锁定」可点），底栏在手势条之上。**但走的是运行时那条路**：主题里的 `windowOptOutEdgeToEdgeEnforcement` 在这台 Android 16 上没生效，见「已知取舍」的“系统栏走运行时 API”一条 |
| 软键盘 | ✓ 点输入框就弹（`mInputShown=true`），窗口跟着键盘缩（`adjustResize` + `windowSoftInputMode`）。**但这台机器上偶发弹不出来**：最后一次 show 是 `STATUS_SUCCESS`，紧接着 `system_server` 发了一次 hide（`mViewVisibility=GONE`）；同一时刻系统“设置”的搜索框能弹，所以不是设备级的。判据看 `dumpsys input_method` 的 `ImeTracker` 与 `mSoftInputShowHideHistory`，详见"中文输入"的“编辑器信息”一节 |
| 指尖点按与注入点按 | ✓ 两者都能把光标落进密码框（`adb shell input tap 630 1187` 之后密码框出现文本光标）。之前“手指点按整个不工作”的根因是应用自己崩了，不是触摸链路，见下方“点按曾整个不工作” |
| 键盘的字能不能进应用 | 应用侧已经接上了（见"中文输入"）：字从 `AndroidApp` 的文本快照读出来、差分后注进 `egui`。注入的 `KeyEvent` 仍然不行：`adb shell input text abc123` 一个字都没进（winit 的角色反查靠设备的 `KeyCharacterMap`，注入的虚拟设备查不到） |
| 返回键退出 | ✓ 干净退出，崩溃缓冲区 0 条；Activity 正常结束、前台回桌面；重开时同一个进程里重新起 `android_main`（数据还在） |
| 崩溃 | ✓ `adb logcat -b crash` 全程 0 条 |

### 点按曾整个不工作

结论：那是**应用在输入法连上的一瞬间自己崩了**，不是触摸事件进不来。

根因：装了原来的 `ToDayActivity` 之后，它重写的 `onCreateInputConnection` 会在新建的连接上调
`InputConnection.setSoftKeyboardActive`，而 AGDK 4.4.0 那个方法末尾无条件 `restartInput()`，
于是回到 `onCreateInputConnection` 再来一遍——Java UI 线程栈溢出。

判据（装机就能看）：

```
adb logcat -b crash | head -1
  java.lang.StackOverflowError: stack size 8188KB
  at com.today.diary.ToDayActivity$ImeSurfaceView.onCreateInputConnection(ToDayActivity.java:63)
  at com.today.diary.ToDayActivity$ImeSurfaceView.onCreateInputConnection(ToDayActivity.java:38)   ← 这两行反复出现
```

现象上是：界面画完最后几帧（所以截图看着完全正常），Java 线程卡在递归里，
触摸回调（`OnTouchListener`）也在那条线程上，随后进程被杀回桌面。
现在 `AndroidManifest.xml` 里直接就是 `com.google.androidgamesdk.GameActivity`，
没有 Java 子类，也没有 JNI 入口。

## 工具链

本机验证过的组合（换版本时把这张表一起改）：

| 东西 | 版本 | 从哪来 |
| --- | --- | --- |
| Rust target | `aarch64-linux-android`（真机）、`x86_64-linux-android`（模拟器） | `rustup target add` |
| cargo-ndk | 4.1.2 | `cargo install cargo-ndk` |
| NDK | 27.0.12077973 | `sdkmanager "ndk;27.0.12077973"` |
| SDK Platform | android-35 | `sdkmanager "platforms;android-35"` |
| build-tools | 35.0.0 | `sdkmanager "build-tools;35.0.0"` |
| Gradle | 8.9 | 仓库自带 wrapper（`android/gradlew`，`gradle-wrapper.jar` 已提交） |
| Android Gradle Plugin | 8.7.3 | `android/build.gradle.kts` |
| JDK | 17 | `JAVA_HOME`（AGP 8 要求 17） |
| adb | platform-tools | `sdkmanager "platform-tools"` |

`cargo-ndk` 找 NDK 的顺序是 `ANDROID_NDK_HOME` / `ANDROID_NDK_ROOT` / `NDK_HOME`，
都没有时 `scripts/build-android.ps1` 会从 `$env:ANDROID_HOME/ndk` 里挑一个装了的。
Gradle 找 SDK 靠 `ANDROID_HOME` / `ANDROID_SDK_ROOT`，或者 `android/local.properties` 里的
`sdk.dir`（该文件不进仓库）。

## 构建与安装

```powershell
pwsh scripts/build-android.ps1                 # 真机：arm64-v8a + debug APK
pwsh scripts/build-android.ps1 -Abi x86_64     # 模拟器
pwsh scripts/build-android.ps1 -Release        # release 的 .so 与 APK
```

脚本就两步，拆开跑也一样：

```bash
cargo ndk -t arm64-v8a -o android/app/src/main/jniLibs build --release --lib
cd android && ./gradlew assembleDebug          # Windows: gradlew.bat
```

`--lib` 是必须的：不带它 `cargo` 还会去编桌面那个 bin（Android 上没有它的入口）。

安装与看日志：

```bash
adb install -r android/app/build/outputs/apk/debug/app-debug.apk
adb logcat -s ToDay          # 应用自己的日志；eframe / wgpu 的日志是别的 tag
```

装之前可以先看看包里到底装了什么（不依赖真机）：

```bash
unzip -l android/app/build/outputs/apk/debug/app-debug.apk | grep -E "lib/|classes"
"$ANDROID_HOME/build-tools/35.0.0/aapt2.exe" dump badging <apk>     # 包名 / minSdk / targetSdk / label
```

### 跟着数走：debug 构建里的量尺

`src/app.rs` 的 `ImeStats`（`#[cfg(all(target_os = "android", debug_assertions))]`，release 里
整个结构不存在）每 2 秒往 logcat 写一行：

```bash
adb logcat -s ToDay | grep -E "量尺|长间隔|慢帧|窗口尺寸变化"
量尺 2.0s：帧 121；绘制 p50 0.2ms / p95 0.4ms / 最长 0.8ms；
  间隔 p50 17ms / p95 19ms / 最长 21ms（≥33ms 0 帧）；读快照最大间隔 20.7ms；档位 画；
  尺寸变化 0 次；窗口焦点=true 控件焦点=true ime=Some(…)
```

哪一项管什么：

| 字段 | 用途 |
| --- | --- |
| 读快照最大间隔 | **上屏延迟的上界**（字进了快照之后最多等多久才被读走） |
| 间隔 p50/p95/最长、≥33ms 帧数 | 掉帧与帧节奏（`dumpsys gfxinfo` 认不出 SurfaceView 里 Vulkan 画的帧，只有应用自己量得到） |
| 绘制 p50/p95/最长 | egui 这一趟的耗时（渲染倍率封顶的判据） |
| 档位 | 这一帧排了下一帧（画）还是没排（停） |
| 窗口尺寸变化 / 慢帧 | 键盘弹出/收起那一下的代价（“要不要改成不 resize”就看长帧与尺寸变化那帧重不重合） |
| 窗口焦点 / 控件焦点 / `ime` | 键盘不弹时到底是应用没要（`ime=None`）还是系统收了（`ime=Some` 但 `mInputShown=false`） |

驱动用的两条指令（真机上验过）：`adb shell input tap X Y` 点按，`adb shell input text nihao`
打字（**需要软键盘已经弹出来**：注入的按键走的是输入法的 `InputConnection`；键盘没弹时一个字都进不去）。
点按坐标与画面一致（`adb exec-out screencap -p` 量出来的像素位置直接用）。

## 构建上踩过的两个坑

**1. `kotlin-stdlib` 重复类（`checkDebugDuplicateClasses` 卡停构建）。**
`kotlin-stdlib-jdk7/jdk8` 从 Kotlin 1.8 起就是空壳（内容并进了 `kotlin-stdlib`），而 appcompat 那条链
（`lifecycle` → `kotlinx-coroutines-android` → `kotlin-stdlib-jdk8:1.6.21`）与 games-activity 都还引着那两个
独立构件，与 `kotlin-stdlib:1.8.22` 同时在场就是重复类。`android/app/build.gradle.kts` 里把这两个模块
在所有 configuration 上排掉了；升级 appcompat / games-activity 时如果这个报错又回来，先看这一条。

**2. `cargo ndk build` 要带 `--lib`。**
不带它 cargo 还会去编桌面那个 bin（`src/main.rs`）——Android 上没有它的入口，白编一遍还多一个链接目标。

## 数据在哪

- `/data/user/0/com.today.diary/files/today.db` —— 来自 `AndroidApp::internal_data_path()`，
  由 `paths::set_data_dir` 定下来。`directories` 没有 Android 实现，会拿 `HOME` 拼一串
  XDG 路径，那个目录在手机上不可写，所以这里不能走它。
- 同一个目录下还有一个 `app.ron`：eframe 的持久化文件，**永远是空的**（见"落盘"）。
- 卸载即删。没有导出、没有云同步，`AndroidManifest.xml` 里 `allowBackup="false"`
  且 `dataExtractionRules` 排除了云备份与设备间迁移 —— 密文也不出设备。
- 想拿出来看（debug 包）：

  ```bash
  adb exec-out run-as com.today.diary cat files/today.db > today.db
  ```

  用 SQLite 打开只能看到"哪几天写过、什么时候改的"，正文是密文（与桌面同一个格式）。

## 为什么是 game-activity

eframe 0.36 的两个 Android feature 里，`android-native-activity` 配 `accesskit` 会
`compile_error!`（accesskit 只兼容 game-activity），而 Android 上 `NativeActivity` 又不支持
输入法（android-activity 的 README 明说"no built-in input method support"）。所以：
`android-game-activity` + `androidx.games:games-activity`。

Java 那侧只有 `GameActivity` 这个壳（`AndroidManifest.xml` 里 `android:name="com.google.androidgamesdk.GameActivity"`），
它按 `android.app.lib_name` 去加载 `libToDay.so`、`dlsym` 找 `android_main`。
**不要**给 Gradle 开 prefab、也不要 `find_package(game-activity)`：C++ glue 由 `android-activity`
自己带一份，上游那份与它不兼容（android-activity README 的 "Don't compile and link the upstream
GameActivity prefab" 一节）。

## 落盘

| 时机 | 行为 |
| --- | --- |
| 打字 | 防抖 0.4 秒，一次写一批（与桌面一致） |
| 离开一行、删除、撤销、切日期、切月份、搜索到点、点锁定 | 立刻写（与桌面一致） |
| **切后台**（`Event::Suspended` → eframe `EventResult::Save`） | `App::save` → `flush()` |
| eframe 的自动保存（默认 30 秒） | 同上 |
| 返回键退到最外层 | 先 `flush()`，写成功才退出；写失败留在界面上（红条说明原因） |

`App::persist_egui_memory()` 返回 `false`：默认值会把 egui 的内存（含 `TextEditState` 的撤销缓冲，
里面有一份正文）明文写进 `app.ron`，与"锁定即丢正文"直接冲突。那个文件因此永远是空的 ——
`persistence_path` 存在的唯一理由就是拿到 `App::save` 这个钩子（`persistence` feature 只在
Android 目标上开，桌面没有这个钩子，行为一字不变）。

强杀（任务管理器 / `adb shell am force-stop`）最多丢最后 0.4 秒的输入，与桌面同一条取舍。

## 返回键与退出

返回键（`Keycode::Back` → winit 的 `Key::Named(BrowserBack)` → egui 的 `Key::BrowserBack`）
逐层退：**浮层 → 搜索 → 关窗**；键盘开着时那一下先收键盘（Android 的规矩，不是应用拦的）。

退到底时 `App::handle_back` 发的是 [`egui::ViewportCommand::Close`]，也就是桌面点关闭按钮那条路：
`handle_close_request` 先把待写的写完（写不成就不关），事件循环退出 → `run_native` 返回 →
`android_main` 返回 → **`android-activity` 的 springboard 自己调 `GameActivity_finish()`**
把 Activity 正常结束。

**不要在这里 `std::process::exit`。** 第一版就是这么写的，真机上直接崩：

```
I ToDay  : 返回键退出：待写的编辑已经落盘
F ...: JNI DETECTED ERROR: thread "android_main" using JNIEnv* from thread "main"
F libc  : Fatal signal 6 (SIGABRT)
```

进程退出会去拆 JVM 与 C++ 静态对象，而那时 Java 的 UI 线程还在跑（hwui 死锁 + JNIEnv 跨线程误用）。
android-activity 的文档也明确说别这么干；它给的替代路就是"让 `android_main` 返回"。

代价：关窗后进程会留在后台（Android 正常行为），重开时在同一个进程里重新起 `android_main`——
真机上验过，重开正常、数据还在。

## 中文输入

真机上量到的链路：软键盘敲字 → 输入法弹候选 → 选中 → 输入法调 `InputConnection.commitText`
（退格走 `deleteSurroundingText`）→ game-activity 的 `GameTextInput` 内部状态 → `android_app.textInputState = 1`
→ winit 的 `InputEvent::TextEvent`。**winit 0.30.13 的 Android 后端不消费它**（`InputEvent` 的 match 只认
`MotionEvent` / `KeyEvent`，其余打一条 warn 就算完），所以字必须由应用自己接。

接法分四段，代码分别在 `src/app.rs`（每帧的时机）、`src/android.rs`（读快照）、`src/ime.rs`（差分）。

### 1. 读快照：不是 `AndroidApp::text_input_state()`

`android-activity` 0.6.1 那个读法在**输入法还没推过任何东西**时会崩：它的回调对零初始化的状态
做 `slice::from_raw_parts(空指针, 0)`，debug 构建里的 UB 检查直接 `abort`。真机栈：

```
core::slice::raw::from_raw_parts::precondition_check
  (inlined by) <AndroidAppInner>::map_input_state_to_text_event_callback  ← android-activity/src/game_activity/mod.rs:717
GameTextInput_getState                       ← AGDK 的 C 函数
  (inlined by) GameActivityGlue>::text_input_state
  (inlined by) <ToDay::app::App>::inject_ime_events
```

而 crates.io 上 `android-activity` 最新就是 0.6.1，没有修复可用。所以 `src/android.rs` 自己调 AGDK
的同一个 C 接口（`GameActivity_getTextInputState`，符号来自 `android-activity` 链进 `libToDay.so`
的那份 glue），回调里先看 `text_utf8` 空不空。native handle 由公开的
`GameActivity.getGameActivityNativeHandle()` 经一次 JNI 读回（`AndroidApp::vm_as_ptr()` /
`activity_as_ptr()` 都是公开口子，**不往 Java 侧放任何东西**）。

两个单位要当心：

* 正文是 **modified UTF-8**（Java 的 `GetStringUTFChars` 格式），用 `simd_cesu8` 解。
* `selection` / `composingRegion` 是 **Java 的 UTF-16 码元下标**——AGDK 直接把 Java 的 `State`
  搬了过来，而正文长度却是 UTF-8 字节数。`src/android.rs` 里换成字节下标再交给差分。

### 2. 差分：`src/ime.rs`

每帧拿到的不是“这一次敲了什么”，而是输入法眼里的**整段正文**（含组词区）。所以比上一次：

| 快照的变化 | 产出的注入 |
| --- | --- |
| 正文是上一次的延长（多出几个字） | `Event::Text(多出来的那段)` |
| 正文短了 | 每个少掉的字符一个 `Key::Backspace` |
| 既不是延长也不是截断（改到中间、整段替换） | 先退到公共前缀，再把剩下的补上 |
| 只有组词区在变（拼音打在候选里） | 什么都不产出——组词区不算上屏 |

这一层不认识 `egui`、不碰磁盘、也不依赖 Android API，所以逐字追加 / 输入法退格 / 拼音组词区 /
中间插入 / 越界或切在字符中间的组词区都有单测（`cargo test ime`）。

一条去重规矩在 `App::apply_ime_snapshot`：**这一帧已经收到键盘的退格时不再补退格**——
输入法删字可能既走 `sendKeyEvent`（→ winit → `Event::Key { Backspace }`）又改快照，两边都注入就一按删两个字。

### 3. 编辑器信息（`inputType`）：不给就叫不出键盘

不给的话 `inputType` 是 0（`TYPE_NULL`），输入法只能摆一个数字/符号键盘（真机上就是这样）。
现在按当前屏给（`App::sync_ime_editor_info`）：

| 屏 | `inputType` |
| --- | --- |
| 设密码 / 解锁（主密码） | `TYPE_CLASS_TEXT \| TYPE_TEXT_FLAG_NO_SUGGESTIONS` |
| 搜索框（浮层开着） | `TYPE_CLASS_TEXT` |
| 正文 | `TYPE_CLASS_TEXT \| TYPE_TEXT_FLAG_MULTI_LINE`（拼音候选要留着） |

`imeOptions` 一律带 `IME_FLAG_NO_FULLSCREEN`：不给时输入法会去走“全屏提取”模式，
真机上量到的表现是 IME 窗口请求整屏高度、键盘一个像素都画不出来。

**不要用 `TYPE_TEXT_VARIATION_PASSWORD`**：vivo 会把带密码变体的编辑器路由给自家的安全输入法，
而这套在这台机器上每次都失败：

```
adb shell dumpsys input_method | grep TYPE_SHOW
type=TYPE_SHOW - STATUS_FAIL - com.vivo.secime.service:...        ← 请求者已经不是应用了
```

`NO_SUGGESTIONS` 不是密码变体，不会触发这条路由，它挡的是“输入法把主密码学进自己的词库”。

### 4. 叫醒：输入法上屏不给应用发事件

输入法上屏只在 game-activity 内部改状态，**不会给 winit 送任何事件**；不自己排帧的话，
一段上屏的字要等到下一次别的输入才画出来。所以**有输入框在焦点上时**
（`ctx.text_edit_focused()`），每帧都再排下一帧（`App::ime_frame` → `ime::next_interval`，
返回 `Duration::ZERO`）；没焦点时一帧都不多排。

**为什么是“立刻”而不是一个如 16ms/50ms 的间隔（真机上量出来的）**：`egui` 的排帧请求只在
“比已经排下的那次更早”时才落地（`ContextImpl::request_repaint_after`），而聚焦的输入框每帧
都会排下 0ms 的请求（光标闪烁、注入的字、eframe 每窗口只存一个“下次重画时间”）。于是任何
非零延迟都会被那一帧吃掉：真机上用 50ms 档量到“有焦点、停手”时，应用仍然只在光标闪烁
（约 480ms）时才画下一帧——上屏延迟就还是那个数。0ms 走的是 `outstanding` 那条路，
`egui` 保证下一趟一定回调。

真机上量到的账（vivo V2453A / 60Hz，同一个打字脚本 5 秒）：

| 场景 | 改前（200ms 轮询 + `desired_maximum_frame_latency: 2`） | 改后（有焦点就立刻排帧 + `1`） |
| --- | --- | --- |
| 连着打字（有焦点、输入法在推） | 最大读快照间隔 **481.9ms**；82 帧/2s，最长间隔 482ms | **20.7ms**；121 帧/2s，最长 21ms |
| 有焦点但停手 | 最大间隔 **484.7ms**；12~15 帧/2s | **20.9ms**；121 帧/2s |
| 没焦点空闲 | 0 帧（19.6 秒内一帧都没画） | 0 帧（一样） |

读快照间隔就是“字进了快照之后最多等多久才被读走”的上界，也就是用户眼里的上屏延迟。
“每帧都排”的代价是键盘开着时按屏幕刷新率画（真机上 121 帧/2s，一帧只画 0.2~0.5ms）——
中途没有可靠的回调可以插。见“已知取舍”。

`desired_maximum_frame_latency` 从默认的 2 降到 1（`src/android.rs` 用
`egui_wgpu::SurfaceConfig::LOW_LATENCY`，`present_mode` 仍是 `AutoVsync`）：少存一帧，
垂直同步与不撕裂照旧。

### 还没验的

真机上要用手指过一遍（我拄不到候选栅）：拼音候选上屏进正文、输入法退格删对一个字、
组词期间拼音不出现在正文里。单测盯的是差分本身，盯不到“快照里到底有没有字”。

## 窄屏外壳

- 断点 `shell::MOBILE_BREAKPOINT` = 600pt，判据是整窗宽度。桌面窗口最小 720pt，
  所以桌面永远走宽屏那一套（行为一字未改）。
- 顶栏：品牌 + 保存状态字在左，"回到今天"（非今天时才出现）与"锁定"在右，去掉日期戳
  （日期在正文装订线里已有）。
- 底栏：左边"本月已写 N 天"，右边「日历」「搜索」「下一天」三枚按钮。**没有键位提示**：
  手机上没有 `Ctrl`+`Enter`、`Ctrl`+`F` 和右键。
  "下一天"= 日记面板那份行列表里的下一天（与 `Ctrl`+`Enter` 同一个位置），
  后面没有了就不画这枚按钮（摆一枚按不动的按钮只会让人以为坏了）。
- 侧栏（搜索框 + 日历 + 页脚）在窄屏排进 `egui::Modal` 浮层：同一批 widget、同一个顺序，
  只有容器不同。点某天 / 点浮层外面 / 按返回键 / 打开一条搜索结果都收起。
- 正文面板拿到的是外壳交下来的 `shell::Chrome`（空页提示指哪儿、撤销条离窗口底边多高、
  放不放正文摘要），它自己仍然看不到断点与宽度：
  - 空页提示：窄屏是“点底栏「日历」挑一天，就可以开始写”，宽屏仍是“点左边日历上的任何一天”。
  - 撤销条浮在当前那条底栏之上（宽屏 30 + 12pt，窄屏 58 + 12pt），不再按屏幕底写死 42pt。
  - 撤销条窄屏不放正文摘要（400pt 的屏上排不下，见“已知取舍”）。
  - 行列表末尾留 18pt 空隙，最后一行不贴着下面的面板。
- 正文左右留白来自 `theme::column` 的 26pt 下限，窄屏不用另加一档。
- 触摸目标：`Piece::button` 的上下内边距在 Android 上撑到 44pt（顶栏/底栏/浮层里的按钮都走它），
  `style.spacing.button_padding` 也调到同一个量级（长按正文弹出的"删除这一天"跟着变大）。
  刻意**不动** `interact_size`：它会把单行输入框的最小高度一起抬起来，而搜索框与密码框是
  自己画的 34pt 底，抬了字就偏出框。
- 长按删除天然可用：egui 0.36 的 `Response::secondary_clicked()` 已经把触摸长按算进来，
  现有的 `response.context_menu(...)` 不用改。

## 真机验收清单

装机之后按这个顺序过一遍（每条都要有结论，结论回填到本节）。
注：带“注入”的几条是用 `adb shell input tap` 验的（触摸事件本身与指尖同一条路）；
**指尖那一轮还没跑**，没勾的都待人验。

- [x] 启动与渲染：不闪白底、`logcat -b crash` 0 条（自动验过）
- [x] 中文不显示方块（vivo 上有 `NotoSansCJK` 与 `NotoSerifCJK`；截图里「先设一个主密码」就是衬线中文）
- [x] 顶栏与底栏没有被状态栏 / 手势条压住（真机量过：状态栏 133px，顶栏 58pt=203px 从 y=133 起画；
      点「锁定」能生效、点密码框能叫起输入法、键盘弹出后卡片跟着上移）
- [x] 点输入框有反应：注入点按落在密码框上 → 字段获得焦点、出现文本光标（注入验的）
- [ ] **指尖**：点密码框 → 键盘弹出 → 键入并看到掩码（键盘能不能弹有偶发问题，见“中文输入”的“编辑器信息”：
      真机上量到过两种：点密码框时 `ImeTracker` 里有 SHOW、随即被系统收掉；点「锁定」回解锁页那次 QWERTY 正常弹出，
      卡片跟着上移（`adjustResize` 还在））
- [ ] **打中文（拼音）：候选上屏进不进得来** ← 应用侧已重做，待人验
- [x] 连着打字不落后：注入的字（`adb shell input text`，真机上算“输入法在推字”）连续 5 秒，
      读快照最大间隔 20.7ms（改前 481.9ms），121 帧/2s、没有一帧超过一个 vsync
- [x] 没焦点空闲不重画：把焦点丢开（ESC）之后 19.6 秒里一帧都没画（与承接前一致）
- [x] 键盘开关不动画：开关 3 轮，窗口尺寸变化那一帧只画了 0.1ms，掉帧 1/242（见“已知取舍”）
- [ ] **指尖**：拼音候选栏弹出/收起那一下不顿；点底栏「日历」「搜索」弹出浮层不顿
- [ ] 打字 → 立刻按 Home 切后台 → 再回来：字没丢，光标与滚动位置还在
- [ ] 杀进程（最近任务里划掉）→ 重开 → 数据还在
- [ ] 点“锁定” → 回解锁界面；再解锁回来，正文重新出现
- [x] 空月（翻到没记录的一个月）：提示指底栏「日历」，不写“左边日历”
- [x] 滚到列表最后一行：下面还留一点空隙，不贴着底栏
- [x] 长按正文 → 弹出“删除这一天” → 删除 → 底部撤销条出现（浮在底栏之上、整条在屏内） → 点“撤销”能回来
- [ ] 底栏「搜索」→ 浮层弹出、焦点在搜索框、键盘弹出 → 输入 → 命中列出 → 点一条：浮层收起、
      跳到那天、命中那句被选中
- [ ] 底栏「日历」→ 浮层弹出（键盘不弹）→ 点某天：浮层收起、正文切到那天
- [ ] 底栏「下一天」跳到下一个写过的日子；本月后面没有记录时这枚按钮不出现
- [x] 返回键：浮层开着先收浮层 → 再按清掉搜索 → 再按退出应用（Activity 正常结束、不复现 SIGABRT）
- [ ] 切后台再回来不会重新弹解锁界面（与桌面一致：不自动锁）
- [ ] `adb exec-out run-as com.today.diary cat files/today.db > today.db` 之后，
      在文件里搜不到正文的明文
- [ ] `app.ron` 是空的（0 字节或只有空结构），里面不该有任何正文

## 已知取舍

- **强杀丢最后 0.4 秒**：与桌面同一条。切后台、返回键退出、eframe 自动保存都会落盘。
- **键盘开着就按屏幕刷新率画**：输入法上屏不给应用发 winit 事件（见"中文输入"），只能自己排帧；
  而 `egui` 的排帧请求只在"比已经排下的那次更早"时才落地，任何非零延迟都会被那一帧吃掉
  （真机上用 50ms 档实测：有焦点、停手时仍然只在光标闪烁的约 480ms 才画下一帧）。
  所以只要**有输入框在焦点上**就每帧再排一帧：真机上 121 帧/2s、一帧画 0.2~0.5ms。
  没焦点时一帧都不多排（空闲 19.6 秒零帧，与接手前一致）——交出去的是"键盘开着但没在敲"
  那段路，换来的是上屏延迟从 480ms 档降到一帧档。
- **渲染倍率没有封顶**：
  真机上按 3.5×（屏幕原生，1260×2800）量：连着打字与弹浮层都是 121 帧/2s（60Hz 满帧）、
  没有一帧超过一个 vsync，所以 `App::MAX_PIXELS_PER_POINT` 保持 3.5（= 不封顶）。
  真撑不住时那一刀在这里（降到 3.0 → 2.5，布局单位是 pt，不受影响）。
- **键盘弹出/收起不改成“不 resize”**：按同一套量尺，键盘开关 3 轮里尺寸变化那一帧只画了
  0.1ms（不是长帧），两秒窗口里 121 帧只掉 1 帧（0.8%，判据是 10% 且长帧与尺寸变化那帧重合）。
  所以 `adjustResize` 不动，也不为此引入 IME inset 布局。
- **输入法快照是“全量比对”，不是“收到一次回调”**：比的是上一次的正文，所以输入法把光标
  移到别处（正文没变）不会产出任何动作；代价是“改到中间”只能退到公共前缀再重打（多几次退格）。
- **退格去重会丢一次“双重退格”**：同一帧里键盘退格与快照变短同时出现时只算一次；
  一次按键删两个字的输入法在这条路上会少删一个（比“一下删两个字”安全）。
- **快照的读法是绕上游 bug 的**：`android-activity` 0.6.1 的 `text_input_state()` 在输入法还没
  推过东西时会 `abort`（判据见"中文输入"），所以自己调 AGDK 的 C 接口。上游出新版、
  且修了这条之后，那一段可以换回公开 API。
- **vivo 上键盘偶发弹不出来**：`ImeTracker` 里能看到我们的 show 成功、随后 `system_server` 又发 hide。
  同一时刻系统“设置”的搜索框能弹，所以不是设备级；换输入法（搜狗 ↔ 百度）也一样。
  真机上遇到“键盘没弹”时先点一下别的输入框再回来，或重启应用。
- **主密码框在输入法眼里不是“安全字段”**：`TYPE_TEXT_VARIATION_PASSWORD` 会被 vivo 路由给
  自家安全输入法（那套在这台机器上弹不出来），所以用 `NO_SUGGESTIONS` 挡学词代替。
- **不做 pre-edit（组词下划线）与选区同步**：组词期间的字不进正文，上屏时才进——日记只要“字进得来”。
- **手机与桌面是两个独立的库**：没有导出、没有同步；卸载 = 日记没了。
- **字体不打包**：用系统那一份（省 10~16 MB）。系统里没有 CJK 字体时中文是方块或 `?`，
  日志里会有一条 warn（`未找到中文字体文件`）。
  真机上踩过一个反面例子：**衬线（`display`）那条候选链只写了桌面路径**，
  于是标题「先设一个主密码」渲染成 `???????`（七个字七个问号）——因为 `NotoSerifCJK` 在
  `/system/fonts/` 下，而候选表里没写。现在四条角色（含衬线、含 sans-bold）都有 Android 兜底，
  并且有一条测试盯着"每个角色都真的能排出中文"（`every_role_has_a_chinese_fallback_on_android`）。
- **输入法的回车/换行行为没单独验**：候选上屏与退格都接了，但输入法那颗回车键是走
  `sendKeyEvent` 还是 `performEditorAction` 没在真机上看过——输密码靠「开始写」按钮，
  正文里换行还没人试过。
- **窄屏的撤销条不放正文摘要**：`已删掉 9 月 18 日的日记` 一行放得下；
  带上 12 字摘要那句在 400pt 的屏上量到 444pt 宽，整条会超出屏幕（右端的「撤销」点不到），
  所以窄屏只报日期。宽屏仍带摘要。
- **系统栏走运行时 API，不走主题那条 opt-out**：`targetSdk 35` 在 Android 15+ 被强制 edge-to-edge。
  `values-v35/themes.xml` 里那条 `android:windowOptOutEdgeToEdgeEnforcement` 编进包里了
  （`aapt2 dump resources` 看得到 `0x0101069a=true`），但这台 Android 16 / OriginOS 上没生效：
  窗口帧量到的是 `[0,0][1260,2800]`，58pt 顶栏从屏幕 y=0 画，被 38pt 的状态栏盖住
  （「锁定」点不到、保存状态字看不见）。现在改在 `android.rs` 启动时 JNI 调
  `Window.setDecorFitsSystemWindows(true)`（API 30+）：窗口帧仍然全屏，但 `DecorView` 把内容视图
  内缩了——surface 从 y=133 起，顶栏完整、点按仍对齐、键盘照旧 `adjustResize`。
  主题里那条留着无害（换系统版本说不定又认它了）；真想自己画到系统栏底下（底色延伸、内容让位），
  得先在 Rust 侧拿到 insets（`GameActivity_getWindowInsets` 是现成的 C 接口），那是另一轮的事。
- **触摸目标**：按钮 ≥44pt，但日历格子（42pt 高）与搜索框（34pt 高）比 44 小一点：
  它们按视觉密度定的高，单独抬会让日历一屏放不下。
- **后台不自动锁定**：与桌面一致（锁定是用户动作）。最近任务里的截图可能露出正文，
  记在这里；真要挡就得加 `FLAG_SECURE`，那要看用户愿不愿意接受"截图也截不了正文"。
- **没有横竖屏专用布局**：手机横屏宽度 ≥600 自然走宽屏那一套（侧栏回到左边一整列）。
- **`app.ron` 永远空着**：它只是拿到 suspend 落盘钩子的代价。
