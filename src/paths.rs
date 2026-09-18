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
    /// `set_data_dir` 想指定成另一个目录（Android）：同一个进程里只能有一个库路径
    #[error("本进程的数据目录已经定成了别的路径")]
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
///
/// 幂等性：**同一个目录可以重复指定**——`android-activity` 会在同一个进程里再调一次
/// `android_main`（Activity 重建、退出后重开都是），那时拿到的是同一个私有目录，
/// 这一步必须放行，否则界面根本不会起来。指定成另一个目录返回 [`PathError::AlreadySet`]，
/// 绝不悄悄换掉：同一个进程里出现两个库路径会变成"写完看不见"。
#[cfg(target_os = "android")]
pub fn set_data_dir(dir: PathBuf) -> Result<(), PathError> {
    let existing = OVERRIDE.get().map(PathBuf::as_path);
    reusable(existing, &dir)?;
    if existing.is_none() {
        // 上一句刚确认过是空的；真被抢了也只可能是同一个路径（同一个进程里只有
        // `android_main` 会走到这里），所以丢弃返回值不算吞错
        let _ = OVERRIDE.set(dir);
    }
    Ok(())
}

/// 这次指定的目录能不能用：没定过、或与已定的是**同一个**目录都可以，换成别的目录不行。
///
/// 判据是路径相等，不是"有没有定过"。抽出来是为了能单测：`set_data_dir` 自己带着
/// `target_os` 的 `cfg`，桌面上编不出来。
#[cfg(any(target_os = "android", test))]
fn reusable(existing: Option<&Path>, dir: &Path) -> Result<(), PathError> {
    match existing {
        Some(existing) if existing != dir => Err(PathError::AlreadySet),
        _ => Ok(()),
    }
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
        assert_eq!(
            resolve(Some(dir)).unwrap().join("today.db"),
            dir.join("today.db")
        );
    }

    /// 没指定时仍然走 `directories`
    #[test]
    fn without_an_override_the_platform_directory_is_used() {
        assert_eq!(resolve(None).unwrap(), data_dir().unwrap());
    }

    /// 同一个目录再来一次要放行：`android-activity` 会在同一个进程里重起 `android_main`
    /// （Activity 重建、退出后重开），第二次拒了就是"一开就退回桌面"
    #[test]
    fn the_same_directory_can_be_set_again() {
        let dir = Path::new("/data/user/0/com.today.diary/files");
        assert!(reusable(None, dir).is_ok(), "还没定过：可以用");
        assert!(reusable(Some(dir), dir).is_ok(), "与已定的是同一个：还是它");
        assert!(
            matches!(
                reusable(Some(dir), Path::new("/data/user/0/别的包/files")),
                Err(PathError::AlreadySet)
            ),
            "换成另一个目录：拒绝"
        );
    }
}
