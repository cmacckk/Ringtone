import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { IconAuto, IconCalendar, IconClose, IconFloat, IconMinus, IconMoon, IconPin, IconSun, IconType } from "./icons";
import { THEME_LABEL, Theme, applyTheme, loadTheme, nextTheme, saveTheme } from "./theme";
import { floatVisible, toggleFloat } from "./api";

const PIN_KEY = "ringtone.alwaysOnTop";

/**
 * 自绘标题栏。
 *
 * `tauri.conf.json` 里设了 `decorations: false`，系统那条灰带没有了，
 * 于是拖动 / 置顶 / 最小化 / 关闭都得自己接：
 * - **拖动**：靠 `data-tauri-drag-region`，Tauri 自己监听鼠标并调 `startDragging`，
 *   不需要我们写 JS（也因此要给窗口 `core:window:allow-start-dragging` 权限）。
 *   注意只有**带这个属性**的元素能拖，子元素默认不参与。
 * - **关闭**：调 `window.close()` → Rust 侧 `CloseRequested` 把它改成「隐藏到托盘」，
 *   所以点 ✕ 不退出程序，调度线程继续跑。
 * - **外观**：字体 / 字号 / 透明度那面板（`AppearancePanel.tsx`）由 `App.tsx` 全屏渲染，
 *   标题栏只负责给个入口（`onAppearance`）；月总览（`Overview.tsx`）同理（`onOverview`）。
 */
export default function TitleBar({
  onAppearance,
  onOverview,
}: {
  onAppearance?: () => void;
  onOverview?: () => void;
}) {
  const [pinned, setPinned] = useState(false);
  /** 浮窗现在是不是显着的（按钮据此画开关状态） */
  const [floatOn, setFloatOn] = useState(false);
  // 主题三态：跟随系统 → 亮色 → 深色 → 跟随系统。
  // 挂到 `<html data-theme>` 上由 CSS 接管，这里只记住选择（main.tsx 启动时已先挂过一次）
  const [theme, setTheme] = useState<Theme>(() => loadTheme());

  function cycleTheme() {
    const next = nextTheme(theme);
    setTheme(next);
    applyTheme(next);
    saveTheme(next);
  }

  // 置顶记在本机：重启后还要保持，否则每次开机都得手点一下
  useEffect(() => {
    if (window.localStorage.getItem(PIN_KEY) !== "1") return;
    setPinned(true);
    // 失败就退回未置顶，别让按钮显示一个假状态
    void getCurrentWindow()
      .setAlwaysOnTop(true)
      .catch(() => setPinned(false));
  }, []);

  async function togglePin() {
    const next = !pinned;
    setPinned(next);
    try {
      await getCurrentWindow().setAlwaysOnTop(next);
      window.localStorage.setItem(PIN_KEY, next ? "1" : "0");
    } catch {
      setPinned(!next);
    }
  }

  // 浮窗可见状态：挂载时问一次就行，之后靠自己的点击结果维护
  useEffect(() => {
    void floatVisible()
      .then(setFloatOn)
      .catch(() => {});
  }, []);

  async function toggleFloatWin() {
    try {
      setFloatOn(await toggleFloat());
    } catch {
      // 切不了（比如窗口还没建出来）就保持原样
    }
  }

  return (
    <div className="titlebar" data-tauri-drag-region>
      <span className="brand" data-tauri-drag-region>
        Ringtone
      </span>
      <span className="spacer" data-tauri-drag-region />
      {onOverview ? (
        <button
          type="button"
          className="win-btn"
          title="月总览：看这个月的整体情况"
          onClick={onOverview}
        >
          <IconCalendar />
        </button>
      ) : null}
      <button
        type="button"
        className="win-btn"
        title={`主题：${THEME_LABEL[theme]}（点一下换下一个）`}
        onClick={cycleTheme}
      >
        {theme === "auto" ? <IconAuto /> : theme === "light" ? <IconSun /> : <IconMoon />}
      </button>
      {onAppearance ? (
        <button
          type="button"
          className="win-btn"
          title="外观：字体 / 字号 / 窗口透明度"
          onClick={onAppearance}
        >
          <IconType />
        </button>
      ) : null}
      <button
        type="button"
        className={floatOn ? "win-btn on" : "win-btn"}
        title={floatOn ? "隐藏浮窗" : "显示浮窗（贴在最前面，鼠标移过去展开）"}
        onClick={() => void toggleFloatWin()}
      >
        <IconFloat />
      </button>
      <button
        type="button"
        className={pinned ? "win-btn on" : "win-btn"}
        title={pinned ? "取消置顶" : "置顶显示"}
        onClick={() => void togglePin()}
      >
        <IconPin />
      </button>
      <button
        type="button"
        className="win-btn"
        title="最小化"
        onClick={() => void getCurrentWindow().minimize()}
      >
        <IconMinus />
      </button>
      <button
        type="button"
        className="win-btn close"
        title="隐藏到托盘（不退出，到点照样提醒）"
        onClick={() => void getCurrentWindow().close()}
      >
        <IconClose />
      </button>
    </div>
  );
}
