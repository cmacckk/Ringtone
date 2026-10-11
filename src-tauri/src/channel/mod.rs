//! 推送渠道层。
//!
//! 已接：钉钉（自定义机器人 webhook + 加签）、飞书（自建应用 `tenant_access_token` + `im/v1/messages`）。
//!
//! 三条设计约束：
//! 1. 发送是**阻塞**的 —— 调度线程本身就是 OS 线程，同步调用最直白；
//!    HTTP 客户端必须带超时，否则一条卡死的请求会把整个 tick 拖住。
//! 2. 错误必须**分类** —— 降级链只对「可重试」的错误退避重试；
//!    凭证失效、参数错、限流要立刻换渠道，傻等只会把提醒拖过期。
//! 3. 每个渠道要报出**自己的速率上限**（`per_minute_limit`）—— 钉钉 20 条/分是硬限，
//!    超了会被封 10 分钟；宁可晚发几十秒，也不能撞这条线。

pub mod dingtalk;
pub mod feishu;

use chrono::{Local, TimeZone};

use crate::db::ChannelRecord;

/// 一条要发出去的消息（与具体渠道无关）
#[derive(Debug, Clone, PartialEq)]
pub struct OutboundMessage {
    pub title: String,
    pub body: String,
    /// 原始到点时刻（epoch 毫秒）
    pub fire_at_ms: i64,
    /// true = 补发（休眠唤醒后迟到，但在窗口内）
    pub late: bool,
}

/// 错误分类。**这是降级链的唯一依据**：新增渠道时必须给出分类，不要一律 Retryable。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// 网络抖动 / 5xx / 超时：值得退避重试
    Retryable,
    /// 被限流（如飞书 `11232`）：值得退避重试
    QuotaExceeded,
    /// 凭证无效 / 无权限：重试无用，换渠道
    Auth,
    /// 参数或接收方不对（如机器人不在群里）：换渠道
    BadRequest,
    /// 本地配置缺失或不完整（还没填 app_secret 之类）
    Config,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SendError {
    pub kind: ErrorKind,
    pub http_code: Option<i64>,
    pub message: String,
}

impl SendError {
    pub fn new(kind: ErrorKind, http_code: Option<i64>, message: impl Into<String>) -> Self {
        Self {
            kind,
            http_code,
            message: message.into(),
        }
    }

    pub fn config(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Config, None, message)
    }

    /// 值得退避重试吗？
    pub fn retryable(&self) -> bool {
        matches!(self.kind, ErrorKind::Retryable | ErrorKind::QuotaExceeded)
    }
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.http_code {
            Some(code) => write!(f, "[{:?}/HTTP {}] {}", self.kind, code, self.message),
            None => write!(f, "[{:?}] {}", self.kind, self.message),
        }
    }
}

/// 发送成功时返回渠道侧的消息 id（写进 `send_log.detail`，将来查「到底发出去没有」靠它）
pub type SendOutcome = Result<String, SendError>;

/// 把一条消息按渠道类型发出去。
///
/// 成功时返回的是**给人和日志看的送达凭据**（如 `message_id=om_xxx`），
/// 因为「到底发出去没有」全靠它回答。
pub fn send_via(rec: &ChannelRecord, msg: &OutboundMessage) -> SendOutcome {
    match rec.channel.kind.as_str() {
        feishu::KIND => {
            let cfg = feishu::FeishuConfig::from_json(&rec.channel.config_json)?;
            let secret = rec
                .secret
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| SendError::config("飞书渠道还没填 app_secret"))?;
            let mut client = feishu::FeishuClient::new(cfg, secret.to_string())?;
            client.send_text(&render_text(msg))
        }
        dingtalk::KIND => {
            let cfg = dingtalk::DingtalkConfig::from_json(&rec.channel.config_json)?;
            let client = dingtalk::DingtalkClient::new(cfg, rec.secret.clone())?;
            client.send_text(&render_text(msg))
        }
        other => Err(SendError::config(format!("暂不支持的渠道类型：{other}"))),
    }
}

/// 各渠道的每分钟发送上限（官方文档值）。
/// 钉钉：20 条/分（超了限流 10 分钟）；飞书 `im/v1/messages`：官方 1000/分、50/秒，这里取保守的 100。
pub fn per_minute_limit(kind: &str) -> i64 {
    match kind {
        dingtalk::KIND => dingtalk::PER_MINUTE_LIMIT,
        feishu::KIND => 100,
        _ => 60,
    }
}

// 新增渠道时要改的地方：本模块的 `send_via` 分发、`per_minute_limit`、
// 以及前端 `src/Settings.tsx` 里对应的表单。不提供「已知 kind 列表」常量是故意的：
// 一个没接完的 kind 写进常量里，只会让界面出现「填了也发不出去」的选项。
/// 推送正文。标题在前、正文可选、时间收尾；补发会显式标注。
pub fn render_text(msg: &OutboundMessage) -> String {
    let head = if msg.late {
        "【补发·待办提醒】"
    } else {
        "【待办提醒】"
    };
    let mut out = format!("{head}{}", msg.title);

    let body =
        crate::scheduler::truncate_chars(msg.body.trim(), crate::scheduler::MAX_NOTIFY_BODY_CHARS);
    if !body.is_empty() {
        out.push('\n');
        out.push_str(&body);
    }

    let clock = Local
        .timestamp_millis_opt(msg.fire_at_ms)
        .single()
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_else(|| "未知时间".to_string());
    out.push_str(&format!("\n时间：{clock}"));
    if msg.late {
        out.push_str("（补发）");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(late: bool) -> OutboundMessage {
        OutboundMessage {
            title: "开会".to_string(),
            body: "  带上笔记本  ".to_string(),
            // 2026-10-08 14:30 本地时间（时区无关的断言只比结构，不比格式里的时区）
            fire_at_ms: 1_791_431_400_000,
            late,
        }
    }

    #[test]
    fn render_keeps_title_body_and_clock() {
        let text = render_text(&msg(false));
        assert!(
            text.starts_with("【待办提醒】开会\n带上笔记本\n时间："),
            "正文结构应是「标题/备注/时间」，实际：{text}"
        );
        assert!(
            !text.contains("【补发"),
            "非补发不该带补发标记，实际：{text}"
        );
    }

    #[test]
    fn render_marks_late_and_skips_empty_body() {
        let mut m = msg(true);
        m.body = "   ".to_string();
        let text = render_text(&m);
        assert!(
            text.starts_with("【补发·待办提醒】开会\n时间："),
            "空备注不该留出空行，实际：{text}"
        );
        assert!(
            text.contains("（补发）"),
            "补发必须在正文结尾标明，实际：{text}"
        );
    }

    #[test]
    fn render_truncates_long_body_by_chars() {
        let mut m = msg(false);
        m.body = "啊".repeat(crate::scheduler::MAX_NOTIFY_BODY_CHARS + 50);
        let text = render_text(&m);
        assert!(
            text.contains('…'),
            "超长备注应带省略号，实际长度 {}",
            text.chars().count()
        );
        let body_line = text.lines().nth(1).unwrap_or("");
        assert_eq!(
            body_line.chars().count(),
            crate::scheduler::MAX_NOTIFY_BODY_CHARS + 1,
            "备注应截断到 {}+1（省略号）个字符，实际 {}",
            crate::scheduler::MAX_NOTIFY_BODY_CHARS,
            body_line.chars().count()
        );
    }
}
