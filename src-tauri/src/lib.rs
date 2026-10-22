//! Ringtone：本地待办 + 到点推送。
//!
//! - M1：SQLite 落库 + 任务 CRUD + 按天视图 + 托盘常驻
//! - M2：到点调度（tick + 幂等认领 + 错过语义）+ 本地系统通知
//! - M3：真实推送渠道（飞书）——配了渠道走渠道，发失败本地通知兜底并记日志
//! - M4：投递队列 + 降级链（钉钉 / 飞书）+ 限流闸门 + 托盘红点

mod background;
mod channel;
mod commands;
mod db;
mod deliver;
mod fonts;
mod holiday;
mod models;
mod mouse;
mod port;
mod scheduler;
mod time;
mod work;

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use commands::AppState;
use scheduler::{TickContext, LATE_WINDOW_MS, TICK_MS};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{
    AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
    WindowEvent,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            // 「一键显示浮窗」：全局快捷键，不聚焦也能叫出来。
            // 回调里只管把浮窗摆出来；具体加速键是什么由设置决定（`apply_hotkey`）。
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        if let Err(e) = show_float_now(app) {
                            eprintln!("[ringtone] 快捷键叫浮窗失败：{e}");
                        }
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            // 开机自启时带上这个参数：登录后起来就缩进托盘，不挡在桌面中间。
            // 启动参数是写在注册表那一项里的，所以改了这里要重新开关一次自启才生效。
            Some(vec![MINIMIZED_ARG]),
        ))
        .setup(|app| {
            let dir = data_dir(app.handle()).map_err(std::io::Error::other)?;
            let conn = db::open(&dir.join("ringtone.db"))?;
            db::migrate(&conn)?;

            // 顺序不能换：**先生成，再扫掉过期的**。
            // 反过来的话，过期实例会在扫描之后才被生成出来，
            // 然后被当成「正常到点」立刻弹出来，等于违背「冷启动不补发」。
            let now = time::now_ms();
            // 上下班提醒是隐藏任务：每次启动按设置重建一遍（时间改了、开关关了都靠这一步生效）。
            // 必须排在 `ensure_occurrences` 前面，否则新建的隐藏任务今天这一轮没有实例。
            let schedule = work::load(&conn);
            let hidden = work::materialize(&conn, &schedule).unwrap_or_else(|e| {
                eprintln!("[ringtone] 上下班提醒重建失败：{e}");
                0
            });
            // 快捷键也在这里读：这个时刻 `conn` 还是本地变量，不用去锁全局状态
            let saved_hotkey = db::setting_get(&conn, HOTKEY_KEY)
                .ok()
                .flatten()
                .unwrap_or_default();
            // 鼠标侧键：和键盘快捷键是两套机制（侧键只能靠低级钩子），所以分开存、分开读
            let saved_mouse = db::setting_get(&conn, mouse::KEY)
                .ok()
                .flatten()
                .unwrap_or_else(|| mouse::OFF.to_string());
            let created = db::ensure_occurrences(&conn, now)?;
            let missed = db::miss_overdue_on_start(&conn, now)?;
            // 收拾上一次运行时被强制退出/断电打断的现场：
            // `sending` 悬挂没人看得到，而用户明确要求「关机后不补发」，所以直接作废。
            let (stuck_jobs, stuck_occs) = db::recover_stuck(&conn, now)?;
            println!(
                "[ringtone] 启动完成：上下班提醒隐藏任务 {hidden} 条；新增 {created} 条提醒实例；\
                 启动前已过期 {missed} 条 → missed（不补发）；\
                 清理上次中断 {stuck_occs} 条实例 / {stuck_jobs} 条作业（不补发）"
            );

            app.manage(AppState {
                conn: Mutex::new(conn),
            });
            build_tray(app)?;
            build_float(app.handle())?;
            // 先把浮窗落点算一次存下来（这时主窗口还没被最小化，显示器信息是准的）
            if let Some(main) = app.get_webview_window("main") {
                let _ = compute_dock(app.handle(), &main);
                // 开机自启带过来的 `--minimized`：直接收进托盘（托盘、调度、推送到点照样跑）。
                // 用 hide 而不是 minimize：最小化在任务栏还留个条，hide 才是真「不出现」。
                if started_minimized() {
                    if let Err(e) = main.hide() {
                        eprintln!("[ringtone] 开机自启没能收进托盘：{e}");
                    } else {
                        println!(
                            "[ringtone] 开机自启：已收进托盘（点托盘图标或快捷键可以随时叫出来）"
                        );
                    }
                }
            }
            // 快捷键：从设置里读，注册失败只警告（不能为了一个快捷键让程序起不来）
            if !saved_hotkey.trim().is_empty() {
                match apply_hotkey(app.handle(), saved_hotkey.trim()) {
                    Ok(_) => println!("[ringtone] 快捷键已注册：{saved_hotkey}"),
                    Err(e) => eprintln!("[ringtone] 快捷键「{saved_hotkey}」没用上：{e}"),
                }
            }
            // 鼠标侧键：同样只是警告。默认 `off`，那时连钩子线程都不会起
            mouse::init(app.handle());
            match mouse::apply(&saved_mouse) {
                Ok(v) if v != mouse::OFF => println!("[ringtone] 鼠标侧键已接管：{v}"),
                Ok(_) => {}
                Err(e) => eprintln!("[ringtone] 鼠标侧键没接管：{e}"),
            }
            spawn_scheduler(app.handle().clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            // 关窗不退出：驻留托盘，调度线程继续跑。
            // **只对主窗口生效**：浮窗没有自己的关闭按钮，但用户按 Alt+F4 也能关它，
            // 一旦关掉就再也叫不出来（只能重启程序），所以这里直接不认。
            if window.label() != "main" {
                return;
            }
            match event {
                WindowEvent::CloseRequested { api, .. } => {
                    // 收进托盘 ⇒ 让浮窗顶上，用户还能看到待办
                    let _ = window.hide();
                    api.prevent_close();
                    sync_float(window.app_handle());
                }
                // 最小化/还原/最大化都会走 Resized，点回主窗口会走 Focused(true)：
                // 两种情况都要重算「浮窗该不该出现」。
                // `Moved` 是「用户把主窗口拖到别的屏去了」：这时主窗口还看得见，落点能算准；
                // 等它最小化再问就晚了——系统已经把窗口挪到屏幕外（-32000），问到的
                // 是一块 size=0 的假显示器。少这一条就会出现「主窗口换了屏、浮窗还在老屏上」。
                WindowEvent::Moved(_) | WindowEvent::Resized(_) | WindowEvent::Focused(true) => {
                    sync_float(window.app_handle());
                }
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::today_local,
            commands::list_tasks,
            commands::count_tasks_by_day,
            commands::next_reminder,
            commands::task_stats,
            commands::holiday_info,
            commands::create_task,
            commands::update_task,
            commands::set_task_done,
            commands::set_task_star,
            commands::set_occurrence_done,
            commands::stop_repeat,
            commands::delete_tasks,
            commands::list_channels,
            commands::save_feishu_channel,
            commands::save_dingtalk_channel,
            commands::save_wxpusher_channel,
            commands::list_fonts,
            commands::set_channel_enabled,
            commands::set_channel_primary,
            commands::delete_channel,
            commands::test_channel,
            commands::verify_channel,
            commands::recent_logs,
            commands::failure_count,
            commands::acknowledge_failures,
            show_main,
            hide_main,
            float_dock,
            float_set_side,
            float_show,
            float_visible,
            toggle_float,
            get_float_hotkey,
            set_float_hotkey,
            get_mouse_button,
            set_mouse_button,
            commands::work_get,
            commands::work_save,
            commands::work_status,
            commands::background_pick,
            commands::background_get,
            commands::background_clear,
            commands::export_tasks,
            commands::import_tasks,
        ])
        .run(tauri::generate_context!())
        .expect("启动 Tauri 应用失败");
}

/// 后台调度线程。
///
/// 用独立 OS 线程而不是 tokio task：数据库是 `Mutex<Connection>`，
/// 同步代码更直白，也不会因为某次发送慢而把 UI 的锁拖住。
fn spawn_scheduler(app: AppHandle) {
    std::thread::spawn(move || {
        let mut last_tick = time::now_ms();
        // 限流窗口只活在这个线程里（单线程访问，不需要锁）
        let mut limiter = deliver::RateLimiter::default();
        loop {
            // 睡到「下一个 TICK_MS 边界」而不是无脑 `sleep(TICK_MS)`。
            // 无脑睡的话，每一轮的执行耗时都会累加进周期，只要某一轮被系统拖慢
            // （实测：首次建托盘、系统调度抖动），相位就**永久**偏移那么多，
            // 到点延迟从此多出那一截（真实观测到 1.5s 的永久漂移）。
            // 对齐到固定网格就不会积累漂移——这就是「到点 ±5 秒」能守住的依据。
            let before = time::now_ms();
            let next = (before / TICK_MS + 1) * TICK_MS;
            std::thread::sleep(Duration::from_millis((next - before) as u64));

            let now = time::now_ms();
            let ctx = scheduler::ctx_for_tick(last_tick, now);
            if ctx == TickContext::Wakeup {
                println!(
                    "[ringtone] 检测到休眠唤醒（上一 tick 距今 {}ms），按唤醒语义处理",
                    now - last_tick
                );
            }
            let gap = now - last_tick;
            last_tick = now;

            let started = time::now_ms();
            if let Err(e) = tick_once(&app, now, ctx, &mut limiter) {
                eprintln!("[ringtone] tick 失败：{e}");
            }
            // 自诊断：提醒时机的上限就是 tick 间隔，所以「单轮耗时」不是小事。
            // 真实事故：每轮 tick 都无条件调托盘 `set_tooltip`/`set_icon`，
            // 在后台线程上会等主线程，单轮耗时从几毫秒涨到 5 秒级，
            // 周期变成两倍，到点延迟从 ~1s 恶化到 ~10s。所以超阈值就要吵。
            let cost = time::now_ms() - started;
            if cost > TICK_MS {
                eprintln!(
                    "[ringtone] 本轮 tick 耗时 {cost}ms，超过 {TICK_MS}ms 的 tick 间隔\
                     （上一轮间隔 {gap}ms），提醒时机会整体后移"
                );
            }
        }
    });
}

/// 一个 tick：生成 → 认领 → 发送 → 收尾。
/// 一个 tick：生成 → 认领 → 入队 → 投递队列 → 刷托盘状态。
///
/// M4 起「认领」不再等于「发送」：认领出来的实例先入队，
/// 真正发出去（含退避重试、降级）交给 `deliver::pump`，
/// 因为重试要等 5s/30s/2min，不能在调度线程里 `sleep`。
fn tick_once(
    app: &AppHandle,
    now: i64,
    ctx: TickContext,
    limiter: &mut deliver::RateLimiter,
) -> Result<(), String> {
    let claimed = {
        let state = app.state::<AppState>();
        let conn = state.conn.lock().map_err(|e| e.to_string())?;
        // 顺序不能换：先生成、再认领。
        db::ensure_occurrences(&conn, now).map_err(|e| e.to_string())?;
        db::claim_due(&conn, now, ctx, LATE_WINDOW_MS).map_err(|e| e.to_string())?
        // 锁在这里释放：发消息可能慢，不能占着锁不放
    };

    for item in &claimed.due {
        match deliver::enqueue(app, item) {
            Ok(deliver::Enqueue::Queued { channel }) => println!(
                "[ringtone] 已入队：{}（occ={} late={}）→ 「{channel}」",
                item.title, item.occurrence_id, item.late
            ),
            // 没有任何可用渠道：当场走本地通知（算送达，不是失败）
            Ok(deliver::Enqueue::Local(delivery)) => {
                settle(app, item.occurrence_id, item.late, &delivery)?;
                println!(
                    "[ringtone] 已通知（本地）：{}（occ={}）→ {}",
                    item.title, item.occurrence_id, delivery.detail
                );
            }
            // 入队都失败（数据库出问题）——不能静默，收尾并报出来
            Err(e) => {
                eprintln!(
                    "[ringtone] 入队失败：{}（occ={}）：{e}",
                    item.title, item.occurrence_id
                );
                let state = app.state::<AppState>();
                let conn = state.conn.lock().map_err(|e| e.to_string())?;
                db::mark_skipped(&conn, item.occurrence_id).map_err(|e| e.to_string())?;
                db::log_event(
                    &conn,
                    time::now_ms(),
                    None,
                    Some(item.occurrence_id),
                    "failed",
                    None,
                    Some(&format!("入队失败：{e}")),
                )
                .map_err(|e| e.to_string())?;
            }
        }
    }

    // 队列里到点的作业：真发（含退避重试 / 降级 / 全失败兜底）
    let stats = deliver::pump(app, now, limiter)?;
    if !stats.is_idle() {
        println!(
            "[ringtone] 投递本轮：成功 {} / 退避 {} / 降级 {} / 限流推迟 {} / 全渠道失败 {}",
            stats.sent, stats.retried, stats.degraded, stats.delayed, stats.exhausted
        );
    }
    refresh_tray_alert(app);

    if claimed.missed > 0 || claimed.skipped > 0 {
        println!(
            "[ringtone] 本轮错过 {} 条、跳过 {} 条（已写 send_log）",
            claimed.missed, claimed.skipped
        );
    }
    Ok(())
}

/// 把一笔不在队列里的投递结果落库（目前只有「本地通知」这一种）
fn settle(
    app: &AppHandle,
    occurrence_id: i64,
    late: bool,
    delivery: &deliver::Delivery,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    let ts = time::now_ms();
    if delivery.ok {
        db::mark_sent(&conn, occurrence_id, ts, late).map_err(|e| e.to_string())?;
    } else {
        db::mark_skipped(&conn, occurrence_id).map_err(|e| e.to_string())?;
    }
    db::log_event(
        &conn,
        ts,
        delivery.channel_id,
        Some(occurrence_id),
        delivery.result,
        delivery.http_code,
        Some(&delivery.detail),
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// 上一次真正刷到托盘上的失败条数。
///
/// 为什么要缓存：`set_tooltip` / `set_icon` 在后台线程上会等主线程处理，
/// 每轮 tick 无条件调一次的代价是**单轮耗时从几毫秒涨到 5 秒级**，
/// 而 tick 节奏 = sleep(5s) + 本轮耗时，于是到点延迟被拖成 ~10 秒。
/// 所以只在「失败条数真的变了」的时候才碰托盘。
/// 初值 0 与 `build_tray` 建托盘时的状态一致（tooltip 是 "Ringtone"、图标是应用图标）。
static APPLIED_FAILURES: AtomicI64 = AtomicI64::new(0);

/// 托盘状态：有「全渠道失败」时换红点图标 + 改 tooltip，清零后恢复。
fn refresh_tray_alert(app: &AppHandle) {
    let n = deliver::failure_count() as i64;
    if APPLIED_FAILURES.load(Ordering::Relaxed) == n {
        return;
    }
    let Some(tray) = app.tray_by_id("main") else {
        return;
    };
    if n == 0 {
        let _ = tray.set_tooltip(Some("Ringtone"));
        if let Some(icon) = app.default_window_icon() {
            let _ = tray.set_icon(Some(icon.clone()));
        }
    } else {
        let _ = tray.set_tooltip(Some(&format!("Ringtone · {n} 条提醒发送失败")));
        match tauri::image::Image::from_bytes(include_bytes!("../icons/tray-alert.png")) {
            Ok(img) => {
                let _ = tray.set_icon(Some(img));
            }
            Err(e) => eprintln!("[ringtone] 加载红点图标失败：{e}"),
        }
    }
    APPLIED_FAILURES.store(n, Ordering::Relaxed);
    // 只在真的变化时打一行，方便在日志里确认「红点该亮的时候亮了」
    if n == 0 {
        println!("[ringtone] 托盘状态：无发送失败，红点已清除");
    } else {
        println!("[ringtone] 托盘状态：{n} 条提醒发送失败（红点已显示）");
    }
}

fn build_tray(app: &mut tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let show_float_item = MenuItem::with_id(app, "float", "显示浮窗", true, None::<&str>)?;
    let prefs = MenuItem::with_id(app, "prefs", "打开设置", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &show_float_item, &prefs, &quit])?;

    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip("Ringtone")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
                // 主窗口回来了，浮窗就该让位
                sync_float(app);
            }
            // 和快捷键 / 鼠标侧键同一条路径
            "float" => {
                if let Err(e) = show_float_now(app) {
                    eprintln!("[ringtone] 托盘叫浮窗失败：{e}");
                }
            }
            // 设置面板是前端画的，Rust 这边只能「把主窗口拿出来 + 通知它打开设置」
            "prefs" => {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.unminimize();
                    let _ = w.show();
                    let _ = w.set_focus();
                }
                let _ = app.emit(OPEN_SETTINGS_EVENT, ());
                sync_float(app);
            }
            "quit" => app.exit(0),
            _ => {}
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// 托盘 / 其它地方要求前端打开设置面板时发的事件（前端 `App.tsx` 监听）。
///
/// 为什么要绕一道事件：设置面板是 React 状态，Rust 侧改不了，只能通知。
const OPEN_SETTINGS_EVENT: &str = "ui://open-settings";

/// 浮窗的窗口 label。`tauri.conf.json` 里只声明了 `main`，浮窗是运行时建的。
const FLOAT_LABEL: &str = "float";
/// 浮窗尺寸（逻辑像素）。和 `src/Float.tsx` 里的常量一致，改一个要改两个。
const FLOAT_W: f64 = 268.0;
const FLOAT_H: f64 = 236.0;
/// 收起后还留在屏幕上的宽度：太窄鼠标够不着，太宽挡视线
const FLOAT_SLIVER: f64 = 9.0;

/// 建浮窗：只建一次、先藏起来。
///
/// 地址带 `?float=1`，前端据此知道「这个实例画浮窗、不画主界面」——
/// 同一个 `index.html` 两套皮肤，打包体积不变。
fn build_float(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    WebviewWindowBuilder::new(
        app,
        FLOAT_LABEL,
        WebviewUrl::App("index.html?float=1".into()),
    )
    .title("Ringtone 浮窗")
    .inner_size(FLOAT_W, FLOAT_H)
    .decorations(false)
    .transparent(true)
    .shadow(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    .focused(false)
    .visible(false)
    .build()
}

/// 主窗口标题栏上的「显示浮窗」按过了吗。
///
/// 按过就一直显示，**不受「主窗口在用就收走」的自动逻辑影响**——否则点击按钮本身就会让
/// 主窗口获得焦点，`Focused(true)` 一进来浮窗立刻又被藏了，按钮看起来像坏的。
/// 再按一次「隐藏浮窗」就把自动逻辑恢复。
static FLOAT_ASKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 浮窗现在是显示的吗（标题栏按钮据此画开关状态）。
#[tauri::command]
fn float_visible(app: AppHandle) -> bool {
    app.get_webview_window(FLOAT_LABEL)
        .and_then(|f| f.is_visible().ok())
        .unwrap_or(false)
}

/// 标题栏按钮：显示 / 隐藏浮窗。返回切换之后的可见状态。
#[tauri::command]
fn toggle_float(app: AppHandle) -> Result<bool, String> {
    let Some(float) = app.get_webview_window(FLOAT_LABEL) else {
        return Err("浮窗还没建出来".to_string());
    };
    if float.is_visible().unwrap_or(false) {
        FLOAT_ASKED.store(false, Ordering::Relaxed);
        float.hide().map_err(|e| format!("隐藏浮窗失败：{e}"))?;
        Ok(false)
    } else {
        FLOAT_ASKED.store(true, Ordering::Relaxed);
        show_float_now(&app)?;
        Ok(true)
    }
}

/// 主窗口「用户在用吗」→ 浮窗该不该出现。
///
/// 触发点：主窗口的 Resized（覆盖最小化/还原）、Focused(true)、CloseRequested，
/// 以及托盘菜单的「显示主窗口」。**不在这里算坐标**——贴边、收起、展开都是
/// 浮窗页面自己的事（只有它知道鼠标在哪、动画到哪一帧）。
fn sync_float(app: &AppHandle) {
    let Some(main) = app.get_webview_window("main") else {
        return;
    };
    let Some(float) = app.get_webview_window(FLOAT_LABEL) else {
        return;
    };
    // 「可见」但已最小化 = 不在用：点最小化时浮窗就该顶上
    let minimized = main.is_minimized().unwrap_or(false);
    let in_use = main.is_visible().unwrap_or(false) && !minimized;
    // 主窗口还没最小化就顺手把落点算准（这是唯一算得准的时候）
    if !minimized {
        let _ = compute_dock(app, &main);
    }
    // 标题栏上「显示浮窗」按过就一直显示（不然用户刚打开、鼠标一进主窗口就被自动逻辑收走）
    let want = FLOAT_ASKED.load(Ordering::Relaxed) || !in_use;
    if float.is_visible().unwrap_or(false) == want {
        return;
    }
    if want {
        // 先把窗口摆到收起位再显示，免得在旧位置闪一下（页面收到事件后还会自己再算一遍）
        if let Ok(dock) = float_dock(app.clone()) {
            let _ = float.set_position(tauri::PhysicalPosition::new(dock.collapsed_x, dock.y));
        }
        let _ = float.show();
        // 别的程序可能把「最前面」抢走，每次出现都重申一遍
        let _ = float.set_always_on_top(true);
        // 通知页面：贴边站好（按当前屏幕算坐标）并亮一会儿
        let _ = float.emit("float://show", minimized);
        println!(
            "[ringtone] 浮窗已显示（主窗口{}）",
            if minimized {
                "已最小化"
            } else {
                "已收进托盘"
            }
        );
    } else {
        let _ = float.hide();
    }
}

/// 「一键显示浮窗」的快捷键存在 `settings` 表的哪个键下。
const HOTKEY_KEY: &str = "float.hotkey";

/// 开机自启时附带的启动参数（写进 Windows 注册表那一项）。
/// 见到它就表示「这次是被开机自启拉起来的」，主窗口起来就收进托盘，不往桌面上弹。
const MINIMIZED_ARG: &str = "--minimized";

/// 这次启动带没带 `--minimized`（手动双击 exe 时不会带）。
fn started_minimized() -> bool {
    std::env::args().any(|a| a == MINIMIZED_ARG)
}

/// 应用数据目录（数据库 `ringtone.db`、背景图 `backgrounds/` 都在里面）。
///
/// 默认是系统给的应用目录；设了环境变量 `RINGTONE_DATA_DIR` 就用它。
/// 加这个口子是为了**用干净数据截 README 的图**：拿另一个目录起一个实例，
/// 里面放演示用的待办，绝不碰用户自己那份真实数据。
///
/// ```bash
/// RINGTONE_DATA_DIR="$TEMP/rt-demo" pnpm tauri dev
/// ```
pub fn data_dir(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    if let Ok(p) = std::env::var("RINGTONE_DATA_DIR") {
        let p = p.trim();
        if !p.is_empty() {
            return Ok(std::path::PathBuf::from(p));
        }
    }
    app.path()
        .app_data_dir()
        .map_err(|e| format!("取应用数据目录失败：{e}"))
}

/// 当前生效的快捷键（进程内记着，换绑时要先把旧的卸掉）。
static HOTKEY: Mutex<Option<String>> = Mutex::new(None);

/// 把浮窗叫出来（不管主窗口现在是什么状态）。
///
/// 与 `sync_float` 的区别：`sync_float` 是「用户不用主窗口了 → 顶上」的被动逻辑，
/// 这里是用户主动要（按快捷键），所以不判断主窗口状态。
fn show_float_now(app: &AppHandle) -> Result<(), String> {
    let Some(float) = app.get_webview_window(FLOAT_LABEL) else {
        return Err("浮窗还没建出来".to_string());
    };
    // 先摆到收起位再显示，免得在旧位置闪一下（页面收到事件后会自己再算一遍）
    if let Ok(dock) = float_dock(app.clone()) {
        let _ = float.set_position(tauri::PhysicalPosition::new(dock.collapsed_x, dock.y));
    }
    let _ = float.set_always_on_top(true);
    float.show().map_err(|e| format!("显示浮窗失败：{e}"))?;
    // 页面收到后会贴边站好并亮一会儿（和主窗口最小化时同一条路径）
    let _ = float.emit("float://show", false);
    Ok(())
}

/// 注册 / 换绑快捷键。传空字符串 = 取消。
///
/// 换绑会**先卸旧的、失败再把旧的装回去**：否则一次手滑（填了个被别的程序占用的组合）
/// 就会把原本能用的快捷键也弄丢。
fn apply_hotkey(app: &AppHandle, accel: &str) -> Result<String, String> {
    let gs = app.global_shortcut();
    let mut cur = HOTKEY.lock().map_err(|_| "快捷键状态锁坏了".to_string())?;
    let old = cur.clone();
    if let Some(prev) = old.as_deref() {
        let _ = gs.unregister(prev);
    }
    if accel.is_empty() {
        *cur = None;
        return Ok(String::new());
    }
    if let Err(e) = gs.register(accel) {
        if let Some(prev) = old.as_deref() {
            let _ = gs.register(prev);
        }
        return Err(format!(
            "这个快捷键注册不上（可能格式不对，或被别的程序占用了）：{e}"
        ));
    }
    *cur = Some(accel.to_string());
    Ok(accel.to_string())
}

/// 按快捷键 / 界面按钮把浮窗叫出来。
#[tauri::command]
fn float_show(app: AppHandle) -> Result<(), String> {
    show_float_now(&app)
}

/// 当前的快捷键（空字符串 = 没设）。
#[tauri::command]
fn get_float_hotkey(state: State<'_, AppState>) -> Result<String, String> {
    let conn = state.conn.lock().map_err(|_| "数据库锁坏了".to_string())?;
    Ok(db::setting_get(&conn, HOTKEY_KEY)
        .map_err(|e| e.to_string())?
        .unwrap_or_default())
}

/// 设置快捷键：先试着注册，成功了才写进设置（免得库里存着一个根本注册不上的组合）。
#[tauri::command]
fn set_float_hotkey(
    app: AppHandle,
    state: State<'_, AppState>,
    accel: String,
) -> Result<String, String> {
    let applied = apply_hotkey(&app, accel.trim())?;
    let conn = state.conn.lock().map_err(|_| "数据库锁坏了".to_string())?;
    db::setting_set(&conn, HOTKEY_KEY, &applied).map_err(|e| e.to_string())?;
    Ok(applied)
}

/// 设置鼠标侧键：先试装钩子，成功了才写进设置（和键盘快捷键同一个原则）。
#[tauri::command]
fn set_mouse_button(state: State<'_, AppState>, button: String) -> Result<String, String> {
    let applied = mouse::apply(&button)?;
    let conn = state.conn.lock().map_err(|_| "数据库锁坏了".to_string())?;
    db::setting_set(&conn, mouse::KEY, &applied).map_err(|e| e.to_string())?;
    Ok(applied)
}

/// 当前接管的鼠标侧键（`off` = 没接管）。
#[tauri::command]
fn get_mouse_button(state: State<'_, AppState>) -> Result<String, String> {
    let conn = state.conn.lock().map_err(|_| "数据库锁坏了".to_string())?;
    Ok(db::setting_get(&conn, mouse::KEY)
        .map_err(|e| e.to_string())?
        .unwrap_or_else(|| mouse::OFF.to_string()))
}

/// 浮窗里点一下 → 把主窗口叫回来。
///
/// 窗口 API 也能从浮窗里调，但「先 unminimize 再 show 再 focus」这个顺序
/// 放在 Rust 侧写一遍就够了；顺便让 `sync_float` 把浮窗自己收起来。
#[tauri::command]
fn show_main(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
    sync_float(&app);
}

/// 主界面空白右键 → 「收进托盘」。
///
/// 为什么不直接从前端 `getCurrentWindow().hide()`：那样**绕过了 `sync_float`**，
/// 主窗口藏了浮窗却不顶上（标题栏 ✕ 走的 `CloseRequested` 分支是先 hide 再 `sync_float`）。
/// 走这条命令，两条收工路径的行为就一致了。
#[tauri::command]
fn hide_main(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("main") {
        w.hide().map_err(|e| format!("收进托盘失败：{e}"))?;
    }
    sync_float(&app);
    Ok(())
}

/// 浮窗贴边要用的坐标。
///
/// **单位是物理像素**（和 `PhysicalPosition` 同一套）：这台机器双屏缩放不一样（一块 150%、
/// 一块 100%），Tauri 报出来的 `scale_factor` 还是错的（报 1、真实 1.5），只要经过一次
/// 「逻辑像素」换算就会跑到屏幕外面去。全程用物理像素就不用赌缩放了。
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct FloatDock {
    /// 贴左边还是右边（用户把浮窗拖到另一头就会变）
    pub side: &'static str,
    pub collapsed_x: f64,
    pub expanded_x: f64,
    pub y: f64,
    /// 目标屏的边界（物理像素）。前端拖动吸附时用它判断「靠哪边」、夹住竖向位置，
    /// 这样前端不用自己去问显示器（问到的可能是拖动中临时经过的那块屏）。
    pub mon_left: f64,
    pub mon_right: f64,
    pub mon_top: f64,
    pub mon_bottom: f64,
    /// 浮窗在那块屏上会占的物理尺寸（同上，前端别自己猜）
    pub win_w: f64,
    pub win_h: f64,
}

/// 浮窗贴哪一边。用户在浮窗上拖一把就换边（见 `float_set_side`）。
static FLOAT_SIDE: std::sync::Mutex<&'static str> = std::sync::Mutex::new("right");

/// 算落点要的全部原始数据：「主窗口那块屏」+「浮窗自己的像素尺寸」。
///
/// 缓存它（而不是缓存算好的 x）是为了支持换边：换边只是同一块屏上的左右镜像，
/// 不该因为「这时候主窗口正好被最小化」就算不出来。
#[derive(Debug, Clone, Copy, PartialEq)]
struct DockBase {
    mon_x: f64,
    mon_y: f64,
    mon_w: f64,
    mon_h: f64,
    win_w: f64,
    win_h: f64,
    scale: f64,
}

/// 主窗口上一次「在用」时的屏幕信息。
///
/// 为什么要缓存：主窗口一最小化，系统就把它挪到屏幕外面（实测 -32000），这时候问它的
/// `current_monitor()` 拿到的是一块 `size=0` 的假显示器（实测算出 `y=0`、x 落在屏幕中间），
/// 浮窗就会贴到奇怪的位置。所以只在主窗口“在用”的时候现算并记下，要贴边时用记下的那份。
static LAST_BASE: std::sync::Mutex<Option<DockBase>> = std::sync::Mutex::new(None);

/// 把「缓存里的屏幕信息 + 当前贴边方向」算成具体坐标。
fn dock_from(base: DockBase, side: &str) -> FloatDock {
    let right = base.mon_x + base.mon_w;
    // 收起时留在屏幕上的那条细边：太窄鼠标够不着，太宽挡视线
    let sliver = (FLOAT_SLIVER * base.scale).round();
    let (collapsed_x, expanded_x) = if side == "left" {
        // 贴左边：收起时窗口的左半部分在屏幕外，只露右边那条细边
        (base.mon_x - base.win_w + sliver, base.mon_x)
    } else {
        // 贴右边：展开时整个窗口贴右边缘（用窗口自己的像素宽，不猜缩放）
        (right - sliver, right - base.win_w)
    };
    FloatDock {
        side: if side == "left" { "left" } else { "right" },
        collapsed_x,
        expanded_x,
        // 竖着放在偏上的位置：视线自然落点，不挡右下角的托盘区
        y: (base.mon_y + base.mon_h * 0.26).round(),
        mon_left: base.mon_x,
        mon_right: right,
        mon_top: base.mon_y,
        mon_bottom: base.mon_y + base.mon_h,
        win_w: base.win_w,
        win_h: base.win_h,
    }
}

/// 浮窗该按哪块屏算。
///
/// **浮窗自己那块屏优先**：它可见时给的位置是真的，而且必须用它那块屏的缩放。
/// 两块屏缩放不同时（本机 2560x1600@150% + 2560x1440@100%），浮窗在 100% 那块屏上
/// 实际只有 268 物理像素宽，却按 150% 算成 402——贴边位置就会差一百多像素，
/// 一拖到两屏交界处就在两个值之间来回跳（这就是用户看到的「边缘会在两个显示器变动」）。
///
/// 浮窗还没显示（或位置读不到）时退回主窗口那块屏，保持启动时的老行为。
fn dock_monitor(app: &AppHandle, main: &WebviewWindow) -> Result<tauri::Monitor, String> {
    if let Some(float) = app.get_webview_window(FLOAT_LABEL) {
        if float.is_visible().unwrap_or(false) {
            if let (Ok(pos), Ok(size)) = (float.outer_position(), float.outer_size()) {
                let cx = pos.x as f64 + size.width as f64 / 2.0;
                let cy = pos.y as f64 + size.height as f64 / 2.0;
                // 先用左上角往里挪 2px（贴右边时它就在目标屏内），不行再退到窗口中心
                // （贴左边时窗口有一半在屏外，左上角反而是屏外的点）。
                for (x, y) in [(pos.x as f64 + 2.0, pos.y as f64 + 2.0), (cx, cy)] {
                    if let Ok(Some(m)) = float.monitor_from_point(x, y) {
                        if m.size().width > 0 && m.size().height > 0 {
                            return Ok(m);
                        }
                    }
                }
            }
        }
    }
    match main.current_monitor() {
        Ok(Some(m)) => Ok(m),
        _ => main
            .primary_monitor()
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "读不到显示器信息".to_string()),
    }
}

/// 现算一份屏幕信息并记下来。
fn refresh_base(app: &AppHandle, main: &WebviewWindow) -> Result<DockBase, String> {
    let mon = dock_monitor(app, main)?;
    // 主窗口被最小化时系统会把它挪到屏幕外，那时问到的是一块 size=0 的假显示器：
    // 宁可不改位置，也不要把这种垃圾值缓下来
    if mon.size().width == 0 || mon.size().height == 0 {
        return Err(format!(
            "显示器尺寸异常（{:?}），不拿它算浮窗落点",
            mon.size()
        ));
    }
    // 缩放只信显示器（实测 DISPLAY20 报 1.5 ✓、DISPLAY21 报 1 ✓，都可以用），
    // **不要去问窗口自己的尺寸**：窗口的物理尺寸取决于它当前落在哪块屏上，而浮窗贴边时
    // 正好横跨两块 DPI 不同的屏——实测同一份代码先后量到 268x236（它在 100% 那块屏上）
    // 和 402x354（在 150% 那块上），拿它算「展开时该往左挪多少」会在两个答案之间跳。
    let scale = {
        let s = mon.scale_factor();
        if s.is_finite() && s > 0.0 {
            s
        } else {
            1.0
        }
    };
    let base = DockBase {
        mon_x: mon.position().x as f64,
        mon_y: mon.position().y as f64,
        mon_w: mon.size().width as f64,
        mon_h: mon.size().height as f64,
        // 窗口是 `decorations(false)` + `resizable(false)`，外框=内容：
        // 逻辑尺寸乘目标屏缩放就是它会占的物理像素
        win_w: FLOAT_W * scale,
        win_h: FLOAT_H * scale,
        scale,
    };
    if let Ok(mut slot) = LAST_BASE.lock() {
        *slot = Some(base);
    }
    Ok(base)
}

/// 现在该把浮窗摆哪：屏幕信息有缓存就用缓存（主窗口已最小化时只有它靠得住）。
fn dock_now() -> Result<FloatDock, String> {
    let side = FLOAT_SIDE.lock().map(|s| *s).unwrap_or("right");
    let base = LAST_BASE
        .lock()
        .ok()
        .and_then(|slot| *slot)
        .ok_or_else(|| "还没有可用的显示器信息".to_string())?;
    let dock = dock_from(base, side);
    let mut changed = true;
    if let Ok(mut slot) = LAST_DOCK.lock() {
        changed = *slot != Some(dock);
        *slot = Some(dock);
    }
    if changed {
        println!(
            "[ringtone] 浮窗落点：side={} collapsed_x={} expanded_x={} y={}（窗口 {}x{} 物理像素）",
            dock.side, dock.collapsed_x, dock.expanded_x, dock.y, base.win_w, base.win_h
        );
    }
    Ok(dock)
}

/// 现算落点（顺手更新缓存）。
fn compute_dock(app: &AppHandle, main: &WebviewWindow) -> Result<FloatDock, String> {
    refresh_base(app, main)?;
    dock_now()
}

/// 上一次算好的落点：只在「真的变了」时打日志用（`Moved` 会连着发很多次）
static LAST_DOCK: std::sync::Mutex<Option<FloatDock>> = std::sync::Mutex::new(None);

/// 现在该把浮窗摆哪：主窗口没最小化就现算，已经最小化就用上次记下的。
///
/// 为什么坐标在 Rust 算、不给前端算：WebView 里的 `window.screen` / `screenX` 在这台机器上
/// 和窗口真实位置对不上（实测 `screenX=4258`，而窗口其实在 4852），显示器读数反而是准的。
/// 为什么用**浮窗自己**所在那块屏的缩放、不用主窗口的：见 `dock_monitor` 的注释。
#[tauri::command]
fn float_dock(app: AppHandle) -> Result<FloatDock, String> {
    let main = app
        .get_webview_window("main")
        .ok_or_else(|| "找不到主窗口".to_string())?;
    // 判「能不能信这块显示器」用 is_minimized，不用 is_visible：
    // 刚启动时窗口可能还没报可见，但显示器信息已经是准的。
    let _ = refresh_base(&app, &main);
    dock_now()
}

/// 换边：用户在浮窗上拖一把、吸附到另一条边之后调它。
///
/// 这里**总是**重算一遍屏幕信息：浮窗正在被拖，它的位置就是最准的输入
/// （`dock_monitor` 会优先用它自己那块屏）。失败就算了，等下次。
#[tauri::command]
fn float_set_side(app: AppHandle, side: String) -> Result<FloatDock, String> {
    let want: &'static str = if side == "left" { "left" } else { "right" };
    if let Ok(mut slot) = FLOAT_SIDE.lock() {
        *slot = want;
    }
    let main = app
        .get_webview_window("main")
        .ok_or_else(|| "找不到主窗口".to_string())?;
    let _ = refresh_base(&app, &main);
    dock_now()
}
