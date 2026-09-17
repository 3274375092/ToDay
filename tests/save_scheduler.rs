//! 落盘策略的验收：什么时候写、写几条、写失败了怎么办。
//!
//! 策略本身是纯逻辑（[`SaveScheduler`]），所以这里不用显示器也不用数据库。

use ToDay::app::{SAVE_DEBOUNCE_SECONDS, SaveScheduler};
use ToDay::model::Change;
use chrono::NaiveDate;

fn date(day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2025, 9, day).unwrap()
}

#[test]
fn nothing_pending_means_nothing_to_write() {
    let mut scheduler = SaveScheduler::default();
    assert!(scheduler.is_empty());
    assert!(!scheduler.is_due(100.0), "没东西可写就不该到点");
    assert_eq!(scheduler.take(), Vec::new());
}

#[test]
fn an_edit_waits_for_the_debounce() {
    let mut scheduler = SaveScheduler::default();
    scheduler.record(Change::set(date(1), "写了一句"), 10.0);

    assert!(!scheduler.is_due(10.0), "刚打完字不该立刻写");
    assert!(
        !scheduler.is_due(10.0 + SAVE_DEBOUNCE_SECONDS - 0.01),
        "差一点点也还不写"
    );
    assert!(
        scheduler.is_due(10.0 + SAVE_DEBOUNCE_SECONDS),
        "安静够久就该写"
    );
}

#[test]
fn the_remaining_wait_counts_down() {
    let mut scheduler = SaveScheduler::default();
    scheduler.record(Change::set(date(1), "写了一句"), 10.0);

    assert!((scheduler.time_until_due(10.1) - (SAVE_DEBOUNCE_SECONDS - 0.1)).abs() < 1e-9);
    assert_eq!(scheduler.time_until_due(99.0), 0.0, "已经到点就不要再等");
}

#[test]
fn typing_again_pushes_the_deadline_back() {
    let mut scheduler = SaveScheduler::default();
    scheduler.record(Change::set(date(1), "第一句"), 10.0);
    scheduler.record(Change::set(date(1), "第二句"), 10.3);

    assert!(!scheduler.is_due(10.4), "又敲了一个字，重新计时");
    assert!(scheduler.is_due(10.8));
}

#[test]
fn one_day_keeps_only_the_last_edit() {
    let mut scheduler = SaveScheduler::default();
    scheduler.record(Change::set(date(1), "第一版"), 10.0);
    scheduler.record(Change::set(date(1), "第二版"), 10.1);
    scheduler.record(Change::delete(date(1)), 10.2);

    assert_eq!(scheduler.take(), vec![Change::delete(date(1))]);
    assert!(scheduler.is_empty(), "取走之后就空了");
    assert!(!scheduler.is_due(999.0), "取走之后就该安静下来");
}

#[test]
fn blank_text_comes_out_as_a_deletion() {
    let mut scheduler = SaveScheduler::default();
    // 界面上把一行清空，落盘时应该是删除，而不是写一条空正文
    scheduler.record(Change::set(date(2), "   \n "), 10.0);

    assert_eq!(scheduler.take(), vec![Change::delete(date(2))]);
}

#[test]
fn several_days_come_out_sorted_by_date() {
    let mut scheduler = SaveScheduler::default();
    scheduler.record(Change::set(date(3), "三号"), 10.0);
    scheduler.record(Change::set(date(1), "一号"), 10.1);

    assert_eq!(
        scheduler.take(),
        vec![Change::set(date(1), "一号"), Change::set(date(3), "三号")]
    );
}

#[test]
fn a_failed_write_keeps_the_edits_and_stops_retrying_by_itself() {
    let mut scheduler = SaveScheduler::default();
    scheduler.record(Change::set(date(1), "会写失败"), 10.0);

    let taken = scheduler.take();
    scheduler.requeue(taken);

    assert!(!scheduler.is_empty(), "写失败的东西要留在队里");
    assert!(
        !scheduler.is_due(999.0),
        "磁盘坏了不能每 0.4 秒忙转一次，等下一次明确动作再试"
    );

    // 再敲一个字就重新开始计时
    scheduler.record(Change::set(date(1), "又写了一点"), 20.0);
    assert!(scheduler.is_due(20.5));
}

#[test]
fn a_requeued_edit_does_not_overwrite_a_newer_one() {
    let mut scheduler = SaveScheduler::default();
    scheduler.record(Change::set(date(1), "旧版"), 10.0);
    let taken = scheduler.take();

    // 写盘失败期间，用户又改了同一天
    scheduler.record(Change::set(date(1), "新版"), 11.0);
    scheduler.requeue(taken);

    assert_eq!(scheduler.take(), vec![Change::set(date(1), "新版")]);
}

/// 排帧的判据：写失败之后不再排帧，否则界面会以 100 fps 空转。
#[test]
fn only_an_edit_that_can_still_be_written_asks_for_the_next_frame() {
    let mut scheduler = SaveScheduler::default();
    assert!(!scheduler.needs_wakeup(), "空队列排帧也没用");
    assert!(!scheduler.is_stuck(), "没写过就谈不上卡住");

    scheduler.record(Change::set(date(1), "写了一句"), 10.0);
    assert!(scheduler.needs_wakeup(), "攒着东西要排帧等到点");
    assert!(!scheduler.is_stuck());

    let taken = scheduler.take();
    scheduler.requeue(taken);
    assert!(scheduler.is_stuck(), "上次写失败了，编辑还压在队里");
    assert!(!scheduler.needs_wakeup(), "卡住时排帧也不会写，只会空转");

    // 再敲一个字：重试的机会来了，重新值得排帧
    scheduler.record(Change::set(date(1), "又写了一点"), 20.0);
    assert!(!scheduler.is_stuck());
    assert!(scheduler.needs_wakeup());

    // 写成功取走之后回到安静
    scheduler.take();
    assert!(!scheduler.is_stuck());
    assert!(!scheduler.needs_wakeup());
}
