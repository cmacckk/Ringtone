/**
 * 任务配色与「重点 / 紧急」的视觉规则。
 *
 * 设计取向（参考待办类产品的做法）：
 *  1. **颜色只当标记，不当底色**：每条任务左侧一条 3px 色条 + 浮窗里一个小圆点，
 *     整行仍然是中性色。大面积上色会「太乱」，也会让深色/浅色主题都很难调。
 *  2. **颜色只用来区分任务，不用来表示状态**：状态（完成 / 逾期 / 快到点）用
 *     形状和位置表达（勾选圈的样式、色条粗细、时间的颜色），两者不抢同一块信息。
 *  3. **低饱和度**：8 个色相均匀铺开、饱和度统一 55%、明度统一 58%，
 *     在白底和深底上都能看清，彼此也不会糊成一片。
 *  4. **「重点」是用户说了算**（手动星标），**「紧急」是时间说了算**（自动）；
 *     两者叠加时只加粗、不换色，避免一条任务出现三四层视觉重量。
 */

/** 8 个低饱和色相（均匀铺开），同一份颜色在浅色 / 深色主题下都用 */
const HUES = [210, 180, 145, 45, 28, 355, 275, 320];

/** 任务色：按 id 固定分配，同一条任务永远同一个颜色（换天、刷新都不变） */
export function taskColor(id: number): string {
  const h = HUES[((id % HUES.length) + HUES.length) % HUES.length];
  return `hsl(${h} 55% 58%)`;
}

/** 快到点 / 已经过期的阈值：30 分钟内算「快到点」 */
export const SOON_MS = 30 * 60 * 1000;

/** 时间维度的强调档位 */
export type Urgency = "overdue" | "soon" | "none";

/**
 * 按时间算强调档位。
 *
 * 只看**未完成**且有具体时刻的任务：已完成的不该再抢注意力，
 * 没定时刻的（`due_at_ms === null`）只排在那天，不算「紧急」。
 */
export function urgencyOf(
  task: { done: boolean; due_at_ms: number | null },
  nowMs: number,
): Urgency {
  if (task.done || task.due_at_ms === null) return "none";
  if (task.due_at_ms < nowMs - 60_000) return "overdue";
  if (task.due_at_ms - nowMs <= SOON_MS) return "soon";
  return "none";
}

/** 一条任务在外观上要叠加的类名（星标 / 紧急），供 CSS 用 */
export function lookClass(
  task: { done: boolean; star: boolean; due_at_ms: number | null },
  nowMs: number,
): string {
  const parts: string[] = [];
  if (task.star) parts.push("starred");
  const u = urgencyOf(task, nowMs);
  if (u !== "none") parts.push(u);
  return parts.join(" ");
}
