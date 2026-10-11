//! 待办导入 / 导出（Markdown 与 CSV）。
//!
//! 设计上的两条硬规矩：
//!
//! 1. **导入只新增，绝不覆盖**：同一天 + 同标题（`trim` 后）已经存在就跳过。
//!    这是本地待办，用户手上那份才是真的；导入把已有条目改掉，比不导入更糟。
//! 2. **CSV 带 UTF-8 BOM**：不带的话 Excel 会把中文认成 GBK，打开就是乱码
//!    （用户导出后 90% 是用 Excel 看）。Markdown 不带 BOM，它没什么必要。
//!
//! 另外：**CSV 导出带重复规则列，能无损往返**；Markdown 里的 `↻ 每天` / `提前 5 分钟`
//! 现在也**导出写全、导入认回来**（`rule_note` ↔ `parse_rule_note`、
//! `advance_note` ↔ `parse_advance_note`），所以 Markdown 同样能往返。
//! 认不出的写法一律当成「不重复 / 不提前」——猜错比丢掉更糟。

use chrono::TimeZone;
use rusqlite::Connection;
use serde::Serialize;

use crate::db;
use crate::models::TaskInput;
use crate::time;
use crate::work::parse_hm;

/// 导入结果，直接显示给用户。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportSummary {
    pub added: usize,
    pub skipped: usize,
    /// 逐条失败原因（最多留前 20 条，免得一个坏文件刷满界面）
    pub failed: Vec<String>,
}

/// 导出成 Markdown：按天分组 + 复选框。
pub fn export_markdown(conn: &Connection) -> Result<String, String> {
    let tasks = db::all_tasks(conn).map_err(|e| e.to_string())?;
    let mut out = String::new();
    out.push_str("# Ringtone 待办导出\n\n");
    out.push_str(&format!("导出时间：{}\n\n", stamp()));
    out.push_str(
        "> 格式说明：`- [ ] HH:MM 标题`；`[x]` 是已完成。\n\
         > 后面跟的 `↻ …` 是重复说明、`提前 …` 是提前提醒，**导入时这两项会认回来**\n\
         > （认得出就照着建，认不出就当没有）。所以 Markdown 也能往返；\n\
         > 写法得是本程序导出的那几种（`每周一、三、五` 这种带顿号的），\n\
         > 自己手写花样多的请用 CSV。\n\n",
    );
    let mut cur_day = String::new();
    for t in &tasks {
        if t.day != cur_day {
            if !cur_day.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("## {}\n\n", t.day));
            cur_day = t.day.clone();
        }
        let mark = if t.done { "x" } else { " " };
        let hm = match t.due_at_ms {
            Some(ms) => format!("{} ", hm_of(ms)),
            None => String::new(),
        };
        out.push_str(&format!("- [{mark}] {hm}{}", t.title));
        if let Some(rule) = t.repeat_rule.as_deref().filter(|r| !r.is_empty()) {
            let note = rule_note(rule);
            // 认不出的脏规则不写「 · ↻ 」，免得给导入端留一句猜不出的空说明
            if !note.is_empty() {
                out.push_str(&format!(" · ↻ {note}"));
            }
        }
        let adv = advance_note(&t.remind_offsets);
        if !adv.is_empty() {
            out.push_str(&format!(" · {adv}"));
        }
        out.push('\n');
    }
    if tasks.is_empty() {
        out.push_str("（还没有任何待办）\n");
    }
    Ok(out)
}

/// 导出成 CSV。列固定，第一列是日期，方便在 Excel 里排序。
pub fn export_csv(conn: &Connection) -> Result<String, String> {
    let tasks = db::all_tasks(conn).map_err(|e| e.to_string())?;
    // BOM：Excel 靠它认出 UTF-8
    let mut out = String::from("\u{feff}");
    out.push_str("day,done,time,title,repeat_rule,remind_offsets\n");
    for t in &tasks {
        let time_col = t.due_at_ms.map(hm_of).unwrap_or_default();
        out.push_str(&csv_field(&t.day));
        out.push(',');
        out.push_str(if t.done { "1" } else { "0" });
        out.push(',');
        out.push_str(&csv_field(&time_col));
        out.push(',');
        out.push_str(&csv_field(&t.title));
        out.push(',');
        out.push_str(&csv_field(t.repeat_rule.as_deref().unwrap_or("")));
        out.push(',');
        out.push_str(&csv_field(&t.remind_offsets));
        out.push('\n');
    }
    Ok(out)
}

/// 按文件名后缀选解析器。`filename` 只用来判类型，不读文件（调用方已经把内容读进来了）。
pub fn import(conn: &Connection, filename: &str, text: &str) -> Result<ImportSummary, String> {
    let lower = filename.to_ascii_lowercase();
    let drafts = if lower.ends_with(".csv") {
        parse_csv(text)?
    } else if lower.ends_with(".md") || lower.ends_with(".markdown") || lower.ends_with(".txt") {
        parse_markdown(text)
    } else {
        return Err(format!(
            "只认 .md / .markdown / .csv / .txt，收到：{filename}（可以先另存成这些格式再导入）"
        ));
    };
    if drafts.is_empty() {
        return Err(format!(
            "在 {filename} 里没找到任何待办。Markdown 要写成 `- [ ] 09:00 标题` 并放在 `## 2026-10-09` 这样的日期下面；CSV 要有 day/title 两列。"
        ));
    }

    let mut summary = ImportSummary {
        added: 0,
        skipped: 0,
        failed: Vec::new(),
    };
    for d in drafts {
        if let Err(e) = time::validate_day(&d.day) {
            push_failure(&mut summary, format!("第 {} 行：{e}", d.line));
            continue;
        }
        // 同一天 + 同标题就算重复（前后空格不算差异）
        let exists = db::tasks_on_day(conn, &d.day)
            .map(|list| {
                list.iter()
                    .any(|t| t.title.trim() == d.title.trim() && !t.repeated)
            })
            .unwrap_or(false);
        if exists {
            summary.skipped += 1;
            continue;
        }
        let due_at_ms = match d.hm.as_deref() {
            Some(hm) => match parse_hm(hm) {
                Ok(tod) => time::ms_at_day_time(&d.day, tod),
                Err(e) => {
                    push_failure(&mut summary, format!("第 {} 行：{e}", d.line));
                    continue;
                }
            },
            None => None,
        };
        let input = TaskInput {
            title: d.title.clone(),
            body: String::new(),
            day: d.day.clone(),
            due_at_ms,
            repeat_rule: d.repeat_rule.clone(),
            remind_offsets: d.remind_offsets.clone(),
            notify_channels: None,
        };
        // `validated()` 会消耗 `input`（回的是清洗过的新值），所以这里重新绑一下
        let input = match input.validated() {
            Ok(v) => v,
            Err(msg) => {
                push_failure(&mut summary, format!("第 {} 行：{msg}", d.line));
                continue;
            }
        };
        match db::create_with_occurrences(conn, &input) {
            Ok(task) => {
                if d.done {
                    // 导入的「已完成」要落到库里，否则用户会以为勾没导进来
                    if let Err(e) = db::set_done(conn, task.id, true) {
                        push_failure(
                            &mut summary,
                            format!("第 {} 行：建好了但勾不上完成：{e}", d.line),
                        );
                    }
                }
                summary.added += 1;
            }
            Err(e) => push_failure(&mut summary, format!("第 {} 行：{e}", d.line)),
        }
    }
    Ok(summary)
}

fn push_failure(s: &mut ImportSummary, msg: String) {
    if s.failed.len() < 20 {
        s.failed.push(msg);
    }
}

/// 导入中间态。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Draft {
    /// 文件里的行号（1 开始），报错时指给用户看
    line: usize,
    day: String,
    /// `HH:MM`；没有具体时刻则为 `None`
    hm: Option<String>,
    title: String,
    done: bool,
    repeat_rule: Option<String>,
    remind_offsets: Option<String>,
}

/// 解析 Markdown。
///
/// 认的形状（尽量宽松，用户手写的也算）：
/// ```text
/// ## 2026-10-09            ← 日期标题（后面带星期几也行）
/// - [ ] 09:30 交周报
/// - [x] 买咖啡豆            ← 没写时间 = 当天不定时刻
/// ```
fn parse_markdown(text: &str) -> Vec<Draft> {
    let mut out = Vec::new();
    let mut day = String::new();
    for (idx, raw) in text.lines().enumerate() {
        let line = raw.trim();
        let no = idx + 1;
        // 日期标题：`## 2026-10-09`、`### 2026-10-09 周五`、`**2026-10-09**`
        if let Some(d) = heading_day(line) {
            day = d;
            continue;
        }
        // 复选框行：`- [ ] …` / `* [x] …` / `- [X] …`
        let rest = match checkbox_body(line) {
            Some(r) => r,
            None => continue,
        };
        if day.is_empty() {
            // 没写日期标题就不能猜（猜错会插到别人那一天）
            out.push(Draft {
                line: no,
                day: String::new(),
                hm: None,
                title: rest.to_string(),
                done: line.contains("[x]") || line.contains("[X]"),
                repeat_rule: None,
                remind_offsets: None,
            });
            continue;
        }
        // 时间前缀：`09:30 ` 或 `9:30 `（导出是补零的，手写常见的没补）
        let (hm, body) = split_hm(rest);
        // `strip_notes` 切下来的那句「 · ↻ 每天 · 提前 5 分钟」早先被丢掉了，
        // 于是 Markdown 导出再导入会悄悄丢掉重复规则与提前提醒。现在认回来。
        let (title, note) = strip_notes(body);
        out.push(Draft {
            line: no,
            day: day.clone(),
            hm,
            title: title.trim().to_string(),
            done: line.contains("[x]") || line.contains("[X]"),
            repeat_rule: parse_rule_note(&note),
            remind_offsets: parse_advance_note(&note),
        });
    }
    out.retain(|d| !d.title.trim().is_empty());
    out
}

/// 解析 CSV。列名认 `day/date/日期`、`title/标题`、`time/时间`、`done/完成`、
/// `repeat_rule`、`remind_offsets`——按表头找列，所以列的先后顺序无所谓。
fn parse_csv(text: &str) -> Result<Vec<Draft>, String> {
    let rows: Vec<Vec<String>> = text
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty())
        .map(parse_csv_line)
        .collect();
    if rows.is_empty() {
        return Err("CSV 是空的".to_string());
    }
    let head: Vec<String> = rows[0].iter().map(|c| c.trim().to_lowercase()).collect();
    let idx_of = |names: &[&str]| -> Option<usize> {
        head.iter()
            .position(|h| names.iter().any(|n| h == n || h.contains(n)))
    };
    let i_day = idx_of(&["day", "date", "日期"])
        .ok_or_else(|| "CSV 里找不到日期列（表头要有 day 或 日期）".to_string())?;
    let i_title = idx_of(&["title", "标题", "任务"])
        .ok_or_else(|| "CSV 里找不到标题列（表头要有 title 或 标题）".to_string())?;
    let i_time = idx_of(&["time", "时间"]);
    let i_done = idx_of(&["done", "完成"]);
    let i_rule = idx_of(&["repeat", "重复"]);
    let i_off = idx_of(&["remind", "提醒"]);

    let mut out = Vec::new();
    for (n, row) in rows.iter().enumerate().skip(1) {
        let cell = |i: Option<usize>| -> String {
            i.and_then(|i| row.get(i)).cloned().unwrap_or_default()
        };
        let title = cell(Some(i_title)).trim().to_string();
        if title.is_empty() {
            continue;
        }
        let done_raw = cell(i_done);
        let hm_raw = cell(i_time).trim().to_string();
        let rule_raw = cell(i_rule).trim().to_string();
        let off_raw = cell(i_off).trim().to_string();
        out.push(Draft {
            line: n + 1,
            day: cell(Some(i_day)).trim().to_string(),
            // `9:30` 补成 `09:30`，好走统一的解析
            hm: if hm_raw.is_empty() {
                None
            } else {
                Some(pad_hm(&hm_raw))
            },
            title,
            done: matches!(
                done_raw.trim().to_lowercase().as_str(),
                "1" | "true" | "yes" | "是" | "已完成"
            ),
            repeat_rule: if rule_raw.is_empty() {
                None
            } else {
                Some(rule_raw)
            },
            remind_offsets: if off_raw.is_empty() {
                None
            } else {
                Some(off_raw)
            },
        });
    }
    Ok(out)
}

/// 一行 CSV → 各列。只处理「引号包裹 + `""` 转义」这套，够用：
/// 导出的标题里如果有换行，导出时就被换成空格了（见 `csv_field`）。
fn parse_csv_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if in_quotes => {
                if chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            }
            '"' => in_quotes = true,
            ',' if !in_quotes => {
                out.push(cur.clone());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// 一个字段的 CSV 写法：有逗号/引号/换行就加引号。
/// 换行被换成空格——**这是为了让导入不用处理跨行字段**，代价是标题里的换行丢了。
fn csv_field(s: &str) -> String {
    let flat = s.replace(['\n', '\r'], " ");
    if flat.contains(',') || flat.contains('"') {
        format!("\"{}\"", flat.replace('"', "\"\""))
    } else {
        flat
    }
}

/// `## 2026-10-09 周五` → `2026-10-09`；不是日期标题就返回 `None`。
fn heading_day(line: &str) -> Option<String> {
    let t = line.trim_start_matches('#').trim_start_matches('*').trim();
    let t = t.trim_end_matches('*').trim();
    let mut parts = t.split_whitespace();
    let first = parts.next()?;
    if is_day_like(first) {
        Some(first.to_string())
    } else {
        None
    }
}

/// `- [ ] xxx` → `xxx`
fn checkbox_body(line: &str) -> Option<&str> {
    let t = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .or_else(|| line.strip_prefix("+ "))?;
    let t = t.trim_start();
    for mark in ["[ ]", "[x]", "[X]"] {
        if let Some(rest) = t.strip_prefix(mark) {
            return Some(rest.trim());
        }
    }
    None
}

/// `09:30 交周报` → `("09:30", "交周报")`；开头不是时间就返回 `(None, 原文)`。
fn split_hm(rest: &str) -> (Option<String>, &str) {
    let s = rest.trim_start();
    let b = s.as_bytes();
    if b.len() >= 5 && b[2] == b':' {
        let hh = &s[0..2];
        let mm = &s[3..5];
        if hh.chars().all(|c| c.is_ascii_digit()) && mm.chars().all(|c| c.is_ascii_digit()) {
            let after = &s[5..];
            if after.is_empty() || after.starts_with(' ') {
                return (Some(format!("{hh}:{mm}")), after.trim_start());
            }
        }
    }
    (None, s)
}

/// 切掉导出时加的 ` · ↻ 每天` / ` · 提前 5 分钟` 尾巴。
fn strip_notes(body: &str) -> (String, String) {
    let mut title = body.to_string();
    let mut notes = Vec::new();
    let mut cut = title.len();
    for (i, _) in body.match_indices(" · ") {
        let tail = &body[i..];
        if tail.contains("↻") || tail.contains("提前") {
            cut = i;
            notes.push(body[i..].to_string());
            break;
        }
    }
    title.truncate(cut);
    (title, notes.join(""))
}

/// `2026-10-9` → `2026-10-09`（补零）；不是日期形状就原样返回，交给 `validate_day` 报错。
fn is_day_like(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

/// `9:30` → `09:30`（CSV 里手写的时间可能没补零）。
fn pad_hm(raw: &str) -> String {
    let s = raw.trim();
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() == 2 {
        if let (Ok(h), Ok(m)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
            return format!("{h:02}:{m:02}");
        }
    }
    s.to_string()
}

/// 任务里的毫秒时间戳 → `HH:MM`（本地时区）。
fn hm_of(ms: i64) -> String {
    let tod = time::tod_ms(ms);
    let mins = tod / 60_000;
    format!("{:02}:{:02}", mins / 60, mins % 60)
}

/// 重复规则的人类可读说明（导出用；导入靠 [`parse_rule_note`] 反解）。
///
/// 必须把 `interval` 与 `until` 都写出来：「每 2 周的周一、三、五」只写
/// 「每周一、三、五」的话，导出再导入就从「隔周」变成「每周」；「到 12-31 为止」
/// 丢了就是一直重复下去。之前正是这样丢的。
fn rule_note(raw: &str) -> String {
    const NAMES: [&str; 7] = ["一", "二", "三", "四", "五", "六", "日"];
    let r = match crate::models::parse_repeat(raw) {
        Some(r) => r,
        // 脏数据不编说明：写一句「重复」反而会让导入端以为有规则
        None => return String::new(),
    };
    let mut s = match r.freq.as_str() {
        "daily" if r.interval <= 1 => "每天".to_string(),
        "daily" => format!("每 {} 天", r.interval),
        "weekdays" => "工作日".to_string(),
        "weekly" => {
            let days: Vec<&str> = r
                .byday
                .iter()
                .filter_map(|d| NAMES.get((*d - 1).max(0) as usize).copied())
                .collect();
            if days.is_empty() {
                // 没指定星期几 = 跟着任务自己那天（`matches_day` 的语义）
                if r.interval <= 1 {
                    "每周".to_string()
                } else {
                    format!("每 {} 周", r.interval)
                }
            } else if r.interval <= 1 {
                format!("每周{}", days.join("、"))
            } else {
                // 「每 2 周的周一、三、五」：每个星期都要带「周」字，不然读起来像
                // 「每两周的一、三、五」（那是什么意思？）
                let with_week: Vec<String> = days.iter().map(|d| format!("周{d}")).collect();
                format!("每 {} 周的{}", r.interval, with_week.join("、"))
            }
        }
        "monthly" if r.interval <= 1 => "每月".to_string(),
        "monthly" => format!("每 {} 个月", r.interval),
        "last_workday" if r.interval <= 1 => "每月最后一个工作日".to_string(),
        "last_workday" => format!("每 {} 个月的最后一个工作日", r.interval),
        "yearly" if r.interval <= 1 => "每年".to_string(),
        "yearly" => format!("每 {} 年", r.interval),
        _ => return String::new(),
    };
    if let Some(u) = r.until.as_deref().filter(|u| !u.is_empty()) {
        s.push_str(&format!("，到 {u}"));
    }
    s
}

/// 提前提醒的人类可读说明。只列负偏移（`0` 是到点，不用赘述）。
fn advance_note(offsets: &str) -> String {
    let list = crate::models::parse_remind_offsets(offsets);
    let mut parts = Vec::new();
    for o in list.iter().filter(|o| **o < 0) {
        let secs = -o;
        if secs % 86_400 == 0 {
            parts.push(format!("提前 {} 天", secs / 86_400));
        } else if secs % 3600 == 0 {
            parts.push(format!("提前 {} 小时", secs / 3600));
        } else {
            parts.push(format!("提前 {} 分钟", secs / 60));
        }
    }
    parts.join("、")
}

/// 从说明尾巴里反解重复规则（`↻ 每 2 周的周一、三、五，到 2026-12-31`）。
///
/// 认不出就返回 `None`（= 不重复）：宁可丢一条重复规则，
/// 也不能猜错把「每月」导成「每天」。
fn parse_rule_note(note: &str) -> Option<String> {
    let seg = note.split('↻').nth(1)?;
    let seg = seg.split(" · ").next()?.trim();
    let (body, until) = match seg.split_once("，到 ") {
        Some((b, u)) => {
            let u = u.trim();
            // 结束日期形状不对就别认这条规则（`validated()` 也会卡，但这里先挡住）
            if !is_day_like(u) {
                return None;
            }
            (b.trim(), Some(u))
        }
        None => (seg, None),
    };
    let (freq, interval, byday) = match body {
        "每天" => ("daily", 1, Vec::new()),
        "工作日" => ("weekdays", 1, Vec::new()),
        "每周" => ("weekly", 1, Vec::new()),
        "每月" => ("monthly", 1, Vec::new()),
        "每月最后一个工作日" => ("last_workday", 1, Vec::new()),
        "每年" => ("yearly", 1, Vec::new()),
        b if b.starts_with("每周") => ("weekly", 1, parse_day_names(&b["每周".len()..])?),
        b if b.starts_with("每 ") => {
            let rest = &b["每 ".len()..];
            let (n, unit) = rest.split_once(' ')?;
            let n: i64 = n.parse().ok()?;
            if !(1..=crate::models::MAX_REPEAT_INTERVAL).contains(&n) {
                return None;
            }
            if let Some(days) = unit.strip_prefix('周') {
                ("weekly", n, parse_day_names(days.trim_start_matches('的'))?)
            } else if unit.starts_with('天') {
                ("daily", n, Vec::new())
            } else if unit.starts_with("个月") {
                // 「每 2 个月的最后一个工作日」与「每 2 个月」只差一个尾巴
                if unit.contains("最后一个工作日") {
                    ("last_workday", n, Vec::new())
                } else {
                    ("monthly", n, Vec::new())
                }
            } else if unit.starts_with('年') {
                ("yearly", n, Vec::new())
            } else {
                return None;
            }
        }
        _ => return None,
    };
    Some(repeat_json(freq, interval, &byday, until))
}

/// `一、三、五`（也认 `周一、周三、周五`）→ `[1, 3, 5]`；空串 → 空数组；
/// 有认不出的字就 `None`。
fn parse_day_names(s: &str) -> Option<Vec<i64>> {
    const NAMES: [&str; 7] = ["一", "二", "三", "四", "五", "六", "日"];
    let s = s.trim();
    if s.is_empty() {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    for part in s.split('、') {
        // 带不带「周」都收：「每周一、三、五」和「每 2 周的周一、三、五」两种写法都存在
        let p = part.trim().trim_start_matches('周');
        let idx = NAMES.iter().position(|n| *n == p)?;
        out.push(idx as i64 + 1);
    }
    Some(out)
}

/// 从说明尾巴里反解提前提醒（`提前 5 分钟` / `提前 1 小时、1 天`）→ `[-90000,…,0]`。
///
/// 末尾一定补一个 `0`（到点那一下），与界面「准点」选项的语义一致。
fn parse_advance_note(note: &str) -> Option<String> {
    let i = note.find("提前")?;
    let seg = note[i + "提前".len()..].split(" · ").next()?.trim();
    let mut secs: Vec<i64> = Vec::new();
    for part in seg.split('、') {
        let (n, unit) = part.trim().split_once(' ')?;
        let n: i64 = n.parse().ok()?;
        if n <= 0 {
            return None;
        }
        let unit_secs = if unit.starts_with('天') {
            86_400
        } else if unit.starts_with("小时") {
            3_600
        } else if unit.starts_with("分钟") {
            60
        } else {
            return None;
        };
        let v = -(n * unit_secs);
        if v.abs() > crate::models::MAX_REMIND_OFFSET_SECS {
            return None;
        }
        secs.push(v);
    }
    if secs.is_empty() {
        return None;
    }
    secs.sort_unstable();
    secs.dedup();
    secs.push(0);
    let list = secs
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join(",");
    Some(format!("[{list}]"))
}

/// 拼 `repeat_rule` 的 JSON。字段名与库里既有字符串一致
/// （`{"freq":…,"interval":…,"byday":[…],"until":…}`），方便肉眼比对。
fn repeat_json(freq: &str, interval: i64, byday: &[i64], until: Option<&str>) -> String {
    let days = byday
        .iter()
        .map(|d| d.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let until = match until {
        Some(u) => format!("\"{u}\""),
        None => "null".to_string(),
    };
    format!(r#"{{"freq":"{freq}","interval":{interval},"byday":[{days}],"until":{until}}}"#)
}

/// `2026-10-09 18:00`（本地时区，写进导出文件的头部）
fn stamp() -> String {
    let ms = time::now_ms();
    let dt = match chrono::Local.timestamp_millis_opt(ms) {
        chrono::LocalResult::Single(dt) => dt,
        _ => return time::day_of_ms(ms),
    };
    dt.format("%Y-%m-%d %H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::TaskInput;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().expect("内存库应该能开");
        db::migrate(&conn).expect("建表应该成功");
        conn
    }

    fn task(
        conn: &Connection,
        day: &str,
        title: &str,
        hm: Option<&str>,
        rule: Option<&str>,
    ) -> i64 {
        let input = TaskInput {
            title: title.to_string(),
            body: String::new(),
            day: day.to_string(),
            due_at_ms: hm.map(|h| {
                let tod = parse_hm(h).expect("测试用的时间应该合法");
                time::ms_at_day_time(day, tod).expect("日期应该算得出来")
            }),
            repeat_rule: rule.map(|r| r.to_string()),
            remind_offsets: None,
            notify_channels: None,
        };
        db::create_with_occurrences(conn, &input)
            .expect("造数据不该失败")
            .id
    }

    #[test]
    fn export_markdown_groups_by_day() {
        let conn = mem();
        task(&conn, "2026-03-02", "上午的活", Some("09:30"), None);
        task(&conn, "2026-03-02", "喝茶", None, None);
        task(&conn, "2026-03-03", "明天的活", Some("10:00"), None);
        let md = export_markdown(&conn).expect("导出不该失败");
        assert!(
            md.contains("## 2026-03-02"),
            "应该有 03-02 的标题，实际：\n{md}"
        );
        assert!(md.contains("## 2026-03-03"), "应该有 03-03 的标题");
        assert!(
            md.contains("- [ ] 09:30 上午的活"),
            "有时间的任务应该带 HH:MM，实际：\n{md}"
        );
        assert!(
            md.contains("- [ ] 喝茶"),
            "没时间的任务不该硬塞时间，实际：\n{md}"
        );
        let pos2 = md.find("## 2026-03-02").expect("有 03-02");
        let pos3 = md.find("## 2026-03-03").expect("有 03-03");
        assert!(pos2 < pos3, "日期要按先后排，实际：\n{md}");
    }

    #[test]
    fn export_csv_has_bom_and_escapes() {
        let conn = mem();
        task(&conn, "2026-03-02", "带,逗号 的标题", Some("09:30"), None);
        let csv = export_csv(&conn).expect("导出不该失败");
        assert!(
            csv.starts_with('\u{feff}'),
            "CSV 必须以 BOM 开头（否则 Excel 乱码）"
        );
        assert!(
            csv.contains("day,done,time,title,repeat_rule,remind_offsets"),
            "表头不对：\n{csv}"
        );
        assert!(
            csv.contains("\"带,逗号 的标题\""),
            "带逗号的字段必须加引号，实际：\n{csv}"
        );
        assert!(
            csv.contains("2026-03-02,0,09:30,"),
            "日期/完成/时间三列的顺序不对，实际：\n{csv}"
        );
    }

    #[test]
    fn markdown_round_trip() {
        let conn = mem();
        task(&conn, "2026-03-02", "交周报", Some("09:30"), None);
        let md = export_markdown(&conn).expect("导出不该失败");

        let other = mem();
        let sum = import(&other, "backup.md", &md).expect("导入应该成功");
        assert_eq!(sum.added, 1, "应该导入 1 条，实际 {sum:?}");
        let list = db::tasks_on_day(&other, "2026-03-02").expect("查得到");
        assert_eq!(list.len(), 1, "应该只有 1 条");
        assert_eq!(list[0].title, "交周报", "标题要一致");
        assert_eq!(
            hm_of(list[0].due_at_ms.expect("应该有时刻")),
            "09:30",
            "时间要一致"
        );
    }

    #[test]
    fn csv_round_trip_keeps_repeat_rule() {
        let conn = mem();
        let rule = r#"{"freq":"daily","interval":1,"byday":[],"until":null}"#;
        task(&conn, "2026-03-02", "每天喝水", Some("09:00"), Some(rule));
        let csv = export_csv(&conn).expect("导出不该失败");

        let other = mem();
        let sum = import(&other, "backup.csv", &csv).expect("导入应该成功");
        assert_eq!(sum.added, 1, "应该导入 1 条，实际 {sum:?}");
        let list = db::tasks_on_day(&other, "2026-03-02").expect("查得到");
        assert_eq!(
            list[0].repeat_rule.as_deref(),
            Some(rule),
            "CSV 往返要保住重复规则（这正是它比 Markdown 强的地方）"
        );
    }

    #[test]
    fn import_skips_same_day_same_title() {
        let conn = mem();
        task(&conn, "2026-03-02", "交周报", Some("09:30"), None);
        let md = "## 2026-03-02\n\n- [ ] 09:30 交周报\n- [ ] 10:30 新来的\n";
        let sum = import(&conn, "x.md", md).expect("导入应该成功");
        assert_eq!(sum.skipped, 1, "同名同天要跳过，实际 {sum:?}");
        assert_eq!(sum.added, 1, "新的那条应该进来，实际 {sum:?}");
        let list = db::tasks_on_day(&conn, "2026-03-02").expect("查得到");
        assert_eq!(list.len(), 2, "库里应该 2 条（没被覆盖成 3 条）");
    }

    #[test]
    fn import_marks_done_rows() {
        let conn = mem();
        let md = "## 2026-03-02\n\n- [x] 08:00 已经干完了\n";
        let sum = import(&conn, "x.md", md).expect("导入应该成功");
        assert_eq!(sum.added, 1, "应该导入 1 条，实际 {sum:?}");
        let list = db::tasks_on_day(&conn, "2026-03-02").expect("查得到");
        assert!(list[0].done, "`[x]` 要落到库里变成已完成");
    }

    #[test]
    fn rule_note_writes_interval_and_until() {
        // 只写「每周一、三、五」会丢 interval，只写「每天」会丢 until——
        // 这两个字段一丢，导出再导入就不是原来那条规则了
        assert_eq!(
            rule_note(r#"{"freq":"weekly","interval":2,"byday":[1,3,5],"until":null}"#),
            "每 2 周的周一、周三、周五",
            "间隔要写出来（每个星期都带「周」字）"
        );
        assert_eq!(
            rule_note(r#"{"freq":"daily","interval":1,"byday":[],"until":"2026-12-31"}"#),
            "每天，到 2026-12-31",
            "结束日期要写出来"
        );
        assert_eq!(
            rule_note(r#"{"freq":"weekly","interval":1,"byday":[],"until":null}"#),
            "每周",
            "没指定星期几就是「每周」"
        );
        assert_eq!(rule_note("not json"), "", "脏规则不编说明");
    }

    #[test]
    fn parse_rule_note_reads_back_every_shape() {
        let cases = [
            (
                "每天",
                r#"{"freq":"daily","interval":1,"byday":[],"until":null}"#,
            ),
            (
                "每 3 天",
                r#"{"freq":"daily","interval":3,"byday":[],"until":null}"#,
            ),
            (
                "工作日",
                r#"{"freq":"weekdays","interval":1,"byday":[],"until":null}"#,
            ),
            (
                "每周",
                r#"{"freq":"weekly","interval":1,"byday":[],"until":null}"#,
            ),
            (
                "每周一、三、五",
                r#"{"freq":"weekly","interval":1,"byday":[1,3,5],"until":null}"#,
            ),
            (
                "每 2 周的周一、三、五",
                r#"{"freq":"weekly","interval":2,"byday":[1,3,5],"until":null}"#,
            ),
            (
                "每月",
                r#"{"freq":"monthly","interval":1,"byday":[],"until":null}"#,
            ),
            (
                "每月最后一个工作日",
                r#"{"freq":"last_workday","interval":1,"byday":[],"until":null}"#,
            ),
            (
                "每 2 个月的最后一个工作日",
                r#"{"freq":"last_workday","interval":2,"byday":[],"until":null}"#,
            ),
            (
                "每 2 个月",
                r#"{"freq":"monthly","interval":2,"byday":[],"until":null}"#,
            ),
            (
                "每年",
                r#"{"freq":"yearly","interval":1,"byday":[],"until":null}"#,
            ),
            (
                "每天，到 2026-12-31",
                r#"{"freq":"daily","interval":1,"byday":[],"until":"2026-12-31"}"#,
            ),
        ];
        for (note, want) in cases {
            let got = parse_rule_note(&format!(" · ↻ {note}"));
            assert_eq!(got.as_deref(), Some(want), "「{note}」应该解析成 {want}");
        }
        // 每一条都得原样往返（导出写成什么样，导入就认回什么样）
        for (_note, json) in cases {
            let back = rule_note(json);
            assert_eq!(
                parse_rule_note(&format!(" · ↻ {back}")).as_deref(),
                Some(json),
                "{json} 的说明「{back}」要能反解回原值"
            );
        }
        assert!(
            parse_rule_note(" · ↻ 每周一三五").is_none(),
            "没顿号不认，别猜"
        );
        assert!(
            parse_rule_note(" · 提前 5 分钟").is_none(),
            "没有 ↻ 就是没有重复"
        );
        assert!(
            parse_rule_note(" · ↻ 每天，到 不是日期").is_none(),
            "结束日期形状不对就不认这条规则"
        );
    }

    #[test]
    fn parse_advance_note_reads_back() {
        assert_eq!(
            parse_advance_note(" · 提前 5 分钟").as_deref(),
            Some("[-300,0]"),
            "5 分钟 = 300 秒"
        );
        assert_eq!(
            parse_advance_note(" · ↻ 每天 · 提前 1 小时、1 天").as_deref(),
            Some("[-86400,-3600,0]"),
            "多个提前点要排序 + 末尾补到点的 0"
        );
        assert!(parse_advance_note(" · ↻ 每天").is_none(), "没有提前段");
        assert!(
            parse_advance_note(" · 提前 5 个").is_none(),
            "单位不认识就返回 None，不要瞎猜成分钟"
        );
        assert!(
            parse_advance_note(" · 提前 30 天").is_none(),
            "超过 7 天上限的不认（`MAX_REMIND_OFFSET_SECS`）"
        );
    }

    #[test]
    fn markdown_round_trip_keeps_rule_and_advance() {
        let conn = mem();
        let rule = r#"{"freq":"weekly","interval":2,"byday":[1,3,5],"until":"2026-12-31"}"#;
        let input = TaskInput {
            title: "隔周交周报".to_string(),
            body: String::new(),
            day: "2026-03-02".to_string(),
            due_at_ms: time::ms_at_day_time("2026-03-02", parse_hm("09:30").expect("合法时间")),
            repeat_rule: Some(rule.to_string()),
            remind_offsets: Some("[-1800,0]".to_string()),
            notify_channels: None,
        };
        db::create_with_occurrences(&conn, &input).expect("造数据不该失败");
        let md = export_markdown(&conn).expect("导出不该失败");
        assert!(
            md.contains(" · ↻ 每 2 周的周一、周三、周五，到 2026-12-31 · 提前 30 分钟"),
            "导出要把间隔、结束日期、提前提醒都写上，实际：\n{md}"
        );

        let other = mem();
        let sum = import(&other, "backup.md", &md).expect("导入应该成功");
        assert_eq!(sum.added, 1, "应该导入 1 条，实际 {sum:?}");
        let list = db::tasks_on_day(&other, "2026-03-02").expect("查得到");
        assert_eq!(list.len(), 1, "应该只有 1 条");
        assert_eq!(
            list[0].repeat_rule.as_deref(),
            Some(rule),
            "Markdown 往返要保住重复规则（含间隔与结束日期）"
        );
        assert_eq!(
            list[0].remind_offsets, "[-1800,0]",
            "Markdown 往返要保住提前提醒"
        );
    }

    #[test]
    fn import_rejects_rows_without_day_heading() {
        let conn = mem();
        let md = "- [ ] 09:00 悬空的条目\n";
        let sum = import(&conn, "x.md", md).expect("前面有内容，不该整体报错");
        assert_eq!(sum.added, 0, "没有日期标题时不能猜日期，实际 {sum:?}");
        assert_eq!(sum.failed.len(), 1, "应该记一条失败原因，实际 {sum:?}");
    }

    #[test]
    fn import_reports_unsupported_suffix() {
        let conn = mem();
        let err = import(&conn, "x.xlsx", "whatever").expect_err("不认识的格式要报错");
        assert!(
            err.contains(".md"),
            "错误信息要告诉用户能用什么格式，实际：{err}"
        );
    }

    #[test]
    fn csv_parser_handles_quotes() {
        let row = parse_csv_line(r#"2026-03-02,0,09:30,"说""喂""的人",,"[0]""#);
        assert_eq!(row.len(), 6, "应该有 6 列，实际 {row:?}");
        assert_eq!(row[3], "说\"喂\"的人", "引号转义没还原对，实际 {row:?}");
    }

    #[test]
    fn strip_notes_drops_export_decorations() {
        let (t, _) = strip_notes("喝水 · ↻ 每天 · 提前 5 分钟");
        assert_eq!(t, "喝水", "导出加的两段说明都要切掉");
        let (t2, _) = strip_notes("只有标题");
        assert_eq!(t2, "只有标题", "没说明时不能动标题");
    }

    #[test]
    fn advance_note_reads_offsets() {
        assert_eq!(advance_note("[-300,0]"), "提前 5 分钟", "-300 秒是 5 分钟");
        assert_eq!(advance_note("[0]"), "", "只有到点就没有「提前」可写");
        assert_eq!(
            advance_note("[-86400,0]"),
            "提前 1 天",
            "整天要换算成天，别写成 1440 分钟"
        );
    }
}
