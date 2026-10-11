//! 飞书渠道：自建应用凭证（`tenant_access_token`）+ `im/v1/messages`。
//!
//! 为什么用自建应用而不是「自定义机器人 webhook」：
//! - 自定义机器人**只能发到群里**，且受 100 次/分钟、5 次/秒 限制；
//! - 自建应用既能发群（`chat_id`），也能发单聊（`open_id` / `email`），
//!   限流更宽（1000 次/分钟、50 次/秒），换接收方只要改 `receive_id_type`。
//!
//! 凭证与接收方的存放：`channels.config_json` 存 `app_id` / `receive_id_type` / `receive_id`，
//! `channels.credential_enc` 单独存 `app_secret`（**不进配置文件、不进日志、不回传前端**）。

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{ErrorKind, SendError, SendOutcome};
use crate::time;

/// `channels.kind` 的取值
pub const KIND: &str = "feishu";
/// 取 tenant_access_token
pub const TOKEN_URL: &str = "https://open.feishu.cn/open-apis/auth/v3/tenant_access_token/internal";
/// 发消息（`receive_id_type` 走 query）
pub const SEND_URL: &str = "https://open.feishu.cn/open-apis/im/v1/messages";
/// 单次 HTTP 超时。调度每 5 秒一跳，一次请求卡久了会拖住整轮 tick。
const TIMEOUT_SECS: u64 = 10;
/// 提前 60 秒判定 token 过期，避免「刚好在过期瞬间发出去」的边界失败
const TOKEN_SAFETY_MARGIN_MS: i64 = 60_000;
const DEFAULT_RECEIVE_ID_TYPE: &str = "chat_id";
const ERR_BODY_CHARS: usize = 300;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeishuConfig {
    pub app_id: String,
    /// `chat_id`（群）/ `open_id` / `user_id` / `email` / `union_id`
    #[serde(default = "default_receive_id_type")]
    pub receive_id_type: String,
    /// 配合 `receive_id_type` 的接收方标识
    pub receive_id: String,
}

fn default_receive_id_type() -> String {
    DEFAULT_RECEIVE_ID_TYPE.to_string()
}

impl FeishuConfig {
    /// 从 `channels.config_json` 解析并校验。任何不完整都在这里拦住，
    /// 不要让半个配置跑到 HTTP 层去换一个看不懂的报错。
    pub fn from_json(raw: &str) -> Result<Self, SendError> {
        let cfg: Self = serde_json::from_str(raw)
            .map_err(|e| SendError::config(format!("飞书配置不是合法 JSON：{e}")))?;
        let cfg = cfg.normalized();
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn to_json(&self) -> Result<String, SendError> {
        serde_json::to_string(self)
            .map_err(|e| SendError::config(format!("飞书配置无法序列化：{e}")))
    }

    /// 去空白 + 补默认接收方类型（手填配置很容易多一个空格）
    pub fn normalized(mut self) -> Self {
        self.app_id = self.app_id.trim().to_string();
        self.receive_id = self.receive_id.trim().to_string();
        self.receive_id_type = self.receive_id_type.trim().to_string();
        if self.receive_id_type.is_empty() {
            self.receive_id_type = DEFAULT_RECEIVE_ID_TYPE.to_string();
        }
        self
    }

    fn validate(&self) -> Result<(), SendError> {
        if self.app_id.is_empty() {
            return Err(SendError::config("飞书配置缺少 app_id"));
        }
        if self.receive_id.is_empty() {
            return Err(SendError::config(
                "飞书配置缺少接收方（群的 chat_id 或用户的 email 等）",
            ));
        }
        Ok(())
    }
}

/// 业务错误码 → 分类。纯函数，可离线单测；**新增已知错误码时改这里**。
pub fn classify_error(http_status: u16, code: i64) -> ErrorKind {
    // HTTP 层先判：网络/服务端问题与业务码无关
    if http_status == 429 {
        return ErrorKind::QuotaExceeded;
    }
    if http_status >= 500 || http_status == 408 {
        return ErrorKind::Retryable;
    }
    if code == 0 {
        // HTTP 非 2xx 但业务码为 0：少见，按可重试的网络问题处理
        return ErrorKind::Retryable;
    }
    match code {
        // 触发限流
        11232 => ErrorKind::QuotaExceeded,
        // token 无效 / 过期 / 无权限：换一个 token 仍可能成功，先判 Auth 让它走「刷新一次再试」
        99_991_663 | 99_991_661 | 99_991_664 | 99_991_668 | 99_991_671 | 99_991_672 => {
            ErrorKind::Auth
        }
        // app_id 不存在 / app_secret 错
        10_003 | 10_014 => ErrorKind::Auth,
        // 接收方无效 / 机器人不在该群 / 无发消息权限：重试多少次都一样，必须换渠道
        230_001 | 230_002 | 230_013 | 230_020 | 99_992_361 => ErrorKind::BadRequest,
        // 其余未知业务码：请求已被理解但被拒，归 BadRequest（可重试的错误网络层已经拦掉了）
        _ => ErrorKind::BadRequest,
    }
}

/// HTTP 层错误（连接失败 / 超时 / 无法构造请求）
fn transport_error(e: reqwest::Error) -> SendError {
    SendError::new(ErrorKind::Retryable, None, format!("网络请求失败：{e}"))
}

fn clip(raw: &str) -> String {
    crate::scheduler::truncate_chars(raw, ERR_BODY_CHARS)
}

/// 把飞书的响应体解析成「错误」或 `Ok(送达凭据)`
pub fn parse_send_response(http_status: u16, body: &str) -> SendOutcome {
    let v: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            return Err(SendError::new(
                classify_error(http_status, -1),
                Some(http_status as i64),
                format!("响应不是合法 JSON（{e}）：{}", clip(body)),
            ))
        }
    };
    let code = v["code"].as_i64().unwrap_or(-1);
    if code == 0 && (200..300).contains(&http_status) {
        let message_id = v["data"]["message_id"].as_str().unwrap_or_default();
        return Ok(if message_id.is_empty() {
            format!("HTTP {http_status}（响应未带 message_id）")
        } else {
            format!("message_id={message_id}")
        });
    }

    let kind = classify_error(http_status, code);
    let msg = v["msg"]
        .as_str()
        .or_else(|| v["message"].as_str())
        .unwrap_or("（响应没有 msg 字段）");
    Err(SendError::new(
        kind,
        Some(http_status as i64),
        format!("飞书返回 code={code} msg={msg}"),
    ))
}

/// 解析取 token 的响应
fn parse_token_response(body: &str) -> Result<(String, i64), SendError> {
    let v: Value = serde_json::from_str(body).map_err(|e| {
        SendError::config(format!(
            "取 token 的响应不是合法 JSON（{e}）：{}",
            clip(body)
        ))
    })?;
    let code = v["code"].as_i64().unwrap_or(-1);
    if code != 0 {
        let msg = v["msg"].as_str().unwrap_or("（响应没有 msg 字段）");
        return Err(SendError::new(
            classify_error(200, code),
            None,
            format!("取 token 失败：code={code} msg={msg}"),
        ));
    }
    let token = v["tenant_access_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| SendError::config("取 token 成功但响应里没有 tenant_access_token"))?;
    let expire = v["expire"].as_i64().unwrap_or(7200).max(60);
    Ok((token.to_string(), expire))
}

/// 发送请求体。注意飞书的 `content` 是**字符串形式的 JSON**，不是嵌套对象。
pub fn send_body(cfg: &FeishuConfig, text: &str) -> Value {
    json!({
        "receive_id": cfg.receive_id,
        "msg_type": "text",
        "content": json!({ "text": text }).to_string(),
    })
}

struct CachedToken {
    value: String,
    good_until_ms: i64,
}

pub struct FeishuClient {
    cfg: FeishuConfig,
    secret: String,
    http: reqwest::blocking::Client,
    token: Option<CachedToken>,
}

impl FeishuClient {
    pub fn new(cfg: FeishuConfig, secret: String) -> Result<Self, SendError> {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(TIMEOUT_SECS))
            .build()
            .map_err(|e| SendError::config(format!("无法创建 HTTP 客户端：{e}")))?;
        Ok(Self {
            cfg,
            secret,
            http,
            token: None,
        })
    }

    /// 只取 token（「测试连接」用，不发送任何消息）
    pub fn verify(&mut self) -> Result<String, SendError> {
        let expires_in = {
            let before = time::now_ms();
            let _ = before;
            self.ensure_token(true)?;
            self.token
                .as_ref()
                .map(|t| (t.good_until_ms - time::now_ms()) / 1000)
                .unwrap_or(0)
        };
        Ok(format!("凭证有效，token 可用约 {expires_in} 秒"))
    }

    /// 发一条纯文本消息，成功返回渠道侧 `message_id`
    pub fn send_text(&mut self, text: &str) -> SendOutcome {
        let token = self.ensure_token(false)?;
        let first = self.send_once(&token, text);
        match first {
            Err(e) if e.kind == ErrorKind::Auth => {
                // token 可能刚好在这一刻失效：强刷一次再试一回。
                // 只重试这一种情况 —— 真正的重试/退避/多级降级是 M4 的事。
                let fresh = self.ensure_token(true)?;
                self.send_once(&fresh, text)
            }
            other => other,
        }
    }

    fn ensure_token(&mut self, force: bool) -> Result<String, SendError> {
        let now = time::now_ms();
        if !force {
            if let Some(t) = &self.token {
                if now < t.good_until_ms {
                    return Ok(t.value.clone());
                }
            }
        }

        let resp = self
            .http
            .post(TOKEN_URL)
            .json(&json!({ "app_id": self.cfg.app_id, "app_secret": self.secret }))
            .send()
            .map_err(transport_error)?;
        let body = resp.text().unwrap_or_default();
        // 取 token 的失败几乎都是业务码层面的（app_id 不存在 / secret 错），
        // HTTP 状态码在 parse_token_response 里不参与分类，所以不单独取。
        let (value, expire_secs) = parse_token_response(&body)?;

        self.token = Some(CachedToken {
            value: value.clone(),
            good_until_ms: now + expire_secs * 1000 - TOKEN_SAFETY_MARGIN_MS,
        });
        Ok(value)
    }

    fn send_once(&self, token: &str, text: &str) -> SendOutcome {
        let resp = self
            .http
            .post(SEND_URL)
            .query(&[("receive_id_type", self.cfg.receive_id_type.as_str())])
            .bearer_auth(token)
            .json(&send_body(&self.cfg, text))
            .send()
            .map_err(transport_error)?;
        let status = resp.status().as_u16();
        let body = resp.text().unwrap_or_default();
        parse_send_response(status, &body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_known_codes() {
        assert_eq!(
            classify_error(200, 11232),
            ErrorKind::QuotaExceeded,
            "11232 是限流"
        );
        assert_eq!(
            classify_error(200, 99991663),
            ErrorKind::Auth,
            "token 无效是 Auth"
        );
        assert_eq!(
            classify_error(200, 10014),
            ErrorKind::Auth,
            "app_secret 错是 Auth"
        );
        assert_eq!(
            classify_error(200, 230002),
            ErrorKind::BadRequest,
            "机器人不在群里是 BadRequest"
        );
        assert_eq!(
            classify_error(200, 99992361),
            ErrorKind::BadRequest,
            "无权限是 BadRequest"
        );
        assert_eq!(
            classify_error(200, 987654321),
            ErrorKind::BadRequest,
            "未知业务码归 BadRequest"
        );
    }

    #[test]
    fn classify_http_layer_beats_business_code() {
        assert_eq!(
            classify_error(500, 0),
            ErrorKind::Retryable,
            "5xx 一律可重试"
        );
        assert_eq!(
            classify_error(503, 230002),
            ErrorKind::Retryable,
            "5xx 优先于业务码"
        );
        assert_eq!(
            classify_error(429, 0),
            ErrorKind::QuotaExceeded,
            "HTTP 429 是限流"
        );
        assert_eq!(
            classify_error(200, 0),
            ErrorKind::Retryable,
            "业务码 0 却走错误分支时按网络问题处理"
        );
    }

    #[test]
    fn parse_ok_response_returns_message_id() {
        let body = r#"{"code":0,"msg":"success","data":{"message_id":"om_abc123"}}"#;
        let got = parse_send_response(200, body).expect("code=0 应当成功");
        assert_eq!(got, "message_id=om_abc123", "应取出 message_id 并自描述");
    }

    #[test]
    fn parse_ok_response_without_message_id_is_still_ok() {
        let got =
            parse_send_response(200, r#"{"code":0,"msg":"success"}"#).expect("没有 data 也算成功");
        assert!(
            got.contains("未带 message_id"),
            "应说明缺 message_id，实际：{got}"
        );
    }

    #[test]
    fn parse_error_response_keeps_code_and_msg() {
        let body = r#"{"code":230002,"msg":"Bot is not in the chat"}"#;
        let err = parse_send_response(400, body).expect_err("非 0 业务码必须报错");
        assert_eq!(err.kind, ErrorKind::BadRequest, "230002 应为 BadRequest");
        assert_eq!(err.http_code, Some(400), "应保留 HTTP 状态码");
        assert!(
            err.message.contains("230002") && err.message.contains("Bot is not in the chat"),
            "错误信息应带上飞书原始 code/msg，实际：{}",
            err.message
        );
    }

    #[test]
    fn parse_non_json_response_is_an_error() {
        let err =
            parse_send_response(502, "<html>bad gateway</html>").expect_err("非 JSON 必须报错");
        assert_eq!(err.kind, ErrorKind::Retryable, "502 + 非 JSON 应为可重试");
        assert!(
            err.message.contains("bad gateway"),
            "错误信息应保留原文片段以便排查，实际：{}",
            err.message
        );
    }

    #[test]
    fn token_response_parses_and_rejects_errors() {
        let ok = r#"{"code":0,"msg":"ok","tenant_access_token":"t-abc","expire":7200}"#;
        let (token, expire) = parse_token_response(ok).expect("code=0 应成功");
        assert_eq!(token, "t-abc", "应取出 token");
        assert_eq!(expire, 7200, "应取出有效期");

        let bad = r#"{"code":10014,"msg":"app secret invalid"}"#;
        let err = parse_token_response(bad).expect_err("code!=0 必须报错");
        assert_eq!(err.kind, ErrorKind::Auth, "app_secret 错应为 Auth");
        assert!(
            err.message.contains("app secret invalid"),
            "应保留原文：{}",
            err.message
        );
    }

    #[test]
    fn token_response_missing_token_is_config_error() {
        let err = parse_token_response(r#"{"code":0,"msg":"ok"}"#).expect_err("缺 token 必须报错");
        assert_eq!(err.kind, ErrorKind::Config, "缺字段应是配置类错误");
    }

    #[test]
    fn config_from_json_fills_defaults_and_trims() {
        let cfg = FeishuConfig::from_json(r#"{"app_id":" cli_x ","receive_id":" oc_y "}"#)
            .expect("应能解析");
        assert_eq!(cfg.app_id, "cli_x", "app_id 应去空白");
        assert_eq!(cfg.receive_id, "oc_y", "receive_id 应去空白");
        assert_eq!(
            cfg.receive_id_type, "chat_id",
            "缺失的 receive_id_type 应补默认 chat_id"
        );
    }

    #[test]
    fn config_from_json_rejects_incomplete() {
        let e1 =
            FeishuConfig::from_json(r#"{"receive_id":"oc_y"}"#).expect_err("缺 app_id 必须报错");
        assert_eq!(e1.kind, ErrorKind::Config, "缺 app_id 是配置错");

        let e2 =
            FeishuConfig::from_json(r#"{"app_id":"cli_x"}"#).expect_err("缺 receive_id 必须报错");
        assert_eq!(e2.kind, ErrorKind::Config, "缺 receive_id 是配置错");

        let e3 = FeishuConfig::from_json(r#"{"app_id":"cli_x","receive_id":"oc_y","extra":1}"#)
            .expect("多余字段应当被忽略而不是报错");
        assert_eq!(e3.receive_id, "oc_y", "多余字段不影响解析");

        let e4 = FeishuConfig::from_json("not json").expect_err("非法 JSON 必须报错");
        assert_eq!(e4.kind, ErrorKind::Config, "非法 JSON 是配置错");
    }

    #[test]
    fn send_body_embeds_content_as_json_string() {
        let cfg = FeishuConfig::from_json(r#"{"app_id":"cli_x","receive_id":"oc_y"}"#).unwrap();
        let body = send_body(&cfg, "【待办提醒】开会\n时间：14:30");
        assert_eq!(body["receive_id"], "oc_y", "应带上接收方");
        assert_eq!(body["msg_type"], "text", "本条只发纯文本");

        let content = body["content"]
            .as_str()
            .expect("content 必须是字符串形式的 JSON");
        let inner: Value = serde_json::from_str(content).expect("content 内部应是合法 JSON");
        assert_eq!(
            inner["text"], "【待办提醒】开会\n时间：14:30",
            "文本应原样落在 content.text 里"
        );
    }
}
