# M6 · 界面增强（主题切换 / 列表筛选 / 底部计数 / 贴边浮窗）报告

> 需求来自用户（m02311，要点）：① 右上角主题切换按钮；② 列表能筛选未完成 / 已完成 / 全部，默认全部；
> ③ 右下角展示今天多少条、全部多少条；④ 类似 kite 的浮窗 —— 最小化时展示待办、自动靠边一段时间后隐藏、
> 鼠标移到差不多位置就展开，**必须在最前面，保证能看到**。
> 顺带修掉上一份报告（`docs/M6-channels-report.md` 第七节）记的缺口 1。

## 一、做了什么

| 功能 | 行为 | 入口 |
| --- | --- | --- |
| 主题切换 | 跟随系统 → 亮色 → 深色 → 跟随系统，循环切换，存 `localStorage` | 标题栏置顶按钮**左边**的按钮 |
| 列表筛选 | 全部 / 未完成 / 已完成，默认**全部**，后两个带数字角标 | 周条下面一排胶囊按钮 |
| 底部计数 | `今天 N 条 · 全部 M 条` | 窗口底部左下角（页脚） |
| 贴边浮窗 | 主窗口最小化 / 收进托盘时出现；贴主窗口那块屏的右边缘；6 秒后自动收起成 9px 的边；鼠标进入展开、离开 2.5 秒再收起；点击恢复主窗口 | 自动，无需操作 |
| 缺口 1 修复 | 没有渠道可降级时，也写一条「渠道失败」到「发送记录」 | 后台行为，界面在推送设置的发送记录里看到 |

三个计数口径（**故意不一样，别把它们统一**）：

| 数字 | 含义 | 来源 |
| --- | --- | --- |
| 顶部「今日进度 X/Y」 | 今天**有提醒点**的条数（重复任务按展开算） | `count_by_day(today, today)` |
| 底部「今天 N 条」 | 同上（同一个数，只是换个位置显示） | 同上 |
| 底部「全部 M 条」 | `tasks` 表总行数（1 条重复任务算 **1** 条，不按展开算） | `SELECT COUNT(*) FROM tasks` |

## 二、四个有取舍的决定

1. **「全部」不按展开算**。理由：用户问的是「一共有多少条待办」——指的是他敲进去的那些任务，不是
   「未来 7 天里会响多少次」。按展开算的话，一条「每天」的任务在 7 天窗口里就是 7 条，数字会随窗口漂。
2. **筛选不参与计数**。筛到「未完成」时底部仍然显示今天的真实总数，否则「今天几条」会随筛选变，跟
   「今日进度」对不上，反而看不懂。筛选只作用于列表。
3. **浮窗用真窗口，不用 always-on-top 的 WebView 内嵌层**。Tauri 的窗口本身就是 OS 窗口，
   `always_on_top` + `skip_taskbar` 是系统级承诺；内嵌一层只能管住自己窗口内。代价是多一个窗口、
   多一份 `capabilities` 放行。
4. **贴边用「收起 → 鼠标移上去展开」而不是「悬停时才创建窗口」**。窗口常驻（只是 `hide()`），
   鼠标进入就能立刻往左滑，不用等窗口创建 + 页面加载（那会明显顿一下）。

## 三、改动清单

后端（`src-tauri/src/`）：

- `db.rs`：`TaskStats { today_total, today_undone, all_total }` + `task_stats()`；「今天」复用
  `count_by_day(conn, &today, &today)`（重复任务照样展开），"全部" 用 `COUNT(*)`。
- `commands.rs`：`task_stats`、`float_dock`、`show_main` 三个命令。
- `lib.rs`：`build_float()` 建 `float` 窗口（`index.html?float=1`，268×236，无边框 / 透明 / 置顶 /
  不进任务栏）；`sync_float()` 按主窗口可见性决定显示/隐藏并**先把窗口摆到收起位**；
  `compute_dock()` 算贴边坐标（物理像素）；`LAST_DOCK` 缓存；`on_window_event` 只处理 `main`
  的窗口事件（`label() != "main"` 直接 return，否则浮窗的 `CloseRequested` 也会被吞掉）。
- `deliver.rs`：`exhaust()` 在本地兜底之前用块作用域加锁补写渠道失败行
  （`Mutex` 不可重入，**必须**用 `{}` 圈住，否则和后面的 `state.conn.lock()` 死锁）。
- `capabilities/default.json`：`windows: ["main", "float"]` + 窗口位置/尺寸/置顶/显示相关权限。

前端（`src/`）：

- 新增 `theme.ts`（`loadTheme / applyTheme / saveTheme / nextTheme`）、`Float.tsx`（浮窗页面）。
- `main.tsx`：渲染前先 `applyTheme(loadTheme())`；按 `isFloatWindow()` 决定渲染 `<App />` 还是
  `<FloatWindow />`（**没有**在 `App.tsx` 里做条件分支，避免条件调用 hooks）。
- `api.ts`：`TaskStats`、`taskStats()`、`floatDock()`、`showMain()`。
- `App.tsx`：`filter` state + `.filters` 一排按钮 + `shown` 过滤列表 + 页脚计数 + `reload()` 里加
  `taskStats()`。
- `TitleBar.tsx`：主题按钮（图标随模式换 `IconAuto / IconSun / IconMoon`）。
- `styles.css`：亮色 token 拆成 `:root[data-theme="light"]` + `:root[data-theme="auto"]` 的媒体查询；
  新增 `.filters`、`.foot-*`、`.float*` 三段。

## 四、实测证据（都是真实运行，不是单测）

| 验证项 | 做法 | 结果 |
| --- | --- | --- |
| 主题循环 | CDP 连点标题栏主题按钮，读 `dataset.theme` + `body` 背景色 | `light / rgb(242,244,248)` → `dark / rgb(14,19,29)` → `auto / rgb(14,19,29)`；结束后 `localStorage = "auto"`（已还原，没留痕） |
| 筛选 | CDP 依次点三个按钮，数 `.task` 行数 | `全部 4 / 未完成 4 / 已完成 0`，筛到空时出现「这个筛选下没有任务」 |
| 底部计数 | CDP 读 `.foot` 文本 | `…今天 4 条 · 全部 6 条`（6 = 库里的真实任务数，4 = 今天的提醒点数） |
| 浮窗出现 | CDP 点主窗口最小化按钮 | 日志 `[ringtone] 浮窗已显示（主窗口已最小化）` |
| 浮窗落点 | `outer_position()` 读回 | 收起 `(2546, 416)`、展开 `(2158, 416)`，窗口物理 402×354；落点所在屏 `DISPLAY20`（2560×1600）与主窗口所在屏一致（主窗口中心 1291,770 在该屏内） |
| 悬停展开 | PowerShell 把真实鼠标停在浮窗露出的边上 1.4 秒 | 位置从收起位变展开位 `(2158, 416)`，鼠标移回后 2.5 秒回到收起位 |
| 点击恢复 | CDP 点浮窗卡片 | 调 `show_main`，主窗口 unminimize + 置顶 + 浮窗隐藏 |
| 浮窗内容 | CDP 截浮窗页面（`--url "float=1" --width 268 --height 236`） | `docs/screenshots/m5-m6-float-{dark,light}.png`：`0/4 已完成 · 全部 6 条` + 4 条未完成事项带时刻 + 「今天 4 条 / 点这里打开主窗口」 |
| 缺口 1 复验 | 把钉钉 `access_token` 改坏（加签密钥不动）+ 插一条 `notify_channels = ["dingtalk"]` 的任务，45 秒后到点 | 日志 `已入队 → 全渠道失败，已用本地通知兜底（[Auth/HTTP 200] errcode=300005）`；`send_log` 得到 **2 行**：`channel_id=2 failed「…没有可降级的渠道」` + `channel_id=null sent「…已用本地通知兜底」`；`occurrences.state=sent`，到点后 **3292 ms**；`push_jobs.attempts=1`（Auth 不重试、不降级，符合 m02085 的决定）；库已逐字节还原、测试任务已删 |

四条检查（`src-tauri/`）：`cargo fmt --check` = 0、`cargo check --all-targets` 无警告、
`cargo clippy --all-targets` 无警告、`cargo test --lib` = **114 passed / 0 failed**。
前端：`pnpm exec tsc --noEmit` = 0、`pnpm exec vite build` 成功（css 13.46 kB / js 272.03 kB）。

## 五、踩到的坑（按代价排序）

1. **PowerShell 5.1 量坐标是错的**。它是 DPI-unaware，`GetWindowRect` 的返回值被缩放：窗口真在
   `(2546, 416)` 时它报 `(3819, 624)`（= ×1.5）。`SetProcessDPIAware()` 在 PS 5.1 里**没救回来**
   （GUI 子系统早在启动时就定了）。我因此花了好几轮去查一个不存在的「坐标被乘 1.5」的 bug。
   权威读数：窗口自己报的 `outer_position()`。
2. **Tauri 的 `scale_factor` 会撒谎**。这块 150% 的屏上它报 `1`（`devicePixelRatio` 是 2，
   真实是 1.5）。任何 `LogicalPosition` 换算都会算歪 —— 这就是浮窗一开始贴着屏幕外沿的原因。
   改用物理像素 + 从 `outer_size / 设计宽` 反推缩放后一次到位。
3. **最小化会把主窗口送到屏幕外**（`-32000, -32000`），这时 `current_monitor()` 给的是 `size = 0`
   的假显示器（算出 y = 0，x 每次都不一样：3438 → 2551 → …）。必须在主窗口还正常时缓存落点。
4. **`on_window_event` 不区分窗口**：`CloseRequested` 的「隐藏到托盘」逻辑会把浮窗的关闭也吞掉，
   加 `window.label() != "main" { return }` 才行。
5. **浮窗成了第二个 CDP 页面**，`scripts/shot.mjs` 的 `pages[0]` 变成浮窗页，早先的 `--eval` 因此
   返回 `{}`（浮窗里没有 `.titlebar`）。脚本加了 `--url` 过滤，且默认挑**不含** `float=1` 的页面。
6. **`main.tsx` 不能把条件渲染放进 `App.tsx`**：`useState`/`useEffect` 会变成条件调用。放在
   渲染入口按 `isFloatWindow()` 二选一才安全。

## 六、已知缺口

1. 浮窗只贴**右边**，不能拖动换边；收起时露出的宽度固定 9 逻辑像素。
2. 浮窗的透明背景靠 JS 给 `body` 挂 `float-mode` 类；脚本没跑起来时会看到一块白底（约 1 帧）。
3. 落点是「主窗口所在那块屏」；开着浮窗时把主窗口拖到另一块屏，**要等下一次最小化**才会换屏。
4. 浮窗每 30 秒自己拉一次数据，不是后端推送 —— 刚勾完的任务最多慢 30 秒才在浮窗里消失。
5. 主题按钮只有一个循环切换（不能直接点图标选）；系统主题变化时「跟随系统」不会主动重绘背景色
   （CSS 变量靠媒体查询生效，多数情况下够用）。
6. 浮窗位置相关的坐标计算**没有单测**（依赖真实显示器），只有上面那张实测表。
7. 「发送记录」界面里现在会出现同一实例的两行（渠道失败 + 本地兜底），文案上是分开的，
   但如果用户按「一条提醒一行」来读会疑惑 —— 暂时靠 `detail` 里的「没有可降级的渠道」区分。
