import { useEffect, useState } from "react";
import { countTasksByDay, listTasks, type DayCount, type Task } from "./api";
import {
  WEEKDAY_SHORT,
  hmFromMs,
  monthEnd,
  monthGrid,
  monthLabel,
  monthStart,
  shiftMonth,
  shortDate,
  splitDay,
  weekdayOf,
} from "./day";
import { IconRepeat } from "./icons";

/**
 * 月总览：一眼看这个月哪天有活、哪天已经干完。
 *
 * 统计口径**和周条、页脚是同一个**：走后端 `countTasksByDay`（按天展开重复任务算出来），
 * 不是把任务表拉回前端自己数 —— 那样重复任务和「展开出来的一天」的算法会跟别处对不上。
 */
export default function Overview({
  day,
  today,
  onPick,
  onClose,
  onDayMenu,
}: {
  /** 打开面板时正在看的那天（默认选中的就是它） */
  day: string;
  /** 今天（后端 `today_local()` 给的，不是前端自己算的） */
  today: string;
  /** 真的跳过去：关掉总览、主列表切到那天 */
  onPick: (day: string) => void;
  onClose: () => void;
  /** 在某一天上右键：弹那天的菜单（在那天添加任务 / 跳过去） */
  onDayMenu?: (day: string, x: number, y: number) => void;
}) {
  // 锚点固定用「这个月的 1 号」：翻月、比较、算范围都省事
  const [anchor, setAnchor] = useState(() => monthStart(day));
  const [days, setDays] = useState<Map<string, DayCount>>(() => new Map());
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(true);
  // 点格子**不跳走**：就地选一天，下面列出那天的任务；想真跳过去再点那个按钮
  const [pick, setPick] = useState(day);
  const [picked, setPicked] = useState<Task[] | null>(null);
  const [pickErr, setPickErr] = useState<string | null>(null);
  const [pickBusy, setPickBusy] = useState(true);

  useEffect(() => {
    let alive = true;
    setBusy(true);
    countTasksByDay(monthStart(anchor), monthEnd(anchor))
      .then((list) => {
        if (!alive) return;
        setDays(new Map(list.map((d) => [d.day, d])));
        setErr(null);
      })
      .catch((e: unknown) => {
        if (alive) setErr(e instanceof Error ? e.message : String(e));
      })
      .finally(() => {
        if (alive) setBusy(false);
      });
    return () => {
      alive = false;
    };
  }, [anchor]);

  // 选中那天的任务：与 `list_by_day` 同一个命令，所以里面重复任务的展开行也在
  useEffect(() => {
    let alive = true;
    setPickBusy(true);
    listTasks(pick)
      .then((list) => {
        if (!alive) return;
        setPicked(list);
        setPickErr(null);
      })
      .catch((e: unknown) => {
        if (alive) setPickErr(e instanceof Error ? e.message : String(e));
      })
      .finally(() => {
        if (alive) setPickBusy(false);
      });
    return () => {
      alive = false;
    };
  }, [pick]);

  const [ay, am] = splitDay(anchor);
  const grid = monthGrid(anchor);
  // 汇总只算「这个月」的日子：网格里还有上个月末尾 / 下个月开头的补白
  const mine = grid.filter((d) => {
    const [y, m] = splitDay(d);
    return y === ay && m === am;
  });

  let total = 0;
  let undone = 0;
  let busyDays = 0;
  let doneDays = 0;
  let leftDays = 0;
  let busiest: { day: string; total: number } | null = null;
  for (const d of mine) {
    const c = days.get(d);
    if (!c || c.total === 0) continue;
    total += c.total;
    undone += c.undone;
    busyDays += 1;
    if (c.undone === 0) doneDays += 1;
    // 「还剩几天有没干完的」= 从今天（往后）数，还有未完成任务的天数
    if (c.undone > 0 && d >= today) leftDays += 1;
    if (!busiest || c.total > busiest.total) busiest = { day: d, total: c.total };
  }
  const done = total - undone;
  const pct = total === 0 ? 0 : Math.round((done / total) * 100);
  const isThisMonth = anchor === monthStart(today);

  return (
    <div className="panel">
      <div className="panel-head">
        <strong>月总览</strong>
        <span className="spacer" />
        <button type="button" className="ghost" onClick={onClose}>
          返回待办
        </button>
      </div>

      <div className="card">
        <div className="month-nav">
          <button
            type="button"
            className="ghost"
            title="上个月"
            onClick={() => setAnchor(shiftMonth(anchor, -1))}
          >
            ‹
          </button>
          <strong>{monthLabel(anchor)}</strong>
          <button
            type="button"
            className="ghost"
            title="下个月"
            onClick={() => setAnchor(shiftMonth(anchor, 1))}
          >
            ›
          </button>
          <span className="spacer" />
          {isThisMonth ? null : (
            <button type="button" className="mini" onClick={() => setAnchor(monthStart(today))}>
              回到本月
            </button>
          )}
        </div>

        <div className="month-stats">
          <span>
            本月 <b>{total}</b> 条
          </span>
          <span className="mdone">已完成 {done}</span>
          <span className="mtodo">未完成 {undone}</span>
          <span className="muted">完成率 {pct}%</span>
        </div>
        <div className="bar">
          <i style={{ width: `${pct}%` }} />
        </div>

        <div className="month-head">
          {WEEKDAY_SHORT.map((w) => (
            <span key={w}>{w}</span>
          ))}
        </div>
        <div className="month">
          {grid.map((d) => {
            const c = days.get(d);
            const [y, m, dd] = splitDay(d);
            const has = c !== undefined && c.total > 0;
            const allDone = has && c.undone === 0;
            const cls = [
              "md",
              y === ay && m === am ? "" : "other",
              d === today ? "today" : "",
              d === pick ? "sel" : "",
              has ? (allDone ? "all" : "open") : "",
            ]
              .filter(Boolean)
              .join(" ");
            return (
              <button
                key={d}
                type="button"
                className={cls}
                title={
                  has ? `${d}：共 ${c.total} 条，还剩 ${c.undone} 条` : `${d}：没有任务`
                }
                onClick={() => setPick(d)}
                onContextMenu={(e) => {
                  e.preventDefault();
                  e.stopPropagation();
                  onDayMenu?.(d, e.clientX, e.clientY);
                }}
              >
                <span className="md-num">{dd}</span>
                {has ? (
                  allDone ? (
                    <span className="md-badge alldone">✓</span>
                  ) : (
                    <span className="md-badge">
                      {c.total - c.undone}/{c.total}
                    </span>
                  )
                ) : null}
              </button>
            );
          })}
        </div>
        <p className="hint">
          格子上的数字是「已完成 / 当天条数」，✓ 表示那天都勾完了；点某一天就在下面列出那天的任务。
          {busy ? "（正在算…）" : ""}
        </p>
      </div>

      <div className="card">
        <div className="card-head">
          <strong>这个月的样子</strong>
        </div>
        <div className="row">
          <span className="muted">有任务的天数</span>
          <span className="spacer" />
          <span>
            {busyDays} / {mine.length} 天
          </span>
        </div>
        <div className="row">
          <span className="muted">已经全干完的天</span>
          <span className="spacer" />
          <span>{doneDays} 天</span>
        </div>
        <div className="row">
          <span className="muted">最忙的一天</span>
          <span className="spacer" />
          <span>{busiest ? `${shortDate(busiest.day)}（${busiest.total} 条）` : "还没安排"}</span>
        </div>
        <div className="row">
          <span className="muted">还剩几天有没干完的</span>
          <span className="spacer" />
          <span>{isThisMonth ? `${leftDays} 天` : "看的是别的月"}</span>
        </div>
      </div>

      <div className="card">
        <div className="card-head">
          <strong>
            {shortDate(pick)} {weekdayOf(pick)}
          </strong>
          <span className="muted">
            {pickBusy && picked === null
              ? "正在读…"
              : picked
                ? `${picked.length} 条`
                : ""}
          </span>
          <span className="spacer" />
          {/* 总览里只看不跳；真要看那天的完整列表（能编辑、能勾）再点这里 */}
          <button type="button" className="mini" onClick={() => onPick(pick)}>
            在列表里打开这天
          </button>
        </div>
        {pickErr ? (
          <div className="error">{pickErr}</div>
        ) : picked && picked.length > 0 ? (
          <div className="day-list">
            {picked.map((t) => (
              <div key={t.id} className={t.done ? "day-task done" : "day-task"}>
                <span className="dt-time">{hmFromMs(t.due_at_ms) || "—"}</span>
                <span className="dt-title">{t.title}</span>
                {t.repeat_rule !== null ? (
                  <span className="chip" title="重复任务（这天是展开出来的一天）">
                    <IconRepeat size={10} />
                  </span>
                ) : null}
                <span className={t.done ? "dt-state done" : "dt-state"}>
                  {t.done ? "已完成" : "未完成"}
                </span>
              </div>
            ))}
          </div>
        ) : (
          <p className="hint">{pickBusy ? "正在读…" : "这天没有任务。"}</p>
        )}
      </div>

      {err ? <div className="error">{err}</div> : null}
    </div>
  );
}
