//! SQLite 访问层：schema 迁移 + 任务 CRUD。
//!
//! 约定（踩坑守则）：
//! - 所有 `*_ms` 字段一律 epoch 毫秒 INTEGER；
//! - 所有日期字段一律 `YYYY-MM-DD` 文本（与 `tasks.day` 同格式），不做混排；
//! - 迁移按 version 递增，可重复执行。

use std::collections::HashSet;
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, Result};
use serde::Serialize;

use crate::models::{
    parse_notify_kinds, parse_remind_offsets, parse_repeat, OccurrenceState, RepeatRule, Task,
    TaskInput, DEFAULT_REMIND_OFFSETS,
};
use crate::scheduler::{self, HORIZON_MS, LATE_WINDOW_MS, LOOKBACK_MS};
use crate::time::{self, now_ms};

/// 最新 schema 版本。**只用于迁移后的自检**，不要拿它当某一步的版本号写进库：
/// 它以前就是写入值，结果把版本号从 2 加到 3 之后，一个还没跑过 V2 的旧库会被记成
/// “已升到 3”，而 V2/V3 一个都没执行。
pub const SCHEMA_VERSION: i64 = 6;

/// 迁移清单：`(版本号, SQL)`。版本号写在这里，`migrate` 逐条比对执行。
/// 加新版本时：往这里追加一行 + 改 `SCHEMA_VERSION`（忘了改会被下面的自检拦住）。
const MIGRATIONS: &[(i64, &str)] = &[
    (1, MIGRATION_V1),
    (2, MIGRATION_V2),
    (3, MIGRATION_V3),
    (4, MIGRATION_V4),
    (5, MIGRATION_V5),
    (6, MIGRATION_V6),
];

/// v1 全量 schema。M1 只用 `tasks`，其余表给 M2（occurrences/push_jobs）和 M3（channels）预留。
const MIGRATION_V1: &str = r#"
CREATE TABLE tasks (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    title          TEXT    NOT NULL,
    body           TEXT    NOT NULL DEFAULT '',
    day            TEXT    NOT NULL,
    due_at_ms      INTEGER,
    remind_offsets TEXT    NOT NULL DEFAULT '[0]',
    repeat_rule    TEXT,
    done           INTEGER NOT NULL DEFAULT 0,
    created_ms     INTEGER NOT NULL,
    updated_ms     INTEGER NOT NULL
);
CREATE INDEX idx_tasks_day ON tasks(day);

-- 一次“到点触发”的实例，幂等的锚点
CREATE TABLE occurrences (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id    INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    fire_at_ms INTEGER NOT NULL,
    state      TEXT    NOT NULL DEFAULT 'pending'
               CHECK (state IN ('pending','sending','sent','late_sent','missed','skipped')),
    sent_ms    INTEGER
);
CREATE UNIQUE INDEX idx_occ_dedupe ON occurrences(task_id, fire_at_ms);

CREATE TABLE channels (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    name           TEXT    NOT NULL UNIQUE,
    kind           TEXT    NOT NULL,
    enabled        INTEGER NOT NULL DEFAULT 1,
    priority       INTEGER NOT NULL DEFAULT 100,
    credential_enc BLOB,
    config_json    TEXT    NOT NULL DEFAULT '{}'
);

CREATE TABLE push_jobs (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    occurrence_id INTEGER NOT NULL REFERENCES occurrences(id) ON DELETE CASCADE,
    channel_id    INTEGER NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    state         TEXT    NOT NULL DEFAULT 'pending'
                  CHECK (state IN ('pending','sending','sent','failed','skipped')),
    attempts      INTEGER NOT NULL DEFAULT 0,
    last_error    TEXT,
    next_retry_ms INTEGER,
    dedupe_key    TEXT    NOT NULL UNIQUE
);
CREATE INDEX idx_jobs_state ON push_jobs(state, next_retry_ms);

CREATE TABLE channel_quota (
    channel_id INTEGER NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    day        TEXT    NOT NULL,
    sent       INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (channel_id, day)
);

CREATE TABLE send_log (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_ms         INTEGER NOT NULL,
    channel_id    INTEGER,
    occurrence_id INTEGER,
    result        TEXT    NOT NULL,
    http_code     INTEGER,
    detail        TEXT
);
CREATE INDEX idx_log_ts ON send_log(ts_ms);
"#;

const SELECT_TASK: &str = "SELECT id, title, body, day, due_at_ms, remind_offsets, \
                            repeat_rule, notify_channels, star, done, created_ms, updated_ms FROM tasks";

/// v2：`push_jobs` 补一个「这条提醒本身是不是补发」的标记。
///
/// 为什么不能不看列直接算：`late` 的意思是「因为休眠/唤醒而迟到」，
/// 而不是「因为退避重试而晚了几十秒」。只看 `now - fire_at_ms` 会把重试误判成补发。
/// 纯 `ADD COLUMN`，不动任何旧数据，老库可平滑升级。
const MIGRATION_V2: &str = r#"
ALTER TABLE push_jobs ADD COLUMN late INTEGER NOT NULL DEFAULT 0;
"#;

/// v3：`occurrences` 补一个「这次被用户勾成完成了」的时间戳。
///
/// 为什么不加一个新的 `state` 值：`state` 上有 `CHECK` 约束，SQLite 改约束要**重建整张表**
/// （建新表 / 复制数据 / 删旧表 / 改名），风险远大于收益。而“完成了”与“作废了”本来就
/// 是两件事：重复任务的某一天勾完成后，这行的 `state` 要变成 `skipped`（免得还去发通知），
/// 但同时必须有地方记住“这是完成、不是错过”。`done_ms` 同时兼顾了这两个语义。
const MIGRATION_V3: &str = r#"
ALTER TABLE occurrences ADD COLUMN done_ms INTEGER;
"#;

/// v4：任务可以「只发指定渠道」（`notify_channels` = JSON 数组，如 `["dingtalk"]`）。
///
/// 空 = 全部启用渠道（跟升级前行为完全一致），所以这是纯 `ADD COLUMN`：
/// 老库升上来不会改变任何既有提醒的去向。
const MIGRATION_V4: &str = r#"
ALTER TABLE tasks ADD COLUMN notify_channels TEXT;
"#;

/// v6：`tasks.star` = 用户自己标的「重点」标记（默认 0，纯展示）。
///
/// 为什么不复用 `priority` 或排名：那类字段会引入“谁排前面”的语义，
/// 而这条需求的全部含义只是「列表里把它显出来」——不加排序、不影响提醒，
/// 所以最小实现就是一列布尔。`update` 的 SQL 故意不碰它：双击改个标题不该把星标弄丢。
const MIGRATION_V6: &str = r#"
ALTER TABLE tasks ADD COLUMN star INTEGER NOT NULL DEFAULT 0;
"#;

/// v5：两个东西一起加。
///
/// 1. `tasks.hidden`：`1` = 这条任务不出现在任何列表 / 计数 / 倒计时里，但**它的提醒照发**。
///    「上下班提醒」就是这么实现的：设置里配的四个时间点被实体化成隐藏任务，
///    于是它白白继承了整套到点调度：幂等、退避重试、降级链、限流闸门、提前提醒、关机不补发。
///    自己再写一条独立的推送路径，等于把这些逻辑再抄一遍，迟早抄不一致。
/// 2. `settings` 键值表：放「不属于某条任务」的设置（上下班时间、显示浮窗的快捷键）。
///    为什么不用 localStorage：快捷键要在**启动时、窗口还没开**就注册给系统，
///    上下班提醒要在后台线程醒来时读——它们都必须由 Rust 侧发得着。
const MIGRATION_V5: &str = r#"
ALTER TABLE tasks ADD COLUMN hidden INTEGER NOT NULL DEFAULT 0;
CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

/// 打开数据库并设置 WAL。目录不存在会自动创建。
pub fn open(path: &Path) -> std::result::Result<Connection, Box<dyn std::error::Error>> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(conn)
}

/// 建表 / 升级。可重复调用。
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version    INTEGER PRIMARY KEY,
            applied_ms INTEGER NOT NULL
        );",
    )?;
    let mut current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    for (version, sql) in MIGRATIONS {
        if current >= *version {
            continue;
        }
        conn.execute_batch(sql)?;
        conn.execute(
            "INSERT INTO schema_migrations (version, applied_ms) VALUES (?1, ?2)",
            params![version, now_ms()],
        )?;
        current = *version;
    }
    // 自检：跑完之后库里记的版本必须等于程序期望的版本。
    // 往 `MIGRATIONS` 加了新版本却忘了改 `SCHEMA_VERSION`（或反过来）时直接报错，
    // 而不是留下一个「程序以为是新版、库结构还是旧的」的库。
    if current != SCHEMA_VERSION {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some(format!(
                "迁移后数据库版本是 {current}，程序期望 {SCHEMA_VERSION}：MIGRATIONS 与 SCHEMA_VERSION 不一致"
            )),
        ));
    }
    Ok(())
}

fn row_to_task(row: &rusqlite::Row<'_>) -> Result<Task> {
    Ok(Task {
        id: row.get("id")?,
        title: row.get("title")?,
        body: row.get("body")?,
        day: row.get("day")?,
        due_at_ms: row.get("due_at_ms")?,
        remind_offsets: row.get("remind_offsets")?,
        repeat_rule: row.get("repeat_rule")?,
        notify_channels: row.get("notify_channels")?,
        // 「重点」标记：用户自己标的，界面据此加粗 + 上色（纯展示，不参与调度）
        star: row.get::<_, i64>("star")? != 0,
        // 单条查询不知道“展开”这回事，只有按天取列表时才会出现展开行
        // （见 `tasks_on_day`），所以这里固定 false。
        repeated: false,
        done: row.get::<_, i64>("done")? != 0,
        created_ms: row.get("created_ms")?,
        updated_ms: row.get("updated_ms")?,
    })
}

/// 某一天应该显示的**全部**任务：任务自己那天 + 重复任务展开到这一天的那次。
///
/// 列表和周条上的圆点都走这里，不允许各算一套：两个入口一旦用不同的判定，
/// 就会出现「这天有圆点但列表是空的」这种谁也说不清的状态。
pub fn tasks_on_day(conn: &Connection, day: &str) -> Result<Vec<Task>> {
    // 先按时间升序，未定时刻的排在最后
    let sql = format!(
        "{SELECT_TASK} WHERE day = ?1 AND hidden = 0 \
         ORDER BY (due_at_ms IS NULL) ASC, due_at_ms ASC, id ASC"
    );
    let finished = done_task_ids_on_day(conn, day)?;
    let mut out: Vec<Task> = {
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![day], row_to_task)?;
        rows.collect::<Result<Vec<_>>>()?
    };
    // 「完成本次」是**按天**记的，`tasks.done` 只表示整个任务完成。
    // 起始日那天的行也必须认这个标记：否则一个今天开始的「每天」任务，
    // 今天勾完之后列表里看不出任何变化（明天反而显示成已完成）。
    for t in &mut out {
        t.done = t.done || finished.contains(&t.id);
    }

    // 重复任务只可能从更早的日期展开过来（`day` 等于当天的那些上面已经拿到了）
    let repeats: Vec<Task> = {
        let sql = format!(
            "{SELECT_TASK} WHERE day < ?1 AND done = 0 AND hidden = 0 \
             AND repeat_rule IS NOT NULL AND repeat_rule <> ''"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![day], row_to_task)?;
        rows.collect::<Result<Vec<_>>>()?
    };
    if !repeats.is_empty() {
        for t in repeats {
            let Some(rule) = t.repeat_rule.as_deref().and_then(parse_repeat) else {
                continue; // 脏规则当不重复，别把这一天白白占掉
            };
            if !rule.matches_day(&t.day, day) {
                continue;
            }
            // 这一天该在几点响：沿用起点那天的时刻。
            // 没定时刻的重复任务只影响“这天有没有安排”的展示，不生成实例。
            let due = t
                .due_at_ms
                .and_then(|base| time::ms_at_day_time(day, time::tod_ms(base)));
            out.push(Task {
                // `day` 保持任务自己的**起始日**，不要改成正在看的那一天：
                // 界面保存编辑时要拿它回存，覆盖掉的话「双击改个标题」
                // 就会把整个系列挪到当前浏览的那一天去。
                due_at_ms: due,
                done: finished.contains(&t.id),
                repeated: true,
                ..t
            });
        }
    }

    out.sort_by(|a, b| {
        let ka = (a.due_at_ms.is_none(), a.due_at_ms.unwrap_or(0), a.id);
        let kb = (b.due_at_ms.is_none(), b.due_at_ms.unwrap_or(0), b.id);
        ka.cmp(&kb)
    });
    Ok(out)
}

/// 列出某天全部任务（`tasks_on_day` 的旧名字，保留给既有调用方/测试）。
pub fn list_by_day(conn: &Connection, day: &str) -> Result<Vec<Task>> {
    tasks_on_day(conn, day)
}

/// 这一天里被勾成「完成本次」的任务 id。
fn done_task_ids_on_day(conn: &Connection, day: &str) -> Result<HashSet<i64>> {
    let (Some(lo), Some(hi)) = (time::day_start_ms(day), next_day_start_ms(day)) else {
        return Ok(HashSet::new());
    };
    let mut stmt = conn.prepare(
        "SELECT DISTINCT task_id FROM occurrences \
          WHERE done_ms IS NOT NULL AND fire_at_ms >= ?1 AND fire_at_ms < ?2",
    )?;
    let it = stmt.query_map(params![lo, hi], |r| r.get::<_, i64>(0))?;
    it.collect()
}

/// 某个任务的哪些天已经被标成「完成本次」（以**那天的零点**为标识）。
fn done_day_starts(conn: &Connection, task_id: i64) -> Result<HashSet<i64>> {
    let mut stmt = conn
        .prepare("SELECT fire_at_ms FROM occurrences WHERE task_id = ?1 AND done_ms IS NOT NULL")?;
    let it = stmt.query_map(params![task_id], |r| r.get::<_, i64>(0))?;
    let mut out = HashSet::new();
    for fire_at in it {
        if let Some(start) = time::day_start_ms(&time::day_of_ms(fire_at?)) {
            out.insert(start);
        }
    }
    Ok(out)
}

fn next_day_start_ms(day: &str) -> Option<i64> {
    time::shift_day(day, 1).and_then(|d| time::day_start_ms(&d))
}

/// 某一天的待办数量：总数 + 未完成数。
///
/// 周条上的「这天有任务」小圆点、顶部的「今日 2/5」，都是这一条查询的产物。
#[derive(Debug, Clone, Serialize)]
pub struct DayCount {
    pub day: String,
    pub total: i64,
    pub undone: i64,
}

/// 右下角的两个数字：今天多少条、全部多少条。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskStats {
    /// 今天的条数（含重复任务展开出来的那几天）
    pub today_total: i64,
    /// 今天还没完成的条数
    pub today_undone: i64,
    /// 全部任务的条数（`tasks` 表行数，含已完成的）
    pub all_total: i64,
}

/// 今天 / 全部的任务条数。
///
/// 今天那部分**故意复用 `count_by_day`**：它和列表、周条圆点走同一套展开逻辑，
/// 否则会出现「列表里 5 条、右下角说 3 条」这种对不上的数字。
pub fn task_stats(conn: &Connection) -> Result<TaskStats> {
    let today = time::today_local();
    let day = count_by_day(conn, &today, &today)?
        .into_iter()
        .next()
        .unwrap_or(DayCount {
            day: today,
            total: 0,
            undone: 0,
        });
    // 「全部」不数隐藏任务：上下班提醒那 4 条不是用户建的待办，
    // 混进来的话右下角的「全部 N 条」会无缘无故比列表里多 4 条。
    let all_total: i64 =
        conn.query_row("SELECT COUNT(*) FROM tasks WHERE hidden = 0", [], |r| {
            r.get(0)
        })?;
    Ok(TaskStats {
        today_total: day.total,
        today_undone: day.undone,
        all_total,
    })
}

// ---------------------------------------------------------------- 通用键值设置（settings 表）
//
// 放「不属于某条任务」的设置。为什么不用前端的 localStorage：
// 快捷键要在窗口还没开的时候就注册给系统、上下班提醒要在后台线程醒来时读，
// 两边都只有 Rust 侧发得着。

/// 读一个设置项；没存过返回 `None`。
pub fn setting_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        params![key],
        |r| r.get(0),
    )
    .optional()
}

/// 写一个设置项（有了就覆盖）。
pub fn setting_set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// 删一个设置项（没存过也不报错）。
pub fn setting_delete(conn: &Connection, key: &str) -> Result<()> {
    conn.execute("DELETE FROM settings WHERE key = ?1", params![key])?;
    Ok(())
}

// ---------------------------------------------------------------- 隐藏任务

/// 建一条**隐藏任务**：不出现在列表 / 周条 / 计数 / 倒计时里，但**提醒照发**。
///
/// 上下班提醒就是这么落的：把四个时间点实体化成隐藏任务，于是白白继承了
/// 整套到点调度（幂等、退避重试、降级链、限流闸门、提前提醒、关机不补发）。
pub fn create_hidden_task(
    conn: &Connection,
    title: &str,
    body: &str,
    day: &str,
    due_at_ms: i64,
    remind_offsets: &str,
    repeat_rule: &str,
) -> Result<i64> {
    let ts = now_ms();
    conn.execute(
        "INSERT INTO tasks
           (title, body, day, due_at_ms, remind_offsets, repeat_rule, notify_channels,
            done, created_ms, updated_ms, hidden)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, 0, ?7, ?7, 1)",
        params![title, body, day, due_at_ms, remind_offsets, repeat_rule, ts],
    )?;
    Ok(conn.last_insert_rowid())
}

/// 清掉全部隐藏任务（改上下班设置时「先清后建」）。
///
/// `occurrences` 是 `ON DELETE CASCADE`，实例跟着一起走；
/// 建好之后由 `ensure_occurrences` 重新补齐未来 7 天的实例，
/// 已过点的（比如今天已经响过的那条）会被扫成 missed，不会重发。
pub fn delete_hidden_tasks(conn: &Connection) -> Result<usize> {
    let n = conn.execute("DELETE FROM tasks WHERE hidden = 1", [])?;
    Ok(n)
}

/// [`from_day`, `to_day`] 区间内每天的任务数量，**只包含有任务的那些天**。
///
/// - 没有任务的日期不会出现在返回值里（前端据此判断不画圆点），不要补 0 行。
/// - 周条上的圆点与列表必须一致，因此这里不能再自己写一条 `GROUP BY day` 的 SQL：
///   那条 SQL 看不见重复任务的展开，会出现「列表里有、圆点没有」。
/// - 能直接拿字符串比大小，是因为 `day` 一律是补零到 10 位的 `YYYY-MM-DD`
///   （`time::validate_day` 卡死了形状）——这种格式的文本序就是时间序。
///   一旦混进 `2026-2-1`，`"2026-10-01" > "2026-2-1"` 这种比较就会悄悄错。
pub fn count_by_day(conn: &Connection, from_day: &str, to_day: &str) -> Result<Vec<DayCount>> {
    let mut out = Vec::new();
    if from_day > to_day {
        return Ok(out);
    }
    let mut day = from_day.to_string();
    // 上限保护：界面只问一周，但接口能被直接调用，
    // 没有这个上限的话一个 from=0001-01-01 的请求会跑到天荒地老。
    for _ in 0..MAX_COUNT_DAYS {
        if day.as_str() > to_day {
            break;
        }
        let items = tasks_on_day(conn, &day)?;
        if !items.is_empty() {
            out.push(DayCount {
                day: day.clone(),
                total: items.len() as i64,
                undone: items.iter().filter(|t| !t.done).count() as i64,
            });
        }
        match time::shift_day(&day, 1) {
            Some(next) => day = next,
            None => break,
        }
    }
    Ok(out)
}

/// `count_by_day` 一次最多扫多少天。
const MAX_COUNT_DAYS: usize = 400;

pub fn get(conn: &Connection, id: i64) -> Result<Option<Task>> {
    let sql = format!("{SELECT_TASK} WHERE id = ?1");
    conn.query_row(&sql, params![id], row_to_task).optional()
}

pub fn create(conn: &Connection, input: &TaskInput) -> Result<Task> {
    let ts = now_ms();
    conn.execute(
        "INSERT INTO tasks
           (title, body, day, due_at_ms, remind_offsets, repeat_rule, notify_channels,
            done, created_ms, updated_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?8)",
        params![
            input.title,
            input.body,
            input.day,
            input.due_at_ms,
            input
                .remind_offsets
                .as_deref()
                .unwrap_or(DEFAULT_REMIND_OFFSETS),
            input.repeat_rule,
            input.notify_channels,
            ts
        ],
    )?;
    let id = conn.last_insert_rowid();
    get(conn, id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
}

/// `create` 本身不会为任务生成提醒实例（实例平时由 tick 里的 `ensure_occurrences` 补），
/// 所以「用户新建一个到期时刻已经过去的任务」这一路径必须显式收尾：
/// 让 `reset_occurrences_for_task` 把已过期的提醒点直接标成 missed。
/// 否则下一 tick 会把它当成“刚到点”，立刻弹一条过期提醒。
///
/// `commands::create_task` 用的就是这个函数；`create` 保留“只写 tasks 表”的纯语义，
/// 免得测试和数据迁移里用到它时被隐式生成实例。
pub fn create_with_occurrences(conn: &Connection, input: &TaskInput) -> Result<Task> {
    let task = create(conn, input)?;
    reset_occurrences_for_task(conn, task.id, now_ms())?;
    Ok(task)
}

/// 更新任务；返回 `None` 表示 id 不存在。
///
/// 时间或提醒点改了之后，旧的提醒实例必须重建，否则会按旧时间乱响——
/// 所以这里直接调 `reset_occurrences_for_task`，把一致性收在数据层，
/// 不指望调用方记得做。
pub fn update(conn: &Connection, id: i64, input: &TaskInput) -> Result<Option<Task>> {
    let ts = now_ms();
    let affected = conn.execute(
        // `notify_channels` 这里**没有** `COALESCE`（提醒点那边有）：
        // 那边的 `None` 意思是「没改、保持原值」，这边的 `None` 是一个真实取值「全部渠道」，
        // 用 `COALESCE` 就再也改不回「全部」了。界面每次都把当前选择一起发上来，所以直接覆盖。
        "UPDATE tasks SET title = ?1, body = ?2, day = ?3, due_at_ms = ?4, \
         repeat_rule = ?5, remind_offsets = COALESCE(?6, remind_offsets), \
         notify_channels = ?7, updated_ms = ?8 WHERE id = ?9",
        params![
            input.title,
            input.body,
            input.day,
            input.due_at_ms,
            input.repeat_rule,
            input.remind_offsets,
            input.notify_channels,
            ts,
            id
        ],
    )?;
    if affected == 0 {
        return Ok(None);
    }
    reset_occurrences_for_task(conn, id, ts)?;
    get(conn, id)
}

/// 勾选 / 取消勾选（整个任务），返回受影响行数（0 = id 不存在）。
///
/// 副作用（重要）：勾成完成时，把该任务还没到点的提醒实例作废（`skipped`），
/// 否则它们会永远挂在 `pending`，任务下次被取消勾选时又莫名其妙响起来。
/// 取消勾选时反过来重建实例。
///
/// 重复任务**不要走这里**（那会把整个系列停掉），界面对重复的行调 `set_occurrence_done`。
/// 导出用：所有**非隐藏**任务，按天升序（同一天里没定时刻的排最后）。
///
/// 不过滤 `done`：导出是「把用户的东西完整拿走」，已完成的历史也是他的数据。
pub fn all_tasks(conn: &Connection) -> Result<Vec<Task>> {
    let sql = format!(
        "{SELECT_TASK} WHERE hidden = 0 \
         ORDER BY day ASC, (due_at_ms IS NULL) ASC, due_at_ms ASC, id ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], row_to_task)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// 标 / 取消「重点」。返回受影响行数（0 = id 不存在）。
///
/// 单独一条命令而不是并进 `update`：编辑任务时前端只发 `TaskInput`，
/// 星标不在 `TaskInput` 里，所以改标题、改时间都不会把它冲掉。
pub fn set_star(conn: &Connection, id: i64, star: bool) -> Result<usize> {
    let flag: i64 = if star { 1 } else { 0 };
    let ts = now_ms();
    let affected = conn.execute(
        "UPDATE tasks SET star = ?1, updated_ms = ?2 WHERE id = ?3",
        params![flag, ts, id],
    )?;
    Ok(affected)
}

pub fn set_done(conn: &Connection, id: i64, done: bool) -> Result<usize> {
    let flag: i64 = if done { 1 } else { 0 };
    let ts = now_ms();
    let affected = conn.execute(
        "UPDATE tasks SET done = ?1, updated_ms = ?2 WHERE id = ?3",
        params![flag, ts, id],
    )?;
    if affected == 0 {
        return Ok(0);
    }
    if done {
        conn.execute(
            "UPDATE occurrences SET state = 'skipped' \
             WHERE task_id = ?1 AND state = 'pending'",
            params![id],
        )?;
    } else {
        reset_occurrences_for_task(conn, id, ts)?;
    }
    Ok(affected)
}

/// 按明确 id 列表删除（不接受通配/条件删除）。
pub fn delete_many(conn: &Connection, ids: &[i64]) -> Result<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("DELETE FROM tasks WHERE id IN ({placeholders})");
    conn.execute(&sql, rusqlite::params_from_iter(ids.iter()))
}

/// 勾选「完成本次」：只影响 `day` 这一天的实例，系列继续。
///
/// 和 `set_done`（完成整个任务，重复也就停了）是两件事，界面按 `Task.repeated` 分流。
///
/// 两种情形都要处理：
/// 1. 那天的实例已经生成 → 打上 `done_ms`，并把 `pending` 作废（`skipped`，免得还去发通知）；
/// 2. 还没生成（视野之外的日子）→ 直接补一条「已完成」的行，
///    否则界面勾了看起来毫无反应。`UNIQUE(task_id, fire_at_ms)` 保证不会插重。
pub fn set_occurrence_done(
    conn: &Connection,
    task_id: i64,
    day: &str,
    done: bool,
) -> Result<usize> {
    let (Some(lo), Some(hi)) = (time::day_start_ms(day), next_day_start_ms(day)) else {
        return Ok(0);
    };
    let ts = now_ms();
    if !done {
        // 取消勾选只把状态还原；已经过了的触发点**不能**变回 pending，
        // 否则下一 tick 会把它当成「刚到点」，立刻弹一条早就过去的提醒。
        return conn.execute(
            "UPDATE occurrences SET done_ms = NULL, \
                 state = CASE WHEN state = 'skipped' \
                              THEN (CASE WHEN fire_at_ms > ?1 THEN 'pending' ELSE 'missed' END) \
                              ELSE state END \
             WHERE task_id = ?2 AND fire_at_ms >= ?3 AND fire_at_ms < ?4 AND done_ms IS NOT NULL",
            params![ts, task_id, lo, hi],
        );
    }

    let n = conn.execute(
        "UPDATE occurrences SET done_ms = ?1, \
             state = CASE WHEN state IN ('pending','sending') THEN 'skipped' ELSE state END \
         WHERE task_id = ?2 AND fire_at_ms >= ?3 AND fire_at_ms < ?4 AND done_ms IS NULL",
        params![ts, task_id, lo, hi],
    )?;
    if n > 0 {
        return Ok(n);
    }

    // 那天还没有实例：补一条。用任务自己的时刻（重复任务沿用起点那天的时刻）。
    let base: Option<i64> = conn
        .query_row(
            "SELECT due_at_ms FROM tasks WHERE id = ?1",
            params![task_id],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten();
    let Some(base) = base else {
        return Ok(0); // 没定时刻的任务本来就不提醒，不需要记完成
    };
    let Some(fire_at) = time::ms_at_day_time(day, time::tod_ms(base)) else {
        return Ok(0);
    };
    conn.execute(
        "INSERT OR IGNORE INTO occurrences (task_id, fire_at_ms, state, done_ms) \
         VALUES (?1, ?2, ?3, ?4)",
        params![task_id, fire_at, OccurrenceState::Skipped.as_str(), ts],
    )?;
    Ok(1)
}

/// 停止重复：保留任务本身（还留在它自己那天），把规则清掉并清掉之后展开出来的实例。
pub fn stop_repeat(conn: &Connection, id: i64) -> Result<usize> {
    let ts = now_ms();
    let affected = conn.execute(
        "UPDATE tasks SET repeat_rule = NULL, updated_ms = ?1 \
         WHERE id = ?2 AND repeat_rule IS NOT NULL",
        params![ts, id],
    )?;
    if affected > 0 {
        // 复用「重建实例」：它会删掉未来的 pending/skipped，再按剩下的 due_at_ms 生成
        // 唯一那一个点（勾过「完成本次」的行带 done_ms，不会被删）
        reset_occurrences_for_task(conn, id, ts)?;
    }
    Ok(affected)
}

/// 一次「该发出去」的到点实例，带着展示所需的字段
#[derive(Debug, Clone, PartialEq)]
pub struct DueItem {
    pub occurrence_id: i64,
    pub task_id: i64,
    pub title: String,
    pub body: String,
    pub day: String,
    pub fire_at_ms: i64,
    /// true = 补发（唤醒后迟到，但在窗口内）
    pub late: bool,
}

/// 一轮认领的结果
#[derive(Debug, Default, Clone)]
pub struct ClaimResult {
    /// 抢到发送权的实例
    pub due: Vec<DueItem>,
    /// 判为错过的条数（已写库 + 写日志）
    pub missed: usize,
    /// 因为任务已完成/已作废而跳过的条数
    pub skipped: usize,
}

impl ClaimResult {
    /// 本次 tick 什么都没干。单测用它断言幂等（第二次认领必须为空）。
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.due.is_empty() && self.missed == 0 && self.skipped == 0
    }
}

// ---------- M2：实例生成 ----------

/// 取出该任务已经有的触发点。用来跳过重复插入。
///
/// 为什么不干脆一路 `INSERT OR IGNORE`：`ensure_occurrences` 每个 tick（2 秒）都会跑，
/// 被 IGNORE 掉的插入虽然不落数据，但**仍会推进 AUTOINCREMENT**——
/// 实测 40 秒里 occurrences.id 就从 1 白涨到 14；对每个提醒点白写一次也不必要。
/// 唯一索引仍然保留作为并发安全网。
fn existing_fire_times(conn: &Connection, task_id: i64) -> Result<HashSet<i64>> {
    let mut stmt = conn.prepare("SELECT fire_at_ms FROM occurrences WHERE task_id = ?1")?;
    let it = stmt.query_map(params![task_id], |r| r.get(0))?;
    it.collect()
}

/// 写一个触发点（已存在则什么都不做，靠唯一索引托底）。返回真的插入了的行数。
fn insert_fire_time(conn: &Connection, task_id: i64, fire_at_ms: i64) -> Result<usize> {
    conn.execute(
        "INSERT OR IGNORE INTO occurrences (task_id, fire_at_ms, state) VALUES (?1, ?2, ?3)",
        params![task_id, fire_at_ms, OccurrenceState::Pending.as_str()],
    )
}

/// 给单个任务补实例。窗口外的直接跳过（避免灌历史）。
fn insert_occurrences_for_task(
    conn: &Connection,
    task_id: i64,
    due_at_ms: i64,
    offsets_raw: &str,
    now_ms: i64,
) -> Result<usize> {
    let lo = now_ms - LOOKBACK_MS;
    let hi = now_ms + HORIZON_MS;
    let existing = existing_fire_times(conn, task_id)?;

    let mut inserted = 0usize;
    for secs in parse_remind_offsets(offsets_raw) {
        // 单位是秒，不是分钟（见 models::parse_remind_offsets 的注释）
        let fire_at = due_at_ms + secs * 1000;
        if fire_at < lo || fire_at > hi || existing.contains(&fire_at) {
            continue;
        }
        // 这里不会把「已经过去的 fire_at」直接写成 missed——必须保留 pending。
        // 因为唤醒补发靠的就是这种行：实例在睡着之前生成（那时 fire_at 还在未来，
        // 所以必然是 pending），然后在睡眠期间跨过了 fire_at，醒来才由
        // `claim_due(Wakeup)` 按迟到多久分档（窗口内 late_sent / 超窗 missed）。
        //
        // 「用户新建/编辑一个到期时刻已经过去的任务」是另一回事，那条路径走
        // `create_with_occurrences` / `reset_occurrences_for_task`，最后会把过期的 pending
        // 扫成 missed（两道门行为一致，不靠这里拦）。
        inserted += insert_fire_time(conn, task_id, fire_at)?;
    }
    Ok(inserted)
}

/// 重复任务的实例展开：把规则命中的**每一天**都变成一组触发点。
///
/// 只展开 `[now - LOOKBACK_MS, now + HORIZON_MS]` 这一小段（与普通任务一致）：
/// 调度器只管眼前这几天，更远的等它进窗口了再说，免得把几年的实例一次性灌进库里。
///
/// 已经勾过「完成本次」的那几天整天空着：不这样的话，提前提醒那个点会照旧补进来，
/// 出现「勾了还照样响」的幽灵提醒。
fn insert_repeat_occurrences(
    conn: &Connection,
    task_id: i64,
    start_day: &str,
    due_at_ms: i64,
    offsets_raw: &str,
    rule: &RepeatRule,
    now_ms: i64,
) -> Result<usize> {
    let lo = now_ms - LOOKBACK_MS;
    let hi = now_ms + HORIZON_MS;
    // 起点不能被窗口越过去：窗口起点晚于任务起点时，从窗口起点开始扫
    let from = {
        let window_start = time::day_of_ms(lo);
        if window_start.as_str() > start_day {
            window_start
        } else {
            start_day.to_string()
        }
    };
    let last = time::day_of_ms(hi);
    let existing = existing_fire_times(conn, task_id)?;
    let finished = done_day_starts(conn, task_id)?;
    let tod = time::tod_ms(due_at_ms);
    let offsets = parse_remind_offsets(offsets_raw);

    let mut inserted = 0usize;
    let mut day = from;
    for _ in 0..MAX_EXPAND_DAYS {
        if day > last {
            break;
        }
        let hit = rule.matches_day(start_day, &day)
            && time::day_start_ms(&day).is_some_and(|s| !finished.contains(&s));
        if hit {
            if let Some(base) = time::ms_at_day_time(&day, tod) {
                for secs in &offsets {
                    let fire_at = base + secs * 1000;
                    if fire_at < lo || fire_at > hi || existing.contains(&fire_at) {
                        continue;
                    }
                    inserted += insert_fire_time(conn, task_id, fire_at)?;
                }
            }
        }
        match time::shift_day(&day, 1) {
            Some(next) => day = next,
            None => break,
        }
    }
    Ok(inserted)
}

/// 展开区间的上限天数。视野只有几天，这里是防呆：
/// 万一 `shift_day` 出了什么问题，也不会变成死循环把调度线程卡死。
const MAX_EXPAND_DAYS: usize = 40;

/// `ensure_occurrences` / `reset_occurrences_for_task` 共用的任务快照。
struct DueTask {
    id: i64,
    day: String,
    due_at_ms: i64,
    offsets: String,
    rule: Option<RepeatRule>,
}

/// 按任务当前的设置补实例：普通任务只有一个触发时刻，重复任务要展开成多天。
fn insert_all_for_task(conn: &Connection, t: &DueTask, now_ms: i64) -> Result<usize> {
    match &t.rule {
        Some(rule) => {
            insert_repeat_occurrences(conn, t.id, &t.day, t.due_at_ms, &t.offsets, rule, now_ms)
        }
        None => insert_occurrences_for_task(conn, t.id, t.due_at_ms, &t.offsets, now_ms),
    }
}

/// 从一行查询结果拼出 `DueTask`（`repeat_rule` 解析不了就当成不重复）。
fn due_task(row: &rusqlite::Row<'_>) -> Result<DueTask> {
    let rule_raw: Option<String> = row.get(4)?;
    Ok(DueTask {
        id: row.get(0)?,
        day: row.get(1)?,
        due_at_ms: row.get(2)?,
        offsets: row.get(3)?,
        rule: rule_raw.as_deref().and_then(parse_repeat),
    })
}

/// 给「未完成 + 定了时刻」的任务补实例，返回新插入条数。可重复调用。
///
/// SQL 里那个 `OR ...` 不能省：重复任务的 `due_at_ms` 是**起点那天**的时刻，
/// 一个从半年前开始的「每周一」任务，它的 `due_at_ms` 早就不在窗口里了，
/// 但它的实例每一周都要继续生成。
pub fn ensure_occurrences(conn: &Connection, now_ms: i64) -> Result<usize> {
    let rows: Vec<DueTask> = {
        let mut stmt = conn.prepare(
            "SELECT id, day, due_at_ms, remind_offsets, repeat_rule FROM tasks \
             WHERE done = 0 AND due_at_ms IS NOT NULL \
               AND ((due_at_ms BETWEEN ?1 AND ?2) \
                    OR (repeat_rule IS NOT NULL AND repeat_rule <> ''))",
        )?;
        let it = stmt.query_map(params![now_ms - LOOKBACK_MS, now_ms + HORIZON_MS], due_task)?;
        it.collect::<Result<Vec<_>>>()?
    };

    let mut inserted = 0usize;
    for t in &rows {
        inserted += insert_all_for_task(conn, t, now_ms)?;
    }
    Ok(inserted)
}

/// 下一条「还没响、也不会被跳过」的提醒，给界面顶部的倒计时用。
#[derive(Debug, Clone, Serialize)]
pub struct NextReminder {
    pub task_id: i64,
    pub title: String,
    pub fire_at_ms: i64,
    pub day: String,
}

/// 取 `now_ms` 之后最早的一条待发提醒。
///
/// **为什么查 `occurrences` 而不是 `tasks.due_at_ms`**：任务可以配「提前 5 分钟」这种提醒点
/// （`remind_offsets` 里的负秒数），它比 `due_at_ms` **早**。只看 `due_at_ms` 会给出一个
/// 偏晚的时间，顶部就会把「马上就要响的那条」显示成「还早」。
///
/// 返回的 `day` 是**这次触发所在的那一天**（从 `fire_at_ms` 反解），不是任务的起始日：
/// 重复任务展开出来的实例跟起始日往往不是同一天，直接照抄 `tasks.day` 会对用户显示错日期。
///
/// 只认 `occurrences` 也意味着视线范围是 `HORIZON_MS`（7 天）——更远的事情界面上不预告，
/// 这是有意的：一个 8 天后的提醒显示成「7 天 23 小时后」没有意义。
pub fn next_reminder(conn: &Connection, now_ms: i64) -> Result<Option<NextReminder>> {
    let row = conn
        .query_row(
            "SELECT o.task_id, t.title, o.fire_at_ms \
               FROM occurrences o JOIN tasks t ON t.id = o.task_id \
              WHERE o.state = ?1 AND o.fire_at_ms >= ?2 AND t.done = 0 AND t.hidden = 0 \
              ORDER BY o.fire_at_ms ASC LIMIT 1",
            params![OccurrenceState::Pending.as_str(), now_ms],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    Ok(row.map(|(task_id, title, fire_at_ms)| NextReminder {
        task_id,
        title,
        fire_at_ms,
        day: time::day_of_ms(fire_at_ms),
    }))
}

/// 重建单个任务的实例（任务被编辑 / 取消完成 / 停止重复时用）。
///
/// 三步，**顺序不能换**：
/// 1. 未来还没决定的（`pending` / `skipped`）先删掉，否则旧时间点还会响；
/// 2. 按当前 `due_at_ms + remind_offsets`（重复任务再加上规则）重新生成；
/// 3. **最后**才把已经过期的 `pending` 扫成 `missed`。
///
/// 第 3 步必须放最后：如果放在第 2 步之前，第 2 步刚生成出来的过期实例
/// 会又变回 `pending`，用户只是改个标题就会被弹一条早就过期的提醒。
///
/// 两处 `done_ms IS NULL` 是给「完成本次」留的活口：勾过的那几天是**用户的操作记录**，
/// 改个标题就把它删掉／改成 missed，会出现「我明明勾了，怎么又变成没勾」。
pub fn reset_occurrences_for_task(conn: &Connection, task_id: i64, now_ms: i64) -> Result<usize> {
    conn.execute(
        "DELETE FROM occurrences \
         WHERE task_id = ?1 AND state IN (?2, ?3) AND fire_at_ms > ?4 AND done_ms IS NULL",
        params![
            task_id,
            OccurrenceState::Pending.as_str(),
            OccurrenceState::Skipped.as_str(),
            now_ms
        ],
    )?;

    // 任务已完成 / 已不存在就没必要重建
    let row = conn
        .query_row(
            "SELECT done, day, due_at_ms, remind_offsets, repeat_rule FROM tasks WHERE id = ?1",
            params![task_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()?;
    let inserted = match row {
        Some((0, day, Some(due_at), offsets, rule_raw)) => insert_all_for_task(
            conn,
            &DueTask {
                id: task_id,
                day,
                due_at_ms: due_at,
                offsets,
                rule: rule_raw.as_deref().and_then(parse_repeat),
            },
            now_ms,
        )?,
        _ => 0,
    };

    conn.execute(
        "UPDATE occurrences SET state = ?1 \
         WHERE task_id = ?2 AND state = ?3 AND fire_at_ms <= ?4 AND done_ms IS NULL",
        params![
            OccurrenceState::Missed.as_str(),
            task_id,
            OccurrenceState::Pending.as_str(),
            now_ms
        ],
    )?;
    Ok(inserted)
}

// ---------- M2：认领与状态流转 ----------

/// 认领所有到期实例。**这是幂等的核心**：
///
/// - `pending` → `sending`：靠 `UPDATE ... WHERE state = 'pending'` 的行数判定谁抢到了，
///   同一批实例第二次调用必然返回空，因此不会重复通知；
/// - `pending` → `missed` / `skipped`：直接落库并写 `send_log` 留痕。
///
/// 调用前请先 `ensure_occurrences`。返回的 `due` 里每一条都已经处于 `sending`，
/// 调用方必须负责收尾（`mark_sent` 或 `mark_skipped`），否则会卡在 `sending`。
pub fn claim_due(
    conn: &Connection,
    now_ms: i64,
    ctx: scheduler::TickContext,
    late_window_ms: i64,
) -> Result<ClaimResult> {
    let rows: Vec<(i64, i64, String, String, String, i64, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT o.id, o.task_id, t.title, t.body, t.day, o.fire_at_ms, t.done \
             FROM occurrences o JOIN tasks t ON t.id = o.task_id \
             WHERE o.state = ?1 AND o.fire_at_ms <= ?2 \
             ORDER BY o.fire_at_ms ASC, o.id ASC",
        )?;
        let it = stmt.query_map(params![OccurrenceState::Pending.as_str(), now_ms], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })?;
        it.collect::<Result<Vec<_>>>()?
    };

    let mut out = ClaimResult::default();
    for (occ_id, task_id, title, body, day, fire_at_ms, task_done) in rows {
        // 任务已经勾完成 → 作废，不算错过（不是我们漏发，是用户不要了）
        if task_done != 0 {
            let n = conn.execute(
                "UPDATE occurrences SET state = ?1 WHERE id = ?2 AND state = ?3",
                params![
                    OccurrenceState::Skipped.as_str(),
                    occ_id,
                    OccurrenceState::Pending.as_str()
                ],
            )?;
            if n == 1 {
                out.skipped += 1;
                log_event(
                    conn,
                    now_ms,
                    None,
                    Some(occ_id),
                    "skipped",
                    None,
                    Some("任务已完成"),
                )?;
            }
            continue;
        }

        let verdict = scheduler::classify(fire_at_ms, now_ms, ctx, late_window_ms);
        if !verdict.should_send() {
            let n = conn.execute(
                "UPDATE occurrences SET state = ?1 WHERE id = ?2 AND state = ?3",
                params![
                    OccurrenceState::Missed.as_str(),
                    occ_id,
                    OccurrenceState::Pending.as_str()
                ],
            )?;
            if n == 1 {
                out.missed += 1;
                let why = format!("错过：应触发 {fire_at_ms}，当前 {now_ms}，上下文 {ctx:?}");
                log_event(conn, now_ms, None, Some(occ_id), "missed", None, Some(&why))?;
            }
            continue;
        }

        let n = conn.execute(
            "UPDATE occurrences SET state = ?1 WHERE id = ?2 AND state = ?3",
            params![
                OccurrenceState::Sending.as_str(),
                occ_id,
                OccurrenceState::Pending.as_str()
            ],
        )?;
        if n == 1 {
            out.due.push(DueItem {
                occurrence_id: occ_id,
                task_id,
                title,
                body,
                day,
                fire_at_ms,
                late: verdict.is_late(),
            });
        }
    }
    Ok(out)
}

/// 冷启动扫描：把启动前还挂着的 `pending` 全部标 `missed`（**不补发**，用户明确要求）。
///
/// 直接复用 `claim_due` 的 `ColdStart` 分支，保证「冷启动不补发」这条规则
/// 只有一处实现，不会和 `claim_due` 走偏。
pub fn miss_overdue_on_start(conn: &Connection, now_ms: i64) -> Result<usize> {
    let r = claim_due(
        conn,
        now_ms,
        scheduler::TickContext::ColdStart,
        LATE_WINDOW_MS,
    )?;
    debug_assert!(
        r.due.is_empty(),
        "ColdStart 不该产生待发条目，实际 {} 条",
        r.due.len()
    );
    Ok(r.missed)
}

/// 发送完成后收尾。`late = true` 落 `late_sent`，否则 `sent`。
pub fn mark_sent(conn: &Connection, occ_id: i64, sent_ms: i64, late: bool) -> Result<usize> {
    let state = if late {
        OccurrenceState::LateSent
    } else {
        OccurrenceState::Sent
    };
    conn.execute(
        "UPDATE occurrences SET state = ?1, sent_ms = ?2 WHERE id = ?3 AND state = ?4",
        params![
            state.as_str(),
            sent_ms,
            occ_id,
            OccurrenceState::Sending.as_str()
        ],
    )
}

/// 发送不可行时收尾，避免实例永远卡在 `sending`
pub fn mark_skipped(conn: &Connection, occ_id: i64) -> Result<usize> {
    conn.execute(
        "UPDATE occurrences SET state = ?1 WHERE id = ?2 AND state = ?3",
        params![
            OccurrenceState::Skipped.as_str(),
            occ_id,
            OccurrenceState::Sending.as_str()
        ],
    )
}

/// 写一条发送日志。`channel_id` 为空表示没走到渠道（例如本地通知兜底）。
pub fn log_event(
    conn: &Connection,
    ts_ms: i64,
    channel_id: Option<i64>,
    occurrence_id: Option<i64>,
    result: &str,
    http_code: Option<i64>,
    detail: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO send_log (ts_ms, channel_id, occurrence_id, result, http_code, detail) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![ts_ms, channel_id, occurrence_id, result, http_code, detail],
    )?;
    Ok(())
}

// ---------- M3：渠道配置 ----------

/// 渠道。**不带凭证内容**，只说明「凭证是否已存」——避免 secret 被序列化回前端。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Channel {
    pub id: i64,
    pub name: String,
    /// 渠道类型（如 `feishu`），决定用哪个发送实现
    pub kind: String,
    pub enabled: bool,
    /// 越小越优先，主渠道 = 启用着的、priority 最小的那个
    pub priority: i64,
    pub config_json: String,
    pub secret_set: bool,
    /// 今天已经发出去几条（设置面板显示用）。
    /// `None` = 本次查询没算它（内部流转不需要），`Some(n)` = 今日已发 n 条。
    pub quota_today: Option<i64>,
}

/// 渠道 + 凭证明文（只给后端内部用，**不要**返回给前端）
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelRecord {
    pub channel: Channel,
    pub secret: Option<String>,
}

const CHANNEL_COLS: &str = "id, name, kind, enabled, priority, config_json, \
     (credential_enc IS NOT NULL AND length(credential_enc) > 0) AS secret_set";

fn row_to_channel(row: &rusqlite::Row<'_>) -> Result<Channel> {
    Ok(Channel {
        id: row.get(0)?,
        name: row.get(1)?,
        kind: row.get(2)?,
        enabled: row.get::<_, i64>(3)? != 0,
        priority: row.get(4)?,
        config_json: row.get(5)?,
        secret_set: row.get::<_, i64>(6)? != 0,
        // 这一列不是所有查询都会带，需要时用 `row_to_channel_with_quota`
        quota_today: None,
    })
}

/// 带「今日已发」的渠道行（`list_channels` 专用）
fn row_to_channel_with_quota(row: &rusqlite::Row<'_>) -> Result<Channel> {
    let mut ch = row_to_channel(row)?;
    ch.quota_today = Some(row.get(7)?);
    Ok(ch)
}

/// 全部渠道，按优先级升序（与降级链的顺序完全一致）。
/// 顺带带上「今日已发条数」：`channel_quota` 只有被显示出来才不是半截功能。
pub fn list_channels(conn: &Connection) -> Result<Vec<Channel>> {
    let sql = format!(
        "SELECT {CHANNEL_COLS}, COALESCE(q.sent, 0) AS quota_today \
         FROM channels c \
         LEFT JOIN channel_quota q ON q.channel_id = c.id AND q.day = ?1 \
         ORDER BY c.priority ASC, c.id ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let it = stmt.query_map(
        params![crate::time::today_local()],
        row_to_channel_with_quota,
    )?;
    it.collect()
}

/// 读一个渠道（含凭证），发送时用
pub fn channel_record(conn: &Connection, id: i64) -> Result<Option<ChannelRecord>> {
    let sql = format!("SELECT {CHANNEL_COLS}, credential_enc FROM channels WHERE id = ?1");
    conn.query_row(&sql, params![id], |row| {
        Ok(ChannelRecord {
            channel: row_to_channel(row)?,
            secret: row
                .get::<_, Option<Vec<u8>>>(7)?
                .map(|b| String::from_utf8_lossy(&b).to_string()),
        })
    })
    .optional()
}

/// 按 `name`（UNIQUE）落库：有则更新，无则新建，返回渠道 id。
///
/// `secret = None` 表示**不改动已存的凭证**（不是清空）——
/// 前端只改接收方时不该被迫重填 app_secret。清除凭证请显式用 [`clear_channel_secret`]。
pub fn upsert_channel(
    conn: &Connection,
    name: &str,
    kind: &str,
    enabled: bool,
    priority: i64,
    config_json: &str,
    secret: Option<&str>,
) -> Result<i64> {
    conn.query_row(
        "INSERT INTO channels (name, kind, enabled, priority, config_json, credential_enc) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT(name) DO UPDATE SET \
             kind = excluded.kind, \
             enabled = excluded.enabled, \
             priority = excluded.priority, \
             config_json = excluded.config_json, \
             credential_enc = COALESCE(excluded.credential_enc, channels.credential_enc) \
         RETURNING id",
        params![
            name,
            kind,
            i64::from(enabled),
            priority,
            config_json,
            secret.map(|s| s.as_bytes().to_vec()),
        ],
        |row| row.get(0),
    )
}

/// 清除某个渠道的凭证。
///
/// 目前只有测试用：界面上「改密钥」= 直接填一个新的（`upsert_channel` 传 `Some`），
/// 不需要先清空。真要「删掉密钥但保留渠道」的入口时再把它接到命令上。
#[cfg(test)]
pub fn clear_channel_secret(conn: &Connection, id: i64) -> Result<usize> {
    conn.execute(
        "UPDATE channels SET credential_enc = NULL WHERE id = ?1",
        params![id],
    )
}

pub fn set_channel_enabled(conn: &Connection, id: i64, enabled: bool) -> Result<usize> {
    conn.execute(
        "UPDATE channels SET enabled = ?1 WHERE id = ?2",
        params![i64::from(enabled), id],
    )
}

/// 设为主渠道：把自己的 `priority` 排到「其他渠道里最小的 priority」再往前 10。
///
/// 用「相对」而不是固定值（如 1）：用户可能自己改过优先级，
/// 直接拍一个 1 会和已有的 1 打平，降级链就变成看 id 了（不确定的行为）。
pub fn make_primary(conn: &Connection, id: i64) -> Result<usize> {
    conn.execute(
        "UPDATE channels SET priority = \
            (SELECT COALESCE(MIN(priority), 100) FROM channels WHERE id <> ?1) - 10 \
         WHERE id = ?1",
        params![id],
    )
}

/// 删一个渠道（只接受明确 id，不做通配删除）
pub fn delete_channel(conn: &Connection, id: i64) -> Result<usize> {
    conn.execute("DELETE FROM channels WHERE id = ?1", params![id])
}

/// 发送记录（面板用）
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LogEntry {
    pub ts_ms: i64,
    pub channel: Option<String>,
    pub result: String,
    pub http_code: Option<i64>,
    pub detail: Option<String>,
}

/// 最近的发送记录，新的在前
pub fn recent_logs(conn: &Connection, limit: i64) -> Result<Vec<LogEntry>> {
    let mut stmt = conn.prepare(
        "SELECT l.ts_ms, c.name, l.result, l.http_code, l.detail \
         FROM send_log l LEFT JOIN channels c ON c.id = l.channel_id \
         ORDER BY l.ts_ms DESC, l.id DESC LIMIT ?1",
    )?;
    let it = stmt.query_map(params![limit.clamp(1, 500)], |row| {
        Ok(LogEntry {
            ts_ms: row.get(0)?,
            channel: row.get(1)?,
            result: row.get(2)?,
            http_code: row.get(3)?,
            detail: row.get(4)?,
        })
    })?;
    it.collect()
}

/// 读某个任务的实例状态（测试与诊断用）
#[cfg(test)]
pub fn occurrence_states(conn: &Connection, task_id: i64) -> Result<Vec<(i64, i64, String)>> {
    let mut stmt = conn.prepare(
        "SELECT id, fire_at_ms, state FROM occurrences WHERE task_id = ?1 ORDER BY fire_at_ms",
    )?;
    let it = stmt.query_map(params![task_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    it.collect()
}

/// 发送日志条数（测试与诊断用）
#[cfg(test)]
pub fn log_count(conn: &Connection, result: &str) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM send_log WHERE result = ?1",
        params![result],
        |r| r.get(0),
    )
}

// ---------- M4：投递队列（push_jobs）+ 日发送量（channel_quota） ----------

/// 投递队列里一条待处理的作业（已 join 出提醒内容和渠道配置）
#[derive(Debug, Clone)]
pub struct JobItem {
    pub job_id: i64,
    pub occurrence_id: i64,
    pub task_id: i64,
    pub title: String,
    pub body: String,
    pub fire_at_ms: i64,
    /// true = 这条提醒本身是补发（休眠唤醒后迟到），不是「因为重试而晚了」
    pub late: bool,
    /// 已尝试次数（含失败和正在做的这一次）
    pub attempts: i64,
    pub channel: ChannelRecord,
}

/// `enabled_channels` 读出来的原始行：(id, name, kind, priority, config_json, credential_enc)
type ChannelRow = (i64, String, String, i64, String, Option<Vec<u8>>);

/// 所有启用中的渠道，按「优先级升序、id 升序」——这就是降级链的顺序
pub fn enabled_channels(conn: &Connection) -> Result<Vec<ChannelRecord>> {
    let rows: Vec<ChannelRow> = {
        let mut stmt = conn.prepare(
            "SELECT id, name, kind, priority, config_json, credential_enc \
             FROM channels WHERE enabled = 1 ORDER BY priority ASC, id ASC",
        )?;
        let it = stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?;
        it.collect::<Result<Vec<_>>>()?
    };
    Ok(rows
        .into_iter()
        .map(
            |(id, name, kind, priority, config_json, blob)| ChannelRecord {
                channel: Channel {
                    id,
                    name,
                    kind,
                    enabled: true,
                    priority,
                    config_json,
                    secret_set: blob.is_some(),
                    // 降级链不需要今日已发数：这里只关心「能不能用」
                    quota_today: None,
                },
                secret: blob.map(|b| String::from_utf8_lossy(&b).into_owned()),
            },
        )
        .collect())
}

/// 这个任务的提醒该发到哪些渠道：已启用渠道 ∩ 任务选定的类型，顺序仍是「优先级升序、id 升序」。
///
/// 任务没选（`notify_channels` 为空 / 读不出）时就是全部启用渠道 —— 跟这个功能上线前完全一样。
pub fn channels_for_task(conn: &Connection, task_id: i64) -> Result<Vec<ChannelRecord>> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT notify_channels FROM tasks WHERE id = ?1",
            params![task_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    let all = enabled_channels(conn)?;
    let Some(kinds) = parse_notify_kinds(raw.as_deref()) else {
        return Ok(all);
    };
    Ok(all
        .into_iter()
        .filter(|c| kinds.contains(&c.channel.kind))
        .collect())
}

/// 同上，按提醒实例查（实例 → 任务）。实例不存在时返回空表。
///
/// 投递和降级都必须走这个函数：降级只能往「任务允许的渠道」里降，
/// 否则用户明明选了「只发钉钉」，主渠道失败后会静静发到飞书去。
pub fn channels_for_occurrence(
    conn: &Connection,
    occurrence_id: i64,
) -> Result<Vec<ChannelRecord>> {
    let task_id: Option<i64> = conn
        .query_row(
            "SELECT task_id FROM occurrences WHERE id = ?1",
            params![occurrence_id],
            |r| r.get(0),
        )
        .optional()?;
    match task_id {
        Some(id) => channels_for_task(conn, id),
        None => Ok(Vec::new()),
    }
}

/// 入队一条投递作业。幂等靠 `dedupe_key = "{occurrence_id}:{channel_id}"` 的唯一约束，
/// 返回 `true` 表示真的插入了（`false` = 这条「实例 + 渠道」已经入过队）。
pub fn enqueue_job(
    conn: &Connection,
    occurrence_id: i64,
    channel_id: i64,
    late: bool,
    now_ms: i64,
) -> Result<bool> {
    let key = format!("{occurrence_id}:{channel_id}");
    let n = conn.execute(
        "INSERT OR IGNORE INTO push_jobs \
         (occurrence_id, channel_id, state, attempts, next_retry_ms, dedupe_key, late) \
         VALUES (?1, ?2, 'pending', 0, ?3, ?4, ?5)",
        params![occurrence_id, channel_id, now_ms, key, late as i64],
    )?;
    Ok(n == 1)
}

/// 这条实例已经入过队的渠道 id（降级时用来跳过已试过的渠道）
pub fn tried_channels(conn: &Connection, occurrence_id: i64) -> Result<Vec<i64>> {
    let mut stmt =
        conn.prepare("SELECT channel_id FROM push_jobs WHERE occurrence_id = ?1 ORDER BY id")?;
    let it = stmt.query_map(params![occurrence_id], |r| r.get(0))?;
    it.collect()
}

/// 到点该做的作业：`pending` 且 `next_retry_ms` 已到（或还没设）。
pub fn due_jobs(conn: &Connection, now_ms: i64) -> Result<Vec<JobItem>> {
    let mut stmt = conn.prepare(
        "SELECT j.id, j.occurrence_id, o.task_id, t.title, t.body, o.fire_at_ms, j.late, j.attempts, \
                c.id, c.name, c.kind, c.enabled, c.priority, c.config_json, c.credential_enc \
         FROM push_jobs j \
         JOIN occurrences o ON o.id = j.occurrence_id \
         JOIN tasks t       ON t.id = o.task_id \
         JOIN channels c    ON c.id = j.channel_id \
         WHERE j.state = 'pending' AND (j.next_retry_ms IS NULL OR j.next_retry_ms <= ?1) \
           AND o.state IN ('pending','sending') \
         ORDER BY j.next_retry_ms ASC, j.id ASC",
    )?;
    let it = stmt.query_map(params![now_ms], |r| {
        let blob: Option<Vec<u8>> = r.get(14)?;
        Ok(JobItem {
            job_id: r.get(0)?,
            occurrence_id: r.get(1)?,
            task_id: r.get(2)?,
            title: r.get(3)?,
            body: r.get(4)?,
            fire_at_ms: r.get(5)?,
            late: r.get::<_, i64>(6)? != 0,
            attempts: r.get(7)?,
            channel: ChannelRecord {
                channel: Channel {
                    id: r.get(8)?,
                    name: r.get(9)?,
                    kind: r.get(10)?,
                    enabled: r.get::<_, i64>(11)? != 0,
                    priority: r.get(12)?,
                    config_json: r.get(13)?,
                    secret_set: blob.is_some(),
                    quota_today: None,
                },
                secret: blob.map(|b| String::from_utf8_lossy(&b).into_owned()),
            },
        })
    })?;
    it.collect()
}

/// 认领一条作业（`pending → sending`）。返回 `false` = 已被别人抢走。
///
/// **不在这里 +1 次数**：次数要等真发完再记，否则「因为限流被推迟」会凭空消耗一次重试机会。
pub fn claim_job(conn: &Connection, job_id: i64) -> Result<bool> {
    let n = conn.execute(
        "UPDATE push_jobs SET state = 'sending' WHERE id = ?1 AND state = 'pending'",
        params![job_id],
    )?;
    Ok(n == 1)
}

/// 限流推迟：放回队列等窗口空出来。**不算一次尝试**（一次都没发出去）。
pub fn job_defer(conn: &Connection, job_id: i64, next_retry_ms: i64) -> Result<usize> {
    conn.execute(
        "UPDATE push_jobs SET state = 'pending', next_retry_ms = ?2 WHERE id = ?1",
        params![job_id, next_retry_ms],
    )
}

/// 作业成功
pub fn job_sent(conn: &Connection, job_id: i64) -> Result<usize> {
    conn.execute(
        "UPDATE push_jobs SET state = 'sent', attempts = attempts + 1, \
                last_error = NULL, next_retry_ms = NULL WHERE id = ?1",
        params![job_id],
    )
}

/// 本次失败，但安排重试
pub fn job_reschedule(
    conn: &Connection,
    job_id: i64,
    next_retry_ms: i64,
    last_error: &str,
) -> Result<usize> {
    conn.execute(
        "UPDATE push_jobs SET state = 'pending', attempts = attempts + 1, \
                next_retry_ms = ?2, last_error = ?3 WHERE id = ?1",
        params![job_id, next_retry_ms, last_error],
    )
}

/// 这个作业彻底失败（不再重试）
pub fn job_failed(conn: &Connection, job_id: i64, last_error: &str) -> Result<usize> {
    conn.execute(
        "UPDATE push_jobs SET state = 'failed', attempts = attempts + 1, \
                next_retry_ms = NULL, last_error = ?2 WHERE id = ?1",
        params![job_id, last_error],
    )
}

/// 这条实例已经在别的渠道上发成功了 —— 把其它还没发的作业作废，
/// 否则降级链一旦开出多条支线，用户会收到好几遍同一条提醒。
pub fn cancel_jobs_for_occurrence(
    conn: &Connection,
    occurrence_id: i64,
    keep_job_id: i64,
) -> Result<usize> {
    conn.execute(
        "UPDATE push_jobs SET state = 'skipped', next_retry_ms = NULL, \
                last_error = '同一实例已在其它渠道发成功' \
         WHERE occurrence_id = ?1 AND id <> ?2 AND state IN ('pending','sending')",
        params![occurrence_id, keep_job_id],
    )
}

/// 启动时收拾「上次运行中断」留下的残局。
///
/// 返回 `(作废的作业数, 落 missed 的实例数)`。
/// **不重发**：用户明确要求「关机后不要再提醒」，所以卡住的作业直接作废，
/// 只把现场写清楚（否则 `sending` 会永久悬挂，谁也看不到）。
pub fn recover_stuck(conn: &Connection, now_ms: i64) -> Result<(usize, usize)> {
    let jobs = conn.execute(
        "UPDATE push_jobs SET state = 'failed', next_retry_ms = NULL, \
                last_error = COALESCE(last_error, '上次运行中断（重启不补发）') \
         WHERE state = 'sending'",
        [],
    )?;
    let occs = conn.execute(
        "UPDATE occurrences SET state = ?1 WHERE state = ?2",
        params![
            OccurrenceState::Missed.as_str(),
            OccurrenceState::Sending.as_str()
        ],
    )?;
    if jobs > 0 || occs > 0 {
        log_event(
            conn,
            now_ms,
            None,
            None,
            "missed",
            None,
            Some(&format!(
                "启动清理：{occs} 条实例停在 sending、{jobs} 条作业被中断 → 不补发"
            )),
        )?;
    }
    Ok((jobs, occs))
}

/// 日发送量 +1（`channel_quota` 的主键是 `(channel_id, day)`）
pub fn bump_quota(conn: &Connection, channel_id: i64, day: &str, n: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO channel_quota (channel_id, day, sent) VALUES (?1, ?2, ?3) \
         ON CONFLICT(channel_id, day) DO UPDATE SET sent = sent + excluded.sent",
        params![channel_id, day, n],
    )?;
    Ok(())
}

/// 读某天已发条数（单测与诊断用；界面上的「今日已发」由 `list_channels` 的 JOIN 直接带出）
#[cfg(test)]
pub fn quota_for_day(conn: &Connection, channel_id: i64, day: &str) -> Result<i64> {
    conn.query_row(
        "SELECT COALESCE(sent, 0) FROM channel_quota WHERE channel_id = ?1 AND day = ?2",
        params![channel_id, day],
        |r| r.get(0),
    )
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(0),
        other => Err(other),
    })
}

/// 队列里还没完成的作业数（今日汇总面板用）
#[cfg(test)]
pub fn pending_job_count(conn: &Connection) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM push_jobs WHERE state = 'pending'",
        [],
        |r| r.get(0),
    )
}

/// 读一条作业（测试与诊断用）
#[cfg(test)]
pub fn job_row(
    conn: &Connection,
    job_id: i64,
) -> Result<(String, i64, Option<i64>, Option<String>)> {
    conn.query_row(
        "SELECT state, attempts, next_retry_ms, last_error FROM push_jobs WHERE id = ?1",
        params![job_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().expect("打开内存库失败");
        migrate(&conn).expect("迁移失败");
        conn
    }

    fn input(title: &str, day: &str, due: Option<i64>) -> TaskInput {
        TaskInput {
            title: title.to_string(),
            body: String::new(),
            day: day.to_string(),
            due_at_ms: due,
            repeat_rule: None,
            remind_offsets: None,
            notify_channels: None,
        }
    }

    #[test]
    fn migrate_is_idempotent() {
        let conn = mem();
        migrate(&conn).expect("第二次迁移失败");
        let v: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
                r.get(0)
            })
            .expect("读取版本失败");
        assert_eq!(
            v, SCHEMA_VERSION,
            "迁移版本应当是 {SCHEMA_VERSION}，实际 {v}"
        );
        // 版本号对得上不代表「可反复执行」，再跑两次确认真幂等
        migrate(&conn).expect("第三次迁移失败");
        migrate(&conn).expect("第四次迁移失败");
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
            .expect("统计迁移记录失败");
        assert_eq!(
            rows, SCHEMA_VERSION,
            "反复迁移不应重复写记录，实际 {rows} 条"
        );
    }

    #[test]
    fn create_then_list_only_that_day() {
        let conn = mem();
        let a = create(&conn, &input("今天的事", "2026-02-01", None)).expect("插入失败");
        create(&conn, &input("明天的事", "2026-02-02", None)).expect("插入失败");

        let got = list_by_day(&conn, "2026-02-01").expect("查询失败");
        assert_eq!(got.len(), 1, "只应查到 1 条，实际 {} 条", got.len());
        assert_eq!(got[0].id, a.id, "查到的 id 不匹配");
        assert_eq!(got[0].remind_offsets, "[0]", "默认提醒点应当是 [0]");
        assert!(!got[0].done, "新任务不应是已完成");
    }

    #[test]
    fn list_orders_by_time_with_nulls_last() {
        let conn = mem();
        create(&conn, &input("无时刻", "2026-02-01", None)).expect("插入失败");
        create(&conn, &input("晚", "2026-02-01", Some(1_770_000_000_000))).expect("插入失败");
        create(&conn, &input("早", "2026-02-01", Some(1_760_000_000_000))).expect("插入失败");

        let titles: Vec<String> = list_by_day(&conn, "2026-02-01")
            .expect("查询失败")
            .into_iter()
            .map(|t| t.title)
            .collect();
        assert_eq!(
            titles,
            vec!["早".to_string(), "晚".to_string(), "无时刻".to_string()],
            "排序应当是 早 < 晚 < 无时刻，实际 {titles:?}"
        );
    }

    #[test]
    fn update_and_missing_id() {
        let conn = mem();
        let t = create(&conn, &input("原标题", "2026-02-01", None)).expect("插入失败");

        let updated = update(&conn, t.id, &input("新标题", "2026-02-03", Some(123)))
            .expect("更新失败")
            .expect("应当返回更新后的任务");
        assert_eq!(updated.title, "新标题", "标题未更新");
        assert_eq!(updated.day, "2026-02-03", "日期未更新");
        assert_eq!(updated.due_at_ms, Some(123), "时间未更新");
        assert_eq!(updated.created_ms, t.created_ms, "created_ms 不该被修改");

        let missing = update(&conn, 99_999, &input("x", "2026-02-01", None))
            .expect("更新不存在的 id 不应报错");
        assert!(missing.is_none(), "不存在的 id 应当返回 None");
    }

    #[test]
    fn done_flag_roundtrip() {
        let conn = mem();
        let t = create(&conn, &input("任务", "2026-02-01", None)).expect("插入失败");

        assert_eq!(set_done(&conn, t.id, true).expect("置为完成失败"), 1);
        assert!(
            get(&conn, t.id).expect("查询失败").expect("应当存在").done,
            "done 应当为 true"
        );

        assert_eq!(set_done(&conn, t.id, false).expect("取消完成失败"), 1);
        assert!(
            !get(&conn, t.id).expect("查询失败").expect("应当存在").done,
            "done 应当为 false"
        );
        assert_eq!(
            set_done(&conn, 99_999, true).expect("置为完成不应报错"),
            0,
            "不存在的 id 应当返回 0 行"
        );
    }

    #[test]
    fn delete_many_is_explicit_only() {
        let conn = mem();
        let a = create(&conn, &input("A", "2026-02-01", None)).expect("插入失败");
        let b = create(&conn, &input("B", "2026-02-01", None)).expect("插入失败");
        create(&conn, &input("C", "2026-02-01", None)).expect("插入失败");

        assert_eq!(delete_many(&conn, &[a.id, b.id]).expect("删除失败"), 2);
        let left = list_by_day(&conn, "2026-02-01").expect("查询失败");
        assert_eq!(left.len(), 1, "应当只剩 1 条，实际 {} 条", left.len());
        assert_eq!(left[0].title, "C", "剩下的应当是 C");
        assert_eq!(delete_many(&conn, &[]).expect("空列表不应报错"), 0);
    }

    #[test]
    fn day_column_format_is_uniform() {
        let conn = mem();
        create(&conn, &input("任务", "2026-02-01", None)).expect("插入失败");
        let day: String = conn
            .query_row("SELECT day FROM tasks LIMIT 1", [], |r| r.get(0))
            .expect("读取 day 失败");
        assert_eq!(day, "2026-02-01", "day 必须存成 YYYY-MM-DD，实际 {day}");
        assert_eq!(day.len(), 10, "day 长度必须是 10，实际 {day}");
    }

    // ---------- M2 ----------

    use crate::scheduler::{TickContext, LATE_WINDOW_MS};

    /// 造一个「due 在未来 due_in_ms 毫秒、提醒点为 offsets」的任务
    fn task_at(conn: &Connection, title: &str, due_in_ms: i64, offsets: &str) -> i64 {
        let t = create(
            conn,
            &TaskInput {
                title: title.to_string(),
                body: "正文内容".to_string(),
                day: "2026-02-01".to_string(),
                due_at_ms: Some(now_ms() + due_in_ms),
                repeat_rule: None,
                remind_offsets: Some(offsets.to_string()),
                notify_channels: None,
            },
        )
        .expect("插入任务失败");
        t.id
    }

    #[test]
    fn creating_a_task_with_past_due_never_fires_it() {
        let conn = mem();
        // 用户新建一个「10 分钟前就该提醒」的任务（比如补录的旧任务），
        // 走 `create_with_occurrences`（commands::create_task 用的就是它）。
        let task = create_with_occurrences(
            &conn,
            &TaskInput {
                title: "补录的旧任务".to_string(),
                body: String::new(),
                day: "2026-02-01".to_string(),
                due_at_ms: Some(now_ms() - 600_000),
                repeat_rule: None,
                remind_offsets: None,
                notify_channels: None,
            },
        )
        .expect("新建失败");

        let states = occurrence_states(&conn, task.id).expect("查询失败");
        assert_eq!(states.len(), 1, "应当生成 1 条实例，实际 {states:?}");
        assert_eq!(
            states[0].2, "missed",
            "新建时已过期的提醒点必须直接落 missed，不能挂成 pending（否则下一 tick 立刻弹一条过期提醒）"
        );

        let claimed =
            claim_due(&conn, now_ms(), TickContext::Steady, LATE_WINDOW_MS).expect("认领失败");
        assert!(
            claimed.is_empty(),
            "落成 missed 的实例不该被认领发送，实际 due={} missed={} skipped={}",
            claimed.due.len(),
            claimed.missed,
            claimed.skipped
        );
    }

    #[test]
    fn repeated_ticks_do_not_burn_occurrence_ids() {
        let conn = mem();
        task_at(&conn, "常驻任务", 3_600_000, "[0]");
        ensure_occurrences(&conn, now_ms()).expect("首次生成失败");
        let max_before: i64 = conn
            .query_row("SELECT MAX(id) FROM occurrences", [], |r| r.get(0))
            .expect("查 MAX(id) 失败");

        for _ in 0..5 {
            assert_eq!(
                ensure_occurrences(&conn, now_ms()).expect("重复 tick 失败"),
                0,
                "重复 tick 不应当插入实例"
            );
        }

        let new_task = task_at(&conn, "新任务", 3_600_000, "[0]");
        ensure_occurrences(&conn, now_ms()).expect("生成新任务实例失败");
        let new_id: i64 = conn
            .query_row(
                "SELECT id FROM occurrences WHERE task_id = ?1",
                params![new_task],
                |r| r.get(0),
            )
            .expect("查新实例失败");
        assert_eq!(
            new_id,
            max_before + 1,
            "空跑的 tick 不应当白白推进 AUTOINCREMENT（旧实现每 tick 都重插一遍再被 IGNORE，把 id 撑大了）"
        );
    }

    #[test]
    fn ensure_occurrences_is_idempotent() {
        let conn = mem();
        let id = task_at(&conn, "任务", 3_600_000, "[0,-300]");

        let first = ensure_occurrences(&conn, now_ms()).expect("首次生成失败");
        assert_eq!(first, 2, "两个提醒点应当生成 2 条实例，实际 {first}");

        let second = ensure_occurrences(&conn, now_ms()).expect("二次生成失败");
        assert_eq!(second, 0, "重复生成不应插入新实例，实际插了 {second} 条");
        assert_eq!(
            occurrence_states(&conn, id).expect("查询失败").len(),
            2,
            "总共应当只有 2 条实例"
        );
    }

    #[test]
    fn ensure_skips_done_tasks_and_far_future() {
        let conn = mem();
        let done_id = task_at(&conn, "已完成", 3_600_000, "[0]");
        set_done(&conn, done_id, true).expect("置完成失败");
        task_at(&conn, "太远了", 8 * 24 * 3_600_000, "[0]");

        let inserted = ensure_occurrences(&conn, now_ms()).expect("生成失败");
        assert_eq!(inserted, 0, "已完成和超窗口的任务都不该生成实例");
    }

    #[test]
    fn claim_due_is_idempotent() {
        let conn = mem();
        let id = task_at(&conn, "任务", -1_000, "[0]");
        ensure_occurrences(&conn, now_ms()).expect("生成失败");

        let first =
            claim_due(&conn, now_ms(), TickContext::Steady, LATE_WINDOW_MS).expect("首次认领失败");
        assert_eq!(first.due.len(), 1, "首次应当认领到 1 条");
        assert_eq!(first.due[0].title, "任务", "认领条目应当带上标题");
        assert_eq!(first.due[0].body, "正文内容", "认领条目应当带上正文");
        assert!(!first.due[0].late, "稳态下不该标成补发");

        let second =
            claim_due(&conn, now_ms(), TickContext::Steady, LATE_WINDOW_MS).expect("二次认领失败");
        assert!(
            second.is_empty(),
            "第二次认领必须为空（防重复轰炸），实际 due={} missed={} skipped={}",
            second.due.len(),
            second.missed,
            second.skipped
        );

        let states = occurrence_states(&conn, id).expect("查询失败");
        assert_eq!(states[0].2, "sending", "认领后应当处于 sending");
    }

    #[test]
    fn mark_sent_records_late_flag() {
        let conn = mem();
        let id = task_at(&conn, "任务", -1_000, "[0]");
        ensure_occurrences(&conn, now_ms()).expect("生成失败");
        let due = claim_due(&conn, now_ms(), TickContext::Steady, LATE_WINDOW_MS)
            .expect("认领失败")
            .due;

        assert_eq!(
            mark_sent(&conn, due[0].occurrence_id, now_ms(), false).expect("标记失败"),
            1,
            "应当更新 1 行"
        );
        let states = occurrence_states(&conn, id).expect("查询失败");
        assert_eq!(states[0].2, "sent", "正常发送应当落 sent");
        assert_eq!(
            mark_sent(&conn, due[0].occurrence_id, now_ms(), false).expect("重复标记不应报错"),
            0,
            "已经不是 sending 状态，重复标记应当影响 0 行"
        );
    }

    #[test]
    fn wakeup_late_window_marks_late_sent_but_long_late_is_missed() {
        let conn = mem();
        let late_id = task_at(&conn, "小迟到", -60_000, "[0]");
        let miss_id = task_at(&conn, "大迟到", -8 * 3_600_000, "[0]");
        ensure_occurrences(&conn, now_ms()).expect("生成失败");

        let r = claim_due(&conn, now_ms(), TickContext::Wakeup, LATE_WINDOW_MS).expect("认领失败");
        assert_eq!(r.due.len(), 1, "只该有 1 条待发，实际 {}", r.due.len());
        assert!(r.due[0].late, "迟到 1 分钟在窗口内，应当标补发");
        assert_eq!(r.missed, 1, "迟到 8 小时应当判 missed");

        mark_sent(&conn, r.due[0].occurrence_id, now_ms(), true).expect("标记失败");
        assert_eq!(
            occurrence_states(&conn, late_id).expect("查询失败")[0].2,
            "late_sent",
            "补发应当落 late_sent"
        );
        assert_eq!(
            occurrence_states(&conn, miss_id).expect("查询失败")[0].2,
            "missed",
            "超窗应当落 missed"
        );
        assert_eq!(
            log_count(&conn, "missed").expect("统计失败"),
            1,
            "应当留 1 条 missed 日志"
        );
    }

    #[test]
    fn cold_start_never_backfills_and_writes_log() {
        let conn = mem();
        let id = task_at(&conn, "关机期间到点", -600_000, "[0]");
        ensure_occurrences(&conn, now_ms()).expect("生成失败");

        let missed = miss_overdue_on_start(&conn, now_ms()).expect("冷启动扫描失败");
        assert_eq!(missed, 1, "冷启动应当把 1 条过期实例标 missed");
        assert_eq!(
            occurrence_states(&conn, id).expect("查询失败")[0].2,
            "missed",
            "冷启动即使只迟到 10 分钟也不补发"
        );
        assert_eq!(log_count(&conn, "missed").expect("统计失败"), 1);
        assert_eq!(
            log_count(&conn, "sent").expect("统计失败"),
            0,
            "冷启动不该发出任何东西"
        );
    }

    #[test]
    fn startup_order_generates_then_sweeps() {
        // 真实启动顺序：先 ensure 生成，再一次性扫掉过期的
        let conn = mem();
        let past = task_at(&conn, "刚过期", -1_000, "[0]");
        let future = task_at(&conn, "还没到", 3_600_000, "[0]");

        ensure_occurrences(&conn, now_ms()).expect("生成失败");
        let missed = miss_overdue_on_start(&conn, now_ms()).expect("扫描失败");

        assert_eq!(missed, 1, "只该扫掉过期的 1 条");
        assert_eq!(
            occurrence_states(&conn, past).expect("查询失败")[0].2,
            "missed"
        );
        assert_eq!(
            occurrence_states(&conn, future).expect("查询失败")[0].2,
            "pending",
            "未来的实例必须保持 pending，否则启动就把它吃掉了"
        );
    }

    #[test]
    fn set_done_skips_pending_then_undo_rebuilds() {
        let conn = mem();
        let id = task_at(&conn, "任务", 3_600_000, "[0]");
        ensure_occurrences(&conn, now_ms()).expect("生成失败");

        set_done(&conn, id, true).expect("置完成失败");
        assert_eq!(
            occurrence_states(&conn, id).expect("查询失败")[0].2,
            "skipped",
            "勾完成后未到点的实例应当作废"
        );

        set_done(&conn, id, false).expect("取消完成失败");
        assert_eq!(
            occurrence_states(&conn, id).expect("查询失败")[0].2,
            "pending",
            "取消完成后应当重建出 pending 实例，否则提醒永久丢失"
        );
        assert_eq!(
            occurrence_states(&conn, id).expect("查询失败").len(),
            1,
            "重建不该留重复实例"
        );
    }

    #[test]
    fn update_time_rebuilds_occurrences() {
        let conn = mem();
        let id = task_at(&conn, "任务", 3_600_000, "[0]");
        ensure_occurrences(&conn, now_ms()).expect("生成失败");
        let old_fire = occurrence_states(&conn, id).expect("查询失败")[0].1;

        let new_due = now_ms() + 7_200_000;
        update(
            &conn,
            id,
            &TaskInput {
                title: "任务".to_string(),
                body: String::new(),
                day: "2026-02-01".to_string(),
                due_at_ms: Some(new_due),
                repeat_rule: None,
                remind_offsets: None,
                notify_channels: None,
            },
        )
        .expect("更新失败")
        .expect("应当返回任务");

        let states = occurrence_states(&conn, id).expect("查询失败");
        assert_eq!(
            states.len(),
            1,
            "改时间后不该留下旧的实例，实际 {} 条",
            states.len()
        );
        assert_eq!(states[0].1, new_due, "实例应当跟着新时间走");
        assert_ne!(states[0].1, old_fire, "旧时间点必须被清掉");
        assert_eq!(states[0].2, "pending", "重建后应当是 pending");
    }

    #[test]
    fn editing_a_task_with_past_due_does_not_fire_it() {
        let conn = mem();
        let id = task_at(&conn, "任务", 3_600_000, "[0]");
        ensure_occurrences(&conn, now_ms()).expect("生成失败");

        // 用户把时间改到 10 分钟前
        update(
            &conn,
            id,
            &TaskInput {
                title: "任务".to_string(),
                body: String::new(),
                day: "2026-02-01".to_string(),
                due_at_ms: Some(now_ms() - 600_000),
                repeat_rule: None,
                remind_offsets: None,
                notify_channels: None,
            },
        )
        .expect("更新失败");

        let states = occurrence_states(&conn, id).expect("查询失败");
        assert_eq!(states[0].2, "missed", "改到过去的时间点应当直接算错过");

        let r = claim_due(&conn, now_ms(), TickContext::Steady, LATE_WINDOW_MS).expect("认领失败");
        assert!(
            r.is_empty(),
            "改完时间后不该立刻弹一条过期提醒，实际 due={}",
            r.due.len()
        );
    }

    #[test]
    fn deleting_task_cascades_occurrences() {
        let conn = mem();
        let id = task_at(&conn, "任务", 3_600_000, "[0]");
        ensure_occurrences(&conn, now_ms()).expect("生成失败");

        delete_many(&conn, &[id]).expect("删除失败");
        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM occurrences", [], |r| r.get(0))
            .expect("统计失败");
        assert_eq!(left, 0, "删任务应当级联删掉实例，实际还剩 {left} 条");
    }

    // ---------- M3：渠道 ----------

    fn feishu_json(receive_id: &str) -> String {
        format!(
            r#"{{"app_id":"cli_test","receive_id_type":"chat_id","receive_id":"{receive_id}"}}"#
        )
    }

    #[test]
    fn upsert_channel_creates_then_updates_same_row() {
        let conn = mem();
        let id = upsert_channel(
            &conn,
            "飞书",
            "feishu",
            true,
            100,
            &feishu_json("oc_1"),
            Some("sec"),
        )
        .expect("首次写入失败");
        let id2 = upsert_channel(
            &conn,
            "飞书",
            "feishu",
            false,
            50,
            &feishu_json("oc_2"),
            None,
        )
        .expect("二次写入失败");

        assert_eq!(id, id2, "同名渠道必须更新同一行，不能新建");
        let all = list_channels(&conn).expect("查询失败");
        assert_eq!(all.len(), 1, "应当只有 1 个渠道，实际 {}", all.len());
        assert_eq!(all[0].config_json, feishu_json("oc_2"), "配置应被覆盖");
        assert!(!all[0].enabled, "enabled 应被覆盖");
        assert_eq!(all[0].priority, 50, "priority 应被覆盖");
        assert!(all[0].secret_set, "传 None 时不应清掉已存的密钥");
    }

    #[test]
    fn channel_record_returns_secret_but_list_does_not() {
        let conn = mem();
        let id = upsert_channel(
            &conn,
            "飞书",
            "feishu",
            true,
            100,
            &feishu_json("oc_1"),
            Some("s3cr3t"),
        )
        .expect("写入失败");

        let rec = channel_record(&conn, id)
            .expect("查询失败")
            .expect("渠道应当存在");
        assert_eq!(
            rec.secret.as_deref(),
            Some("s3cr3t"),
            "内部读取应拿到明文密钥"
        );

        // 对外结构（会被序列化回前端）里不能出现密钥字段
        let json =
            serde_json::to_string(&list_channels(&conn).expect("查询失败")).expect("序列化失败");
        assert!(
            !json.contains("s3cr3t"),
            "渠道列表绝不能带出密钥内容，实际：{json}"
        );
        assert!(
            json.contains("secret_set"),
            "应只暴露「密钥是否已设」，实际：{json}"
        );
    }

    #[test]
    fn clear_channel_secret_only_clears_that_row() {
        let conn = mem();
        let a = upsert_channel(
            &conn,
            "飞书",
            "feishu",
            true,
            100,
            &feishu_json("oc_1"),
            Some("sec_a"),
        )
        .expect("写入失败");
        upsert_channel(
            &conn,
            "钉子",
            "feishu",
            true,
            200,
            &feishu_json("oc_2"),
            Some("sec_b"),
        )
        .expect("写入失败");

        clear_channel_secret(&conn, a).expect("清除失败");
        let all = list_channels(&conn).expect("查询失败");
        assert!(!all[0].secret_set, "被清除的那行应变成无密钥");
        assert!(all[1].secret_set, "另一行的密钥不该被牵连");
    }

    #[test]
    fn enabled_channels_follows_priority_and_skips_disabled() {
        let conn = mem();
        assert!(
            enabled_channels(&conn).expect("查询失败").is_empty(),
            "没有渠道时应返回空表"
        );

        upsert_channel(
            &conn,
            "禁用但更优先",
            "feishu",
            false,
            1,
            &feishu_json("oc_a"),
            Some("s"),
        )
        .expect("写入失败");
        let second = upsert_channel(
            &conn,
            "次优先",
            "feishu",
            true,
            200,
            &feishu_json("oc_b"),
            Some("s"),
        )
        .expect("写入失败");
        let best = upsert_channel(
            &conn,
            "主渠道",
            "feishu",
            true,
            10,
            &feishu_json("oc_c"),
            Some("s"),
        )
        .expect("写入失败");

        let chain = enabled_channels(&conn).expect("查询失败");
        assert_eq!(chain.len(), 2, "降级链应当只含启用的渠道");
        assert_eq!(chain[0].channel.id, best, "头一个应是 priority 最小的");
        assert_eq!(
            chain[0].channel.config_json,
            feishu_json("oc_c"),
            "头一个应是 oc_c"
        );
        assert_eq!(chain[1].channel.id, second, "第二个应是次优先");
        assert_eq!(chain[0].secret.as_deref(), Some("s"), "降级链要带凭证");
    }

    #[test]
    fn delete_channel_removes_only_that_row() {
        let conn = mem();
        let a = upsert_channel(
            &conn,
            "A",
            "feishu",
            true,
            100,
            &feishu_json("oc_a"),
            Some("s"),
        )
        .expect("写入失败");
        upsert_channel(
            &conn,
            "B",
            "feishu",
            true,
            200,
            &feishu_json("oc_b"),
            Some("s"),
        )
        .expect("写入失败");

        let n = delete_channel(&conn, a).expect("删除失败");
        assert_eq!(n, 1, "应删掉 1 行，实际 {n}");
        let all = list_channels(&conn).expect("查询失败");
        assert_eq!(all.len(), 1, "只该剩 1 个渠道，实际 {}", all.len());
        assert_eq!(all[0].name, "B", "剩下的应当是 B");
        assert_eq!(
            delete_channel(&conn, 9999).expect("删除不存在的行"),
            0,
            "不存在的 id 应影响 0 行"
        );
    }

    #[test]
    fn recent_logs_joins_channel_name_newest_first() {
        let conn = mem();
        let id = upsert_channel(
            &conn,
            "飞书",
            "feishu",
            true,
            100,
            &feishu_json("oc_1"),
            Some("s"),
        )
        .expect("写入失败");

        log_event(
            &conn,
            1_000,
            Some(id),
            None,
            "test_sent",
            Some(200),
            Some("ok"),
        )
        .expect("写日志失败");
        log_event(&conn, 2_000, None, None, "sent", None, Some("本地通知")).expect("写日志失败");

        let logs = recent_logs(&conn, 10).expect("查询失败");
        assert_eq!(logs.len(), 2, "应当有 2 条日志，实际 {}", logs.len());
        assert_eq!(logs[0].ts_ms, 2_000, "新的应排在前");
        assert_eq!(logs[0].channel, None, "本地通知没有渠道名");
        assert_eq!(logs[1].channel.as_deref(), Some("飞书"), "应 join 出渠道名");
        assert_eq!(logs[1].http_code, Some(200), "应保留 HTTP 状态码");

        let only_one = recent_logs(&conn, 1).expect("查询失败");
        assert_eq!(only_one.len(), 1, "limit 应生效");
        assert_eq!(only_one[0].ts_ms, 2_000, "limit 应取最新的那条");
    }

    // ---------- M4：作业队列 / 降级链 / 配额 ----------

    /// 造一条待发实例，返回 (task_id, occurrence_id, channel_id)
    fn job_fixture(conn: &Connection) -> (i64, i64, i64) {
        let task = task_at(conn, "队列任务", -1_000, "[0]");
        ensure_occurrences(conn, now_ms()).expect("生成实例失败");
        let occ = occurrence_states(conn, task).expect("查询实例失败")[0].0;
        let ch = upsert_channel(
            conn,
            "飞书",
            "feishu",
            true,
            100,
            &feishu_json("oc_1"),
            Some("s"),
        )
        .expect("建渠道失败");
        (task, occ, ch)
    }

    fn job_id(conn: &Connection, occ: i64, ch: i64) -> i64 {
        conn.query_row(
            "SELECT id FROM push_jobs WHERE occurrence_id = ?1 AND channel_id = ?2",
            params![occ, ch],
            |r| r.get(0),
        )
        .expect("查作业 id 失败")
    }

    /// (state, attempts, next_retry_ms, last_error)
    fn job(conn: &Connection, id: i64) -> (String, i64, Option<i64>, Option<String>) {
        job_row(conn, id).expect("查作业失败")
    }

    #[test]
    fn enqueue_job_is_idempotent() {
        let conn = mem();
        let (_, occ, ch) = job_fixture(&conn);

        assert!(
            enqueue_job(&conn, occ, ch, false, 1_000).expect("入队失败"),
            "首次入队应当真的插入"
        );
        assert!(
            !enqueue_job(&conn, occ, ch, false, 2_000).expect("二次入队失败"),
            "同一实例同一渠道二次入队必须被去重（防重复轰炸）"
        );
        assert_eq!(
            pending_job_count(&conn).expect("统计失败"),
            1,
            "去重后应当只有 1 条作业"
        );
    }

    #[test]
    fn defer_does_not_consume_attempt_but_failure_does() {
        let conn = mem();
        let (_, occ, ch) = job_fixture(&conn);
        enqueue_job(&conn, occ, ch, false, 1_000).expect("入队失败");
        let id = job_id(&conn, occ, ch);

        assert!(claim_job(&conn, id).expect("认领失败"), "首次应当认领成功");
        assert!(
            !claim_job(&conn, id).expect("二次认领失败"),
            "已被认领的作业不能被第二个 tick 再认领（防重复发送）"
        );
        assert_eq!(job(&conn, id).1, 0, "认领本身不该消耗尝试次数");

        // 限流推迟：放回 pending 且不涨计数
        job_defer(&conn, id, 9_999).expect("推迟失败");
        let deferred = job(&conn, id);
        assert_eq!(deferred.0, "pending", "推迟后应回到 pending");
        assert_eq!(deferred.2, Some(9_999), "应记住下次可发时间");
        assert_eq!(deferred.1, 0, "推迟不等于失败，不该消耗尝试次数");

        // 真失败：涨计数
        claim_job(&conn, id).expect("认领失败");
        job_reschedule(&conn, id, 20_000, "超时").expect("重排失败");
        let retried = job(&conn, id);
        assert_eq!(retried.1, 1, "失败的尝试必须被记账");
        assert_eq!(retried.3.as_deref(), Some("超时"), "应当记下失败原因");
        assert_eq!(retried.0, "pending", "重排后应可再次被认领");

        job_failed(&conn, id, "凭证失效").expect("置失败失败");
        let failed = job(&conn, id);
        assert_eq!(failed.0, "failed", "放弃后状态应为 failed");
        assert_eq!(failed.3.as_deref(), Some("凭证失效"));
    }

    #[test]
    fn job_sent_records_attempt_and_clears_retry() {
        let conn = mem();
        let (_, occ, ch) = job_fixture(&conn);
        enqueue_job(&conn, occ, ch, false, 1_000).expect("入队失败");
        let id = job_id(&conn, occ, ch);
        claim_job(&conn, id).expect("认领失败");
        job_sent(&conn, id).expect("置已发失败");

        let done = job(&conn, id);
        assert_eq!(done.0, "sent", "应置为 sent");
        assert_eq!(done.1, 1, "成功一次也要记账（用来算重试次数）");
        assert_eq!(done.2, None, "已发就不该再有下次重试时间");
        assert_eq!(done.3, None, "已发就应清掉旧的错误信息");
    }

    #[test]
    fn due_jobs_respects_retry_time_and_apply_order() {
        let conn = mem();
        let (_, occ, ch) = job_fixture(&conn);
        enqueue_job(&conn, occ, ch, true, 1_000).expect("入队失败");

        let now = now_ms();
        let due = due_jobs(&conn, now).expect("查询到期作业失败");
        assert_eq!(due.len(), 1, "刚到期的作业应当被取出来");
        assert!(
            due[0].late,
            "补发标记应当被带出来（否则会丢掉「补发」字样）"
        );
        assert_eq!(due[0].title, "队列任务", "作业要带任务标题");
        assert_eq!(due[0].body, "正文内容", "作业要带任务正文");
        assert_eq!(due[0].channel.channel.id, ch, "作业要能直接拿到渠道与凭证");
        assert_eq!(due[0].channel.secret.as_deref(), Some("s"));

        let id = job_id(&conn, occ, ch);
        claim_job(&conn, id).expect("认领失败");
        job_reschedule(&conn, id, now + 60_000, "网络错误").expect("重排失败");
        assert!(
            due_jobs(&conn, now).expect("查询失败").is_empty(),
            "还没到重试时间的作业不该被取出来"
        );
        assert_eq!(
            due_jobs(&conn, now + 61_000).expect("查询失败").len(),
            1,
            "过了重试时间应当重新可取"
        );

        // 实例已经送达（终态）时，遗留的 pending 作业不应再发
        conn.execute(
            "UPDATE occurrences SET state = 'sent' WHERE id = ?1",
            params![occ],
        )
        .expect("改实例状态失败");
        assert!(
            due_jobs(&conn, now + 61_000).expect("查询失败").is_empty(),
            "实例已终态时，遗留作业不该被再发一次"
        );
    }

    #[test]
    fn cancel_jobs_keeps_the_one_that_won() {
        let conn = mem();
        let (_, occ, ch) = job_fixture(&conn);
        let ch2 = upsert_channel(
            &conn,
            "钉钉",
            "dingtalk",
            true,
            200,
            r#"{"webhook":"https://oapi.dingtalk.com/robot/send?access_token=t"}"#,
            None,
        )
        .expect("建渠道失败");
        enqueue_job(&conn, occ, ch, false, 1_000).expect("入队失败");
        enqueue_job(&conn, occ, ch2, false, 1_000).expect("入队失败");
        let winner = job_id(&conn, occ, ch);
        let loser = job_id(&conn, occ, ch2);

        let skipped = cancel_jobs_for_occurrence(&conn, occ, winner).expect("作废失败");
        assert_eq!(skipped, 1, "应当作废 1 条支线作业");
        assert_eq!(job(&conn, winner).0, "pending", "赢的那条不能被作废");
        assert_eq!(
            job(&conn, loser).0,
            "skipped",
            "降级支线应当被作废，避免同一提醒收到好几遍"
        );
        assert_eq!(
            due_jobs(&conn, now_ms()).expect("查询失败").len(),
            1,
            "作废后只应剩 1 条可发作业"
        );
    }

    #[test]
    fn recover_stuck_does_not_resend_on_restart() {
        let conn = mem();
        let (task, occ, ch) = job_fixture(&conn);
        enqueue_job(&conn, occ, ch, false, 1_000).expect("入队失败");
        let id = job_id(&conn, occ, ch);
        claim_job(&conn, id).expect("认领失败");
        // 模拟「进程在发送中被杀掉」：作业停在 sending、实例也停在 sending
        conn.execute(
            "UPDATE occurrences SET state = 'sending' WHERE id = ?1",
            params![occ],
        )
        .expect("改实例状态失败");

        let (jobs, occs) = recover_stuck(&conn, now_ms()).expect("清理失败");
        assert_eq!((jobs, occs), (1, 1), "应当清掉 1 条作业 + 1 条实例");
        assert_eq!(job(&conn, id).0, "failed", "悬挂作业应被置为 failed");
        assert_eq!(
            occurrence_states(&conn, task).expect("查询失败")[0].2,
            "missed",
            "悬挂实例应落 missed（重启不补发，否则同一提醒会重复）"
        );
        assert!(
            due_jobs(&conn, now_ms()).expect("查询失败").is_empty(),
            "清理后不该还有可发作业"
        );
    }

    #[test]
    fn quota_and_primary_are_per_channel() {
        let conn = mem();
        let a = upsert_channel(
            &conn,
            "钉钉",
            "dingtalk",
            true,
            100,
            r#"{"webhook":"https://oapi.dingtalk.com/robot/send?access_token=t"}"#,
            None,
        )
        .expect("建渠道失败");
        let b = upsert_channel(
            &conn,
            "飞书",
            "feishu",
            true,
            200,
            &feishu_json("oc_1"),
            Some("s"),
        )
        .expect("建渠道失败");

        let day = crate::time::today_local();
        bump_quota(&conn, a, &day, 3).expect("计数失败");
        bump_quota(&conn, a, &day, 2).expect("计数失败");
        assert_eq!(
            quota_for_day(&conn, a, &day).expect("查询失败"),
            5,
            "同一天同一渠道应当累加"
        );
        assert_eq!(
            quota_for_day(&conn, b, &day).expect("查询失败"),
            0,
            "额度必须按渠道隔离"
        );
        assert_eq!(
            quota_for_day(&conn, a, "2026-01-01").expect("查询失败"),
            0,
            "额度必须按天隔离"
        );

        let listed = list_channels(&conn).expect("查询失败");
        let ding = listed.iter().find(|c| c.id == a).expect("应能查到钉钉");
        assert_eq!(
            ding.quota_today,
            Some(5),
            "列表应当带出「今日已发」（否则额度就落不了地）"
        );

        make_primary(&conn, b).expect("设主渠道失败");
        assert_eq!(
            enabled_channels(&conn).expect("查询失败")[0].channel.id,
            b,
            "设为主渠道后应当排在降级链最前"
        );
        assert_eq!(
            list_channels(&conn).expect("查询失败")[0].id,
            b,
            "列表排序也应当跟着变"
        );
    }

    #[test]
    fn count_by_day_groups_days_and_splits_done() {
        let conn = mem();
        create(&conn, &input("一", "2026-02-01", None)).expect("插入失败");
        let second = create(&conn, &input("二", "2026-02-01", None)).expect("插入失败");
        create(&conn, &input("三", "2026-02-03", None)).expect("插入失败");
        create(&conn, &input("区间外", "2026-02-20", None)).expect("插入失败");
        set_done(&conn, second.id, true).expect("置为完成失败");

        let got = count_by_day(&conn, "2026-02-01", "2026-02-07").expect("统计失败");
        let flat: Vec<(String, i64, i64)> = got
            .iter()
            .map(|d| (d.day.clone(), d.total, d.undone))
            .collect();
        assert_eq!(
            flat,
            vec![
                ("2026-02-01".to_string(), 2, 1),
                ("2026-02-03".to_string(), 1, 1),
            ],
            "应当按天分组、只统计区间内、并区分已完成，实际 {flat:?}"
        );

        assert!(
            count_by_day(&conn, "2026-03-01", "2026-03-07")
                .expect("统计失败")
                .is_empty(),
            "没有任务的区间应当返回空表，不能补一串 0 —— 前端靠「这一天不在结果里」决定不画圆点"
        );
    }

    #[test]
    fn count_by_day_boundaries_are_inclusive() {
        let conn = mem();
        create(&conn, &input("首日", "2026-02-01", None)).expect("插入失败");
        create(&conn, &input("末日", "2026-02-07", None)).expect("插入失败");

        let days: Vec<String> = count_by_day(&conn, "2026-02-01", "2026-02-07")
            .expect("统计失败")
            .into_iter()
            .map(|d| d.day)
            .collect();
        assert_eq!(
            days,
            vec!["2026-02-01".to_string(), "2026-02-07".to_string()],
            "区间两端都应当算在内（周条一次查 7 天，少一天就会少一个圆点），实际 {days:?}"
        );
    }

    #[test]
    fn next_reminder_prefers_advance_point_and_skips_done() {
        let conn = mem();
        // 基准时间必须落在任务那一天附近：`NextReminder.day` 取的是**这次触发所在的那天**，
        // 用一个十万八千里外的合成时间戳（早先这里是 1_800_000_000_000）会断言出一个假的日期。
        let now = crate::time::day_start_ms("2026-02-01").expect("2026-02-01 应当能算出当天零点")
            + 6 * 3_600_000;
        let late =
            create(&conn, &input("晚的", "2026-02-02", Some(now + 3_600_000))).expect("插入失败");
        let soon =
            create(&conn, &input("早的", "2026-02-01", Some(now + 600_000))).expect("插入失败");

        // 「早的」配上「提前 5 分钟」：它的触发点比 due_at_ms 早 300 秒，
        // 只查 tasks.due_at_ms 的实现会把「晚的」…不，会把这条自己的时间认成 10 分钟后。
        conn.execute(
            "UPDATE tasks SET remind_offsets = '[0,-300]' WHERE id = ?1",
            params![soon.id],
        )
        .expect("改提醒点失败");
        ensure_occurrences(&conn, now).expect("生成实例失败");

        let got = next_reminder(&conn, now)
            .expect("查询失败")
            .expect("应当能查到一条将要响的提醒");
        assert_eq!(got.task_id, soon.id, "应当挑最早的那条，实际 {got:?}");
        assert_eq!(
            got.fire_at_ms,
            now + 600_000 - 300_000,
            "应当取「提前 5 分钟」那个点，而不是 due_at_ms；实际 {}",
            got.fire_at_ms
        );
        assert_eq!(
            got.day, "2026-02-01",
            "应当带出这次触发所在的那一天（不是任务起始日），实际 {}",
            got.day
        );

        set_done(&conn, soon.id, true).expect("置为完成失败");
        let after = next_reminder(&conn, now)
            .expect("查询失败")
            .expect("完成一条后应当还有另一条");
        assert_eq!(
            after.task_id, late.id,
            "已完成的任务不该再被预告，实际 {after:?}"
        );

        set_done(&conn, late.id, true).expect("置为完成失败");
        assert!(
            next_reminder(&conn, now).expect("查询失败").is_none(),
            "全部完成后应当返回 None（前端据此显示「没有安排了」）"
        );

        // 已经过去的触发点不能再被预告，否则顶部会一直显示「已到点」
        set_done(&conn, soon.id, false).expect("取消完成失败");
        assert!(
            next_reminder(&conn, now + 900_000)
                .expect("查询失败")
                .is_none(),
            "触发点已经过去的实例不该再被当成「下一条」，实际 {:?}",
            next_reminder(&conn, now + 900_000).expect("查询失败")
        );
    }

    // ---------- M5：重复规则 ----------

    /// 以「今天」为基准偏移若干天。
    ///
    /// 不写死真实日期（明天再跑就错），也不用假到离谱的日期：
    /// `stop_repeat` / `reset_occurrences_for_task` 内部用的是**真实时钟**，
    /// 用一个过去的假日期会让它们把刚生成的实例直接删干净。
    fn day_offset(days: i64) -> String {
        crate::time::shift_day(&crate::time::today_local(), days).expect("算日期失败")
    }

    /// 某天的早上 9 点（本地）。
    fn at_9(day: &str) -> i64 {
        crate::time::day_start_ms(day).expect("算零点失败") + 9 * 3_600_000
    }

    fn repeat_input(title: &str, day: &str, due: i64, rule: &str) -> TaskInput {
        TaskInput {
            repeat_rule: Some(rule.to_string()),
            ..input(title, day, Some(due))
        }
    }

    /// 提前提醒（`remind_offsets`）的三个场景：创建时用给的值、更新时不传要保留原值、传了就覆盖。
    /// 第二条最要紧：界面没碰这个下拉时传的是 `None`，绝不能把用户原来设的提前点冲掉。
    #[test]
    fn remind_offsets_create_and_update_semantics() {
        let conn = mem();
        let day = day_offset(1);
        let mut args = input("自带提醒点", &day, Some(at_9(&day)));
        args.remind_offsets = Some("[-300,0]".to_string());

        let task = create_with_occurrences(&conn, &args).expect("新建失败");
        assert_eq!(
            task.remind_offsets, "[-300,0]",
            "创建时给了提醒点，就不该用默认的 [0]"
        );
        assert_eq!(
            occ_times(&conn, task.id).len(),
            2,
            "提前 5 分钟 + 到点应当各生成 1 条实例"
        );

        let edit = input("自带提醒点", &day, Some(at_9(&day)));
        let after = update(&conn, task.id, &edit)
            .expect("更新失败")
            .expect("任务应当存在");
        assert_eq!(
            after.remind_offsets, "[-300,0]",
            "更新时没带提醒点，必须保留原值（否则界面不碰就会静默丢掉提前提醒）"
        );

        let mut edit = edit;
        edit.remind_offsets = Some("[0]".to_string());
        let after = update(&conn, task.id, &edit)
            .expect("更新失败")
            .expect("任务应当存在");
        assert_eq!(after.remind_offsets, "[0]", "显式给了提醒点就要覆盖");
    }

    /// 渠道选择的落库与覆盖语义：这里是**直接覆盖**（不像提醒点用 `COALESCE`），
    /// 因为 `None` 是「全部渠道」这个真实取值，不是「保持原值」。
    #[test]
    fn notify_channels_create_and_update_overwrite() {
        let conn = mem();
        let day = day_offset(1);
        let mut args = input("挑渠道", &day, None);
        args.notify_channels = Some(r#"["dingtalk"]"#.to_string());
        let task = create(&conn, &args).expect("新建失败");
        assert_eq!(
            task.notify_channels.as_deref(),
            Some(r#"["dingtalk"]"#),
            "创建时要把渠道选择存下来"
        );

        // 传 null 就是「改回全部渠道」—— 这一条单独守着：用 COALESCE 就再也改不回来
        let edit = input("挑渠道", &day, None);
        assert_eq!(edit.notify_channels, None, "默认就是不限制渠道");
        let after = update(&conn, task.id, &edit)
            .expect("更新失败")
            .expect("任务应当存在");
        assert_eq!(after.notify_channels, None, "传 null 应当改回「全部渠道」");

        let mut edit = input("挑渠道", &day, None);
        edit.notify_channels = Some(r#"["feishu","dingtalk"]"#.to_string());
        let after = update(&conn, task.id, &edit)
            .expect("更新失败")
            .expect("任务应当存在");
        assert_eq!(
            after.notify_channels.as_deref(),
            Some(r#"["feishu","dingtalk"]"#),
            "显式给了渠道就要覆盖"
        );
    }

    fn ch_ids(v: Vec<ChannelRecord>) -> Vec<i64> {
        v.into_iter().map(|c| c.channel.id).collect()
    }

    /// 渠道选择管的是「发到哪」：没勾 = 全部启用渠道；勾了 = 只那些；
    /// 勾的渠道停用了 = 空表（调用方据此走本地兜底，绝不能改发别的渠道）；
    /// 库里是脏值 = 按「不限制」处理，不能变成「一个都不发」。
    #[test]
    fn notify_channels_filter_delivery_targets() {
        let conn = mem();
        let ding = upsert_channel(
            &conn,
            "钉钉",
            "dingtalk",
            true,
            100,
            r#"{"webhook":"https://oapi.dingtalk.com/robot/send?access_token=t"}"#,
            None,
        )
        .expect("建钉钉失败");
        let fei = upsert_channel(
            &conn,
            "飞书",
            "feishu",
            true,
            200,
            &feishu_json("oc_1"),
            Some("s"),
        )
        .expect("建飞书失败");

        // 没勾渠道 → 全部启用渠道（顺序仍是优先级升序）
        let any = task_at(&conn, "不挑渠道", 3_600_000, "[0]");
        assert_eq!(
            ch_ids(channels_for_task(&conn, any).expect("查询失败")),
            vec![ding, fei],
            "没勾渠道就应当发全部启用渠道"
        );

        // 只勾钉钉
        let mut args = input("只发钉钉", &day_offset(1), None);
        args.notify_channels = Some(r#"["dingtalk"]"#.to_string());
        let only_ding = create(&conn, &args).expect("新建失败");
        assert_eq!(
            ch_ids(channels_for_task(&conn, only_ding.id).expect("查询失败")),
            vec![ding],
            "只勾了钉钉，就应当只剩钉钉（飞书不能再被选中）"
        );

        // 只勾飞书，然后把飞书停用 → 空表
        let mut args = input("只发飞书", &day_offset(1), None);
        args.notify_channels = Some(r#"["feishu"]"#.to_string());
        let only_fei = create(&conn, &args).expect("新建失败");
        conn.execute(
            "UPDATE channels SET enabled = 0 WHERE id = ?1",
            params![fei],
        )
        .expect("停用失败");
        assert!(
            channels_for_task(&conn, only_fei.id)
                .expect("查询失败")
                .is_empty(),
            "限定的渠道停用后应当是空表（走本地兜底），不能改成别的渠道"
        );
        assert_eq!(
            ch_ids(channels_for_task(&conn, any).expect("查询失败")),
            vec![ding],
            "没勾渠道的会跟着变少（= 全部启用渠道）"
        );

        // 脏值按「不限制」处理
        conn.execute(
            "UPDATE tasks SET notify_channels = '不是 JSON' WHERE id = ?1",
            params![any],
        )
        .expect("写入脏值失败");
        assert_eq!(
            ch_ids(channels_for_task(&conn, any).expect("查询失败")),
            vec![ding],
            "脏值应当按「不限制」处理，绝不能变成「一个都不发」"
        );
    }

    fn occ_times(conn: &Connection, task_id: i64) -> Vec<i64> {
        let mut stmt = conn
            .prepare("SELECT fire_at_ms FROM occurrences WHERE task_id = ?1 ORDER BY fire_at_ms")
            .expect("准备查询失败");
        let it = stmt
            .query_map(params![task_id], |r| r.get::<_, i64>(0))
            .expect("查询失败");
        it.collect::<Result<Vec<i64>>>().expect("收集失败")
    }

    #[test]
    fn repeat_daily_expands_one_per_day() {
        let conn = mem();
        let today = day_offset(0);
        let base = at_9(&today);
        let t = create_with_occurrences(
            &conn,
            &repeat_input("每天的早会", &today, base, r#"{"freq":"daily"}"#),
        )
        .expect("插入失败");

        // 用 base 当「现在」，实例会展开到 base + 7 天
        ensure_occurrences(&conn, base).expect("生成失败");
        let times = occ_times(&conn, t.id);
        assert!(
            times.len() >= 7,
            "每天重复至少该展开出一周的实例，实际 {} 条：{times:?}",
            times.len()
        );
        for pair in times.windows(2) {
            assert_eq!(
                pair[1] - pair[0],
                24 * 3_600_000,
                "每天重复应当按 24 小时铺开、不漏不重，实际 {times:?}"
            );
        }
    }

    #[test]
    fn repeat_weekdays_skips_weekend() {
        use chrono::{Datelike, Weekday};

        let conn = mem();
        let today = day_offset(0);
        let base = at_9(&today);
        let t = create_with_occurrences(
            &conn,
            &repeat_input("工作日提醒", &today, base, r#"{"freq":"weekdays"}"#),
        )
        .expect("插入失败");
        ensure_occurrences(&conn, base).expect("生成失败");

        let days: Vec<String> = occ_times(&conn, t.id)
            .iter()
            .map(|ms| crate::time::day_of_ms(*ms))
            .collect();
        assert!(
            !days.is_empty(),
            "工作日规则至少该展开出几天，实际一条都没有"
        );
        for d in &days {
            let wd = crate::time::parse_day(d).expect("解析日期失败").weekday();
            assert!(
                !matches!(wd, Weekday::Sat | Weekday::Sun),
                "{d} 是周末，工作日规则不该展开出实例（实际展开了：{days:?}）"
            );
        }
    }

    #[test]
    fn completing_whole_repeat_task_stops_the_series() {
        let conn = mem();
        let today = day_offset(0);
        let base = at_9(&today);
        let t = create_with_occurrences(
            &conn,
            &repeat_input("每天的早会", &today, base, r#"{"freq":"daily"}"#),
        )
        .expect("插入失败");

        set_done(&conn, t.id, true).expect("置为完成失败");
        assert_eq!(
            ensure_occurrences(&conn, base).expect("生成失败"),
            0,
            "任务整体完成后不该再补实例（重复系列到此为止）"
        );
    }

    #[test]
    fn occurrence_done_only_touches_that_day() {
        let conn = mem();
        let today = day_offset(0);
        // 用**明天**做目标：真实时钟已经过了今天的 09:00，拿今天测就只能断到
        // 「已经是 missed」这种非确定性状态；明天那个点一定还是 pending。
        let target = day_offset(1);
        let other = day_offset(2);
        let base = at_9(&today);
        let t = create_with_occurrences(
            &conn,
            &repeat_input("每天记账", &today, base, r#"{"freq":"daily"}"#),
        )
        .expect("插入失败");
        ensure_occurrences(&conn, base).expect("生成失败");

        let state_of = |day: &str| -> (Option<i64>, String) {
            conn.query_row(
                "SELECT done_ms, state FROM occurrences WHERE task_id = ?1 AND fire_at_ms = ?2",
                params![t.id, at_9(day)],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("读取失败")
        };

        set_occurrence_done(&conn, t.id, &target, true).expect("勾选失败");

        let (done_ms, state) = state_of(&target);
        assert!(
            done_ms.is_some(),
            "勾选后应当记下完成时间（界面据此显示为已完成）"
        );
        assert_eq!(
            state, "skipped",
            "勾完的那天不该再去发通知（应作废成 skipped），实际 {state}"
        );
        assert_eq!(
            state_of(&other).1,
            "pending",
            "别的那天照旧要提醒（重复系列继续）"
        );
        assert!(
            state_of(&today).0.is_none(),
            "勾明天不该在今天也留下完成记录"
        );

        assert_eq!(
            ensure_occurrences(&conn, base).expect("生成失败"),
            0,
            "已经勾过的那些天不该被重新生成回来（否则勾选会凭空消失）"
        );

        let list = tasks_on_day(&conn, &target).expect("查询失败");
        assert_eq!(
            list.len(),
            1,
            "明天应当只有这一条，实际 {} 条：{list:?}",
            list.len()
        );
        assert!(
            list[0].repeated,
            "明天那行是展开出来的（界面靠它把勾选变成「完成本次」）"
        );
        assert!(
            list[0].done,
            "勾过「完成本次」的那天，列表里应当显示为已完成"
        );

        // 取消勾选：那一格要还回来，而且因为还没到点，状态要回到 pending
        set_occurrence_done(&conn, t.id, &target, false).expect("取消勾选失败");
        let (done_again, state_again) = state_of(&target);
        assert!(done_again.is_none(), "取消勾选后不该还留着完成时间");
        assert_eq!(
            state_again, "pending",
            "还没到点的那天取消勾选后要回到 pending（照旧提醒），实际 {state_again}"
        );
        assert!(
            !tasks_on_day(&conn, &target).expect("查询失败")[0].done,
            "取消勾选后列表里应当不再显示为已完成"
        );
    }

    #[test]
    fn tasks_on_day_marks_expanded_rows_as_repeated() {
        let conn = mem();
        let today = day_offset(0);
        let later = day_offset(3);
        let base = at_9(&today);
        create_with_occurrences(
            &conn,
            &repeat_input("每天的早会", &today, base, r#"{"freq":"daily"}"#),
        )
        .expect("插入失败");

        let own = tasks_on_day(&conn, &today).expect("查询失败");
        assert_eq!(own.len(), 1, "起始日应当只有这一条，实际 {} 条", own.len());
        assert!(
            !own[0].repeated,
            "起始日那天的行不是「展开行」，不该标 repeated"
        );

        let list = tasks_on_day(&conn, &later).expect("查询失败");
        assert_eq!(
            list.len(),
            1,
            "第 3 天应当展开出这一条，实际 {} 条：{list:?}",
            list.len()
        );
        assert!(
            list[0].repeated,
            "非起始日展开出来的行必须标 repeated（界面靠它把勾选变成「完成本次」）"
        );
        assert_eq!(
            list[0].due_at_ms,
            Some(at_9(&later)),
            "展开行应当沿用起点那天的时刻"
        );
        assert_eq!(
            list[0].day, today,
            "展开行的 day 仍然是任务自己的起始日（界面保存编辑时要拿它回存）"
        );
        assert!(!list[0].done, "还没勾过，不该显示为已完成");
    }

    #[test]
    fn count_by_day_counts_repeat_days() {
        let conn = mem();
        let today = day_offset(0);
        let base = at_9(&today);
        let t = create_with_occurrences(
            &conn,
            &repeat_input("每天的早会", &today, base, r#"{"freq":"daily"}"#),
        )
        .expect("插入失败");

        let days = count_by_day(&conn, &day_offset(0), &day_offset(6)).expect("统计失败");
        assert_eq!(
            days.len(),
            7,
            "每天重复应当在周条上点亮连续 7 天，实际 {days:?}"
        );
        assert!(
            days.iter().all(|d| d.total == 1 && d.undone == 1),
            "每天应当算 1 条未完成，实际 {days:?}"
        );

        set_occurrence_done(&conn, t.id, &today, true).expect("勾选失败");
        let after = count_by_day(&conn, &day_offset(0), &day_offset(6)).expect("统计失败");
        assert_eq!(
            after[0].undone, 0,
            "今天勾完后未完成数应当是 0，实际 {after:?}"
        );
        assert_eq!(after[1].undone, 1, "明天还是未完成，实际 {after:?}");
    }

    #[test]
    fn stop_repeat_keeps_task_and_drops_future_instances() {
        let conn = mem();
        let today = day_offset(0);
        let base = at_9(&today);
        let t = create_with_occurrences(
            &conn,
            &repeat_input("每天的早会", &today, base, r#"{"freq":"daily"}"#),
        )
        .expect("插入失败");
        ensure_occurrences(&conn, base).expect("生成失败");
        assert!(
            occ_times(&conn, t.id).len() > 1,
            "先确认真的展开了多条，否则这个测试什么也没测"
        );

        assert_eq!(
            stop_repeat(&conn, t.id).expect("停止重复失败"),
            1,
            "应当改到 1 行（repeat_rule 被清掉）"
        );
        let rule: Option<String> = conn
            .query_row(
                "SELECT repeat_rule FROM tasks WHERE id = ?1",
                params![t.id],
                |r| r.get(0),
            )
            .expect("读取失败");
        assert!(rule.is_none(), "停止重复后规则应当被清空，实际 {rule:?}");
        assert!(
            get(&conn, t.id).expect("查询失败").is_some(),
            "停止重复不该把任务本身删掉"
        );

        let left = occ_times(&conn, t.id);
        assert_eq!(left.len(), 1, "停止重复后只该留起点那一个点，实际 {left:?}");
        assert_eq!(left[0], base, "留下的应当是任务自己那个时刻，实际 {left:?}");

        assert_eq!(
            stop_repeat(&conn, t.id).expect("第二次停止失败"),
            0,
            "本来就不是重复任务时应当返回 0（幂等，不重复改数据）"
        );
    }
}
