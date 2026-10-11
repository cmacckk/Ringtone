# KiteAlarm · 本地待办到点推送工具 — 正式方案（v1 待确认）

> 状态：已确认，M1 进行中
> 定位：Windows 本地待办 App（类 Kite 待办）+ 到点把提醒推送到社交渠道
> 名称：Ringtone（用户指定）｜目录：仓库根 F:/develop/Ringtone（不换目录）
> 日期：2026-02（规划稿）

## 0. 已确认决策（2026-02）

- 名称 `Ringtone`，就地开发于 `F:/develop/Ringtone` 仓库根目录
- 飞书落点：群 `ai`，`chat_id = <群chat_id>`（已实测机器人在群内；真实值只存本地数据库与本地配置，不写进仓库）
- 渠道清单缩减为 **3 个：飞书（群）、钉钉自定义机器人（群）、PushPlus**；**Server酱 不做**
- App secret 不重置（用户决定）；**绝不写入任何 git 跟踪文件**，仅存本地非跟踪配置/加密库
- 飞书机器人能力已开通（用户侧完成）

---

## 1. 目标与成功标准

**要解决的问题**：待办放在电脑里，到点只有电脑弹窗；人离开电脑或没看屏幕就漏掉。

**成功标准（必须可验证）**：设定的提醒时间到达后，在指定社交渠道收到该待办（标题 + 正文），且不重复、不因额度/限流而静默丢失。

## 2. 范围

### 做（IN）
- 待办 CRUD、今日视图、月总览（批量完成/未完成/导出 Markdown）
- 提醒时间 + 重复规则 + 多提醒点（提前 N 分钟/天）
- 4 个渠道适配器：飞书（自建应用单聊）、钉钉自定义机器人、PushPlus、Server酱
- 渠道能力表 + 令牌桶限流 + 日额度保护 + 自动降级
- 发送记录面板、渠道自测按钮、今日用量显示
- 本地弹窗兜底 + 托盘常驻 + 开机自启
- 密钥本地加密存储

### 不做（OUT，已确认）
- WebDAV / 云同步 / 多设备 / 多人协作 / 账号登录
- 双向交互（在钉钉里点“完成”）、公网回调、回调服务器
- 消息脱敏（按用户要求：内容不脱敏，明文发送）
- 权限管控 / 审批流
- 任何服务端、域名、云主机

## 3. 技术选型

| 项 | 选择 | 理由 / 风险 |
|---|---|---|
| 桌面壳 | Tauri 2（Rust + WebView2） | Kite 安装包 3.72MB，说明体积敏感；Electron 起步 80MB+ |
| 前端 | React + Vite（或 Vue，M1 定） | 生态成熟，macOS/Linux 预留 |
| 本地库 | SQLite（sqlx，静态链接） | 单文件、免安装、WAL |
| 调度器 | Rust tokio，1s tick 单进程 | 与 UI 同进程，无额外服务 |
| 平台 | Windows 10+ 优先 | 风险：需 WebView2 运行时（Kite 也踩过），M5 加检测与提示页 |

## 4. 数据模型（关键字段）

```sql
-- 时间统一存 epoch 毫秒 INTEGER，禁止文本时间排序（避免 RFC3339 vs 'YYYY-MM-DD HH:MM:SS' 混排）
task(id, title, body, due_at_ms INTEGER, remind_offsets TEXT/*json 例 [0,-300,-86400]*/,
     repeat_rule TEXT/*RRULE 子集*/, status TEXT, created_ms INTEGER, updated_ms INTEGER)

occurrence(id, task_id, fire_at_ms INTEGER, state TEXT, sent_ms INTEGER)
-- state: pending | sending | sent | late_sent | missed | skipped

channel(id, name TEXT, kind TEXT, enabled INTEGER, priority INTEGER,
        credential_enc BLOB, config_json TEXT)

push_job(id, occurrence_id, channel_id, state TEXT, attempts INTEGER,
         last_error TEXT, next_retry_ms INTEGER,
         dedupe_key TEXT UNIQUE)   -- 幂等基石：occurrence_id + ':' + channel_id

channel_quota(channel_id, day TEXT/*YYYY-MM-DD 本地日期*/, sent INTEGER)
send_log(id, ts_ms, channel_id, occurrence_id, result, http_code, detail)
```

**一致性约束**：所有 `*_ms` 字段一律 epoch 毫秒；`day` 一律 `YYYY-MM-DD`；写库与读库格式必须相同（有测试守住）。

## 5. 触发与“错过”语义（核心决策）

- **正常路径**：tick 扫描 `fire_at_ms <= now AND state='pending'` → 入队 `push_job` → 发送 → `sent`。
- **幂等**：`push_job.dedupe_key` UNIQUE 约束，重复入队直接忽略，杜绝重复轰炸。
- **休眠唤醒**（进程未退）：tick 发现 `now - last_tick > 90s` → 判定为唤醒跳变：
  - `fire_at_ms` 已过且距今 ≤ 30 分钟（可配）→ 补发，标 `late_sent`
  - 超过 30 分钟 → 标 `missed`，只写日志、不推送
- **冷启动 / 关机错过**（按用户要求）：启动时把所有 `fire_at_ms < now` 的 `pending` 一律标 `missed`，**不补发**，`send_log` 留痕。
- **手动**：UI 上可对 `missed` 记录点“补发一次”，走同一队列。

## 6. 渠道能力表（2026-02 实测/官方文档）

| 渠道 | 落点 | 频率上限 | 日额度 | 单条上限 | 内容显示 | 关键前提 |
|---|---|---|---|---|---|---|
| 飞书 自建应用 | 单聊（open_id / email）或群 chat_id | 1000 次/分、50 次/秒 | 无 | 20KB | 完整 | 需 `im:message` 或 `im:message:send_as_bot` 权限；**实测凭据有效，机器人当前不在任何群** |
| 飞书 自定义机器人 | **仅群** | 100 次/分、5 次/秒 | 无 | 20KB | 完整 | 整点/半点易触发 11232 限流；需加签 |
| 钉钉 自定义机器人 | **仅群** | **20 条/分，超限限流 10 分钟** | 无 | 20KB | 完整 | 需加签或关键词；个人单聊通常需企业内部应用 |
| PushPlus | 微信 / 企业微信 / webhook | — | **未实名 0 条**；实名微信 200 次/日 | — | 完整 | 必须实名认证 |
| Server酱 Turbo | 微信 | 50 条/分 | **免费 5 条/日** | — | **免费版仅显示标题，不显示内容** | 免费额度极小 |

**接口地址（可直接复制）**
- 飞书取 token：`https://open.feishu.cn/open-apis/auth/v3/tenant_access_token/internal`
- 飞书发消息：`https://open.feishu.cn/open-apis/im/v1/messages?receive_id_type=email`
- 飞书群列表：`https://open.feishu.cn/open-apis/im/v1/chats`
- 钉钉机器人：`https://oapi.dingtalk.com/robot/send?access_token=<token>`
- PushPlus：`https://www.pushplus.plus/send`
- Server酱：`https://sctapi.ftqq.com/<SendKey>.send`

**⚠ 冲突**：Server酱免费版只显示标题，与“内容不脱敏、完整推送”的期望直接冲突 → 建议降为末位备选，UI 上标注“免费版仅标题”。

## 7. 限流排队 + 降级策略

1. **主渠道优先**：每个 occurrence 按 `channel.priority` 只发给第一个可用渠道；失败或超限才降级到下一个。
2. **令牌桶 + 日计数**：每渠道独立令牌桶（按上表频率），日计数落 `channel_quota`；达到日额度 90% 时 UI 提前告警。
3. **合并摘要**：同渠道 60s 窗口内 ≥2 条待发 → 合并成一条 markdown（省额度）；窗口可配，可关。
4. **重试分级**：
   - 可重试（网络错误 / 5xx / 429）→ 指数退避 5s / 30s / 2min，最多 3 次
   - 不可重试（401 凭证失效 / 400 参数错 / 额度用尽）→ **立即**降级下一渠道，不浪费额度
5. **全渠道失败** → 写 `send_log` + 本地弹窗 + 托盘红点 + 面板告警条。

## 8. 界面

- 主窗口：今日待办 / 月总览 / 发送记录 / 设置
- 迷你浮窗（置顶、快速完成）
- 渠道卡片：启用开关、优先级上下移、凭证输入（掩码）、[测试] 按钮、`今日用量 x/y`、最近错误信息

## 9. 里程碑

- **M1 骨架**：Tauri 2 工程 + SQLite 迁移 + 任务 CRUD + 按天视图 + 托盘常驻
- **M2 调度**：tick 调度器 + 本地弹窗 + 幂等 + 错过语义（含休眠跳变/冷启动用例的单元测试）
- **M3 首个渠道**：飞书自建应用单聊端到端跑通（只做 1 个渠道，验证链路）
- **M4 渠道层**：其余 3 渠道 + 能力表 + 限流队列 + 降级 + 发送记录面板
- **M5 打磨**：迷你浮窗、开机自启、WebView2 检测、密钥加密、安装包

## 10. 验收（可执行的验证清单）

1. 设 2 分钟后的任务 → 到点 ±5s 内飞书收到，标题+正文完整。
2. 同任务发送前后重启进程 → 不重复发送（查 `dedupe_key` UNIQUE）。
3. 关机 10 分钟后开机 → 无补发，`send_log` 有 `missed` 记录。
4. 到点时断网 → 恢复后按退避重试成功，且只收到 1 条。
5. 故意填错飞书凭证 → 401 不重试、自动降级下一渠道，UI 显示“凭证无效”。
6. 60s 内造 5 条任务 → 合并为 1 条（默认配置下）。
7. Server酱 免费额度耗尽 → 自动降级，程序不崩、有明确日志。

## 11. 待确认项

全部已答复，见第 0 节。剩余待实测项：

1. 钉钉/飞书群机器人需用户侧一次性配置（建群拉机器人、加签、关键词）——尚未提供凭证
2. PushPlus 需用户实名认证后才有额度（未实名 0 条）——尚未提供 token
