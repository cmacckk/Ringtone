# M6 · 开机自启 + 重复任务的日期区间（从哪天到哪天）

> 需求原文：「工具没有开机启动选项 还有就是有些任务能不能设置从哪天到哪天
> 比如每天提醒 那么第二天 第三天的任务栏中都显示这条任务」

一句话结论：**「从哪天到哪天」后端本来就支持**（`repeat_rule.until` = 结束日期，区间内每天都会出现在当天的列表里），
缺的是**界面上设不了、也看不出来**——连起始日都改不了。所以这一轮做的是：起始日可编辑（「从」）+ 结束日改名「重复到」，
读到一行就是「从 X 重复到 Y」；另外补上用户要的开机自启开关。

---

## 1. 功能

| 功能 | 入口 | 行为 |
| --- | --- | --- |
| 开机自启 | 设置 → 通用 → 「开机自启」卡 → 勾「开机自动启动」 | 写当前用户注册表 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`，值 = `ringtone.exe --minimized`；登录后**直接待在托盘**，主窗口不弹 |
| 起始日（「从」） | 添加任务的第二行 / 双击标题后的编辑器 | 改这条任务的起始日（重复任务 = 系列第一天），保存后视图跟着跳到那天 |
| 结束日（「重复到」） | 同上（选了频率才出现） | 区间内每天出现在当天列表，**结束日含当天**；留空 = 一直重复 |
| 一键看区间效果 | — | 建「从 10-13 重复到 10-15」后逐天点周条：10-13 / 10-14 / 10-15 都有它，10-16 没有 |

界面读到的样子（添加任务那一行）：

```text
每天  ·  每隔 [1] 天  ·  从 [2026/10/13]  ·  重复到 [2026/10/15]  ·  每天，到 2026-10-15  ·  提醒 [准点]  ·  通知 [飞书][钉钉]
```

## 2. 改动清单

后端（Rust）：

- `src-tauri/Cargo.toml`：新增 `tauri-plugin-autostart = "2"`。
- `src-tauri/src/lib.rs`：
  - 注册插件 `.plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec![MINIMIZED_ARG])))`；
  - 新增 `const MINIMIZED_ARG: &str = "--minimized"` 与 `fn started_minimized()`；
  - setup 里启动参数带 `--minimized` 就 `main.hide()` + 打一行日志。**用 hide 不用 minimize**：最小化会在任务栏留一条。
- `src-tauri/capabilities/default.json`：放行 `autostart:allow-enable` / `allow-disable` / `allow-is-enabled`。
- 重复规则本身**一行没改**（`repeat_rule` 里的 `until` 早就在用）。

前端（TypeScript）：

- `src/Prefs.tsx`：新增「开机自启」卡（在「快捷键」与「导入 / 导出」之间，通用页签从四张卡变五张卡）；
  `readAutostart()` 读不出来时返回 `null`（**不拿 `false` 冒充「没开」**），失败会明说「请重新打开这个面板确认」；
  顺手修掉 `saveSchedule` 那行 `{` 与 `setErr("")` 挤在一起的格式。
- `src/Repeat.tsx`：结束日标签「结束」→「重复到」；新增可选 prop `from`（渲染在「重复到」**前面**，没传就不显示）。
- `src/App.tsx`：
  - `Editing` 加 `origDay`（判断「用户是不是真改了日期」用），编辑态传 `from`；
  - 添加框新增 `addDay` 状态，`startDay = addDay ?? day`，`parseQuick(title, today, startDay)` 走它，
    打开输入框 / 添加成功 / Esc 收起都清回 `null`。
- `scripts/shot.mjs`：新增 `--pre "表达式"`（截图前先跑一段可 await 的多步操作）。

## 3. 实测证据（都是真点真填，CDP 驱动界面）

| 项 | 怎么测的 | 结果 |
| --- | --- | --- |
| 开机自启写入 | 点勾选框后 `reg query HKCU\...\Run` | `Ringtone    REG_SZ    F:\develop\Ringtone\src-tauri\target\debug\ringtone.exe --minimized` ✓ |
| 开机自启关闭 | 取消勾选后再查注册表 | 没有 `Ringtone` 项 ✓（已还原成用户原来的「未开启」） |
| `--minimized` 真的收托盘 | 手动跑 `ringtone.exe --minimized` 看日志 | `[ringtone] 开机自启：已收进托盘（点托盘图标或快捷键可以随时叫出来）` + `[ringtone] 浮窗已显示（主窗口已收进托盘）` ✓ |
| 区间展开 | 建「从 10-13 重复到 10-15」，逐天点周条 | 10-13 ✓ 10-14 ✓ 10-15 ✓（结束日含当天）10-16 ✗ |
| 只改「重复到」 | 在 10-14 编辑（系列起点 10-13），把「重复到」改成 10-14 保存 | 视图**留在 10-14 不跳**（`origDay` 生效）；10-15 不再有它 ✓ |
| 改「从」 | 同一任务把「从」改成 10-12 保存 | 视图跳到 `周一 10-12`，那天的列表里有它 ✓ |
| 输入框默认值 | 打开输入框读「从」 | 默认 = 正在看的那天 `2026-10-11`；改成 10-13 后 Esc 再做一次 → 又回到 10-11（`addDay` 每轮清空）✓ |
| 字段顺序 | 读 `.repeat > *` 的 DOM 顺序 | 无频率：`频率` → `从`；选「每天」后：`频率` → `每隔 N 天` → `从` → `重复到` → `摘要` → `提醒` → `通知` ✓ |
| Esc 收起 | 在标题输入框派发 `keydown Escape` | 输入框关掉 ✓（在 `document` 上派发不会触发——监听在输入框上） |

截图（`docs/screenshots/`）：

- `m5-m6-prefs-autostart-dark.png` / `-light.png`：通用页五张卡，第二张就是「开机自启 已关闭 + 开机自动启动」。
- `m5-m6-range-composer-dark.png` / `-light.png`：添加任务里 `每天 · 每隔 1 天 · 从 2026/10/13 · 重复到 2026/10/15 · 每天，到 2026-10-15`。
- `m5-m6-range-edit-dark.png` / `-light.png`：双击标题后的行内编辑器（`不重复 · 从 2026/10/11 · 提醒 · 通知`）。

测试产生的任务（`区间走查B`、`区间走查·从10-13 到10-15`）已按 id 逐条删掉，`tasks` 回到 17 条（全是用户自己的）。

## 4. 设计取舍

1. **没做「只到某天」的独立字段**：`repeat_rule.until` 就是它，再加一个字段会出现两个真值来源。
2. **「从」交给 `RepeatEditor` 渲染，而不是留在 `extra` 里**：`extra` 排在摘要**后面**，
   渲染出来是「重复到 … 摘要 … 从」，读起来像两件事；现在 DOM 顺序是「从」在「重复到」前面，一行读下来就是区间。
3. **改起始日要跟着换视图，但不改就不换**：所以用 `origDay` 对比，而不是「`editing.day !== day` 就跳」。
   在别的日子编辑重复任务时，行内那天带的是系列起始日，天生不同——那样一保存就会跳到系列第一天，看着像 bug。
4. **`addDay` 每轮清空**：不清的话，上次设的日期会静默影响下一条任务（用户根本看不见它还在）。
5. **开机自启用 hide 不用 minimize**；**读不出系统状态就报不确定**，不猜。
6. **`--minimized` 只认启动参数**，不写注册表以外的地方：想关就在界面上取消勾选，卸载干净。

## 5. 坑

1. **vite dev 把模块服务成空文件**（这轮又踩到，且比之前顽固）：`curl http://localhost:1420/src/App.tsx`
   只有 **163 字节**（只剩一行 sourcemap 注释，`sourcesContent: [""]`）→ 页面 `#root` 为空、全白，
   日志报 `does not provide an export named 'default'`。
   **`touch src/App.tsx` 这次不管用**（还是 163 字节），`printf '\n' >> src/App.tsx`（真的改一个字节）才好。
   教训：这类问题必须造成**真实内容变化**，光改 mtime 不够。
2. **在 `document` 上派发 `keydown` 测不出 Esc**：监听挂在输入框的 `onKeyDown` 上，要往输入框派发。
3. **带 `-RedirectStandardOutput` 的 `Start-Process` 会让 bash 调用一直阻塞到超时**（进程确实起来了，日志是好的）
   ——测第二实例用后台方式起，别等它返回。
4. **debug 版 exe 的 devUrl 指向 vite**：vite 一停，第二实例就是 `chrome-error://chromewebdata/`，
   CDP 查它窗口状态会报 `ERR Origin header is not a valid URL`。要在「vite 还活着」的时候测。
5. 给 `RepeatEditor` 加 `from` 之后忘了删 `extra` 里的旧 label：会渲染出**两套「从」**。
   看 DOM 顺序就能发现（本轮就是这么发现的）。

## 6. 验证

| 项 | 命令 | 结果 |
| --- | --- | --- |
| 格式 | `cargo fmt --check`（在 `src-tauri/` 下跑） | 0 |
| 编译 | `cargo check --all-targets` | 0 警告 |
| 静态检查 | `cargo clippy --all-targets` | 0 警告 |
| 单元测试 | `cargo test --lib` | **146 passed / 0 failed** |
| 前端类型 | `pnpm exec tsc --noEmit` | 0 |
| 前端产物 | `pnpm exec vite build` | `index.html 0.39 kB` / `css 21.61 kB` / `js 311.26 kB` |

## 7. 已知缺口

1. **「从」与「重复到」都是日期，没有「到某天为止的几点」**：区间是按天算的，时刻仍由任务自己的时间决定。
2. **区间只能一条任务一个**：没有「工作日区间」这种按星期组合的模板（要组合就用「每周 + 周选择」）。
3. **没有节假日的例外日期**（比如「这几天跳过」）。
4. **开机自启只写 HKCU**：装在多用户机器上要给别的用户开，得各自登录后各勾一次（也正因如此不需要管理员权限）。
5. **`--minimized` 只在启动时看参数**：已经跑着的时候再带参数启动是第二个实例，不会把第一个叫回来（托盘/快捷键才是入口）。
6. **界面语言只有中文**。
7. **安装包已重打（4,877,590 字节 / SHA256 `0b2e8c78…`），但这份代码还没提交**：
   护栏不让我往 `master` 提交/推送，所以 release 的附件与仓库源码要等用户自己提交后才对得上（见 `docs/M6-package-report.md`）。
