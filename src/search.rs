//! 全文搜索的纯逻辑：一处命中带摘要，以及"这串字像不像日期"。
//!
//! 这里不碰磁盘、不认识 egui：调用方把（日期、明文正文、查询）递进来，
//! 这个模块只回答"命中在哪、上下文是什么、查询能不能当日期跳过去"。
//! 正文从哪来、扫几个月的库、要不要防抖，都由控制器决定（见 `app.rs`）。

use chrono::{Datelike, Local, NaiveDate};

/// 结果上限：最多留最近的这么多天（库里按日期从新到旧给）
pub const MAX_HITS: usize = 200;

/// 一次全库搜索的结果。
///
/// `truncated` 说的是“到上限就停了，库里还有更老的命中没列出来”：
/// 命中数恰好等于上限时也要能分清“就这么多”与“被截了”，
/// 所以由扫描的一方判定，而不是让界面拿 `hits.len() == MAX_HITS` 去猜。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchResults {
    /// 命中，按日期从新到旧
    pub hits: Vec<Hit>,
    /// 是否因为到 [`MAX_HITS`] 而停下（库里还有更老的没列）
    pub truncated: bool,
}

/// 摘要里命中前留多少字符
const EXCERPT_BEFORE: usize = 14;
/// 摘要里命中后留多少字符
const EXCERPT_AFTER: usize = 36;

/// 一天里的一处命中：位置加三段摘要。
///
/// 只带摘要，不带整篇正文——卡片要回答的是"这条在哪一天"，
/// 不是把一天读一遍。`before` / `after` 已被折成一行，两端截断处补 `…`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// 命中发生在哪一天
    pub date: NaiveDate,
    /// 命中在正文里的字符下标（不是字节下标，选中它时要按字符算）
    pub at: usize,
    /// 命中前的上下文
    pub before: String,
    /// 命中的原文，保留大小写
    pub matched: String,
    /// 命中后的上下文
    pub after: String,
}

impl Hit {
    /// 命中那一段的字符区间，给"跳过去之后选中它"用。
    ///
    /// 正文若在跳转前被改过，下标可能已经越界：由 egui 把区间夹进当前文本长度。
    pub fn char_range(&self) -> std::ops::Range<usize> {
        self.at..self.at + self.matched.chars().count()
    }
}

/// 这一天的正文算不算命中：命中给摘要，否则 `None`。
///
/// `needle` 去掉首尾空白后为空、或比正文长 → `None`。
/// 比较只对 ASCII 忽略大小写（`char::eq_ignore_ascii_case`），中文与其它脚本按原样比。
/// 一天多处命中只取最靠前的那一处。
///
/// 匹配阶段一趟扫完、不分配：库大时这条路径在按天的循环里跑，
/// 每行都摊一份整篇正文的字符表会自己堆上去（搜索在 UI 线程上做）。
pub fn hit(date: NaiveDate, text: &str, needle: &str) -> Option<Hit> {
    let needle = needle.trim();
    let needle_chars: Vec<char> = needle.chars().collect();
    if needle_chars.is_empty() {
        return None;
    }

    let at = match_char_index(text, &needle_chars)?;
    let matched_len = needle_chars.len();
    let total = text.chars().count();

    let start = at.saturating_sub(EXCERPT_BEFORE);
    let end = (at + matched_len + EXCERPT_AFTER).min(total);
    let matched_end = at + matched_len;
    Some(Hit {
        date,
        at,
        before: excerpt(&chars_between(text, start, at), start > 0, false),
        matched: excerpt(&chars_between(text, at, matched_end), false, false),
        after: excerpt(&chars_between(text, matched_end, end), false, end < total),
    })
}

/// 在 `text` 里找出 `needle_chars` 第一次出现的位置（按字符算下标）。
///
/// 留一个长度为 `needle_chars.len()` 的滑动窗口，按字符推进：
/// 窗内逐位比，不匹配就把窗尾吐出来、再吞一个字符。全程只有窗口这一点内存，
/// 不把整篇正文摊成字符表。
fn match_char_index(text: &str, needle_chars: &[char]) -> Option<usize> {
    let mut window: std::collections::VecDeque<char> = std::collections::VecDeque::new();
    for (index, ch) in text.chars().enumerate() {
        window.push_back(ch);
        if window.len() > needle_chars.len() {
            window.pop_front();
        }
        // 窗未满（正文比查询短）时不算命中
        if window.len() == needle_chars.len()
            && window
                .iter()
                .zip(needle_chars)
                .all(|(found, wanted)| found.eq_ignore_ascii_case(wanted))
        {
            // 窗满且对上：命中起点是当前字符下标往前退窗口长度
            return Some(index + 1 - needle_chars.len());
        }
    }
    None
}

/// `text` 里第 `start..end` 个字符（按字符算，不是字节）
fn chars_between(text: &str, start: usize, end: usize) -> Vec<char> {
    text.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

/// 查询像不像日期；像就给出跳到哪一天。
///
/// 只认两段与三段的写法，一段一律当正文关键词（`2025`、`0917`、`17` 都不算日期）：
///
/// * 三段：`2025-09-17` / `2025/9/17` / `2025.9.17` / `2025年9月17日`（`日` 可省），
///   年份按字面值；
/// * 两段且第一段 4 位：`2025-09` / `2025年9月` → 该月 1 号；
/// * 两段且第一段非 4 位：`9-17` / `9/17` / `9.17` / `9月17日` → 今天所在那一年，
///   哪怕那天还没到。
///
/// 年月日不存在（`2025-02-30`）或分隔符混用（`2025-9.17`）时返回 `None`。
pub fn date_query(needle: &str) -> Option<NaiveDate> {
    let parts = split_segments(needle.trim())?;
    if parts.len() != 2 && parts.len() != 3 {
        return None;
    }

    let (year, month, day) = if parts.len() == 3 {
        (
            i32::try_from(number(parts[0])?).ok()?,
            number(parts[1])?,
            number(parts[2])?,
        )
    } else if parts[0].len() == 4 {
        // 年份写在前面：`2025-09` 指这个月的 1 号
        (i32::try_from(number(parts[0])?).ok()?, number(parts[1])?, 1)
    } else {
        // 只写了月日：按今天所在的年份算
        (
            Local::now().date_naive().year(),
            number(parts[0])?,
            number(parts[1])?,
        )
    };
    NaiveDate::from_ymd_opt(year, month, day)
}

/// 把查询拆成两段或三段。
///
/// ASCII 分隔符 `-` / `/` / `.` 只认同一种；中文只认 `年` / `月` 与末尾的 `日`。
/// 两边不许混用，拆出来不是 2~3 段也交给调用方去判。
fn split_segments(text: &str) -> Option<Vec<&str>> {
    if text.contains(['年', '月', '日']) {
        if text.contains(['-', '/', '.']) {
            return None;
        }
        let text = text.strip_suffix('日').unwrap_or(text);
        let mut parts: Vec<&str> = text.split(['年', '月']).collect();
        // 末尾的分隔符会多切出一个空段（`2025年9月`），它不算一段
        if parts.last() == Some(&"") {
            parts.pop();
        }
        return Some(parts);
    }

    let mut parts = None;
    for separator in ['-', '/', '.'] {
        if text.contains(separator) {
            if parts.is_some() {
                // 两种 ASCII 分隔符混着写
                return None;
            }
            parts = Some(text.split(separator).collect());
        }
    }
    parts
}

/// 只收非空的 ASCII 数字串：`09` 收，`+9`、`九`、空串都不收
fn number(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// 摘要的一段：折成一行，被截断的那端补 `…`。
///
/// 折行是为了让摘要永远是一行 `Label`：`\n` 会把卡片那一行撑成两行高。
fn excerpt(chars: &[char], cut_before: bool, cut_after: bool) -> String {
    let mut out = String::new();
    if cut_before {
        out.push('…');
    }
    out.extend(chars.iter().map(|ch| match ch {
        '\n' | '\r' | '\t' => ' ',
        other => *other,
    }));
    if cut_after {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2025, 9, day).unwrap()
    }

    #[test]
    fn a_hit_points_at_character_indices() {
        let found = hit(date(17), "今天喝了两杯咖啡", "咖啡").expect("该命中");
        assert_eq!(found.at, 6, "下标按字符算，不是字节");
        assert_eq!(found.char_range(), 6..8);
        assert_eq!(found.matched, "咖啡");
        assert_eq!(found.before, "今天喝了两杯");
        assert_eq!(found.after, "");
    }

    #[test]
    fn ascii_case_is_ignored_and_the_match_keeps_its_case() {
        let found = hit(date(17), "Rust 与 rust 都写", "RUST").expect("该命中");
        assert_eq!(found.at, 0);
        assert_eq!(found.matched, "Rust", "摘要给命中的原文");

        assert!(hit(date(17), "咖啡", "咖非").is_none(), "中文按原样比");
    }

    #[test]
    fn only_the_first_hit_in_a_day_is_taken() {
        let found = hit(date(17), "先咖啡后来还是咖啡", "咖啡").expect("该命中");
        assert_eq!(found.at, 1);
        assert_eq!(found.after, "后来还是咖啡", "后面的命中不进摘要");
    }

    #[test]
    fn the_excerpt_folds_newlines_and_marks_the_cut_ends() {
        let text = format!("{}\n命中\n{}", "前".repeat(30), "后".repeat(50));
        let found = hit(date(17), &text, "命中").expect("该命中");

        assert!(
            found.before.starts_with('…'),
            "前面截断了：{:?}",
            found.before
        );
        assert!(found.after.ends_with('…'), "后面截断了：{:?}", found.after);
        for part in [&found.before, &found.after] {
            assert!(!part.contains('\n'), "摘要该折成一行：{part:?}");
        }
        assert_eq!(found.before.chars().count(), EXCERPT_BEFORE + 1);
        assert_eq!(found.after.chars().count(), EXCERPT_AFTER + 1);
        assert_eq!(found.matched, "命中", "命中的原文不截断");
    }

    #[test]
    fn a_short_text_needs_no_ellipsis() {
        let found = hit(date(17), "写了咖啡", "咖啡").expect("该命中");
        assert_eq!(found.before, "写了");
        assert_eq!(found.after, "");
        assert!(!found.before.contains('…') && !found.after.contains('…'));
    }

    #[test]
    fn an_empty_or_too_long_needle_never_matches() {
        assert!(hit(date(17), "写了点东西", "").is_none());
        assert!(hit(date(17), "写了点东西", "   ").is_none());
        assert!(hit(date(17), "短", "太长太长").is_none());
        assert!(hit(date(17), "", "字").is_none());
        // 查询两端的空白不算内容
        assert_eq!(
            hit(date(17), "喝了咖啡", " 咖啡 ").map(|found| found.matched),
            Some("咖啡".to_owned())
        );
    }

    #[test]
    fn a_match_at_the_far_end_is_found() {
        // 滑动窗要一直吞到末尾才满：这一步容易把“窗满”与“窗够长”判错
        let found = hit(date(17), "前面写了很多字咖啡", "咖啡").expect("该命中末尾");
        assert_eq!(found.at, 7, "命中在最后一处，下标按字符算");
        assert_eq!(found.char_range(), 7..9);
        assert_eq!(found.matched, "咖啡");

        // 查询与正文一样长
        let whole = hit(date(17), "咖啡", "咖啡").expect("整篇就是命中");
        assert_eq!(whole.at, 0);
        assert_eq!(whole.char_range(), 0..2);
    }

    /// 命中就在第 0 个字符上：`index + 1 - len` 在这里最小，
    /// 不能下溢（debug 下会 panic）
    #[test]
    fn a_hit_at_the_very_start_does_not_underflow() {
        for (text, needle) in [
            ("咖啡", "咖啡"),
            ("咖啡真好", "咖啡"),
            ("ab", "ab"),
            ("a", "a"),
        ] {
            let found = hit(date(17), text, needle).unwrap_or_else(|| panic!("{text}/{needle}"));
            assert_eq!(found.at, 0, "{text}/{needle}");
        }
    }

    #[test]
    fn a_match_shorter_than_the_window_does_not_leak() {
        // 正文比查询短：窗永远不满，不该有命中
        assert!(hit(date(17), "咖", "咖啡").is_none());
        // 中间断了一处：只有完整对上才算
        assert!(hit(date(17), "咖X啡", "咖啡").is_none());
    }

    #[test]
    fn three_segment_dates_are_recognized() {
        for text in [
            "2025-09-17",
            "2025/9/17",
            "2025.9.17",
            "2025年9月17日",
            "2025年9月17",
        ] {
            assert_eq!(date_query(text), Some(date(17)), "{text}");
        }
        assert_eq!(
            date_query("  2025-09-17  "),
            Some(date(17)),
            "两头空白不算内容"
        );
    }

    #[test]
    fn a_two_segment_query_with_a_four_digit_year_is_the_first_of_that_month() {
        assert_eq!(date_query("2025-09"), Some(date(1)));
        assert_eq!(date_query("2025年9月"), Some(date(1)));
        assert_eq!(date_query("2025.09"), Some(date(1)), "只用点号分隔也认");
    }

    #[test]
    fn a_two_segment_query_without_a_year_uses_the_current_year() {
        let today = Local::now().date_naive();
        let expected = NaiveDate::from_ymd_opt(today.year(), 9, 17).unwrap();
        for text in ["9-17", "9/17", "9.17", "9月17日", "9月17"] {
            assert_eq!(date_query(text), Some(expected), "{text}");
        }
    }

    #[test]
    fn single_segment_and_broken_dates_are_not_dates() {
        for text in [
            "2025",
            "0917",
            "17",
            "abc",
            "2025-02-30",
            "",
            "   ",
            "2025-",
            "9-",
            "-9-17",
            "2025-09-17-01",
            "2025年",
            "9月",
            "2025年13月",
            "2025-9.17",
            "2025年9-17",
            "17日",
        ] {
            assert_eq!(date_query(text), None, "{text}");
        }
    }
}
