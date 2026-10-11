# Ringtone · M3 里程碑报告（飞书渠道打通）

日期：2026-10-08 ｜ 状态：✅ 完成并有实测证据 ｜ 上一步：[M2 到点调度](./M2-report.md)

## 一句话结论

到点之后，Ringtone 会**真的把提醒发到飞书**（群或单聊）；渠道发不出去时**不会静默丢**——立刻用本机系统通知兜底，并把失败原因写进发送记录。

## 实测证据（都是真跑，不是单测）

### 1. 正常路径：到点 → 飞书群收到

在名为 `ai` 的群里，机器人发出了一条真实消息：

| 项目 | 值 |
| --- | --- |
| 任务 | `M3 冒烟·飞书到点推送` |
| 应触发时刻 | `1791441844050` |
| 实际发送时刻 | `1791441845360` |
| **时延** | **1310 ms（1.31 秒）** |
| 实例状态 | `occurrences.state = sent` |
| 发送结果 | `send_log.result = sent`，`HTTP 200` |
| 飞书消息 ID | `om_x100b6347f58d78acb4b5152268f321b` |
| 该实例的日志条数 | **1**（无重复发送） |

### 2. 失败路径：故意把密钥改错

| 项目 | 值 |
| --- | --- |
| 任务 | `M3 冒烟·错误密钥兜底` |
| 渠道返回 | `code=10014 msg=app secret invalid`（HTTP 200，业务错误码） |
| 分类结果 | `Auth` → **不可重试**，不再浪费时间重试 |
| 记录 | `send_log.result = failed`，`detail` 原样保留飞书 `code`/`msg` |
| 兜底 | 同一秒改走本机系统通知，人不会漏掉提醒 |
| 时延 | 797 ms |

跑完已把密钥恢复成正确的（长度 32）。

### 3. 界面

窗口右上角「推送设置」：飞书 `App ID` / `App Secret` / 接收方类型 / 接收方，以及
「保存并启用」「验证凭据（不发消息）」「发测试消息」「停用 / 启用」「删除」，
下方是发送记录（时间 / 渠道 / 结果 / HTTP 状态 / 详情）。

## 关键设计决定（以及为什么）

| 决定 | 理由 |
| --- | --- |
| 用**自建应用**而不是「自定义机器人 webhook」 | webhook 机器人**只能发群**，且限制更严（100 次/分、5 次/秒）；自建应用既能发群也能发单聊（`receive_id_type` 支持 `chat_id` / `email` / `open_id` / `user_id`） |
| 错误分成 `Retryable` / `QuotaExceeded` / `Auth` / `BadRequest` / `Config` | 这是 M4 降级链的唯一依据：`Auth`（凭证失效）和额度耗尽重试毫无意义，必须**立刻**换渠道；只有网络 / 5xx / 429 才值得退避 |
| 只有 `Auth` 会**内联**重试一次（强刷 token 再发） | 飞书 token 有效期 2 小时，缓存窗口内偶发失效是唯一值得原地重试的情况；其他错误重试只会重复失败 |
| 密钥单独存 `credential_enc`，**不回传前端** | 界面只需要知道「密钥已存 / 未填」；密钥永远不出现在 IPC 响应和日志里（单测断言序列化结果不含密钥串） |
| 保存时 `app_secret` 留空 = 不改动已存密钥 | 否则用户每改一次接收方就要重打一次密钥 |
| 没配渠道 / 渠道失败 → 本机通知兜底 + 写日志 | 你明确要求的是「提醒要送到人眼前」，推送只是手段；静默失败是这类工具最致命的坑 |

## 代码改动

新增：

- `src-tauri/src/channel/mod.rs`：`OutboundMessage` / `ErrorKind` / `SendError` / `send_via` / `render_text`
- `src-tauri/src/channel/feishu.rs`：取 token、发消息、错误码分类、token 缓存
- `src-tauri/src/deliver.rs`：渠道选择、发送编排、失败兜底、测试发送

改动：

- `src-tauri/src/db.rs`：渠道 CRUD（`list_channels` / `channel_record` / `primary_channel` / `upsert_channel` / `set_channel_enabled` / `delete_channel`）、`LogEntry` + `recent_logs`；`log_event` 增加 `channel_id` / `http_code` 两个参数（4 处调用点已全部更新）
- `src-tauri/src/commands.rs`：7 个新命令
- `src-tauri/src/lib.rs`：`tick_once` 改走 `deliver::deliver`，注册新命令
- `src-tauri/src/scheduler.rs`：截断逻辑抽成 `truncate_chars` 供复用
- 前端：`src/api.ts`（渠道/日志类型与命令封装）、新建 `src/Settings.tsx`（推送设置面板）、`src/App.tsx`（入口按钮）、`src/styles.css`（面板样式）

## 验证

| 检查 | 结果 |
| --- | --- |
| `cargo fmt --check` | 退出码 0 |
| `cargo check --all-targets` | 退出码 0，0 警告 |
| `cargo clippy --all-targets` | 0 警告 |
| `cargo test --lib` | **61 passed / 0 failed**（M2 为 41 条，本次 +20） |
| `pnpm exec tsc --noEmit` | 退出码 0 |
| `pnpm exec vite build` | 成功（index.js 230.11 kB，gzip 72.27 kB） |

## 尚未验证 / 已知缺口

1. **界面按钮的命令层没有自动化用例**（`save_feishu_channel` / `test_channel` / `verify_channel` 要真的点一下）。它们的下半段（`upsert_channel`、`channel::send_via`）已被单测和上面的端到端覆盖，但按钮本身请你在界面上点一次确认。
2. 失败后**没有指数退避重试**（只兜底 + 记日志）——计划在 M4 和降级链一起做。
3. 密钥目前是**明文**存在本机 SQLite 的 `credential_enc` 列（列名预留了加密，M5 接 Windows DPAPI）。
4. 数据库里留了 6 条冒烟用的测试任务（`M2 冒烟*`、`M3 冒烟*`），需要清理请说一声。

## 下一步（M4 建议）

钉钉（webhook + 加签）→ PushPlus → **降级链**（主渠道失败按 `ErrorKind` 决定「退避重试」还是「立刻换下一个渠道」）→ 限流（钉钉 20 条/分是最大雷区）→ 同渠道 60 秒合并成一条摘要省额度。

需要你提供：钉钉机器人 webhook + 加签密钥、PushPlus token（**PushPlus 要先实名，否则额度是 0**）。
