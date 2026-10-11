import { useCallback, useEffect, useRef, useState, type MouseEvent as ReactMouseEvent } from "react";
import { listen } from "@tauri-apps/api/event";
import AppearancePanel from "./AppearancePanel";
import ContextMenu, { CtxItem } from "./ContextMenu";
import Overview from "./Overview";
import Settings from "./Settings";
import TitleBar from "./TitleBar";
import WeekStrip from "./WeekStrip";
import RepeatEditor from "./Repeat";
import AdvanceSelect from "./Advance";
import NotifyPicker from "./Notify";
import {
  DayCount,
  NextReminder,
  RepeatRule,
  Task,
  TaskStats,
  WorkStatus,
  KIND_LABEL,
  advanceToJson,
  countTasksByDay,
  createTask,
  deleteTasks,
  failureCount,
  floatVisible,
  hideMain,
  listChannels,
  listTasks,
  nextReminder as fetchNextReminder,
  notifyKindsToJson,
  parseAdvance,
  parseNotifyKinds,
  parseRepeat,
  repeatToJson,
  setOccurrenceDone,
  setTaskDone,
  setTaskStar,
  stopRepeat,
  taskStats,
  todayLocal,
  toggleFloat,
  updateTask,
  workStatus,
} from "./api";
import {
  advanceLabel,
  describeRepeat,
  fireAtLabel,
  hmFromMs,
  humanSpan,
  msFromHm,
  shortDate,
  weekDays,
  weekdayOf,
} from "./day";
import { IconAlert, IconBell, IconCheck, IconPlus, IconRepeat, IconSliders, IconStar } from "./icons";
import { loadAppearance, watchAppearance } from "./appearance";
import { lookClass, taskColor } from "./colors";
import { loadPhoto, loadPhotoOpacity } from "./photo";
import { parseQuick } from "./quick";
import { watchFocusComposer, watchOpenSettings, watchPhoto, watchTasks } from "./sync";

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** `day` 必须用任务自己的起始日（`t.day`），不能用正在浏览的那天：改重复任务时用浏览日会把整个系列挪走 */
/** 列表筛选：全部 / 未完成 / 已完成 */
type Filter = "all" | "undone" | "done";

type Editing = {
  id: number;
  title: string;
  hm: string;
  day: string;
  /**
   * 打开编辑框时任务自己的起始日（`day` 会被用户改，这里留着原值）。
   * 重复任务在别的日子编辑时，`day` 天生就不同于正在看的那天（展开行带的是系列起始日），
   * 不能拿它当「用户改了日期」——否则一保存视图就跳回系列第一天。
   */
  origDay: string;
  rule: RepeatRule | null;
  /** 提前多少秒；`null` = 认不出来，保存时保持库里的原值 */
  advance: number | null;
  /** 限定发到哪些渠道类型；`null` = 不限制（全部启用渠道） */
  notify: string[] | null;
};

export default function App() {
  const [today, setToday] = useState("");
  const [day, setDay] = useState("");
  const [tasks, setTasks] = useState<Task[]>([]);
  /** 这一周每天的待办数量：周条的圆点 + 顶部的「今日 x/y」 */
  const [counts, setCounts] = useState<Map<string, DayCount>>(new Map());
  /** 下一条将要响的提醒；`null` = 未来 7 天没有安排 */
  const [next, setNext] = useState<NextReminder | null>(null);
  /** 倒计时是「相对现在」算的，得有个会走的基准，否则界面停在打开那一刻 */
  const [nowMs, setNowMs] = useState(() => Date.now());
  const [adding, setAdding] = useState(false);
  const [title, setTitle] = useState("");
  const [hm, setHm] = useState("");
  /** 添加时的重复规则；null = 普通任务 */
  const [repeat, setRepeat] = useState<RepeatRule | null>(null);
  /** 添加任务时的「提前多少秒」，0 = 准点；提交时变成 `remind_offsets` */
  const [advance, setAdvance] = useState(0);
  /** 添加任务时限定发到哪些渠道；`null` = 不限制（全部启用渠道） */
  const [notify, setNotify] = useState<string[] | null>(null);
  /** 当前启用中的渠道类型（只有启用中的才在界面上给开关） */
  const [kinds, setKinds] = useState<string[]>([]);
  const [editing, setEditing] = useState<Editing | null>(null);
  const [err, setErr] = useState("");
  const [showSettings, setShowSettings] = useState(false);
  /** 外观面板（字体 / 字号 / 透明度）；和推送设置一样是全屏面板，不是浮层 */
  const [showAppearance, setShowAppearance] = useState(false);
  // 月总览：标题栏那个日历图标、以及上面「今日进度」那块都能打开它
  const [showOverview, setShowOverview] = useState(false);
  // 所有渠道都没发出去的提醒数（已用本机通知兜底）——托盘红点的界面版
  const [failures, setFailures] = useState(0);
  /** 列表筛选：默认「全部」（用户没要求默认只看未完成，而且刚勾完就会「消失」很吓人） */
  const [filter, setFilter] = useState<Filter>("all");
  /** 右下角的「今天 N 条 · 全部 M 条」 */
  const [stats, setStats] = useState<TaskStats | null>(null);
  const [work, setWork] = useState<WorkStatus | null>(null);
  /** 上面这行是什么时候取的（`left_ms` 得减去经过的时间才是现在的剩余） */
  const [workAt, setWorkAt] = useState(0);
  const [photo, setPhoto] = useState<string | null>(null);
  const [photoOpacity, setPhotoOpacity] = useState(() => loadPhotoOpacity().main);
  /** 右键菜单（只能同时开一个）。`task` 为 `null` = 在空白处点的，`at` 是那天的日期（没有就是没指定） */
  const [menu, setMenu] = useState<{
    x: number;
    y: number;
    task: Task | null;
    at: string | null;
  } | null>(null);
  /** 浮窗现在是显示着的吗（空白菜单里那条要按真实状态出文案） */
  const [floatOn, setFloatOn] = useState(false);
  /** 输入框：空白处右键「添加任务」要把它聚焦 */
  const composerRef = useRef<HTMLInputElement | null>(null);
  /**
   * 输入框里那个「从 <日期>」（新任务的起始日）。`null` = 跟着正在看的这天。
   * 每次打开输入框 / 添加成功后都清回 `null`，否则上次留下的日期会静默影响下一条任务。
   */
  const [addDay, setAddDay] = useState<string | null>(null);
  /**
   * `openComposer` 每次渲染都是一个新函数（里面读的是当时的 `day`）。
   * 跨窗口那个 effect 只挂一次，闭包里会是旧的 `day`，所以从 ref 里拿最新的那个。
   */
  const openComposerRef = useRef<() => void>(() => {});
  /** 外观里的「任务配色」开关（关掉就只剩星标 + 紧急强调，不刷 8 种颜色） */
  const [colorsOn, setColorsOn] = useState(() => loadAppearance().taskColors);

  // 「今天」不是常数：跨天、或者系统时间被校准（虚拟机挂起 / 睡眠唤醒之后很常见）都会变。
  // 只在挂载时算一次的话，常驻托盘的窗口会一直把昨天当今天——今日进度、周条的今天高亮、
  // 添加任务的默认日期就全停在昨天。所以每 30 秒顺手核一次（见下面的 tick）。
  const todayRef = useRef(today);
  const dayRef = useRef(day);
  useEffect(() => {
    todayRef.current = today;
  }, [today]);
  useEffect(() => {
    dayRef.current = day;
  }, [day]);

  useEffect(() => {
    todayLocal()
      .then((d) => {
        setToday(d);
        setDay(d);
      })
      .catch((e: unknown) => setErr(message(e)));
  }, []);

  const reload = useCallback(async (d: string) => {
    if (!d) return;
    try {
      const week = weekDays(d);
      const from = week[0] ?? d;
      const to = week[week.length - 1] ?? d;
      // 一次把「这一天的任务 / 这一周每天的数量 / 下一条提醒」都取回来：
      // 三个都是读操作、都很便宜，合并成一轮省得界面出现「列表已更新但圆点还是旧的」
      const [list, dayCounts, nextOne, chans, st, w] = await Promise.all([
        listTasks(d),
        countTasksByDay(from, to),
        fetchNextReminder(),
        listChannels(),
        taskStats(),
        workStatus(),
      ]);
      setTasks(list);
      setCounts(new Map(dayCounts.map((c) => [c.day, c])));
      setNext(nextOne);
      setStats(st);
      setWork(w);
      setWorkAt(Date.now());
      // 界面能选的渠道类型：只列启用中的，顺序跟着主渠道优先级（跟后端发信顺序一致）
      const usable = chans
        .filter((c) => c.enabled)
        .sort((a, b) => a.priority - b.priority)
        .map((c) => c.kind);
      setKinds([...new Set(usable)]);
      setNowMs(Date.now());
      setErr("");
    } catch (e: unknown) {
      setErr(message(e));
    }
  }, []);

  useEffect(() => {
    void reload(day);
  }, [day, reload]);

  // 浮窗那边勾了 / 改了任务（它可能不打开主窗口就直接改库）：跟着重新拉一遍，
  // 不然主窗口的列表、进度、下一条提醒都是旧的。跨窗口事件见 `sync.ts`。
  useEffect(() => watchTasks(() => void reload(dayRef.current)), [reload]);

  // 外观面板里改了「任务配色」要当场生效（同窗口事件，见 `appearance.ts`）
  useEffect(
    () => watchAppearance((a) => setColorsOn(a.taskColors)),
    [],
  );

  // 托盘菜单里的「打开设置」：Rust 侧只能发事件，面板得前端自己开
  useEffect(() => {
    const off = listen("ui://open-settings", () => setShowSettings(true));
    return () => {
      void off.then((un) => un());
    };
  }, []);

  // 浮窗里右键「添加任务」/「设置」：同一 origin，`localStorage` 广播一下就行（见 `sync.ts`）
  useEffect(() => watchFocusComposer(() => openComposerRef.current()), []);
  useEffect(() => watchOpenSettings(() => setShowSettings(true)), []);

  // 背景图：图片本体存在后端（应用目录里那份拷贝），前端只拿 data URL + 浓度。
  // 另一个窗口（浮窗）改了图片/浓度会 ping 过来，见 `sync.ts` 的 `watchPhoto`。
  useEffect(() => {
    const refresh = () => {
      void loadPhoto("main").then(setPhoto);
      setPhotoOpacity(loadPhotoOpacity().main);
    };
    refresh();
    return watchPhoto(refresh);
  }, []);

  // 有背景图时给 body 挂个 class：`styles.css` 靠它把卡片调成半透明，
  // 否则背景图会被实心卡片盖住，看起来像没生效
  useEffect(() => {
    document.body.classList.toggle("has-photo", photo !== null);
  }, [photo]);

  // 调度线程在后台跑，不会主动推给前端：每 30 秒自己看一眼（失败数 + 下一条 + 倒计时基准）
  useEffect(() => {
    let alive = true;
    const tick = () => {
      if (!alive) return;
      setNowMs(Date.now());
      // 顺便核一次「今天」有没有变（跨天 / 系统时间被校准）
      void todayLocal()
        .then((t) => {
          if (!alive || !t) return;
          const prev = todayRef.current;
          if (t === prev) return;
          todayRef.current = t;
          setToday(t);
          if (dayRef.current === prev) {
            // 用户正看着「今天」→ 跟着挪到新的今天（setDay 会触发 reload）
            setDay(t);
          } else {
            // 看的是别的日期 → 顺手刷一遍，周条的今天高亮和「今日进度」才不会停在昨天
            void reload(dayRef.current);
          }
        })
        .catch(() => {
          // 读不到就算了，30 秒后再试
        });
      void failureCount()
        .then((n) => {
          if (alive) setFailures(n);
        })
        .catch(() => {
          // 读不到就不显示，不打扰用户（例如后端正在重启）
        });
      void fetchNextReminder()
        .then((n) => {
          if (alive) setNext(n);
        })
        .catch(() => {});
    };
    const timer = window.setInterval(tick, 30_000);
    return () => {
      alive = false;
      window.clearInterval(timer);
    };
  }, []);

  // 快捷录入的实时预览（只用来显示「识别到什么」；真正落库时在 onAdd 里再算一次）
  // 新任务的起始日：界面上的「从 <日期>」选了就用它，没选就跟着正在看的这天
  const startDay = addDay ?? day;
  // 快捷录入的实时预览（只用来显示「识别到什么」；真正落库时在 onAdd 里再算一次）
  const quick = parseQuick(title, today, startDay);

  /**
   * 把一句话落库，返回落在哪一天（失败返回 `null`）。
   *
   * 「明天九点交周报」→ 日期 / 时刻 / 重复都从这句里认；认不出就全按界面上的值走。
   * 单独抽出来是给「从剪贴板新建」用：那条路径没有输入框，只有一个字符串。
   */
  async function addFromText(
    raw: string,
    hmFallback: string,
    dayFallback: string = day,
  ): Promise<string | null> {
    const text = raw.trim();
    if (!text) return null;
    const parsed = parseQuick(text, today, dayFallback);
    const targetDay = parsed.day ?? dayFallback;
    const targetHm = parsed.hm ?? hmFallback;
    try {
      await createTask({
        title: parsed.title.trim() || text,
        body: "",
        day: targetDay,
        due_at_ms: msFromHm(targetDay, targetHm),
        repeat_rule: repeatToJson(parsed.repeat ?? repeat),
        remind_offsets: advanceToJson(advance),
        notify_channels: notifyKindsToJson(notify),
      });
      return targetDay;
    } catch (e: unknown) {
      setErr(message(e));
      return null;
    }
  }

  async function onAdd() {
    const landed = await addFromText(title, hm, startDay);
    if (landed === null) return;
    setTitle("");
    setHm("");
    setAddDay(null);;
    setRepeat(null);
    setAdvance(0);
    setNotify(null);
    // 认出来的日期不是当前这天，就跟着跳过去：否则用户会以为「加了但没看见」
    if (landed !== day) setDay(landed);
    await reload(landed);
  }

  /**
   * 从面板里回到列表。总览 / 设置都是**全屏分支**（`return` 掉了主列表），
   * 不先回列表的话，输入框开了 / 任务建了用户也看不见。
   */
  function backToList() {
    setShowOverview(false);
    setShowSettings(false);
  }

  /** 打开输入框（可以指定先切到哪一天）：空白处右键、浮窗里「添加任务」都走这里 */
  function openComposer(target?: string) {
    backToList();
    if (target && target !== day) setDay(target);
    setEditing(null);
    // 起始日跟着这次打开的那天走，别把上次留下的日期带过来
    setAddDay(null);
    setAdding(true);
    // 输入框自带 `autoFocus`，这里再兜一次：从别的面板切回来时焦点可能还在别处
    window.requestAnimationFrame(() => composerRef.current?.focus());
  }
  // 供「浮窗要求打开输入框」那个跨窗口事件用（见上面的 `openComposerRef`）
  openComposerRef.current = openComposer;

  /**
   * 右键 → 「从剪贴板新建任务」。
   *
   * 只取**第一行**有内容的文字：复制一整篇文章时整段当一个标题并不好用，
   * 剩下的交给快捷录入（「明天九点交周报」这类写法跟输入框里一样）。
   * 读剪贴板失败（系统不给权限 / 窗口没聚焦）会明说，不默默什么都不发生。
   */
  async function pasteNewTask() {
    let text = "";
    try {
      text = await navigator.clipboard.readText();
    } catch {
      setErr("读不到剪贴板（系统没给权限）。可以点「添加任务」自己粘进去。");
      return;
    }
    const line = text
      .split(/\r?\n/)
      .map((s) => s.trim())
      .find((s) => s !== "");
    if (!line) {
      setErr("剪贴板里没有能当任务标题的文字。");
      return;
    }
    const landed = await addFromText(line, "");
    if (landed === null) return;
    backToList();
    if (landed !== day) setDay(landed);
    await reload(landed);
  }

  async function onToggle(task: Task) {
    try {
      // 重复任务勾的是「完成本次」：只结算这一天，系列继续（下拉里改规则、或下面「停止重复」才是真停）。
      // 普通任务勾的就是整个任务。判断用 `repeat_rule` 而不是 `repeated`：
      // 重复任务**起始日**那行 `repeated` 是 false，但勾选含义同样是「完成本次」。
      if (task.repeat_rule !== null) {
        await setOccurrenceDone(task.id, day, !task.done);
      } else {
        await setTaskDone(task.id, !task.done);
      }
      await reload(day);
    } catch (e: unknown) {
      setErr(message(e));
    }
  }

  async function onStopRepeat(id: number) {
    if (!window.confirm("停止重复？以后不再生成新的提醒，任务和已经勾过的记录都保留。")) return;
    try {
      await stopRepeat(id);
      setEditing(null);
      await reload(day);
    } catch (e: unknown) {
      setErr(message(e));
    }
  }

  async function onStar(t: Task) {
    try {
      await setTaskStar(t.id, !t.star);
      await reload(day);
    } catch (e: unknown) {
      setErr(message(e));
    }
  }

  async function onRemove(id: number) {
    if (!window.confirm("删除这条待办？")) return;
    try {
      await deleteTasks([id]);
      await reload(day);
    } catch (e: unknown) {
      setErr(message(e));
    }
  }

  async function onSaveEdit() {
    if (!editing) return;
    const t = editing.title.trim();
    if (!t) return;
    try {
      await updateTask(editing.id, {
        title: t,
        body: "",
        // 用 editing.day（任务起始日）而不是 day（正在看的那天）：重复任务在别的日子编辑时，
        // 用浏览日会把系列整体挪到那天
        day: editing.day,
        due_at_ms: msFromHm(editing.day, editing.hm),
        repeat_rule: repeatToJson(editing.rule),
        // null = 没动那个下拉 → 后端保持原值（不要编一个值出来把用户的提前提醒冲掉）
        remind_offsets: editing.advance === null ? null : advanceToJson(editing.advance),
        notify_channels: notifyKindsToJson(editing.notify),
      });
      const moved = editing.day !== editing.origDay;
      setEditing(null);
      // 起始日改了：跟着跳到那天。不跳的话这条任务从当前列表里消失，看着像被删了
      if (moved) setDay(editing.day);
      else await reload(day);
    } catch (e: unknown) {
      setErr(message(e));
    }
  }

  // 背景层：铺在最底下、`pointer-events: none`（见 `styles.css` 的 `.backdrop`）
  const backdrop = photo ? (
    <div
      className="backdrop"
      aria-hidden="true"
      style={{ backgroundImage: `url(${photo})`, opacity: photoOpacity }}
    />
  ) : null;
  /** 打开编辑（双击标题、右键「编辑」都走这里）。`t.day` = 任务自己的起始日，展开行也得用它回存 */
  function openEditor(t: Task) {
    setEditing({
      id: t.id,
      title: t.title,
      hm: hmFromMs(t.due_at_ms),
      day: t.day,
      origDay: t.day,
      rule: parseRepeat(t.repeat_rule),
      advance: parseAdvance(t.remind_offsets),
      notify: parseNotifyKinds(t.notify_channels),
    });
  }

  /**
   * 空白处右键菜单。`at` 是那天的日期（在周条 / 总览格子上点的），没有就是普通留白。
   *
   * 日期格子上的菜单只放「那天」干得了的事（在那天添加 / 跳过去），
   * 全局动作（浮窗、设置、总览、收进托盘）只放留白处。
   */
  function blankMenu(at: string | null): CtxItem[] {
    const items: CtxItem[] = [
      {
        label: at === null ? "添加任务" : `在 ${shortDate(at)} 添加任务`,
        onPick: () => openComposer(at ?? undefined),
      },
    ];
    if (at !== null && at !== day) {
      items.push({ label: `跳到 ${shortDate(at)}`, onPick: () => setDay(at) });
    }
    items.push({ label: "从剪贴板新建任务", onPick: () => void pasteNewTask() });
    if (at !== null) return items;
    items.push(
      {
        label: floatOn ? "隐藏浮窗" : "显示浮窗",
        onPick: () => void toggleFloat().then(setFloatOn).catch(() => {}),
        separatorBefore: true,
      },
      { label: "设置（推送渠道 · 通用）", onPick: () => setShowSettings(true) },
      { label: "总览（这个月）", onPick: () => setShowOverview(true) },
      {
        label: "收进托盘（到点照样提醒）",
        // 走 Rust 的 `hide_main`：和前端的 `hide()` 不同，它会顺路 `sync_float`，让浮窗顶上
        onPick: () => void hideMain(),
        separatorBefore: true,
      },
    );
    return items;
  }

  /** 在空白处点右键：开菜单，并顺手问一下浮窗现在是不是显示着的（那条的文案得跟真实状态一致） */
  function openBlankMenu(x: number, y: number, at: string | null) {
    setMenu({ x, y, task: null, at });
    void floatVisible()
      .then(setFloatOn)
      .catch(() => {
        // 问不到就按「没显示」出文案，真点下去也会把状态拽回来
      });
  }

  /**
   * 空白处右键的入口。子元素的事件也会冒到这儿，所以**必须按目标判断**，
   * 不能指望子元素都自己 `stopPropagation`（漏一个就会弹两层菜单）。
   */
  function onBlankCtx(e: ReactMouseEvent) {
    const el = e.target as HTMLElement;
    if (el.closest("button, input, select, textarea, a, .task, .day, .md, .float-item, .ctx-menu")) {
      return;
    }
    e.preventDefault();
    openBlankMenu(e.clientX, e.clientY, null);
  }

  /** 右键菜单浮层：主界面和总览都要用，所以在几个分支return之前算好 */
  const menuLayer = menu ? (
    <ContextMenu
      x={menu.x}
      y={menu.y}
      onClose={() => setMenu(null)}
      items={
        menu.task === null
          ? blankMenu(menu.at)
          : taskMenu(menu.task, {
              star: () => void onStar(menu.task as Task),
              edit: () => openEditor(menu.task as Task),
              done: () => void onToggle(menu.task as Task),
              remove: () => void onRemove((menu.task as Task).id),
            })
      }
    />
  ) : null;

  const workLeft = work
    ? humanSpan(Math.max(0, Math.round((work.left_ms - (nowMs - workAt)) / 1000)))
    : "";

  if (showSettings) {
    return (
      <div className="app">
        {backdrop}
        <TitleBar
          onAppearance={() => {
            setShowSettings(false);
            setShowAppearance(true);
          }}
        />
        <Settings
          onClose={() => {
            setShowSettings(false);
            void reload(day);
          }}
        />
      </div>
    );
  }

  if (showAppearance) {
    return (
      <div className="app">
        {backdrop}
        {/* 面板一开着就不用在标题栏再给一次入口了（不传 `onAppearance` 就不显示那个按钮） */}
        <TitleBar />
        <AppearancePanel onClose={() => setShowAppearance(false)} />
      </div>
    );
  }

  if (showOverview) {
    return (
      <div className="app" onContextMenu={onBlankCtx}>
        {backdrop}
        {/* 总览里点某一天 → 关掉面板、跳到那天（`day` 一变主列表自己会重载） */}
        <TitleBar />
        <Overview
          day={day}
          today={today}
          onPick={(d) => {
            setShowOverview(false);
            setDay(d);
          }}
          onDayMenu={(d, x, y) => openBlankMenu(x, y, d)}
          onClose={() => setShowOverview(false)}
        />
        {menuLayer}
      </div>
    );
  }

  const todayCount = counts.get(today);
  const total = todayCount?.total ?? 0;
  const undone = todayCount?.undone ?? 0;
  const doneCount = total - undone;
  const pct = total === 0 ? 0 : Math.round((doneCount / total) * 100);
  // 筛选只影响看得见的行，不动上面那几个统计（统计是「今天有多少」，与筛选无关）
  const shown = tasks.filter((t) =>
    filter === "all" ? true : filter === "undone" ? !t.done : t.done,
  );
  const nextValue = next ? fireAtLabel(next.fire_at_ms, nowMs) : "没有安排";
  const nextSub = next
    ? next.day === today
      ? next.title
      : `${next.title} · ${shortDate(next.day)}`
    : "未来 7 天都没有提醒";

  return (
    <div className="app" onContextMenu={onBlankCtx}>
      {backdrop}
      <TitleBar
        onAppearance={() => setShowAppearance(true)}
        onOverview={() => setShowOverview(true)}
      />

      <div className="body">
        <div className="hero">
          <button
            type="button"
            className="hero-btn"
            title="推送渠道 · 快捷键 · 导入导出 · 上下班 · 背景图"
            onClick={() => setShowSettings(true)}
          >
            <IconSliders size={17} />
            <span>设置</span>
            {failures > 0 ? (
              <span className="badge" title={`${failures} 条提醒所有渠道都没发出去`}>
                {failures}
              </span>
            ) : null}
          </button>

          <div className="hero-block">
            <span className="hero-label">下一条提醒</span>
            <span className="hero-value">{nextValue}</span>
            <span className="hero-sub">{nextSub}</span>
          </div>

          {/* 点「今日进度」也能开月总览：想看的不只是今天。格子/样式见 `.hero-block.tap` */}
          <button
            type="button"
            className="hero-block narrow tap"
            title="月总览：看这个月的整体情况"
            onClick={() => setShowOverview(true)}
          >
            <span className="hero-label">今日进度</span>
            <span className="hero-value">
              {doneCount}/{total}
            </span>
            <div className="bar">
              <i style={{ width: `${pct}%` }} />
            </div>
          </button>
        </div>

        <WeekStrip
          day={day}
          today={today}
          counts={counts}
          onPick={setDay}
          onDayMenu={(d, x, y) => openBlankMenu(x, y, d)}
        />

        {/* 上下班倒计时：点开去改时间（设置 → 通用）。没开就不占地方 */}
        {work?.enabled && work.next_label ? (
          <button
            type="button"
            className="work-bar"
            onClick={() => setShowSettings(true)}
            title="点这里改上下班时间（设置 → 通用）"
          >
            <IconBell size={11} />
            <span>
              距离{work.next_label}还有 {workLeft}
            </span>
          </button>
        ) : null}

        {/* 筛选：只动看得见的行；「今天 x/y」和右下角的数字始终是全部 */}
        <div className="filters" role="group" aria-label="筛选任务">
          {(
            [
              ["all", "全部"],
              ["undone", "未完成"],
              ["done", "已完成"],
            ] as const
          ).map(([key, label]) => (
            <button
              key={key}
              type="button"
              className={filter === key ? "on" : ""}
              onClick={() => setFilter(key)}
            >
              {label}
              {key === "all" ? null : (
                <span className="n">
                  {tasks.filter((t) => (key === "undone" ? !t.done : t.done)).length}
                </span>
              )}
            </button>
          ))}
        </div>

        {failures > 0 ? (
          <button type="button" className="alert" onClick={() => setShowSettings(true)}>
            <IconAlert />
            <span>{failures} 条提醒没发出去（已用本机通知兜底，点开看记录）</span>
          </button>
        ) : null}

        {err ? <div className="error">{err}</div> : null}

        <main className="sheet">
          {shown.map((t) => {
            const advance = t.due_at_ms === null ? "" : advanceLabel(t.remind_offsets);
            // 重复规则单独算一份文案：`parseRepeat` 对脏数据返回 null，`describeRepeat` 再兜一层
            const repeatText = describeRepeat(parseRepeat(t.repeat_rule), t.day);
            // 这条任务是否限定了渠道（`null` = 不限制，发全部启用渠道 → 不显示标签）
            const only = parseNotifyKinds(t.notify_channels);
            const notifyText = only === null ? "" : `仅${only.map((k) => KIND_LABEL[k] ?? k).join("、")}`;
            // 选了但现在没启用的渠道：标签里说清楚，否则用户会以为会发过去
            const stale = only === null ? [] : only.filter((k) => !kinds.includes(k));
            // 外观强调：`starred`（用户标的重点）+ `soon`/`overdue`（时间说了算）
            const look = lookClass(t, nowMs);
            return (
              <div
                key={t.id}
                className={
                  (t.done ? "task done" : "task") +
                  (colorsOn ? " colored" : "") +
                  (look === "" ? "" : ` ${look}`)
                }
                onContextMenu={(e) => {
                  e.preventDefault();
                  // 不让它冒到「空白处」那个处理器上，否则会先弹任务菜单、再被空白菜单盖掉
                  e.stopPropagation();
                  setMenu({ x: e.clientX, y: e.clientY, task: t, at: null });
                }}
              >
                {/* 颜色标记：细条就够（用整行底色会很乱），完成的不再着色 */}
                {colorsOn && !t.done ? (
                  <span
                    className="tbar"
                    aria-hidden="true"
                    style={{ background: taskColor(t.id) }}
                  />
                ) : null}
                <button
                  type="button"
                  className="check"
                  title={
                    t.repeat_rule !== null
                      ? t.done
                        ? "取消「完成本次」"
                        : "完成本次（系列继续）"
                      : t.done
                        ? "标记为未完成"
                        : "标记为已完成"
                  }
                  onClick={() => void onToggle(t)}
                >
                  {t.done ? <IconCheck size={13} /> : null}
                </button>

                {editing !== null && editing.id === t.id ? (
                  <div className="edit">
                    <div className="edit-row">
                      <input
                        className="edit-title"
                        value={editing.title}
                        autoFocus
                        onChange={(e) => setEditing({ ...editing, title: e.target.value })}
                        onKeyDown={(e) => {
                          if (e.key === "Enter") void onSaveEdit();
                          if (e.key === "Escape") setEditing(null);
                        }}
                      />
                      <input
                        className="edit-time"
                        type="time"
                        value={editing.hm}
                        onChange={(e) => setEditing({ ...editing, hm: e.target.value })}
                      />
                      <button
                        type="button"
                        className="mini primary"
                        onClick={() => void onSaveEdit()}
                      >
                        保存
                      </button>
                      <button type="button" className="mini" onClick={() => setEditing(null)}>
                        取消
                      </button>
                    </div>

                    <RepeatEditor
                      rule={editing.rule}
                      day={editing.day}
                      onChange={(rule) => setEditing({ ...editing, rule })}
                      /* 起始日：「从 X 重复到 Y」要连起来读，所以交给 RepeatEditor 排在自己前面 */
                      from={{
                        value: editing.day,
                        onChange: (value) => setEditing({ ...editing, day: value }),
                      }}
                      extra={
                        <>
                          <AdvanceSelect
                            value={editing.advance}
                            onChange={(advance) => setEditing({ ...editing, advance })}
                          />
                          <NotifyPicker
                            kinds={editing.notify}
                            available={kinds}
                            onChange={(notify) => setEditing({ ...editing, notify })}
                          />
                        </>
                      }
                    />

                    {t.repeated ? (
                      <span className="edit-hint">
                        这是重复任务展开出来的一天，改的是整个系列
                      </span>
                    ) : null}

                    {t.repeat_rule !== null ? (
                      <div className="edit-row">
                        <button
                          type="button"
                          className="mini"
                          title="不再重复，任务和已勾过的记录都留着"
                          onClick={() => void onStopRepeat(t.id)}
                        >
                          停止重复
                        </button>
                      </div>
                    ) : null}
                  </div>
                ) : (
                  <>
                    <div className="task-main">
                      <span
                        className="title"
                        title="双击可编辑（含重复规则）"
                        onDoubleClick={() => openEditor(t)}
                      >
                        {t.title}
                      </span>
                      {repeatText || advance || notifyText ? (
                        <span className="meta">
                          {repeatText ? (
                            <span
                              className="chip"
                              title={
                                t.repeated
                                  ? "重复任务展开出来的一天：勾选 = 只完成本次"
                                  : "重复任务：勾选 = 完成本次，系列继续"
                              }
                            >
                              <IconRepeat size={11} /> {repeatText}
                            </span>
                          ) : null}
                          {advance ? (
                            <span className="chip">
                              <IconBell size={11} /> {advance}
                            </span>
                          ) : null}
                          {notifyText ? (
                            <span
                              className="chip"
                              title={
                                stale.length > 0
                                  ? `只发到这些渠道；${stale.map((k) => KIND_LABEL[k] ?? k).join("、")} 现在没启用，到点会走本机通知兜底`
                                  : "这条任务的提醒只发到这些渠道"
                              }
                            >
                              <IconBell size={11} /> {notifyText}
                            </span>
                          ) : null}
                        </span>
                      ) : null}
                    </div>
                    <span className="due">{hmFromMs(t.due_at_ms) || "—"}</span>
                    <button
                      type="button"
                      className={t.star ? "star-btn on" : "star-btn"}
                      title={t.star ? "取消重点" : "标为重点（只会更醒目，不影响提醒）"}
                      onClick={() => void onStar(t)}
                    >
                      <IconStar size={13} filled={t.star} />
                    </button>
                    <button
                      type="button"
                      className="row-action"
                      title="删除"
                      onClick={() => void onRemove(t.id)}
                    >
                      删除
                    </button>
                  </>
                )}
              </div>
            );
          })}

          {adding ? (
            <div className="composer">
              <div className="composer-row">
                <input
                  ref={composerRef}
                  type="text"
                  autoFocus
                  placeholder="要做什么？（可写「明天九点交周报」）"
                  value={title}
                  onChange={(e) => setTitle(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") void onAdd();
                    if (e.key === "Escape") {
                      setAdding(false);
                      setAddDay(null);
                    }
                  }}
                />
                <input type="time" value={hm} onChange={(e) => setHm(e.target.value)} />
                <button
                  type="button"
                  className="primary"
                  disabled={!title.trim()}
                  onClick={() => void onAdd()}
                >
                  添加
                </button>
              </div>
              {quick.label ? (
                <div className="quick-hint">
                  识别到：{quick.label}
                  {quick.title.trim() && quick.title.trim() !== title.trim()
                    ? ` · 标题「${quick.title.trim()}」`
                    : ""}
                  {quick.past ? " · 时间已过去，不会提醒" : ""}
                </div>
              ) : null}
              <RepeatEditor
                rule={repeat}
                day={startDay}
                onChange={setRepeat}
                /* 不选就跟着正在看的这天（`startDay` = `addDay ?? day`） */
                from={{ value: startDay, onChange: (value) => setAddDay(value) }}
                extra={
                  <>
                    <AdvanceSelect
                      value={advance}
                      onChange={(secs) => setAdvance(secs ?? 0)}
                    />
                    <NotifyPicker kinds={notify} available={kinds} onChange={setNotify} />
                  </>
                }
              />
            </div>
          ) : (
            <button type="button" className="add-row" onClick={() => setAdding(true)}>
              <IconPlus size={15} />
              <span>添加任务</span>
            </button>
          )}

          {shown.length === 0 && !adding ? (
            <div className="empty">
              {tasks.length > 0
                ? "这个筛选下没有任务"
                : day === today
                  ? "今天还没有安排"
                  : `${weekdayOf(day)}这天还没有安排`}
            </div>
          ) : null}
        </main>

        <div className="foot">
          <span className="foot-hint">
            双击标题可编辑（重复规则在里面）· 任务行右键有菜单 · 留白 / 日期上右键也是菜单 ·
            点 ✕ 只是收进托盘，到点照样提醒
          </span>
          <span className="foot-count">
            今天 {stats?.today_total ?? 0} 条 · 全部 {stats?.all_total ?? 0} 条
          </span>
        </div>
      </div>
      {menuLayer}
    </div>
  );
}

/** 任务行的右键菜单：只放真能在这行上干的事 */
function taskMenu(
  t: Task,
  actions: { star: () => void; edit: () => void; done: () => void; remove: () => void },
): CtxItem[] {
  return [
    { label: t.star ? "取消重点" : "标记重点", onPick: actions.star },
    { label: "编辑（重复规则 / 提醒）", onPick: actions.edit },
    {
      label: t.done ? "取消完成" : "标记完成",
      onPick: actions.done,
      separatorBefore: true,
    },
    { label: "删除", onPick: actions.remove, danger: true },
  ];
}


