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
| 中文（拼音）输入 | 桥已做（见"中文输入"）：软键盘上屏的字现在走 Java 的 `InputConnection` → JNI → egui。Rust 那半有测试盯着，**真机上要用手指敲一遍才算数**（我盲点不到候选栏） |

## 真机上验过什么

设备：vivo V2453A（OriginOS，Android 16，1260×2800 / density 560）。

| 项 | 结论 |
| --- | --- |
| 启动与渲染 | ✓ `egui_wgpu` 选到 Vulkan / Adreno 825（备选 GLES 3.2），首帧不闪白底 |
| 中文字体 | ✓ 无方块：`/system/fonts/NotoSansCJK-Regular.ttc`（无衬线）与 `NotoSerifCJK-Regular.ttc`（衬线）都在，两条候选路径都补上了 |
| 系统栏 | ✓ 内容没有被状态栏与手势条压住（`values-v35` 那句退出强制 edge-to-edge 生效了） |
| 软键盘 | ✓ 点输入框就弹（`mInputShown=true`），窗口跟着键盘缩（`adjustResize` + `windowSoftInputMode`），卡片没被挡住 |
| 键盘的字能不能进应用 | ✗ **进不来**：软键盘的字走 `InputConnection.commitText` → game-activity 的 `TextEvent`，而 winit 0.30.13 不消费它（注入的 `KeyEvent` 也不行：winit 对没有 `KeyCharacterMap` 的虚拟设备反查不出字符）。这是本轮最大的发现，修法见"中文输入" |
| 返回键退出 | ✓ 干净退出，崩溃缓冲区 0 条；Activity 结束、前台回桌面；重开时同一个进程里重新起 `android_main`（数据还在） |
| 崩溃 | ✓ `adb logcat -b crash` 全程 0 条（修之前 `std::process::exit` 那条路是 `SIGABRT`） |

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

## 中文输入（JNI 桥）

真机上量到的链路：软键盘（搜狗）敲字 → 输入法弹候选 → 选中 → 输入法调 `InputConnection.commitText`
→ game-activity 的 `GameTextInput` → `TextEvent` → **winit 0.30.13 不消费**（源码里 `InputEvent` 的 match
只认 `MotionEvent` / `KeyEvent`，其余只打一条 warn）→ 应用一个字也收不到。
注入的 `KeyEvent` 也不行：winit 的角色反查靠设备的 `KeyCharacterMap`，虚拟设备（`input keyevent`）查不到。

所以字必须从 Java 侧接：

1. `android/app/src/main/java/com/today/diary/ToDayActivity.java`
   —— 继承 `GameActivity`，在 `createSurfaceView()` 里换成自己的 SurfaceView，交出去一份
   `gametextinput.InputConnection` 的子类：`commitText` 与 `deleteSurroundingText`
   各推一次 native（`nativePushText` / `nativePushBackspace`），两份都照旧调 `super`
   （GameActivity 自己那份状态要跟输入法保持一致）。
   AGDK 的语义要注意：软键盘没被请求显示时 `onCreateInputConnection` **就返回 null**，
   窗口刚拿到焦点时输入法先来问一次——照旧返回 null，别自作聪明。
2. `src/android.rs` —— 两个 `#[unsafe(no_mangle)] extern "system"` 入口，只做加锁入队
   （可能在 Java 的 UI 线程被调，绝不在那里碰 egui）。
3. `src/app.rs` —— `push_text` / `push_backspace` 入队，[`App::raw_input_hook`](crate::app::App::raw_input_hook)
   每帧取走并注入 `egui::Event::Text` / `egui::Event::Key { Backspace }`（eframe 官方给输入注入留的口子）。
   退格也造按键事件，是因为输入法退格走的也是 `deleteSurroundingText`，不是键盘事件。

不做 pre-edit（组词下划线）：组词期间的字不进正文，上屏时才进——日记只要"字进得来"。

直觉上应该能行的地方：点中输入框时 egui 通过 `set_ime_allowed(true)` → winit 的
`show_soft_input(true)` 把软键盘弹出来，键盘上的字母键走 `KeyEvent` → `KeyCharacterMap` → ASCII 能进。

拼音这条路不走 `KeyEvent`：输入法把按键自己吃掉，组好词之后通过 `InputConnection.commitText`
上屏，那在 game-activity 里是 `InputEvent::TextEvent`，而 winit 0.30.13 的 Android 后端不处理它
（`InputEvent` 的 match 只有 `MotionEvent` 与 `KeyEvent`，其余打一条 warn 就算完）。
**所以拼音候选上屏很可能收不到。**

真机结论决定下一步：

| 真机现象 | 结论 |
| --- | --- |
| 拼音能上屏 | 什么都不用做（那就说明设备上走的是别的路径） |
| 拼音上屏丢字 / 候选不上屏 | 做 JNI 桥：Gradle 侧给 Activity 挂一个 0 尺寸 `EditText`（或子类化 GameActivity 覆盖 `onCreateInputConnection`），把 `commitText` 经 JNI 推给 Rust；Rust 侧在 `App::raw_input_hook`（eframe 官方给输入注入留的口子）里 drain 成 `egui::Event::Text` 塞进 `raw_input.events` |
| 连软键盘都不弹 | 先查 `set_ime_allowed`，再查窗口是不是被 `windowOptOutEdgeToEdgeEnforcement` 之外的东西挡住了 |

不做 pre-edit 下划线与选区同步：日记只要"字进得来"。

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
- 正文左右留白来自 `theme::column` 的 26pt 下限，窄屏不用另加一档。
- 触摸目标：`Piece::button` 的上下内边距在 Android 上撑到 44pt（顶栏/底栏/浮层里的按钮都走它），
  `style.spacing.button_padding` 也调到同一个量级（长按正文弹出的"删除这一天"跟着变大）。
  刻意**不动** `interact_size`：它会把单行输入框的最小高度一起抬起来，而搜索框与密码框是
  自己画的 34pt 底，抬了字就偏出框。
- 长按删除天然可用：egui 0.36 的 `Response::secondary_clicked()` 已经把触摸长按算进来，
  现有的 `response.context_menu(...)` 不用改。

## 真机验收清单

装机之后按这个顺序过一遍（每条都要有结论，结论回填到本节）：

- [x] 首次启动 → 设主密码（≥8 字符）→ 进主界面，不闪白底（`adb logcat -s ToDay` 看得到启动日志）
- [x] 中文不显示方块（vivo 上有 `NotoSansCJK` 与 `NotoSerifCJK`）
- [x] 顶栏与底栏没有被状态栏 / 手势条压住（内容能点得到）
- [x] 点正文 → 软键盘弹出
- [ ] **打中文（拼音）：候选上屏进不进得来** ← JNI 桥做好后待人验（Rust 侧有测试盯着）
- [ ] 打字 → 立刻按 Home 切后台 → 再回来：字没丢，光标与滚动位置还在
- [ ] 杀进程（最近任务里划掉）→ 重开 → 数据还在
- [ ] 点"锁定" → 回解锁界面；再解锁回来，正文重新出现
- [ ] 长按正文 → 弹出"删除这一天" → 删除 → 底部撤销条出现 → 点"撤销"能回来
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
- **手机与桌面是两个独立的库**：没有导出、没有同步；卸载 = 日记没了。
- **系统栏**：`targetSdk 35` 会被强制 edge-to-edge，而 winit 0.30 不把 insets 转给应用，
  所以 `values-v35/themes.xml` 里关掉了这条强制（`android:windowOptOutEdgeToEdgeEnforcement`）。
  想让内容画满屏幕就得先在 Rust 侧拿到安全区，那是另一轮的事。
- **字体不打包**：用系统那一份（省 10~16 MB）。系统里没有 CJK 字体时中文是方块或 `?`，
  日志里会有一条 warn（`未找到中文字体文件`）。
  真机上踩过一个反面例子：**衬线（`display`）那条候选链只写了桌面路径**，
  于是标题「先设一个主密码」渲染成 `???????`（七个字七个问号）——因为 `NotoSerifCJK` 在
  `/system/fonts/` 下，而候选表里没写。现在四条角色（含衬线、含 sans-bold）都有 Android 兜底，
  并且有一条测试盯着"每个角色都真的能排出中文"（`every_role_has_a_chinese_fallback_on_android`）。
- **输入法的回车/换行行为没单独验**：候选上屏与退格都接了，但输入法那颗回车键是走
  `sendKeyEvent` 还是 `performEditorAction` 没在真机上看过——输密码靠「开始写」按钮，
  正文里换行还没人试过。
- **触摸目标**：按钮 ≥44pt，但日历格子（42pt 高）与搜索框（34pt 高）比 44 小一点：
  它们按视觉密度定的高，单独抬会让日历一屏放不下。
- **后台不自动锁定**：与桌面一致（锁定是用户动作）。最近任务里的截图可能露出正文，
  记在这里；真要挡就得加 `FLAG_SECURE`，那要看用户愿不愿意接受"截图也截不了正文"。
- **没有横竖屏专用布局**：手机横屏宽度 ≥600 自然走宽屏那一套（侧栏回到左边一整列）。
- **`app.ron` 永远空着**：它只是拿到 suspend 落盘钩子的代价。
