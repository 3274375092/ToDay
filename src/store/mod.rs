//! 存储边界。
//!
//! 上层只认识 [`Vault`] 这个接口：不知道底下是 SQLite，也拿不到密钥字节——
//! 密钥留在实现内部，绕过加密写盘这件事在类型上就做不到。
//! 想换存储引擎（比如换成文件、换成本地服务），再实现一次这个 trait 就够了。

use crate::model::{Change, MonthKey, MonthSheet};
use crate::search::SearchResults;
use std::collections::BTreeMap;

pub mod sqlite;

/// 存储层的失败，按"上层能怎么办"分类
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// 没解锁。这个状态下不接受读也不接受写。
    #[error("日记本还没解锁")]
    Locked,
    #[error("主密码不对")]
    WrongPassword,
    #[error("这个日记本已经设过主密码了")]
    AlreadySetUp,
    #[error("数据目录或文件出错：{0}")]
    Io(#[from] std::io::Error),
    #[error("数据库出错：{0}")]
    Db(#[from] rusqlite::Error),
    #[error("加解密出错：{0}")]
    Crypto(#[from] crate::crypto::CryptoError),
    /// 库里的东西和预期对不上（版本太新、参数串坏了、密文解不开）
    #[error("数据损坏：{0}")]
    Corrupt(String),
}

/// 日记本的存储边界。
pub trait Vault {
    /// 还没设过主密码？
    fn needs_setup(&self) -> Result<bool, StoreError>;

    /// 首次设置主密码，设完直接进入解锁状态
    fn setup(&mut self, password: &str) -> Result<(), StoreError>;

    /// 用主密码解锁
    fn unlock(&mut self, password: &str) -> Result<(), StoreError>;

    /// 锁上：丢掉会话密钥
    fn lock(&mut self);

    /// 读一个月（未解锁时返回 [`StoreError::Locked`]）
    fn load_month(&self, key: MonthKey) -> Result<MonthSheet, StoreError>;

    /// 一次事务写一批编辑（未解锁时返回 [`StoreError::Locked`]）
    fn apply(&mut self, changes: &[Change]) -> Result<(), StoreError>;

    /// 全库搜索：正文里含 `needle` 的那些天，按日期从新到旧，一天最多一条，
    /// 最多 [`crate::search::MAX_HITS`] 天。
    ///
    /// `needle` 去掉首尾空白后为空时返回空结果（不碰磁盘）；未解锁返回 [`StoreError::Locked`]；
    /// 某一天的密文解不开返回 [`StoreError::Corrupt`]（与 [`Vault::load_month`] 同一口径：
    /// 宁可报错，不要悄悄少几天）。
    fn search(&self, needle: &str) -> Result<SearchResults, StoreError>;
}

/// 同一批编辑里同一天只留最后一次。
///
/// 一天被反复改（打字、删掉、又写回来）时，中间那些状态没有落盘的价值；
/// 合并之后一个日期在一次事务里只动一行。
pub fn coalesce(changes: &[Change]) -> Vec<Change> {
    let mut merged: BTreeMap<_, _> = BTreeMap::new();
    for change in changes {
        merged.insert(change.date(), change.clone());
    }
    merged.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn date(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2025, 9, day).unwrap()
    }

    #[test]
    fn coalesce_keeps_the_last_change_per_day() {
        let merged = coalesce(&[
            Change::set(date(1), "第一版"),
            Change::set(date(2), "另一天"),
            Change::set(date(1), "改过的"),
            Change::delete(date(2)),
        ]);

        assert_eq!(
            merged,
            vec![Change::set(date(1), "改过的"), Change::delete(date(2))],
            "每个日期只留最后一次，结果按日期排序"
        );
    }
}
