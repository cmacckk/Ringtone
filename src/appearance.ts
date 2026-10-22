/**
 * 外观设置：字体、字号、窗口透明度。
 *
 * 存 `localStorage`（键 `ringtone.appearance`）。主窗口和浮窗是**同一个 origin**，
 * 所以两个窗口共用一份设置；另一个窗口里的改动靠 `storage` 事件同步过来
 * （`storage` 只在**别的**文档里触发，自己改的自己知道），不需要后端参与。
 *
 * 三个值都落到 `<html>` 的行内 CSS 变量上：
 *  - `--fs-k`：字号倍率。`styles.css` 里所有 `font-size` 都写成 `calc(Npx * var(--fs-k, 1))`，
 *    所以改这一个数，全界面文字一起缩放，而**不改变**间距 / 盒子尺寸（这是「字号」而不是「整体缩放」）。
 *  - `--font-family`：字体栈。
 *  - `--win-opacity`：**当前窗口**的透明度。窗口在 Rust 侧建成了 `transparent(true)`，
 *    所以 `body { opacity: var(--win-opacity) }` 能真的透出桌面，而不只是把界面涂灰。
 */
export type Appearance = {
  /** 字体栈；正常不会是空串，默认见 `SYSTEM_FONT` */
  fontFamily: string;
  /** 字号基准（px）；`APPEARANCE_DEFAULTS.fontScale` 相当于 100% */
  fontScale: number;
  /** 主窗口透明度，`OPACITY_MIN`..1 */
  winOpacity: number;
  /** 浮窗透明度，`OPACITY_MIN`..1 */
  floatOpacity: number;
  /** 浮窗自己的字体；`null` = 跟主窗口一样 */
  floatFontFamily: string | null;
  /** 浮窗自己的字号（px）；`null` = 跟主窗口一样 */
  floatFontScale: number | null;
  /**
   * 任务配色：给每条任务按 id 分一个低饱和色（主窗口左侧细条 / 浮窗小圆点）。
   *
   * 关掉后只剩「重点」（星标）与「紧急」（快到点 / 已过期）两种强调，界面更素。
   */
  taskColors: boolean;
};

/** 默认字体栈：中英文都能落到系统里比较顺眼的字体上 */
export const SYSTEM_FONT =
  'system-ui, -apple-system, "Segoe UI", "Microsoft YaHei", "PingFang SC", sans-serif';

export const APPEARANCE_KEY = "ringtone.appearance";

export const FONT_MIN = 12;
export const FONT_MAX = 20;
export const OPACITY_MIN = 0.5;

export const APPEARANCE_DEFAULTS: Appearance = {
  fontFamily: SYSTEM_FONT,
  fontScale: 14,
  winOpacity: 1,
  floatOpacity: 1,
  floatFontFamily: null,
  floatFontScale: null,
  taskColors: true,
};

/**
 * 字体名 → CSS 的 `font-family` 值。
 *
 * 存下来的是**单个字体家族名**（如 `Microsoft YaHei`、`楷体`），写进 CSS 时补引号 +
 * 系统栈做回退：字体被卸了也不至于变成浏览器默认的那种弱字体。
 * 老配置里可能存的是整段栈（带逗号，如 `"Microsoft YaHei", sans-serif`）——那种原样用。
 */
export function fontCss(family: string): string {
  const value = family.trim();
  if (value === "" || value === SYSTEM_FONT || value.includes(",")) return value || SYSTEM_FONT;
  return `"${value}", ${SYSTEM_FONT}`;
}

/** 读不到系统字体列表时的兜底（正常的全量列表来自后端 `list_fonts` 命令） */
export const FONT_FALLBACK: string[] = [
  "Microsoft YaHei",
  "微软雅黑",
  "DengXian",
  "Source Han Sans SC",
  "SimSun",
  "KaiTi",
];

function clamp(n: number, lo: number, hi: number): number {
  if (!Number.isFinite(n)) return lo;
  return Math.min(hi, Math.max(lo, n));
}

/**
 * 从存下来的值里取一个数：没存 / 存坏了就回落到默认值，存了但越界就夹回区间。
 *
 * 注意不能写成 `clamp(Number(v), lo, hi)`——`Number(undefined)` 是 `NaN`，
 * `clamp` 会把 `NaN` 当成「非有限数」返回下界，于是「没有设置」就变成
 * 「字号最小 + 窗口最透明」（真实踩过：第一次打开是半透明的 12px 小字）。
 */
function pick(v: unknown, fallback: number, lo: number, hi: number): number {
  if (typeof v !== "number" || !Number.isFinite(v)) return fallback;
  return clamp(v, lo, hi);
}

/** 读设置。缺字段 / 脏数据一律回默认值，数值夹到合法区间（用户手改 localStorage 也炸不了） */
export function loadAppearance(): Appearance {
  let raw: unknown = null;
  try {
    const text = window.localStorage.getItem(APPEARANCE_KEY);
    raw = text ? JSON.parse(text) : null;
  } catch {
    raw = null;
  }
  const o = (raw ?? {}) as Partial<Appearance>;
  const family =
    typeof o.fontFamily === "string" && o.fontFamily.trim() !== ""
      ? o.fontFamily
      : APPEARANCE_DEFAULTS.fontFamily;
  // 浮窗那两条可以为 `null`（= 跟主窗口一样），所以不能用 `pick`（它会把缺失值变成基准字号）
  const floatScale =
    typeof o.floatFontScale === "number" && Number.isFinite(o.floatFontScale)
      ? clamp(o.floatFontScale, FONT_MIN, FONT_MAX)
      : null;
  const floatFamily =
    typeof o.floatFontFamily === "string" && o.floatFontFamily.trim() !== ""
      ? o.floatFontFamily
      : null;
  return {
    fontFamily: family,
    fontScale: pick(o.fontScale, APPEARANCE_DEFAULTS.fontScale, FONT_MIN, FONT_MAX),
    winOpacity: pick(o.winOpacity, APPEARANCE_DEFAULTS.winOpacity, OPACITY_MIN, 1),
    floatOpacity: pick(o.floatOpacity, APPEARANCE_DEFAULTS.floatOpacity, OPACITY_MIN, 1),
    floatFontFamily: floatFamily,
    floatFontScale: floatScale,
    // 老配置里没这个字段（`undefined`）→ 默认开着
    taskColors: o.taskColors !== false,
  };
}

/**
 * 本窗口改完外观后发的事件。
 *
 * 为什么不能只靠 `storage`：那个事件**只在别的文档**里触发，所以同一个窗口里
 * 「拖完浓度自己不变」过（背景图那轮踩过同样的坑，见 `sync.ts`）。
 */
const APPEARANCE_LOCAL_EVENT = "ringtone:appearance-changed";

export function saveAppearance(a: Appearance): void {
  window.localStorage.setItem(APPEARANCE_KEY, JSON.stringify(a));
  window.dispatchEvent(new Event(APPEARANCE_LOCAL_EVENT));
}

/**
 * 把设置落到 CSS 变量上。
 *
 * `which` 决定用哪一份字体/字号/透明度：浮窗页面传 `"float"`，其它窗口（含主窗口）传 `"main"`。
 * 主窗口和浮窗可以**显示不同的透明度**（字号/字体也能分开），所以不能把两条都写上去——
 * `--win-opacity` 是「当前窗口」的意思，谁在用谁写自己的那一份。
 * 浮窗的字体/字号没设过（`null`）就沿用主窗口那套。
 *
 * 主题（`data-theme`）不在这里：浮窗永远跟主窗口一致，见 `theme.ts` 的 `watchTheme`。
 */
export function applyAppearance(a: Appearance, which: "main" | "float" = "main"): void {
  const root = document.documentElement;
  const isFloat = which === "float";
  const scale = (isFloat ? a.floatFontScale : null) ?? a.fontScale;
  const family = (isFloat ? a.floatFontFamily : null) ?? a.fontFamily;
  root.style.setProperty("--fs-k", String(scale / APPEARANCE_DEFAULTS.fontScale));
  root.style.setProperty("--font-family", fontCss(family));
  root.style.setProperty(
    "--win-opacity",
    String(isFloat ? a.floatOpacity : a.winOpacity),
  );
}

/** 另一个窗口改了设置时同步过来；返回取消订阅的函数 */
export function watchAppearance(cb: (a: Appearance) => void): () => void {
  function onStorage(e: StorageEvent) {
    if (e.key === APPEARANCE_KEY) cb(loadAppearance());
  }
  // 同窗口的直接改（比如主窗口自己改完任务配色）也要能收到
  function onLocal() {
    cb(loadAppearance());
  }
  window.addEventListener("storage", onStorage);
  window.addEventListener(APPEARANCE_LOCAL_EVENT, onLocal);
  return () => {
    window.removeEventListener("storage", onStorage);
    window.removeEventListener(APPEARANCE_LOCAL_EVENT, onLocal);
  };
}
