//! 钉钉群机器人渠道（自定义机器人 webhook + 加签）。
//!
//! 官方限制（与 `docs/kitealarm-plan.md` §6 对齐）：
//! **每个机器人每分钟最多 20 条，超过会被限流 10 分钟** —— 这是所有渠道里最硬的一条，
//! 所以 `PER_MINUTE_LIMIT` 既是本模块的常量，也是 `deliver::RateLimiter` 的默认值。
//!
//! 加签算法（官方规定，不要「优化」）：
//! ```text
//! timestamp = 当前毫秒
//! string_to_sign = "{timestamp}\n{secret}"
//! sign = urlencode(base64(HMAC-SHA256(key = secret, message = string_to_sign)))
//! ```
//! 注意 HMAC 的**密钥是 secret，消息是 string_to_sign**（不是反过来）。

use std::time::Duration;

use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

use super::{ErrorKind, SendError, SendOutcome};

pub const KIND: &str = "dingtalk";
/// 官方 webhook 前缀；`config_json` 里存完整 webhook 或只存 access_token 都能用
pub const WEBHOOK_PREFIX: &str = "https://oapi.dingtalk.com/robot/send";
/// 只给裸 `access_token` 时的默认地址；用户配了完整 URL（含自建网关）就按用户的来。
pub const TIMEOUT_SECS: u64 = 10;
/// 官方限制：每个机器人每分钟最多 20 条
pub const PER_MINUTE_LIMIT: i64 = 20;

/// 渠道配置（存在 `channels.config_json`）。
/// 加签密钥**不在这里** —— 它属于密钥，存 `credential_enc`。
#[derive(Debug, Clone, PartialEq)]
pub struct DingtalkConfig {
    /// 完整 webhook URL 或裸 access_token（两种都接受，见 `access_token()`）
    pub webhook: String,
}

impl DingtalkConfig {
    pub fn from_json(raw: &str) -> Result<Self, SendError> {
        let v: Value = serde_json::from_str(raw)
            .map_err(|e| SendError::config(format!("钉钉渠道配置不是合法 JSON：{e}")))?;
        let webhook = v["webhook"]
            .as_str()
            .or_else(|| v["access_token"].as_str())
            .unwrap_or_default()
            .trim()
            .to_string();
        let cfg = Self { webhook }.normalized();
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn to_json(&self) -> Result<String, SendError> {
        serde_json::to_string(&json!({ "webhook": self.webhook }))
            .map_err(|e| SendError::config(format!("钉钉配置序列化失败：{e}")))
    }

    pub fn normalized(mut self) -> Self {
        self.webhook = self.webhook.trim().to_string();
        self
    }

    fn validate(&self) -> Result<(), SendError> {
        if self.webhook.is_empty() {
            return Err(SendError::config("钉钉渠道还没填 webhook / access_token"));
        }
        if self.access_token().is_empty() {
            return Err(SendError::config(
                "钉钉 webhook 里找不到 access_token（粘贴完整 webhook，或只填 access_token）",
            ));
        }
        Ok(())
    }

    /// 从 webhook 里取出 access_token。裸 token 直接返回。
    ///
    /// 允许用户直接粘贴官方给的那一整条 URL —— 这是最不容易填错的方式。
    pub fn access_token(&self) -> String {
        let raw = self.webhook.trim();
        if let Some(idx) = raw.find("access_token=") {
            let rest = &raw[idx + "access_token=".len()..];
            let end = rest.find('&').unwrap_or(rest.len());
            return rest[..end].trim().to_string();
        }
        raw.to_string()
    }
}

/// 计算加签。抽成纯函数是为了能对着官方样例断言。
pub fn sign(secret: &str, timestamp_ms: i64) -> Result<String, SendError> {
    let string_to_sign = format!("{timestamp_ms}\n{secret}");
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .map_err(|e| SendError::config(format!("加签密钥不可用：{e}")))?;
    mac.update(string_to_sign.as_bytes());
    Ok(base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes()))
}

/// 严格的 urlencode：只放行 `A-Za-z0-9-_.~`，其余按 `%XX` 大写十六进制。
///
/// 自己实现而不用 reqwest 的 `query()`：base64 里的 `+` `/` `=` 必须被编码，
/// 而这一条是「加签不对但看起来一切正常」的经典现场，自己写才测得住。
pub fn urlencode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for b in raw.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 组装请求 URL。没配加签密钥时不带 `timestamp`/`sign`（此时机器人应配置了关键词或 IP 白名单）。
pub fn build_url(
    cfg: &DingtalkConfig,
    secret: Option<&str>,
    timestamp_ms: i64,
) -> Result<String, SendError> {
    let cfg = cfg.clone().normalized();
    cfg.validate()?;
    let raw = cfg.webhook.trim();
    // 配了完整 URL 就**原样用**：企业内网可能自己架了转发网关，不该被静默改写到官方域名。
    // 只给裸 access_token 时才拼官方地址。
    let mut url = if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_string()
    } else {
        format!(
            "{WEBHOOK_PREFIX}?access_token={}",
            urlencode(&cfg.access_token())
        )
    };
    if let Some(secret) = secret.map(str::trim).filter(|s| !s.is_empty()) {
        let s = sign(secret, timestamp_ms)?;
        let sep = if url.contains('?') { '&' } else { '?' };
        url.push_str(&format!(
            "{sep}timestamp={timestamp_ms}&sign={}",
            urlencode(&s)
        ));
    }
    Ok(url)
}

/// 钉钉的 `errcode` → 错误分类。**这条映射决定「退避重试」还是「立刻换渠道」**。
///
/// 钉钉业务错误几乎总是 HTTP 200 + `errcode != 0`，所以只看 HTTP 状态是不够的。
pub fn classify_error(http_status: u16, errcode: i64, errmsg: &str) -> ErrorKind {
    if http_status == 429 {
        return ErrorKind::QuotaExceeded;
    }
    if http_status >= 500 || http_status == 408 {
        return ErrorKind::Retryable;
    }
    if errcode == 0 {
        // 钉钉说「成功」却走到错误分类里：只可能是 HTTP 层异常（网关/代理插了一脚），
        // 这种情况重试是唯一合理的选择。正常成功根本不会调到本函数。
        return ErrorKind::Retryable;
    }
    let lower = errmsg.to_ascii_lowercase();
    // 限流：官方是 410100（send too fast），但换文案/换网关都可能变，所以文案也认
    if errcode == 410100
        || lower.contains("too fast")
        || lower.contains("exceed")
        || errmsg.contains("限流")
        || errmsg.contains("频繁")
    {
        return ErrorKind::QuotaExceeded;
    }
    match errcode {
        // token 不存在 / 无效 / 已停用
        300001..=300007 => ErrorKind::Auth,
        // 加签或关键词不匹配 —— 改一遍配置就能好，重试无用
        310000 => ErrorKind::BadRequest,
        // 群已被解散 / 机器人被移出
        310001..=310005 => ErrorKind::BadRequest,
        _ => ErrorKind::BadRequest,
    }
}

/// 把钉钉响应体解析成「错误」或 `Ok(送达凭据)`。
pub fn parse_send_response(http_status: u16, body: &str) -> SendOutcome {
    let v: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            return Err(SendError::new(
                classify_error(http_status, -1, ""),
                Some(http_status as i64),
                format!(
                    "响应不是合法 JSON（{e}）：{}",
                    crate::scheduler::truncate_chars(body, 200)
                ),
            ))
        }
    };
    let errcode = v["errcode"].as_i64().unwrap_or(-1);
    let errmsg = v["errmsg"].as_str().unwrap_or("（响应没有 errmsg 字段）");
    if errcode == 0 && (200..300).contains(&http_status) {
        return Ok("errcode=0（钉钉不回 message_id）".to_string());
    }

    let mut hint = String::new();
    if errcode == 310000 {
        hint = if errmsg.to_ascii_lowercase().contains("sign") {
            "；加签不对：检查密钥是否与「安全设置 - 加签」里的一致，以及系统时间是否准".to_string()
        } else {
            "；机器人安全设置要「关键词」时，消息里必须出现该关键词".to_string()
        };
    }
    Err(SendError::new(
        classify_error(http_status, errcode, errmsg),
        Some(http_status as i64),
        format!("钉钉返回 errcode={errcode} errmsg={errmsg}{hint}"),
    ))
}

/// 钉钉的请求体：`msgtype=text`，正文在 `text.content`
pub fn send_body(text: &str) -> Value {
    json!({ "msgtype": "text", "text": { "content": text } })
}

/// 只发一条消息的客户端（无状态：钉钉的鉴权全在 URL 里，没有 token 要缓存）
pub struct DingtalkClient {
    cfg: DingtalkConfig,
    secret: Option<String>,
    http: reqwest::blocking::Client,
}

impl DingtalkClient {
    pub fn new(cfg: DingtalkConfig, secret: Option<String>) -> Result<Self, SendError> {
        let cfg = cfg.normalized();
        cfg.validate()?;
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(TIMEOUT_SECS))
            .build()
            .map_err(|e| SendError::config(format!("建 HTTP 客户端失败：{e}")))?;
        Ok(Self {
            cfg,
            secret: secret.filter(|s| !s.trim().is_empty()),
            http,
        })
    }

    /// 这个渠道**没有**「只验证不发送」的接口（唯一的入口就是发消息本身），
    /// 所以 verify 只做静态校验，真要用请点「发测试消息」。
    pub fn verify(&self) -> Result<String, SendError> {
        if self.secret.is_some() {
            Ok(
                "配置完整（已启用加签）。钉钉没有「只验证不发消息」的接口，请用「发测试消息」确认"
                    .into(),
            )
        } else {
            Ok("配置完整（未填加签密钥，机器人需已配置关键词或 IP 白名单）。请用「发测试消息」确认".into())
        }
    }

    pub fn send_text(&self, text: &str) -> SendOutcome {
        let ts = crate::time::now_ms();
        let url = build_url(&self.cfg, self.secret.as_deref(), ts)?;
        let resp = self
            .http
            .post(&url)
            .json(&send_body(text))
            .send()
            .map_err(|e| {
                SendError::new(ErrorKind::Retryable, None, format!("请求钉钉失败：{e}"))
            })?;
        let status = resp.status().as_u16();
        let body = resp.text().unwrap_or_default();
        parse_send_response(status, &body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 假密钥（形状与真实的一致：`SEC` + 64 位十六进制，共 67 字符）。
    /// 仓库是公开的，真的加签密钥与 webhook 一律不放进来。
    const SECRET: &str = "SEC0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn cfg(webhook: &str) -> DingtalkConfig {
        DingtalkConfig {
            webhook: webhook.to_string(),
        }
    }

    #[test]
    fn sign_matches_official_sample() {
        // 期望值用 node:crypto 独立算出来的（key=secret, message="{ts}\n{secret}"）
        let got = sign(SECRET, 1_791_445_000_000).expect("加签不该失败");
        assert_eq!(
            got, "ZGXlsEVlEJBlP+Yk2pHscf1jchKOHKP1wNfljhqdqEU=",
            "加签结果必须与官方算法一致，实际：{got}"
        );
    }

    #[test]
    fn urlencode_escapes_base64_specials() {
        let raw = sign("abc", 1_791_445_000_123).expect("加签不该失败");
        assert!(
            raw.contains('+') && raw.ends_with('='),
            "这个样例本身应含 + 与 =，实际：{raw}"
        );
        let encoded = urlencode(&raw);
        assert!(
            !encoded.contains('+') && !encoded.contains('=') && !encoded.contains('/'),
            "base64 里的 + / = 必须被编码，否则加签校验会失败，实际：{encoded}"
        );
        assert_eq!(
            encoded, "FpMO3xvpKeQytuqM4XO49%2B%2B0Y3SXMva90EyYNjgvryA%3D",
            "百分号编码必须与 encodeURIComponent 一致"
        );
    }

    #[test]
    fn build_url_without_secret_has_no_sign() {
        let url = build_url(
            &cfg("0123456789abcdef0123456789abcdef"),
            None,
            1_791_445_000_000,
        )
        .expect("应能组装 URL");
        assert_eq!(
            url,
            "https://oapi.dingtalk.com/robot/send?access_token=0123456789abcdef0123456789abcdef",
            "没配加签时不该带 timestamp/sign"
        );
    }

    #[test]
    fn build_url_with_secret_carries_timestamp_and_sign() {
        let url = build_url(&cfg("tok123"), Some(SECRET), 1_791_445_000_000).expect("应能组装 URL");
        assert!(
            url.contains("&timestamp=1791445000000&sign="),
            "加签时 URL 必须带 timestamp 与 sign，实际：{url}"
        );
        assert!(
            url.contains("sign=ZGXlsEVlEJBlP%2BYk2pHscf1jchKOHKP1wNfljhqdqEU%3D"),
            "sign 必须是编码后的 base64，实际：{url}"
        );
    }

    #[test]
    fn build_url_keeps_a_custom_host() {
        // 回归：以前不管用户填什么 host，都被改写成官方域名。
        // 后果有两个：① 企业自建网关永远用不上；② 「填个不可达地址测重试」根本测不到。
        let url = build_url(
            &cfg("http://127.0.0.1:8899/robot/send?access_token=tok123"),
            Some(SECRET),
            1_791_445_000_000,
        )
        .expect("应能组装 URL");
        assert!(
            url.starts_with("http://127.0.0.1:8899/robot/send?access_token=tok123"),
            "用户填的 host 必须保留，实际：{url}"
        );
        assert!(
            url.contains("&timestamp=1791445000000&sign="),
            "已有查询参数时要用 & 接加签参数，实际：{url}"
        );

        // 没有查询参数时得用 ? 接，否则加签参数会被当成路径的一部分
        let bare =
            build_url(&cfg("http://127.0.0.1:8899/hook"), Some(SECRET), 1).expect("裸路径也该能拼");
        assert!(
            bare.contains("/hook?timestamp=1&sign="),
            "无查询参数时必须用 ? 起头，实际：{bare}"
        );
    }

    #[test]
    fn full_webhook_url_is_accepted() {
        let url = "https://oapi.dingtalk.com/robot/send?access_token=0123456789abcdef";
        assert_eq!(
            cfg(url).access_token(),
            "0123456789abcdef",
            "粘整条 webhook 时应自动取出 access_token"
        );
        assert_eq!(
            cfg("0123456789abcdef").access_token(),
            "0123456789abcdef",
            "只填裸 token 时原样返回"
        );
    }

    #[test]
    fn config_rejects_empty_webhook() {
        let err = DingtalkConfig::from_json("{}").expect_err("空配置必须报错");
        assert!(
            err.message.contains("webhook"),
            "错误信息应指出缺 webhook，实际：{}",
            err.message
        );
        assert_eq!(err.kind, ErrorKind::Config);
    }

    #[test]
    fn config_round_trips_through_json() {
        let json = cfg("https://oapi.dingtalk.com/robot/send?access_token=t1")
            .to_json()
            .expect("序列化不该失败");
        let back = DingtalkConfig::from_json(&json).expect("反序列化不该失败");
        assert_eq!(back.access_token(), "t1", "配置应能往返");
    }

    #[test]
    fn classify_known_errcodes() {
        assert_eq!(
            classify_error(200, 300001, "token is not exist"),
            ErrorKind::Auth,
            "300001 应判为凭证问题"
        );
        assert_eq!(
            classify_error(200, 410100, "send too fast, exceed 20 times per minute"),
            ErrorKind::QuotaExceeded,
            "410100 应判为限流"
        );
        assert_eq!(
            classify_error(200, 310000, "keywords not in content"),
            ErrorKind::BadRequest,
            "310000 应判为参数/配置类"
        );
        assert_eq!(
            classify_error(200, 0, "ok"),
            ErrorKind::Retryable,
            "errcode=0 走到这里说明状态码异常，按可重试处理"
        );
    }

    #[test]
    fn classify_http_level_beats_errcode() {
        assert_eq!(
            classify_error(503, 310000, "service unavailable"),
            ErrorKind::Retryable,
            "5xx 优先按可重试处理"
        );
        assert_eq!(
            classify_error(429, 0, ""),
            ErrorKind::QuotaExceeded,
            "HTTP 429 按限流处理"
        );
    }

    #[test]
    fn classify_rate_limit_by_text_fallback() {
        assert_eq!(
            classify_error(200, 130101, "请求过于频繁，请稍后再试"),
            ErrorKind::QuotaExceeded,
            "错误码变了也要靠文案认出限流"
        );
    }

    #[test]
    fn parse_ok_response() {
        let got = parse_send_response(200, r#"{"errcode":0,"errmsg":"ok"}"#).expect("应解析成功");
        assert!(
            got.contains("errcode=0"),
            "成功凭据应说明 errcode=0，实际：{got}"
        );
    }

    #[test]
    fn parse_error_keeps_original_code_and_msg() {
        let err = parse_send_response(200, r#"{"errcode":310000,"errmsg":"sign not match"}"#)
            .expect_err("应报错");
        assert_eq!(err.kind, ErrorKind::BadRequest);
        assert_eq!(err.http_code, Some(200));
        assert!(
            err.message.contains("310000") && err.message.contains("sign not match"),
            "必须保留原始 code/msg，实际：{}",
            err.message
        );
        assert!(
            err.message.contains("加签不对"),
            "加签类错误要给出可操作的提示，实际：{}",
            err.message
        );
    }

    #[test]
    fn parse_non_json_response() {
        let err = parse_send_response(502, "<html>bad gateway</html>").expect_err("应报错");
        assert_eq!(err.kind, ErrorKind::Retryable, "502 应判为可重试");
        assert!(
            err.message.contains("bad gateway"),
            "应把响应片段带进错误信息，实际：{}",
            err.message
        );
    }

    #[test]
    fn send_body_shape() {
        let body = send_body("【待办提醒】开会");
        assert_eq!(
            body["msgtype"], "text",
            "钉钉文本消息的 msgtype 必须是 text"
        );
        assert_eq!(
            body["text"]["content"], "【待办提醒】开会",
            "正文要放在 text.content 里"
        );
    }
}
