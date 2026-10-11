//! 上下班提醒：把设置里配的四个时间点**实体化成隐藏任务**。
//!
//! 为什么这么做（而不是另写一条定时逻辑）：隐藏任务（`tasks.hidden = 1`）不出现在
//! 列表 / 周条 / 计数里，但调度器一视同仁——于是它白白继承了幂等去重、退避重试、
//! 渠道降级、限流闸门、提前提醒、以及用户明确要求过的「关机不补发」。
//! 自己再写一条推送路径，等于把这些逻辑抄第二遍，迟早抄出不一致。
//!
//! 四个时间点都用「每天」重复（`freq = "daily"`），所以只要建一次，
//! `ensure_occurrences` 会一直往后补未来 7 天的实例。

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::db;
use crate::time;

/// `settings` 表里的键名。
pub const SCHEDULE_KEY: &str = "work.schedule";

/// 上下班时间设置。
///
/// 用 `#[serde(default)]` 而不是给每个字段写 `default = "..."`：
/// 前端将来少传一个字段时，这里会拿到默认值而不是整段解析失败。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkSchedule {
    pub enabled: bool,
    /// `HH:MM`（24 小时制，补零）
    pub morning_in: String,
    pub morning_out: String,
    pub afternoon_in: String,
    pub afternoon_out: String,
    /// 提前几分钟提醒（0 = 准点）。上限见 `MAX_ADVANCE_MIN`。
    pub advance_min: i64,
}

/// 提前提醒的上限（分钟）= 12 小时。比这更早就是「昨天提醒明天」了，没意义。
pub const MAX_ADVANCE_MIN: i64 = 12 * 60;

impl Default for WorkSchedule {
    fn default() -> Self {
        Self {
            enabled: false,
            morning_in: "09:00".to_string(),
            morning_out: "12:00".to_string(),
            afternoon_in: "14:00".to_string(),
            afternoon_out: "18:00".to_string(),
            advance_min: 5,
        }
    }
}

/// 四个时间点在界面上的名字，顺序就是一天的先后。
/// 改这里的顺序不用改别的地方——`status()` 与 `materialize()` 都按这个顺序走。
const POINTS: [(&str, &str, &str); 4] = [
    (
        "morning_in",
        "上午上班",
        "新的一天开始了，先把今天要做的排一下。",
    ),
    ("morning_out", "上午下班", "上午收工，去吃饭休息一下。"),
    ("afternoon_in", "下午上班", "下午开始，接着干。"),
    ("afternoon_out", "下午下班", "今天辛苦了，收个尾吧。"),
];

/// 取某个时间点的字符串值。
fn point<'a>(s: &'a WorkSchedule, key: &str) -> &'a str {
    match key {
        "morning_in" => &s.morning_in,
        "morning_out" => &s.morning_out,
        "afternoon_in" => &s.afternoon_in,
        "afternoon_out" => &s.afternoon_out,
        _ => "",
    }
}

/// 把 `HH:MM` 解析成「当天过了多少毫秒」。形状卡死（补零到 5 位）：
/// `9:0` / `9：00`（中文冒号）/ `25:00` 一律拒绝——这些字符串会被直接拿去
/// 拼时间戳，宁可在这里报错，也不要静默算出一个离谱的提醒时刻。
pub fn parse_hm(raw: &str) -> Result<i64, String> {
    let s = raw.trim();
    let b = s.as_bytes();
    if b.len() != 5 || b[2] != b':' {
        return Err(format!("时间格式必须是 HH:MM（补零），收到：{s}"));
    }
    if !b
        .iter()
        .enumerate()
        .all(|(i, c)| if i == 2 { true } else { c.is_ascii_digit() })
    {
        return Err(format!("时间只能是数字，收到：{s}"));
    }
    let hh: i64 = s[0..2].parse().map_err(|_| format!("小时读不出来：{s}"))?;
    let mm: i64 = s[3..5].parse().map_err(|_| format!("分钟读不出来：{s}"))?;
    if hh > 23 || mm > 59 {
        return Err(format!("时间超出范围（00:00~23:59），收到：{s}"));
    }
    Ok((hh * 3600 + mm * 60) * 1000)
}

/// 校验一套设置。返回的字符串直接给用户看，所以要说清是哪个时间点错了。
pub fn validate(s: &WorkSchedule) -> Result<(), String> {
    for (key, label, _) in POINTS {
        let raw = point(s, key);
        let ms = parse_hm(raw).map_err(|e| format!("{label}：{e}"))?;
        // 相邻顺序检查：上午上班要早于上午下班，等等。
        // 不做「上午下班 <= 下午上班」之外的跨段检查——午休时长为 0 是合法的。
        let idx = POINTS.iter().position(|(k, _, _)| *k == key).unwrap_or(0);
        if idx > 0 {
            let prev_key = POINTS[idx - 1].0;
            let prev_label = POINTS[idx - 1].1;
            let prev_ms = parse_hm(point(s, prev_key)).map_err(|e| format!("{prev_label}：{e}"))?;
            if ms <= prev_ms {
                return Err(format!(
                    "{label}（{raw}）不能早于或等于{prev_label}（{}）",
                    point(s, prev_key)
                ));
            }
        }
    }
    if s.advance_min < 0 || s.advance_min > MAX_ADVANCE_MIN {
        return Err(format!(
            "提前提醒要在 0~{} 分钟之间，收到：{}",
            MAX_ADVANCE_MIN, s.advance_min
        ));
    }
    Ok(())
}

/// 读设置。解析不了就回默认值（**不报错**）：设置坏了不该让程序起不来。
pub fn load(conn: &Connection) -> WorkSchedule {
    db::setting_get(conn, SCHEDULE_KEY)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<WorkSchedule>(&s).ok())
        .filter(|s| validate(s).is_ok())
        .unwrap_or_default()
}

/// 保存设置，并**立刻**把隐藏任务重新建一遍。
pub fn save(conn: &Connection, s: &WorkSchedule) -> Result<(), String> {
    validate(s)?;
    let json = serde_json::to_string(s).map_err(|e| format!("设置序列化失败：{e}"))?;
    db::setting_set(conn, SCHEDULE_KEY, &json).map_err(|e| format!("保存设置失败：{e}"))?;
    materialize(conn, s)?;
    Ok(())
}

/// 按设置重建隐藏任务（先清后建）。返回建了几条。
///
/// 「先清后建」会连**已经发出去的历史实例**一起删掉——这是有意的：
/// 用户把上午上班从 09:00 改成 09:30，就不该再留着 09:00 那条旧实例。
pub fn materialize(conn: &Connection, s: &WorkSchedule) -> Result<usize, String> {
    db::delete_hidden_tasks(conn).map_err(|e| format!("清理旧的上下班提醒失败：{e}"))?;
    if !s.enabled {
        return Ok(0);
    }
    let today = time::today_local();
    // 提前提醒：`[-N*60, 0]`（N=0 时就是 `[0]`）
    let offsets = if s.advance_min > 0 {
        format!("[-{},0]", s.advance_min * 60)
    } else {
        crate::models::DEFAULT_REMIND_OFFSETS.to_string()
    };
    // 工作日重复，无结束日期。`weekdays` 现在会看国务院的放假安排（见 `holiday.rs`）：
    // 法定假日不响，调休补班的周六日照响。
    //
    // 早先这里是 `daily`（周末也响），改成 `weekdays` 是**行为变更**——
    // 「上下班提醒」对着上班族，周末响纯属噪音；但要留意：真在普通周末加班时
    // 它不会提醒，那时候把这一行改回 `daily` 就行。
    let rule = r#"{"freq":"weekdays","interval":1,"byday":[],"until":null}"#;
    let mut n = 0;
    for (key, label, body) in POINTS {
        let tod = parse_hm(point(s, key))?;
        let due = time::ms_at_day_time(&today, tod)
            .ok_or_else(|| format!("{label}：今天的日期算不出来（{today}）"))?;
        let id = db::create_hidden_task(conn, label, body, &today, due, &offsets, rule)
            .map_err(|e| format!("建「{label}」提醒失败：{e}"))?;
        // 建完立刻排实例：调度线程不会自己为「刚建的隐藏任务」补实例
        // （`ensure_occurrences` 只在启动时跑），不叫这一步就会出现
        // 「改完下班时间，今天却一条提醒都没响，重启才生效」。
        db::reset_occurrences_for_task(conn, id, time::now_ms())
            .map_err(|e| format!("排「{label}」提醒失败：{e}"))?;
        n += 1;
    }
    Ok(n)
}

/// 界面上「距离下班还有多久」需要的全部信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkStatus {
    pub enabled: bool,
    /// `before` / `morning` / `noon` / `afternoon` / `off`
    pub phase: String,
    /// 下一个时间点的名字（如「下班」）；`off` 之后为空
    pub next_label: String,
    /// 下一个时间点的时刻（毫秒）；没有则为 `None`
    pub next_ms: Option<i64>,
    /// 今天最后一个时间点（下午下班）的时刻
    pub off_ms: Option<i64>,
    /// 距离下一个时间点还有多久（毫秒）；`next_ms` 为空时为 0
    pub left_ms: i64,
}

/// 算「现在处于一天的哪一段、离下一个时间点还有多久」。
///
/// 传入 `now` 而不是内部取当前时间：测试要能造时间。
pub fn status(s: &WorkSchedule, now: i64) -> WorkStatus {
    let phase_of = |key: &str| -> String {
        match key {
            "morning_in" => "before",
            "morning_out" => "morning",
            "afternoon_in" => "noon",
            _ => "afternoon",
        }
        .to_string()
    };
    let empty = WorkStatus {
        enabled: s.enabled,
        phase: "off".to_string(),
        next_label: String::new(),
        next_ms: None,
        off_ms: None,
        left_ms: 0,
    };
    if !s.enabled {
        return empty;
    }
    let today = time::day_of_ms(now);
    let mut times: Vec<(&str, &str, i64)> = Vec::new();
    for (key, label, _) in POINTS {
        let Ok(tod) = parse_hm(point(s, key)) else {
            return empty;
        };
        let Some(ms) = time::ms_at_day_time(&today, tod) else {
            return empty;
        };
        times.push((key, label, ms));
    }
    let off_ms = times.last().map(|(_, _, ms)| *ms);
    // 找第一个还没到的时间点
    for (key, label, ms) in &times {
        if *ms > now {
            return WorkStatus {
                enabled: true,
                phase: phase_of(key),
                next_label: short_label(label),
                next_ms: Some(*ms),
                off_ms,
                left_ms: *ms - now,
            };
        }
    }
    WorkStatus {
        enabled: true,
        phase: "off".to_string(),
        next_label: String::new(),
        next_ms: None,
        off_ms,
        left_ms: 0,
    }
}

/// 「上午上班」→「上班」、「下午下班」→「下班」：界面上要的是短词。
fn short_label(label: &str) -> String {
    match label {
        "上午上班" => "上班",
        "上午下班" => "午休",
        "下午上班" => "下午上班",
        "下午下班" => "下班",
        _ => label,
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sched() -> WorkSchedule {
        WorkSchedule {
            enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn parse_hm_accepts_zero_padded() {
        assert_eq!(parse_hm("00:00").unwrap(), 0, "00:00 应该是 0");
        assert_eq!(
            parse_hm("09:30").unwrap(),
            (9 * 3600 + 30 * 60) * 1000,
            "09:30 应该是 9.5 小时"
        );
        assert_eq!(
            parse_hm("23:59").unwrap(),
            (23 * 3600 + 59 * 60) * 1000,
            "23:59 应该是一天里的倒数第二分钟"
        );
    }

    #[test]
    fn parse_hm_rejects_loose_shapes() {
        for bad in [
            "9:00", "09:0", "09：00", "09:00:00", "", "24:00", "12:60", "aa:bb",
        ] {
            assert!(
                parse_hm(bad).is_err(),
                "{bad:?} 不该被接受（形状或范围不合法）"
            );
        }
        // 前后的空白是宽容的：用户手打/粘进来常带空格，没必要为这个报错
        assert_eq!(
            parse_hm(" 09:00 ").expect("带空格的应该先 trim 再解析"),
            (9 * 3600) * 1000,
            "` 09:00 ` 应该当成 09:00"
        );
    }

    #[test]
    fn validate_checks_order() {
        let ok = sched();
        assert!(validate(&ok).is_ok(), "默认的四个时间点应该合法");

        let bad = WorkSchedule {
            morning_out: "08:00".to_string(),
            ..ok.clone()
        };
        let err = validate(&bad).expect_err("上午下班早于上午上班，应该报错");
        assert!(
            err.contains("上午下班"),
            "错误信息要指出是哪个时间点，实际：{err}"
        );
    }

    #[test]
    fn validate_rejects_huge_advance() {
        let bad = WorkSchedule {
            advance_min: MAX_ADVANCE_MIN + 1,
            ..sched()
        };
        assert!(
            validate(&bad).is_err(),
            "提前提醒超过 {} 分钟应该报错",
            MAX_ADVANCE_MIN
        );
    }

    #[test]
    fn status_walks_through_the_day() {
        let s = WorkSchedule {
            morning_in: "09:00".to_string(),
            morning_out: "12:00".to_string(),
            afternoon_in: "14:00".to_string(),
            afternoon_out: "18:00".to_string(),
            ..sched()
        };
        let day = "2026-03-02";
        let at = |hm: &str| {
            let tod = parse_hm(hm).expect("测试用的时间点应该合法");
            time::ms_at_day_time(day, tod).expect("日期应该能算出来")
        };

        let before = status(&s, at("07:30"));
        assert_eq!(before.phase, "before", "07:30 还没上班");
        assert_eq!(before.next_label, "上班", "07:30 的下一个点是上班");
        assert_eq!(
            before.left_ms,
            90 * 60 * 1000,
            "07:30 距离上班应该还有 90 分钟"
        );

        let afternoon = status(&s, at("15:00"));
        assert_eq!(afternoon.phase, "afternoon", "15:00 在下午班上");
        assert_eq!(afternoon.next_label, "下班", "15:00 的下一个点是下班");
        assert_eq!(
            afternoon.left_ms,
            3 * 3600 * 1000,
            "15:00 距离下班应该还有 3 小时"
        );
        assert_eq!(
            afternoon.off_ms,
            Some(at("18:00")),
            "off_ms 应该是下午下班那一刻"
        );

        let off = status(&s, at("20:00"));
        assert_eq!(off.phase, "off", "20:00 已经下班");
        assert_eq!(off.next_label, "", "下班之后没有下一个点");
        assert_eq!(off.left_ms, 0, "下班之后剩余时间归零");
    }

    #[test]
    fn status_disabled_is_empty() {
        let off = status(&Default::default(), 1_800_000_000_000);
        assert!(!off.enabled, "默认设置是关的");
        assert_eq!(off.next_ms, None, "没开启就不该有下一个时间点");
    }
}
