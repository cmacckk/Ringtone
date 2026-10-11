//! 到点调度的**纯逻辑**：不碰数据库、不碰 AppHandle，所以能直接单测。
//!
//! 错过语义（与 `docs/kitealarm-plan.md` §5 对齐）：
//!
//! | 上下文 | 已到期实例怎么处理 |
//! | --- | --- |
//! | `Steady`（进程一直在跑） | 直接发，标 `sent` |
//! | `Wakeup`（tick 间隔突然变大 = 休眠唤醒） | 迟到 ≤ 窗口 → 补发标 `late_sent`；超出 → `missed` |
//! | `ColdStart`（进程重启 / 关机后开机） | **一律 `missed`，不补发**（用户明确要求） |

/// tick 间隔。
///
/// 取值直接决定「到点后多久才发出去」的上限：闹钟是网格对齐的，
/// 最坏情况就是刚好错过一格，所以**上限 = TICK_MS**。
/// 取 2s 是因为验收标准写的是「到点 ±5 秒内送达」——如果间隔就是 5s，
/// 那么每一格都贴着标准边界跑（实测 5s 间隔 + 1.5s 相位漂移 = 6.5s，已经超标）。
/// 2s 则最坏 ≈2s，付的代价只是每轮几个走索引的查询（实测每轮 0~1ms）。
pub const TICK_MS: i64 = 2_000;

/// 补发窗口：休眠唤醒后，迟到 30 分钟内的还补发，超过就不补了
pub const LATE_WINDOW_MS: i64 = 30 * 60 * 1000;

/// tick 间隔超过这个值就判定为「休眠唤醒」，而不是正常的 tick 抖动
pub const WAKEUP_JUMP_MS: i64 = 90_000;

/// 实例生成的前瞻窗口：未来 7 天内到点的任务先生成实例
pub const HORIZON_MS: i64 = 7 * 24 * 3600 * 1000;

/// 实例生成的回看窗口：只回看 1 天。
/// 回看是为了让「刚过期、还没被处理的」也能进状态机，
/// 不回看更久是为了避免启动时往历史里灌一堆实例。
pub const LOOKBACK_MS: i64 = 24 * 3600 * 1000;

/// 通知正文的最大长度：系统通知有长度限制，超长会被截断甚至发送失败
pub const MAX_NOTIFY_BODY_CHARS: usize = 200;

/// 这一 tick 所处的上下文
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickContext {
    Steady,
    Wakeup,
    ColdStart,
}

/// 一条到期实例该怎么处理
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 正常发送 → `sent`
    Send,
    /// 补发（唤醒后迟到但还在窗口内）→ `late_sent`
    LateSend,
    /// 错过，只留痕不发送 → `missed`
    Missed,
}

impl Verdict {
    /// 是否属于「补发」
    pub fn is_late(self) -> bool {
        matches!(self, Self::LateSend)
    }

    /// 这条判决是否是「要发出去」
    pub fn should_send(self) -> bool {
        matches!(self, Self::Send | Self::LateSend)
    }
}

/// 判定一条实例该怎么处理。
///
/// 只应对 `fire_at_ms <= now_ms`（已到期）的实例调用；
/// 未到期的会被当成 `Send` 直接返回，调用方应自己先按到期时间过滤。
pub fn classify(fire_at_ms: i64, now_ms: i64, ctx: TickContext, late_window_ms: i64) -> Verdict {
    if fire_at_ms > now_ms {
        return Verdict::Send;
    }
    match ctx {
        TickContext::Steady => Verdict::Send,
        TickContext::Wakeup => {
            if now_ms - fire_at_ms <= late_window_ms {
                Verdict::LateSend
            } else {
                Verdict::Missed
            }
        }
        TickContext::ColdStart => Verdict::Missed,
    }
}

/// tick 间隔是否突然变大（休眠 / 挂起 / 被调试器冻住）。
/// `last_tick_ms <= 0` 表示「还没有上一 tick」，不算跳变。
pub fn is_wakeup_jump(last_tick_ms: i64, now_ms: i64, threshold_ms: i64) -> bool {
    last_tick_ms > 0 && now_ms - last_tick_ms > threshold_ms
}

/// 由上一次 tick 时间推出本 tick 的上下文。
///
/// 注意：**冷启动不走这里**。冷启动是启动时的一次性扫描
/// （`db::miss_overdue_on_start`），之后进循环一律是 Steady / Wakeup；
/// 否则启动后头几秒内到点的任务会被误判成 missed。
pub fn ctx_for_tick(last_tick_ms: i64, now_ms: i64) -> TickContext {
    if is_wakeup_jump(last_tick_ms, now_ms, WAKEUP_JUMP_MS) {
        TickContext::Wakeup
    } else {
        TickContext::Steady
    }
}

/// 通知正文：优先用任务备注，空则给默认文案，并且截断到安全长度。
pub fn notify_body(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "到点了".to_string();
    }
    truncate_chars(trimmed, MAX_NOTIFY_BODY_CHARS)
}

/// 按**字符**截断（不是字节），避免把中文切成半个字导致乱码。
/// 超长时补一个省略号，省略号也算在返回值里。
pub fn truncate_chars(raw: &str, max_chars: usize) -> String {
    if raw.chars().count() <= max_chars {
        return raw.to_string();
    }
    let mut out: String = raw.chars().take(max_chars).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_800_000_000_000;

    #[test]
    fn steady_sends_normally() {
        assert_eq!(
            classify(T0, T0, TickContext::Steady, LATE_WINDOW_MS),
            Verdict::Send,
            "稳态下正好到点应当正常发送"
        );
        assert_eq!(
            classify(T0, T0 + 4_999, TickContext::Steady, LATE_WINDOW_MS),
            Verdict::Send,
            "稳态下迟到不到一 tick 应当是正常发送，不是补发"
        );
    }

    #[test]
    fn wakeup_within_window_is_late_send() {
        assert_eq!(
            classify(T0, T0 + 60_000, TickContext::Wakeup, LATE_WINDOW_MS),
            Verdict::LateSend,
            "唤醒后迟到 1 分钟应当补发"
        );
        assert_eq!(
            classify(T0, T0 + LATE_WINDOW_MS, TickContext::Wakeup, LATE_WINDOW_MS),
            Verdict::LateSend,
            "正好卡在窗口边界（30 分钟）应当补发"
        );
    }

    #[test]
    fn wakeup_past_window_is_missed() {
        assert_eq!(
            classify(
                T0,
                T0 + LATE_WINDOW_MS + 1,
                TickContext::Wakeup,
                LATE_WINDOW_MS
            ),
            Verdict::Missed,
            "超过窗口 1 毫秒就该判 missed"
        );
        assert_eq!(
            classify(
                T0,
                T0 + 8 * 3600 * 1000,
                TickContext::Wakeup,
                LATE_WINDOW_MS
            ),
            Verdict::Missed,
            "唤醒后迟到 8 小时应当 missed"
        );
    }

    #[test]
    fn cold_start_never_backfills() {
        for late_by in [1_i64, 60_000, LATE_WINDOW_MS, 86_400_000] {
            assert_eq!(
                classify(T0, T0 + late_by, TickContext::ColdStart, LATE_WINDOW_MS),
                Verdict::Missed,
                "冷启动迟到 {late_by}ms 也必须 missed（不补发）"
            );
        }
    }

    #[test]
    fn future_items_are_never_missed() {
        assert_eq!(
            classify(T0, T0 - 1, TickContext::ColdStart, LATE_WINDOW_MS),
            Verdict::Send,
            "还没到期的实例不该被判 missed"
        );
        assert!(
            Verdict::Send.should_send() && !Verdict::Send.is_late(),
            "Send 应当要发且不算补发"
        );
        assert!(
            Verdict::LateSend.should_send() && Verdict::LateSend.is_late(),
            "LateSend 应当要发且算补发"
        );
        assert!(!Verdict::Missed.should_send(), "Missed 不应该发");
    }

    #[test]
    fn wakeup_jump_boundary() {
        assert!(
            !is_wakeup_jump(T0, T0 + WAKEUP_JUMP_MS, WAKEUP_JUMP_MS),
            "正好等于阈值不算跳变"
        );
        assert!(
            is_wakeup_jump(T0, T0 + WAKEUP_JUMP_MS + 1, WAKEUP_JUMP_MS),
            "超过阈值 1ms 就算跳变"
        );
        assert!(
            !is_wakeup_jump(0, T0, WAKEUP_JUMP_MS),
            "last_tick 未初始化时不算跳变"
        );
        assert!(
            !is_wakeup_jump(T0, T0 + 4_999, WAKEUP_JUMP_MS),
            "正常 5s tick 不算跳变"
        );
    }

    #[test]
    fn ctx_for_tick_boundary() {
        assert_eq!(
            ctx_for_tick(T0, T0 + TICK_MS),
            TickContext::Steady,
            "正常 tick 应当是稳态"
        );
        assert_eq!(
            ctx_for_tick(T0, T0 + WAKEUP_JUMP_MS + 1),
            TickContext::Wakeup,
            "间隔异常变大应当是唤醒"
        );
    }

    #[test]
    fn notify_body_rules() {
        assert_eq!(notify_body("   "), "到点了", "空备注应当给默认文案");
        assert_eq!(notify_body("  交周报  "), "交周报", "备注应当 trim");
        let long: String = "测".repeat(MAX_NOTIFY_BODY_CHARS + 50);
        let got = notify_body(&long);
        assert_eq!(
            got.chars().count(),
            MAX_NOTIFY_BODY_CHARS + 1,
            "超长正文应当截断成 {} 字加省略号，实际 {} 字",
            MAX_NOTIFY_BODY_CHARS,
            got.chars().count()
        );
        assert!(got.ends_with('…'), "截断后应当有省略号");
    }
}
