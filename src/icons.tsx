/**
 * 内联 SVG 图标。
 *
 * 为什么不用 emoji：同一个 emoji 在 Win10/Win11/不同字体下的形状、基线、颜色都不一样，
 * 而且在 `<button>` 里会被当成文字参与行高计算。也不用图标库：为了十来个图标多背一个依赖不值。
 */
import type { ReactNode } from "react";

function Svg({ size, children }: { size: number; children: ReactNode }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.9}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      {children}
    </svg>
  );
}

type IconProps = { size?: number };

/** 勾选标记（画在自绘的圆形勾选框里） */
export function IconCheck({ size = 14 }: IconProps) {
  return (
    <Svg size={size}>
      <path d="M20 6 9 17l-5-5" strokeWidth={2.6} />
    </Svg>
  );
}

export function IconPlus({ size = 16 }: IconProps) {
  return (
    <Svg size={size}>
      <path d="M12 5v14M5 12h14" />
    </Svg>
  );
}

export function IconChevronLeft({ size = 18 }: IconProps) {
  return (
    <Svg size={size}>
      <path d="m15 18-6-6 6-6" />
    </Svg>
  );
}

export function IconChevronRight({ size = 18 }: IconProps) {
  return (
    <Svg size={size}>
      <path d="m9 18 6-6-6-6" />
    </Svg>
  );
}

/** 置顶（图钉） */
export function IconPin({ size = 15 }: IconProps) {
  return (
    <Svg size={size}>
      <path d="M12 17v5" />
      <path d="M9 10.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1z" />
    </Svg>
  );
}

export function IconMinus({ size = 15 }: IconProps) {
  return (
    <Svg size={size}>
      <path d="M5 12h14" />
    </Svg>
  );
}

export function IconClose({ size = 15 }: IconProps) {
  return (
    <Svg size={size}>
      <path d="M18 6 6 18M6 6l12 12" />
    </Svg>
  );
}

/** 推送设置（滑杆） */
export function IconSliders({ size = 16 }: IconProps) {
  return (
    <Svg size={size}>
      <path d="M4 21v-7M4 10V3M12 21v-9M12 8V3M20 21v-5M20 12V3" />
      <path d="M1 14h6M9 8h6M17 16h6" />
    </Svg>
  );
}

export function IconAlert({ size = 14 }: IconProps) {
  return (
    <Svg size={size}>
      <path d="m10.29 3.86-8.47 14.14A2 2 0 0 0 3.53 21h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z" />
      <path d="M12 9v4M12 17h.01" />
    </Svg>
  );
}

export function IconBell({ size = 14 }: IconProps) {
  return (
    <Svg size={size}>
      <path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9" />
      <path d="M10.3 21a1.94 1.94 0 0 0 3.4 0" />
    </Svg>
  );
}

export function IconClock({ size = 14 }: IconProps) {
  return (
    <Svg size={size}>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 7v5l3 2" />
    </Svg>
  );
}

/** 重复（首尾相接的箭头） */
export function IconRepeat({ size = 13 }: IconProps) {
  return (
    <Svg size={size}>
      <path d="m17 2 4 4-4 4" />
      <path d="M3 11v-1a4 4 0 0 1 4-4h14" />
      <path d="m7 22-4-4 4-4" />
      <path d="M21 13v1a4 4 0 0 1-4 4H3" />
    </Svg>
  );
}

/* ---------- 主题三态（标题栏那个按钮）---------- */

/** 亮色：太阳 */
export function IconSun({ size = 14 }: IconProps) {
  return (
    <Svg size={size}>
      <circle cx="12" cy="12" r="4" />
      <path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" />
    </Svg>
  );
}

/** 深色：月牙 */
export function IconMoon({ size = 14 }: IconProps) {
  return (
    <Svg size={size}>
      <path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z" />
    </Svg>
  );
}

/** 跟随系统：半明半暗的圆 */
export function IconAuto({ size = 14 }: IconProps) {
  return (
    <Svg size={size}>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 3a9 9 0 0 1 0 18z" fill="currentColor" stroke="none" />
    </Svg>
  );
}

/** 外观设置：一大一小两个 A */
export function IconType({ size = 14 }: IconProps) {
  return (
    <Svg size={size}>
      <path d="M2 20 8.4 4.6 14.8 20M4.6 15.6h7.6" />
      <path d="M15.4 20l3.3-8.2L22 20M16.6 17.4h4.2" />
    </Svg>
  );
}

/** 月总览：日历（标题栏那个按钮） */
export function IconCalendar({ size = 14 }: IconProps) {
  return (
    <Svg size={size}>
      <rect x="3" y="4.6" width="18" height="16.4" rx="2.4" />
      <path d="M3 9.6h18M8 2.6v4M16 2.6v4" />
      <path d="M7.4 13.4h1.4M11.4 13.4h1.4M15.4 13.4h1.4M7.4 17h1.4M11.4 17h1.4" />
    </Svg>
  );
}

/** 重点任务：五角星。`filled` = 实心（已经标了重点） */
export function IconStar({ size = 14, filled = false }: IconProps & { filled?: boolean }) {
  return (
    <Svg size={size}>
      <path
        d="m12 3.6 2.6 5.4 5.9.8-4.3 4.2 1 5.9-5.2-2.8-5.2 2.8 1-5.9L3.5 9.8l5.9-.8z"
        fill={filled ? "currentColor" : "none"}
      />
    </Svg>
  );
}

/**
 * 浮窗：一块贴着屏幕边的面板。
 *
 * 画的是「靠右贴着边的一条 」——和这个按钮想表达的东西一致。
 */
export function IconFloat({ size = 15 }: IconProps) {
  return (
    <Svg size={size}>
      <rect x="2.6" y="4.6" width="18.8" height="14.8" rx="2.4" />
      <path d="M17 7.6v.01" />
      <rect x="15.4" y="9.4" width="3.6" height="7.6" rx="1.1" fill="currentColor" stroke="none" />
    </Svg>
  );
}
