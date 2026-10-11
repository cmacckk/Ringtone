import { invoke } from "@tauri-apps/api/core";

/**
 * 重复频率。必须与 Rust 侧 `models::REPEAT_FREQS` 对得上：
 * 后端认不出来的频率会**静默当成「不重复」**（宁可少响，也不能被一条脏规则搞崩）。
 */
export type RepeatFreq =
  | "daily"
  | "weekdays"
  | "weekly"
  | "monthly"
  | "last_workday"
  | "yearly";

/**
 * 重复规则，与 Rust 侧 `models::RepeatRule` 一一对应（**存库就是这一份 JSON**）。
 * - `interval`：每隔几个周期，1 = 每个周期
 * - `byday`：1=周一 … 7=周日；**只有 `weekly` 能用**，留空表示继承起始日的星期几
 * - `until`：结束日期，本地日期 `YYYY-MM-DD`，**含当天**；null = 一直重复
 */
export type RepeatRule = {
  freq: RepeatFreq;
  interval: number;
  byday: number[];
  until: string | null;
};

/** 提交给后端的 `repeat_rule`：字符串 JSON，不重复就是 null */
export const repeatToJson = (rule: RepeatRule | null): string | null =>
  rule === null ? null : JSON.stringify(rule);

/** 读库里的 `repeat_rule`；解析不了（旧数据 / 手改过）就当成不重复 */
export const parseRepeat = (raw: string | null): RepeatRule | null => {
  if (!raw) return null;
  try {
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) return null;
    const r = parsed as Partial<RepeatRule>;
    if (typeof r.freq !== "string") return null;
    return {
      freq: r.freq as RepeatFreq,
      interval: typeof r.interval === "number" && r.interval >= 1 ? r.interval : 1,
      byday: Array.isArray(r.byday) ? r.byday.filter((v): v is number => typeof v === "number") : [],
      until: typeof r.until === "string" ? r.until : null,
    };
  } catch {
    return null;
  }
};

/** 与 Rust 侧 `models::Task` 一一对应（字段名必须保持 snake_case） */
export type Task = {
  id: number;
  title: string;
  body: string;
  /**
   * 本地日期 `YYYY-MM-DD`。
   *
   * 对重复任务展开出来的行，这里仍然是**任务自己的起始日**，不是正在看的那一天
   * （界面保存编辑时要拿它回存，改成浏览日会把整个系列挪走）。
   */
  day: string;
  /** epoch 毫秒；null = 只排在那天、不定具体时刻 */
  due_at_ms: number | null;
  /** JSON 数组字符串，如 "[0,-300]" */
  remind_offsets: string;
  repeat_rule: string | null;
  /**
   * 只发到这些渠道类型（JSON 数组，如 `["dingtalk"]`）；`null` = 不限制，发全部启用渠道。
   *
   * 存的是渠道**类型**而不是渠道 id：渠道删了重建、id 会变。
   */
  notify_channels: string | null;
  /**
   * true = 用户标了「重点」。
   *
   * 只管**外观突出**（左侧色条 + 加粗 + 星标），不参与排序、不影响提醒；
   * 编辑任务不会把它清掉（后端 `update` 的 SQL 故意不碰这一列）。
   */
  star: boolean;
  /**
   * true = 重复任务在**非起始日**展开出来的那一条，勾选含义是「只完成这次」。
   * 起始日那天和普通任务都是 false。
   */
  repeated: boolean;
  done: boolean;
  created_ms: number;
  updated_ms: number;
};

export type TaskInput = {
  title: string;
  body: string;
  day: string;
  due_at_ms: number | null;
  repeat_rule: string | null;
  /**
   * 提醒点（JSON 数组字符串，元素是相对 `due_at_ms` 的秒偏移）。
   *
   * `null` 的含义看场景：**新建**时 = 用默认的到点提醒；**更新**时 = 保持库里原值不动。
   * 界面没碰这个下拉时不要编个值凑上去，否则会把用户已有的提前提醒冲掉。
   */
  remind_offsets: string | null;
  /**
   * 提醒发到哪些渠道类型（JSON 数组，如 `["dingtalk"]`）。`null` = 不限制，发全部启用渠道。
   *
   * **和 `remind_offsets` 的 `null` 不是一个意思**：这里是「全部渠道」这个真实取值，
   * 不是「保持原值」——所以编辑时总是把当前选择算好了发上来（后端也是直接覆盖）。
   */
  notify_channels: string | null;
};

export const listTasks = (day: string) => invoke<Task[]>("list_tasks", { day });

export const createTask = (input: TaskInput) => invoke<Task>("create_task", { input });

export const updateTask = (id: number, input: TaskInput) =>
  invoke<Task | null>("update_task", { id, input });

export const setTaskDone = (id: number, done: boolean) =>
  invoke<void>("set_task_done", { id, done });

/**
 * 「完成本次」：只把那一天算完成，重复系列继续。
 *
 * 只对重复任务有意义——普通任务统一走 `setTaskDone`（那是完成整个任务）。
 */
export const setOccurrenceDone = (taskId: number, day: string, done: boolean) =>
  invoke<void>("set_occurrence_done", { taskId, day, done });

/** 标记 / 取消「重点」（只影响外观） */
export const setTaskStar = (id: number, star: boolean) =>
  invoke<void>("set_task_star", { id, star });

/** 停止重复：任务本身留着，只是不再重复（已经勾过的那些天也会保留） */
export const stopRepeat = (id: number) => invoke<void>("stop_repeat", { id });

/** 「提前提醒」能选的档位（秒），`0` = 准点。界面只给这几档；接口/库里可以随便填别的。 */
export const REMIND_PRESETS = [0, 60, 300, 600, 1800, 3600, 86_400];

/** 「提前多少秒」→ 存库的 JSON。`0` 就是只要到点那一下。 */
export function advanceToJson(secs: number): string {
  return secs === 0 ? "[0]" : JSON.stringify([-Math.abs(secs), 0]);
}

/**
 * 库里存的 `remind_offsets` → 「提前多少秒」。
 *
 * 只认「一个提前点（可带不带到点）」这一种形状；其它情况（多个提前点、奇怪的数值）返回 `null`，
 * 界面显示「保持原样」并在保存时原值不动——**宁可不猜**，猜错就把用户设的提醒点弄丢了。
 */
export function parseAdvance(raw: string | null): number | null {
  if (raw === null) return 0;
  let offsets: number[];
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return null;
    offsets = parsed.filter((v): v is number => typeof v === "number");
  } catch {
    return null;
  }
  // 到点之后的偏移（正数）也不认：界面表达不了
  if (offsets.some((s) => s > 0)) return null;
  const early = offsets.filter((s) => s < 0);
  if (early.length === 0) return 0;
  if (early.length > 1) return null;
  const secs = -early[0];
  return REMIND_PRESETS.includes(secs) ? secs : null;
}

export const deleteTasks = (ids: number[]) => invoke<void>("delete_tasks", { ids });

export const todayLocal = () => invoke<string>("today_local");

/** 某天的待办数量（与 Rust 侧 `db::DayCount` 一一对应） */
export type DayCount = {
  day: string;
  total: number;
  /** 未完成条数：周条圆点和「今日 x/y」都用它 */
  undone: number;
};

/** 下一条将要响的提醒（与 Rust 侧 `db::NextReminder` 一一对应） */
export type NextReminder = {
  task_id: number;
  title: string;
  fire_at_ms: number;
  day: string;
};

/**
 * 区间内每天的待办数量，**只返回有任务的那些天**（没任务的日子不在数组里）。
 * 周条一次查 7 天，两端都算在内。
 */
export const countTasksByDay = (fromDay: string, toDay: string) =>
  invoke<DayCount[]>("count_tasks_by_day", { fromDay, toDay });

/** 下一条将要响的提醒；没有任何安排时是 `null` */
export const nextReminder = () => invoke<NextReminder | null>("next_reminder");

/** 右下角的两个数字（与 Rust 侧 `db::TaskStats` 一一对应） */
export type TaskStats = {
  /** 今天的条数（含重复任务展开出来的） */
  today_total: number;
  /** 今天还没完成的条数 */
  today_undone: number;
  /** 全部任务的条数（含已完成的） */
  all_total: number;
};

/** 「今天 N 条 · 全部 M 条」——主窗口右下角和浮窗都用它 */
export const taskStats = () => invoke<TaskStats>("task_stats");

/** 浮窗里点一下：把主窗口叫回来（顺手把浮窗收掉） */
export const showMain = () => invoke<void>("show_main");

/**
 * 主界面留白右键 → 「收进托盘」。
 *
 * 不要用 `getCurrentWindow().hide()` 代替：那条路径**不经过 `sync_float`**，
 * 主窗口藏了浮窗却不顶上（和标题栏 ✕ 的行为不一致）。
 */
export const hideMain = () => invoke<void>("hide_main");

/** 浮窗贴边的目标坐标（由 Rust 按「主窗口所在那块屏」算，**物理像素**） */
export type FloatDock = {
  /** 收起时贴哪条边 */
  side: FloatSide;
  collapsed_x: number;
  expanded_x: number;
  y: number;
  /**
   * 目标屏的边界 + 浮窗在那块屏上的物理尺寸。
   *
   * 拖动吸附时用这几个数（**不要在浮窗里自己调 `currentMonitor()`**）：拖动过程中窗口
   * 可能刚好横跨两块 DPI 不同的屏，那时问到的尺寸会在 268x236 与 402x354 之间跳。
   */
  mon_left: number;
  mon_right: number;
  mon_top: number;
  mon_bottom: number;
  win_w: number;
  win_h: number;
};
export const floatDock = () => invoke<FloatDock>("float_dock");

/** 浮窗贴哪条边（用户把它拖到另一头就换边） */
export type FloatSide = "left" | "right";

/** 换边：Rust 侧记住这条边并重算落点（拖完吸附时调） */
export const floatSetSide = (side: FloatSide) =>
  invoke<FloatDock>("float_set_side", { side });

// ---------- M3：推送渠道 ----------

/**
 * 与 Rust 侧 `db::Channel` 一一对应。
 * **不含密钥内容**：后端只回传 `secret_set`（凭证是否已存），密钥不会到前端。
 */
export type Channel = {
  id: number;
  name: string;
  /** 渠道类型："feishu" | "dingtalk" */
  kind: string;
  enabled: boolean;
  /** 越小越优先；主渠道 = 启用着的、priority 最小的那个 */
  priority: number;
  /** 渠道自己的配置 JSON 字符串（飞书是 app_id / receive_id_type / receive_id） */
  config_json: string;
  secret_set: boolean;
  /** 今天已发几 条（`list_channels` 带出）；null = 后端本次没算 */
  quota_today: number | null;
};

/** 与 Rust 侧 `db::LogEntry` 一一对应 */
export type LogEntry = {
  ts_ms: number;
  /** 渠道名；null = 没走渠道（本地通知兜底） */
  channel: string | null;
  result: string;
  http_code: number | null;
  detail: string | null;
};

/** 飞书渠道的配置形态（后端 `commands::FeishuChannelInput`） */
export type FeishuChannelInput = {
  name?: string | null;
  enabled: boolean;
  priority?: number | null;
  app_id: string;
  /** null / 空串 = 不改动已存的密钥 */
  app_secret?: string | null;
  receive_id_type?: string | null;
  receive_id: string;
};

export const listChannels = () => invoke<Channel[]>("list_channels");

export const saveFeishuChannel = (input: FeishuChannelInput) =>
  invoke<number>("save_feishu_channel", { input });

export const setChannelEnabled = (id: number, enabled: boolean) =>
  invoke<void>("set_channel_enabled", { id, enabled });

export const deleteChannel = (id: number) => invoke<void>("delete_channel", { id });

/** 真的发一条消息出去，返回渠道侧 message_id */
export const testChannel = (id: number) => invoke<string>("test_channel", { id });

/** 只取 token，不发送任何消息 */
export const verifyChannel = (id: number) => invoke<string>("verify_channel", { id });

export const recentLogs = (limit: number) => invoke<LogEntry[]>("recent_logs", { limit });

// ---------- M4：钉钉渠道 + 降级链 ----------

/** 钉钉渠道的配置形态（后端 `commands::DingtalkChannelInput`） */
export type DingtalkChannelInput = {
  name?: string | null;
  enabled: boolean;
  priority?: number | null;
  /** 完整 webhook 地址（也接受裸 access_token） */
  webhook: string;
  /** 加签密钥，格式 SEC...；**没配也行**（那时得靠关键词或 IP 白名单过安全检查） */
  sign_secret?: string | null;
};

export const saveDingtalkChannel = (input: DingtalkChannelInput) =>
  invoke<number>("save_dingtalk_channel", { input });

/** 把某渠道设为主渠道（排到降级链最前），返回它新的 priority */
export const setChannelPrimary = (id: number) => invoke<number>("set_channel_primary", { id });

/** 从上次确认到现在，有多少条提醒最终没发出去（托盘红点用它） */
export const failureCount = () => invoke<number>("failure_count");

/** 确认已看到失败提示：清红点 */
export const acknowledgeFailures = () => invoke<void>("acknowledge_failures");

/** 把渠道配置解析成飞书字段；解析失败返回 null（界面只读展示，不做补救） */
export function parseFeishuConfig(c: Channel): {
  app_id: string;
  receive_id_type: string;
  receive_id: string;
} | null {
  try {
    const raw = JSON.parse(c.config_json) as Record<string, unknown>;
    return {
      app_id: typeof raw.app_id === "string" ? raw.app_id : "",
      receive_id_type:
        typeof raw.receive_id_type === "string" ? raw.receive_id_type : "chat_id",
      receive_id: typeof raw.receive_id === "string" ? raw.receive_id : "",
    };
  } catch {
    return null;
  }
}

/** 把钉钉渠道配置解析出来；**加签密钥永远不会回传**，所以这里只有 webhook */
export function parseDingtalkConfig(c: Channel): { webhook: string } | null {
  try {
    const raw = JSON.parse(c.config_json) as Record<string, unknown>;
    if (typeof raw.webhook === "string") return { webhook: raw.webhook };
    // 兼容只存了 access_token 的情况
    if (typeof raw.access_token === "string") {
      return { webhook: `https://oapi.dingtalk.com/robot/send?access_token=${raw.access_token}` };
    }
    return { webhook: "" };
  } catch {
    return null;
  }
}

/** 渠道类型的中文名（界面用） */
export const KIND_LABEL: Record<string, string> = {
  feishu: "飞书",
  dingtalk: "钉钉",
};

/**
 * 读出任务选定的渠道类型；`null` = 不限制（发全部启用渠道）。
 *
 * 认不出来（空串、脏 JSON、空数组）一律当「不限制」：宁可多发，也不要因为一个坏值
 * 变成「一个渠道都不发」。后端 `models::parse_notify_kinds` 是同一套语义。
 */
export function parseNotifyKinds(raw: string | null): string[] | null {
  if (raw === null || raw.trim() === "") return null;
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return null;
    const kinds = parsed
      .filter((k): k is string => typeof k === "string")
      .map((k) => k.trim())
      .filter((k) => k !== "");
    return kinds.length === 0 ? null : [...new Set(kinds)];
  } catch {
    return null;
  }
}

/** 渠道选择 → 请求体里的字符串；`null`（= 全部）原样传 `null` */
export function notifyKindsToJson(kinds: string[] | null): string | null {
  if (kinds === null || kinds.length === 0) return null;
  return JSON.stringify([...kinds].sort());
}

// ---------------------------------------------------------------- 通用设置（v5）

/** 上下班时间设置（与 Rust 侧 `work::WorkSchedule` 一一对应） */
export type WorkSchedule = {
  enabled: boolean;
  /** 全部是补零的 `HH:MM` */
  morning_in: string;
  morning_out: string;
  afternoon_in: string;
  afternoon_out: string;
  /** 提前几分钟提醒（0 = 准点） */
  advance_min: number;
};

/** 「距离下班还有多久」需要的状态（与 Rust 侧 `work::WorkStatus` 一一对应） */
export type WorkStatus = {
  enabled: boolean;
  /** `before` / `morning` / `noon` / `afternoon` / `off` */
  phase: string;
  /** 下一个时间点的短名字，如「下班」 */
  next_label: string;
  next_ms: number | null;
  /** 今天下午下班那一刻 */
  off_ms: number | null;
  /** 距离下一个时间点还有多少毫秒 */
  left_ms: number;
};

export const workGet = () => invoke<WorkSchedule>("work_get");
export const workStatus = () => invoke<WorkStatus>("work_status");
/** 保存设置，返回新的状态（省一次往返） */
export const workSave = (schedule: WorkSchedule) =>
  invoke<WorkStatus>("work_save", { schedule });

/** 当前「一键显示浮窗」的快捷键（空字符串 = 没设） */
export const getFloatHotkey = () => invoke<string>("get_float_hotkey");
/** 设置快捷键：注册成功才写入；失败会把原来的装回去 */
export const setFloatHotkey = (accel: string) => invoke<string>("set_float_hotkey", { accel });
/** 立刻显示浮窗（设置面板里的「试一下」按钮） */
export const floatShow = () => invoke<void>("float_show");

/** 浮窗现在是显示的吗（标题栏按钮用它画开关状态） */
export const floatVisible = () => invoke<boolean>("float_visible");

/** 标题栏按钮：显示 / 隐藏浮窗。返回切换之后的可见状态 */
export const toggleFloat = () => invoke<boolean>("toggle_float");

/**
 * 鼠标侧键：`off` = 不用，`x1` = 常见的「后退」键，`x2` = 「前进」键。
 *
 * 设了之后那个侧键就**全局**用来叫浮窗（钩子会把按键吞掉，不再触发浏览器的前进/后退）。
 */
export type MouseButton = "off" | "x1" | "x2";
export const getMouseButton = () => invoke<string>("get_mouse_button");
export const setMouseButton = (button: MouseButton) =>
  invoke<string>("set_mouse_button", { button });

/** 背景图（`main` / `float`），返回值是可直接塞进 style 的 data URL */
export type BackgroundTarget = "main" | "float";
export const backgroundGet = (which: BackgroundTarget) =>
  invoke<string | null>("background_get", { which });
/** 选图（弹系统文件框）；返回 `null` = 用户取消 */
export const backgroundPick = (which: BackgroundTarget) =>
  invoke<string | null>("background_pick", { which });
export const backgroundClear = (which: BackgroundTarget) =>
  invoke<void>("background_clear", { which });

/** 导出结果 */
export type ExportResult = {
  path: string;
  tasks: number;
  bytes: number;
};
export const exportTasks = (kind: "md" | "csv") =>
  invoke<ExportResult | null>("export_tasks", { kind });

/** 导入结果 */
export type ImportSummary = {
  added: number;
  skipped: number;
  failed: string[];
};
export const importTasks = () => invoke<ImportSummary | null>("import_tasks");

/** 内置的节假日数据覆盖到哪几年（`covered` 形如 `2026`，`this_year` = 今年有没有数据） */
export type HolidayInfo = {
  covered: string;
  this_year: boolean;
};
/** 没有某一年的数据时，「工作日」会退化成「周一至周五」——界面要用这个提醒用户去更新数据 */
export const holidayInfo = () => invoke<HolidayInfo>("holiday_info");
