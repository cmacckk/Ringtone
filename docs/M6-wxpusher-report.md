# M6 报告：微信推送（WxPusher）渠道 + 字体可选全部已安装字体

> 这一轮做两件事：① 新增第三个推送渠道 **WxPusher**（消息进微信）；② 外观里的字体下拉从「6 款硬编码」
> 改成「**本机已安装的全部字体**」。
>
> 用户只给了 UID（`UID_` 开头），没给 **appToken** —— 所以本轮验证的是**失败路径**（服务端真回了业务错误），
> 「真发到微信」这一步要等 appToken 填进设置面板后才能确认。

## 1. 做了什么

| # | 功能 | 现在的行为 |
| --- | --- | --- |
| 1 | WxPusher 渠道 | 设置 → 推送渠道多一张「微信推送（WxPusher）」卡：UID（多个用逗号/空格隔开，≤ 20 个）+ appToken（`AT_` 开头，留空 = 不改动已存的）。支持「检查配置 / 发测试消息 / 设为主渠道 / 停用 / 删除」，以及和钉钉飞书一样的今日配额与发送记录 |
| 2 | 投递链路 | 与另外两个渠道完全同构：入队 → 限流闸门 → 发送 → 可重试就退避（5s/30s/2min）→ 不可重试就**降级到下一个启用渠道** → 全挂了用本机通知兜底并记一条失败 |
| 3 | 字体下拉 | 后端读 Windows 注册表拿**全部已安装字体**（这台机器 **215 项**，按字母排序），下拉 217 项（+「系统默认」「自己填…」）。存的只是字体家族名（如 `KaiTi`），写 CSS 时 `fontCss()` 自动补引号 + 叠系统字体回退 |

## 2. WxPusher 的接口事实（都核对过官方文档 + 实测）

| 项目 | 值 |
| --- | --- |
| 发送 | `POST https://wxpusher.zjiecode.com/api/send/message`（`Content-Type: application/json`） |
| 请求体 | `{"appToken":"AT_xxx","content":"...","summary":"...","contentType":1,"uids":["UID_xxx"]}` |
| 成功响应 | `{"code":1000,"msg":"处理成功","data":[{"uid","messageContentId","sendRecordId",...}],"success":true}` |
| 官方限制 | `summary` ≤ 20 字符、`content` ≤ 40000 字符且 ≤ 65535 字节、单次 ≤ 2000 UID、**接口 QPS ≤ 1**、单个 UID 每天 ≤ 2000 条 |
| 业务码 | `1000` 成功、`1002` appToken 无效/未登录、`1001` **通用**业务错误 |

**实测发现（重要）**：拿一把假 appToken（`AT_invalid_token_for_verification`）真发一次，服务端回的是
`code=1001 msg=appToken不正确` —— 不是 `1002`。所以：

- 不能靠 `code` 判断「是 token 错还是内容错」，**要看服务端的 `msg`**；
- 界面上原来给 `1001` 加的解说是「参数错误：通常是内容超长或 UID 格式不对」，对 token 错的情况是**误导**，已删掉，
  现在只对 `1002` 给提示，其余原样透出 `code` 与 `msg`。

## 3. 实测证据

| 场景 | 结果 |
| --- | --- |
| 界面卡片渲染 | 设置 → 推送渠道的卡片顺序：钉钉 / 飞书 / **微信推送（WxPusher）** / 发送记录；未配置时显示「还没配置」 |
| 保存 | 填 `UID_Kk0…`（用户真实 UID）+ 假 appToken → 提示「已保存并启用」，卡片头变「已启用 · appToken 已存 · 今日已发 0 条」，UID 回填正确 |
| 真发一次（失败路径） | `发测试消息` → `[BadRequest/HTTP 200] WxPusher 返回 code=1001 msg=appToken不正确`；`send_log` 记两条 `test_failed`，detail 带原始 code 与 msg |
| 字体数量 | `list_fonts` → **215** 项（`0xProto Nerd Font…`、`KaiTi`、`Microsoft YaHei` 都在） |
| 字体下拉 | 主窗口 217 项、浮窗 217 项（浮窗首项是「跟主窗口一样」）；选 `KaiTi` → `--font-family` 变成 `"KaiTi", system-ui, -apple-system, "Segoe UI", "Microsoft YaHei", "PingFang SC", sans-serif`，`localStorage` 里存的是 `KaiTi` |
| 检查 | `cargo fmt --check` 0、`cargo check --all-targets` 0 警告、`cargo clippy --all-targets` 0 警告、`cargo test --lib` **160 passed**、`pnpm exec tsc --noEmit` 0、`vite build` 通过 |

## 4. 挖出来的三个真问题

1. **字体列表静默回落到兜底那 6 项**（最坑的一个）。`RegEnumValueW` 给了 4 字节数据区，而字体表里的值都是
   字体文件名，于是每次调用都返回 `ERROR_MORE_DATA (234)` —— 名字其实**已经填进缓冲区了**，代码却把 234 当失败跳过，
   读完整个注册表得到的是空列表，然后回落兜底。原来的测试只断言「非空」，兜底让它照样绿。
   修法：数据区传 `null`（只要名字），并把 234 当成功；另加 `reads_real_fonts_from_registry`（要求 > 20 项）拦住「静默回落」。
2. **`1001` 的提示文案误导**（见第 2 节）。
3. **`summary` 必须 ≤ 20 字符**：官方按字符算，超一个整条就报错。已按 `chars().take(20)` 硬截（不加省略号，
   省略号也会占掉一个字符）。

## 5. 环境坑（下次会再遇到）

- 用户已经用**装好的安装包**（`D:\Ringtone\ringtone.exe`）在跑，它占着同一个 WebView2 用户数据目录
  ⇒ dev 实例启动会报 `failed to create webview: WebView2 error: HRESULT(0x8007139F) 组或资源的状态不是执行请求操作的正确状态`。
  验证时给 dev 实例加了两个环境变量隔离：
  `WEBVIEW2_USER_DATA_FOLDER=%TEMP%\rt-wv2-dev` + `RINGTONE_DATA_DIR=%TEMP%\rt-demo2`（演示库，不碰真实数据），
  再加 `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9223` 走 CDP。
- `Start-Process -Environment` 是 PowerShell **7** 的参数，5.1 没有 ⇒ 用 `$env:X='...'; Start-Process ...` 让子进程继承。

## 6. 还没做 / 已知缺口

1. **真实送达未验证**：没有 appToken，「真发到微信」这一步没跑过（失败路径已验证）。你把 `AT_` 填进设置面板
   点一次「发测试消息」就能确认。
2. **只发文本（`contentType=1`）**：WxPusher 支持 html / markdown，这里没用（提醒内容很短，纯文本够了）。
3. **主题（topic）不支持**：只按 UID 发；要群发得加 `topicIds`。
4. **QPS ≤ 1 只是靠 10 条/分的闸门近似**：真在同一秒内挤 2 条（比如两个任务同时到点）时会撞 QPS 限制，
   那时候会拿到限流类错误并**推迟**——不会丢提醒，但会晚几十秒。
5. **字体列表只在启动时读一次**（`OnceLock` 缓存）：新装了字体要重启应用才出现在下拉里。
6. **字体是按家族名铺开的整张表**（215 项，含 Bold/Italic 之类的子族名）：没有做「只列常规字重」的过滤，
   列表偏长。用搜索（浏览器的 select 自带键盘匹配）能快一点。
7. 演示数据目录（`%TEMP%\rt-demo2`）与隔离的 WebView2 目录是本轮验证用的，可以随时删。
