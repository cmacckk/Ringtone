/**
 * 主题：跟随系统 / 强制亮色 / 强制深色。
 *
 * 三态而不是开关：用户可能既想「平时跟着系统走」，又想临时钉死一边。
 * 颜色本身全在 `styles.css` 里（`:root` = 深色，`[data-theme="light"]` = 亮色），
 * 这里只负责在 `<html>` 上挂一个 `data-theme` 并记到本机。
 */
export type Theme = "auto" | "light" | "dark";

const KEY = "ringtone.theme";

export const THEME_LABEL: Record<Theme, string> = {
  auto: "跟随系统",
  light: "亮色",
  dark: "深色",
};

/** 读取上次的选择；没存过或存坏了都当「跟随系统」 */
export function loadTheme(): Theme {
  const raw = window.localStorage.getItem(KEY);
  return raw === "light" || raw === "dark" ? raw : "auto";
}

/** 挂到 `<html>` 上——CSS 选择器靠这个属性切换 */
export function applyTheme(theme: Theme): void {
  document.documentElement.dataset.theme = theme;
}

export function saveTheme(theme: Theme): void {
  window.localStorage.setItem(KEY, theme);
}

/**
 * 别的窗口改了主题时同步过来。
 *
 * 主窗口和浮窗是两个独立页面：主窗口点主题按钮只改自己那一份，浮窗不会知道，
 * 会永远停在它加载时那个主题（实测过：主窗口已经是深色，浮窗还是亮的，得重启才跟上）。
 */
export function watchTheme(cb: (theme: Theme) => void): () => void {
  function onStorage(e: StorageEvent) {
    if (e.key === KEY || e.key === null) cb(loadTheme());
  }
  window.addEventListener("storage", onStorage);
  return () => window.removeEventListener("storage", onStorage);
}

/** 点一下换下一个：跟随系统 → 亮色 → 深色 → 跟随系统 */
export function nextTheme(theme: Theme): Theme {
  if (theme === "auto") return "light";
  if (theme === "light") return "dark";
  return "auto";
}
