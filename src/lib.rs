//! ToDay：一日一记的极简日记。
//!
//! 分层：`ui → app → model / store → crypto / paths`。依赖只能自上而下，
//! 下层不认识上层。详见 `docs/architecture.md`。

// crate 名固定为产品名 ToDay；改成 snake_case 会改掉品牌名与 `use ToDay::…`，故在 crate 级放行
#![allow(non_snake_case)]

pub mod app;
pub mod crypto;
pub mod model;
pub mod paths;
pub mod store;
pub mod ui;
