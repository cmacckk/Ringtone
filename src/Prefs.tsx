/**
 * 通用设置（设置面板的第二个页签）。
 *
 * 四块：一键显示浮窗的快捷键、导入导出、上下班提醒、窗口背景图。
 * 为什么和「推送渠道」分两个页签而不是塞一张长页：渠道是配置类（配完就不动），
 * 这四块是使用类（会反复调），混在一起两边都难找。
 */
import { useCallback, useEffect, useState } from "react";
import {
  HolidayInfo,
  ImportSummary,
  MouseButton,
  WorkSchedule,
  WorkStatus,
  backgroundClear,
  backgroundPick,
  exportTasks,
  floatShow,
  getFloatHotkey,
  getMouseButton,
  holidayInfo,
  importTasks,
  setFloatHotkey,
  setMouseButton,
  workGet,
  workSave,
  workStatus,
} from "./api";
import { humanSpan } from "./day";
import {
  PHOTO_OPACITY_DEFAULT,
  PhotoOpacity,
  loadPhoto,
  loadPhotoOpacity,
  pingPhoto,
  pickOpacity,
  savePhotoOpacity,
} from "./photo";

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** 没读到时先用这套（和 Rust 侧 `WorkSchedule::default()` 一致，免得界面闪一下别人的默认值） */
const BLANK_SCHEDULE: WorkSchedule = {
  enabled: false,
  morning_in: "09:00",
  morning_out: "12:00",
  afternoon_in: "14:00",
  afternoon_out: "18:00",
  advance_min: 5,
};

/** 快捷键的键名映射：浏览器叫 ` ` 和 `ArrowUp`，Tauri 只认 `Space` / `Up` */
const KEY_ALIAS: Record<string, string> = {
  " ": "Space",
  ArrowUp: "Up",
  ArrowDown: "Down",
  ArrowLeft: "Left",
  ArrowRight: "Right",
};

/**
 * 键盘事件 → 加速键字符串（`Ctrl+Alt+F`）。
 *
 * **必须带至少一个修饰键**：不带的话用户一敲字母就被全局抢走，正常打字全废。
 * 只按修饰键的过程返回 `null`（还在组合中）。
 */
export function accelFromEvent(e: KeyboardEvent): string | null {
  if (["Control", "Alt", "Shift", "Meta"].includes(e.key)) return null;
  const parts: string[] = [];
  if (e.ctrlKey) parts.push("Ctrl");
  if (e.altKey) parts.push("Alt");
  if (e.shiftKey) parts.push("Shift");
  if (e.metaKey) parts.push("Super");
  if (parts.length === 0) return null;
  const key = KEY_ALIAS[e.key] ?? (e.key.length === 1 ? e.key.toUpperCase() : e.key);
  if (!key) return null;
  parts.push(key);
  return parts.join("+");
}

export default function Prefs({ onChanged }: { onChanged?: () => void }) {
  const [err, setErr] = useState("");
  const [ok, setOk] = useState("");

  const [hotkey, setHotkey] = useState("");
  const [recording, setRecording] = useState(false);

  const [schedule, setSchedule] = useState<WorkSchedule>(BLANK_SCHEDULE);
  const [status, setStatus] = useState<WorkStatus | null>(null);

  const [photos, setPhotos] = useState<{ main: string | null; float: string | null }>({
    main: null,
    float: null,
  });
  const [opacity, setOpacity] = useState<PhotoOpacity>(() => loadPhotoOpacity());

  const [busy, setBusy] = useState(false);
  const [imported, setImported] = useState<ImportSummary | null>(null);
  /** 鼠标侧键：`off` / `x1` / `x2` */
  const [mouse, setMouse] = useState<MouseButton>("off");
  /** 节假日数据覆盖到哪几年（用来说清「跳过节假日」到底算不算得上） */
  const [holiday, setHoliday] = useState<HolidayInfo | null>(null);

  // 初始化：快捷键（含鼠标侧键）、上下班设置、背景图
  useEffect(() => {
    void (async () => {
      try {
        const [hk, w, st, mb, hd] = await Promise.all([
          getFloatHotkey(),
          workGet(),
          workStatus(),
          getMouseButton(),
          holidayInfo(),
        ]);
        setHotkey(hk);
        setSchedule(w);
        setStatus(st);
        setMouse((mb === "x1" || mb === "x2" ? mb : "off") as MouseButton);
        setHoliday(hd);
      } catch (e) {
        setErr(message(e));
      }
      setPhotos({ main: await loadPhoto("main"), float: await loadPhoto("float") });
    })();
  }, []);

  // 录制快捷键：录制期间把键盘全吃掉，免得顺手触发了别的按钮
  useEffect(() => {
    if (!recording) return;
    function onKeyDown(e: KeyboardEvent) {
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "Escape") {
        setRecording(false);
        return;
      }
      const accel = accelFromEvent(e);
      if (!accel) return; // 还在按修饰键
      setRecording(false);
      void applyHotkey(accel);
    }
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
    // applyHotkey 只是用 setState，不需要进依赖
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [recording]);

  async function applyHotkey(accel: string) {
    setErr("");
    setOk("");
    try {
      const applied = await setFloatHotkey(accel);
      setHotkey(applied);
      setOk(
        applied
          ? `快捷键已设为 ${applied}（现在按它就能把浮窗叫出来）`
          : "快捷键已清除（浮窗可以用托盘菜单或最小化主窗口叫出来）",
      );
    } catch (e) {
      setErr(message(e));
    }
  }

  async function applyMouse(button: MouseButton) {
    setErr("");
    setOk("");
    const before = mouse;
    setMouse(button);
    try {
      const applied = await setMouseButton(button);
      setMouse((applied === "x1" || applied === "x2" ? applied : "off") as MouseButton);
      setOk(
        applied === "off"
          ? "鼠标侧键已关（侧键恢复成浏览器的前进 / 后退）"
          : `鼠标侧键已设为${applied === "x1" ? "侧键 4" : "侧键 5"}（全局生效）`,
      );
    } catch (e) {
      // 装不上钩子（极少见，被安全软件拦）就退回原来那个，别显示假状态
      setMouse(before);
      setErr(message(e));
    }
  }

  async function saveSchedule(next: WorkSchedule) {    setErr("");
    setOk("");
    setBusy(true);
    try {
      const st = await workSave(next);
      setSchedule(next);
      setStatus(st);
      setOk(
        next.enabled
          ? `已保存：${next.morning_in} 上班 · ${next.morning_out} 下班 · ${next.afternoon_in} 上班 · ${next.afternoon_out} 下班`
          : "已关闭上下班提醒",
      );
      onChanged?.();
    } catch (e) {
      setErr(message(e));
    } finally {
      setBusy(false);
    }
  }

  async function pickPhoto(which: "main" | "float") {
    setErr("");
    setOk("");
    setBusy(true);
    try {
      const url = await backgroundPick(which);
      if (url === null) return; // 用户取消
      setPhotos((p) => ({ ...p, [which]: url }));
      pingPhoto();
      setOk(which === "main" ? "主窗口背景已经换好了" : "浮窗背景已经换好了");
    } catch (e) {
      setErr(message(e));
    } finally {
      setBusy(false);
    }
  }

  async function clearPhoto(which: "main" | "float") {
    setErr("");
    setOk("");
    setBusy(true);
    try {
      await backgroundClear(which);
      setPhotos((p) => ({ ...p, [which]: null }));
      pingPhoto();
      setOk(which === "main" ? "主窗口背景已恢复默认" : "浮窗背景已恢复默认");
    } catch (e) {
      setErr(message(e));
    } finally {
      setBusy(false);
    }
  }

  function changeOpacity(which: "main" | "float", v: number) {
    const next = { ...opacity, [which]: pickOpacity(v) };
    setOpacity(next);
    savePhotoOpacity(next);
    pingPhoto(); // 另一个窗口（浮窗）跟着变
  }

  const doExport = useCallback(async (kind: "md" | "csv") => {
    setErr("");
    setOk("");
    setBusy(true);
    try {
      const r = await exportTasks(kind);
      if (r === null) return; // 用户取消
      setOk(`已导出 ${r.tasks} 条到 ${r.path}（${Math.round(r.bytes / 1024)} KB）`);
    } catch (e) {
      setErr(message(e));
    } finally {
      setBusy(false);
    }
  }, []);

  async function doImport() {
    setErr("");
    setOk("");
    setBusy(true);
    try {
      const r = await importTasks();
      if (r === null) return;
      setImported(r);
      setOk(`导入完成：新增 ${r.added} 条，跳过 ${r.skipped} 条（同一天已经有同名任务）`);
      onChanged?.();
    } catch (e) {
      setErr(message(e));
    } finally {
      setBusy(false);
    }
  }

  const range = `${schedule.morning_in}–${schedule.morning_out} · ${schedule.afternoon_in}–${schedule.afternoon_out}`;

  return (
    <>
      {err ? <div className="error">{err}</div> : null}
      {ok ? <div className="ok-line">{ok}</div> : null}

      <div className="card">
        <div className="card-head">
          <strong>快捷键</strong>
          <span className="muted">一键把浮窗叫出来</span>
        </div>
        <div className="row">
          <span className="hotkey-box">{hotkey || "还没设快捷键"}</span>
          <button
            type="button"
            className={recording ? "primary" : ""}
            disabled={busy}
            onClick={() => {
              setRecording(true);
              setErr("");
              setOk("现在按下组合键（比如 Ctrl+Alt+F）；按 Esc 取消");
            }}
          >
            {recording ? "请按下组合键…" : "录制快捷键"}
          </button>
          <button type="button" disabled={busy || !hotkey} onClick={() => void applyHotkey("")}>
            清除
          </button>
          <button
            type="button"
            disabled={busy}
            onClick={() => void floatShow().catch((e: unknown) => setErr(message(e)))}
          >
            试一下
          </button>
        </div>
        <div className="hint">
          全局生效：不聚焦这个窗口也能按（切到别的程序里照样弹浮窗）。必须带 Ctrl / Alt
          这类修饰键——否则会把你正常打字的按键抢走。
        </div>
        <div className="row">
          <span className="appearance-name">鼠标侧键</span>
          <select
            value={mouse}
            disabled={busy}
            onChange={(e) => void applyMouse(e.target.value as MouseButton)}
          >
            <option value="off">不用</option>
            <option value="x1">侧键 4（常见的「后退」）</option>
            <option value="x2">侧键 5（「前进」）</option>
          </select>
        </div>
        <div className="hint">
          键盘之外也可以只用鼠标侧键（侧键不用配 Ctrl）。设上之后它在「全局」用来叫浮窗，
          也就是再也不触发浏览器的前进 / 后退了；不想用就选「不用」。
        </div>
      </div>

      <div className="card">
        <div className="card-head">
          <strong>导入 / 导出</strong>
          <span className="muted">备份、换机器、丢进笔记软件</span>
        </div>
        <div className="row">
          <button type="button" disabled={busy} onClick={() => void doExport("md")}>
            导出 Markdown
          </button>
          <button type="button" disabled={busy} onClick={() => void doExport("csv")}>
            导出 CSV
          </button>
          <button type="button" className="primary" disabled={busy} onClick={() => void doImport()}>
            导入文件…
          </button>
        </div>
        {imported && imported.failed.length > 0 ? (
          <div className="hint">
            有 {imported.failed.length} 条没进来：
            <ul className="fail-list">
              {imported.failed.map((f) => (
                <li key={f}>{f}</li>
              ))}
            </ul>
          </div>
        ) : null}
        <div className="hint">
          导出的是「全部」待办（含已完成的，不含上下班那种隐藏提醒）。
          Markdown 是给人看的（`- [ ] 09:30 交周报`）；CSV 用 Excel 打开不乱码，而且带重复规则，
          拿去再导入能原样还原。导入「只新增不覆盖」：同一天、同标题的会跳过。
        </div>
      </div>

      <div className="card">
        <div className="card-head">
          <strong>上下班提醒</strong>
          <span className="muted">
            {schedule.enabled ? range : "未开启"}
            {status?.enabled && status.next_label
              ? ` · 距离${status.next_label} ${humanSpan(Math.round(status.left_ms / 1000))}`
              : ""}
          </span>
        </div>
        <label className="row">
          <input
            type="checkbox"
            checked={schedule.enabled}
            onChange={(e) => void saveSchedule({ ...schedule, enabled: e.target.checked })}
          />
          <span>开启上下班提醒</span>
        </label>
        <div className="form">
          <label>
            <span>上午上班</span>
            <input
              type="time"
              value={schedule.morning_in}
              onChange={(e) => setSchedule({ ...schedule, morning_in: e.target.value })}
            />
          </label>
          <label>
            <span>上午下班</span>
            <input
              type="time"
              value={schedule.morning_out}
              onChange={(e) => setSchedule({ ...schedule, morning_out: e.target.value })}
            />
          </label>
          <label>
            <span>下午上班</span>
            <input
              type="time"
              value={schedule.afternoon_in}
              onChange={(e) => setSchedule({ ...schedule, afternoon_in: e.target.value })}
            />
          </label>
          <label>
            <span>下午下班</span>
            <input
              type="time"
              value={schedule.afternoon_out}
              onChange={(e) => setSchedule({ ...schedule, afternoon_out: e.target.value })}
            />
          </label>
          <label>
            <span>提前提醒（分钟）</span>
            <input
              type="number"
              min={0}
              max={720}
              value={schedule.advance_min}
              onChange={(e) =>
                setSchedule({ ...schedule, advance_min: Number(e.target.value) || 0 })
              }
            />
          </label>
        </div>
        <div className="row">
          <button
            type="button"
            className="primary"
            disabled={busy}
            onClick={() => void saveSchedule(schedule)}
          >
            保存并启用
          </button>
          <button
            type="button"
            disabled={busy || !schedule.enabled}
            onClick={() => void saveSchedule({ ...schedule, enabled: false })}
          >
            关掉
          </button>
        </div>
        <div className="hint">
          到点后会像普通待办一样推送到你配的渠道（钉钉 / 飞书），也能提前 N 分钟提醒。
          它是「隐藏任务」：不会出现在列表和计数里，但要改时间就回这儿改。只在工作日响：
          法定节假日不响，调休补班的周六日照响
          {holiday
            ? `（节假日数据：${holiday.covered}${holiday.this_year ? "" : "，今年没有数据，会退化成只看周一到周五"}）。`
            : "。"}
        </div>
      </div>

      <div className="card">
        <div className="card-head">
          <strong>背景图片</strong>
          <span className="muted">两个窗口各一张</span>
        </div>
        {(["main", "float"] as const).map((which) => (
          <div className="photo-row" key={which}>
            <div
              className="photo-thumb"
              style={
                photos[which]
                  ? { backgroundImage: `url(${photos[which]})`, opacity: opacity[which] }
                  : undefined
              }
            >
              {photos[which] ? null : "未设置"}
            </div>
            <div className="photo-side">
              <div className="muted">{which === "main" ? "主窗口" : "浮窗"}</div>
              <div className="row">
                <button type="button" disabled={busy} onClick={() => void pickPhoto(which)}>
                  选择图片
                </button>
                <button
                  type="button"
                  disabled={busy || !photos[which]}
                  onClick={() => void clearPhoto(which)}
                >
                  移除
                </button>
              </div>
              <label className="row">
                <span className="muted">浓度</span>
                <input
                  className="slider"
                  type="range"
                  min={10}
                  max={100}
                  step={5}
                  value={Math.round(opacity[which] * 100)}
                  onChange={(e) => changeOpacity(which, Number(e.target.value) / 100)}
                />
                <span className="muted">{Math.round(opacity[which] * 100)}%</span>
              </label>
            </div>
          </div>
        ))}
        <div className="hint">
          选中的图片会「复制」进应用自己的目录（删掉桌面那张、拔掉 U 盘都不影响）。
          浓度是背景图自己的透明度：调低更淡（字更清楚），
          {PHOTO_OPACITY_DEFAULT === 0.5 ? "默认 50%" : "默认一般"}。支持 png / jpg / webp / gif / bmp。
        </div>
      </div>
    </>
  );
}
