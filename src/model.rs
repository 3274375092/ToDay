//! 领域模型：一个月、一页记录、一次编辑。
//!
//! 这一层只有数据与不变量：不认识 egui，也不认识 SQL。
//! 月份边界、日期格式、以及"空白等于没写"这条规则都收在这里，
//! 别的层不要再各算一遍。

use chrono::{Datelike, Months, NaiveDate};
use std::collections::{BTreeMap, BTreeSet};

/// 库里存日期的格式：ISO 8601 日期。
///
/// 选它是因为按字典序比较等于按时间比较，`WHERE date >= '2025-09-01'`
/// 这种按月查询不用做任何转换。
pub const DATE_FORMAT: &str = "%Y-%m-%d";

/// 把日期写成库里的字符串
pub fn format_date(date: NaiveDate) -> String {
    date.format(DATE_FORMAT).to_string()
}

/// 解析库里的日期字符串
pub fn parse_date(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text, DATE_FORMAT).ok()
}

/// 某年某月。
///
/// 构造时校验过合法性，所以 [`MonthKey::first_day`] 一定有值；
/// 上下翻月走到日期范围尽头时停在原地，不会 panic。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MonthKey {
    year: i32,
    month: u32,
}

impl MonthKey {
    /// 年月合法（月 1..=12、日期在 chrono 支持的范围内）才有值
    pub fn new(year: i32, month: u32) -> Option<Self> {
        NaiveDate::from_ymd_opt(year, month, 1)?;
        Some(Self { year, month })
    }

    /// 某一天所在的月份
    pub fn from_date(date: NaiveDate) -> Self {
        Self {
            year: date.year(),
            month: date.month(),
        }
    }

    pub fn year(&self) -> i32 {
        self.year
    }

    pub fn month(&self) -> u32 {
        self.month
    }

    /// 这个月 1 号
    pub fn first_day(&self) -> NaiveDate {
        // MonthKey 只能由 new/from_date 构造，年月必然能过日期校验
        NaiveDate::from_ymd_opt(self.year, self.month, 1).expect("MonthKey 的年月一定合法")
    }

    /// 这个月有多少天
    pub fn days(&self) -> u32 {
        let start = self.first_day();
        match start.checked_add_months(Months::new(1)) {
            Some(next) => (next - start).num_days() as u32,
            // 只有 chrono 日期范围的最边上才走这里
            None => 31,
        }
    }

    /// 给按月查询用的半开区间 `[本月 1 号, 下月 1 号)`
    pub fn range(&self) -> (NaiveDate, NaiveDate) {
        let start = self.first_day();
        let end = start.checked_add_months(Months::new(1)).unwrap_or(start);
        (start, end)
    }

    /// 这个月的每一天，从 1 号开始
    pub fn dates(&self) -> impl Iterator<Item = NaiveDate> {
        let (year, month) = (self.year, self.month);
        (1..=self.days()).filter_map(move |day| NaiveDate::from_ymd_opt(year, month, day))
    }

    /// 上一个月
    pub fn prev(&self) -> Self {
        self.shift(-1)
    }

    /// 下一个月
    pub fn next(&self) -> Self {
        self.shift(1)
    }

    fn shift(&self, months: i32) -> Self {
        let delta = Months::new(months.unsigned_abs());
        let moved = if months >= 0 {
            self.first_day().checked_add_months(delta)
        } else {
            self.first_day().checked_sub_months(delta)
        };
        // 走到日期范围的尽头就停在原地，宁可不动也不 panic
        moved.map_or(*self, Self::from_date)
    }
}

/// 一个月的记录：日期 → 那天的正文。
///
/// 不变量：`rows` 里不含空白正文——空白等于没写，[`MonthSheet::set`] 是唯一的入口。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonthSheet {
    key: MonthKey,
    rows: BTreeMap<NaiveDate, String>,
}

impl MonthSheet {
    /// 空的一页
    pub fn empty(key: MonthKey) -> Self {
        Self {
            key,
            rows: BTreeMap::new(),
        }
    }

    pub fn key(&self) -> MonthKey {
        self.key
    }

    /// 某天写了什么（没写过就是空串）
    pub fn text(&self, date: NaiveDate) -> &str {
        self.rows.get(&date).map(String::as_str).unwrap_or("")
    }

    /// 这天写过东西吗
    pub fn has(&self, date: NaiveDate) -> bool {
        self.rows.contains_key(&date)
    }

    /// 写过东西的日子，给日历打点用
    pub fn marked(&self) -> BTreeSet<NaiveDate> {
        self.rows.keys().copied().collect()
    }

    /// 按日期升序遍历
    pub fn iter(&self) -> impl Iterator<Item = (NaiveDate, &str)> {
        self.rows.iter().map(|(date, text)| (*date, text.as_str()))
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// 写入一天：纯空白等于删除
    pub fn set(&mut self, date: NaiveDate, text: &str) {
        if text.trim().is_empty() {
            self.rows.remove(&date);
        } else {
            self.rows.insert(date, text.to_owned());
        }
    }

    /// 删掉一天
    pub fn remove(&mut self, date: NaiveDate) {
        self.rows.remove(&date);
    }

    /// 把一次编辑落到这一页上
    pub fn apply(&mut self, change: &Change) {
        match change {
            Change::Set { date, text } => self.set(*date, text),
            Change::Delete { date } => self.remove(*date),
        }
    }
}

/// 一次编辑：写一天，或删一天。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Set { date: NaiveDate, text: String },
    Delete { date: NaiveDate },
}

impl Change {
    /// 造一次编辑。纯空白等价于删除，空行不会在库里留下痕迹。
    pub fn set(date: NaiveDate, text: impl Into<String>) -> Self {
        let text = text.into();
        if text.trim().is_empty() {
            Self::Delete { date }
        } else {
            Self::Set { date, text }
        }
    }

    /// 删掉一天
    pub fn delete(date: NaiveDate) -> Self {
        Self::Delete { date }
    }

    pub fn date(&self) -> NaiveDate {
        match self {
            Self::Set { date, .. } | Self::Delete { date } => *date,
        }
    }

    /// 要写进去的正文（删除是空串）
    pub fn text(&self) -> &str {
        match self {
            Self::Set { text, .. } => text,
            Self::Delete { .. } => "",
        }
    }

    pub fn is_delete(&self) -> bool {
        matches!(self, Self::Delete { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn month_lengths_cover_leap_years() {
        assert_eq!(MonthKey::new(2025, 1).unwrap().days(), 31);
        assert_eq!(MonthKey::new(2025, 2).unwrap().days(), 28);
        assert_eq!(MonthKey::new(2024, 2).unwrap().days(), 29);
        assert_eq!(MonthKey::new(2000, 2).unwrap().days(), 29);
        assert_eq!(MonthKey::new(1900, 2).unwrap().days(), 28);
        assert_eq!(MonthKey::new(2025, 4).unwrap().days(), 30);
        assert_eq!(MonthKey::new(2025, 12).unwrap().days(), 31);
    }

    #[test]
    fn invalid_months_are_rejected() {
        assert!(MonthKey::new(2025, 0).is_none());
        assert!(MonthKey::new(2025, 13).is_none());
    }

    #[test]
    fn range_is_half_open() {
        let (start, end) = MonthKey::new(2025, 12).unwrap().range();
        assert_eq!(start, date(2025, 12, 1));
        assert_eq!(end, date(2026, 1, 1));
    }

    #[test]
    fn prev_and_next_wrap_the_year() {
        assert_eq!(
            MonthKey::new(2025, 1).unwrap().prev(),
            MonthKey::new(2024, 12).unwrap()
        );
        assert_eq!(
            MonthKey::new(2025, 12).unwrap().next(),
            MonthKey::new(2026, 1).unwrap()
        );
    }

    #[test]
    fn dates_enumerates_the_whole_month() {
        let days: Vec<_> = MonthKey::new(2024, 2).unwrap().dates().collect();
        assert_eq!(days.len(), 29);
        assert_eq!(days.first(), Some(&date(2024, 2, 1)));
        assert_eq!(days.last(), Some(&date(2024, 2, 29)));
    }

    #[test]
    fn sheet_drops_blank_text() {
        let key = MonthKey::new(2025, 9).unwrap();
        let mut sheet = MonthSheet::empty(key);
        sheet.set(date(2025, 9, 1), "写了");
        assert!(sheet.has(date(2025, 9, 1)));

        sheet.set(date(2025, 9, 1), "   \n ");
        assert!(!sheet.has(date(2025, 9, 1)), "纯空白必须等于没写");
        assert!(sheet.is_empty());
    }

    #[test]
    fn change_normalizes_blank_to_delete() {
        let day = date(2025, 9, 2);
        assert_eq!(Change::set(day, "  "), Change::Delete { date: day });
        assert!(!Change::set(day, "有字").is_delete());
        assert_eq!(Change::set(day, "有字").text(), "有字");
        assert_eq!(Change::delete(day).date(), day);
    }
}
