//! 投递编排（M4）：**入队 → 按优先级依次尝试 → 只对可重试的错误退避 → 全渠道失败才本地兜底**。
//!
//! 为什么要队列（`push_jobs`）而不是像 M3 那样「一次 tick 里同步发完」：
//! 1. 重试要等 5s / 30s / 2min —— 在调度线程里 `sleep` 会把其它提醒的 tick 一起拖住；
//! 2. 崩溃/退出后重试计划会丢 —— 落库才有「重启后知道有哪些没发完」这回事
//!    （虽然按用户要求不补发，但必须能收拾干净，见 `db::recover_stuck`）；
//! 3. 降级链本质上是「一条实例 × 多个渠道」的状态机，用表表达比用嵌套 `match` 清楚。
//!
//! 降级规则（与 `docs/kitealarm-plan.md` §7 对齐）：
//!
//! | 错误分类 | 处理 |
//! | --- | --- |
//! | `Retryable`（网络 / 超时 / 5xx，未超过重试次数） | 退避 5s / 30s / 2min 后重试**同一渠道** |
//! | `QuotaExceeded`（限流 / 额度耗尽） | **立刻**换下一个渠道（继续撞限流只会被封更久） |
//! | `Auth` / `BadRequest` / `Config` | **立刻**换下一个渠道（重试无意义） |
//! | 所有渠道都失败 | 本地系统通知兜底 + 托盘红点 + 如实记账 |

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use tauri::AppHandle;
use tauri::Manager;
use tauri_plugin_notification::NotificationExt;

use crate::channel::{self, OutboundMessage};
use crate::commands::AppState;
use crate::db::{self, ChannelRecord, DueItem, JobItem};
use crate::scheduler;

/// 一个作业最多尝试几次：首次 + 3 次重试
pub const MAX_ATTEMPTS: usize = 4;
/// 退避间隔：第 1 次失败后等 5s，第 2 次 30s，第 3 次 2min。
/// 长度 = 允许的重试次数，所以 `attempts`（已完成次数）正好可以当索引。
pub const BACKOFF_MS: [i64; 3] = [5_000, 30_000, 120_000];
/// 速率统计窗口
pub const RATE_WINDOW_MS: i64 = 60_000;

/// 「全渠道失败」的累计条数（托盘红点 / 界面角标用）。清零由用户点「知道了」触发。
static FAILURES: AtomicUsize = AtomicUsize::new(0);

pub fn failure_count() -> usize {
    FAILURES.load(Ordering::Relaxed)
}

pub fn clear_failures() {
    FAILURES.store(0, Ordering::Relaxed)
}

/// 每分钟发送配额的**内存滑动窗口**。
///
/// 只放在内存里（进程重启后归零）：这是「别把渠道撞限流」的自我保护，
/// 不是计费依据 —— 真正的日发送量落在 `channel_quota` 表里。
/// 重启后丢失只意味着「重启那一分钟可能多发几条」，可以接受。
#[derive(Debug, Default)]
pub struct RateLimiter {
    windows: HashMap<i64, Vec<i64>>,
}

impl RateLimiter {
    /// 尝试占用一个发送名额。
    ///
    /// - `Ok(())`：已占位，可以发
    /// - `Err(wait_ms)`：这一分钟已经满了，至少要等这么久
    pub fn try_acquire(
        &mut self,
        channel_id: i64,
        now_ms: i64,
        per_minute: i64,
    ) -> Result<(), i64> {
        let window = self.windows.entry(channel_id).or_default();
        window.retain(|t| now_ms - *t < RATE_WINDOW_MS);
        let limit = per_minute.max(1);
        if (window.len() as i64) < limit {
            window.push(now_ms);
            return Ok(());
        }
        let oldest = window.first().copied().unwrap_or(now_ms);
        Err((oldest + RATE_WINDOW_MS - now_ms).max(1))
    }

    /// 当前窗口内的条数（测试用）
    #[cfg(test)]
    pub fn used(&self, channel_id: i64) -> usize {
        self.windows.get(&channel_id).map_or(0, Vec::len)
    }
}

/// 投递结果，交给调用方写库（`mark_sent` / `mark_skipped` + `send_log`）
#[derive(Debug, Clone, PartialEq)]
pub struct Delivery {
    /// 是否送达（渠道成功或本地通知成功都算送达）
    pub ok: bool,
    /// `sent` / `late_sent` / `failed`
    pub result: &'static str,
    pub channel_id: Option<i64>,
    pub http_code: Option<i64>,
    pub detail: String,
}

/// 入队的结果
#[derive(Debug, Clone)]
pub enum Enqueue {
    /// 已排进队列，等 `pump` 去发
    Queued { channel: String },
    /// 根本没有可用渠道，当场走本地通知（这是「送达」而不是「失败」）
    Local(Delivery),
}

/// 一条到点提醒进入投递队列：只给**优先级最高**的渠道入队，
/// 后面要不要降级由 `pump` 按上表决定。
pub fn enqueue(app: &AppHandle, item: &DueItem) -> Result<Enqueue, String> {
    // 只捡「这条任务允许的渠道」：任务没勾渠道时就是全部启用渠道（上线前的老行为）。
    // 顺便把全部启用渠道也取出来，单纯为了区分「一个渠道都没配」和
    // 「任务勾的渠道现在都没启用」这两种走本地通知的原因。
    let (channels, configured) = {
        let state = app.state::<AppState>();
        let conn = state.conn.lock().map_err(|e| e.to_string())?;
        let allowed =
            db::channels_for_occurrence(&conn, item.occurrence_id).map_err(|e| e.to_string())?;
        let all = db::enabled_channels(&conn).map_err(|e| e.to_string())?;
        (allowed, all)
    };

    let Some(first) = channels.first() else {
        let why = if configured.is_empty() {
            "未配置推送渠道，走本地通知"
        } else {
            "这条任务限定的渠道当前没启用，走本地通知"
        };
        return Ok(Enqueue::Local(local_delivery(app, item, None, why)));
    };

    let state = app.state::<AppState>();
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    db::enqueue_job(
        &conn,
        item.occurrence_id,
        first.channel.id,
        item.late,
        crate::time::now_ms(),
    )
    .map_err(|e| e.to_string())?;
    Ok(Enqueue::Queued {
        channel: first.channel.name.clone(),
    })
}

/// 本轮 pump 的统计
#[derive(Debug, Default, Clone, PartialEq)]
pub struct PumpStats {
    /// 成功送达（含降级后成功）
    pub sent: usize,
    /// 安排退避重试
    pub retried: usize,
    /// 降级到下一个渠道
    pub degraded: usize,
    /// 限流推迟（不算失败，等窗口空出来再发）
    pub delayed: usize,
    /// 全部渠道都失败（已走本地兜底）
    pub exhausted: usize,
}

impl PumpStats {
    /// 这一轮什么都没干（用于「安静模式」打印）
    pub fn is_idle(&self) -> bool {
        *self == Self::default()
    }
}

/// 处理队列里所有到点的作业。每个 tick 调一次。
pub fn pump(app: &AppHandle, now_ms: i64, limiter: &mut RateLimiter) -> Result<PumpStats, String> {
    let jobs = {
        let state = app.state::<AppState>();
        let conn = state.conn.lock().map_err(|e| e.to_string())?;
        db::due_jobs(&conn, now_ms).map_err(|e| e.to_string())?
    };

    let mut stats = PumpStats::default();
    for job in jobs {
        match send_one(app, &job, now_ms, limiter)? {
            Outcome::Sent => stats.sent += 1,
            Outcome::Retry => stats.retried += 1,
            Outcome::Degrade => stats.degraded += 1,
            Outcome::Delayed => stats.delayed += 1,
            Outcome::Exhausted => stats.exhausted += 1,
        }
    }
    Ok(stats)
}

enum Outcome {
    Sent,
    Retry,
    Degrade,
    Delayed,
    Exhausted,
}

/// 发送一个作业，并把结果落库（成功 / 重试 / 降级 / 兜底）。
fn send_one(
    app: &AppHandle,
    job: &JobItem,
    now_ms: i64,
    limiter: &mut RateLimiter,
) -> Result<Outcome, String> {
    // 1) 认领。抢不到说明别人在处理（实际上只有一个调度线程，防的是重入）
    {
        let state = app.state::<AppState>();
        let conn = state.conn.lock().map_err(|e| e.to_string())?;
        if !db::claim_job(&conn, job.job_id).map_err(|e| e.to_string())? {
            return Ok(Outcome::Delayed);
        }
    }

    // 2) 速率闸门：撞限流的下场比晚发几十秒糟得多（钉钉会被封 10 分钟）
    let per_minute = channel::per_minute_limit(&job.channel.channel.kind);
    if let Err(wait_ms) = limiter.try_acquire(job.channel.channel.id, now_ms, per_minute) {
        let state = app.state::<AppState>();
        let conn = state.conn.lock().map_err(|e| e.to_string())?;
        db::job_defer(&conn, job.job_id, now_ms + wait_ms).map_err(|e| e.to_string())?;
        println!(
            "[ringtone] {} 已达 {per_minute} 条/分上限，{} ms 后再发：{}",
            job.channel.channel.name, wait_ms, job.title
        );
        return Ok(Outcome::Delayed);
    }

    // 3) 真发。锁已经释放。
    let msg = OutboundMessage {
        title: job.title.clone(),
        body: job.body.clone(),
        fire_at_ms: job.fire_at_ms,
        late: job.late,
    };
    let started = crate::time::now_ms();
    let result = channel::send_via(&job.channel, &msg);
    let ts = crate::time::now_ms();
    // 拆开两个数：tick 网格决定「什么时候开始发」，网络决定「多久才真的送达」。
    // 这两个数混在一起时，一次慢请求会被误读成「调度不准」。
    let net_ms = ts - started;
    let lag_ms = ts - job.fire_at_ms;

    match result {
        Ok(receipt) => {
            let name = job.channel.channel.name.clone();
            let detail = format!("{name}：{receipt}");
            let state = app.state::<AppState>();
            let conn = state.conn.lock().map_err(|e| e.to_string())?;
            db::job_sent(&conn, job.job_id).map_err(|e| e.to_string())?;
            db::mark_sent(&conn, job.occurrence_id, ts, job.late).map_err(|e| e.to_string())?;
            // 同一条实例在别的渠道上还有 pending 作业的一律作废，否则会收到两遍
            let cancelled = db::cancel_jobs_for_occurrence(&conn, job.occurrence_id, job.job_id)
                .map_err(|e| e.to_string())?;
            db::bump_quota(
                &conn,
                job.channel.channel.id,
                &crate::time::today_local(),
                1,
            )
            .map_err(|e| e.to_string())?;
            db::log_event(
                &conn,
                ts,
                Some(job.channel.channel.id),
                Some(job.occurrence_id),
                if job.late { "late_sent" } else { "sent" },
                Some(200),
                Some(&detail),
            )
            .map_err(|e| e.to_string())?;
            println!(
                "[ringtone] 已发送：{}（occ={} late={} attempts={} 网络 {net_ms}ms 到点后 {lag_ms}ms \
                 作废同实例其它作业 {} 条）→ {}",
                job.title,
                job.occurrence_id,
                job.late,
                job.attempts + 1,
                cancelled,
                detail
            );
            Ok(Outcome::Sent)
        }
        Err(err) => {
            let retry = if err.retryable() {
                "可重试"
            } else {
                "不可重试"
            };
            let attempt_no = job.attempts + 1;

            // 可重试且还有退避次数 → 原地等
            if err.retryable() && (job.attempts as usize) < BACKOFF_MS.len() {
                let wait = BACKOFF_MS[job.attempts as usize];
                let state = app.state::<AppState>();
                let conn = state.conn.lock().map_err(|e| e.to_string())?;
                db::job_reschedule(&conn, job.job_id, ts + wait, &err.to_string())
                    .map_err(|e| e.to_string())?;
                db::log_event(
                    &conn,
                    ts,
                    Some(job.channel.channel.id),
                    Some(job.occurrence_id),
                    "failed",
                    err.http_code,
                    Some(&format!(
                        "{retry}：{err}；第 {attempt_no} 次失败，{wait} ms 后重试"
                    )),
                )
                .map_err(|e| e.to_string())?;
                println!(
                    "[ringtone] 发送失败将重试：{}（{}，第 {attempt_no}/{MAX_ATTEMPTS} 次，{wait} ms 后）",
                    job.title, err
                );
                return Ok(Outcome::Retry);
            }

            // 否则降级：这条作业作废，改投下一个没试过的渠道
            let next = {
                let state = app.state::<AppState>();
                let conn = state.conn.lock().map_err(|e| e.to_string())?;
                db::job_failed(&conn, job.job_id, &err.to_string()).map_err(|e| e.to_string())?;
                let tried =
                    db::tried_channels(&conn, job.occurrence_id).map_err(|e| e.to_string())?;
                // 降级只在「这条任务允许的渠道」里挑：用户选了「只发钉钉」，
                // 就不能因为钉钉挂了而悄悄发到飞书去（那种情况按约定走本地兜底 + 记一条失败）。
                let enabled = db::channels_for_occurrence(&conn, job.occurrence_id)
                    .map_err(|e| e.to_string())?;
                enabled.into_iter().find(|c| !tried.contains(&c.channel.id))
            };

            let next = match next {
                Some(rec) => rec,
                None => return exhaust(app, job, err, retry, ts),
            };

            let name = next.channel.name.clone();
            let state = app.state::<AppState>();
            let conn = state.conn.lock().map_err(|e| e.to_string())?;
            db::enqueue_job(&conn, job.occurrence_id, next.channel.id, job.late, ts)
                .map_err(|e| e.to_string())?;
            db::log_event(
                &conn,
                ts,
                Some(job.channel.channel.id),
                Some(job.occurrence_id),
                "failed",
                err.http_code,
                Some(&format!("{retry}：{err}；已降级到「{name}」")),
            )
            .map_err(|e| e.to_string())?;
            println!(
                "[ringtone] 发送失败已降级：{}（{}，第 {attempt_no} 次）→ 改投「{name}」",
                job.title, err
            );
            Ok(Outcome::Degrade)
        }
    }
}

/// 所有渠道都失败：本地通知兜底 + 红点计数 + 如实记账
fn exhaust(
    app: &AppHandle,
    job: &JobItem,
    err: channel::SendError,
    retry: &str,
    ts: i64,
) -> Result<Outcome, String> {
    let detail = format!("全渠道失败，最后一条：{retry}：{err}");
    // 先如实记一条「**这个渠道**失败了」。降级路径（还有下一个渠道可试）已经逐条写过，
    // 但这条路径（没有下一个渠道可降 —— 就是用户「只勾一个渠道」的情形）以前直接跳过，
    // 于是「发送记录」里只有一条不挂渠道的本地兜底行，看不出是哪个渠道、因为什么失败。
    // 锁要用小块包住：下面的本地兜底还会再拿一次同一把锁（Mutex 不可重入，重叠就死锁）。
    {
        let state = app.state::<AppState>();
        let conn = state.conn.lock().map_err(|e| e.to_string())?;
        db::log_event(
            &conn,
            ts,
            Some(job.channel.channel.id),
            Some(job.occurrence_id),
            "failed",
            err.http_code,
            Some(&format!("{retry}：{err}；没有可降级的渠道")),
        )
        .map_err(|e| e.to_string())?;
    }
    let delivery = local_delivery(app, &due_of(job), Some(job.channel.channel.id), &detail);
    FAILURES.fetch_add(1, Ordering::Relaxed);
    if delivery.ok {
        eprintln!(
            "[ringtone] 全渠道失败，已用本地通知兜底：{}（{}）",
            job.title, err
        );
    } else {
        eprintln!(
            "[ringtone] 全渠道失败且本地通知也失败：{}（{}）",
            job.title, err
        );
    }
    // 注意：这里 channel_id 传 None —— 这一行记的是「本地兜底」这个动作本身；
    // 那个失败渠道自己的记录在函数开头已经写过一条了。
    let state = app.state::<AppState>();
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    if delivery.ok {
        db::mark_sent(&conn, job.occurrence_id, ts, job.late).map_err(|e| e.to_string())?;
    } else {
        db::mark_skipped(&conn, job.occurrence_id).map_err(|e| e.to_string())?;
    }
    db::log_event(
        &conn,
        ts,
        None,
        Some(job.occurrence_id),
        delivery.result,
        None,
        Some(&delivery.detail),
    )
    .map_err(|e| e.to_string())?;
    Ok(Outcome::Exhausted)
}

/// 把作业还原成一条 `DueItem`（本地兜底只需要这几个字段）
fn due_of(job: &JobItem) -> DueItem {
    DueItem {
        occurrence_id: job.occurrence_id,
        task_id: job.task_id,
        title: job.title.clone(),
        body: job.body.clone(),
        day: String::new(),
        fire_at_ms: job.fire_at_ms,
        late: job.late,
    }
}

/// 走本地系统通知，并按结果组装 `Delivery`
fn local_delivery(
    app: &AppHandle,
    item: &DueItem,
    channel_id: Option<i64>,
    prefix: &str,
) -> Delivery {
    match notify_local(app, &item.title, &scheduler::notify_body(&item.body)) {
        Ok(()) => Delivery {
            ok: true,
            result: if item.late { "late_sent" } else { "sent" },
            channel_id,
            http_code: None,
            detail: format!("{prefix}；已用本地通知兜底"),
        },
        Err(e) => Delivery {
            ok: false,
            result: "failed",
            channel_id,
            http_code: None,
            detail: format!("{prefix}；本地通知也失败：{e}"),
        },
    }
}

/// 发本地系统通知（没配渠道时的主路径，配了渠道时的兜底路径）
pub fn notify_local(app: &AppHandle, title: &str, body: &str) -> Result<(), String> {
    app.notification()
        .builder()
        .title(title)
        .body(body)
        .show()
        .map_err(|e| e.to_string())
}

/// 「发测试消息」按钮用：只发这一个渠道，不做本地兜底、不入队、不做降级
/// （测试就是要看到这个渠道自己的成功或失败原因）
pub fn send_test(rec: &ChannelRecord, text: &str) -> channel::SendOutcome {
    let msg = OutboundMessage {
        title: "Ringtone 测试消息".to_string(),
        body: text.to_string(),
        fire_at_ms: crate::time::now_ms(),
        late: false,
    };
    channel::send_via(rec, &msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limiter_allows_up_to_limit_then_waits() {
        let mut rl = RateLimiter::default();
        let now = 1_791_445_000_000;
        for i in 0..20 {
            assert!(
                rl.try_acquire(7, now + i, 20).is_ok(),
                "第 {} 条不该被拦（限额 20）",
                i + 1
            );
        }
        let wait = rl
            .try_acquire(7, now + 20, 20)
            .expect_err("第 21 条必须被拦住");
        assert_eq!(
            wait,
            RATE_WINDOW_MS - 20,
            "等待时长应等于「最早那条出窗口」所需时间"
        );
    }

    #[test]
    fn rate_limiter_frees_slot_after_window() {
        let mut rl = RateLimiter::default();
        let now = 1_791_445_000_000;
        assert!(rl.try_acquire(1, now, 1).is_ok(), "第一条应放行");
        assert!(rl.try_acquire(1, now + 1, 1).is_err(), "同分钟第二条应被拦");
        assert!(
            rl.try_acquire(1, now + RATE_WINDOW_MS, 1).is_ok(),
            "过了窗口就该放行"
        );
    }

    #[test]
    fn rate_limiter_isolates_channels() {
        let mut rl = RateLimiter::default();
        let now = 1_791_445_000_000;
        assert!(rl.try_acquire(1, now, 1).is_ok(), "渠道 1 第一条应放行");
        assert!(
            rl.try_acquire(2, now, 1).is_ok(),
            "渠道 2 有自己的窗口，不该被渠道 1 拖累"
        );
        assert_eq!(rl.used(1), 1, "渠道 1 窗口应为 1");
        assert_eq!(rl.used(2), 1, "渠道 2 窗口应为 1");
    }

    #[test]
    fn backoff_table_is_the_documented_5s_30s_2min() {
        assert_eq!(
            BACKOFF_MS,
            [5_000, 30_000, 120_000],
            "退避必须是 5s/30s/2min（文档承诺过）"
        );
        assert_eq!(
            MAX_ATTEMPTS,
            BACKOFF_MS.len() + 1,
            "总尝试次数应等于「首次 + 退避表长度」，否则会多试或少试"
        );
    }

    #[test]
    fn pump_stats_idle_detection() {
        assert!(
            PumpStats::default().is_idle(),
            "默认统计应当是「什么都没干」"
        );
        let busy = PumpStats {
            sent: 1,
            ..Default::default()
        };
        assert!(!busy.is_idle(), "有发送就不能算 idle");
    }

    #[test]
    fn failures_counter_round_trip() {
        clear_failures();
        assert_eq!(failure_count(), 0, "清零后应为 0");
        FAILURES.fetch_add(3, Ordering::Relaxed);
        assert_eq!(failure_count(), 3, "累计应为 3");
        clear_failures();
        assert_eq!(failure_count(), 0, "再次清零后应为 0");
    }
}
