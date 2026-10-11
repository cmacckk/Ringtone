//! 中国法定节假日（含调休补班）。
//!
//! 为什么内置一张表而不是算：放假安排是**国务院每年发文定的**（连哪天补班都是行政决定），
//! 没有任何日历算法能推出来。表里的数据来自国务院办公厅《关于 2026 年部分节假日安排的通知》
//! （2025-11-04 发布）。
//!
//! **每年 11 月要来这里加下一年**（新通知通常 11 月初发布）。没有数据的年份会退化成
//! 「周一至周五算工作日」，也就是**不认那一年的节假日**——不会崩，但会多响几次。
//! 界面上「工作日」重复与上下班提醒都走这里。

use chrono::Datelike;
use serde::Serialize;

/// 一年的放假安排。
struct Year {
    year: i32,
    /// 放假（不上班）的日子
    off: &'static [&'static str],
    /// 调休补班的日子（通常是周六或周日，但要上班）
    work: &'static [&'static str],
}

/// 表里有哪些年份（给界面提示用）。
const YEARS: &[Year] = &[Year {
    year: 2026,
    // 元旦 1/1–1/3；春节 2/15–2/23；清明 4/4–4/6；劳动节 5/1–5/5；
    // 端午 6/19–6/21；中秋 9/25–9/27；国庆 10/1–10/7
    off: &[
        "2026-01-01",
        "2026-01-02",
        "2026-01-03",
        "2026-02-15",
        "2026-02-16",
        "2026-02-17",
        "2026-02-18",
        "2026-02-19",
        "2026-02-20",
        "2026-02-21",
        "2026-02-22",
        "2026-02-23",
        "2026-04-04",
        "2026-04-05",
        "2026-04-06",
        "2026-05-01",
        "2026-05-02",
        "2026-05-03",
        "2026-05-04",
        "2026-05-05",
        "2026-06-19",
        "2026-06-20",
        "2026-06-21",
        "2026-09-25",
        "2026-09-26",
        "2026-09-27",
        "2026-10-01",
        "2026-10-02",
        "2026-10-03",
        "2026-10-04",
        "2026-10-05",
        "2026-10-06",
        "2026-10-07",
    ],
    // 1/4（周日）、2/14（周六）、2/28（周六）、5/9（周六）、9/20（周日）、10/10（周六）
    work: &[
        "2026-01-04",
        "2026-02-14",
        "2026-02-28",
        "2026-05-09",
        "2026-09-20",
        "2026-10-10",
    ],
}];

fn find_year(year: i32) -> Option<&'static Year> {
    YEARS.iter().find(|y| y.year == year)
}

/// 这一天是不是法定放假日。没数据的年份一律 `false`。
pub fn is_day_off(day: &str) -> bool {
    match (crate::time::parse_day(day), day_year(day)) {
        (Some(_), Some(y)) => find_year(y).is_some_and(|t| t.off.contains(&day)),
        _ => false,
    }
}

/// 这一天是不是调休补班日（周末要上班）。
pub fn is_makeup_workday(day: &str) -> bool {
    match day_year(day) {
        Some(y) => find_year(y).is_some_and(|t| t.work.contains(&day)),
        None => false,
    }
}

/// 这一天算不算「工作日」：调休上班的周六周日**算**，法定放假的周一到周五**不算**。
///
/// 没有节假日数据的年份退化成「周一至周五」——宁可多响几次，也不要因为查不到数据就整天不响。
pub fn is_workday(day: &str) -> bool {
    if is_makeup_workday(day) {
        return true;
    }
    if is_day_off(day) {
        return false;
    }
    crate::time::parse_day(day)
        .map(|d| d.weekday().number_from_monday() <= 5)
        .unwrap_or(false)
}

/// 表里覆盖的年份是不是包含这一年（界面提示/文档用）。
pub fn has_year(year: i32) -> bool {
    find_year(year).is_some()
}

/// 覆盖到的年份，形如 `2026`（多个用 `、` 连接）。给报告与界面文案用。
pub fn covered() -> String {
    YEARS
        .iter()
        .map(|y| y.year.to_string())
        .collect::<Vec<_>>()
        .join("、")
}

/// 节假日数据的覆盖情况（界面提示用）。
///
/// 给界面看是为了说清一件事：**没有某一年的数据时，那一年的「工作日」会退化成
/// 周一至周五**（不认节假日）。不说清的话，用户会以为「跳过节假日」这个功能坏了。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HolidayInfo {
    /// 内置了哪几年的安排，形如 `2026`（多年用 `、` 连接）
    pub covered: String,
    /// 今天所在的年份有没有数据
    pub this_year: bool,
}

pub fn info() -> HolidayInfo {
    let today = crate::time::today_local();
    let year = day_year(&today).unwrap_or(0);
    HolidayInfo {
        covered: covered(),
        this_year: has_year(year),
    }
}

/// 从 `YYYY-MM-DD` 里取年份。形状不对就 `None`（调用方自己保证形状，这里只是兜底）。
fn day_year(day: &str) -> Option<i32> {
    if day.len() < 4 {
        return None;
    }
    day[0..4].parse::<i32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holiday_weekday_is_not_workday() {
        // 2026-01-01 是周四（元旦）
        assert!(is_day_off("2026-01-01"), "元旦应当算放假");
        assert!(!is_workday("2026-01-01"), "元旦不该算工作日");
        // 2026-02-16 是周一（春节假期里）
        assert!(!is_workday("2026-02-16"), "春节假期里的周一不该算工作日");
        // 2026-10-05 是周一（国庆假期里）
        assert!(!is_workday("2026-10-05"), "国庆假期里的周一不该算工作日");
    }

    #[test]
    fn makeup_weekend_is_workday() {
        for d in [
            "2026-01-04", // 周日
            "2026-02-14", // 周六
            "2026-02-28", // 周六
            "2026-05-09", // 周六
            "2026-09-20", // 周日
            "2026-10-10", // 周六
        ] {
            assert!(is_makeup_workday(d), "{d} 应当是调休补班日");
            assert!(is_workday(d), "{d} 补班应当算工作日");
        }
    }

    #[test]
    fn plain_workday_and_weekend() {
        assert!(is_workday("2026-10-08"), "普通周四应当算工作日");
        assert!(!is_workday("2026-10-11"), "普通周日不该算工作日");
        assert!(!is_day_off("2026-10-08"), "普通周四不是放假日");
    }

    #[test]
    fn unknown_year_falls_back_to_weekday() {
        assert!(!has_year(2030), "2030 没数据");
        // 2030-01-01 是周二：没有节假日数据时按「周一至周五」算，所以会算工作日（多响，不少响）
        assert!(is_workday("2030-01-01"), "没数据的年份退化成周一至周五");
        assert!(!is_workday("2030-01-05"), "没数据时周六仍不算工作日");
    }

    #[test]
    fn covered_lists_2026() {
        assert!(covered().contains("2026"), "覆盖年份里要有 2026");
    }
}
