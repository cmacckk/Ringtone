/**
 * 待办浮窗（第二个窗口，label = `float`）。
 *
 * 什么时候出现、什么时候藏起来由 Rust 定（`sync_float`：主窗口最小化或收进托盘时顶上，
 * 主窗口回来了就让位），这里只管三件事：
 *  1. **贴边**：钉在屏幕右侧，收起时只留一条 9px 的边 —— 这就是「自动靠边、一段时间后隐藏」；
 *  2. **鼠标进来就展开**（MouseEnter），离开两三秒后再收起；
 *  3. 自己拉数据（`listTasks` + `taskStats`），30 秒一次。
 *
 * 位置动画是一帧一帧调 `setPosition` 画的：窗口本身没有 CSS 可动画，
 * 想让它「滑」出来只能自己算帧。
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { cursorPosition, getCurrentWindow } from "@tauri-apps/api/window";
import { PhysicalPosition } from "@tauri-apps/api/dpi";
import {
  FloatSide,
  Task,
  TaskStats,
  createTask,
  floatDock,
  floatSetSide,
  listTasks,
  repeatToJson,
  setOccurrenceDone,
  setTaskDone,
  setTaskStar,
  showMain,
  taskStats,
  todayLocal,
} from "./api";
import { hmFromMs, msFromHm } from "./day";
import { parseQuick } from "./quick";
import { taskColor } from "./colors";
import ContextMenu, { CtxItem } from "./ContextMenu";
import { IconCheck, IconStar } from "./icons";
import { loadPhoto, loadPhotoOpacity } from "./photo";
import { pingFocusComposer, pingOpenSettings, pingTasks, watchPhoto, watchTasks } from "./sync";

/** 展开 / 收起的滑动时长 */
const SLIDE_MS = 160;
/** 出现之后、鼠标离开之后，各自多久自动收起 */
const AFTER_SHOW_MS = 6000;
const AFTER_LEAVE_MS = 2500;
/** 自己拉数据的间隔（调度线程不会主动推给前端） */
const POLL_MS = 30_000;
/** 列表最多列几条（再多了列表自己滚，见 `.float-list` 的 `overflow-y`） */
const MAX_ROWS = 5;

/** 是不是浮窗那个实例：Rust 建窗口时带了 `?float=1` */
export function isFloatWindow(): boolean {
  return (
    new URLSearchParams(window.location.search).get("float") === "1" ||
    window.location.hash === "#float"
  );
}

/** 自己拖到哪一边 / 什么高度就记下来；下次启动还在这边这个高度 */
const SIDE_KEY = "ringtone.floatSide";
const DRAG_Y_KEY = "ringtone.floatY";

/** 拖动时算不算「真的动了」（像素）——没动就是「点一下打开主窗口」 */
const DRAG_MIN_PX = 4;

function loadSide(): FloatSide {
  return window.localStorage.getItem(SIDE_KEY) === "left" ? "left" : "right";
}

function loadDragY(): number | null {
  const raw = window.localStorage.getItem(DRAG_Y_KEY);
  const n = raw === null ? NaN : Number(raw);
  return Number.isFinite(n) ? n : null;
}

function clamp(n: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, n));
}

export default function FloatWindow() {
  const [tasks, setTasks] = useState<Task[]>([]);
  const [stats, setStats] = useState<TaskStats | null>(null);
  const [err, setErr] = useState("");
  const [photo, setPhoto] = useState<string | null>(null);
  const [photoOpacity, setPhotoOpacity] = useState(() => loadPhotoOpacity().float);

  /** 重新读一遍背景图 + 浓度（挂载时、以及主窗口那边改完 ping 过来时） */
  const refreshPhoto = useCallback(() => {
    void loadPhoto("float").then(setPhoto);
    setPhotoOpacity(loadPhotoOpacity().float);
  }, []);

  useEffect(() => {
    refreshPhoto();
  }, [refreshPhoto]);

  // 有背景图时给 body 挂个 class：`styles.css` 靠它把卡片调成半透明，
  // 否则背景图会被实心卡片盖住，看起来像没生效
  useEffect(() => {
    document.body.classList.toggle("has-photo", photo !== null);
  }, [photo]);
  /** 贴哪一边：拖到另一头就换边，记在本机（重启还在这边） */
  const side = useRef<FloatSide>(loadSide());
  /** 用户自己拖的竖向位置；`null` = 用 Rust 算的标准位置 */
  const dragY = useRef<number | null>(loadDragY());
  const expandedX = useRef(0);
  const collapsedX = useRef(0);
  const y = useRef(0);
  const collapsed = useRef(true);
  const anim = useRef<number | null>(null);
  const hideTimer = useRef<number | null>(null);
  /** 正在被用户拖着：这时候不要自动收起 */
  const dragging = useRef(false);
  /** 鼠标按下了（还没抬起来）。区分「点一下」和「拖一把」用：只有真动了才算拖 */
  const pressed = useRef(false);
  /** 真的拖动过（和 `pressed` 合起来推「点一下」） */
  const moved = useRef(false);
  /** 拖动起点：窗口位置 + 鼠标屏幕位置（都是物理像素） */
  const dragFrom = useRef<{ px: number; py: number; cx: number; cy: number } | null>(null);
  /** 已经有一次「读光标 + 摆窗口」在飞了（拖动事件比 IPC 快，不排一下会越拖越落后） */
  const moving = useRef(false);
  /** 右键菜单：坐标 + 哪条任务（`task` 为 `null` = 在空白处点的） */
  const [menu, setMenu] = useState<{ x: number; y: number; task: Task | null } | null>(null);
  /** 右下角那句话：平时是「点这里打开主窗口」，出错 / 刚新建时临时换一下 */
  const [note, setNote] = useState("");
  const noteTimer = useRef(0);
  /** 在右下角说一句话，4 秒后恢复原样 */
  const say = useCallback((text: string) => {
    setNote(text);
    window.clearTimeout(noteTimer.current);
    noteTimer.current = window.setTimeout(() => setNote(""), 4000);
  }, []);
  /** 后端给的「今天」。勾重复任务时要带上它（勾的是「这一天的那个实例」） */
  const todayRef = useRef("");

  /** 摆窗口 */
  const place = useCallback(async (x: number, at: number) => {
    await getCurrentWindow().setPosition(
      new PhysicalPosition(Math.round(x), Math.round(at)),
    );
  }, []);

  const load = useCallback(async () => {
    try {
      const today = await todayLocal();
      const [list, st] = await Promise.all([listTasks(today), taskStats()]);
      todayRef.current = today;
      setTasks(list);
      setStats(st);
      setErr("");
    } catch (e: unknown) {
      setErr(e instanceof Error ? e.message : String(e));
    }
  }, []);

  /**
   * 在浮窗里勾 / 取消勾选。
   *
   * 规则和主窗口一致：重复任务动的是「今天这一天」的实例（`set_occurrence_done`），
   * 普通任务动的是任务本身（`set_task_done`）。判断用 `repeat_rule` 而不是 `repeated`——
   * 重复任务在它起始那一天 `repeated=false`，但含义同样是「完成本次」。
   */
  const toggle = useCallback(
    async (t: Task) => {
      try {
        if (t.repeat_rule !== null) {
          if (todayRef.current === "") return; // 还没拿到「今天」，等下一次轮询
          await setOccurrenceDone(t.id, todayRef.current, !t.done);
        } else {
          await setTaskDone(t.id, !t.done);
        }
        await load();
        // 主窗口（可能正收在托盘里）那边重新拉一遍，不然它还是旧数字
        pingTasks();
      } catch (e: unknown) {
        setErr(e instanceof Error ? e.message : String(e));
      }
    },
    [load],
  );

  /** 算好两个 x（展开位 / 收起位），并把窗口先摆到收起位（免得先整块闪一下再收） */
  const dock = useCallback(async () => {
    try {
      // 坐标问 Rust 要：它按「主窗口所在那块屏」算，比 WebView 里的 window.screen / screenX 靠谱
      // （实测 screenX=4258，而窗口其实在 4852）；顺便也解决「浮窗掉到另一块屏去」的问题——
      // 浮窗是运行时建的，不指定的话默认落在主显示器上，用户看的却是主窗口那块屏。
      // **坐标是物理像素**（Rust 那边从 `outer_size` 反推缩放），所以这里也必须用 PhysicalPosition：
      // 这台机器两块屏缩放不同（150% / 100%），过一次逻辑像素换算就会跑到屏幕外面。
      //
      // 用 `floatSetSide` 而不是 `floatDock`：它顺便把「贴哪一边」告诉 Rust。
      // 本机记着左边而 Rust 默认右边时，只有这样两边才一致。
      const d = await floatSetSide(side.current);
      expandedX.current = d.expanded_x;
      collapsedX.current = d.collapsed_x;
      // 自己拖过高度就用自己那个（没拖过用 Rust 算的标准高度）
      y.current = dragY.current ?? d.y;
      await place(collapsedX.current, y.current);
      collapsed.current = true;
    } catch {
      // 读不到显示器信息就保持原来位置，不影响看内容
    }
  }, [place]);

  /** 滑到某个 x（收/展都用它） */
  const slideTo = useCallback(
    (target: number) => {
      if (anim.current !== null) cancelAnimationFrame(anim.current);
      const from = collapsed.current ? collapsedX.current : expandedX.current;
      const start = performance.now();
      const step = (now: number) => {
        const k = Math.min(1, (now - start) / SLIDE_MS);
        const ease = 1 - Math.pow(1 - k, 3);
        void place(from + (target - from) * ease, y.current);
        anim.current = k < 1 ? requestAnimationFrame(step) : null;
      };
      anim.current = requestAnimationFrame(step);
      collapsed.current = target === collapsedX.current;
    },
    [place],
  );

  const expand = useCallback(() => {
    if (hideTimer.current !== null) {
      window.clearTimeout(hideTimer.current);
      hideTimer.current = null;
    }
    if (!collapsed.current) return;
    slideTo(expandedX.current);
  }, [slideTo]);

  const hideLater = useCallback(
    (ms: number) => {
      // 按着 / 拖着的时候不能收：拖动中鼠标会跑出窗口，`mouseleave` 会误触发
      if (dragging.current || pressed.current) return;
      if (hideTimer.current !== null) window.clearTimeout(hideTimer.current);
      hideTimer.current = window.setTimeout(() => {
        hideTimer.current = null;
        slideTo(collapsedX.current);
      }, ms);
    },
    [slideTo],
  );

  /**
   * 拖完吸附到最近的那条边。
   *
   * 用窗口**中心**跟屏幕中心比：偏左就贴左边。竖向位置保留用户拖到的那个高度
   * （夹在屏幕里，别拖出屏幕外找不回来），下次启动还是这个高度。
   */
  const snap = useCallback(async () => {
    const win = getCurrentWindow();
    try {
      // 几何**全部**问 Rust（`floatDock` 用的是「主窗口所在那块屏」）：拖动过程中浮窗可能
      // 正好横跨两块缩放不同的屏，自己问 `currentMonitor()` 会拿到「这次刚好落上的那块」，
      // 于是尺寸/边界在半路变样（实测 268x236 与 402x354 来回跳）。
      const [pos, d0] = await Promise.all([win.outerPosition(), floatDock()]);
      const center = pos.x + d0.win_w / 2;
      const mid = (d0.mon_left + d0.mon_right) / 2;
      const want: FloatSide = center < mid ? "left" : "right";
      if (want !== side.current) {
        side.current = want;
        window.localStorage.setItem(SIDE_KEY, want);
      }
      // 夹一下：至少要留 8px 在屏幕里，不然拖出去就再也够不着了
      const lo = d0.mon_top + 8;
      const hi = d0.mon_bottom - d0.win_h - 8;
      dragY.current = Math.round(clamp(pos.y, Math.min(lo, hi), Math.max(lo, hi)));
      window.localStorage.setItem(DRAG_Y_KEY, String(dragY.current));
      // 不论换没换边都重新贴一次：拖完回到收起位，不然它会一直挡着
      const d = await floatSetSide(side.current);
      expandedX.current = d.expanded_x;
      collapsedX.current = d.collapsed_x;
      y.current = dragY.current;
      await place(collapsedX.current, y.current);
      collapsed.current = true;
    } catch {
      // 拿不到就保持现状
    }
  }, [place]);

  /** 重新贴边 + 拉数据 + 亮一会儿：初次挂载和 Rust 发 `float://show` 时都走它 */
  const appear = useCallback(() => {
    void dock().then(() => {
      void load();
      expand();
      hideLater(AFTER_SHOW_MS);
    });
  }, [dock, expand, hideLater, load]);

  useEffect(() => {
    appear();
    const poll = window.setInterval(() => void load(), POLL_MS);
    // Rust 每次把浮窗显示出来都会发这个事件（屏幕可能换了、分辨率也可能变了）
    const off = listen("float://show", () => appear());
    // 主窗口那边勾了 / 改了任务，这边跟着刷新（反过来也一样，见 `toggle`）
    const offTasks = watchTasks(() => void load());
    // 背景图 / 浓度是主窗口那边设的：它改完会 ping 过来
    const offPhoto = watchPhoto(refreshPhoto);
    return () => {
      window.clearInterval(poll);
      void off.then((un) => un());
      offTasks();
      offPhoto();
    };
  }, [appear, load, refreshPhoto]);

  /**
   * 手动拖动（不用 `data-tauri-drag-region`）。
   *
   * 为什么不用 Tauri 自带的拖动区：那是把窗口交给 Windows 的**原生移动循环**
   * （`WM_NCLBUTTONDOWN` + `HTCAPTION`）。对一个透明 + 无边框 + 置顶的窗口，
   * 这个循环会和 WebView 合成打架，拖起来一顿一顿的；鼠标跑得比窗口快时还会卡住。
   * 自己每帧摆位置反而跟手。
   *
   * 为什么不用事件里的 `e.screenX`：这台机器上 WebView 报的屏幕坐标是错的
   * （实测偏几百像素），所以光标坐标也问 Tauri（`cursorPosition`，物理像素）。
   */
  const dragStart = useCallback(
    async (e: React.PointerEvent<HTMLDivElement>) => {
      if (e.button !== 0) return;
      // 点在勾选框这类子按钮上：那是点击，不是拖动
      if ((e.target as HTMLElement).closest("button")) return;
      pressed.current = true;
      moved.current = false;
      dragging.current = false;
      // 抓住的一瞬间先停下动画：动画和拖动都在改 x，两个一起跑就是「一卡一卡」
      if (anim.current !== null) {
        cancelAnimationFrame(anim.current);
        anim.current = null;
      }
      // 只露着一条细边的时候拖很难对准，先直接摆到展开位（**不要**走动画，理由同上）
      if (collapsed.current) {
        collapsed.current = false;
        void place(expandedX.current, y.current);
      }
      // ⚠ 这个 handler 是 async：下面有 `await`，React 在 handler 返回后会把
      // 合成事件的 `currentTarget` 置成 null（之前就是在这里抛
      // `Cannot read properties of null (reading 'setPointerCapture')`，拖动直接断掉）。
      // 所以先把元素存下来，异步之后用存下来的引用。
      const el = e.currentTarget;
      const pointerId = e.pointerId;
      try {
        const [pos, cur] = await Promise.all([
          getCurrentWindow().outerPosition(),
          cursorPosition(),
        ]);
        dragFrom.current = { px: pos.x, py: pos.y, cx: cur.x, cy: cur.y };
      } catch {
        dragFrom.current = null;
      }
      // 指针捕获：鼠标滑出窗口时还能接着收到 pointermove，不然拖着拖着就断。
      // 指针已经抬起时 `setPointerCapture` 会抛：那就把按下状态也放掉，
      // 否则 `dragMove` 会继续按着旧起点摆窗口。
      try {
        el.setPointerCapture(pointerId);
      } catch {
        pressed.current = false;
        dragFrom.current = null;
      }
    },
    [place],
  );

  const dragMove = useCallback(async () => {
    const from = dragFrom.current;
    if (!pressed.current || from === null || moving.current) return;
    moving.current = true;
    try {
      const cur = await cursorPosition();
      const dx = cur.x - from.cx;
      const dy = cur.y - from.cy;
      if (!moved.current && Math.abs(dx) + Math.abs(dy) < DRAG_MIN_PX) return;
      moved.current = true;
      dragging.current = true;
      await place(from.px + dx, from.py + dy);
    } catch {
      // 读光标失败：这一帧不摆，下一帧再说
    } finally {
      moving.current = false;
    }
  }, [place]);

  const dragEnd = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      const wasPressed = pressed.current;
      pressed.current = false;
      dragFrom.current = null;
      if (!wasPressed) return;
      if (e.currentTarget.hasPointerCapture(e.pointerId)) {
        e.currentTarget.releasePointerCapture(e.pointerId);
      }
      if ((e.target as HTMLElement).closest("button")) return;
      if (dragging.current) {
        dragging.current = false;
        void snap();
      } else {
        // 没动 = 点一下：打开主窗口
        void showMain().catch(() => {});
      }
    },
    [snap],
  );

  /** 把主窗口叫出来，必要时再让主窗口干一件事（跨窗口「打开输入框 / 打开设置」） */
  const openMain = useCallback(async (after?: () => void) => {
    try {
      await showMain();
    } catch {
      // 叫不出来也不能因此不做后面的事（至少主窗口里手动点一下还能用）
    }
    after?.();
  }, []);

  /** 在浮窗里标 / 取消重点 */
  const toggleStar = useCallback(
    async (t: Task) => {
      try {
        await setTaskStar(t.id, !t.star);
        await load();
        pingTasks();
      } catch (e: unknown) {
        setErr(e instanceof Error ? e.message : String(e));
      }
    },
    [load],
  );

  /**
   * 从剪贴板新建一条任务（不脱敏、只取第一行）。
   *
   * 只取**第一行**：复制一整篇文章时，整段变一个标题并不好用；
   * 时间 / 重复照主窗口那套快捷录入解析（「明天九点交周报」）。
   * `remind_offsets` / `notify_channels` 传 `null` = 用默认的到点提醒、不限渠道。
   */
  const pasteNewTask = useCallback(async () => {
    let text = "";
    try {
      // 读剪贴板要窗口是聚焦的（右键点浮窗时它是聚焦的）；系统不给权限会在下面提示
      text = await navigator.clipboard.readText();
    } catch {
      say("读不到剪贴板，去主窗口手动粘");
      return;
    }
    const line = text
      .split(/\r?\n/)
      .map((s) => s.trim())
      .find((s) => s !== "");
    if (!line) {
      say("剪贴板里没有文字");
      return;
    }
    try {
      const parsed = parseQuick(line, todayRef.current, todayRef.current);
      const day = parsed.day ?? todayRef.current;
      await createTask({
        title: parsed.title.trim() || line,
        body: "",
        day,
        due_at_ms: msFromHm(day, parsed.hm ?? ""),
        repeat_rule: repeatToJson(parsed.repeat),
        remind_offsets: null,
        notify_channels: null,
      });
      await load();
      pingTasks();
      say(`已新建：${parsed.label || parsed.title}`);
      // 「明天九点交周报」落到明天了：浮窗只看今天，所以把主窗口叫出来看一眼
      if (day !== todayRef.current) void showMain().catch(() => {});
    } catch (e: unknown) {
      setErr(e instanceof Error ? e.message : String(e));
    }
  }, [load, say]);

  // 「最前面」会被别的程序抢走，隔一会儿重申一遍
  useEffect(() => {
    const win = getCurrentWindow();
    const t = window.setInterval(() => void win.setAlwaysOnTop(true).catch(() => {}), 15_000);
    return () => window.clearInterval(t);
  }, []);

  const total = tasks.length;
  const doneCount = tasks.filter((t) => t.done).length;
  // 已完成的也列出来：不然勾完就消失，想取消都点不到；多了让列表自己滚
  const shown = tasks.slice(0, MAX_ROWS);
  const more = total - shown.length;

  return (
    <div className="float" data-side={side.current}>
      <div
        className="float-card"
        title="点一下打开主窗口；左边小圈可以勾完成；右键有菜单；拖一下可以换边"
        onMouseEnter={expand}
        onMouseLeave={() => hideLater(AFTER_LEAVE_MS)}
        onPointerDown={(e) => void dragStart(e)}
        onPointerMove={() => void dragMove()}
        onPointerUp={dragEnd}
        onPointerCancel={dragEnd}
        onContextMenu={(e) => {
          // 空白处（条目以外）右键：只放「跟整个浮窗有关」的事。
          // 条目自己的菜单会 `stopPropagation`，不会跑到这里来
          e.preventDefault();
          setMenu({ x: e.clientX, y: e.clientY, task: null });
        }}
      >
        {/* 背景图铺在卡片里面（窗口本身是透明的，图不能铺到卡片外） */}
        {photo ? (
          <div
            className="backdrop"
            aria-hidden="true"
            style={{ backgroundImage: `url(${photo})`, opacity: photoOpacity }}
          />
        ) : null}
        <div className="float-head">
          <span className="float-title">今天</span>
          <span className="float-count">
            <b>{doneCount}</b>/{total} 已完成 · 全部 {stats?.all_total ?? 0} 条
          </span>
        </div>

        <div className="float-list">
          {shown.length === 0 ? (
            <div className="float-empty">今天没有安排</div>
          ) : (
            shown.map((t) => (
              <div
                key={t.id}
                className={
                  (t.done ? "float-item done" : "float-item") +
                  (t.star ? " starred" : "")
                }
                onContextMenu={(e) => {
                  e.preventDefault();
                  // 不让它冒到卡片那层，否则刚开的条目菜单会被空白菜单盖掉
                  e.stopPropagation();
                  setMenu({ x: e.clientX, y: e.clientY, task: t });
                }}
              >
                <button
                  type="button"
                  className={t.done ? "float-check on" : "float-check"}
                  title={
                    t.repeat_rule !== null
                      ? t.done
                        ? "取消「完成本次」"
                        : "完成本次（系列继续）"
                      : t.done
                        ? "标记为未完成"
                        : "标记为已完成"
                  }
                  // 两个都拦：不拦 pointerdown 卡片会当你按住要拖，不拦 click 会去打开主窗口
                  onPointerDown={(e) => e.stopPropagation()}
                  onClick={(e) => {
                    e.stopPropagation();
                    void toggle(t);
                  }}
                >
                  {t.done ? <IconCheck size={10} /> : null}
                </button>
                {/* 颜色小点：按任务固定分配，和主窗口左边那条色条同一个颜色。
                    完成的不再显示（主窗口那条色条也是这个规则），而且少一个元素
                    已完成的文字就和未完成的对齐了。 */}
                {t.done ? null : (
                  <span className="dot" style={{ background: taskColor(t.id) }} />
                )}
                <span className="t">{t.title}</span>
                {t.star ? <IconStar size={11} filled /> : null}
                <span className="hm">{hmFromMs(t.due_at_ms) || "—"}</span>
              </div>
            ))
          )}
          {more > 0 ? <div className="float-foot">还有 {more} 条…</div> : null}
        </div>

        <div className="float-foot">
          <span>今天 {stats?.today_total ?? 0} 条</span>
          <span>{note || (err ? "读取失败，点开看主窗口" : "点这里打开主窗口")}</span>
        </div>
      </div>
      {menu ? (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          onClose={() => setMenu(null)}
          items={
            menu.task
              ? floatMenu(
                  menu.task,
                  () => void toggle(menu.task as Task),
                  () => void toggleStar(menu.task as Task),
                )
              : blankMenu(
                  () => void pasteNewTask(),
                  (after) => void openMain(after),
                )
          }
        />
      ) : null}
    </div>
  );
}

/** 浮窗条目的右键菜单：就三件在浮窗里干得了的事 */
function floatMenu(t: Task, toggleDone: () => void, toggleStar: () => void): CtxItem[] {
  return [
    {
      label: t.done ? "取消完成" : "标记完成",
      onPick: toggleDone,
    },
    {
      label: t.star ? "取消重点" : "标记重点",
      onPick: toggleStar,
    },
    {
      label: "打开主窗口",
      onPick: () => void showMain().catch(() => {}),
      separatorBefore: true,
    },
  ];
}

/**
 * 浮窗空白处的右键菜单。
 *
 * 只放四件事：浮窗自己很窄（268×236），菜单项再多就会被窗口切掉；
 * 需要的动作先 `showMain()` 把主窗口叫出来，再用 `ping*` 告诉它「打开输入框 / 打开设置」
 * （两个窗口同一个 origin，`localStorage` 广播比加一层 Rust 中转便宜）。
 */
function blankMenu(
  pasteNewTask: () => void,
  openMain: (after?: () => void) => void,
): CtxItem[] {
  return [
    { label: "添加任务（打开主窗口写）", onPick: () => openMain(() => pingFocusComposer()) },
    { label: "从剪贴板新建任务", onPick: pasteNewTask },
    { label: "打开主窗口", onPick: () => openMain() },
    { label: "设置（打开主窗口）", onPick: () => openMain(() => pingOpenSettings()) },
  ];
}
