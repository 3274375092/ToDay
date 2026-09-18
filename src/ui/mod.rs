//! 视图层：只画界面，只表达意图。
//!
//! 依赖只有 `egui`（加一点纯数据模型）：不认识 `eframe`，也不认识 SQLite。
//! 用户点了什么、输了什么，都作为返回值交给控制器，副作用不在这里发生。

pub mod calendar;
pub mod diary;
pub mod search;
pub mod shell;
pub mod theme;
pub mod unlock;
