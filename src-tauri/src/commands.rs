//! Tauri 命令层：薄封装，只做参数校验 + 错误转字符串。

use std::path::PathBuf;
use std::sync::Mutex;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;

use crate::channel::{dingtalk, feishu};
use crate::models::{Task, TaskInput};
use crate::time;
use crate::{background, db, deliver, port, work};

/// 全局状态：单连接 + 互斥锁（本地单机应用，够用且无连接池复杂度）
pub struct AppState {
    pub conn: Mutex<Connection>,
}

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// 返回本机今天的 `YYYY-MM-DD`（由后端决定，避免前端时区/格式不一致）
#[tauri::command]
pub fn today_local() -> String {
    time::today_local()
}

#[tauri::command]
pub fn list_tasks(state: State<'_, AppState>, day: String) -> Result<Vec<Task>, String> {
    time::validate_day(&day)?;
    let conn = state.conn.lock().map_err(err)?;
    db::list_by_day(&conn, &day).map_err(err)
}

/// 区间内每天的待办数量。一周一次调用（周条的圆点），当天的那格顺手算「今日 x/y」。
#[tauri::command]
pub fn count_tasks_by_day(
    state: State<'_, AppState>,
    from_day: String,
    to_day: String,
) -> Result<Vec<db::DayCount>, String> {
    time::validate_day(&from_day)?;
    time::validate_day(&to_day)?;
    // 传反了就直接报错，别静默返回空表——空表在界面上长得像「这几天都没安排」
    if from_day > to_day {
        return Err(format!("起始日期 {from_day} 不能晚于结束日期 {to_day}"));
    }
    let conn = state.conn.lock().map_err(err)?;
    db::count_by_day(&conn, &from_day, &to_day).map_err(err)
}

/// 下一条将要响的提醒；没有任何安排时返回 `null`。
#[tauri::command]
pub fn next_reminder(state: State<'_, AppState>) -> Result<Option<db::NextReminder>, String> {
    let conn = state.conn.lock().map_err(err)?;
    db::next_reminder(&conn, time::now_ms()).map_err(err)
}

/// 右下角的两个数字：今天多少条、全部多少条。
#[tauri::command]
pub fn task_stats(state: State<'_, AppState>) -> Result<db::TaskStats, String> {
    let conn = state.conn.lock().map_err(err)?;
    db::task_stats(&conn).map_err(err)
}

/// 内置的节假日数据覆盖到哪几年（界面上给「工作日」与上下班提醒做提示用）。
#[tauri::command]
pub fn holiday_info() -> crate::holiday::HolidayInfo {
    crate::holiday::info()
}

#[tauri::command]
pub fn create_task(state: State<'_, AppState>, input: TaskInput) -> Result<Task, String> {
    let input = input.validated()?;
    let conn = state.conn.lock().map_err(err)?;
    db::create_with_occurrences(&conn, &input).map_err(err)
}

#[tauri::command]
pub fn update_task(
    state: State<'_, AppState>,
    id: i64,
    input: TaskInput,
) -> Result<Option<Task>, String> {
    let input = input.validated()?;
    let conn = state.conn.lock().map_err(err)?;
    db::update(&conn, id, &input).map_err(err)
}

#[tauri::command]
pub fn set_task_done(state: State<'_, AppState>, id: i64, done: bool) -> Result<(), String> {
    let conn = state.conn.lock().map_err(err)?;
    let n = db::set_done(&conn, id, done).map_err(err)?;
    if n == 0 {
        return Err(format!("任务不存在：{id}"));
    }
    Ok(())
}

#[tauri::command]
pub fn set_task_star(state: State<'_, AppState>, id: i64, star: bool) -> Result<(), String> {
    let conn = state.conn.lock().map_err(err)?;
    let n = db::set_star(&conn, id, star).map_err(err)?;
    if n == 0 {
        return Err(format!("任务不存在：{id}"));
    }
    Ok(())
}

#[tauri::command]
pub fn delete_tasks(state: State<'_, AppState>, ids: Vec<i64>) -> Result<usize, String> {
    if ids.is_empty() {
        return Err("没有指定要删除的任务".to_string());
    }
    let conn = state.conn.lock().map_err(err)?;
    db::delete_many(&conn, &ids).map_err(err)
}

/// 勾选「完成本次」：只完成 `day` 这一天，重复系列继续。
///
/// 跟 `set_task_done`（完成整个任务，重复也跟着停）是两件事，界面按 `Task.repeated` 分流。
#[tauri::command]
pub fn set_occurrence_done(
    state: State<'_, AppState>,
    task_id: i64,
    day: String,
    done: bool,
) -> Result<(), String> {
    time::validate_day(&day)?;
    let conn = state.conn.lock().map_err(err)?;
    db::set_occurrence_done(&conn, task_id, &day, done).map_err(err)?;
    Ok(())
}

/// 停止重复：任务本身留着（还留在它自己那天），只把规则清掉并回收之后展开出来的实例。
#[tauri::command]
pub fn stop_repeat(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    let conn = state.conn.lock().map_err(err)?;
    let n = db::stop_repeat(&conn, id).map_err(err)?;
    if n == 0 {
        return Err(format!("任务不存在，或它本来就不是重复任务：{id}"));
    }
    Ok(())
}

// ---------- M3：渠道配置 ----------

/// 前端提交的飞书渠道配置。
/// `app_secret` 为 `None`/空串表示「不动已存的凭证」——只改接收方时不用重填密钥。
#[derive(Debug, Deserialize)]
pub struct FeishuChannelInput {
    /// 渠道名（默认「飞书」）。`channels.name` 有 UNIQUE 约束，是 upsert 的锚点。
    #[serde(default)]
    pub name: Option<String>,
    pub enabled: bool,
    #[serde(default)]
    pub priority: Option<i64>,
    pub app_id: String,
    #[serde(default)]
    pub app_secret: Option<String>,
    #[serde(default)]
    pub receive_id_type: Option<String>,
    pub receive_id: String,
}

#[tauri::command]
pub fn list_channels(state: State<'_, AppState>) -> Result<Vec<db::Channel>, String> {
    let conn = state.conn.lock().map_err(err)?;
    db::list_channels(&conn).map_err(err)
}

/// 保存（新建或更新）飞书渠道，返回渠道 id。
/// 校验统一走 [`feishu::FeishuConfig::from_json`]，不在前端和后端维护两套规则。
#[tauri::command]
pub fn save_feishu_channel(
    state: State<'_, AppState>,
    input: FeishuChannelInput,
) -> Result<i64, String> {
    let name = input
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("飞书")
        .to_string();

    let raw = serde_json::json!({
        "app_id": input.app_id,
        "receive_id_type": input.receive_id_type.clone().unwrap_or_default(),
        "receive_id": input.receive_id,
    })
    .to_string();
    let cfg = feishu::FeishuConfig::from_json(&raw).map_err(err)?;
    let config_json = cfg.to_json().map_err(err)?;

    let secret = input
        .app_secret
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let conn = state.conn.lock().map_err(err)?;

    // 首次配置必须有密钥：否则渠道看上去「已启用」，实际每次发送都报配置错。
    if secret.is_none() {
        let already_has_secret = db::list_channels(&conn)
            .map_err(err)?
            .into_iter()
            .any(|c| c.name == name && c.secret_set);
        if !already_has_secret {
            return Err("首次配置必须填写 app_secret".to_string());
        }
    }

    db::upsert_channel(
        &conn,
        &name,
        feishu::KIND,
        input.enabled,
        input.priority.unwrap_or(100),
        &config_json,
        secret,
    )
    .map_err(err)
}

#[tauri::command]
pub fn set_channel_enabled(
    state: State<'_, AppState>,
    id: i64,
    enabled: bool,
) -> Result<(), String> {
    let conn = state.conn.lock().map_err(err)?;
    let n = db::set_channel_enabled(&conn, id, enabled).map_err(err)?;
    if n == 0 {
        return Err(format!("渠道不存在：{id}"));
    }
    Ok(())
}

#[tauri::command]
pub fn delete_channel(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    let conn = state.conn.lock().map_err(err)?;
    let n = db::delete_channel(&conn, id).map_err(err)?;
    if n == 0 {
        return Err(format!("渠道不存在：{id}"));
    }
    Ok(())
}

/// 前端提交的钉钉渠道配置。
///
/// `webhook` 允许粘整条官方 webhook，也允许只填裸 `access_token`；
/// `sign_secret` 为 `None`/空串 = 不动已存的加签密钥（也允许本来就没填）。
#[derive(Debug, Deserialize)]
pub struct DingtalkChannelInput {
    #[serde(default)]
    pub name: Option<String>,
    pub enabled: bool,
    #[serde(default)]
    pub priority: Option<i64>,
    pub webhook: String,
    #[serde(default)]
    pub sign_secret: Option<String>,
}

/// 保存（新建或更新）钉钉渠道，返回渠道 id。
#[tauri::command]
pub fn save_dingtalk_channel(
    state: State<'_, AppState>,
    input: DingtalkChannelInput,
) -> Result<i64, String> {
    let name = input
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("钉钉")
        .to_string();

    let raw = serde_json::json!({ "webhook": input.webhook }).to_string();
    // 校验统一走渠道自己的 `from_json`，不在前端和后端维护两套规则
    let cfg = dingtalk::DingtalkConfig::from_json(&raw).map_err(err)?;
    let config_json = cfg.to_json().map_err(err)?;

    let secret = input
        .sign_secret
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let conn = state.conn.lock().map_err(err)?;
    db::upsert_channel(
        &conn,
        &name,
        dingtalk::KIND,
        input.enabled,
        input.priority.unwrap_or(100),
        &config_json,
        secret,
    )
    .map_err(err)
}

/// 设为主渠道：把自己的 `priority` 排到所有其他渠道前面（降级链从它开始试）。
/// 返回生效后的 priority，方便前端直接回显。
#[tauri::command]
pub fn set_channel_primary(state: State<'_, AppState>, id: i64) -> Result<i64, String> {
    let conn = state.conn.lock().map_err(err)?;
    let n = db::make_primary(&conn, id).map_err(err)?;
    if n == 0 {
        return Err(format!("渠道不存在：{id}"));
    }
    db::list_channels(&conn)
        .map_err(err)?
        .into_iter()
        .find(|c| c.id == id)
        .map(|c| c.priority)
        .ok_or_else(|| format!("渠道不存在：{id}"))
}

/// 界面角标用：累计「全渠道失败」条数
#[tauri::command]
pub fn failure_count() -> usize {
    deliver::failure_count()
}

/// 用户点「知道了」：清掉红点计数（发送记录里的历史不会动）
#[tauri::command]
pub fn acknowledge_failures() {
    deliver::clear_failures();
}

/// 「发一条测试消息」：这条**真的会发出去**，用来证明端到端能通。
/// 不走到点流程、不碰 `occurrences`，但会写 `send_log`（结果 `test_sent` / `test_failed`）。
#[tauri::command]
pub fn test_channel(state: State<'_, AppState>, id: i64) -> Result<String, String> {
    let rec = {
        let conn = state.conn.lock().map_err(err)?;
        db::channel_record(&conn, id)
            .map_err(err)?
            .ok_or_else(|| format!("渠道不存在：{id}"))?
    }; // 锁在这里释放：发送是阻塞的 HTTP 请求

    let text = format!(
        "这是 Ringtone 的测试消息（{}）。收到它说明渠道配置正确，到点提醒会发到这里。",
        time::today_local()
    );

    let conn = state.conn.lock().map_err(err)?;
    let ts = time::now_ms();
    match deliver::send_test(&rec, &text) {
        Ok(receipt) => {
            let detail = format!("{}：测试消息 {receipt}", rec.channel.name);
            db::log_event(
                &conn,
                ts,
                Some(rec.channel.id),
                None,
                "test_sent",
                Some(200),
                Some(&detail),
            )
            .map_err(err)?;
            Ok(format!("已发送，{receipt}"))
        }
        Err(e) => {
            // 失败的这条也要写进发送记录：设置面板要能看到「上次为什么没发出去」
            let detail = format!("{}：测试失败：{e}", rec.channel.name);
            db::log_event(
                &conn,
                ts,
                Some(rec.channel.id),
                None,
                "test_failed",
                e.http_code,
                Some(&detail),
            )
            .map_err(err)?;
            Err(e.to_string())
        }
    }
}

/// 「仅验证凭据」：能做静态/只读校验的渠道就做，做不到的渠道如实说明。
/// 跟 `test_channel` 的区别是：想确认配置对不对、又不想往群里发消息时用这个。
#[tauri::command]
pub fn verify_channel(state: State<'_, AppState>, id: i64) -> Result<String, String> {
    let rec = {
        let conn = state.conn.lock().map_err(err)?;
        db::channel_record(&conn, id)
            .map_err(err)?
            .ok_or_else(|| format!("渠道不存在：{id}"))?
    };

    match rec.channel.kind.as_str() {
        feishu::KIND => {
            // 飞书有只读接口（取 token），可以真的验一把
            let cfg = feishu::FeishuConfig::from_json(&rec.channel.config_json).map_err(err)?;
            let secret = rec
                .secret
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| "该渠道还没填 app_secret".to_string())?;
            let mut client = feishu::FeishuClient::new(cfg, secret).map_err(err)?;
            client.verify().map_err(err)
        }
        dingtalk::KIND => {
            // 钉钉机器人只有一个写接口，没有「只验证不发消息」的办法，不假装能验
            let cfg = dingtalk::DingtalkConfig::from_json(&rec.channel.config_json).map_err(err)?;
            let client = dingtalk::DingtalkClient::new(cfg, rec.secret.clone()).map_err(err)?;
            client.verify().map_err(err)
        }
        other => Err(format!("暂不支持验证该渠道类型：{other}")),
    }
}

/// 最近的发送记录（设置面板用）
#[tauri::command]
pub fn recent_logs(
    state: State<'_, AppState>,
    limit: Option<i64>,
) -> Result<Vec<db::LogEntry>, String> {
    let conn = state.conn.lock().map_err(err)?;
    db::recent_logs(&conn, limit.unwrap_or(50)).map_err(err)
}

// ---------------------------------------------------------------- 上下班提醒

/// 当前上下班设置。
#[tauri::command]
pub fn work_get(state: State<'_, AppState>) -> Result<work::WorkSchedule, String> {
    let conn = state.conn.lock().map_err(err)?;
    Ok(work::load(&conn))
}

/// 保存上下班设置：
/// 成功返回新的「距离下班」状态（前端省一次往返）。
#[tauri::command]
pub fn work_save(
    state: State<'_, AppState>,
    schedule: work::WorkSchedule,
) -> Result<work::WorkStatus, String> {
    let conn = state.conn.lock().map_err(err)?;
    work::save(&conn, &schedule)?;
    Ok(work::status(&schedule, time::now_ms()))
}

/// 重新发一次隐藏任务（给「设了但今天没生效」善后用：
/// 改完设置后调度线程不用重启，实例已经当场建好了）。
#[tauri::command]
pub fn work_status(state: State<'_, AppState>) -> Result<work::WorkStatus, String> {
    let conn = state.conn.lock().map_err(err)?;
    let s = work::load(&conn);
    Ok(work::status(&s, time::now_ms()))
}

// ---------------------------------------------------------------- 背景图

/// 选一张图当背景（复制进应用目录），返回给前端直接用的 data URL。
#[tauri::command]
pub async fn background_pick(
    app: AppHandle,
    state: State<'_, AppState>,
    which: String,
) -> Result<Option<String>, String> {
    background::check_which(&which)?;
    // 弹窗闭包要拿走一个 AppHandle，所以先克隆一份：后面还要用 `app` 取应用目录
    let app_for_dialog = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app_for_dialog
            .dialog()
            .file()
            .set_title("选一张背景图")
            .add_filter("图片", &["png", "jpg", "jpeg", "webp", "gif", "bmp"])
            .blocking_pick_file()
    })
    .await
    .map_err(|e| format!("打开选文件窗口失败：{e}"))?;
    let Some(picked) = picked else {
        return Ok(None); // 用户取消
    };
    let src = file_path(picked)?;
    let app_dir = app_dir(&app)?;
    let conn = state.conn.lock().map_err(err)?;
    background::save(&conn, &app_dir, &which, &src)?;
    let url = background::data_url(&conn, &app_dir, &which)?;
    println!("[ringtone] 背景图已更新：{which} ← {}", src.display());
    Ok(url)
}

/// 读当前背景图（启动时前端调一次）。
#[tauri::command]
pub fn background_get(
    app: AppHandle,
    state: State<'_, AppState>,
    which: String,
) -> Result<Option<String>, String> {
    let app_dir = app_dir(&app)?;
    let conn = state.conn.lock().map_err(err)?;
    background::data_url(&conn, &app_dir, &which)
}

/// 清掉背景图。
#[tauri::command]
pub fn background_clear(
    app: AppHandle,
    state: State<'_, AppState>,
    which: String,
) -> Result<(), String> {
    let app_dir = app_dir(&app)?;
    let conn = state.conn.lock().map_err(err)?;
    background::clear(&conn, &app_dir, &which)
}

/// 导入导出结果（`None` = 用户取消了弹窗）。
#[derive(Debug, Clone, Serialize)]
pub struct ExportResult {
    /// 存到哪里了（界面显示给用户看）
    pub path: String,
    pub tasks: usize,
    pub bytes: usize,
}

/// 导出全部待办。`kind` = `md` / `csv`。
#[tauri::command]
pub async fn export_tasks(
    app: AppHandle,
    state: State<'_, AppState>,
    kind: String,
) -> Result<Option<ExportResult>, String> {
    let (content, ext, tasks) = {
        let conn = state.conn.lock().map_err(err)?;
        let tasks = db::all_tasks(&conn).map_err(err)?.len();
        match kind.as_str() {
            "md" | "markdown" => (port::export_markdown(&conn)?, "md", tasks),
            "csv" => (port::export_csv(&conn)?, "csv", tasks),
            other => return Err(format!("只支持导出 md 或 csv，收到：{other}")),
        }
    };
    let default_name = format!("ringtone-{}.{ext}", time::today_local());
    let ext2 = ext.to_string();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("导出待办")
            .set_file_name(&default_name)
            .add_filter(if ext2 == "csv" { "CSV" } else { "Markdown" }, &[&ext2])
            .blocking_save_file()
    })
    .await
    .map_err(|e| format!("打开保存窗口失败：{e}"))?;
    let Some(picked) = picked else {
        return Ok(None);
    };
    let path = file_path(picked)?;
    std::fs::write(&path, content.as_bytes()).map_err(|e| format!("写文件失败：{e}"))?;
    println!("[ringtone] 已导出 {tasks} 条 → {}", path.display());
    Ok(Some(ExportResult {
        path: path.display().to_string(),
        tasks,
        bytes: content.len(),
    }))
}

/// 导入待办（只新增不覆盖）。`None` = 用户取消了弹窗。
#[tauri::command]
pub async fn import_tasks(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<port::ImportSummary>, String> {
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("选一个待办文件（Markdown 或 CSV）")
            .add_filter("待办文件", &["md", "markdown", "csv", "txt"])
            .blocking_pick_file()
    })
    .await
    .map_err(|e| format!("打开选文件窗口失败：{e}"))?;
    let Some(picked) = picked else {
        return Ok(None);
    };
    let path = file_path(picked)?;
    let text = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "读不到这个文件（{}）：{e}\n提示：必须是 UTF-8 编码的文本（Excel 里另存为「CSV UTF-8」）",
            path.display()
        )
    })?;
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let conn = state.conn.lock().map_err(err)?;
    let sum = port::import(&conn, &name, &text)?;
    println!(
        "[ringtone] 导入完成：新增 {} 条 / 跳过 {} 条 / 失败 {} 条（{name}）",
        sum.added,
        sum.skipped,
        sum.failed.len()
    );
    Ok(Some(sum))
}

/// 应用数据目录（背景图、数据库都在里面）。
fn app_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map_err(|e| format!("取应用数据目录失败：{e}"))
}

/// 把弹窗返回的路径转成 `PathBuf`（桌面上永远是路径）。
fn file_path(p: tauri_plugin_dialog::FilePath) -> Result<PathBuf, String> {
    p.into_path()
        .map_err(|e| format!("这个位置不是本地路径：{e}"))
}
