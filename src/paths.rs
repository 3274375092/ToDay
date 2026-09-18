//! 应用数据目录。
//!
//! 所有落盘路径只有这一处实现：桌面由 `directories` 决定放在哪个平台目录，
//! Android 由 `android_main` 把 `AndroidApp::internal_data_path()` 交进 `set_data_dir`——
//! `directories` 没有 Android 实现，会拿 `HOME` 拼出一串 XDG 路径，那个目录在手机上不可写。
//! 上层一律通过这里拿路径，不自己拼 HOME / APPDATA。

use directories::ProjectDirs;
use std::path::{Path, PathBuf};

/// 解析数据目录时的失败
#[derive(Debug, thiserror::Error)]
pub enum PathError {
    #[error("找不到本机的应用数据目录（directories 返回空）")]
    Unavailable,
    /// Android 的 app 私有目录没指定：没有它就没有可写的地方，只能不启动
    #[error("拿不到应用私有目录")]
    NoAppDir,
    /// [`set_data_dir`] 只该被调一次；再调说明有两处入口在抢同一个进程
    #[error("本进程的数据目录已经定过了")]
    AlreadySet,
}

/// 解析数据目录：进程内被指定过就听指定的，否则走 `directories`。
///
/// `override_dir` 就是 Android 交进来的 app 私有目录。纯函数，不读进程状态，所以能单测。
fn resolve(override_dir: Option<&Path>) -> Result<PathBuf, PathError> {
    if let Some(dir) = override_dir {
        return Ok(dir.to_path_buf());
    }
    let dirs = ProjectDirs::from("", "", "ToDay").ok_or(PathError::Unavailable)?;
    Ok(dirs.data_dir().to_path_buf())
}

/// 进程内被指定过的数据目录，见 [`set_data_dir`]
#[cfg(target_os = "android")]
static OVERRIDE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// 应用数据目录。可能还不存在，由 [`crate::store::sqlite::SqliteVault::open`] 按需创建。
///
/// Android 上没有可推的默认值：必须由 [`set_data_dir`] 指定过，否则返回 [`PathError::NoAppDir`]。
#[cfg(target_os = "android")]
pub fn data_dir() -> Result<PathBuf, PathError> {
    resolve(Some(OVERRIDE.get().ok_or(PathError::NoAppDir)?))
}

/// 应用数据目录。可能还不存在，由 [`crate::store::sqlite::SqliteVault::open`] 按需创建。
#[cfg(not(target_os = "android"))]
pub fn data_dir() -> Result<PathBuf, PathError> {
    resolve(None)
}

/// 指定本进程的数据目录，在这之后 [`data_dir`] 都返回它。
///
/// 只在 Android 上有：那边的私有目录 `/data/user/0/<包名>/files` 只有 `AndroidApp` 知道。
/// 幂等性：**只允许调一次**，第二次返回 [`PathError::AlreadySet`]，绝不悄悄换成另一个目录——
/// 同一个进程里出现两个库路径会变成"写完看不见"。
#[cfg(target_os = "android")]
pub fn set_data_dir(dir: PathBuf) -> Result<(), PathError> {
    OVERRIDE.set(dir).map_err(|_| PathError::AlreadySet)
}

/// 数据库文件
pub fn database_path() -> Result<PathBuf, PathError> {
    Ok(data_dir()?.join("today.db"))
}

/// 窗口尺寸记忆文件（见 [`crate::window_state`]）
pub fn window_state_path() -> Result<PathBuf, PathError> {
    Ok(data_dir()?.join("window.txt"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_file_lives_under_the_data_dir() {
        let dir = data_dir().unwrap();
        let db = database_path().unwrap();
        assert_eq!(db.parent(), Some(dir.as_path()));
        assert_eq!(db.file_name().unwrap(), "today.db");
    }

    #[test]
    fn window_state_file_lives_under_the_data_dir() {
        let dir = data_dir().unwrap();
        let window = window_state_path().unwrap();
        assert_eq!(window.parent(), Some(dir.as_path()));
        assert_eq!(window.file_name().unwrap(), "window.txt");
    }

    /// 指定过目录之后，两个落盘路径都落在那个目录下——Android 的库必须落在 app 私有目录里
    #[test]
    fn an_explicit_directory_wins_over_the_platform_default() {
        let dir = std::path::Path::new("D:/tmp/一个数据目录");
        assert_eq!(resolve(Some(dir)).unwrap(), dir);
        assert_eq!(resolve(Some(dir)).unwrap().join("today.db"), dir.join("today.db"));
    }

    /// 没指定时仍然走 `directories`：桌面语义一点没变
    #[test]
    fn without_an_override_the_platform_directory_is_used() {
        assert_eq!(resolve(None).unwrap(), data_dir().unwrap());
    }
}
