/**
 * 右键菜单（全局共用一个）。
 *
 * 为什么自己做而不是用系统的原生菜单：待办行 / 浮窗条目要的菜单项是动态的
 * （这条是不是重点、是重复任务还是普通任务），原生菜单要绕一大圈才能改文案，
 * 而且浮窗是无边框小窗，原生菜单在这上面表现也不一致。
 *
 * 用法：父组件在 `onContextMenu` 里 `preventDefault()` 之后把坐标和菜单项传进来。
 */
import { useEffect, useLayoutEffect, useRef, useState } from "react";

export type CtxItem = {
  /** 菜单文字 */
  label: string;
  /** 点下去干什么 */
  onPick: () => void;
  /** 危险动作（删除）——红色 */
  danger?: boolean;
  /** 灰掉不可点 */
  disabled?: boolean;
  /** 两组之间画一条分隔线 */
  separatorBefore?: boolean;
};

export default function ContextMenu({
  x,
  y,
  items,
  onClose,
}: {
  x: number;
  y: number;
  items: CtxItem[];
  onClose: () => void;
}) {
  const box = useRef<HTMLDivElement | null>(null);
  const [pos, setPos] = useState({ x, y });

  // 摆进来之后再量一次，别让菜单跑出窗口外面
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    const maxX = window.innerWidth - r.width - 6;
    const maxY = window.innerHeight - r.height - 6;
    setPos({
      x: Math.max(6, Math.min(x, maxX)),
      y: Math.max(6, Math.min(y, maxY)),
    });
  }, [x, y, items.length]);

  useEffect(() => {
    // 用 mousedown 而不是 click：点空白处要**立刻**关掉，不等 mouseup
    const off = (e: MouseEvent) => {
      if (box.current?.contains(e.target as Node)) return;
      onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("mousedown", off, true);
    window.addEventListener("keydown", onKey);
    window.addEventListener("blur", onClose);
    window.addEventListener("resize", onClose);
    return () => {
      window.removeEventListener("mousedown", off, true);
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("blur", onClose);
      window.removeEventListener("resize", onClose);
    };
  }, [onClose]);

  return (
    <div
      className="ctx-menu"
      ref={box}
      style={{ left: pos.x, top: pos.y }}
      // 菜单自己右键不该又冒出一层菜单
      onContextMenu={(e) => e.preventDefault()}
    >
      {items.map((it, i) => (
        <button
          key={`${it.label}-${i}`}
          type="button"
          className={
            (it.danger ? "ctx-item danger" : "ctx-item") +
            (it.separatorBefore ? " sep" : "")
          }
          disabled={it.disabled ?? false}
          onClick={() => {
            onClose();
            it.onPick();
          }}
        >
          {it.label}
        </button>
      ))}
    </div>
  );
}
