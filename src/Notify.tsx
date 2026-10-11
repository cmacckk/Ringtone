import { KIND_LABEL } from "./api";

/**
 * 「这条提醒发到哪些渠道」的开关组。
 *
 * - **全开 = `null`**：交给后端当「不限制」，以后新增的渠道也会自动算进来；
 * - 只开一部分 = 明确的类型列表（存类型不存渠道 id，渠道删了重建也能对上）；
 * - **至少留一个**：全关等于「一条都不发」，没有这个选项，所以最后一个点不动；
 * - 可用渠道少于 2 个时整个不显示（只有一个渠道，要么发要么不发，那是渠道自己的启用开关管的）；
 * - 选了但现在已经停用的渠道，只灰字提示一句，不画成可点的开关（点了也没用）。
 */
export default function NotifyPicker({
  kinds,
  available,
  onChange,
}: {
  /** `null` = 不限制（全部启用渠道） */
  kinds: string[] | null;
  /** 当前启用中的渠道类型 */
  available: string[];
  onChange: (kinds: string[] | null) => void;
}) {
  const stale = (kinds ?? []).filter((k) => !available.includes(k));
  if (available.length < 2) return null;

  const on = (kind: string) => (kinds === null ? true : kinds.includes(kind));

  const toggle = (kind: string) => {
    // 全开状态下点一个 → 变成「只发其余那些」；否则在明确列表上加/减
    const next = new Set(kinds === null ? available : kinds.filter((k) => available.includes(k)));
    if (next.has(kind)) {
      if (next.size === 1) return; // 至少留一个
      next.delete(kind);
    } else {
      next.add(kind);
    }
    // 又全选回来了 → 用 `null`（不限制），而不是把当前这几个渠道钉死
    onChange(next.size === available.length ? null : available.filter((k) => next.has(k)));
  };

  return (
    <span className="notify">
      <span className="notify-label" title="这条提醒发到哪些渠道；全选 = 全部启用渠道">
        通知
      </span>
      {available.map((k) => (
        <button
          key={k}
          type="button"
          className={on(k) ? "kind on" : "kind"}
          title={on(k) ? "点一下改成只发其他渠道（至少留一个）" : "点一下加上这个渠道"}
          onClick={() => toggle(k)}
        >
          {KIND_LABEL[k] ?? k}
        </button>
      ))}
      {stale.length > 0 ? (
        <span className="notify-hint" title="这些渠道现在没启用，到点会走本机通知兜底">
          {stale.map((k) => KIND_LABEL[k] ?? k).join("、")}已停用
        </span>
      ) : null}
    </span>
  );
}
