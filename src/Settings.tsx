import { useCallback, useEffect, useState } from "react";
import Prefs from "./Prefs";
import {
  Channel,
  LogEntry,
  acknowledgeFailures,
  deleteChannel,
  failureCount,
  listChannels,
  parseDingtalkConfig,
  parseFeishuConfig,
  parseWxpusherConfig,
  recentLogs,
  saveDingtalkChannel,
  saveFeishuChannel,
  saveWxpusherChannel,
  setChannelEnabled,
  setChannelPrimary,
  testChannel,
  verifyChannel,
} from "./api";

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function stamp(ms: number): string {
  const d = new Date(ms);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getMonth() + 1}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

const RESULT_LABEL: Record<string, string> = {
  sent: "已发送",
  late_sent: "补发成功",
  failed: "发送失败",
  missed: "错过（不补发）",
  skipped: "已跳过",
  test_sent: "测试成功",
  test_failed: "测试失败",
};

/** 接收方类型：跟飞书 `receive_id_type` 对齐 */
const ID_TYPES: Array<{ value: string; label: string }> = [
  { value: "chat_id", label: "群（chat_id）" },
  { value: "email", label: "用户邮箱（email）" },
  { value: "open_id", label: "用户 open_id" },
  { value: "user_id", label: "用户 user_id" },
];

/** 一个渠道卡片下方那一排通用按钮（验证 / 测试 / 主渠道 / 启停 / 删除） */
function ChannelActions({
  channel,
  busy,
  run,
  onPrimary,
}: {
  channel: Channel;
  busy: boolean;
  run: (fn: () => Promise<unknown>) => void;
  onPrimary: boolean;
}) {
  return (
    <div className="row">
      <button
        type="button"
        disabled={busy}
        onClick={() => void run(() => verifyChannel(channel.id))}
      >
        {channel.kind === "dingtalk" ? "检查配置" : "验证凭据（不发消息）"}
      </button>
      <button type="button" disabled={busy} onClick={() => void run(() => testChannel(channel.id))}>
        发测试消息
      </button>
      <span className="spacer" />
      {onPrimary ? (
        <span className="muted">主渠道（优先发这个）</span>
      ) : (
        <button
          type="button"
          disabled={busy}
          onClick={() => void run(() => setChannelPrimary(channel.id))}
        >
          设为主渠道
        </button>
      )}
      <button
        type="button"
        disabled={busy}
        onClick={() => void run(() => setChannelEnabled(channel.id, !channel.enabled))}
      >
        {channel.enabled ? "停用" : "启用"}
      </button>
      <button
        type="button"
        className="ghost"
        disabled={busy}
        onClick={() => {
          if (!window.confirm(`删除渠道「${channel.name}」？已发出的记录会保留。`)) return;
          void run(() => deleteChannel(channel.id));
        }}
      >
        删除
      </button>
    </div>
  );
}

export default function Settings({
  onClose,
  onChanged,
}: {
  onClose: () => void;
  /** 通用设置里改了会影响到列表的东西（导入任务、上下班隐藏任务）时，让外面重新读一遍 */
  onChanged?: () => void;
}) {
  const [tab, setTab] = useState<"push" | "prefs">("push");
  const [channels, setChannels] = useState<Channel[]>([]);
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [failures, setFailures] = useState(0);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState("");
  const [err, setErr] = useState("");

  // 飞书表单
  const [appId, setAppId] = useState("");
  const [secret, setSecret] = useState("");
  const [idType, setIdType] = useState("chat_id");
  const [receiveId, setReceiveId] = useState("");
  // 钉钉表单
  const [webhook, setWebhook] = useState("");
  const [signSecret, setSignSecret] = useState("");
  // WxPusher（微信推送）表单
  const [appToken, setAppToken] = useState("");
  const [uids, setUids] = useState("");

  const feishu = channels.find((c) => c.kind === "feishu") ?? null;
  const dingtalk = channels.find((c) => c.kind === "dingtalk") ?? null;
  const wxpusher = channels.find((c) => c.kind === "wxpusher") ?? null;
  // 主渠道 = 启用中的、priority 最小的那个（与后端降级链同一条规则）
  const primaryId =
    channels.filter((c) => c.enabled).sort((a, b) => a.priority - b.priority)[0]?.id ?? null;

  // 已经回填过的配置快照：避免每次「验证/测试」后刷新时把用户正在改的内容冲掉
  const [applied, setApplied] = useState<{ feishu: string; dingtalk: string; wxpusher: string }>({
    feishu: "",
    dingtalk: "",
    wxpusher: "",
  });

  const refresh = useCallback(async () => {
    try {
      setChannels(await listChannels());
      setLogs(await recentLogs(50));
      setFailures(await failureCount());
      setErr("");
    } catch (e: unknown) {
      setErr(message(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // 已有渠道时回填表单；**只在库里的配置真的变了之后**回填（保存后、或外部改动后）。
  // 密钥不会回填：后端从不把密钥回传前端。
  const feishuConfig = feishu?.config_json ?? "";
  const dingtalkConfig = dingtalk?.config_json ?? "";
  const wxpusherConfig = wxpusher?.config_json ?? "";
  useEffect(() => {
    if (!feishu || feishuConfig === applied.feishu) return;
    const cfg = parseFeishuConfig(feishu);
    if (!cfg) return;
    setAppId(cfg.app_id);
    setIdType(cfg.receive_id_type);
    setReceiveId(cfg.receive_id);
    setApplied((prev) => ({ ...prev, feishu: feishuConfig }));
  }, [feishu, feishuConfig, applied.feishu]);

  useEffect(() => {
    if (!dingtalk || dingtalkConfig === applied.dingtalk) return;
    const cfg = parseDingtalkConfig(dingtalk);
    if (!cfg) return;
    setWebhook(cfg.webhook);
    setApplied((prev) => ({ ...prev, dingtalk: dingtalkConfig }));
  }, [dingtalk, dingtalkConfig, applied.dingtalk]);

  useEffect(() => {
    if (!wxpusher || wxpusherConfig === applied.wxpusher) return;
    const cfg = parseWxpusherConfig(wxpusher);
    if (!cfg) return;
    setUids(cfg.uids);
    setApplied((prev) => ({ ...prev, wxpusher: wxpusherConfig }));
  }, [wxpusher, wxpusherConfig, applied.wxpusher]);

  async function run(fn: () => Promise<unknown>): Promise<void> {
    setBusy(true);
    setMsg("");
    setErr("");
    try {
      const r = await fn();
      setMsg(typeof r === "string" ? r : "已完成");
      await refresh();
    } catch (e: unknown) {
      setErr(message(e));
      await refresh();
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="panel">
      <div className="panel-head">
        <strong>设置</strong>
        <span className="spacer" />
        <button type="button" className="ghost" onClick={onClose}>
          返回待办
        </button>
      </div>

      <div className="tabs">
        <button
          type="button"
          className={tab === "push" ? "on" : ""}
          onClick={() => setTab("push")}
        >
          推送渠道
        </button>
        <button
          type="button"
          className={tab === "prefs" ? "on" : ""}
          onClick={() => setTab("prefs")}
        >
          通用
        </button>
      </div>

      {tab === "prefs" ? (
        <Prefs onChanged={onChanged} />
      ) : (
        <>
      <div className="hint">
        到点后会发到下面的渠道；没配渠道、或渠道发送失败时，会自动用本机系统通知兜底
        （发不出去的记录都会留在下面的「发送记录」里）。主渠道发不出去时，会自动降级到下一个启用的渠道。
      </div>

      {failures > 0 ? (
        <div className="error">
          有 {failures} 条提醒所有渠道都没发出去（已用本机通知兜底）。
          <button
            type="button"
            className="ghost"
            disabled={busy}
            onClick={() => void run(() => acknowledgeFailures())}
          >
            知道了
          </button>
        </div>
      ) : null}

      <div className="card">
        <div className="card-head">
          <strong>钉钉</strong>
          {dingtalk ? (
            <span className="muted">
              {dingtalk.enabled ? "已启用" : "已停用"} ·{" "}
              {dingtalk.secret_set ? "已配加签" : "未配加签"} · 今日已发{" "}
              {dingtalk.quota_today ?? 0} 条
            </span>
          ) : (
            <span className="muted">还没配置</span>
          )}
        </div>

        <div className="form">
          <label className="wide">
            <span>Webhook 地址</span>
            <input
              type="text"
              value={webhook}
              placeholder="https://oapi.dingtalk.com/robot/send?access_token=xxxx"
              onChange={(e) => setWebhook(e.target.value)}
            />
          </label>
          <label className="wide">
            <span>加签密钥</span>
            <input
              type="password"
              value={signSecret}
              placeholder={dingtalk?.secret_set ? "留空 = 不改动已存的密钥" : "SEC 开头；不填则靠关键词过安全校验"}
              onChange={(e) => setSignSecret(e.target.value)}
            />
          </label>
        </div>

        <div className="row">
          <button
            type="button"
            className="primary"
            disabled={busy || !webhook.trim()}
            onClick={() =>
              void run(async () => {
                await saveDingtalkChannel({
                  name: "钉钉",
                  enabled: true,
                  priority: dingtalk?.priority ?? 100,
                  webhook: webhook.trim(),
                  // 空串 = 不改动已存的密钥
                  sign_secret: signSecret.trim() === "" ? null : signSecret.trim(),
                });
                setSignSecret("");
                return "已保存并启用";
              })
            }
          >
            保存并启用
          </button>
          {dingtalk ? (
            <ChannelActions
              channel={dingtalk}
              busy={busy}
              run={run}
              onPrimary={primaryId === dingtalk.id}
            />
          ) : null}
        </div>
      </div>

      <div className="card">
        <div className="card-head">
          <strong>飞书</strong>
          {feishu ? (
            <span className="muted">
              {feishu.enabled ? "已启用" : "已停用"} ·{" "}
              {feishu.secret_set ? "密钥已存" : "密钥未填"} · 今日已发{" "}
              {feishu.quota_today ?? 0} 条
            </span>
          ) : (
            <span className="muted">还没配置</span>
          )}
        </div>

        <div className="form">
          <label>
            <span>App ID</span>
            <input
              type="text"
              value={appId}
              placeholder="cli_xxxxxxxxxxxx"
              onChange={(e) => setAppId(e.target.value)}
            />
          </label>
          <label>
            <span>App Secret</span>
            <input
              type="password"
              value={secret}
              placeholder={feishu?.secret_set ? "留空 = 不改动已存的密钥" : "第一次配置必填"}
              onChange={(e) => setSecret(e.target.value)}
            />
          </label>
          <label>
            <span>接收方类型</span>
            <select value={idType} onChange={(e) => setIdType(e.target.value)}>
              {ID_TYPES.map((t) => (
                <option key={t.value} value={t.value}>
                  {t.label}
                </option>
              ))}
            </select>
          </label>
          <label>
            <span>接收方</span>
            <input
              type="text"
              value={receiveId}
              placeholder="oc_xxxxxxxxxxxx（群的 chat_id）"
              onChange={(e) => setReceiveId(e.target.value)}
            />
          </label>
        </div>

        <div className="row">
          <button
            type="button"
            className="primary"
            disabled={busy || !appId.trim() || !receiveId.trim()}
            onClick={() =>
              void run(async () => {
                await saveFeishuChannel({
                  name: "飞书",
                  enabled: true,
                  priority: feishu?.priority ?? 100,
                  app_id: appId.trim(),
                  // 空串 = 不改动已存的密钥（第一次配置必须填，后端会拦）
                  app_secret: secret.trim() === "" ? null : secret.trim(),
                  receive_id_type: idType,
                  receive_id: receiveId.trim(),
                });
                setSecret("");
                return "已保存并启用";
              })
            }
          >
            保存并启用
          </button>
          {feishu ? (
            <ChannelActions
              channel={feishu}
              busy={busy}
              run={run}
              onPrimary={primaryId === feishu.id}
            />
          ) : null}
        </div>
      </div>

      <div className="card">
        <div className="card-head">
          <strong>微信推送（WxPusher）</strong>
          {wxpusher ? (
            <span className="muted">
              {wxpusher.enabled ? "已启用" : "已停用"} ·{" "}
              {wxpusher.secret_set ? "appToken 已存" : "appToken 未填"} · 今日已发{" "}
              {wxpusher.quota_today ?? 0} 条
            </span>
          ) : (
            <span className="muted">还没配置</span>
          )}
        </div>

        <div className="form">
          <label className="wide">
            <span>UID（多个用逗号或空格隔开）</span>
            <input
              type="text"
              value={uids}
              placeholder="UID_xxxxxxxxxxxxxxxxx"
              onChange={(e) => setUids(e.target.value)}
            />
          </label>
          <label className="wide">
            <span>appToken</span>
            <input
              type="password"
              value={appToken}
              placeholder={
                wxpusher?.secret_set ? "留空 = 不改动已存的 appToken" : "AT_ 开头，第一次配置必填"
              }
              onChange={(e) => setAppToken(e.target.value)}
            />
          </label>
        </div>

        <div className="row">
          <button
            type="button"
            className="primary"
            disabled={busy || !uids.trim()}
            onClick={() =>
              void run(async () => {
                await saveWxpusherChannel({
                  name: "微信推送",
                  enabled: true,
                  priority: wxpusher?.priority ?? 100,
                  uids: uids.trim(),
                  // 空串 = 不改动已存的 appToken（第一次配置必须填，后端会拦）
                  app_token: appToken.trim() === "" ? null : appToken.trim(),
                });
                setAppToken("");
                return "已保存并启用";
              })
            }
          >
            保存并启用
          </button>
          {wxpusher ? (
            <ChannelActions
              channel={wxpusher}
              busy={busy}
              run={run}
              onPrimary={primaryId === wxpusher.id}
            />
          ) : null}
        </div>

        <div className="hint">
          先去 wxpusher.zjiecode.com 后台建应用拿到 appToken（AT_ 开头），再让接收人在 app / 公众号里关注该应用拿到 UID（UID_ 开头）。官方限制：接口 QPS ≤ 1、单个 UID 每天 2000 条，这里按 10 条/分 发。
        </div>
      </div>

      {err ? <div className="error">{err}</div> : null}
      {msg ? <div className="ok">{msg}</div> : null}

      <div className="card">
        <div className="card-head">
          <strong>发送记录</strong>
          <span className="spacer" />
          <button type="button" className="ghost" disabled={busy} onClick={() => void refresh()}>
            刷新
          </button>
        </div>
        <div className="logs">
          {logs.length === 0 ? (
            <div className="empty">还没有记录</div>
          ) : (
            logs.map((l, i) => (
              <div className="log" key={`${l.ts_ms}-${i}`}>
                <span className="when">{stamp(l.ts_ms)}</span>
                <span className={`tag ${l.result}`}>{RESULT_LABEL[l.result] ?? l.result}</span>
                <span className="chan">{l.channel ?? "本机通知"}</span>
                <span className="http">{l.http_code ? `HTTP ${l.http_code}` : ""}</span>
                <span className="detail" title={l.detail ?? ""}>
                  {l.detail ?? ""}
                </span>
              </div>
            ))
          )}
        </div>
      </div>
        </>
      )}
    </div>
  );
}
