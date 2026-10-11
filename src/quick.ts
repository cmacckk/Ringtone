/**
 * 快捷录入：从「明天九点交周报」这种一句话里认出日期、时间、重复规则。
 *
 * 设计原则：
 * - **只认确定的说法**，认不出就什么都不做（宁可当成普通标题，也不要猜错时间）；
 * - 认出来的片段会从标题里摘掉，界面**先把结果给用户看**（识别到什么、标题剩什么），再让他回车；
 * - 纯函数、不碰 React、时间也由参数传进来（`today` / `nowMs`），方便单独验证。
 */
import type { RepeatRule } from "./api";
import { describeRepeat, shortDate, shiftDay, weekdayNumber } from "./day";

export type QuickParse = {
  /** 摘掉日期/时间/重复说法之后剩下的标题；摘没了就退回原文 */
  title: string;
  /** 识别到的日期 `YYYY-MM-DD`；`null` = 没说日期（用当前正在看的那天） */
  day: string | null;
  /** 识别到的时刻 `HH:MM`；`null` = 没说时刻 */
  hm: string | null;
  /** 识别到的重复规则；`null` = 没说重复 */
  repeat: RepeatRule | null;
  /** 「明天 09:00」这种给人看的确认文案；什么都没识别到就是空串 */
  label: string;
  /** 拼出来的时刻已经过去了（比如下午三点说「九点」）→ 后端会算成「错过」，不会真的提醒 */
  past: boolean;
};

const CN_DIGIT: Record<string, number> = {
  零: 0,
  一: 1,
  二: 2,
  两: 2,
  三: 3,
  四: 4,
  五: 5,
  六: 6,
  七: 7,
  八: 8,
  九: 9,
};

/** 「九」「十一」「二十三」「10」→ 数字；认不出返回 null */
function cnNumber(raw: string): number | null {
  const s = raw.trim();
  if (/^\d+$/.test(s)) return Number(s);
  if (s.length === 1 && CN_DIGIT[s] !== undefined) return CN_DIGIT[s];
  const m = /^([一二两三四五六七八九]?)十([一二三四五六七八九])?$/.exec(s);
  if (m) {
    const tens = m[1] ? CN_DIGIT[m[1]] : 1;
    const ones = m[2] ? CN_DIGIT[m[2]] : 0;
    return tens * 10 + ones;
  }
  return null;
}

const DAY_WORD: Record<string, number> = { 今天: 0, 明天: 1, 后天: 2, 大后天: 3 };
const WEEKDAY_WORD: Record<string, number> = {
  一: 1,
  二: 2,
  三: 3,
  四: 4,
  五: 5,
  六: 6,
  日: 7,
  天: 7,
};

const pad2 = (n: number) => String(n).padStart(2, "0");

/** 给人看的日期：今天 / 明天 / 同年报 `10-20`；跨年必须带上年（只报 `03-05` 看不出是哪年） */
function dayText(day: string, today: string): string {
  if (day === today) return "今天";
  if (day === shiftDay(today, 1)) return "明天";
  return day.slice(0, 4) === today.slice(0, 4) ? shortDate(day) : day;
}

/** 用字面年/月/日拼 `YYYY-MM-DD`；`Date` 会自己处理进位（32 日 → 下个月 1 日）*/
function mkDay(y: number, m: number, d: number): string {
  const dt = new Date(y, m - 1, d);
  return `${dt.getFullYear()}-${pad2(dt.getMonth() + 1)}-${pad2(dt.getDate())}`;
}

/**
 * 认日期。返回 `[YYYY-MM-DD, 命中的原文]`；没认出返回 null。
 *
 * 支持的写法：今天 / 明天 / 后天 / 大后天、N 天后、周三（可带「本|这|下|下下」前缀，也能写星期/礼拜）、
 * `3月5日`、`3月5号`、`15号`、`10/20`（只认斜杠）。
 */
function parseDay(text: string, today: string): [string, string] | null {
  const word = /(大后天|今天|明天|后天)/.exec(text);
  if (word) {
    const got = shiftDay(today, DAY_WORD[word[1]]);
    if (got) return [got, word[0]];
  }
  const after = /(\d+|[一二两三四五六七八九十]{1,3})\s*天后/.exec(text);
  if (after) {
    const n = cnNumber(after[1]);
    const got = n === null ? null : shiftDay(today, n);
    if (got) return [got, after[0]];
  }
  const week = /(本|这|下下|下)?(?:周|星期|礼拜)([一二三四五六日天])/.exec(text);
  if (week) {
    const target = WEEKDAY_WORD[week[2]];
    const base = weekdayNumber(today);
    let delta = target - base;
    if (delta < 0) delta += 7; // 已经过去了就说的是下周的那个
    // 「下周三」= 再往后一周；「下下周三」再加一周
    if (week[1] === "下") delta += 7;
    else if (week[1] === "下下") delta += 14;
    const got = shiftDay(today, delta);
    if (got) return [got, week[0]];
  }
  const monthDay = /(\d{1,2})\s*月\s*(\d{1,2})\s*[日号]/.exec(text);
  if (monthDay) {
    const y = Number(today.slice(0, 4));
    const m = Number(monthDay[1]);
    const d = Number(monthDay[2]);
    if (m >= 1 && m <= 12 && d >= 1 && d <= 31) {
      let got = mkDay(y, m, d);
      if (got < today) got = mkDay(y + 1, m, d); // 今年的已经过了 → 明年
      return [got, monthDay[0]];
    }
  }
  const dayOnly = /(\d{1,2})\s*[日号]/.exec(text);
  if (dayOnly) {
    const y = Number(today.slice(0, 4));
    const m = Number(today.slice(5, 7));
    const d = Number(dayOnly[1]);
    if (d >= 1 && d <= 31) {
      let got = mkDay(y, m, d);
      if (got < today) got = mkDay(y, m + 1, d); // 这个月的已经过了 → 下个月
      return [got, dayOnly[0]];
    }
  }
  // `10/20`：**只认斜杠**。用 `.` 会把「3.5」这种版本号读成 3 月 5 日，
  // 用 `-` 会把时间区间读成日期——猜错了比不认更糟。
  const slash = /(?:^|[^\d/])(\d{1,2})\/(\d{1,2})(?![\d/])/.exec(text);
  if (slash) {
    const y = Number(today.slice(0, 4));
    const m = Number(slash[1]);
    const d = Number(slash[2]);
    if (m >= 1 && m <= 12 && d >= 1 && d <= 31) {
      let got = mkDay(y, m, d);
      if (got < today) got = mkDay(y + 1, m, d); // 今年的已经过了 → 明年
      return [got, `${m}/${d}`];
    }
  }
  return null;
}

/**
 * 认时刻。返回 `[HH:MM, 命中的原文]`；没认出返回 null。
 *
 * 支持的写法：`9点` / `九点` / `9点半` / `9点20分` / `9:20` / `下午3点` / `晚上八点` / `中午12点`。
 * 只说「下午」不给数字不算（信息不够，猜不出来）。
 */
function parseHm(text: string): [string, string] | null {
  const m =
    /(凌晨|早上|早晨|上午|中午|下午|傍晚|晚上|今晚|夜里)?\s*(\d{1,2}|[一二两三四五六七八九十]{1,3})\s*[:：点时]\s*(\d{1,2}|[一二两三四五六七八九十]{1,3}|半)?\s*分?/.exec(
      text,
    );
  if (!m) return null;
  let hour = cnNumber(m[2]);
  if (hour === null) return null;
  const minRaw = m[3];
  const minute = minRaw === undefined || minRaw === "" ? 0 : minRaw === "半" ? 30 : cnNumber(minRaw);
  if (minute === null) return null;
  const period = m[1];
  if (period === "中午") {
    if (hour < 6) hour += 12; // 中午 1 点 = 13 点
  } else if (period === "下午" || period === "傍晚" || period === "晚上" || period === "今晚" || period === "夜里") {
    if (hour < 12) hour += 12;
  }
  if (hour === 24) hour = 0;
  if (hour > 23 || minute > 59) return null;
  return [`${pad2(hour)}:${pad2(minute)}`, m[0]];
}

/**
 * 认重复规则。返回 `[规则, 命中的原文]`；没认出返回 null。
 *
 * 支持的写法：每天 / 每日 / 每隔 2 天 / 每 2 天 / 工作日 / 每周一（可连写「每周一三五」）/
 * 每月 8 号 / 每月最后一个工作日 / 每年 3 月 5 日。
 */
function parseRepeat(text: string): [RepeatRule, string] | null {
  const everyDays = /每隔\s*(\d+|[一二两三四五六七八九十]{1,3})\s*天|每\s*(\d+|[一二两三四五六七八九十]{1,3})\s*天/.exec(text);
  if (everyDays) {
    const n = cnNumber(everyDays[1] ?? everyDays[2]) ?? 1;
    return [{ freq: "daily", interval: Math.max(1, Math.min(99, n)), byday: [], until: null }, everyDays[0]];
  }
  const daily = /每(?:天|日)/.exec(text);
  if (daily) return [{ freq: "daily", interval: 1, byday: [], until: null }, daily[0]];
  // 「每月最后一个工作日」必须排在「工作日」前面：
  // 不然 `/工作日/` 先把「每月最后一个工作日」里的三个字吃掉，认成「每个工作日」。
  const lastWdEvery =
    /每\s*隔?\s*(\d+|[一二两三四五六七八九十]{1,3})\s*个月\s*的?\s*最后\s*(?:一个)?工作日/.exec(text);
  if (lastWdEvery) {
    const n = cnNumber(lastWdEvery[1]) ?? 1;
    return [
      { freq: "last_workday", interval: Math.max(1, Math.min(99, n)), byday: [], until: null },
      lastWdEvery[0],
    ];
  }
  const lastWd = /每\s*(?:个)?月\s*(?:的)?\s*最后\s*(?:一个)?工作日/.exec(text);
  if (lastWd) {
    return [{ freq: "last_workday", interval: 1, byday: [], until: null }, lastWd[0]];
  }
  const workdays = /工作日/.exec(text);
  if (workdays) return [{ freq: "weekdays", interval: 1, byday: [], until: null }, workdays[0]];
  // 允许中间有顿号/逗号/和/空格：「每周二、四」「每周一三五」都要认成一组
  const weekly = /每\s*(?:周|星期|礼拜)\s*([一二三四五六日天](?:\s*[、,，和]?\s*[一二三四五六日天])*)/.exec(text);
  if (weekly) {
    const byday = [
      ...new Set(
        [...weekly[1]]
          .map((c) => WEEKDAY_WORD[c])
          .filter((v): v is number => typeof v === "number"),
      ),
    ].sort((a, b) => a - b);
    if (byday.length === 0) return null;
    return [{ freq: "weekly", interval: 1, byday, until: null }, weekly[0]];
  }
  const monthly = /每\s*(?:个)?月\s*(\d{1,2})\s*[日号]?/.exec(text);
  if (monthly) {
    const d = Number(monthly[1]);
    if (d >= 1 && d <= 31) return [{ freq: "monthly", interval: 1, byday: [], until: null }, monthly[0]];
  }
  const yearly = /每年\s*(\d{1,2})\s*月\s*(\d{1,2})\s*[日号]?/.exec(text);
  if (yearly) {
    const m = Number(yearly[1]);
    const d = Number(yearly[2]);
    if (m >= 1 && m <= 12 && d >= 1 && d <= 31) {
      return [{ freq: "yearly", interval: 1, byday: [], until: null }, yearly[0]];
    }
  }
  return null;
}

/**
 * 解析一句话。
 *
 * `baseDay` 是「没说日期时任务会落在哪天」（也就是界面正在看的那天），默认取 `today`。
 */
export function parseQuick(
  text: string,
  today: string,
  baseDay: string = today,
  nowMs: number = Date.now(),
): QuickParse {
  const raw = text.trim();
  const repeatHit = raw ? parseRepeat(raw) : null;
  const dayHit = raw ? parseDay(raw, today) : null;
  const hmHit = raw ? parseHm(raw) : null;

  // 标题：把命中的片段摘掉（先摘长的，避免「每周一」被摘成「每」+「周一」两份）
  let rest = raw;
  for (const hit of [repeatHit?.[1], dayHit?.[1], hmHit?.[1]]) {
    if (hit) rest = rest.replace(hit, " ");
  }
  rest = rest.replace(/\s+/g, " ").replace(/^[，,、。;；\s]+|[，,、。;；\s]+$/g, "").trim();
  const title = rest === "" ? raw : rest;

  const repeat = repeatHit?.[0] ?? null;
  const day = dayHit?.[0] ?? null;
  const hm = hmHit?.[0] ?? null;
  const target = day ?? baseDay;

  // 给人看的确认文案：说了日期才报日期，说了重复就报重复，时刻单独带上
  const parts: string[] = [];
  if (repeat) parts.push(describeRepeat(repeat, target));
  else if (day) parts.push(dayText(day, today));
  if (hm) parts.push(hm);

  let past = false;
  if (hm) {
    const [y, m, d] = [Number(target.slice(0, 4)), Number(target.slice(5, 7)), Number(target.slice(8, 10))];
    const at = new Date(y, m - 1, d, Number(hm.slice(0, 2)), Number(hm.slice(3, 5))).getTime();
    past = at <= nowMs;
  }

  return { title, day, hm, repeat, label: parts.join(" "), past };
}
