/**
 * 两个窗口之间的「数据变了」通知。
 *
 * 主窗口和浮窗是**同一个 origin** 的两个文档，`localStorage` 共用，而 `storage` 事件
 * 只在**别的**文档里触发 —— 正好就是我们要的广播语义（自己改的自己知道，不用通知自己）。
 *
 * 为什么不用 Tauri 的 `emit`：那要 Rust 侧或 `@tauri-apps/api/event` 的窗口定向广播，
 * 而这里只是「另一个人动了库，重新拉一遍」这种弱一致性需求，用现成的事件够用，
 * 也不需要在后端加中转。
 */
const TASKS_KEY = "ringtone.tasksPing";
const PHOTO_KEY = "ringtone.photoPing";
const FOCUS_KEY = "ringtone.focusComposerPing";
const SETTINGS_KEY = "ringtone.openSettingsPing";
const LOCAL_PHOTO_EVENT = "ringtone:photo-changed";

/** 往 localStorage 写一个时间戳（写入失败不报错：隐私模式、配额满都可能失败） */
function ping(key: string): void {
  try {
    window.localStorage.setItem(key, String(Date.now()));
  } catch {
    // 存不进去也不影响当前窗口，另一个窗口最多晚 30 秒（它自己有轮询）
  }
}

/** 订阅某个键的变化；返回取消订阅的函数 */
function watch(key: string, cb: () => void): () => void {
  function onStorage(e: StorageEvent) {
    if (e.key === key) cb();
  }
  window.addEventListener("storage", onStorage);
  return () => window.removeEventListener("storage", onStorage);
}

/** 告诉另一个窗口「任务数据变了，去重新读一遍」 */
export function pingTasks(): void {
  ping(TASKS_KEY);
}

/** 另一个窗口动了任务时回调；返回取消订阅的函数 */
export function watchTasks(cb: () => void): () => void {
  return watch(TASKS_KEY, cb);
}

/** 告诉另一个窗口「背景图变了」（图片本体在后端，它自己去拉） */
export function pingPhoto(): void {
  ping(PHOTO_KEY);
  // `storage` 只在**别的**文档里触发，可「改浓度」是本窗口自己改的：
  // 不发这一下，主窗口的面板拖完滑块，主窗口自己的背景要等重启才变（实测踩过）。
  window.dispatchEvent(new Event(LOCAL_PHOTO_EVENT));
}

/** 另一个窗口换了背景图时回调（本窗口自己换的也会回调）；返回取消订阅的函数 */
export function watchPhoto(cb: () => void): () => void {
  const off = watch(PHOTO_KEY, cb);
  window.addEventListener(LOCAL_PHOTO_EVENT, cb);
  return () => {
    off();
    window.removeEventListener(LOCAL_PHOTO_EVENT, cb);
  };
}

/** 让主窗口把输入框打开并聚焦（浮窗空白处右键 → 添加任务） */
export function pingFocusComposer(): void {
  ping(FOCUS_KEY);
}

/** 另一个窗口要求打开输入框时回调；返回取消订阅的函数 */
export function watchFocusComposer(cb: () => void): () => void {
  return watch(FOCUS_KEY, cb);
}

/** 让主窗口把设置面板打开（浮窗里点「设置」） */
export function pingOpenSettings(): void {
  ping(SETTINGS_KEY);
}

/** 另一个窗口要求打开设置时回调；返回取消订阅的函数 */
export function watchOpenSettings(cb: () => void): () => void {
  return watch(SETTINGS_KEY, cb);
}
