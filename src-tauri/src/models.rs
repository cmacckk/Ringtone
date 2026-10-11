use chrono::Datelike;
use serde::{Deserialize, Serialize};

/// 待办任务。字段与前端 `src/api.ts` 的 `Task` 一一对应，保持 snake_case。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: i64,
    pub title: String,
    pub body: String,
    /// 本地日期 `YYYY-MM-DD`
    pub day: String,
    /// epoch 毫秒；`None` = 归属当天但不定具体时刻
    pub due_at_ms: Option<i64>,
    /// JSON 数组字符串，如 `"[0,-300]"` = 到点 + 提前 300 秒各提醒一次
    pub remind_offsets: String,
    pub repeat_rule: Option<String>,
    /// 只发到这些渠道类型（JSON 数组，如 `["dingtalk"]`）；`None` = 不限制，发全部启用渠道。
    ///
    /// 存的是渠道**类型**而不是渠道 id：渠道删了重建、id 会变，
    /// 按 id 存的勾选会指到一个不存在的渠道，用户的「只发钉钉」会变成「一个都不发」。
    #[serde(default)]
    pub notify_channels: Option<String>,
    /// 用户自己标的「重点」标记。纯展示：列表里加粗 + 上色，不参与排序、不影响提醒。
    ///
    /// 编辑任务**不会**改它（`TaskInput` 里没这个字段，`update` 的 SQL 也不碰它），
    /// 只能通过 `set_task_star` 命令改——否则「双击改个标题」会把星标弄丢。
    #[serde(default)]
    pub star: bool,
    /// 这一行是不是「重复任务在非起始日期上的展开」。
    ///
    /// 为 `true` 时，勾选的含义是「完成本次」（系列继续），而不是完成整个任务；
    /// 界面据此决定调 `set_occurrence_done` 还是 `set_task_done`。
    #[serde(default)]
    pub repeated: bool,
    pub done: bool,
    pub created_ms: i64,
    pub updated_ms: i64,
}

/// 新建 / 更新任务的入参
#[derive(Debug, Clone, Deserialize)]
pub struct TaskInput {
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub day: String,
    #[serde(default)]
    pub due_at_ms: Option<i64>,
    #[serde(default)]
    pub repeat_rule: Option<String>,
    /// 提醒点：JSON 数组字符串，元素是相对 `due_at_ms` 的**秒**偏移（`0` = 到点，`-300` = 提前 5 分钟）。
    ///
    /// `None` 的含义跟场景有关：**创建**时代表「用默认的到点提醒」，
    /// **更新**时代表「保持库里原值不动」——所以界面不动这个下拉时，就不会把用户已有的提前点冲掉。
    #[serde(default)]
    pub remind_offsets: Option<String>,
    /// 提醒发到哪些渠道类型（JSON 数组，如 `["dingtalk"]`、`["feishu","dingtalk"]`）。
    ///
    /// `None` = 不限制，发全部启用渠道。**它和 `remind_offsets` 的 `None` 不是一个意思**：
    /// 这里是「全部」这个真实取值，不是「保持原值」——编辑表单每次都会把当前选择一起发上来，
    /// 所以更新时直接覆盖是安全的（不需要 `COALESCE` 那套）。
    #[serde(default)]
    pub notify_channels: Option<String>,
}

/// 新建任务的默认提醒点：仅到点提醒
pub const DEFAULT_REMIND_OFFSETS: &str = "[0]";

/// 提醒点偏移的绝对值上限（秒）= 7 天。超出视为脏数据丢弃。
pub const MAX_REMIND_OFFSET_SECS: i64 = 7 * 24 * 3600;

/// `notify_channels` 最多允许几个渠道类型（防串数据，不是产品限制）
pub const MAX_NOTIFY_CHANNELS: usize = 8;

/// 把 `notify_channels` 归一化成规范字符串：去空格、去重、固定顺序。
///
/// **故意不校验「这个渠道类型认不认识」**：渠道类型清单在 `channel` 模块里，
/// 那里刻意没有常量表（理由见 `channel/mod.rs` 的注释）。这里只卡形状——
/// 值是个没接完的类型时，运行时找不到对应渠道，会走本地通知兜底，不会静默把提醒丢掉。
pub fn normalize_notify_channels(raw: &str) -> Result<String, String> {
    let t = raw.trim();
    if t.is_empty() {
        return Err("提醒渠道不能是空字符串（要发全部渠道请传 null）".to_string());
    }
    let parsed: Vec<String> = serde_json::from_str(t)
        .map_err(|_| format!("提醒渠道必须是 JSON 数组，例如 [\"dingtalk\"]：{t}"))?;
    if parsed.is_empty() {
        return Err(format!(
            "提醒渠道不能是空数组（要发全部渠道请传 null）：{t}"
        ));
    }
    if parsed.len() > MAX_NOTIFY_CHANNELS {
        return Err(format!(
            "提醒渠道最多 {MAX_NOTIFY_CHANNELS} 个，收到 {} 个：{t}",
            parsed.len()
        ));
    }
    let mut kinds: Vec<String> = Vec::new();
    for k in parsed {
        let k = k.trim().to_string();
        if k.is_empty() {
            return Err(format!("提醒渠道里不能有空名字：{t}"));
        }
        if !kinds.contains(&k) {
            kinds.push(k);
        }
    }
    // 排序后序列化：同一组选择永远存成同一个字符串，"值有没有变" 才好判断
    kinds.sort();
    serde_json::to_string(&kinds).map_err(|e| format!("提醒渠道序列化失败：{e}"))
}

/// 读出任务选定的渠道类型；`None` = 不限制（发全部启用渠道）。
///
/// 读的时候兜住脏数据：库里可能躺着历史行或手改过的值，
/// 解析不出来就当「不限制」——绝不让一个坏值变成「一个渠道都不发」。
pub fn parse_notify_kinds(raw: Option<&str>) -> Option<Vec<String>> {
    let t = raw?.trim();
    if t.is_empty() {
        return None;
    }
    let parsed: Vec<String> = serde_json::from_str(t).ok()?;
    let kept: Vec<String> = parsed
        .into_iter()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .collect();
    if kept.is_empty() {
        None
    } else {
        Some(kept)
    }
}

/// 重复频率的可选值。字符串而不是枚举，是因为它直接存在 `tasks.repeat_rule` 的 JSON 里，
/// 要和前端 `src/api.ts` 的 `RepeatRule.freq` 对得上（写错就静默不展开）。
pub const REPEAT_FREQS: [&str; 6] = [
    "daily",
    "weekdays",
    "weekly",
    "monthly",
    "last_workday",
    "yearly",
];

/// 重复间隔上限。取值大到没意义，还容易把展开循环写成“看起来卡住”。
pub const MAX_REPEAT_INTERVAL: i64 = 99;

/// 重复规则，序列化后存在 `tasks.repeat_rule`（`None` = 不重复）。
///
/// 语义（用户 m00014 确认「时间规则按常见待办软件的规则来」，即滴答清单那一套）：
/// - 起点是任务自己的 `day`，规则**只往后展开**，永远不会生成起点之前的日子；
/// - `until` 是**含当天**的最后一天，`None` = 永不结束；
/// - `weekly` 的 `byday` 为空时，取起点那天的星期几；
/// - `monthly` 按起点的「几号」来，**当月没有这个号就落到当月最后一天**
///   （31 号的任务在 2 月落 28/29 号、4 月落 30 号，不再整月不响）；
/// - `yearly` 同理：2 月 29 日只在闰年响。
///
/// 「完成本次、系列继续」不在这里——那是 `db::set_occurrence_done` 的事，
/// 这里只回答“哪几天该响”。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepeatRule {
    /// 取值见 `REPEAT_FREQS`
    pub freq: String,
    /// 每隔 N 个周期（`weekdays` 忽略它）
    #[serde(default = "default_interval")]
    pub interval: i64,
    /// 每周几：1=周一 … 7=周日（只对 `weekly` 有意义）
    #[serde(default)]
    pub byday: Vec<i64>,
    /// 结束日期 `YYYY-MM-DD`，含当天
    #[serde(default)]
    pub until: Option<String>,
}

fn default_interval() -> i64 {
    1
}

/// 解析 `repeat_rule`。**脏数据一律当成“不重复”**（返回 `None`）：
/// 宁可少一次重复提醒，也不能因为一条坏规则把调度线程搞崩或者生成一堆离谱实例。
pub fn parse_repeat(raw: &str) -> Option<RepeatRule> {
    if raw.trim().is_empty() {
        return None;
    }
    let rule = serde_json::from_str::<RepeatRule>(raw).ok()?;
    // 频率不认识就当成「不重复」：`validated()` 只管入库前的入参，
    // 但库里可能已经躺着历史脏数据（或手改过的规则），
    // 读的时候不兜住的话 `matches_day` 会静默返回 false —— “看着配了重复，其实就是不响”。
    if !REPEAT_FREQS.contains(&rule.freq.as_str()) {
        return None;
    }
    Some(rule)
}

/// 把规则写回 JSON 字符串。目前只有测试在造数据时用它（界面提交的字符串由前端拼），
/// 所以标 `cfg(test)`——不标的话生产构建会报 dead_code。
#[cfg(test)]
pub fn serialize_repeat(rule: &RepeatRule) -> String {
    serde_json::to_string(rule).unwrap_or_default()
}

impl RepeatRule {
    /// 入参校验。界面上的选项有限，但接口能被直接调用，所以这里必须自己卡。
    pub fn validated(&self) -> Result<(), String> {
        if !REPEAT_FREQS.contains(&self.freq.as_str()) {
            return Err(format!(
                "重复频率不认识：{}（可选：{}）",
                self.freq,
                REPEAT_FREQS.join("/")
            ));
        }
        if self.interval < 1 || self.interval > MAX_REPEAT_INTERVAL {
            return Err(format!(
                "重复间隔必须在 1~{} 之间，收到：{}",
                MAX_REPEAT_INTERVAL, self.interval
            ));
        }
        if !self.byday.is_empty() {
            if self.freq != "weekly" {
                return Err("只有「每周」才能指定星期几".to_string());
            }
            for d in &self.byday {
                if *d < 1 || *d > 7 {
                    return Err(format!("星期必须是 1~7（1=周一），收到：{d}"));
                }
            }
        }
        if let Some(until) = &self.until {
            crate::time::validate_day(until)?;
        }
        Ok(())
    }

    /// 规则展开后的某一天该不该生成提醒。
    ///
    /// 用的是「逐天判断」而不是「从起点跳到下一个」：视野窗口只有几天，
    /// 逐天判断不会慢，而且不用为每种频率各写一套跳步逻辑（那才是 bug 温床）。
    pub fn matches_day(&self, start_day: &str, day: &str) -> bool {
        // 文本比较安全：`day` 一律是补零到 10 位的 `YYYY-MM-DD`（time::validate_day 卡死了形状）
        if day < start_day {
            return false;
        }
        if let Some(until) = &self.until {
            if day > until.as_str() {
                return false;
            }
        }
        let (Some(start), Some(cur)) = (
            crate::time::parse_day(start_day),
            crate::time::parse_day(day),
        ) else {
            return false;
        };
        let interval = self.interval.max(1);
        let diff_days = cur.signed_duration_since(start).num_days();
        let weekday = cur.weekday().number_from_monday() as i64;
        match self.freq.as_str() {
            "daily" => diff_days % interval == 0,
            // 工作日 = 周一到周五，**但要跳过法定节假日、并把调休补班的周末算进来**（问 `holiday`，
            // 它内置了国务院的放假安排）；与间隔无关（“每隔 2 个工作日”不实现，避免语义模糊）
            "weekdays" => crate::holiday::is_workday(day),
            "weekly" => {
                let weeks = week_index(cur) - week_index(start);
                // 没指定星期几就继承**起点那天的星期几**（点「每周」不选星期时，
                // 用户要的是“每周同一天”，而不是“这一周里随便哪天都响”）。
                let hit = if self.byday.is_empty() {
                    start.weekday().number_from_monday() as i64 == weekday
                } else {
                    self.byday.contains(&weekday)
                };
                weeks % interval == 0 && hit
            }
            // 「几号」对不上就落到**当月最后一天**：31 号的任务在 2 月落 28/29 号、4 月落 30 号，
            // 而不是整月不响（用户选的就是「每月一次」）
            "monthly" => {
                cur.day() == month_target_day(start.day(), cur.year(), cur.month())
                    && months_between(start, cur) % interval == 0
            }
            // 「每月最后一个工作日」：月底往回找第一个工作日。
            // 节假日表认识调休补班，所以「10 月最后一天是周六、但周六要补班」这种日子不会被跳过。
            "last_workday" => {
                cur.day() == last_workday_of_month(cur.year(), cur.month())
                    && months_between(start, cur) % interval == 0
            }
            "yearly" => {
                cur.month() == start.month()
                    && cur.day() == start.day()
                    && (cur.year() - start.year()) as i64 % interval == 0
            }
            _ => false,
        }
    }
}

/// 这个日期属于第几个自然周（以周一为周始）。
///
/// 除以 7 之前先把日期退到那周的周一，否则「相隔 6 天的同一周」和「相隔 1 天的跨周」算出来差不多。
fn week_index(date: chrono::NaiveDate) -> i64 {
    let monday_offset = date.weekday().number_from_monday() as i64 - 1;
    date.num_days_from_ce() as i64 - monday_offset
}

fn months_between(from: chrono::NaiveDate, to: chrono::NaiveDate) -> i64 {
    (to.year() - from.year()) as i64 * 12 + (to.month() as i64 - from.month() as i64)
}

/// 「几号」在目标月里的实际落点：当月没有这个号（31 号遇到小月）就落到月末。
fn month_target_day(start_day: u32, year: i32, month: u32) -> u32 {
    start_day.min(days_in_month(year, month))
}

/// 某个月的最后一个工作日（月底往回找第一个非周末/非假日的日子）。
fn last_workday_of_month(year: i32, month: u32) -> u32 {
    for d in (1..=days_in_month(year, month)).rev() {
        if crate::holiday::is_workday(&format!("{year:04}-{month:02}-{d:02}")) {
            return d;
        }
    }
    // 一个月全是节假日不可能发生（节假日安排最多连休 9 天），兵底返回 1
    1
}

/// 目标月有几天。用「下个月 1 号减一天」算，不手写闰年判断。
fn days_in_month(year: i32, month: u32) -> u32 {
    let (ny, nm) = if month >= 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    // 走到这一步说明 year/month 来自真实日期，取不到只可能是极端年份：兜底 28 天（永远是合法的）
    let first_next = match chrono::NaiveDate::from_ymd_opt(ny, nm, 1) {
        Some(d) => d,
        None => return 28,
    };
    first_next
        .pred_opt()
        .map(|d| chrono::Datelike::day(&d))
        .unwrap_or(28)
}

/// 解析 `remind_offsets`。单位是**秒**（负数 = 提前），与字段既有注释一致，
/// 不要改成分钟——库里的旧值都按秒解释，改单位会静默错位。
///
/// 任何脏数据一律兜底成 `[0]`：宁可少提醒一次，也不能崩，
/// 更不能因为一个坏值生成一堆离谱的实例。
pub fn parse_remind_offsets(raw: &str) -> Vec<i64> {
    let parsed: Vec<i64> = serde_json::from_str(raw).unwrap_or_default();
    let mut out: Vec<i64> = parsed
        .into_iter()
        .filter(|secs| secs.abs() <= MAX_REMIND_OFFSET_SECS)
        .collect();
    out.sort_unstable();
    out.dedup();
    if out.is_empty() {
        out.push(0);
    }
    out
}

/// 把提醒点写回 JSON 数组字符串。
///
/// 目前只有单测用得到（界面还没开放编辑提醒点），因此加了 `cfg(test)` 门，
/// 免得 release 包里带一段没人调用的代码。
#[cfg(test)]
pub fn serialize_remind_offsets(offsets: &[i64]) -> String {
    serde_json::to_string(offsets).unwrap_or_else(|_| DEFAULT_REMIND_OFFSETS.to_string())
}

/// 提醒实例状态。
///
/// `sending` 是**认领锁**：只有把状态从 `pending` 抢到 `sending` 的那个执行者才有权发送，
/// 这是防重复轰炸的关键（见 `db::claim_due`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OccurrenceState {
    Pending,
    Sending,
    Sent,
    LateSent,
    Missed,
    Skipped,
}

impl OccurrenceState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Sending => "sending",
            Self::Sent => "sent",
            Self::LateSent => "late_sent",
            Self::Missed => "missed",
            Self::Skipped => "skipped",
        }
    }

    /// 字符串→枚举。目前只有单测用到（报文里暂时不需要解析状态字符串）。
    #[cfg(test)]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "pending" => Some(Self::Pending),
            "sending" => Some(Self::Sending),
            "sent" => Some(Self::Sent),
            "late_sent" => Some(Self::LateSent),
            "missed" => Some(Self::Missed),
            "skipped" => Some(Self::Skipped),
            _ => None,
        }
    }
}

impl TaskInput {
    /// 校验 + 归一化（trim 标题），返回清洗后的副本
    pub fn validated(self) -> Result<Self, String> {
        let title = self.title.trim().to_string();
        if title.is_empty() {
            return Err("标题不能为空".to_string());
        }
        if title.chars().count() > 200 {
            return Err("标题最多 200 字".to_string());
        }
        crate::time::validate_day(&self.day)?;
        if let Some(ms) = self.due_at_ms {
            if ms <= 0 {
                return Err(format!("提醒时间非法：{ms}"));
            }
        }
        if let Some(rule) = &self.repeat_rule {
            if rule.trim().is_empty() {
                return Err("重复规则不能是空字符串（不需要重复请传 null）".to_string());
            }
            let parsed = parse_repeat(rule)
                .ok_or_else(|| format!("重复规则不是合法 JSON 或字段缺失：{rule}"))?;
            parsed.validated()?;
        }
        if let Some(raw) = &self.remind_offsets {
            let t = raw.trim();
            if t.is_empty() {
                return Err("提醒点不能是空字符串（要恢复默认请传 null）".to_string());
            }
            // 这里**故意不用** `parse_remind_offsets` 做校验：它对垃圾输入会兜底成 `[0]`，
            // 于是任何字符串都“看起来合法”，用户传错了也发现不了。
            let parsed: Vec<i64> = serde_json::from_str(t)
                .map_err(|_| format!("提醒点必须是 JSON 数组，例如 [0] 或 [-300,0]：{t}"))?;
            if parsed.is_empty() {
                return Err(format!("提醒点不能是空数组（要恢复默认请传 null）：{t}"));
            }
            if parsed
                .iter()
                .any(|secs| secs.abs() > MAX_REMIND_OFFSET_SECS)
            {
                return Err(format!(
                    "提醒点的偏移最多 {MAX_REMIND_OFFSET_SECS} 秒（7 天）：{t}"
                ));
            }
        }
        let notify_channels = match self.notify_channels.as_deref() {
            None => None,
            Some(raw) => Some(normalize_notify_channels(raw)?),
        };
        Ok(Self {
            title,
            notify_channels,
            ..self
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(title: &str) -> TaskInput {
        TaskInput {
            title: title.to_string(),
            body: String::new(),
            day: "2026-02-01".to_string(),
            due_at_ms: None,
            repeat_rule: None,
            remind_offsets: None,
            notify_channels: None,
        }
    }

    #[test]
    fn trims_title() {
        let got = input("  写周报  ").validated().expect("应当通过校验");
        assert_eq!(got.title, "写周报", "标题应当被 trim");
    }

    /// 提醒点是界面直接传进来的字符串，格式错了必须在入库前拦住。
    /// 注意不能用 `parse_remind_offsets` 做校验——它对垃圾输入会兜底成 `[0]`，
    /// 于是任何字符串都“看起来合法”。
    #[test]
    fn rejects_bad_remind_offsets() {
        for raw in ["", "  ", "[]", "0", "[0,", "[\"a\"]", "[999999999]"] {
            let mut args = input("写周报");
            args.remind_offsets = Some(raw.to_string());
            let got = args.validated();
            assert!(
                got.is_err(),
                "提醒点 {raw:?} 应当被拒绝，实际通过了：{:?}",
                got.map(|t| t.remind_offsets)
            );
        }
        for raw in ["[0]", "[-300,0]", "[-604800,0]"] {
            let mut args = input("写周报");
            args.remind_offsets = Some(raw.to_string());
            assert!(args.validated().is_ok(), "提醒点 {raw:?} 应当通过校验");
        }
    }

    /// 不传提醒点（`None`）是合法的：创建时用默认的到点提醒，更新时表示「保持原值」。
    #[test]
    fn absent_remind_offsets_is_fine() {
        let args = input("写周报");
        assert_eq!(args.remind_offsets, None, "默认应当是不传");
        assert!(args.validated().is_ok(), "不传提醒点应当通过校验");
    }

    #[test]
    fn rejects_bad_notify_channels() {
        for raw in [
            "",
            "  ",
            "[]",
            "dingtalk",
            "[\"dingtalk\"",
            "[\"\"]",
            "[\" \"]",
            "[1,2]",
        ] {
            let mut args = input("写周报");
            args.notify_channels = Some(raw.to_string());
            let got = args.validated();
            assert!(
                got.is_err(),
                "提醒渠道 {raw:?} 应当被拒绝，实际通过了：{:?}",
                got.map(|t| t.notify_channels)
            );
        }
    }

    /// 归一化：去空格、去重、固定顺序——同一组选择永远存成同一个字符串。
    #[test]
    fn normalizes_notify_channels() {
        for (raw, want) in [
            ("[\"dingtalk\"]", "[\"dingtalk\"]"),
            ("[\"dingtalk\",\"dingtalk\"]", "[\"dingtalk\"]"),
            ("[ \"feishu\" , \"dingtalk\" ]", "[\"dingtalk\",\"feishu\"]"),
        ] {
            let mut args = input("写周报");
            args.notify_channels = Some(raw.to_string());
            let got = args.validated().expect("应当通过校验");
            assert_eq!(
                got.notify_channels.as_deref(),
                Some(want),
                "提醒渠道 {raw:?} 归一化结果不对"
            );
        }
    }

    /// 不传（`None`）= 发全部启用渠道；这不是脏数据，是最常见的取值。
    #[test]
    fn absent_notify_channels_means_all() {
        let args = input("写周报");
        assert_eq!(args.notify_channels, None, "默认应当是不传（= 全部渠道）");
        assert!(args.validated().is_ok(), "不传提醒渠道应当通过校验");
        assert_eq!(
            parse_notify_kinds(None),
            None,
            "读出来的语义必须是「不限制」"
        );
        // 脏数据也要兜住：解析不出来就当「不限制」，不能变成「一个都不发」
        for raw in ["", "   ", "不是 JSON", "[]", "[\"\"]", "{}"] {
            assert_eq!(
                parse_notify_kinds(Some(raw)),
                None,
                "脏数据 {raw:?} 应当按「不限制」处理"
            );
        }
        assert_eq!(
            parse_notify_kinds(Some("[\"dingtalk\"]")),
            Some(vec!["dingtalk".to_string()]),
            "正常值应当被读出来"
        );
    }

    #[test]
    fn rejects_empty_title() {
        let e = input("   ").validated().expect_err("空标题应当被拒绝");
        assert!(
            e.contains("标题不能为空"),
            "错误信息应当说明原因，实际：{e}"
        );
    }

    #[test]
    fn rejects_bad_day() {
        let mut bad = input("x");
        bad.day = "2026/02/01".to_string();
        assert!(bad.validated().is_err(), "非法日期应当被拒绝");
    }

    #[test]
    fn rejects_negative_due() {
        let mut bad = input("x");
        bad.due_at_ms = Some(-1);
        assert!(bad.validated().is_err(), "负数时间戳应当被拒绝");
    }

    #[test]
    fn parses_offsets_sorted_and_deduped() {
        let got = parse_remind_offsets("[0,600,-300,600]");
        assert_eq!(
            got,
            vec![-300, 0, 600],
            "应当去重并升序（提前的在前），实际 {got:?}"
        );
    }

    #[test]
    fn bad_offsets_fall_back_to_zero() {
        for bad in ["", "[]", "abc", "null", "{}", "[0,]", "[\"x\"]"] {
            let got = parse_remind_offsets(bad);
            assert_eq!(got, vec![0], "脏数据 {bad} 应当兜底成 [0]，实际 {got:?}");
        }
    }

    #[test]
    fn out_of_range_offsets_are_dropped() {
        let got = parse_remind_offsets("[-5,999999999,10]");
        assert_eq!(got, vec![-5, 10], "越界偏移应当被丢弃，实际 {got:?}");

        let all_bad = parse_remind_offsets("[999999999,-999999999]");
        assert_eq!(all_bad, vec![0], "全部越界时应当兜底成 [0]");
    }

    #[test]
    fn offsets_roundtrip_through_serialize() {
        let raw = serialize_remind_offsets(&[-300, 0]);
        assert_eq!(
            parse_remind_offsets(&raw),
            vec![-300, 0],
            "序列化后应能原样解析"
        );
    }

    #[test]
    fn occurrence_state_roundtrip() {
        for st in [
            OccurrenceState::Pending,
            OccurrenceState::Sending,
            OccurrenceState::Sent,
            OccurrenceState::LateSent,
            OccurrenceState::Missed,
            OccurrenceState::Skipped,
        ] {
            assert_eq!(
                OccurrenceState::parse(st.as_str()),
                Some(st),
                "{} 应当能解析回枚举",
                st.as_str()
            );
        }
        assert_eq!(
            OccurrenceState::parse("bogus"),
            None,
            "未知状态应当返回 None"
        );
    }

    fn rule(json: &str) -> RepeatRule {
        parse_repeat(json).unwrap_or_else(|| panic!("规则应当能解析：{json}"))
    }

    #[test]
    fn repeat_rule_parses_and_roundtrips() {
        let raw = r#"{"freq":"weekly","interval":2,"byday":[1,3,5],"until":"2026-12-31"}"#;
        let r = rule(raw);
        assert_eq!(r.freq, "weekly", "频率解析错误");
        assert_eq!(r.interval, 2, "间隔解析错误");
        assert_eq!(r.byday, vec![1, 3, 5], "星期解析错误");
        r.validated().expect("合法规则不应被拒绝");
        assert_eq!(
            parse_repeat(&serialize_repeat(&r)),
            Some(r.clone()),
            "序列化后应当能原样解析回来"
        );
        // interval 缺省成 1，否则前端漏传一个字段就变成「永不重复」
        assert_eq!(rule(r#"{"freq":"daily"}"#).interval, 1, "缺省间隔应为 1");
    }

    #[test]
    fn repeat_rule_rejects_garbage() {
        for bad in [
            "",
            "   ",
            "null",
            "{}",
            "[1,2]",
            "abc",
            r#"{"freq":"每小时"}"#,
        ] {
            assert!(
                parse_repeat(bad).is_none(),
                "脏规则 {bad} 应当被当成“不重复”，实际被解析出来了"
            );
        }
        assert!(
            rule(r#"{"freq":"daily","interval":0}"#)
                .validated()
                .is_err(),
            "间隔 0 应当被拒绝（会让展开循环除零或永不停）"
        );
        assert!(
            rule(r#"{"freq":"daily","byday":[1]}"#).validated().is_err(),
            "只有「每周」才能带星期几"
        );
        assert!(
            rule(r#"{"freq":"weekly","byday":[8]}"#)
                .validated()
                .is_err(),
            "星期越界应当被拒绝"
        );
        assert!(
            rule(r#"{"freq":"daily","until":"2026-2-1"}"#)
                .validated()
                .is_err(),
            "结束日期必须补零，否则会被当文本比大小"
        );
    }

    #[test]
    fn repeat_daily_and_weekdays() {
        let daily = rule(r#"{"freq":"daily"}"#);
        assert!(
            daily.matches_day("2026-02-01", "2026-02-01"),
            "起点当天就该响"
        );
        assert!(
            !daily.matches_day("2026-02-01", "2026-01-31"),
            "起点之前永不展开"
        );

        let every3 = rule(r#"{"freq":"daily","interval":3}"#);
        assert!(
            every3.matches_day("2026-02-01", "2026-02-04"),
            "隔 3 天应当命中"
        );
        assert!(
            !every3.matches_day("2026-02-01", "2026-02-05"),
            "隔 3 天不该连续命中"
        );

        // 2026-02-07 是周六，02-08 是周日
        let weekdays = rule(r#"{"freq":"weekdays"}"#);
        assert!(
            !weekdays.matches_day("2026-02-02", "2026-02-07"),
            "周六不该响"
        );
        assert!(
            !weekdays.matches_day("2026-02-02", "2026-02-08"),
            "周日不该响"
        );
        assert!(
            weekdays.matches_day("2026-02-02", "2026-02-09"),
            "周一应当响"
        );
    }

    #[test]
    fn repeat_weekly_byday_and_interval() {
        // 起点 2026-02-02 是周一
        let mwf = rule(r#"{"freq":"weekly","byday":[1,3,5]}"#);
        assert!(mwf.matches_day("2026-02-02", "2026-02-02"), "周一应当响");
        assert!(!mwf.matches_day("2026-02-02", "2026-02-03"), "周二不该响");
        assert!(mwf.matches_day("2026-02-02", "2026-02-04"), "周三应当响");
        assert!(
            mwf.matches_day("2026-02-02", "2026-02-09"),
            "下一周周一应当响"
        );

        let biweekly = rule(r#"{"freq":"weekly","interval":2,"byday":[1]}"#);
        assert!(
            biweekly.matches_day("2026-02-02", "2026-02-16"),
            "隔一周的周一应当响"
        );
        assert!(
            !biweekly.matches_day("2026-02-02", "2026-02-09"),
            "中间那周不该响"
        );

        // byday 为空时取起点那天的星期几
        let inherit = rule(r#"{"freq":"weekly"}"#);
        assert!(
            inherit.matches_day("2026-02-02", "2026-02-09"),
            "默认应当继承起点的周一"
        );
        assert!(
            !inherit.matches_day("2026-02-02", "2026-02-10"),
            "周二不该响"
        );
    }

    #[test]
    fn repeat_monthly_yearly_and_until() {
        let m = rule(r#"{"freq":"monthly"}"#);
        assert!(
            m.matches_day("2026-01-31", "2026-03-31"),
            "每个月 31 号应当响"
        );
        assert!(
            m.matches_day("2026-01-31", "2026-02-28"),
            "2 月没有 31 号就落到月末（28 号），不再整月不响"
        );
        assert!(
            !m.matches_day("2026-01-31", "2026-02-27"),
            "月末前一天不该响"
        );
        assert!(
            !m.matches_day("2026-01-31", "2026-03-30"),
            "3 月有 31 号，30 号就不该响"
        );
        assert!(
            m.matches_day("2026-01-31", "2026-04-30"),
            "4 月只有 30 天，31 号的任务落到 30 号"
        );
        assert!(
            m.matches_day("2024-01-31", "2024-02-29"),
            "闰年 2 月落到 29 号"
        );
        // 间隔也要管：每 2 个月的 31 号，中间那个月不响
        let m2 = rule(r#"{"freq":"monthly","interval":2}"#);
        assert!(m2.matches_day("2026-01-31", "2026-03-31"), "隔月应当响");
        assert!(
            !m2.matches_day("2026-01-31", "2026-02-28"),
            "间隔为 2 时 2 月不该响"
        );

        // 每月最后一个工作日：跳过周末（也跳过法定假日，因为 `holiday` 表认得）
        let lw = rule(r#"{"freq":"last_workday"}"#);
        assert!(
            lw.matches_day("2026-01-01", "2026-10-30"),
            "2026-10-31 是周六，10 月末应当是 10-30（周五）"
        );
        assert!(
            !lw.matches_day("2026-01-01", "2026-10-31"),
            "周六不是工作日"
        );
        assert!(
            !lw.matches_day("2026-01-01", "2026-10-29"),
            "不是最后那个工作日就不该响"
        );
        assert!(
            lw.matches_day("2026-01-01", "2026-01-30"),
            "2026-01-31 是周六，1 月末应当是 01-30"
        );
        assert!(
            lw.matches_day("2026-01-01", "2026-02-28"),
            "2026-02-28 虽然是周六，但它是春节调休的**补班日**，所以它是 2 月最后一个工作日"
        );
        assert!(
            !lw.matches_day("2026-01-01", "2026-02-27"),
            "2-27 是周五，但它不是「最后一个」（2-28 要上班）"
        );
        assert!(
            !lw.matches_day("2026-10-30", "2026-09-30"),
            "起点之前的日子不该响"
        );
        let lw2 = rule(r#"{"freq":"last_workday","interval":2}"#);
        assert!(
            lw2.matches_day("2026-01-01", "2026-03-31"),
            "间隔 2 个月时 3 月末应当响"
        );
        assert!(
            !lw2.matches_day("2026-01-01", "2026-02-27"),
            "间隔 2 个月时 2 月末不该响"
        );

        let y = rule(r#"{"freq":"yearly"}"#);
        assert!(
            y.matches_day("2026-03-08", "2027-03-08"),
            "每年同一天应当响"
        );
        assert!(!y.matches_day("2026-03-08", "2027-03-07"), "差一天不该响");
        assert!(
            !y.matches_day("2024-02-29", "2025-02-28"),
            "2 月 29 日在平年没有对应日就不该响"
        );
        assert!(y.matches_day("2024-02-29", "2028-02-29"), "闰年应当响");

        let until = rule(r#"{"freq":"daily","until":"2026-02-03"}"#);
        assert!(
            until.matches_day("2026-02-01", "2026-02-03"),
            "最后一天含在内"
        );
        assert!(!until.matches_day("2026-02-01", "2026-02-04"), "之后不该响");
    }
}
