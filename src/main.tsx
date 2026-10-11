import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import FloatWindow, { isFloatWindow } from "./Float";
import { applyTheme, loadTheme, watchTheme } from "./theme";
import { applyAppearance, loadAppearance, watchAppearance } from "./appearance";
import "./styles.css";

// 同一个 index.html 两个实例：主窗口和待办浮窗。
// 浮窗那个窗口是透明无边框的，body 也得真透明，否则圆角外面会露一块深色方块。
const float = isFloatWindow();
// 主题和外观都要在渲染前先挂上：晚一步会先闪一下默认样式
applyTheme(loadTheme());
// 透明度分两条（主窗口 / 浮窗），所以这里要告诉 applyAppearance 自己是哪个窗口
applyAppearance(loadAppearance(), float ? "float" : "main");
// 另一个窗口改了外观就跟着变（`storage` 只在别的窗口触发，不需要后端参与）
watchAppearance((a) => applyAppearance(a, float ? "float" : "main"));
// 主题也一样：主窗口点了主题按钮，浮窗要跟着换（不然它会停在加载时那个主题）
watchTheme((t) => applyTheme(t));
if (float) document.body.classList.add("float-mode");

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>{float ? <FloatWindow /> : <App />}</React.StrictMode>,
);
