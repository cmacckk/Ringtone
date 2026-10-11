//! 时间工具。
//!
//! 全库时间字段统一用 epoch 毫秒（INTEGER），禁止把时间存成文本再去比较：
//! 一旦出现 `2026-02-01 10:00:00` 与 `2026-02-01T10:00:00Z` 混排，
//! 文本排序会把新写入的行排到最前面（`T` 0x54 > 空格 0x20）。

use chrono::{Duration, Local, NaiveDate, NaiveDateTime, TimeZone};

/// 一天的毫秒数。只用于「跨到第二天」这类整数比较，不参与时区换算。
pub const DAY_MS: i64 = 24 * 3600 * 1000;

/// 当前时间，epoch 毫秒
pub fn now_ms() -> i64 {
    Local::now().timestamp_millis()
}

/// 本地日期，格式固定 `YYYY-MM-DD`
pub fn today_local() -> String {
    day_of_ms(now_ms())
}

/// 某个 epoch 毫秒落在哪个本地日期（`YYYY-MM-DD`）。
///
/// 取不到时（理论上是时间戳越界）退回今天：宁可日期差一天，也不能让调用方拿到空值。
pub fn day_of_ms(ms: i64) -> String {
    match Local.timestamp_millis_opt(ms).single() {
        Some(dt) => dt.format("%Y-%m-%d").to_string(),
        None => Local::now().format("%Y-%m-%d").to_string(),
    }
}

/// 本地日期 → 当天 `00:00` 的 epoch 毫秒。日期非法时返回 `None`。
pub fn day_start_ms(day: &str) -> Option<i64> {
    let date = parse_day(day)?;
    local_ms(date.and_hms_opt(0, 0, 0)?)
}

/// `day` 当天、零点之后 `tod_ms` 毫秒处的 epoch 毫秒。
///
/// `tod_ms` 超过一天时按取模处理（`24:00` = 次日零点），免得调用方先算边界。
pub fn ms_at_day_time(day: &str, tod_ms: i64) -> Option<i64> {
    let date = parse_day(day)?;
    let base = date.and_hms_opt(0, 0, 0)?;
    let tod = tod_ms.rem_euclid(DAY_MS);
    local_ms(base + Duration::milliseconds(tod))
}

/// 某个 epoch 毫秒在它自己那天里的偏移（当天零点起算的毫秒）。
///
/// 用「减去当天零点」而不是直接取 UTC 时分：UTC 换算在晚上 8 点后会切到第二天。
pub fn tod_ms(ms: i64) -> i64 {
    match day_start_ms(&day_of_ms(ms)) {
        Some(start) => ms - start,
        None => 0,
    }
}

/// 日期加减天数。跨月、跨年、闰日都由 chrono 算，`None` = 日期非法或越界。
pub fn shift_day(day: &str, delta_days: i64) -> Option<String> {
    let date = parse_day(day)?;
    let moved = date.checked_add_signed(Duration::days(delta_days))?;
    Some(moved.format("%Y-%m-%d").to_string())
}

/// 解析日期（已通过形状校验）。日期非法返回 `None`。
pub fn parse_day(day: &str) -> Option<NaiveDate> {
    if validate_day(day).is_err() {
        return None;
    }
    NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()
}

/// 都是本地时区，不涉及夏令时的歧义处理：本机时区里不存在的本地时刻只能放弃（返回 `None`）。
fn local_ms(naive: NaiveDateTime) -> Option<i64> {
    Local
        .from_local_datetime(&naive)
        .single()
        .map(|dt| dt.timestamp_millis())
}

/// 严格校验日期：格式必须是 `YYYY-MM-DD` 且必须是真实存在的日期。
///
/// 注意：chrono 的 `%Y-%m-%d` 对补零不严格（`2026-2-1` 也能解析成功），
/// 而 `day` 列是要参与文本比较的，长度不齐会破坏排序，所以这里先做形状校验。
pub fn validate_day(day: &str) -> Result<(), String> {
    let b = day.as_bytes();
    let shaped = b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[8..].iter().all(u8::is_ascii_digit);
    if !shaped {
        return Err(format!("日期格式必须是 YYYY-MM-DD，收到：{day}"));
    }
    NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .map(|_| ())
        .map_err(|_| format!("日期不存在：{day}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn today_matches_day_format() {
        let today = today_local();
        assert!(
            validate_day(&today).is_ok(),
            "today_local() 必须产出 YYYY-MM-DD，实际：{today}"
        );
        assert_eq!(today.len(), 10, "日期长度必须是 10，实际：{today}");
    }

    #[test]
    fn rejects_bad_day() {
        for bad in [
            "2026-2-1",
            "2026-02-1",
            "2026/02/01",
            "20260201",
            "",
            "2026-13-01",
            "2026-02-30",
            "2026-02-01 ",
            "2026-02-0a",
        ] {
            assert!(
                validate_day(bad).is_err(),
                "非法日期 {bad} 应当被拒绝，但通过了校验"
            );
        }
    }

    #[test]
    fn accepts_real_dates_including_leap_day() {
        for good in ["2024-02-29", "2026-02-28", "2026-12-31"] {
            assert!(
                validate_day(good).is_ok(),
                "合法日期 {good} 应当通过校验，实际被拒绝"
            );
        }
        assert!(
            validate_day("2026-02-29").is_err(),
            "2026 不是闰年，2026-02-29 应当被拒绝"
        );
    }

    #[test]
    fn day_helpers_roundtrip() {
        let start = day_start_ms("2026-02-01").expect("2026-02-01 应当能算出零点");
        assert_eq!(
            day_of_ms(start),
            "2026-02-01",
            "零点时间戳反解回来必须还是同一天"
        );
        assert_eq!(
            tod_ms(start),
            0,
            "当天零点的偏移必须是 0，实际 {}",
            tod_ms(start)
        );

        let nine = ms_at_day_time("2026-02-01", 9 * 3600 * 1000).expect("9 点应当能算出来");
        assert_eq!(
            day_of_ms(nine),
            "2026-02-01",
            "9 点必须落在当天（用 UTC 算时分会在晚上 8 点后跨天）"
        );
        assert!(nine > start, "9 点必须晚于当天零点");
    }

    #[test]
    fn shift_day_crosses_month_and_leap_day() {
        assert_eq!(shift_day("2026-02-28", 1).as_deref(), Some("2026-03-01"));
        assert_eq!(shift_day("2024-02-28", 1).as_deref(), Some("2024-02-29"));
        assert_eq!(shift_day("2026-01-01", -1).as_deref(), Some("2025-12-31"));
        assert_eq!(shift_day("2026-13-01", 1), None, "非法日期应当返回 None");
    }

    #[test]
    fn now_ms_is_plausible() {
        let ms = now_ms();
        // 2020-01-01 之后
        assert!(ms > 1_577_836_800_000, "now_ms() 返回值过小：{ms}");
    }
}
