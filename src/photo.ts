/**
 * 窗口背景图（前端侧）。
 *
 * 图片本体在后端（复制进应用数据目录了），这里只管两件事：
 * 1. 拉 data URL 出来给 `.backdrop` 用；
 * 2. 「照片浓度」——它不是窗口透明度，而是背景图自己的 `opacity`。
 *    默认 50%：再高会盖住待办的字，再低就等于没设。
 */
import { backgroundGet, type BackgroundTarget } from "./api";
import { pingPhoto, watchPhoto } from "./sync";

/** `main` / `float` → 各自一张 */
export type PhotoOpacity = { main: number; float: number };

export const PHOTO_OPACITY_DEFAULT = 0.5;
const OPACITY_KEY = "ringtone.photoOpacity";

/** 拿到的值必须是 0.1~1 之间的有限数；否则回默认（`NaN` 会被夹成下界，所以要单独挡一道） */
export function pickOpacity(v: unknown): number {
  if (typeof v !== "number" || !Number.isFinite(v)) return PHOTO_OPACITY_DEFAULT;
  return Math.min(1, Math.max(0.1, v));
}

export function loadPhotoOpacity(): PhotoOpacity {
  try {
    const raw = window.localStorage.getItem(OPACITY_KEY);
    if (!raw) return { main: PHOTO_OPACITY_DEFAULT, float: PHOTO_OPACITY_DEFAULT };
    const o = JSON.parse(raw) as Partial<PhotoOpacity>;
    return { main: pickOpacity(o.main), float: pickOpacity(o.float) };
  } catch {
    return { main: PHOTO_OPACITY_DEFAULT, float: PHOTO_OPACITY_DEFAULT };
  }
}

export function savePhotoOpacity(p: PhotoOpacity): void {
  try {
    window.localStorage.setItem(OPACITY_KEY, JSON.stringify(p));
  } catch {
    // 存不进去只影响下次启动，不影响当前窗口
  }
}

/** 这个窗口的背景图；没设置（或读不到）返回 `null` */
export async function loadPhoto(which: BackgroundTarget): Promise<string | null> {
  try {
    return await backgroundGet(which);
  } catch {
    // 后端还没起来 / 数据库锁着：当成没背景图，不要弹错误把界面弄脏
    return null;
  }
}

export { pingPhoto, watchPhoto };
