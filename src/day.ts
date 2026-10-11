/**
 * 日期/时间格式化的唯一出处。
 *
 * 两条约定，都是为了不踩时区的坑：
 * 1. 字符串一律 `YYYY-MM-DD`（补零到 10 位），和 Rust 侧 `tasks.day` 同一格式；
 * 2. **不用 `toISOString()`**：它按 UTC 切日期，北京时间晚上 8 点之后会算出「明天」。
 */

import type { RepeatRule } from "./api";

export const WEEKDAYS = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"];

export function pad(n: number): string {
  return String(n).padStart(2, "0");
}

export function splitDay(day: string): [number, number, number] {
  const parts = day.split("-").map((v) => Number(v));
  return [parts[0] ?? 1970, parts[1] ?? 1, parts[2] ?? 1];
}

export function toDay(dt: Date): string {
  return `${dt.getFullYear()}-${pad(dt.getMonth() + 1)}-${pad(dt.getDate())}`;
}

export function shiftDay(day: string, delta: number): string {
  const [y, m, d] = splitDay(day);
  return toDay(new Date(y, m - 1, d + delta));
}

export function weekdayOf(day: string): string {
  const [y, m, d] = splitDay(day);
  return WEEKDAYS[new Date(y, m - 1, d).getDay()] ?? "";
}

/** `08-20`：周条格子里那行小字 */
export function shortDate(day: string): string {
  const [, m, d] = splitDay(day);
  return `${pad(m)}-${pad(d)}`;
}

/**
 * 一周的第一天。**周一是第一天**（和 kite 一致）：
 * 周日要往前退 6 天，不能按 `getDay()` 直接减。
 */
export function startOfWeek(day: string): string {
  const [y, m, d] = splitDay(day);
  const back = (new Date(y, m - 1, d).getDay() + 6) % 7;
  return shiftDay(day, -back);
}

export function weekDays(anchor: string): string[] {
  const start = startOfWeek(anchor);
  return Array.from({ length: 7 }, (_, i) => shiftDay(start, i));
}

/* ---------- 月总览用（月份网格）---------- */

/** 这个月的第一天（`YYYY-MM-01`）。锚点可以是月中任意一天。 */
export function monthStart(day: string): string {
  const [y, m] = splitDay(day);
  return `${y}-${pad(m)}-01`;
}

/**
 * 往前 / 往后挪一个月，返回那个月的 1 号。
 * 基准固定用 1 号：拿 31 号加一个月会溢出到下下个月（JS 的 `Date` 是滚动式进位）。
 */
export function shiftMonth(day: string, delta: number): string {
  const [y, m] = splitDay(day);
  return toDay(new Date(y, m - 1 + delta, 1));
}

/** 这个月的最后一天（已处理 28/29/30/31）。 */
export function monthEnd(day: string): string {
  return shiftDay(shiftMonth(day, 1), -1);
}

/** `2026 年 10 月`：总览面板的标题 */
export function monthLabel(day: string): string {
  const [y, m] = splitDay(day);
  return `${y} 年 ${m} 月`;
}

/**
 * 月视图的日历网格：从「这个月 1 号所在那一周的周一」开始，固定 6 行 × 7 列 = 42 格。
 * 固定 42 格是为了翻月时格子不跳高度（4~6 行的月份来回跳看着很畺）。
 */
export function monthGrid(day: string): string[] {
  const start = startOfWeek(monthStart(day));
  return Array.from({ length: 42 }, (_, i) => shiftDay(start, i));
}

export function hmFromMs(ms: number | null): string {
  if (ms === null) return "";
  const dt = new Date(ms);
  return `${pad(dt.getHours())}:${pad(dt.getMinutes())}`;
}

export function msFromHm(day: string, hm: string): number | null {
  if (!hm) return null;
  const [y, m, d] = splitDay(day);
  const [hh, mm] = hm.split(":").map((v) => Number(v));
  return new Date(y, m - 1, d, hh ?? 0, mm ?? 0, 0, 0).getTime();
}

/**
 * 秒 → 「1 天」/「2 小时」/「5 分钟」/「90 秒」。
 *
 * **参数是秒不是毫秒**：库里所有 `*_ms` 都是毫秒，传进来之前先 `/ 1000`。
 * 曾经把毫秒直接丢进来，页面上出现过「距离下班还有 64667 分 37 秒」。
 */
export function humanSpan(seconds: number): string {
  if (seconds >= 86_400 && seconds % 86_400 === 0) return `${seconds / 86_400} 天`;
  if (seconds >= 3_600 && seconds % 3_600 === 0) return `${seconds / 3_600} 小时`;
  if (seconds >= 60 && seconds % 60 === 0) return `${seconds / 60} 分钟`;
  if (seconds >= 60) return `${Math.floor(seconds / 60)} 分 ${seconds % 60} 秒`;
  return `${seconds} 秒`;
}

/**
 * 触发时刻 → 顶部那行大字。
 *
 * 用「日历天」判断明天/后天，而不是「差值 > 24 小时」：
 * 今天 23:00 看明天 01:00 只有 2 小时，说「2 小时后」没错，
 * 但今天 09:00 看明天 08:00（23 小时）说「明天 08:00」比「23 小时后」更好认。
 */
export function fireAtLabel(ms: number, nowMs: number): string {
  const fire = new Date(ms);
  const now = new Date(nowMs);
  const fireDay = toDay(fire);
  const today = toDay(now);
  const hm = `${pad(fire.getHours())}:${pad(fire.getMinutes())}`;
  const diff = ms - nowMs;

  if (fireDay === today) {
    if (diff <= 0) return "就是现在";
    if (diff < 60_000) return "不到 1 分钟";
    const mins = Math.round(diff / 60_000);
    if (mins < 60) return `${mins} 分钟后`;
    const h = Math.floor(diff / 3_600_000);
    const m = Math.round((diff % 3_600_000) / 60_000);
    return m === 0 ? `${h} 小时后` : `${h} 小时 ${m} 分后`;
  }
  if (fireDay === shiftDay(today, 1)) return `明天 ${hm}`;
  return `${shortDate(fireDay)} ${hm}`;
}

/**
 * `remind_offsets`（JSON 数组字符串，单位秒）→ 「提前 5 分钟」。
 * 没有提前提醒（只有 `0`）时返回空串，界面上不显示这行小字。
 */
export function advanceLabel(offsetsRaw: string): string {
  let offsets: number[] = [];
  try {
    const parsed: unknown = JSON.parse(offsetsRaw);
    if (Array.isArray(parsed)) offsets = parsed.filter((v): v is number => typeof v === "number");
  } catch {
    return "";
  }
  const early = offsets.filter((s) => s < 0).map((s) => humanSpan(-s));
  return early.length === 0 ? "" : `提前 ${early.join("、")}`;
}

/** 「提前多少秒」→ 下拉里的文案；`0` = 准点。 */
export function advanceOptionLabel(secs: number): string {
  return secs === 0 ? "准点" : `提前 ${humanSpan(secs)}`;
}

/** 周一到周日：星期选择按钮和「每周一、三、五」都用它 */
export const WEEKDAY_SHORT = ["一", "二", "三", "四", "五", "六", "日"];

/** 1=周一 … 7=周日（和后端 `RepeatRule.byday` 同一套编号） */
export function weekdayNumber(day: string): number {
  const [y, m, d] = splitDay(day);
  const js = new Date(y, m - 1, d).getDay(); // 0=周日
  return js === 0 ? 7 : js;
}

/**
 * 重复规则 → 一行中文（「每周一、三、五」「每 2 个月 8 日，到 2026-12-31」）。
 *
 * `startDay` 是**任务自己的起始日**：`monthly` / `yearly` 规则里只存频率，不存「几号」——
 * 只有跟起始日放一起才说得清到底是每月的哪一天。
 */
export function describeRepeat(rule: RepeatRule | null, startDay: string): string {
  if (!rule) return "";
  const [, m, d] = splitDay(startDay);
  const n = rule.interval > 1 ? rule.interval : 0;
  const until = rule.until ? `，到 ${rule.until}` : "";
  let base: string;
  switch (rule.freq) {
    case "daily":
      base = n ? `每 ${n} 天` : "每天";
      break;
    case "weekdays":
      base = "工作日";
      break;
    case "weekly": {
      const list =
        rule.byday.length > 0
          ? rule.byday
              .map((v) => WEEKDAY_SHORT[v - 1] ?? "")
              .filter(Boolean)
              .join("、")
          : WEEKDAY_SHORT[weekdayNumber(startDay) - 1] ?? "";
      base = n ? `每 ${n} 周的周${list}` : `每周${list}`;
      break;
    }
    case "monthly":
      base = n ? `每 ${n} 个月 ${d} 日` : `每月 ${d} 日`;
      break;
    case "last_workday":
      // 「几号」不重要：这个频率看的是月末，而且会跳过周末与法定假日
      base = n ? `每 ${n} 个月的最后一个工作日` : "每月最后一个工作日";
      break;
    case "yearly":
      base = n ? `每 ${n} 年 ${m} 月 ${d} 日` : `每年 ${m} 月 ${d} 日`;
      break;
    default:
      // 认不出来的频率（旧数据 / 手改过）宁可不解释，也不编一个看起来像真的文案
      return "";
  }
  return base + until;
}
