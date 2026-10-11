# M6 · 浮窗：能勾完成 + 独立字体字号 + 主题跟随

一句话：贴边浮窗从「只能看」变成「能干活」——左边小圈可以直接勾完成，勾完主窗口列表跟着变；
外观面板多了一块「浮窗」，字体 / 字号 / 透明度都能跟主窗口不一样；主题永远跟主窗口一致。

## 1. 做了什么

| 元素 | 说明 |
| --- | --- |
| 浮窗勾选框 | 每行左边一个 15×15 自绘圆圈（`button.float-check`），点一下「完成这次」、再点一下取消；`done` 的行标题划线变灰 |
| 勾的是谁 | 重复任务 → `set_occurrence_done(id, 今天, done)`（完成今天这一次，系列继续）；普通任务 → `set_task_done(id, done)`（判断用 `repeat_rule !== null`，与主窗口同一条规则） |
| 已完成也显示 | `MAX_ROWS = 5`，**含已完成**的（不然勾完就消失，想撤销都点不到） |
| 勾完同步主窗口 | 新增 `src/sync.ts`：`pingTasks()` 往 `localStorage` 写 `ringtone.tasksPing`，主窗口 `watchTasks()` 收到 `storage` 事件重新拉列表 |
| 浮窗外观 | 外观面板拆成「字体（主窗口）」「字号（主窗口）」「**浮窗**」「主窗口透明度」四块；浮窗那块默认「跟主窗口一样」 |
| 浮窗跟随的存法 | `Appearance.floatFontFamily` / `floatFontScale`，`null` = 跟随主窗口；透明度仍是浮窗自己那一条 |
| 主题 | 不设开关，两个窗口永远一致（`watchTheme` 监听 `storage`） |
| 点击语义 | 点卡片空白处 → 打开主窗口；点小圈 → 只勾完成；拖一下 → 换边（三种手势互不干扰） |

## 2. 四个决定

1. **勾完成的点击必须两处 `stopPropagation()`**（`onMouseDown` 与 `onClick`）：
   不拦 `mousedown`，卡片的 `data-tauri-drag-region` 会把这一下当成「按住要拖窗口」；
   不拦 `click`，卡片的 `onClick` 会打开主窗口。实测点完勾选框主窗口 `document.hasFocus()` 仍是 `false`
   （没被 `showMain` 抢焦点），说明两处都拦住了。
2. **用 `localStorage` 的 `storage` 事件当跨窗口广播，不用 Tauri `emit`**：
   两个窗口同 origin，`storage` 只在**别的**文档触发，天然就是广播；这里只需要「重新拉一遍」这种弱一致性，
   不需要后端参与。反例是主题那次漏接 `watchTheme`（见「坑」）。
3. **「跟主窗口一样」存成 `null`，而不是存一份拷贝**：
   存拷贝的话，用户改主窗口字体时浮窗还停在旧值上，看起来像坏了。`null` 让浮窗永远跟着主窗口变。
   代价是 `loadAppearance()` 里这两个字段**不能用 `pick()`**（`pick` 会把缺失值变成基准值，就再也回不到「跟随」了）。
4. **已完成的也列出来（最多 5 行）**：浮窗的价值是「顺手勾一下」，勾完变空的话就成了单向操作，
   点错了只能打开主窗口撤销。

## 3. 改动清单

后端 **Rust 一行没改**（`set_task_done` / `set_occurrence_done` 早已注册进 `invoke_handler`，
自定义命令不需要 capability 条目）。

| 文件 | 改动 |
| --- | --- |
| `src/appearance.ts` | `Appearance` 加 `floatFontFamily` / `floatFontScale`（`null` = 跟随）；`loadAppearance()` 用「是有限数才留」而不是 `pick`；`applyAppearance()` 里浮窗取 `a.floatX ?? a.X` |
| `src/sync.ts`（新） | `pingTasks()` / `watchTasks(cb)`，键 `ringtone.tasksPing` |
| `src/AppearancePanel.tsx` | 拆出「浮窗」卡片：字体下拉含「跟主窗口一样」（`__follow__` 哨兵）、字号滑条 + 「跟主窗口一样」按钮、浮窗透明度、预览块（用浮窗的字体字号渲染） |
| `src/Float.tsx` | `MAX_ROWS = 5`、`todayRef`、`toggle(t)`（勾完 `load()` + `pingTasks()`）、行内勾选框、`watchTasks` 订阅、卡片 `title` 文案 |
| `src/App.tsx` | `useEffect(() => watchTasks(() => void reload(dayRef.current)), [reload])` |
| `src/styles.css` | `.float-item` 改 `align-items: center`；`.float-dot` 换成 `button.float-check`（圆形、`.on` 蓝底白勾）+ `.float-item.done .t`（划线变灰） |

## 4. 实测证据（CDP 走真实界面）

| 步骤 | 实测 |
| --- | --- |
| 浮窗行 | 5 项、`scrollHeight - clientHeight = 0`（不溢出）、5 个 `.float-check` |
| 点勾选框 | 浮窗该行 `done:true / on:true`，头部 `2/6 已完成 · 全部 8 条` |
| 主窗口同步 | 主窗口列表同一行 `done:true`（`pingTasks` 的 `storage` 广播生效） |
| 没抢焦点 | 点完主窗口 `document.hasFocus()` 仍为 `false` ⇒ 两处 `stopPropagation()` 都生效，没触发 `showMain` |
| 再点回来 | 浮窗行回 `done:false`、DB `tasks.done = 0`（状态已还原） |
| 浮窗字号 → 12 | 浮窗 `--fs-k = 0.8571428571428571`，**主窗口仍是 `1`** |
| 浮窗字体 → 楷体 | 浮窗 `font-family: KaiTi, 楷体, serif`，主窗口仍是思源黑体 |
| 浮窗透明度 → 0.7 | 浮窗 `body.opacity = 0.7`，主窗口 `1` |
| 浮窗卡预览 | `style="font-family: KaiTi, 楷体, serif; font-size: 12px;"` ✓ |
| 「跟主窗口一样」 | 浮窗 `--fs-k` 回 `1`（该按钮只管字号，字体保持不变） |
| 「全部恢复默认」 | 两边都回 `--fs-k=1` / 系统字体 / `opacity=1`；存储里 `"floatFontFamily":null,"floatFontScale":null` ✓ |
| 主题跟随 | 主窗口点主题按钮 `dark → auto → light`，每一步浮窗的 `data-theme` 都跟着变 |
| 收工状态 | 两个窗口 `theme=dark`、`--fs-k=1`、`opacity=1`、无 `.error`；主窗口 `minimized=false visible=true`；外观面板已关、回到 `周五 10-09` 的 5 条任务 |

截图：`docs/screenshots/m5-m6-float-check-dark.png` / `-light.png`（浮窗带勾选框，第一行已完成 =
蓝底白勾 + 划线）、`m5-m6-appearance-float-dark.png` / `-light.png`（外观面板带「浮窗」那块，深色已肉眼确认）。

## 5. 检查

| 检查 | 结果 |
| --- | --- |
| `cargo fmt --check`（在 `src-tauri/` 下跑） | 0 |
| `cargo check --all-targets` | 0 警告 |
| `cargo clippy --all-targets` | 0 警告 |
| `cargo test --lib` | **114 passed / 0 failed** |
| `pnpm exec tsc --noEmit` | 0 |
| `pnpm exec vite build` | `index.html 0.39 kB` / `css 17.47 kB` / `js 288.44 kB` |

## 6. 坑

1. **`node:sqlite` 长连接读到的是旧快照**：第一次点完勾选框去读 `tasks.done` 读到 `0`，
   而 App 自己的读取路径（主窗口 DOM、浮窗 DOM）都显示已勾上。**库的真相以 App 的读取路径为准**，
   长连接只适合读「不会被同时改」的表。
2. **造测试数据时别从周条格子里抠日期**：格子上写的是 `10-09` 这种短日期，
   `textContent.replace(/[^0-9-]/g,'')` 会得到 `10-09` 而不是 `2026-10-09`，插进去就是一条 `day='10-09'`
   的脏行、任务根本不显示（误插的 3 条已按显式 id 删除）。日期用页面里 `new Date()` 自己拼。
3. **验证时改掉了用户的主题要记得改回来**：试主题同步时主题停在 `light`（用户原来是 `dark`），
   收工前已还原，并且顺手确认两个窗口都跟上。
4. **主窗口被最小化时，CDP 截图照样能拍**（CDP 是 WebView2 自己渲染的图）——
   但截图前如果先要把窗口抬起来，用应用自己的 `show_main`（`plugin:window|unminimize` 没放行权限）。

## 7. 已知缺口

1. 浮窗只能贴**左 / 右**两条边，收起宽度固定 9 逻辑像素（不能调）。
2. 浮窗列表最多 5 行（含已完成），当天更多要打开主窗口看；不显示「还有 N 条」那种折叠提示。
3. 浮窗里**不能编辑、不能删除、不能看提前提醒**，只能勾完成 + 打开主窗口。
4. 换边靠真鼠标拖；CDP 合成事件只能验证「按下 → 移动 → 吸附 → 记住边」的逻辑，真手感没自动化。
5. 浮窗勾选是用合成事件验证的（真鼠标点击没自动化）；不过点击目标是子按钮，
   `data-tauri-drag-region` 只对它自己的元素生效，所以不会把点击当成拖窗口。
6. 浮窗的字体 / 字号只影响浮窗；浮窗里**没有**自己的主题开关（这是刻意的：主题必须两个窗口一致）。
7. `sync.ts` 走的是 `localStorage` 广播 —— **弱一致性**：主窗口如果正在显示「历史某天」，
   浮窗勾完成会把它重新拉一遍那天（数据是对的，只是多做一次请求）。
