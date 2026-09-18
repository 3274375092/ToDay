//! ToDay：一日一记的极简日记。
//!
//! 分层：`ui → app → model / store → crypto / paths`。依赖只能自上而下，
//! 下层不认识上层。详见 `docs/architecture.md`。
//!
//! 两个入口共用同一套装配与控制器：桌面是 `main.rs`，Android 是 `android.rs` 的 `android_main`
//! （由 game-activity 调用）。两者都把 `boot::assemble` 的结果交给 eframe，只有 `NativeOptions`
//! 与数据目录的来源不同。

// crate 名固定为产品名 ToDay；改成 snake_case 会改掉品牌名与 `use ToDay::…`，故在 crate 级放行
#![allow(non_snake_case)]

#[cfg(target_os = "android")]
pub mod android;
pub mod app;
pub mod boot;
pub mod crypto;
pub mod model;
pub mod paths;
pub mod search;
pub mod store;
pub mod ui;
pub mod window_state;
