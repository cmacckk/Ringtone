import type { DayCount } from "./api";
import { shortDate, startOfWeek, weekDays, weekdayOf, shiftDay } from "./day";
import { IconChevronLeft, IconChevronRight } from "./icons";

type Props = {
  /** 当前选中的那天 */
  day: string;
  /** 本机今天（由后端给，别用前端时间算——前后端时区不一致时圆点会错位一天） */
  today: string;
  /** 这一周每天的待办数量，只有「有任务的天」才会出现在里面 */
  counts: Map<string, DayCount>;
  onPick: (day: string) => void;
  /** 在某一天上右键：弹那天的菜单（在那天添加任务 / 跳过去） */
  onDayMenu?: (day: string, x: number, y: number) => void;
};

/**
 * 周条：7 天平铺，两侧箭头整周翻页。
 *
 * 选中日给一块浅色圆角底，**有任务的那天右上角一个橙点**（kite 的做法）——
 * 这样不用点进去就知道哪天有安排。
 */
export default function WeekStrip({ day, today, counts, onPick, onDayMenu }: Props) {
  const anchor = day || today;
  const days = weekDays(anchor);
  const start = startOfWeek(anchor);

  return (
    <div className="week">
      <button
        type="button"
        className="week-arrow"
        title="上一周"
        onClick={() => onPick(shiftDay(start, -7))}
      >
        <IconChevronLeft />
      </button>

      {days.map((d) => {
        const count = counts.get(d);
        const cls = ["day"];
        if (d === day) cls.push("sel");
        if (d === today) cls.push("today");
        return (
          <button
            key={d}
            type="button"
            className={cls.join(" ")}
            title={
              count
                ? `${d} · ${count.undone} 条未完成 / 共 ${count.total} 条`
                : `${d} · 没有安排`
            }
            onClick={() => onPick(d)}
            onContextMenu={(e) => {
              // 不让它冒到「空白处」那个处理器上：否则会先弹这天的菜单、再被全局菜单覆盖
              e.preventDefault();
              e.stopPropagation();
              onDayMenu?.(d, e.clientX, e.clientY);
            }}
          >
            <span className="wd">{weekdayOf(d)}</span>
            <span className="dd">{shortDate(d)}</span>
            {count ? <span className="mark" /> : null}
          </button>
        );
      })}

      <button
        type="button"
        className="week-arrow"
        title="下一周"
        onClick={() => onPick(shiftDay(start, 7))}
      >
        <IconChevronRight />
      </button>
    </div>
  );
}
