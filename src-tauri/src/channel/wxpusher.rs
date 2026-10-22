//! WxPusher 渠道（<https://wxpusher.zjiecode.com>）。
//!
//! 接入方式（标准推送 API）：
//! 后台创建应用拿 `appToken`（`AT_` 开头）→ 让接收者关注应用拿到 `UID`（`UID_` 开头）→
//! `POST /api/send/message` 发消息。**appToken 是密钥**，所以放 `channels.credential_enc`
//! （和其它渠道的密钥同一列），UID 列表放 `config_json`。
//!
//! 官方限制（抄进代码是为了别在夜里被拦还不明白为什么）：
//! - 接口 **QPS ≤ 1**（连续 10 秒打 10 次会被拦），单 UID 每天 ≤ 2000 条；
//! - `content` ≤ 40000 字符且 UTF-8 编码后 ≤ 65535 字节；`summary` ≤ 20 字符（超了微信侧显示不全）；
//! - 单次最多 2000 个 UID。
//! - 业务码：`1000` 成功、`1001` 通用业务错误（含内容超长）、`1002` 未登录 / appToken 无效。

use super::{ErrorKind, SendError, SendOutcome};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

pub const KIND: &str = "wxpusher";
pub const SEND_URL: &str = "https://wxpusher.zjiecode.com/api/send/message";
pub const TIMEOUT_SECS: u64 = 10;
/// 官方是「QPS ≤ 1」，我们的闸门按分钟算：10 条/分既够用（提醒是稀疏事件），
/// 也不会因为一次 tick 补发几条而撞上 QPS。
pub const PER_MINUTE_LIMIT: i64 = 10;
/// 单次最多 2000 个 UID，这里按「一个人用」夹到 20 个：多了没意义，还容易配错。
pub const MAX_UIDS: usize = 20;
/// `summary` 官方上限 20 字符（微信通知栏显示不全）。
pub const SUMMARY_CHARS: usize = 20;
pub const TOKEN_PREFIX: &str = "AT_";
pub const UID_PREFIX: &str = "UID_";

/// 存在 `config_json` 里的部分（不含密钥）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WxpusherConfig {
    pub uids: Vec<String>,
}

/// 把用户填的一段文本拆成 UID 列表：逗号（中英文）、空格、分号、换行都当分隔符，去重保序。
pub fn parse_uids(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in raw.split(|c: char| {
        c.is_whitespace() || c == ',' || c == '，' || c == ';' || c == '；' || c == '、'
    }) {
        let uid = part.trim();
        if uid.is_empty() {
            continue;
        }
        if !out.iter().any(|seen| seen == uid) {
            out.push(uid.to_string());
        }
    }
    out
}

impl WxpusherConfig {
    pub fn from_json(raw: &str) -> Result<Self, SendError> {
        let parsed: WxpusherConfig = serde_json::from_str(raw)
            .map_err(|e| SendError::config(format!("WxPusher 配置解析失败：{e}")))?;
        let cfg = parsed.normalized();
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// 去掉空白、去重、夹上限。**不改大小写**：UID 是大小写敏感的。
    pub fn normalized(&self) -> Self {
        let mut uids: Vec<String> = Vec::new();
        for raw in &self.uids {
            for uid in parse_uids(raw) {
                if !uids.iter().any(|seen| seen == &uid) {
                    uids.push(uid);
                }
            }
        }
        uids.truncate(MAX_UIDS);
        Self { uids }
    }

    pub fn validate(&self) -> Result<(), SendError> {
        if self.uids.is_empty() {
            return Err(SendError::config(
                "WxPusher 渠道至少要填一个 UID（形如 UID_xxxxx）",
            ));
        }
        for uid in &self.uids {
            if !uid.starts_with(UID_PREFIX) || uid.len() <= UID_PREFIX.len() {
                return Err(SendError::config(format!(
                    "WxPusher 的 UID 要以 {UID_PREFIX} 开头，这个看起来不对：{uid}"
                )));
            }
        }
        Ok(())
    }
}

/// 正文短于上限就原样；超了硬截到 n 个字符（不加省略号——官方按字符数算，多加一个就超）。
pub fn clip_chars(raw: &str, max_chars: usize) -> String {
    let trimmed = raw.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    trimmed.chars().take(max_chars).collect()
}

/// 请求体。`contentType = 1`（纯文本）；`summary` 用任务标题（通知栏那一行）。
pub fn send_body(cfg: &WxpusherConfig, app_token: &str, title: &str, text: &str) -> Value {
    json!({
        "appToken": app_token,
        "content": text,
        "summary": clip_chars(title, SUMMARY_CHARS),
        "contentType": 1,
        "uids": cfg.uids,
    })
}

/// 业务码分类。HTTP 层优先（网关 5xx / 429 跟业务码没关系）。
pub fn classify_error(http_status: u16, code: i64, msg: &str) -> ErrorKind {
    if http_status == 429 {
        return ErrorKind::QuotaExceeded;
    }
    if http_status == 408 || http_status >= 500 {
        return ErrorKind::Retryable;
    }
    if http_status == 401 || http_status == 403 {
        return ErrorKind::Auth;
    }

    let lowered = msg.to_lowercase();
    if lowered.contains("频率")
        || lowered.contains("限流")
        || lowered.contains("too fast")
        || lowered.contains("超过")
        || lowered.contains("超过限制")
    {
        return ErrorKind::QuotaExceeded;
    }

    match code {
        // 只有非 2xx 才会走到这里：接口说成功了却是 HTTP 异常，只可能是网关插了一脚。
        1000 => ErrorKind::Retryable,
        1001 => ErrorKind::BadRequest,
        1002 => ErrorKind::Auth,
        // 文档只列了 1001/1002，其余按「服务端处理异常」处理：重试一次比直接换渠道合理。
        1005 => ErrorKind::Retryable,
        _ => ErrorKind::BadRequest,
    }
}

/// 解析发送响应。成功时把 `messageContentId` / `sendRecordId` 拼成回执串写进发送记录。
pub fn parse_send_response(http_status: u16, body: &str) -> SendOutcome {
    let parsed: Value = serde_json::from_str(body).map_err(|_| {
        SendError::new(
            ErrorKind::Retryable,
            Some(http_status as i64),
            format!(
                "WxPusher 返回的不是 JSON：{}",
                body.chars().take(120).collect::<String>()
            ),
        )
    })?;

    let code = parsed.get("code").and_then(Value::as_i64).unwrap_or(-1);
    let msg = parsed
        .get("msg")
        .and_then(Value::as_str)
        .unwrap_or("（没有 msg）")
        .to_string();

    let http_ok = (200..300).contains(&http_status);
    if http_ok && code == 1000 {
        let first = parsed
            .get("data")
            .and_then(Value::as_array)
            .and_then(|items| items.first());
        let content_id = first
            .and_then(|d| d.get("messageContentId"))
            .and_then(Value::as_i64);
        let record_id = first
            .and_then(|d| d.get("sendRecordId"))
            .and_then(Value::as_i64);
        let receipt = match (content_id, record_id) {
            (Some(c), Some(r)) => format!("messageContentId={c} sendRecordId={r}"),
            (Some(c), None) => format!("messageContentId={c}"),
            (None, Some(r)) => format!("sendRecordId={r}"),
            (None, None) => "code=1000（响应未带 id）".to_string(),
        };
        return Ok(receipt);
    }

    // 1001 是**通用业务错误**：实测 appToken 写错也走它（服务端的 msg 会说明原因），
    // 所以除了 1002 以外不再自己猜「是哪一项错了」。
    let tip = match code {
        1002 => "（appToken 无效或没登录，去 WxPusher 后台确认）",
        _ => "",
    };
    Err(SendError::new(
        classify_error(http_status, code, &msg),
        Some(http_status as i64),
        format!("WxPusher 返回 code={code} msg={msg}{tip}"),
    ))
}

pub struct WxpusherClient {
    cfg: WxpusherConfig,
    app_token: String,
    http: Client,
}

impl WxpusherClient {
    pub fn new(cfg: WxpusherConfig, app_token: String) -> Result<Self, SendError> {
        let cfg = cfg.normalized();
        cfg.validate()?;
        let app_token = app_token.trim().to_string();
        if !app_token.starts_with(TOKEN_PREFIX) || app_token.len() <= TOKEN_PREFIX.len() {
            return Err(SendError::config(format!(
                "WxPusher 的 appToken 要以 {TOKEN_PREFIX} 开头（在后台「应用管理」里能看到）"
            )));
        }
        let http = Client::builder()
            .timeout(Duration::from_secs(TIMEOUT_SECS))
            .build()
            .map_err(|e| SendError::config(format!("HTTP 客户端创建失败：{e}")))?;
        Ok(Self {
            cfg,
            app_token,
            http,
        })
    }

    /// WxPusher 没有「只验证凭据不发消息」的接口：这里只做静态校验，
    /// 真要确认能不能发，请用「发测试消息」。返回一句说明，界面直接显示。
    pub fn verify(&self) -> Result<String, SendError> {
        Ok(format!(
            "配置看着没问题（appToken {}…、{} 个 UID）；WxPusher 没有单独的校验接口，点「发测试消息」才能真正确认",
            self.app_token.chars().take(8).collect::<String>(),
            self.cfg.uids.len()
        ))
    }

    pub fn send_text(&self, title: &str, text: &str) -> SendOutcome {
        let body = send_body(&self.cfg, &self.app_token, title, text);
        let resp = self
            .http
            .post(SEND_URL)
            .header("Content-Type", "application/json")
            .json(&body)
            .send();

        match resp {
            Ok(r) => {
                let status = r.status().as_u16();
                let text = r.text().unwrap_or_default();
                parse_send_response(status, &text)
            }
            Err(e) => {
                // 超时 / 连不上都算可重试（和别的渠道一致）
                Err(SendError::new(
                    ErrorKind::Retryable,
                    None,
                    format!("WxPusher 请求失败：{e}"),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(uids: &[&str]) -> WxpusherConfig {
        WxpusherConfig {
            uids: uids.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn parse_uids_splits_on_common_separators() {
        let got = parse_uids("UID_aaa, UID_bbb\nUID_ccc；UID_aaa\tUID_ddd");
        assert_eq!(
            got,
            vec![
                "UID_aaa".to_string(),
                "UID_bbb".to_string(),
                "UID_ccc".to_string(),
                "UID_ddd".to_string()
            ],
            "应支持中英文逗号 / 空格 / 换行 / 分号，并去重保序，实际：{got:?}"
        );
    }

    #[test]
    fn normalize_truncates_to_max_uids() {
        let many: Vec<String> = (0..(MAX_UIDS + 5)).map(|i| format!("UID_{i}")).collect();
        let got = WxpusherConfig { uids: many }.normalized();
        assert_eq!(got.uids.len(), MAX_UIDS, "UID 数量应被夹到 {MAX_UIDS}");
    }

    #[test]
    fn validate_rejects_bad_uid_and_empty() {
        assert!(cfg(&[]).validate().is_err(), "空 UID 列表应报错");
        assert!(
            cfg(&["not-a-uid"]).validate().is_err(),
            "不以 UID_ 开头的值应报错"
        );
        assert!(cfg(&["UID_Kk0cy"]).validate().is_ok(), "正常 UID 应通过");
    }

    #[test]
    fn client_requires_at_prefix() {
        assert!(
            WxpusherClient::new(cfg(&["UID_x"]), "wrong".to_string()).is_err(),
            "appToken 不以 AT_ 开头应报错"
        );
        assert!(
            WxpusherClient::new(cfg(&["UID_x"]), "AT_abc".to_string()).is_ok(),
            "AT_ 开头的 token 应通过"
        );
    }

    #[test]
    fn send_body_has_token_uids_and_clipped_summary() {
        let long_title = "这是一个标题很长的任务用来检查摘要截断逻辑是否按字符数执行";
        let body = send_body(&cfg(&["UID_a", "UID_b"]), "AT_tok", long_title, "正文内容");
        assert_eq!(body["appToken"], "AT_tok", "appToken 应在 body 里");
        assert_eq!(body["contentType"], 1, "应发纯文本（contentType=1）");
        assert_eq!(body["content"], "正文内容", "content 应是正文");
        assert_eq!(
            body["uids"].as_array().map(|a| a.len()),
            Some(2),
            "两个 UID 都应带上"
        );
        let summary = body["summary"].as_str().unwrap_or_default();
        assert_eq!(
            summary.chars().count(),
            SUMMARY_CHARS,
            "摘要应按字符截到 {SUMMARY_CHARS}，实际：{summary}"
        );
    }

    #[test]
    fn clip_chars_is_charwise_not_bytewise() {
        assert_eq!(clip_chars("中文标题", 3), "中文标", "应按字符截断");
        assert_eq!(clip_chars("  短  ", 10), "短", "应 trim");
    }

    #[test]
    fn parse_success_reads_ids() {
        let body = r#"{"code":1000,"msg":"处理成功","data":[{"uid":"UID_a","messageContentId":2123,"sendRecordId":12313,"code":1000,"status":"创建发送任务成功"}],"success":true}"#;
        let got = parse_send_response(200, body).expect("code=1000 应判成功");
        assert_eq!(
            got, "messageContentId=2123 sendRecordId=12313",
            "回执应包含两个 id，实际：{got}"
        );
    }

    #[test]
    fn parse_success_without_data_still_ok() {
        let got = parse_send_response(200, r#"{"code":1000,"msg":"处理成功","data":null}"#)
            .expect("没有 data 也算成功");
        assert!(got.contains("1000"), "应给个说明，实际：{got}");
    }

    #[test]
    fn parse_error_keeps_code_and_msg() {
        let err = parse_send_response(200, r#"{"code":1002,"msg":"未登录","data":null}"#)
            .expect_err("code=1002 应失败");
        assert_eq!(err.kind, ErrorKind::Auth, "1002 应判成鉴权问题");
        assert!(err.message.contains("1002"), "错误信息应带业务码");
        assert!(err.message.contains("未登录"), "错误信息应带原始 msg");
        assert!(
            err.message.contains("appToken"),
            "1002 要给 appToken 的提示"
        );
    }

    #[test]
    fn classify_covers_http_and_business_codes() {
        assert_eq!(classify_error(429, 0, ""), ErrorKind::QuotaExceeded);
        assert_eq!(classify_error(503, 0, ""), ErrorKind::Retryable);
        assert_eq!(classify_error(408, 0, ""), ErrorKind::Retryable);
        assert_eq!(classify_error(403, 0, ""), ErrorKind::Auth);
        assert_eq!(classify_error(200, 1001, "参数错误"), ErrorKind::BadRequest);
        assert_eq!(classify_error(200, 1002, "未登录"), ErrorKind::Auth);
        assert_eq!(classify_error(200, 1005, "接口异常"), ErrorKind::Retryable);
        assert_eq!(
            classify_error(200, 4000, "超过频率限制"),
            ErrorKind::QuotaExceeded,
            "文案里提到频率/限流应判额度问题"
        );
        assert_eq!(classify_error(200, 4000, "未知"), ErrorKind::BadRequest);
    }

    #[test]
    fn non_json_response_is_retryable() {
        let err = parse_send_response(502, "<html>bad gateway</html>").expect_err("非 JSON 应报错");
        assert_eq!(err.kind, ErrorKind::Retryable, "502 应可重试");
    }
}
