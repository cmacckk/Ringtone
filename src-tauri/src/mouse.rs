//! 全局「鼠标侧键」热键：Windows 低级鼠标钩子（`WH_MOUSE_LL`）。
//!
//! 为什么必须自己写：`tauri-plugin-global-shortcut` 底层是 `global-hotkey`，
//! 它只认键盘（Windows 后端的实现里根本没有鼠标按钮这条路径），系统也没有
//! 「把鼠标键注册成全局热键」的 API。要做到「切到别的程序里按侧键也能叫出浮窗」，
//! 只有低级钩子这一条路。
//!
//! **取舍**：被选中的侧键会被**吞掉**（回调返回非 0，不再往下传），
//! 因为「热键」的语义就是按了就归我——代价是那个键从此不再做系统的「后退 / 前进」。
//! 所以默认是 `off`，要用户自己在设置里开；钩子线程也只在真的选了侧键时才起，
//! 没开的时候进程里连钩子都不存在（少一点杀毒软件误报的理由）。
//!
//! 线程模型：低级钩子要求「装钩子的线程自己有消息泵」，所以这里开一个专用线程
//! 跑 `GetMessageW` 循环，回调里只做一件事：把浮窗叫出来。
//!
//! 为什么不引 `windows-sys`：本项目只需要 user32 里这 6 个函数、2 个结构体、
//! 3 个常量，手写十行声明就够了。**而且实测把 `windows-sys 0.61` 加进来之后，
//! 测试可执行文件在加载时报 `STATUS_ENTRYPOINT_NOT_FOUND`（0xc0000139）**——
//! 它用 `raw-dylib` 链接，和本机的 GNU 工具链合不来。手写 `#[link(name = "user32")]`
//! 走的是传统导入库，链接干净、依赖也不增加。

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Mutex;
use tauri::AppHandle;

/// 设置键：`float.mouse`，取值 `off` / `x1` / `x2`
pub const KEY: &str = "float.mouse";
/// 关闭（默认）
pub const OFF: &str = "off";

/// 要监听的侧键：`0` = 不监听，`1` = XBUTTON1（常见的「后退」），`2` = XBUTTON2（「前进」）
static WANTED: AtomicU8 = AtomicU8::new(0);
/// 钩子线程是否已经起过（只起一次，之后靠 `WANTED` 开关）
static STARTED: AtomicBool = AtomicBool::new(false);
/// 钩子回调里要用 AppHandle 去叫窗口，存在这里给别的线程用。
static APP: Mutex<Option<AppHandle>> = Mutex::new(None);

/// 记下 AppHandle（`setup` 里调一次）。只记，不装钩子——装不装看设置。
pub fn init(app: &AppHandle) {
    if let Ok(mut slot) = APP.lock() {
        *slot = Some(app.clone());
    }
}

/// 把设置值规整成侧键号（纯逻辑，不碰系统）。
///
/// `0` = 关闭；`1` = XBUTTON1（常见的「后退」），`2` = XBUTTON2（「前进」）。
///
/// 认三种正规写法：`off` / `x1` / `x2`，另外宽容地接受 `1`、`2`、`mouse4`、`mouse5`、
/// `back`、`forward`——用户在设置里只会看到 `off` / `x1` / `x2`，宽容只是为了
/// 别人手改数据库时不至于让程序报错。
pub fn normalize(value: &str) -> Result<u8, String> {
    let v = value.trim().to_ascii_lowercase();
    match v.as_str() {
        "" | "off" | "0" | "none" => Ok(0),
        "x1" | "1" | "mouse4" | "button4" | "back" => Ok(1),
        "x2" | "2" | "mouse5" | "button5" | "forward" => Ok(2),
        other => Err(format!(
            "不认识的侧键「{other}」：只能是 off / x1 / x2（x1 = 常见的「后退」键，x2 = 「前进」键）"
        )),
    }
}

/// 把设置值应用到钩子上。返回规整后的值（用来写回设置）。
pub fn apply(value: &str) -> Result<String, String> {
    let button = normalize(value)?;
    WANTED.store(button, Ordering::Relaxed);
    if button != 0 {
        ensure_started()?;
    }
    Ok(if button == 0 {
        OFF.to_string()
    } else {
        format!("x{button}")
    })
}

/// 保证钩子线程已经起来。第一次调用才真的装钩子；
/// 装失败时把 `STARTED` 退回 false，下次还能再试（不能因为装不上钩子就让程序起不来）。
///
/// **测试构建里这是个空实现**，故意的：真的那条路径会（经 `fire` → `show_float_now`）
/// 把 tauri 的窗口代码拉进测试可执行文件，链接出来的 `ringtone_lib-*.exe` 会带着
/// 整包 GUI 导入表（comctl32 / dwmapi / ole32 …），在**没有清单（manifest）**的
/// 测试宿主里加载时直接 `STATUS_ENTRYPOINT_NOT_FOUND`（0xc0000139），一个测试都跑不了。
#[cfg(not(test))]
fn ensure_started() -> Result<(), String> {
    if STARTED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let spawned = std::thread::Builder::new()
        .name("rt-mouse-hook".to_string())
        .spawn(move || hook_loop(tx));
    match spawned {
        Ok(_) => match rx.recv() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                STARTED.store(false, Ordering::SeqCst);
                Err(e)
            }
            Err(_) => Err("鼠标钩子线程没回话（可能已经退出了）".to_string()),
        },
        Err(e) => {
            STARTED.store(false, Ordering::SeqCst);
            Err(format!("起不了鼠标钩子线程：{e}"))
        }
    }
}

/// 测试构建里的空实现（理由见上面 `ensure_started` 的注释）。
#[cfg(test)]
fn ensure_started() -> Result<(), String> {
    Ok(())
}

/// 叫出浮窗。钩子回调在系统线程上跑，窗口操作挪回主线程做。
#[cfg(all(windows, not(test)))]
fn fire() {
    let app = APP.lock().ok().and_then(|g| g.clone());
    let Some(app) = app else {
        return;
    };
    let for_main = app.clone();
    if let Err(e) = app.run_on_main_thread(move || {
        if let Err(e) = crate::show_float_now(&for_main) {
            eprintln!("[ringtone] 鼠标侧键叫浮窗失败：{e}");
        }
    }) {
        eprintln!("[ringtone] 鼠标侧键叫浮窗失败（派发不到主线程）：{e}");
    }
}

#[cfg(all(windows, not(test)))]
mod hook {
    //! 手写的 user32 声明（只声明用得到的这几个）。
    use core::ffi::c_void;

    /// 低级鼠标钩子
    pub const WH_MOUSE_LL: i32 = 14;
    /// 钩子回调里 `code` 的正常取值
    pub const HC_ACTION: u32 = 0;
    /// 侧键按下（`wparam`）
    pub const WM_XBUTTONDOWN: u32 = 0x020B;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct Point {
        pub x: i32,
        pub y: i32,
    }

    /// `MSG`：`GetMessageW` 的取出对象。字段顺序和 C 定义一致。
    #[repr(C)]
    pub struct Msg {
        pub hwnd: *mut c_void,
        pub message: u32,
        pub wparam: usize,
        pub lparam: isize,
        pub time: u32,
        pub pt: Point,
    }

    impl Msg {
        pub fn zeroed() -> Self {
            Msg {
                hwnd: std::ptr::null_mut(),
                message: 0,
                wparam: 0,
                lparam: 0,
                time: 0,
                pt: Point::default(),
            }
        }
    }

    /// `MSLLHOOKSTRUCT`：低级鼠标钩子拿到的结构。**`mouseData` 的高 16 位是哪个侧键**。
    #[repr(C)]
    pub struct MsllHookStruct {
        pub pt: Point,
        pub mouse_data: u32,
        pub flags: u32,
        pub time: u32,
        pub extra_info: usize,
    }

    pub type HookProc = Option<unsafe extern "system" fn(i32, usize, isize) -> isize>;

    // 函数指针类型（运行时从 user32.dll 取）
    type SetHookFn = unsafe extern "system" fn(i32, HookProc, *mut c_void, u32) -> *mut c_void;
    type UnhookFn = unsafe extern "system" fn(*mut c_void) -> i32;
    type CallNextFn = unsafe extern "system" fn(*mut c_void, i32, usize, isize) -> isize;
    type GetMsgFn = unsafe extern "system" fn(*mut Msg, *mut c_void, u32, u32) -> i32;
    type TransMsgFn = unsafe extern "system" fn(*const Msg) -> i32;
    type DispMsgFn = unsafe extern "system" fn(*const Msg) -> isize;

    /// 用得到的几个 user32 函数（运行时解析出来的）
    pub struct User32 {
        pub set_hook: SetHookFn,
        pub unhook: UnhookFn,
        pub call_next: CallNextFn,
        pub get_msg: GetMsgFn,
        pub trans_msg: TransMsgFn,
        pub disp_msg: DispMsgFn,
    }

    // SAFETY: 全是函数指针 + 一个模块句柄，跨线程只读使用。
    unsafe impl Send for User32 {}
    unsafe impl Sync for User32 {}

    // 故意**不写** `#[link(name = "kernel32")]`：msvc 目标默认就会链 kernel32.lib，
    // 而手动加一条 `#[link]` 会在导入表里多插一条**同名不同大小写的描述符**
    // （工具链写 `KERNEL32.dll`、我们写 `kernel32.dll`），实测那种镜像加载时会直接
    // 报 `STATUS_ENTRYPOINT_NOT_FOUND`。不写 `#[link]` 时符号会从默认导入库里解析，正好避免重复。
    extern "system" {
        fn LoadLibraryW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    }

    /// 运行时把 user32 的入口取出来。
    ///
    /// **为什么不在编译期 `#[link(name = "user32")]`**：实测那样链出来的测试可执行文件
    /// 加载时报 `STATUS_ENTRYPOINT_NOT_FOUND`（0xc0000139）—— 导入表里会出现**同一个
    /// 用户态 DLL 的两条描述符**（工具链自己一条 + 我们一条），Windows 加载器不吃这套。
    /// 改成 `LoadLibraryW` + `GetProcAddress` 后，导入表只多一个 `kernel32`（本来就链了），
    /// 而且 user32 缺失时程序照常起（只是侧键用不了）。
    pub fn load() -> Result<User32, String> {
        let wide: Vec<u16> = "user32.dll\0".encode_utf16().collect();
        // SAFETY: 传的是以 0 结尾的宽字符串字面量。
        let module = unsafe { LoadLibraryW(wide.as_ptr()) };
        if module.is_null() {
            return Err("装不上鼠标钩子：LoadLibraryW(user32.dll) 返回空".to_string());
        }
        // 取一个入口地址。单独写成安全函数 + 内部 unsafe 块，免得“unsafe fn 里没 unsafe”的告警。
        fn sym(module: *mut c_void, name: &str) -> Result<*const c_void, String> {
            let mut bytes = name.as_bytes().to_vec();
            bytes.push(0);
            // SAFETY: `module` 是刚从 LoadLibraryW 拿到的合法模块句柄，`bytes` 是 0 结尾的 ASCII。
            let p = unsafe { GetProcAddress(module, bytes.as_ptr()) };
            if p.is_null() {
                Err(format!("user32.dll 里找不到 {name}"))
            } else {
                Ok(p as *const c_void)
            }
        }
        // SAFETY: 取到的确实是 user32 里那几个函数的地址，函数类型按 MSDN 声明。
        unsafe {
            let set_hook =
                std::mem::transmute::<*const c_void, SetHookFn>(sym(module, "SetWindowsHookExW")?);
            let unhook =
                std::mem::transmute::<*const c_void, UnhookFn>(sym(module, "UnhookWindowsHookEx")?);
            let call_next =
                std::mem::transmute::<*const c_void, CallNextFn>(sym(module, "CallNextHookEx")?);
            let get_msg =
                std::mem::transmute::<*const c_void, GetMsgFn>(sym(module, "GetMessageW")?);
            let trans_msg =
                std::mem::transmute::<*const c_void, TransMsgFn>(sym(module, "TranslateMessage")?);
            let disp_msg =
                std::mem::transmute::<*const c_void, DispMsgFn>(sym(module, "DispatchMessageW")?);
            Ok(User32 {
                set_hook,
                unhook,
                call_next,
                get_msg,
                trans_msg,
                disp_msg,
            })
        }
    }
}

#[cfg(all(windows, not(test)))]
static USER32: std::sync::OnceLock<hook::User32> = std::sync::OnceLock::new();

#[cfg(all(windows, not(test)))]
fn hook_loop(report: std::sync::mpsc::Sender<Result<(), String>>) {
    use hook::*;

    let loaded = match hook::load() {
        Ok(api) => api,
        Err(e) => {
            let _ = report.send(Err(e));
            return;
        }
    };
    // 第一次进来就存下；存不进去说明已经有人存过（`ensure_started` 保证不会有），那就用现成的
    let _ = USER32.set(loaded);
    let Some(api) = USER32.get() else {
        let _ = report.send(Err("user32 入口表丢了".to_string()));
        return;
    };

    // SAFETY: 装的是本进程内的函数指针；`hmod` 传空、线程 id 传 0，
    // 这是 `WH_MOUSE_LL` 的标准用法（低级钩子不需要 DLL，也不绑线程）。
    let install = unsafe { (api.set_hook)(WH_MOUSE_LL, Some(hook_proc), std::ptr::null_mut(), 0) };
    if install.is_null() {
        let _ = report.send(Err(
            "SetWindowsHookExW 失败：装不上全局鼠标钩子（可能被安全软件拦了）".to_string(),
        ));
        return;
    }
    let _ = report.send(Ok(()));
    println!("[ringtone] 鼠标侧键监听已就绪");

    // 低级钩子要求装钩子的线程自己跑消息泵，否则系统不会把事件送过来。
    let mut msg = Msg::zeroed();
    // SAFETY: `msg` 是本函数的局部变量，指针只在调用期间使用；
    // 循环退出（进程结束时不会再返回）后卸钩子。
    unsafe {
        while (api.get_msg)(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            (api.trans_msg)(&msg);
            (api.disp_msg)(&msg);
        }
        (api.unhook)(install);
    }
}

#[cfg(all(not(windows), not(test)))]
fn hook_loop(report: std::sync::mpsc::Sender<Result<(), String>>) {
    let _ = report.send(Err("这个平台没有鼠标钩子（只有 Windows 有）".to_string()));
}

#[cfg(all(windows, not(test)))]
unsafe extern "system" fn hook_proc(code: i32, wparam: usize, lparam: isize) -> isize {
    use hook::*;

    // `code < 0` 时必须直接往下传，别的钩子靠这个值判断要不要跳过（MSDN 的硬要求）。
    // `HC_ACTION` 定义成 `u32`，所以这里做一次转换。
    if code as u32 == HC_ACTION && wparam as u32 == WM_XBUTTONDOWN {
        let want = WANTED.load(Ordering::Relaxed);
        if want != 0 {
            // SAFETY: 对 `WM_XBUTTONDOWN` 来说 `lparam` 一定是 `MSLLHOOKSTRUCT`（MSDN 规定）。
            let info = unsafe { &*(lparam as *const MsllHookStruct) };
            // 哪个侧键在 `mouse_data` 的高 16 位：1 = XBUTTON1，2 = XBUTTON2
            let button = ((info.mouse_data >> 16) & 0xffff) as u8;
            if button == want {
                fire();
                // 吞掉这个按键：不再往下传，也不调 `CallNextHookEx`。
                // 不吞的话按一下侧键会「既弹出浮窗、又让浏览器后退」。
                return 1;
            }
        }
    }
    // SAFETY: 纯转发，参数原样交给下一个钩子。
    match USER32.get() {
        Some(api) => unsafe { (api.call_next)(std::ptr::null_mut(), code, wparam, lparam) },
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_accepts_and_rejects() {
        assert_eq!(normalize("off").unwrap(), 0, "off = 关闭");
        assert_eq!(normalize("").unwrap(), 0, "空串当关闭");
        assert_eq!(normalize("X1").unwrap(), 1, "大写也要认");
        assert_eq!(normalize(" x2 ").unwrap(), 2, "两边空格要去掉");
        assert_eq!(normalize("mouse4").unwrap(), 1, "mouse4 = x1，宽容写法");
        assert_eq!(normalize("forward").unwrap(), 2, "forward = x2，宽容写法");
        let err = normalize("x9").unwrap_err();
        assert!(err.contains("x9"), "错误信息里要带上用户写的值：{err}");
        assert!(
            normalize("左键").is_err(),
            "不认识的写法必须报错，不能静默关掉"
        );
    }

    #[test]
    fn apply_normalizes_and_rejects() {
        assert_eq!(apply("off").unwrap(), "off", "off 应原样返回");
        assert_eq!(apply("").unwrap(), "off", "空串当关闭");
        assert_eq!(apply("X1").unwrap(), "x1", "大写也要认");
        assert_eq!(apply(" x2 ").unwrap(), "x2", "两边空格要去掉");
        assert_eq!(apply("mouse4").unwrap(), "x1", "mouse4 = x1，宽容写法");
        assert_eq!(apply("forward").unwrap(), "x2", "forward = x2，宽容写法");
        let err = apply("x9").unwrap_err();
        assert!(err.contains("x9"), "错误信息里要带上用户写的值：{err}");
        assert!(apply("左键").is_err(), "不认识的写法必须报错，不能静默关掉");
    }

    #[test]
    fn apply_off_does_not_start_hook_thread() {
        assert_eq!(apply("off").unwrap(), "off", "off 应成功");
        assert!(
            !STARTED.load(Ordering::SeqCst),
            "只是关掉侧键，不该启动钩子线程（没开的时候进程里不该有钩子）"
        );
        assert_eq!(WANTED.load(Ordering::Relaxed), 0, "关闭后 WANTED 必须是 0");
    }
}
