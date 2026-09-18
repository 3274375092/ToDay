//! 输入法快照的差分：把"这次比上次多/少了哪些字"翻成注入给 `egui` 的动作。
//!
//! 为什么需要这一层：Android 上软键盘敲出来的字不走 `KeyEvent`，走输入法的
//! `InputConnection.commitText` → game-activity 的 `TextEvent`，而 winit 0.30.13 的
//! Android 后端不消费 `TextEvent`（只打一条 warn）。于是应用自己接：
//! [`eframe::App::raw_input_hook`] 每帧读一次输入法快照（读法在 `crate::android`），
//! 交给这里的 [`injections`] 比出差量，再注入成 `egui::Event`。
//!
//! 这个模块不认识 `egui`、不碰磁盘、也不依赖 Android API：输入是两段正文与一个组词区，
//! 输出是 [`Injection`] 列表，所以桌面（`cargo test` 跑的地方）也能测。
//!
//! **下标单位**：`text` 按 UTF-8 字节计，`compose` 也是字节下标——`android-activity`
//! 就是把 game-activity 给的下标按 UTF-8 长度 clamp 的。快照里的下标可能越界或落在字符
//! 中间，这里的读法是先 clamp 再向字符边界收回，不会 panic。
//!
//! **组词区**（拼音候选、日文转换中）里的字还不算上屏：输入法把它们写进 `text` 的同时用
//! `compose` 圈出范围，此时不产出注入；上屏那一刻范围消失，那些字才变成注入。
//!
//! 排帧节奏（[`next_interval`]）也在这里：上屏的字要等到应用自己排的那一帧才被读走，
//! "多久排一帧"因此也是这一段的一部分，而且它同样是平台无关的纯逻辑。

use std::time::Duration;

/// 有输入框在焦点上时，下一帧排在多久之后；`None` = 这一帧不排帧。
///
/// 只有两个答案，都是真机上量出来的：
///
/// | 情况 | 返回 | 为什么 |
/// | --- | --- | --- |
/// | 有输入框在焦点上 | `Duration::ZERO`（立刻） | 上屏的字只在应用自己画的那一帧被读走（见模块注释）；而 `egui` 的排帧请求只在"比已经排下的那次更早"时才落地，聚焦的输入框每帧都会排下 0ms 的请求（光标闪烁、注入的字），**任何非零的延迟都会被那一帧吃掉**。真机上量到过：有焦点但停手时用 50ms 档，应用仍然要等到光标闪烁（约 480ms）才画下一帧——上屏延迟就还是那个数。0ms 走的是 `outstanding` 那条路，`egui` 保证过一趟一定会回调 |
/// | 没有输入框在焦点上 | `None` | 空闲一帧都不多画（省电） |
///
/// 代价是"键盘开着、人不在敲"时也在按屏幕刷新率画（实测约 60fps，一帧画 0.5ms 上下）；
/// 换来的是"停手再敲的第一个字"也最多只等一帧——中途没有可靠的回调可以插，
/// 非零延迟与 480ms 空窗在真机上是一回事。
///
/// `has_input`（这一帧有输入事件）与 `injected`（这一帧读出了新上屏的字/退格）不进这个
/// 判断：它们只在焦点刚要丢的那一刻少画一帧，代价却是多一套"热窗口"状态——真机上量到
/// 的重点是焦点在谁身上，不是这一帧碰巧有没有输入。
#[must_use]
pub fn next_interval(focused: bool) -> Option<Duration> {
    focused.then_some(Duration::ZERO)
}

/// 一次要注入给 `egui` 的输入法动作
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Injection {
    /// 在光标处插入这段文字
    Text(String),
    /// 在光标左边删掉 `count` 个字符（`egui` 的退格一次删一个字符）
    Backspace(usize),
}

/// 拿"上一次的上屏正文"与这一次的快照比出差量。
///
/// * `prev`：上一次调用的返回值；第一次传 `""`。
/// * `text`：快照里的完整正文，含组词区。
/// * `compose`：组词区的 `(start, end)`（字节下标，`end` 不含）；`None` 表示没有组词区。
///
/// 返回这次的动作（可能是空的）与**这一次的上屏正文**（去掉组词区之后的），
/// 调用方把它存下来当下一次的 `prev`——两次调用之间输入法改了什么都靠它认出来。
///
/// 边界：`text` 是 `prev` 的延长 → 只产出新增那一段；短了 → 只产出对应个数的退格；
/// 既不是延长也不是截断（输入法改到中间、整段替换）→ 先退到公共前缀再补上。
/// 两段相等不产出任何动作。组词区越界、反向、或者切在字符中间时按能切出多少算；
/// 组词区为空、顺序反了、越界到整段之外时按"没有组词区"算。
pub fn injections(
    prev: &str,
    text: &str,
    compose: Option<(usize, usize)>,
) -> (Vec<Injection>, String) {
    let committed = committed_text(text, compose);
    let actions = diff(prev, &committed);
    (actions, committed)
}

/// 去掉组词区之后的正文：输入法还没上屏的那几个字不算。
fn committed_text(text: &str, compose: Option<(usize, usize)>) -> String {
    let Some((start, end)) = compose else {
        return text.to_owned();
    };
    let start = floor_char_boundary(text, start);
    let end = floor_char_boundary(text, end);
    if end <= start {
        return text.to_owned();
    }
    let mut committed = String::with_capacity(text.len() - (end - start));
    committed.push_str(&text[..start]);
    committed.push_str(&text[end..]);
    committed
}

/// 向下收到字符边界（快照里的下标可能落在多字节字符中间）
fn floor_char_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// 两段正文的差：先退到公共前缀，再补上新的那一段
fn diff(prev: &str, next: &str) -> Vec<Injection> {
    if prev == next {
        return Vec::new();
    }
    let common = common_prefix_len(prev, next);
    let mut actions = Vec::new();
    let removed = prev[common..].chars().count();
    if removed > 0 {
        actions.push(Injection::Backspace(removed));
    }
    if !next[common..].is_empty() {
        actions.push(Injection::Text(next[common..].to_owned()));
    }
    actions
}

/// 公共前缀的**字节**长度。返回的下标在两段里都是字符边界。
fn common_prefix_len(prev: &str, next: &str) -> usize {
    let mut len = 0;
    for (a, b) in prev.chars().zip(next.chars()) {
        if a != b {
            break;
        }
        len += a.len_utf8();
    }
    len
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 逐字追加：输入法一个字一个字上屏时，每个字只注入一次
    #[test]
    fn appending_one_char_at_a_time_injects_just_that_char() {
        let mut prev = String::new();
        let mut injected = String::new();
        for text in ["你", "你好", "你好呀"] {
            let (actions, committed) = injections(&prev, text, None);
            let expected = text.strip_prefix(&prev).expect("前缀没变").to_owned();
            assert_eq!(actions, vec![Injection::Text(expected.clone())]);
            injected.push_str(&expected);
            prev = committed;
        }
        assert_eq!(injected, "你好呀");
    }

    /// 输入法的退格：正文短了，按少了几个字符补几次退格
    #[test]
    fn a_shorter_text_becomes_backspaces() {
        let (actions, committed) = injections("你好呀", "你好", None);
        assert_eq!(actions, vec![Injection::Backspace(1)]);
        assert_eq!(committed, "你好");

        // 多字节字符按字符数算，不是字节数
        let (actions, _) = injections("你好呀", "", None);
        assert_eq!(actions, vec![Injection::Backspace(3)]);
    }

    /// 组词区里的拼音不算上屏；候选上屏那一刻才注入
    #[test]
    fn the_composing_region_stays_out_of_the_body() {
        // 拼音打在组词区里：正文没变化，一个动作都不产出
        let (actions, committed) = injections("", "nihao", Some((0, 5)));
        assert_eq!(actions, Vec::new());
        assert_eq!(committed, "");

        // 接着接着打字（组词区往后长），同样不产出
        let (actions, committed) = injections(&committed, "nihaoma", Some((0, 7)));
        assert_eq!(actions, Vec::new());
        assert_eq!(committed, "");

        // 选了候选：组词区消失，那几个字一次性注入
        let (actions, committed) = injections(&committed, "你好吗", None);
        assert_eq!(actions, vec![Injection::Text("你好吗".to_owned())]);
        assert_eq!(committed, "你好吗");
    }

    /// 改到中间：先退到公共前缀再补上，顺序是"先退后补"
    #[test]
    fn editing_in_the_middle_rewinds_to_the_common_prefix() {
        let (actions, committed) = injections("abcd", "abXYcd", None);
        assert_eq!(
            actions,
            vec![Injection::Backspace(2), Injection::Text("XYcd".to_owned())]
        );
        assert_eq!(committed, "abXYcd");

        // 整段换掉：公共前缀是空的，全删再全加
        let (actions, _) = injections("abc", "xyz", None);
        assert_eq!(
            actions,
            vec![Injection::Backspace(3), Injection::Text("xyz".to_owned())]
        );
    }

    /// 快照里的组词区可能越界或切在字符中间：不 panic，按能切出来的算
    #[test]
    fn out_of_range_compose_regions_do_not_panic() {
        // "你" 占 3 个字节：下标 1 落在字符中间，向字符边界收回，剩下的 "好" 算上屏
        let (actions, committed) = injections("", "你好", Some((1, 5)));
        assert_eq!(actions, vec![Injection::Text("好".to_owned())]);
        assert_eq!(committed, "好");

        // 越界到正文之外：整段都在组词区里
        let (actions, committed) = injections("", "你好", Some((0, 99)));
        assert_eq!(actions, Vec::new());
        assert_eq!(committed, "");

        // 顺序反了：按没有组词区算，整段都算上屏
        let (actions, committed) = injections("", "你好", Some((3, 0)));
        assert_eq!(actions, vec![Injection::Text("你好".to_owned())]);
        assert_eq!(committed, "你好");
    }

    /// 两段一样（输入法只是动了光标）不产出动作
    #[test]
    fn an_unchanged_text_injects_nothing() {
        let (actions, committed) = injections("你好", "你好", Some((3, 3)));
        assert_eq!(actions, Vec::new());
        assert_eq!(committed, "你好");
    }

    /// 排帧只看焦点：有输入框在焦点上就立刻再画一帧，没焦点一帧都不排
    #[test]
    fn the_next_frame_is_only_scheduled_while_an_editor_has_focus() {
        assert_eq!(next_interval(false), None, "没焦点：空闲一帧都不多画");
        assert_eq!(
            next_interval(true),
            Some(Duration::ZERO),
            "有焦点：立刻再画一帧（非零的延迟会被帧内的排帧请求吃掉，见函数注释）"
        );
    }
}
