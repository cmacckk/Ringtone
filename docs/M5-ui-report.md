# M5 · 界面改造报告（kite 风格）

> 目标：把「能用但丑」的界面改成和 kite 待办同一种视觉语言。
> 范围：只动界面与两个只读查询；**不做安装包**（用户明确说先不弄）。
> 结论：**已达成**。深浅两套主题、待办页 / 添加态 / 推送设置页 / 最小窗口四种形态都截图自查过；
> 四条 Rust 检查 + 前端 tsc/build 全绿（`cargo test --lib` 92 passed）。

## 一、用户拍板的三件事

| 项 | 选择 | 影响 |
| --- | --- | --- |
| 窗口外壳 | **自绘标题栏**（`decorations: false`） | 少了系统标题栏那条灰边，标题左、图钉/最小化/关闭右 |
| 顶部信息块 | 下一条提醒倒计时 **和** 今日完成进度 **都要** | 顶部三格：推送设置按钮 + 下一条提醒 + 今日进度 |
| 主题 | **深色 + 跟随系统亮色** | 颜色全走 CSS 变量，亮色只覆盖变量 |

## 二、丑在哪：4 个结构性决定

好看不是靠配色花哨。对比参考站后，差距集中在四处结构：

1. **窄竖窗，不是宽横窗**——原来 980×680 像后台管理页；现在 460×720（最小 400×560）。
2. **顶部要有「大号数字」**——人一眼要看的是「还有多久」「今天差几条」，不是一堆按钮。
   小灰标签 + 大号数值 + 一行副标题。
3. **周条代替一行日期**——7 天平铺、选中日浅色圆角块、**有安排的那天右上角一个橙点**，
   不用点进去就知道哪天有事。两侧箭头整周翻页。
4. **列表是整块圆角卡片**——自绘圆形勾选框、15px 主标题、行间 1px 暗分隔、底部一行全宽「+ 添加任务」，
   而不是原生复选框 + 原生输入框堆叠。

## 三、改动清单

### 新增

| 文件 | 作用 |
| --- | --- |
| `src/day.ts` | 日期工具的**唯一出处**：`pad/splitDay/toDay/shiftDay/weekdayOf/shortDate/startOfWeek/weekDays/hmFromMs/msFromHm/humanSpan/fireAtLabel/advanceLabel`。**不用 `toISOString()`**（UTC 切日会在晚上 8 点后算成明天）。周一为周始。 |
| `src/icons.tsx` | 内联 SVG 图标（勾、加、左右箭头、图钉、最小化、关闭、滑杆、警告、铃铛、时钟）。**不引图标库、不用 emoji**——emoji 在不同系统长得不一样，会毁掉版式。 |
| `src/TitleBar.tsx` | 自绘标题栏：`data-tauri-drag-region` 拖动；图钉=置顶（状态存 `localStorage`）；最小化；关闭=**隐藏到托盘**（不退出，调度线程继续跑）。 |
| `src/WeekStrip.tsx` | 周条：7 天 + 有任务的天右上角橙点 + 整周翻页。 |
| `scripts/shot.mjs` | 用 CDP 给正在跑的窗口截图（深浅两套、可先点某个按钮、可量尺寸）。见第五节。 |

### 修改

| 文件 | 改了什么 |
| --- | --- |
| `src/styles.css` | **整体重写**。颜色/圆角/间距全走 CSS 变量；深色为默认 + `@media (prefers-color-scheme: light)` 覆盖亮色；覆盖任务页与推送设置页全部类。 |
| `src/App.tsx` | 按 kite 结构重排：标题栏 → 顶部三格 → 周条 → 失败横幅 → 卡片列表 → 添加行 → 底部提示。新增「下一条提醒」「今日进度」「周条圆点」三处数据。 |
| `src/api.ts` | 新增 `DayCount`、`NextReminder` 类型与 `countTasksByDay()`、`nextReminder()`。 |
| `src/Settings.tsx` | 不动逻辑，套进新的外壳（`.panel` 自己滚 + 顶部「返回待办」）。 |
| `src-tauri/tauri.conf.json` | 窗口 460×720、最小 400×560、`decorations: false`、`shadow: true`、居中。 |
| `src-tauri/capabilities/default.json` | 放行 `core:window:allow-start-dragging` / `allow-minimize` / `allow-close` / `allow-hide` / `allow-set-always-on-top`。 |

### 新增的后端接口（两个只读查询）

| 命令 | 作用 |
| --- | --- |
| `count_tasks_by_day(from_day, to_day)` | 区间内每天的「总数 / 未完成数」。`db::count_by_day` 用 `GROUP BY day`，**只返回有任务的天，不补 0 行**。两个日期都过 `validate_day`，`from > to` 直接报错。 |
| `next_reminder()` | 下一条将要响的提醒（`db::next_reminder`）。 |

两个设计要点（写在代码注释里）：

- `count_by_day` 能直接比字符串是因为 **`tasks.day` 一律补零到 10 位**（见 README 设计约定）。
- `next_reminder` **必须查 `occurrences` 而不是 `tasks.due_at_ms`**：提前提醒的下一次触发点是 `fire_at_ms`，
  看 `due_at_ms` 会显示成偏晚。同时过滤 `done = 0`（已完成的不算）与 `fire_at_ms >= now`，视线范围 7 天。

## 四、验证证据

| 项 | 结果 |
| --- | --- |
| `cargo fmt --check` | 0 差异 |
| `cargo check --all-targets` | Finished，0 警告 |
| `cargo clippy --all-targets` | 0 警告 |
| `cargo test --lib` | **92 passed / 0 failed**（新增 3 个：`count_by_day_groups_days_and_splits_done`、`count_by_day_boundaries_are_inclusive`、`next_reminder_prefers_advance_point_and_skips_done`） |
| `pnpm exec tsc --noEmit` | 0 |
| `pnpm exec vite build` | 成功，css 9.67 kB（gzip 2.49）/ js 254.90 kB（gzip 78.89） |
| 实机：周条橙点 | **截图里 10-08 右上角有橙点** → 说明前端驼峰 `fromDay/toDay` ↔ 后端 `from_day/to_day` 的参数名转换是通的（这一条不验证就会静默拿不到数据） |
| 实机：下一条提醒 | 顶部显示「35 分钟后 / 检查邮件和消息」，与库里的 `fire_at_ms` 一致 |
| 实机：今日进度 | `1/5` + 未完成行的勾选框空心、已完成行蓝底白勾 + 删除线 |
| 虚拟最小窗口 | 用 CDP 把视口设成 400×560 截图，顶格/周条/列表都不截断 |

截图（`docs/screenshots/`）：

| 文件 | 内容 |
| --- | --- |
| `m5-dark.png` / `m5-light.png` | 待办页深色 / 亮色 |
| `m5-composer-dark.png` | 点开「+ 添加任务」后的输入态（回车添加、Esc 收起） |
| `m5-settings-dark.png` / `m5-settings-light.png` | 推送设置页深色 / 亮色（钉钉 + 飞书两张卡） |
| `m5-narrow-dark.png` | 400×560（窗口最小尺寸）时不截断 |

截图里的待办是**临时造的数据**，看完已按明确 id 删除（连同 `occurrences` / `push_jobs`）；
渠道配置保留原样（钉钉 priority 100 主渠道、飞书 200）。

## 五、踩到的坑（都写成了可复用做法）

### 1. 给 Tauri 窗口截图：三种办法都不行，最后用 CDP

| 办法 | 结果 |
| --- | --- |
| `CopyFromScreen` 直接截屏 | **截到的是别的窗口**——`SetForegroundWindow` 从后台进程调用会被系统拒绝，窗口没到最前，而那块屏幕区域正好被别的程序盖着 |
| `PrintWindow(hwnd, dc, 0)` | WebView2 走 DirectComposition，出来是白屏/黑屏 |
| `ShowWindow(SW_SHOW=5)` 后 `GetWindowRect` | 窗口当时是最小化的，`SW_SHOW` 不恢复，坐标停在 `-48000,-48000`（顺手记下：要用 `SW_RESTORE=9`） |
| **`PrintWindow(hwnd, dc, 2)`（`PW_RENDERFULLCONTENT`）** | 能出内容，但只能拿深色——没法验亮色主题 |
| **CDP（`scripts/shot.mjs`）✅** | 图是 WebView2 自己吐的：不抢焦点、不怕被遮挡、能固定 460×720 视口、能用 `Emulation.setEmulatedMedia` **临时切亮色**（不用改系统主题） |

前置：应用要以调试端口启动

```bash
WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=9223" pnpm tauri dev
node scripts/shot.mjs                                   # 深浅各一张
node scripts/shot.mjs --click ".hero-btn" --name settings
node scripts/shot.mjs --eval "表达式"                     # 量尺寸/找溢出，不截图
```

### 2. PowerShell 脚本的 param 块里不能写中文注释

`param([string]$Mode = "print"   # 注释…)` 让 `powershell -File` 直接报
`MissingEndParenthesisInFunctionParameterList`。原因是脚本存成 UTF-8 无 BOM，Windows PowerShell 5.1
按 GBK 读，中文注释的字节被解成别的字符把语法弄坏了。**做法：param 块只用英文，中文注释放函数体里**
（或者用 `-Encoding UTF8` 写文件）。

### 3. 前端别自己算日期

`day.ts` 的注释里写死了原因：`toISOString()` 是 UTC，晚上 8 点后（东八区）算出来的「今天」会变成昨天，
周条和圆点会整体错一天。前端的「今天」一律从后端 `today_local()` 拿。

## 六、已知缺口

1. **亮色主题是 CDP 模拟验证的**，没有真的把 Windows 切成浅色主题跑一遍（那要改系统设置，没做）。
2. 界面按钮**仍然没有自动化用例**（和 M4 一样，靠截图自查 + 人工点）。
3. 列表里「未定时刻」的任务显示 `—`（用了 em dash），没有任何列排序控制。
4. 30 秒轮询只刷新「下一条提醒 / 失败数 / 倒计时基准」；**外部直接改库**不会刷新列表和周条圆点
   （正常使用都是走界面改，不走库）。

## 七、M5 还没做的（按用户要求先不做安装包）

- 重复规则（照抄滴答清单的 RRULE + 提前 N 分钟/天）
- 快速录入（自然语言识时间，如「明天九点交周报」）
- 渠道密钥加密（现在 `credential_enc` 是明文）
- 打包分发（NSIS 安装包）与开机自启
