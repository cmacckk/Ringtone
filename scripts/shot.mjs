// 用 CDP（Chrome DevTools Protocol）给正在运行的 Ringtone 截图。
//
// 为什么不直接截屏窗口：
//   1. Tauri 窗口是无边框自绘的，PrintWindow 对 WebView2（DirectComposition）常出白屏；
//   2. CopyFromScreen 要求把窗口抬到最前，会抢焦点，而且别的窗口盖在上面就截错了（踩过一次）。
// CDP 是 WebView2 自己吐出来的图：不用抢焦点、不受遮挡影响，还能临时把
// prefers-color-scheme 切成亮色来验证浅色主题——**不用去改系统主题设置**。
//
// 前置：应用要以远程调试端口启动，例如
//   WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=9223" pnpm tauri dev
//
// 用法：
//   node scripts/shot.mjs                                  # 深色 + 亮色各一张
//   node scripts/shot.mjs --theme dark --out docs/shots    # 只要深色，换输出目录

import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const opts = new Map();
// 参数解析：`--key value`；`--flag` 后面跟的是另一个 `--xxx`（或到头了）时，当成开关（值 "1"）。
// 注意 `--flag` 不能空着不处理：以前那种「按位置两两配对」的写法会让 `--reload` 把下一个 `--name` 吃掉，
// 结果 name / eval 这些后面的参数全都读不到，而且不报错（图会默默存成 `dark.png`）。
for (let i = 2; i < process.argv.length; i++) {
  const a = process.argv[i];
  if (!a.startsWith("--")) continue;
  const next = process.argv[i + 1];
  if (next === undefined || next.startsWith("--")) {
    opts.set(a.slice(2), "1");
  } else {
    opts.set(a.slice(2), next);
    i++;
  }
}
const port = opts.get("port") ?? "9223";
const outDir = opts.get("out") ?? "docs/screenshots";
const wanted = opts.get("theme") ?? "both";
const themes = wanted === "both" ? ["dark", "light"] : [wanted];
const prefix = opts.get("prefix") ?? "m5-";
// --name xxx → 输出 m5-xxx-dark.png（`--prefix ""` 可以去掉前缀，README 用的那批图就是这么拍的）；--click "选择器" 可在截图前先点一下（用于截弹层/设置页）；--dbl "选择器" 双击进编辑态
const name = opts.get("name");
const clickSel = opts.get("click");
// --eval "表达式"：先跑一段 JS 把结果打出来（用于量尺寸、找溢出的元素），不做截图
const evalExpr = opts.get("eval");
// --url "子串"：有多个窗口（主窗口 + 待办浮窗）时按地址挑一个，例如 --url "float=1"
const urlFilter = opts.get("url");

const list = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
const pages = list.filter((t) => t.type === "page" && t.webSocketDebuggerUrl);
// 有两个窗口（主窗口 + 待办浮窗）时**默认挑主窗口**：
// 浮窗那个页面里没有标题栏、没有列表，默认挑错的话 --eval/--click 会全部找不到元素。
const page = urlFilter
  ? pages.find((t) => t.url.includes(urlFilter))
  : (pages.find((t) => !t.url.includes("float=1")) ?? pages[0]);
if (!page) {
  console.error(`没在 127.0.0.1:${port} 找到页面目标（应用没起来？端口没开？）`);
  console.error(`当前有的目标：\n${pages.map((t) => `  ${t.url}`).join("\n") || "  （一个都没有）"}`);
  process.exit(1);
}

const ws = new WebSocket(page.webSocketDebuggerUrl);
const pending = new Map();
let seq = 0;

function call(method, params = {}) {
  const id = ++seq;
  ws.send(JSON.stringify({ id, method, params }));
  return new Promise((resolve, reject) => pending.set(id, { resolve, reject }));
}

ws.addEventListener("message", (ev) => {
  let msg;
  try {
    msg = JSON.parse(ev.data);
  } catch {
    return; // 不是 JSON 的消息（CDP 事件之外的东西）直接忽略
  }
  if (msg.id && pending.has(msg.id)) {
    const p = pending.get(msg.id);
    pending.delete(msg.id);
    if (msg.error) p.reject(new Error(`${msg.error.message} (${method ?? ""})`));
    else p.resolve(msg.result);
  }
});

await new Promise((r) => ws.addEventListener("open", r, { once: true }));
await call("Page.enable");

// --reload：先刷新页面再操作。页面会记住上次停在哪个界面（外观面板 / 设置页），
// 不刷新的话上一个截图流程留下的状态会让 --click 找不到元素。
if (opts.has("reload")) {
  await call("Runtime.evaluate", { expression: "location.reload()" });
  await new Promise((r) => setTimeout(r, 3500));
}

if (clickSel) {
  // 支持一串选择器，用 ";" 分隔，按顺序点（例如先「返回待办」再「添加任务」）
  for (const sel of clickSel.split(";").map((s) => s.trim()).filter(Boolean)) {
    const probe = await call("Runtime.evaluate", {
      expression: `(() => { const el = document.querySelector(${JSON.stringify(
        sel,
      )}); if (!el) return "NO_MATCH"; el.click(); return "CLICKED"; })()`,
      returnByValue: true,
    });
    console.log(`CLICK ${sel} → ${probe.result.value}`);
    if (probe.result.value !== "CLICKED") process.exit(1);
    await new Promise((r) => setTimeout(r, 350)); // 让弹层渲染完
  }
  await new Promise((r) => setTimeout(r, 300));
}

// --pre "表达式"：截图前先跑一段（可以是多步操作，支持 await），跑完继续截图。
// 例如先把设置面板切到「通用」页签、或者往输入框里填好值再拍。
const preExpr = opts.get("pre");
if (preExpr) {
  const r = await call("Runtime.evaluate", {
    expression: preExpr,
    returnByValue: true,
    awaitPromise: true,
  });
  console.log(`PRE → ${JSON.stringify(r.result.value ?? r.result.description)}`);
  await new Promise((r) => setTimeout(r, 400));
}

// --dbl "选择器"：双击（进入行内编辑，展示重复规则编辑器）。
// 用合成事件就够了：React 的 onDoubleClick 监听的就是 dblclick，且事件会冒泡。
const dblSel = opts.get("dbl");
if (dblSel) {
  const probe = await call("Runtime.evaluate", {
    expression: `(() => { const el = document.querySelector(${JSON.stringify(
      dblSel,
    )}); if (!el) return "NO_MATCH"; el.dispatchEvent(new MouseEvent("dblclick", { bubbles: true, cancelable: true, view: window })); return "DBLCLICKED"; })()`,
    returnByValue: true,
  });
  console.log(`DBL ${dblSel} → ${probe.result.value}`);
  if (probe.result.value !== "DBLCLICKED") process.exit(1);
  await new Promise((r) => setTimeout(r, 350));
}
// 按逻辑像素给一个固定视口：截图尺寸与窗口实际大小、系统缩放都解耦
const vw = Number(opts.get("width") ?? 460);
const vh = Number(opts.get("height") ?? 720);
await call("Emulation.setDeviceMetricsOverride", {
  width: vw,
  height: vh,
  deviceScaleFactor: 2,
  mobile: false,
});

if (evalExpr) {
  // awaitPromise：允许 --eval 里直接跑 async IIFE（多步操作 + 等待界面更新）
  const r = await call("Runtime.evaluate", {
    expression: evalExpr,
    returnByValue: true,
    awaitPromise: true,
  });
  console.log(JSON.stringify(r.result.value ?? r.result.description, null, 2));
  ws.close();
  process.exit(0);
}

mkdirSync(outDir, { recursive: true });
for (const theme of themes) {
// --ctx "选择器"：右键（弹出上下文菜单，用于截图）
const ctxSel = opts.get("ctx");
if (ctxSel) {
  const probe = await call("Runtime.evaluate", {
    expression: `(() => { const el = document.querySelector(${JSON.stringify(
      ctxSel,
    )}); if (!el) return "NO_MATCH"; const r = el.getBoundingClientRect(); el.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, view: window, clientX: Math.round(r.left + 40), clientY: Math.round(r.top + 12) })); return "MENU"; })()`,
    returnByValue: true,
  });
  console.log(`CTX ${ctxSel} → ${probe.result.value}`);
  if (probe.result.value !== "MENU") process.exit(1);
  await new Promise((r) => setTimeout(r, 350));
}

  await call("Emulation.setEmulatedMedia", {
    features: [{ name: "prefers-color-scheme", value: theme }],
  });
  await new Promise((r) => setTimeout(r, 900)); // 等一帧渲染 + 数据回来
  const file = join(outDir, `${prefix}${name ? `${name}-` : ""}${theme}.png`);
  const shot = await call("Page.captureScreenshot", { format: "png" });
  writeFileSync(file, Buffer.from(shot.data, "base64"));
  console.log(`SAVED ${file}`);
}
ws.close();
