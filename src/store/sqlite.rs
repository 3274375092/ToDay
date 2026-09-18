//! SQLite 实现：日期明文、正文逐字段加密。
//!
//! 选 SQLite 是因为"按月查一批天"这种访问模式天生适合它：
//! 日期用明文存，日历打点和翻月都只是一次带索引的范围查询，
//! 而内容列是密文，直接打开库文件看不到一个字。

use chrono::{NaiveDate, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

use super::{StoreError, Vault};
use crate::crypto::{self, FORMAT_VERSION, KdfParams, Session};
use crate::model::{Change, MonthKey, MonthSheet, format_date, parse_date};
use crate::search::{self, SearchResults};

/// `meta` 表里已占用的键
const META_KDF_PARAMS: &str = "kdf.params";
const META_VERIFIER: &str = "kdf.verifier";

/// 一次迁移：把库从这个版本改到下一个版本
type Migration = fn(&Connection) -> Result<(), StoreError>;

/// 顺序迁移表。数组下标 + 1 就是迁移之后的 `user_version`，
/// 加表加列时往后追加一个函数，老库按序补齐，新库一次跑完。
/// 用 SQLite 自带的 `user_version` 就够了，不值得自己造一套迁移框架。
const MIGRATIONS: &[Migration] = &[create_schema];

/// SQLite 日记本
pub struct SqliteVault {
    conn: Connection,
    /// 解锁后活着；`lock()` 之后就没了
    session: Option<Session>,
}

impl SqliteVault {
    /// 打开（必要时创建）数据库文件并跑完迁移
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        Self::from_connection(Connection::open(path)?)
    }

    /// 内存库：测试用，以及"先试着跑一遍"的场合
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self, StoreError> {
        tune(&conn)?;
        migrate(&conn)?;
        Ok(Self {
            conn,
            session: None,
        })
    }

    /// 读一个 `meta` 键
    fn meta(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    /// 读 KDF 参数（没设过主密码时是 None）
    fn kdf_params(&self) -> Result<Option<KdfParams>, StoreError> {
        let Some(blob) = self.meta(META_KDF_PARAMS)? else {
            return Ok(None);
        };
        let text = String::from_utf8(blob)
            .map_err(|_| StoreError::Corrupt("KDF 参数不是合法的 UTF-8".into()))?;
        KdfParams::decode(&text)
            .map(Some)
            .map_err(|e| StoreError::Corrupt(e.to_string()))
    }
}

impl Vault for SqliteVault {
    fn needs_setup(&self) -> Result<bool, StoreError> {
        Ok(self.meta(META_VERIFIER)?.is_none())
    }

    fn setup(&mut self, password: &str) -> Result<(), StoreError> {
        if !self.needs_setup()? {
            return Err(StoreError::AlreadySetUp);
        }
        // 参数在、验证块不在：说明这个库被人动过。这时候不能另起一套参数，
        // 否则已经写进去的正文全成废纸，只能老实报告损坏。
        if self.kdf_params()?.is_some() {
            return Err(StoreError::Corrupt(
                "库里已有 KDF 参数却缺验证块，无法安全地重新设置主密码".into(),
            ));
        }

        let params = KdfParams::generate();
        let key = crypto::derive_key(password, &params)?;
        let verifier = crypto::verifier(&key)?;

        // 参数与验证块一起写：只写成功一半的库没法解锁，等于白设
        let tx = self.conn.transaction()?;
        put_meta(&tx, META_KDF_PARAMS, params.encode().as_bytes())?;
        put_meta(&tx, META_VERIFIER, &verifier)?;
        tx.commit()?;

        self.session = Some(Session::new(key));
        Ok(())
    }

    fn unlock(&mut self, password: &str) -> Result<(), StoreError> {
        let params = self
            .kdf_params()?
            .ok_or_else(|| StoreError::Corrupt("库里没有 KDF 参数".into()))?;
        let verifier = self
            .meta(META_VERIFIER)?
            .ok_or_else(|| StoreError::Corrupt("库里没有密码验证块".into()))?;

        let session = Session::new(crypto::derive_key(password, &params)?);
        // 解锁只验验证块，不碰 entries：空库也能判断密码对错
        if !session.matches_verifier(&verifier) {
            return Err(StoreError::WrongPassword);
        }
        self.session = Some(session);
        Ok(())
    }

    fn lock(&mut self) {
        // Session 一 drop，密钥就被清零
        self.session = None;
    }

    fn load_month(&self, key: MonthKey) -> Result<MonthSheet, StoreError> {
        let session = self.session.as_ref().ok_or(StoreError::Locked)?;
        let (start, end) = key.range();

        let mut sheet = MonthSheet::empty(key);
        let mut stmt = self
            .conn
            .prepare_cached("SELECT date, body FROM entries WHERE date >= ?1 AND date < ?2")?;
        let rows = stmt.query_map(params![format_date(start), format_date(end)], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;

        for row in rows {
            let (date_text, body) = row?;
            let (date, text) = open_row(session, &date_text, &body)?;
            sheet.set(date, &text);
        }
        Ok(sheet)
    }

    fn search(&self, needle: &str) -> Result<SearchResults, StoreError> {
        let needle = needle.trim();
        if needle.is_empty() {
            return Ok(SearchResults::default());
        }
        let session = self.session.as_ref().ok_or(StoreError::Locked)?;

        let mut results = SearchResults::default();
        // 倒序取：收满 `MAX_HITS` 就停时，留下的正好是最近的那些天。
        // `date` 是 TEXT PRIMARY KEY，这个排序走它的索引，不必另建一个
        let mut stmt = self
            .conn
            .prepare_cached("SELECT date, body FROM entries ORDER BY date DESC")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;

        for row in rows {
            let (date_text, body) = row?;
            let (date, text) = open_row(session, &date_text, &body)?;
            if let Some(hit) = search::hit(date, &text, needle) {
                if results.hits.len() >= search::MAX_HITS {
                    // 已经收满，而这里又多出一条：确实截了
                    results.truncated = true;
                    break;
                }
                results.hits.push(hit);
            }
        }
        Ok(results)
    }

    fn apply(&mut self, changes: &[Change]) -> Result<(), StoreError> {
        if changes.is_empty() {
            return Ok(());
        }
        let session = self.session.as_ref().ok_or(StoreError::Locked)?;
        let now = Utc::now().timestamp();

        // 一批编辑只开一个事务：要么全写进去，要么一条都不留
        let merged = super::coalesce(changes);
        let tx = self.conn.transaction()?;
        {
            // 语句在事务里准备，批量写完一次提交
            let mut upsert = tx.prepare_cached(
                "INSERT INTO entries (date, body, updated_at, format) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(date) DO UPDATE SET
                     body = excluded.body,
                     updated_at = excluded.updated_at,
                     format = excluded.format",
            )?;
            let mut delete = tx.prepare_cached("DELETE FROM entries WHERE date = ?1")?;

            for change in &merged {
                let date = format_date(change.date());
                match change {
                    Change::Delete { .. } => {
                        delete.execute([&date])?;
                    }
                    Change::Set { text, .. } => {
                        // AAD 就是日期：密文被挪到别的日期上会解不开
                        let body = session.seal(&date, text)?;
                        upsert.execute(params![date, body, now, FORMAT_VERSION])?;
                    }
                }
            }
        }
        tx.commit()?;
        Ok(())
    }
}

/// 把库里的一行还原成（日期，明文正文）。
///
/// 密码已经验过，日期解析不了、密文解不开都只可能是数据坏了；
/// 与 [`Vault::load_month`] 同一口径：宁可报错，不要悄悄少几天。
fn open_row(
    session: &Session,
    date_text: &str,
    body: &[u8],
) -> Result<(NaiveDate, String), StoreError> {
    let date = parse_date(date_text)
        .ok_or_else(|| StoreError::Corrupt(format!("日期列无法解析：{date_text}")))?;
    let text = session
        .open(date_text, body)
        .map_err(|_| StoreError::Corrupt(format!("{date_text} 这一天的密文解不开")))?;
    Ok((date, text))
}

/// 连接级别的一次性设置
fn tune(conn: &Connection) -> Result<(), StoreError> {
    // WAL：读写不互相堵，日常写盘更快
    conn.pragma_update(None, "journal_mode", "WAL")?;
    // NORMAL：断电最多丢最后一个事务，换取不必每个事务都 fsync
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    // 多进程/多线程撞上锁时等一会儿，而不是立刻返回 SQLITE_BUSY
    conn.pragma_update(None, "busy_timeout", 5000)?;
    // 外键约束现在用不上，但开着是为了以后加表时不踩坑
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

/// 按 `user_version` 顺序补齐迁移
fn migrate(conn: &Connection) -> Result<(), StoreError> {
    let mut version = user_version(conn)? as usize;
    if version > MIGRATIONS.len() {
        // 老程序打不开新库：宁可明确报错，也不要按老结构乱写
        return Err(StoreError::Corrupt(format!(
            "数据库是第 {version} 版，这个程序只认识到第 {} 版",
            MIGRATIONS.len()
        )));
    }

    while version < MIGRATIONS.len() {
        let migration = MIGRATIONS[version];
        // 一个迁移一个事务：中途失败整个库还停在上一个干净的版本
        let tx = conn.unchecked_transaction()?;
        migration(&tx)?;
        tx.pragma_update(None, "user_version", (version + 1) as i64)?;
        tx.commit()?;
        version += 1;
    }
    Ok(())
}

fn user_version(conn: &Connection) -> Result<i64, StoreError> {
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

/// 写一个 `meta` 键
fn put_meta(conn: &Connection, key: &str, value: &[u8]) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// v1：元数据表 + 记录表
fn create_schema(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "CREATE TABLE meta (
             key   TEXT PRIMARY KEY,
             value BLOB NOT NULL
         );
         CREATE TABLE entries (
             date       TEXT PRIMARY KEY,  -- 'YYYY-MM-DD'，明文：日历打点与按月查询要用
             body       BLOB NOT NULL,     -- nonce(12) || 密文 || tag(16)
             updated_at INTEGER NOT NULL,  -- Unix 秒
             format     INTEGER NOT NULL   -- 这一行的密文格式版本
         );",
    )?;
    // 不补 entries(date) 索引：TEXT PRIMARY KEY 已隐含唯一索引，再加一个只是白拖写入
    Ok(())
}
