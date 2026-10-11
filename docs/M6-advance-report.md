# M6 第 2 项报告 · 提前提醒（`remind_offsets`）的界面

> 目标：把「提前 5 分钟 / 提前 1 小时」这种提醒点做成界面能设的东西。
> 之前只有后端和库里支持（`remind_offsets`），界面上根本没法设——截图里那种「提前 5 分钟」标签只能靠手写数据库造出来。
>
> 结论：后端 + 界面都完成，`cargo fmt/check/clippy` 0 问题、**109 个单测全绿**（新增 3 个）、`tsc` 与 `vite build` 干净。

## 一、`remind_offsets` 的语义（已成文，别随手改）

`remind_offsets` 是 JSON 数组字符串，元素是**相对 `due_at_ms` 的秒偏移**：`0` = 到点，`-300` = 提前 5 分钟。

| 存的值 | 含义 | 生成几条实例 |
| --- | --- | --- |
| `"[0]"` | 只在到点提醒（默认） | 1 |
| `"[-300,0]"` | 提前 5 分钟 + 到点 | 2 |
| `"[-600,-300,0]"` | 多个提前点（界面设不出来，接口/库可以直接写） | 3 |

**入参 `TaskInput.remind_offsets: Option<String>` 的 `None` 是两义的，这是本项最关键的设计**：

- **新建**时 `None` = 用默认的 `"[0]"`（到点提醒）；
- **更新**时 `None` = **保持库里原值不动**（SQL 是 `remind_offsets = COALESCE(?6, remind_offsets)`）。

为什么非要这样：界面上的下拉**不碰**的时候，前端不应该编一个值发上去。否则用户库里存的三个提前点，
会被界面一次「改个标题」静默压成「准点」。测试 `remind_offsets_create_and_update_semantics` 就是钉这一条的。

## 二、界面

控件是一个下拉，塞在**重复规则编辑器的同一行**（`RepeatEditor` 新增 `extra` 槽位，两个控件在同一行、放不下自动换行）：

```text
提醒  [ 准点 / 提前 1 分钟 / 提前 5 分钟 / 提前 10 分钟 / 提前 30 分钟 / 提前 1 小时 / 提前 1 天 ]
```

出现的地方：添加任务的第二行、双击标题后的编辑区第二行（和重复下拉并排）。列表行上的 `提前 5 分钟` 标签沿用 `advanceLabel()`。

- 新增 `F:/develop/Ringtone/src/Advance.tsx`：`AdvanceSelect({ value, onChange })`，`value` 是**提前秒数**，`null` = 认不出来。
- `F:/develop/Ringtone/src/api.ts`：`REMIND_PRESETS`、`advanceToJson(secs)`（`0 → "[0]"`，否则 `[-secs,0]`）、`parseAdvance(raw)`。
- `F:/develop/Ringtone/src/day.ts`：`advanceOptionLabel(secs)`（复用已有的 `humanSpan`）。
- `F:/develop/Ringtone/src/App.tsx`：`Editing` 加 `advance: number | null`；composer 加 `advance` state；
  `onAdd` 传 `advanceToJson(advance)`；`onSaveEdit` 在 `advance === null` 时传 `null`。

### 认不出来的情况显示「保持原样」

`parseAdvance` 只认「**至多一个**提前点（可带不带到点）」，而且提前量必须在预设档位里：
库里是多提前点（`[-600,-300,0]`）或奇怪数值时返回 `null`，下拉显示 **保持原样**，保存时原样不动。
**宁可不猜**——猜错就是把用户设的提醒点弄丢了。

## 三、校验（后端）

`TaskInput::validated()` 里新增一段，**故意不复用** `parse_remind_offsets`：

> `parse_remind_offsets` 对垃圾输入 `serde_json::from_str(...).unwrap_or_default()` 兜底成 `[0]`，
> 于是**任何字符串都“看起来合法”**，用户传错了也发现不了。所以校验走严格的：
> 必须是 JSON 数组、非空、每个元素的绝对值 ≤ 7 天（`MAX_REMIND_OFFSET_SECS`）。

## 四、验证

| 检查 | 结果 |
| --- | --- |
| `cargo fmt --check` | 0 差异 |
| `cargo check --all-targets` | 0 警告 |
| `cargo clippy --all-targets` | 0 警告 |
| `cargo test --lib` | **109 passed / 0 failed**（新增 `remind_offsets_create_and_update_semantics`、`rejects_bad_remind_offsets`、`absent_remind_offsets_is_fine`） |
| `pnpm exec tsc --noEmit` | 0 |
| `pnpm exec vite build` | 成功（css 10.61 kB / js 260.48 kB） |
| 实机渲染 | 运行中的应用里，添加区与编辑区都渲染出了新控件，选项为「准点 / 提前 1 分钟 / 提前 5 分钟 / 提前 10 分钟 / 提前 30 分钟 / 提前 1 小时 / 提前 1 天」 |

**没做完的一步**：用界面手点一遍（选「提前 5 分钟」添加 → 看列表标签 → 改标题保存后提醒点是否保留）并截图。
原因：验证时发现应用窗口**正被使用**（有一个任务处于编辑态、一条 17:27 的提醒待发），我停掉了 UI 自动化脚本以免打扰，
所以这一步留到应用空闲时补。逻辑本身已由上面 3 个单测覆盖（含「更新时保留原值」这条最关键的）。

**界面手点这一步已在同场会话里补上**（应用空闲后做的，见 `docs/M6-quick-report.md` 的实机一节）：
`docs/screenshots/m6-quick-add-dark.png` / `m6-quick-add-light.png` 里能看到添加区第二行的
「不重复」+「提醒 准点」下拉与快捷录入的提示行；快捷录入那轮冒烟同时验证了添加/保存路径。
仍未做的只是「在编辑区里改提前档位再保存」这一步的手点截图。

## 五、顺带核对：用户真实任务的两条准点投递（不属于本项代码，只是同场会话的证据）

用户在窗口里自己建的两条任务在到点后都真的发出去了（主渠道钉钉，`channels.id=2 priority=100`）：

| 实例 | 到点 `fire_at_ms` | 发出 `sent_ms` | 延迟 | 渠道 / 结果 | 作业 |
| --- | --- | --- | --- | --- | --- |
| 112（`（用户自己的任务 A）`） | 1791451620000（17:27:00） | 1791451622272 | **2272 ms** | 钉钉 `errcode=0` HTTP 200 | `id=10` attempts=1 `dedupe_key=112:2` |
| 113（`（用户自己的任务 B）`） | 1791451800000（17:30:00） | 1791451802222 | **2222 ms** | 钉钉 `errcode=0` HTTP 200 | `id=11` attempts=1 `dedupe_key=113:2` |

两条都在「到点 ±5 秒」的验收范围内，每个实例只入队一次（`dedupe_key = occurrence:channel` 幂等生效），
并且**没有走降级**（飞书 priority=200 没被用到）。顺带验证了 M2 调度在真实数据上的延迟表现
（M4 测的那次 `lag=10837ms` 复查未重现，这两次都是 ~2.2 秒）。

## 六、已知缺口

1. 界面只给固定档位，**设不了多个提前点**（`[-600,-300,0]` 这种只能走接口/库）；提前量上限 7 天。
2. 前端仍然没有自动化测试（`parseAdvance` / `advanceToJson` 这类纯函数靠人眼 + 上面那张渲染证据），项目里也没搭 vitest。
3. 提醒点只跟 `due_at_ms` 挂钩：任务不设具体时刻（`due_at_ms = null`）时，这个下拉没有意义（界面没隐藏它，只是不生效）。
