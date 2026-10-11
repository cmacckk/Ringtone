# M6 · 外观设置（字体 / 字号 / 窗口透明度）+ 浮窗拖动换边

一句话：标题栏多了个 `Aa` 按钮，能调字体、字号、主窗口与浮窗各自的透明度；
浮窗不再死贴右边，拖着换边会被记住；顺手把浮窗的几个老缺口补掉。

## 1. 做了什么

| 功能 | 在哪 | 说明 |
| --- | --- | --- |
| 字体 | 外观面板下拉（六款常见中文字体 + 自定义手填 `font-family`） | 主窗与浮窗共用 |
| 字号 | 12~20 px 滑杆（默认 14，旁边有「标准 14」按钮） | 主窗与浮窗共用，右边实时显示 px |
| 主窗口透明度 | 50%~100% 滑杆 | 只影响主窗口 |
| 浮窗透明度 | 50%~100% 滑杆 | 只影响浮窗 |
| 全部恢复默认 | 面板最下面一条 | 字体 / 字号 / 两条透明度一起回默认 |
| 浮窗拖动换边 | 按住浮窗卡片拖到屏幕另一侧 | 松手吸附该边并记住（`ringtone.floatSide` / `ringtone.floatY`） |
| 主窗口换屏跟随 | 把主窗口拖到另一块显示器 | 浮窗落点跟着重算（`Moved` / `Resized` / `Focused(true)`） |

改动落点：

- 新增 `src/appearance.ts`（读 / 存 / 应用 / 跨窗口监听）、`src/AppearancePanel.tsx`（整屏面板）。
- `src/main.tsx` 启动时读一次外观并挂 `storage` 监听；`src/TitleBar.tsx` 多一个 `Aa` 按钮。
- `src/styles.css`：43 处 `font-size` 改成 `calc(Npx * var(--fs-k, 1))`；新增 `--fs-k` / `--font-family` / `--win-opacity`；
  `body` 的 `font:` 简写拆成三行；底色从 `body` 挪到 `.app`。
- `src-tauri/tauri.conf.json`：主窗口加 `transparent: true`。
- `src-tauri/src/lib.rs`：`float_set_side` 命令、`FloatDock` 扩到 10 个字段、落点改信显示器 `scale_factor()`、
  主窗口 `Moved` / `Resized` / `Focused(true)` 时重算落点。
- `src/Float.tsx`：拖动 → 吸附 → 记住边；吸附时用 `floatDock()` 给的屏幕边界，不再自己问显示器。

## 2. 三个有取舍的决定

1. **透明度用「整体合成」而不是系统能力**：Tauri 的 window 插件里没有 `setOpacity`
   （`desktop-schema.json` 里也搜不到 `core:window:allow-set-opacity`），所以窗口建成 `transparent(true)`，
   再让 `body { opacity: var(--win-opacity) }` 整体按比例合成。
   **代价：文字也会一起变淡**（这是能力边界，面板里直接写明了），范围定在 50%~100%（再低就看不清）。
2. **字号只缩放文字，不缩放间距**：把 43 处 `font-size` 换成 `calc(Npx * var(--fs-k, 1))`，
   间距仍是 px。这样改一个变量就能整体缩放文字，而版式不会被拉变形（字变大时间距会被文字撑开一点）。
3. **「贴哪条边」由前端告诉 Rust，落点由 Rust 算**：前端只知道鼠标往哪拖，落点必须按「主窗口那块屏」的
   物理像素算，所以 `float_set_side(side)` 只传方向；反过来 `float_dock()` 把屏幕边界
   （`mon_left/right/top/bottom`）与窗口物理尺寸（`win_w/win_h`）一起给前端，前端吸附时直接用，不再调 `currentMonitor()`。

## 3. 实测证据

外观面板（CDP 走真实 UI：原生 value setter + `input` / `change` 事件）：

| 步骤 | 操作 | 实测 |
| --- | --- | --- |
| 0 | 初始（`localStorage` 里没有记录） | `--fs-k=1`、`body opacity=1`、头部显示 `14 px`、两条滑杆 `100%` |
| 1 | 字号拖到 18 | `--fs-k=1.2857`、头部 `18 px`、存 `fontScale=18` |
| 2 | 字体选楷体 | `--font-family = KaiTi, "楷体", serif` |
| 3 | 主窗口透明度 0.8 | `body opacity=0.8`、显示 `80%` |
| 4 | 浮窗透明度 0.6 | 存 `floatOpacity=0.6`，而**主窗口 opacity 仍是 0.8**（两条互不干扰） |
| 5 | 全部恢复默认 | 回到 14 px / 1 / 1 |

跨窗口：主窗口写 `localStorage` 后浮窗 `body opacity` 变 0.6、主窗口保持 1 ⇒ `storage` 同步生效。
主题同步（新补的）：点主题按钮三次，主窗与浮窗一起走 `dark → auto → light → dark`。

透明度「真的透出桌面」（这是本次最难的取证，用屏幕抓图像素对账）：

| 量 | 值 |
| --- | --- |
| 抓法 | 窗口置顶且可见 → 抓 100% → `hide` 抓桌面底图 → `show` 后设 2% 再抓（PowerShell `CopyFromScreen` + `SetProcessDPIAware`，node 手写 BMP 解析） |
| 桌面底图（同位置） | `rgb(239,239,239)` |
| 2% 实测 | `rgb(235,235,236)` |
| 「透出桌面」预期 | `rgb(235,235,236)`（误差 **0.3**） |
| 「不透明（画布底色）」预期 | `rgb(15,15,15)`（误差 220.8） |

修之前同一套方法的结论相反：15% 时反推出的底色是 `rgb(14,20,29)`，正好等于 `--bg`（`rgb(14,19,29)`）
—— 也就是「界面淡了，底下还是自己那块深灰」，桌面压根没透出来。

浮窗几何（改完缩放来源后稳定，不再随它当前落在哪块屏跳）：

| 调用 | 返回 |
| --- | --- |
| `float_dock()` | `side=right` `collapsed_x=2546` `expanded_x=2158` `y=416` `mon=0,0..2560,1600` `win=402x354` |
| `float_set_side("left")` | `collapsed_x=-388` `expanded_x=0`（同一块屏、同尺寸） |
| 换回 `right` | 又是 `2546` / `2158` ✓ |

（`402 = 268 × 1.5`、露出宽度 `14 = round(9 × 1.5)` —— 主窗口那块屏是 150%，另一块是 100%。）

拖动吸附端到端：合成 `mousedown` 到 `.float-card` → 把窗口挪到 `(300,500)` → 等去抖 400 ms 后
`localStorage.side` 变 `left`、`dragY` 存 `500`、窗口被吸附到 `x=0,y=500`（日志 `浮窗落点：side=left`）。
验完清理了 `ringtone.floatSide` / `ringtone.floatY`，恢复到「贴右边」。

## 4. 踩到的坑（都值得记）

1. **`body` 的背景会被「传播」成画布背景**，那一层不受 `body { opacity }` 影响 ——
   所以「把底色画在 `body` 上」会让透明度只能把界面调暗，桌面永远透不出来。底色必须画在自己的容器上。
2. **`clamp(Number(undefined), 12, 20)` 返回 12**：`NaN` 被当成「夹到区间」返回**下界**，
   于是「没有存过设置」= 最小字号 + 一半透明。要用「不是有限数就回默认值」的写法（`pick`）。
3. **别用窗口实际像素宽反推缩放**：浮窗贴边时横跨两块缩放不同的屏，同一个函数先后量到 `268x236` 与 `402x354`，
   落点就差 134 px。信显示器报的 `scale_factor()`（实测 1.5 / 1 都准）。
4. **vite dev 会把模块服务成空文件**：`src/icons.tsx` 返回 163 字节（只有一行空 sourcemap 注释）→
   主窗口整页白屏；`src/styles.css` 返回 451 字节、`document.styleSheets[0].cssRules` 是 0 → 界面完全没样式。
   `touch` 一下那个文件就恢复（重新 transform），不用重启 vite。
5. **主题 / 外观这类「只读一次」的东西要显式监听跨窗口变化**：外观加了 `storage` 监听、主题漏了，
   结果主窗口切深色后浮窗还是亮的。
6. **屏幕取证要先确认窗口在屏上且置顶**：主窗口被最小化时系统把它挪到 `(-32000,-32000)`，
   抓屏抓到的是别的窗口，结论会被污染（本轮就浪费了一轮）。
7. **文件名大小写**：`Appearance.tsx` 和 `appearance.ts` 在 Windows 上撞名，TS 会报
   `differs ... only in casing` —— 面板文件改叫 `AppearancePanel.tsx`。

## 5. 验证

| 检查 | 结果 |
| --- | --- |
| `cargo fmt --check`（在 `src-tauri/` 下跑） | 0 |
| `cargo check --all-targets` | 0 警告 |
| `cargo clippy --all-targets` | 0 警告 |
| `cargo test --lib` | **114 passed / 0 failed** |
| `pnpm exec tsc --noEmit` | 0 |
| `pnpm exec vite build` | `css 15.04 kB` / `js 279.30 kB` |

截图：`docs/screenshots/m5-m6-appearance-dark.png`、`-light.png`（外观面板）、
`m5-m6-main-dark.png`（标题栏 `Aa` + 筛选排 + 页脚计数）、`m5-m6-float-dark.png`、`-light.png`。

## 6. 已知缺口

1. 浮窗只能贴**左 / 右**两条边，不能贴上下；收起时露出的宽度固定 9 逻辑像素（不能调）。
2. 浮窗换边靠真鼠标拖，CDP 只能验证「按下 → 移动 → 吸附 → 记住边」这段逻辑，真手感没自动化。
3. 字号只缩放文字，间距不跟着放大（间距是 px，只会被文字撑开一点）。
4. 透明度是整体合成，文字也跟着变淡；范围锁在 50%~100%。
5. 字体下拉只有六款 + 手填；字体没装就回退系统默认（面板里没做「这个字体在不在」的检测）。
6. 浮窗透明背景靠 JS 给 `body` 挂 `float-mode` 类，脚本没跑起来时会看到一块白底。
7. 浮窗自己跨屏放置时，按「主窗口那块屏」的缩放算宽度，两块屏缩放不同时看着会比在另一块屏上小一点。
