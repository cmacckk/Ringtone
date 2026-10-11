import { useState } from "react";
import {
  APPEARANCE_DEFAULTS,
  Appearance as AppearanceValue,
  FONT_CHOICES,
  FONT_MAX,
  FONT_MIN,
  OPACITY_MIN,
  applyAppearance,
  loadAppearance,
  saveAppearance,
} from "./appearance";

/** 下拉里的「自己填」哨兵值：不会和真实字体栈撞 */
const CUSTOM = "__custom__";
/** 浮窗字体下拉里的「跟主窗口一样」：`floatFontFamily` 存 `null` */
const FOLLOW = "__follow__";

function pct(v: number): string {
  return `${Math.round(v * 100)}%`;
}

/**
 * 外观面板：字体 / 字号 / 主窗口与浮窗的透明度。
 *
 * 改一下**立刻生效并保存**（没有「保存」按钮）：这几项全是看得见的东西，边拖边看才对；
 * 「恢复默认」用来一键回到初始值。
 *
 * 注意这里只 `applyAppearance(next)`（= 主窗口那一份字体/字号/透明度）：
 * 浮窗是另一个窗口，它会自己按 `storage` 事件按「浮窗那一份」重算
 * （`applyAppearance(a, "float")` 会优先用 `floatFontFamily` / `floatFontScale` / `floatOpacity`）。
 * 主题（深浅色）没有独立设置：浮窗永远跟主窗口一致。
 */
export default function Appearance({ onClose }: { onClose: () => void }) {
  const [value, setValue] = useState<AppearanceValue>(() => loadAppearance());
  // 字体是不是「自己填」：下拉里没有当前值（用户手写过 / 改过 localStorage）就算自己填
  const [custom, setCustom] = useState(
    () => !FONT_CHOICES.some((f) => f.value === loadAppearance().fontFamily),
  );

  // 浮窗到底用哪套字体/字号：自己设过就用自己那份，没设过（null）就跟主窗口
  const floatFamily = value.floatFontFamily ?? value.fontFamily;
  const floatScale = value.floatFontScale ?? value.fontScale;
  const floatFollowsFont = value.floatFontFamily === null;
  const floatFollowsScale = value.floatFontScale === null;

  function update(patch: Partial<AppearanceValue>) {
    const next = { ...value, ...patch };
    setValue(next);
    saveAppearance(next);
    applyAppearance(next);
  }

  function reset() {
    const next = { ...APPEARANCE_DEFAULTS };
    setValue(next);
    setCustom(false);
    saveAppearance(next);
    applyAppearance(next);
  }

  return (
    <div className="panel">
      <div className="panel-head">
        <strong>外观</strong>
        <span className="spacer" />
        <button type="button" className="ghost" onClick={onClose}>
          返回待办
        </button>
      </div>

      <div className="hint">
        改一下立刻生效，浮窗那边也会马上跟着变；主题（深浅色）两个窗口永远一致，
        要换就在标题栏那个月亮按钮上换。
      </div>

      <div className="card">
        <div className="card-head">
          <strong>字体</strong>
          <span className="muted">主窗口</span>
        </div>
        <div className="form">
          <label className="wide">
            字体
            <select
              value={custom ? CUSTOM : value.fontFamily}
              onChange={(e) => {
                if (e.target.value === CUSTOM) {
                  setCustom(true);
                  return;
                }
                setCustom(false);
                update({ fontFamily: e.target.value });
              }}
            >
              {FONT_CHOICES.map((f) => (
                <option key={f.label} value={f.value}>
                  {f.label}
                </option>
              ))}
              <option value={CUSTOM}>自己填…</option>
            </select>
          </label>
          {custom ? (
            <label className="wide">
              字体名（CSS 的 font-family，可以逗号隔开写几个做回退）
              <input
                value={value.fontFamily}
                placeholder={APPEARANCE_DEFAULTS.fontFamily}
                onChange={(e) => update({ fontFamily: e.target.value })}
              />
            </label>
          ) : null}
        </div>
        <div
          className="appearance-sample"
          style={{ fontFamily: value.fontFamily }}
          title="这块就是当前字体"
        >
          交周报 10:00 · 提前 5 分钟 · 今天 4 条
        </div>
      </div>

      <div className="card">
        <div className="card-head">
          <strong>字号</strong>
          <span className="muted">主窗口 {value.fontScale} px</span>
        </div>
        <div className="row">
          <input
            className="slider"
            type="range"
            min={FONT_MIN}
            max={FONT_MAX}
            step={1}
            value={value.fontScale}
            onChange={(e) => update({ fontScale: Number(e.target.value) })}
          />
          <button
            type="button"
            className="mini"
            onClick={() => update({ fontScale: APPEARANCE_DEFAULTS.fontScale })}
          >
            标准 {APPEARANCE_DEFAULTS.fontScale}
          </button>
        </div>
        <div className="hint">只缩放文字，窗口和间距不动（间距会被文字撑开一点）。</div>
      </div>

      <div className="card">
        <div className="card-head">
          <strong>浮窗</strong>
          <span className="muted">
            {floatFollowsFont && floatFollowsScale ? "字体字号跟主窗口一样" : "自己一套"}
          </span>
        </div>
        <div className="form">
          <label className="wide">
            字体
            <select
              value={value.floatFontFamily ?? FOLLOW}
              onChange={(e) =>
                update({
                  floatFontFamily: e.target.value === FOLLOW ? null : e.target.value,
                })
              }
            >
              <option value={FOLLOW}>跟主窗口一样</option>
              {FONT_CHOICES.map((f) => (
                <option key={f.label} value={f.value}>
                  {f.label}
                </option>
              ))}
            </select>
          </label>
        </div>
        <div className="row">
          <span className="appearance-name">字号</span>
          <input
            className="slider"
            type="range"
            min={FONT_MIN}
            max={FONT_MAX}
            step={1}
            value={floatScale}
            onChange={(e) => update({ floatFontScale: Number(e.target.value) })}
          />
          <span className="appearance-value">{floatScale} px</span>
          <button
            type="button"
            className="mini"
            disabled={floatFollowsScale}
            onClick={() => update({ floatFontScale: null })}
          >
            跟主窗口一样
          </button>
        </div>
        <div className="row">
          <span className="appearance-name">透明度</span>
          <input
            className="slider"
            type="range"
            min={OPACITY_MIN}
            max={1}
            step={0.05}
            value={value.floatOpacity}
            onChange={(e) => update({ floatOpacity: Number(e.target.value) })}
          />
          <span className="appearance-value">{pct(value.floatOpacity)}</span>
        </div>
        <div
          className="appearance-sample"
          style={{ fontFamily: floatFamily, fontSize: `${floatScale}px` }}
          title="这块就是浮窗现在的字体和字号"
        >
          今天 <b>1</b>/4 已完成 · 交周报 10:00
        </div>
        <div className="hint">
          浮窗比主窗口窄，字可以单独调小一点。主题颜色没有单独的开关，永远跟主窗口走。
        </div>
      </div>

      <div className="card">
        <div className="card-head">
          <strong>主窗口透明度</strong>
        </div>
        <div className="row">
          <span className="appearance-name">透明度</span>
          <input
            className="slider"
            type="range"
            min={OPACITY_MIN}
            max={1}
            step={0.05}
            value={value.winOpacity}
            onChange={(e) => update({ winOpacity: Number(e.target.value) })}
          />
          <span className="appearance-value">{pct(value.winOpacity)}</span>
        </div>
        <div className="hint">
          越低越透，能看见后面的桌面。系统没有给单个窗口调透明度的接口，
          这里是让整个界面按这个比例合成，所以文字也会一起变淡。
        </div>
      </div>

      <div className="card">
        <div className="card-head">
          <strong>任务配色</strong>
          <span className="muted">{value.taskColors ? "开" : "关"}</span>
        </div>
        <div className="row">
          <label className="appearance-name" style={{ flex: 1 }}>
            <input
              type="checkbox"
              checked={value.taskColors}
              onChange={(e) => update({ taskColors: e.target.checked })}
            />{" "}
            给每条任务分一个颜色（主窗口左侧细条 / 浮窗小圆点）
          </label>
        </div>
        <div className="hint">
          关掉后只剩「重点」（星标，加粗 + 左侧粗条）与「紧急」（快 30 分钟内 / 已过期）两种强调，界面更素。
        </div>
        <div className="row">
          <button type="button" className="ghost" onClick={reset}>
            全部恢复默认
          </button>
        </div>
      </div>
    </div>
  );
}
