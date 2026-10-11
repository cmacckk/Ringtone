//! 窗口背景图。
//!
//! 两个决定：
//!
//! 1. **选中的图片会复制进应用数据目录**（`backgrounds/main.<ext>`），不是记住原路径。
//!    用户把桌面那张图删了、挪了、U 盘拔了，背景就不该跟着消失。
//! 2. 图片以 **data URL** 交给前端（`background_data`），而不是让 webview 直接读文件。
//!    读本地文件要走 asset 协议 + 每次配一遍 scope，而背景图只有一张、一个窗口一份，
//!    编码成 base64 的开销（本地读盘）比配协议出错的风险划算得多。
//!
//! 遮罩透明度没存在这里：那是纯显示参数，前端 localStorage 里就够（跟外观设置同一套）。

use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use rusqlite::Connection;

use crate::db;

/// 设置键：`background.main` / `background.float`，值是**文件名**（不是全路径）。
pub fn key_of(which: &str) -> String {
    format!("background.{which}")
}

/// 只认这两个窗口，避免前端传个 `../../etc/passwd` 之类的字符串下来。
pub fn check_which(which: &str) -> Result<(), String> {
    match which {
        "main" | "float" => Ok(()),
        other => Err(format!("只支持 main / float 两个窗口，收到：{other}")),
    }
}

/// 图片按扩展名定 MIME；认不出的扩展名一律拒绝（免得出一个 webview 显示不了的背景）。
fn mime_of(path: &Path) -> Result<(&'static str, &'static str), String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    Ok(match ext.as_str() {
        "png" => ("png", "image/png"),
        "jpg" | "jpeg" => ("jpg", "image/jpeg"),
        "webp" => ("webp", "image/webp"),
        "gif" => ("gif", "image/gif"),
        "bmp" => ("bmp", "image/bmp"),
        other => {
            return Err(format!(
                "只支持 png / jpg / webp / gif / bmp 这几种图片，收到：{other}"
            ))
        }
    })
}

/// `backgrounds` 子目录（不存在就建）。
pub fn dir(app_dir: &Path) -> Result<PathBuf, String> {
    let d = app_dir.join("backgrounds");
    fs::create_dir_all(&d).map_err(|e| format!("建背景图目录失败：{e}"))?;
    Ok(d)
}

/// 把 `src` 复制成这个窗口的背景图。返回存下来的文件名。
///
/// 旧的同名文件（比如原来是 `main.png`、现在选了 jpg）会被清掉：
/// 否则设置里指着一张、磁盘上躺着另一张，很容易看成「选了没生效」。
pub fn save(conn: &Connection, app_dir: &Path, which: &str, src: &Path) -> Result<String, String> {
    check_which(which)?;
    let (ext, _) = mime_of(src)?;
    let data = fs::read(src).map_err(|e| format!("读不到这张图（{}）：{e}", src.display()))?;
    if data.is_empty() {
        return Err("这张图是空的（0 字节）".to_string());
    }
    let d = dir(app_dir)?;
    let name = format!("{which}.{ext}");
    fs::write(d.join(&name), &data).map_err(|e| format!("写背景图失败：{e}"))?;
    // 换格式时清掉旧的
    for other in ["png", "jpg", "webp", "gif", "bmp"] {
        if other == ext {
            continue;
        }
        let old = d.join(format!("{which}.{other}"));
        if old.exists() {
            let _ = fs::remove_file(old);
        }
    }
    db::setting_set(conn, &key_of(which), &name).map_err(|e| format!("保存设置失败：{e}"))?;
    Ok(name)
}

/// 清掉这个窗口的背景图（设置与文件都清）。
pub fn clear(conn: &Connection, app_dir: &Path, which: &str) -> Result<(), String> {
    check_which(which)?;
    if let Ok(Some(name)) = db::setting_get(conn, &key_of(which)) {
        let p = app_dir.join("backgrounds").join(&name);
        if p.exists() {
            let _ = fs::remove_file(p);
        }
    }
    db::setting_delete(conn, &key_of(which)).map_err(|e| format!("清设置失败：{e}"))?;
    Ok(())
}

/// 当前背景图的 data URL；没设置（或文件已丢）返回 `None`。
pub fn data_url(conn: &Connection, app_dir: &Path, which: &str) -> Result<Option<String>, String> {
    check_which(which)?;
    let Some(name) = db::setting_get(conn, &key_of(which)).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let path = app_dir.join("backgrounds").join(&name);
    let Ok(bytes) = fs::read(&path) else {
        // 文件没了：当成没设置（设置里那条留在库里无所谓，界面会显示「未设置」）
        return Ok(None);
    };
    let (_, mime) = mime_of(&path)?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(Some(format!("data:{mime};base64,{b64}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().expect("内存库应该能开");
        db::migrate(&conn).expect("建表应该成功");
        conn
    }

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ringtone-test-{name}"));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).expect("建临时目录应该成功");
        d
    }

    #[test]
    fn rejects_unknown_window() {
        let err = check_which("../main").expect_err("非法的窗口名要拒绝");
        assert!(err.contains("main"), "错误信息要说明支持什么，实际：{err}");
    }

    #[test]
    fn rejects_unknown_extension() {
        let err = mime_of(Path::new("a/heic")).expect_err("heic 不支持");
        assert!(err.contains("png"), "要列出支持的格式，实际：{err}");
    }

    #[test]
    fn save_then_read_round_trip() {
        let conn = mem();
        let app = tmpdir("bg");
        let src = app.join("src.png");
        // 1x1 的透明 PNG
        let png = [
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        fs::write(&src, png).expect("写测试图应该成功");

        let name = save(&conn, &app, "main", &src).expect("保存背景图应该成功");
        assert_eq!(name, "main.png", "文件名应该是 main.png，实际 {name}");
        let url = data_url(&conn, &app, "main")
            .expect("读背景图不该报错")
            .expect("应该读到背景图");
        assert!(
            url.starts_with("data:image/png;base64,"),
            "要返回 data URL，实际：{}",
            &url[..40.min(url.len())]
        );

        clear(&conn, &app, "main").expect("清除背景图应该成功");
        assert!(
            data_url(&conn, &app, "main").expect("读不该报错").is_none(),
            "清掉之后就不该再读到"
        );
    }
}
