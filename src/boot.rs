//! 装配：把"打开数据库 → 建好控制器"收在一处，桌面入口与 Android 入口共用。
//!
//! 这里只做装配，不做决策：不认识 SQL，也不认识界面细节。数据目录由调用方先定好
//! （桌面靠 `directories`，Android 靠 `AndroidApp::internal_data_path()`，见 [`crate::paths`]），
//! 窗口怎么开由调用方决定——两边的 `NativeOptions` 不一样。
//!
//! 失败一律原样交给调用方：桌面用 `anyhow` 加人话 context 后退出，
//! Android 写进 logcat 后不启动界面。

use crate::app::App;
use crate::store::StoreError;
use crate::store::sqlite::SqliteVault;
use std::path::Path;

/// 打开 `db` 并建好控制器。
///
/// `db` 的父目录会被 [`SqliteVault::open`] 按需创建。返回 `Err` 时控制器还没建起来：
/// 库打不开、版本太新、迁移失败，都在这一步挡下（`App::new` 只问一句"设过密码没有"）。
pub fn assemble(db: impl AsRef<Path>) -> Result<App, StoreError> {
    let vault = SqliteVault::open(db)?;
    App::new(Box::new(vault))
}
