/**
 * 「提前提醒」下拉。值是**提前多少秒**（0 = 准点）。
 *
 * 只提供固定几档：`remind_offsets` 本身支持任意多个提醒点，但那种形状这个下拉表达不了，
 * 于是 `parseAdvance` 认不出来时返回 `null`，这里显示「保持原样」——保存时原值不动，
 * 既不猜、也不会把用户已有的提醒点冲掉。
 */
import { REMIND_PRESETS } from "./api";
import { advanceOptionLabel } from "./day";

export default function AdvanceSelect({
  value,
  onChange,
}: {
  /** 提前秒数；`null` = 认不出来（库里存了多个提前点等），保存时保持原值 */
  value: number | null;
  onChange: (secs: number | null) => void;
}) {
  return (
    <label className="interval" title="提醒时间：准点，或者提前一段时间">
      提醒
      <select
        value={value === null ? "keep" : String(value)}
        onChange={(e) => {
          const v = e.target.value;
          onChange(v === "keep" ? null : Number(v));
        }}
      >
        {value === null ? <option value="keep">保持原样</option> : null}
        {REMIND_PRESETS.map((secs) => (
          <option key={secs} value={secs}>
            {advanceOptionLabel(secs)}
          </option>
        ))}
      </select>
    </label>
  );
}
