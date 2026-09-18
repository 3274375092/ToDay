# Android

同一个 crate 多编一个 `cdylib`（`libToDay.so`，入口是里面的 `android_main`）：界面、加密、
`Vault` 接口、SQLite 表结构一行不改，桌面那套代码就是手机跑的那套。数据落在 app 私有目录，
卸载即删，没有导出。

桌面与手机是**两个互相独立的库**：同一个主密码也打不开对方的 `today.db`（每个库自带盐与参数）。

## 现状

| 项 | 状态 |
| --- | --- |
| 编译与打包 | 本机验证过：`scripts/build-android.ps1` 出 `app-debug.apk` |
| 装机、真机行为 | **没有验证过**：写这份文档的机器上没有真机。`adb install` 与下面那份"真机验收清单"要有人跑一遍才算 |
| 中文（拼音）输入 | **待定**：winit 0.30.13 的 Android 后端只处理 `MotionEvent` / `KeyEvent`，不消费 game-activity 的 `TextEvent`（源码 `platform_impl/android/mod.rs` 里 `InputEvent` 只认前两者）。它只从 `KeyCharacterMap` 反查按键字符，而输入法的合成文本不走这条路 —— 所以拼音候选上屏**很可能收不到**。真机的结论决定要不要做 JNI 桥，见下面"中文输入" |

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
逐层退：**浮层 → 搜索 → 退出**。

退到最外层时由 `App::with_quit_hook` 装的钩子执行 `std::process::exit(0)`（`android.rs`）。
两件事先说清楚：

- **Activity 不会自己结束**：winit 对非音量键一律返回 `InputStatus::Handled`，
  系统收不到"这个返回键没人处理"，所以不会自己 finish。
- **为什么不调 `Activity::finish()`**：android-activity 的文档确实不建议在 `android_main` 里
  `process::exit`，但它给的替代路径要反过来依赖 winit 转发 Activity 的生命周期 —— 而 winit 0.30.13
  的 Android 后端**忽略** `MainEvent::Destroy`（源码里是 TODO），事件循环不会因为 Activity 结束而结束：
  本进程只有一个 Activity，进程跟着键一起走最干净。数据已经落盘（commit 过的 WAL 不怕进程退）。

`TODO:` 真机上如果发现最近任务里留下残影卡片，改成 JNI 调 `Activity::finish()`。
触发条件写在 `src/android.rs` 的退出钩子旁边。

## 中文输入（待真机确认）

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

- [ ] 首次启动 → 设主密码（≥8 字符）→ 进主界面，不闪白底（`adb logcat -s ToDay` 看得到启动日志）
- [ ] 中文不显示方块（`/system/fonts` 里找得到 CJK 字体；看不到就是"已知取舍"里那条）
- [ ] 顶栏与底栏没有被状态栏 / 手势条压住（内容能点得到）
- [ ] 点正文 → 软键盘弹出；打 ASCII 能进
- [ ] 打中文（拼音）：候选上屏进不进得来 ← **这条决定要不要做 JNI 桥**
- [ ] 打字 → 立刻按 Home 切后台 → 再回来：字没丢，光标与滚动位置还在
- [ ] 杀进程（最近任务里划掉）→ 重开 → 数据还在
- [ ] 点"锁定" → 回解锁界面；再解锁回来，正文重新出现
- [ ] 长按正文 → 弹出"删除这一天" → 删除 → 底部撤销条出现 → 点"撤销"能回来
- [ ] 底栏「搜索」→ 浮层弹出、焦点在搜索框、键盘弹出 → 输入 → 命中列出 → 点一条：浮层收起、
      跳到那天、命中那句被选中
- [ ] 底栏「日历」→ 浮层弹出（键盘不弹）→ 点某天：浮层收起、正文切到那天
- [ ] 底栏「下一天」跳到下一个写过的日子；本月后面没有记录时这枚按钮不出现
- [ ] 返回键：浮层开着先收浮层 → 再按清掉搜索 → 再按退出应用（任务列表里不留卡片）
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
- **字体不打包**：用系统那一份（省 10~16 MB）。系统里没有 CJK 字体时中文是方块，
  日志里会有一条 warn（`未找到中文字体文件`）。真机上出方块就改成打包一份。
- **触摸目标**：按钮 ≥44pt，但日历格子（42pt 高）与搜索框（34pt 高）比 44 小一点：
  它们按视觉密度定的高，单独抬会让日历一屏放不下。
- **后台不自动锁定**：与桌面一致（锁定是用户动作）。最近任务里的截图可能露出正文，
  记在这里；真要挡就得加 `FLAG_SECURE`，那要看用户愿不愿意接受"截图也截不了正文"。
- **没有横竖屏专用布局**：手机横屏宽度 ≥600 自然走宽屏那一套（侧栏回到左边一整列）。
- **`app.ron` 永远空着**：它只是拿到 suspend 落盘钩子的代价。
