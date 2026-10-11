/**
 * 重复规则编辑器。
 *
 * 只管「规则本身」，不管保存：父组件从 `onChange` 拿新规则，再决定什么时候提交。
 * 规则形状与后端 `models::RepeatRule` 完全一致（存库就是同一份 JSON）。
 */
import type { ReactNode } from "react";
import type { RepeatFreq, RepeatRule } from "./api";
import { WEEKDAY_SHORT, describeRepeat } from "./day";

const FREQ_OPTIONS: { value: RepeatFreq | "none"; label: string }[] = [
  { value: "none", label: "不重复" },
  { value: "daily", label: "每天" },
  { value: "weekdays", label: "工作日" },
  { value: "weekly", label: "每周" },
  { value: "monthly", label: "每月" },
  { value: "last_workday", label: "每月最后工作日" },
  { value: "yearly", label: "每年" },
];

const INTERVAL_UNIT: Record<Exclude<RepeatFreq, "weekdays">, string> = {
  daily: "天",
  weekly: "周",
  monthly: "个月",
  last_workday: "个月",
  yearly: "年",
};

/** 换频率时保留「每隔几」「到哪天」；`byday` 只有「每周」才保留（别的频率带上会被后端拒） */
function makeRule(freq: RepeatFreq, prev: RepeatRule | null): RepeatRule {
  return {
    freq,
    interval: prev?.interval ?? 1,
    byday: freq === "weekly" && prev?.freq === "weekly" ? prev.byday : [],
    until: prev?.until ?? null,
  };
}

export default function RepeatEditor({
  rule,
  day,
  onChange,
  extra,
}: {
  rule: RepeatRule | null;
  /** 任务自己的**起始日**：`每月 / 每年` 规则只存频率不存「几号」，跟它放一起才说得清 */
  day: string;
  onChange: (rule: RepeatRule | null) => void;
  /** 塞在同一个 flex 行里的额外控件（比如「提前提醒」下拉）；一行放不下会自动换行 */
  extra?: ReactNode;
}) {
  const summary = describeRepeat(rule, day);
  return (
    <div className="repeat">
      <select
        value={rule?.freq ?? "none"}
        title="重复方式"
        onChange={(e) => {
          const v = e.target.value;
          onChange(v === "none" ? null : makeRule(v as RepeatFreq, rule));
        }}
      >
        {FREQ_OPTIONS.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>

      {rule && rule.freq !== "weekdays" ? (
        <label className="interval">
          每隔
          <input
            type="number"
            min={1}
            max={99}
            value={rule.interval}
            onChange={(e) => {
              const n = Number(e.target.value);
              // 空串 / 0 / 负数一律按 1 处理：交给后端也只会被拒，不如这里就夹住
              const interval = Number.isFinite(n) && n >= 1 ? Math.min(99, Math.floor(n)) : 1;
              onChange({ ...rule, interval });
            }}
          />
          {INTERVAL_UNIT[rule.freq]}
        </label>
      ) : null}

      {rule && rule.freq === "weekly" ? (
        <div className="byday" title="不选就按任务自己那天的星期几（推荐先选一天再选别的）">
          {WEEKDAY_SHORT.map((name, i) => {
            const d = i + 1;
            const on = rule.byday.includes(d);
            return (
              <button
                key={name}
                type="button"
                className={on ? "wd on" : "wd"}
                title={on ? `不再在周${name}提醒` : `每周${name}都提醒`}
                onClick={() =>
                  onChange({
                    ...rule,
                    byday: on
                      ? rule.byday.filter((v) => v !== d)
                      : [...rule.byday, d].sort((a, b) => a - b),
                  })
                }
              >
                {name}
              </button>
            );
          })}
        </div>
      ) : null}

      {rule ? (
        <label className="until">
          结束
          <input
            type="date"
            value={rule.until ?? ""}
            min={day}
            title="留空 = 一直重复；填了就含当天"
            onChange={(e) => onChange({ ...rule, until: e.target.value || null })}
          />
        </label>
      ) : null}

      {summary ? <span className="muted">{summary}</span> : null}
      {rule && (rule.freq === "weekdays" || rule.freq === "last_workday") ? (
        <span className="muted" title="认 2026 年国务院安排，含调休补班的周末">
          跳过法定节假日与周末
        </span>
      ) : null}
      {extra}
    </div>
  );
}
