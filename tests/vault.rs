//! 存储层的验收：迁移、往返、锁、事务，以及"磁盘上读不到正文"。

use ToDay::model::{Change, MonthKey, MonthSheet};
use ToDay::search::MAX_HITS;
use ToDay::store::sqlite::SqliteVault;
use ToDay::store::{StoreError, Vault};
use chrono::NaiveDate;
use rusqlite::Connection;
use std::path::Path;

const PASSWORD: &str = "正确的马电池订书钉";

fn date(day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2025, 9, day).unwrap()
}

fn month() -> MonthKey {
    MonthKey::new(2025, 9).unwrap()
}

/// 临时目录 + 已经在库里的日记本（未设密码）
fn fresh() -> (tempfile::TempDir, SqliteVault) {
    let dir = tempfile::tempdir().unwrap();
    let vault = SqliteVault::open(dir.path().join("today.db")).unwrap();
    (dir, vault)
}

/// 临时目录 + 设好密码、已经解锁的日记本
fn unlocked() -> (tempfile::TempDir, SqliteVault) {
    let (dir, mut vault) = fresh();
    vault.setup(PASSWORD).unwrap();
    (dir, vault)
}

/// 从库里读原始的行（绕过 Vault 的加密层）
fn raw_row(path: &Path, date_text: &str) -> Option<(Vec<u8>, i64, i64)> {
    let conn = Connection::open(path).unwrap();
    conn.query_row(
        "SELECT body, updated_at, format FROM entries WHERE date = ?1",
        [date_text],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .ok()
}

#[test]
fn a_new_database_is_migrated_and_starts_unset() {
    let (dir, vault) = fresh();

    assert!(vault.needs_setup().unwrap(), "新库要先设主密码");

    let conn = Connection::open(dir.path().join("today.db")).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 1, "迁移跑完 user_version 应该是 1");
}

#[test]
fn the_journal_uses_wal() {
    let (dir, _vault) = fresh();

    let conn = Connection::open(dir.path().join("today.db")).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode.to_lowercase(), "wal");
}

#[test]
fn setup_then_read_back_a_month() {
    let (_dir, mut vault) = unlocked();
    assert!(!vault.needs_setup().unwrap());

    vault
        .apply(&[
            Change::set(date(1), "九月一号"),
            Change::set(date(17), "九月十七号"),
        ])
        .unwrap();

    let sheet = vault.load_month(month()).unwrap();
    assert_eq!(sheet.text(date(1)), "九月一号");
    assert_eq!(sheet.text(date(17)), "九月十七号");
    assert_eq!(sheet.text(date(2)), "", "没写过的那天是空的");
    assert_eq!(
        sheet.marked(),
        [date(1), date(17)].into_iter().collect(),
        "日历打点只认真正有内容的那几天"
    );
}

#[test]
fn other_months_are_not_returned() {
    let (_dir, mut vault) = unlocked();
    vault
        .apply(&[
            Change::set(date(1), "九月"),
            Change::set(NaiveDate::from_ymd_opt(2025, 10, 1).unwrap(), "十月"),
        ])
        .unwrap();

    let sheet = vault.load_month(month()).unwrap();
    assert_eq!(sheet.len(), 1);
    assert_eq!(sheet.text(date(1)), "九月");
}

#[test]
fn blank_text_counts_as_a_deletion() {
    let (dir, mut vault) = unlocked();
    vault.apply(&[Change::set(date(3), "写了点东西")]).unwrap();

    // 界面上把一行清空，落盘时就该是删除
    vault.apply(&[Change::set(date(3), "   \n\t ")]).unwrap();

    assert!(!vault.load_month(month()).unwrap().has(date(3)));
    assert!(
        raw_row(&dir.path().join("today.db"), "2025-09-03").is_none(),
        "库里不该留下空行"
    );
}

#[test]
fn delete_removes_the_day() {
    let (_dir, mut vault) = unlocked();
    vault
        .apply(&[
            Change::set(date(4), "会被删掉"),
            Change::set(date(5), "留着"),
        ])
        .unwrap();

    vault.apply(&[Change::delete(date(4))]).unwrap();

    let sheet = vault.load_month(month()).unwrap();
    assert!(!sheet.has(date(4)));
    assert_eq!(sheet.text(date(5)), "留着");
}

#[test]
fn reopening_with_the_same_password_keeps_the_data() {
    let (dir, mut vault) = unlocked();
    let path = dir.path().join("today.db");
    vault
        .apply(&[Change::set(date(9), "关掉再打开也该在")])
        .unwrap();
    vault.lock();
    drop(vault);

    let mut reopened = SqliteVault::open(&path).unwrap();
    assert!(!reopened.needs_setup().unwrap(), "设过密码就不该再要求设置");
    reopened.unlock(PASSWORD).unwrap();

    let sheet = reopened.load_month(month()).unwrap();
    assert_eq!(sheet.text(date(9)), "关掉再打开也该在");
}

#[test]
fn a_wrong_password_is_refused() {
    let (dir, vault) = unlocked();
    let path = dir.path().join("today.db");
    drop(vault);

    let mut vault = SqliteVault::open(&path).unwrap();
    assert!(matches!(
        vault.unlock("不是这个密码"),
        Err(StoreError::WrongPassword)
    ));
    assert!(
        matches!(vault.load_month(month()), Err(StoreError::Locked)),
        "密码不对就不该解锁"
    );
}

#[test]
fn an_empty_vault_still_checks_the_password() {
    let (_dir, mut vault) = unlocked();
    // 一条记录都没有的时候也要能判密码对错
    assert!(vault.load_month(month()).unwrap().is_empty());
    assert!(matches!(
        vault.unlock("错的"),
        Err(StoreError::WrongPassword)
    ));
    assert!(vault.unlock(PASSWORD).is_ok());
}

#[test]
fn a_locked_vault_refuses_to_read_or_write() {
    let (_dir, mut vault) = unlocked();
    vault.apply(&[Change::set(date(6), "锁之前写的")]).unwrap();

    vault.lock();

    assert!(matches!(vault.load_month(month()), Err(StoreError::Locked)));
    assert!(matches!(
        vault.apply(&[Change::set(date(7), "锁着的时候写不进去")]),
        Err(StoreError::Locked)
    ));

    vault.unlock(PASSWORD).unwrap();
    let sheet = vault.load_month(month()).unwrap();
    assert_eq!(sheet.text(date(6)), "锁之前写的");
    assert!(!sheet.has(date(7)), "锁着的时候那一笔不该落库");
}

#[test]
fn setup_twice_is_rejected() {
    let (_dir, mut vault) = unlocked();
    assert!(matches!(
        vault.setup("另一个密码"),
        Err(StoreError::AlreadySetUp)
    ));
}

#[test]
fn a_batch_of_changes_is_one_transaction() {
    let (dir, mut vault) = unlocked();
    let path = dir.path().join("today.db");

    // 用触发器让这一批里第二天写入失败，看第一天会不会留下半条
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER reject_second_day BEFORE INSERT ON entries
         WHEN NEW.date = '2025-09-02'
         BEGIN SELECT RAISE(ABORT, '测试用的拒绝'); END;",
    )
    .unwrap();
    drop(conn);

    let result = vault.apply(&[
        Change::set(date(1), "第一天"),
        Change::set(date(2), "第二天会被触发器挡住"),
    ]);
    assert!(result.is_err(), "批量写入应该整体失败");

    let sheet = vault.load_month(month()).unwrap();
    assert!(
        !sheet.has(date(1)),
        "事务里失败的批次必须整体回滚，不能只写进去一半"
    );
}

#[test]
fn an_empty_batch_is_a_no_op() {
    let (_dir, mut vault) = unlocked();
    vault.apply(&[]).unwrap();
    assert!(vault.load_month(month()).unwrap().is_empty());
}

#[test]
fn repeated_edits_in_one_batch_keep_the_last_one() {
    let (_dir, mut vault) = unlocked();
    vault
        .apply(&[
            Change::set(date(8), "第一版"),
            Change::set(date(8), "改过的"),
            Change::set(date(8), "最终版"),
        ])
        .unwrap();

    assert_eq!(vault.load_month(month()).unwrap().text(date(8)), "最终版");
}

#[test]
fn nothing_readable_is_left_on_disk() {
    const MARKER: &str = "机密暗号QUASAR1234";
    let (dir, mut vault) = unlocked();
    vault.apply(&[Change::set(date(10), MARKER)]).unwrap();
    vault.lock();
    drop(vault);

    // 主库和 WAL / SHM 都要查：数据可能还停在 WAL 里
    let mut checked = 0;
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let path = entry.unwrap().path();
        let bytes = std::fs::read(&path).unwrap();
        checked += 1;
        assert!(
            !bytes
                .windows(MARKER.len())
                .any(|window| window == MARKER.as_bytes()),
            "{} 里能搜到正文明文",
            path.file_name().unwrap().to_string_lossy()
        );
    }
    assert!(checked >= 1, "至少该读到主库文件");
}

#[test]
fn the_body_column_holds_a_sealed_blob() {
    let (dir, mut vault) = unlocked();
    let path = dir.path().join("today.db");
    vault.apply(&[Change::set(date(11), "明文")]).unwrap();

    let (body, updated_at, format) = raw_row(&path, "2025-09-11").unwrap();
    assert_eq!(format, 1, "行里要带上密文格式版本");
    assert!(updated_at > 0, "updated_at 该是 Unix 秒");
    assert!(
        body.len() >= 12 + 16,
        "密文至少要有 nonce 和 tag 那么长：实际 {} 字节",
        body.len()
    );
    assert!(
        !body.windows("明文".len()).any(|w| w == "明文".as_bytes()),
        "BLOB 里不该有明文"
    );
}

#[test]
fn a_body_moved_to_another_day_is_reported_as_corrupt() {
    let (dir, mut vault) = unlocked();
    let path = dir.path().join("today.db");
    vault
        .apply(&[
            Change::set(date(12), "十二号写的"),
            Change::set(date(13), "十三号写的"),
        ])
        .unwrap();
    vault.lock();
    drop(vault);

    // 把十二号的密文原样搬到十三号：AAD 是日期，解开时必须失败
    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "UPDATE entries SET body = (SELECT body FROM entries WHERE date = '2025-09-12')
         WHERE date = '2025-09-13'",
        [],
    )
    .unwrap();
    drop(conn);

    let mut vault = SqliteVault::open(&path).unwrap();
    vault.unlock(PASSWORD).unwrap();
    assert!(matches!(
        vault.load_month(month()),
        Err(StoreError::Corrupt(_))
    ));
}

#[test]
fn a_truncated_body_is_reported_as_corrupt() {
    let (dir, mut vault) = unlocked();
    let path = dir.path().join("today.db");
    vault.apply(&[Change::set(date(14), "会被截断")]).unwrap();
    vault.lock();
    drop(vault);

    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "UPDATE entries SET body = x'00' WHERE date = '2025-09-14'",
        [],
    )
    .unwrap();
    drop(conn);

    let mut vault = SqliteVault::open(&path).unwrap();
    vault.unlock(PASSWORD).unwrap();
    assert!(matches!(
        vault.load_month(month()),
        Err(StoreError::Corrupt(_))
    ));
}

#[test]
fn a_month_without_records_is_an_empty_sheet() {
    let (_dir, vault) = unlocked();
    let sheet: MonthSheet = vault.load_month(MonthKey::new(1999, 1).unwrap()).unwrap();
    assert!(sheet.is_empty());
    assert_eq!(sheet.key(), MonthKey::new(1999, 1).unwrap());
}

#[test]
fn a_newer_schema_version_is_refused() {
    let (dir, mut vault) = unlocked();
    let path = dir.path().join("today.db");
    vault.lock();
    drop(vault);

    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 99i64).unwrap();
    drop(conn);

    // 打不开比按老结构乱写安全：直接打开就该报错
    assert!(matches!(
        SqliteVault::open(&path),
        Err(StoreError::Corrupt(_))
    ));
}

#[test]
fn an_in_memory_vault_works_the_same() {
    let mut vault = SqliteVault::open_in_memory().unwrap();
    vault.setup(PASSWORD).unwrap();
    vault.apply(&[Change::set(date(15), "内存库")]).unwrap();
    assert_eq!(vault.load_month(month()).unwrap().text(date(15)), "内存库");

    vault.lock();
    assert!(matches!(vault.load_month(month()), Err(StoreError::Locked)));
}

#[test]
fn search_finds_text_across_months_newest_first() {
    let (_dir, mut vault) = unlocked();
    let august = NaiveDate::from_ymd_opt(2025, 8, 20).unwrap();
    vault
        .apply(&[
            Change::set(august, "八月写的咖啾"),
            Change::set(date(3), "九月也写了咖啾"),
            Change::set(date(17), "今天喝了咖啾"),
        ])
        .unwrap();

    let found = vault.search("咖啾").unwrap();
    let hits = &found.hits;
    let dates: Vec<_> = hits.iter().map(|hit| hit.date).collect();
    assert_eq!(
        dates,
        vec![date(17), date(3), august],
        "跨月、按日期从新到旧"
    );
    assert_eq!(hits[0].matched, "咖啾");
    assert_eq!(hits[0].at, 4, "命中位置按字符算");
    assert_eq!(hits[0].char_range(), 4..6, "选中区间就是命中那两个字符");
    assert_eq!(hits[0].before, "今天喝了");
}

#[test]
fn search_gives_one_hit_per_day() {
    let (_dir, mut vault) = unlocked();
    vault
        .apply(&[Change::set(date(3), "咖啾，又是咖啾")])
        .unwrap();

    let found = vault.search("咖啾").unwrap();
    assert_eq!(found.hits.len(), 1, "一天最多一条");
    assert_eq!(found.hits[0].at, 0, "取最靠前的那一处");
}

#[test]
fn a_blank_needle_is_an_empty_search() {
    let (_dir, mut vault) = unlocked();
    vault.apply(&[Change::set(date(3), "写了点东西")]).unwrap();

    assert!(vault.search("").unwrap().hits.is_empty());
    assert!(
        vault.search("  \n ").unwrap().hits.is_empty(),
        "纯空白不碰磁盘"
    );
}

#[test]
fn a_locked_vault_refuses_to_search() {
    let (_dir, mut vault) = unlocked();
    vault
        .apply(&[Change::set(date(3), "锁之前写的咖啾")])
        .unwrap();
    vault.lock();

    assert!(matches!(vault.search("咖啾"), Err(StoreError::Locked)));
}

#[test]
fn search_sees_the_latest_edit() {
    let (_dir, mut vault) = unlocked();
    vault.apply(&[Change::set(date(3), "咖啾")]).unwrap();
    assert_eq!(vault.search("咖啾").unwrap().hits.len(), 1);

    vault.apply(&[Change::set(date(3), "改成了茶")]).unwrap();
    assert!(
        vault.search("咖啾").unwrap().hits.is_empty(),
        "搜到的该是新内容"
    );
    assert_eq!(vault.search("茶").unwrap().hits.len(), 1);
}

#[test]
fn search_keeps_only_the_newest_days_up_to_the_cap() {
    let (_dir, mut vault) = unlocked();
    let start = date(1);
    let changes: Vec<Change> = (0..MAX_HITS + 5)
        .map(|offset| {
            let day = start + chrono::Duration::days(offset as i64);
            Change::set(day, format!("第 {offset} 天写了咖啾"))
        })
        .collect();
    vault.apply(&changes).unwrap();

    let found = vault.search("咖啾").unwrap();
    let hits = &found.hits;
    assert_eq!(hits.len(), MAX_HITS, "最多留最近 {MAX_HITS} 天");
    assert!(found.truncated, "库里还有更老的命中没列出来，该报被截了");
    assert_eq!(
        hits[0].date,
        start + chrono::Duration::days((MAX_HITS + 4) as i64),
        "留下的是最近的"
    );
    assert!(
        hits.windows(2).all(|pair| pair[0].date > pair[1].date),
        "仍然是按日期从新到旧"
    );
}

/// 命中数恰好等于上限、库里也确实只有这么多：不该说“被截了”。
#[test]
fn exactly_the_cap_is_not_truncation() {
    let (_dir, mut vault) = unlocked();
    let start = date(1);
    let changes: Vec<Change> = (0..MAX_HITS)
        .map(|offset| {
            let day = start + chrono::Duration::days(offset as i64);
            Change::set(day, format!("第 {offset} 天写了咖啾"))
        })
        .collect();
    vault.apply(&changes).unwrap();

    let found = vault.search("咖啾").unwrap();
    assert_eq!(found.hits.len(), MAX_HITS, "正好 {MAX_HITS} 天");
    assert!(
        !found.truncated,
        "恰好这么多就不该说被截了（界面正是靠这个标志决定说不说）"
    );
}
