//! 应用数据目录。
//!
//! 所有落盘路径只有这一处实现：具体放在哪个平台目录由 `directories` 决定，
//! 上层一律通过这里拿路径，不自己拼 HOME / APPDATA。

use directories::ProjectDirs;
use std::path::PathBuf;

/// 解析数据目录时的失败
#[derive(Debug, thiserror::Error)]
pub enum PathError {
    #[error("找不到本机的应用数据目录（directories 返回空）")]
    Unavailable,
}

/// 应用数据目录。可能还不存在，由 [`crate::store::sqlite::SqliteVault::open`] 按需创建。
pub fn data_dir() -> Result<PathBuf, PathError> {
    let dirs = ProjectDirs::from("", "", "ToDay").ok_or(PathError::Unavailable)?;
    Ok(dirs.data_dir().to_path_buf())
}

/// 数据库文件
pub fn database_path() -> Result<PathBuf, PathError> {
    Ok(data_dir()?.join("today.db"))
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
}
