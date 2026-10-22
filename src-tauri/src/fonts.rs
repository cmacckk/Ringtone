//! 系统里装了哪些字体（设置 → 外观的字体下拉用它）。
//!
//! 浏览器给不了「枚举已安装字体」的接口，所以直接读注册表：
//! `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts`（全机安装）
//! 与 `HKCU\...` 同路径（Win10 1809 起允许「为当前用户安装」字体）。
//! 值名形如 `微软雅黑 & Microsoft YaHei & Microsoft YaHei UI (TrueType)`：
//! 去掉结尾的 ` (TrueType)` 再按 `&` 拆开，每一段都是一个能直接写进 CSS 的字体名。
//!
//! 读不到（注册表被改坏 / 非 Windows）就回落一份常用的，界面不至于空着。

use std::sync::OnceLock;

/// 读不到注册表时的兜底（也就是外观面板原来那几项）。
const FALLBACK: &[&str] = &[
    "Microsoft YaHei",
    "微软雅黑",
    "DengXian",
    "Source Han Sans SC",
    "SimSun",
    "KaiTi",
];

/// 缓存：字体列表在进程生命周期里几乎不变，没必要每次开面板都枚举一遍注册表。
static CACHE: OnceLock<Vec<String>> = OnceLock::new();

/// 已安装字体名（去重、排序）。第一项永远是兜底里的系统字体，前端把它当「系统默认」。
pub fn list() -> Vec<String> {
    CACHE.get_or_init(build).clone()
}

fn build() -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    #[cfg(windows)]
    {
        names.extend(read_registry());
    }
    if names.is_empty() {
        names = FALLBACK.iter().map(|s| s.to_string()).collect();
    }
    names.sort_by_key(|s| s.to_lowercase());
    names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    names
}

/// 把一个注册表值名拆成字体名。`微软雅黑 & Microsoft YaHei (TrueType)` → 两个名字。
pub fn parse_value_name(raw: &str) -> Vec<String> {
    let mut base = raw.trim();
    // 去掉结尾的 "(TrueType)" / "(OpenType)" / "(All res)" 之类
    while base.ends_with(')') {
        match base.rfind(" (") {
            Some(idx) => base = base[..idx].trim_end(),
            None => break,
        }
    }
    base.split('&')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        // `@微软雅黑` 是竖排字形，不是给人选的
        .filter(|s| !s.starts_with('@'))
        .map(|s| s.to_string())
        .collect()
}

#[cfg(windows)]
fn read_registry() -> Vec<String> {
    #[link(name = "advapi32")]
    extern "system" {
        fn RegOpenKeyExW(
            hkey: isize,
            sub_key: *const u16,
            options: u32,
            sam: u32,
            result: *mut isize,
        ) -> i32;
        fn RegEnumValueW(
            hkey: isize,
            index: u32,
            value_name: *mut u16,
            name_len: *mut u32,
            reserved: *mut u32,
            kind: *mut u32,
            data: *mut u8,
            data_len: *mut u32,
        ) -> i32;
        fn RegCloseKey(hkey: isize) -> i32;
    }

    const HKEY_CURRENT_USER: isize = 0x8000_0001_u32 as i32 as isize;
    const HKEY_LOCAL_MACHINE: isize = 0x8000_0002_u32 as i32 as isize;
    const KEY_READ: u32 = 0x2_0019;
    const ERROR_SUCCESS: i32 = 0;
    // 缓冲区给的数据区太小（值是字体文件名）时就是这个码。
    // **名字已经取到了**，不能当成失败——以前就是这里把整个字体表丢掉的。
    const ERROR_MORE_DATA: i32 = 234;
    const ERROR_NO_MORE_ITEMS: i32 = 259;
    const SUB_KEY: &str = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Fonts";

    let wide: Vec<u16> = SUB_KEY.encode_utf16().chain(std::iter::once(0)).collect();

    let mut out: Vec<String> = Vec::new();
    for root in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        let mut key: isize = 0;
        // SAFETY: 句柄与缓冲区都由本函数拥有；只在 RegOpenKeyExW 成功后使用 key。
        // 数据区传 null（只要名字）：这样不会因为「值有多大」拿到 ERROR_MORE_DATA。
        unsafe {
            if RegOpenKeyExW(root, wide.as_ptr(), 0, KEY_READ, &mut key) != ERROR_SUCCESS {
                continue;
            }
            let mut index: u32 = 0;
            loop {
                let mut name = [0u16; 512];
                let mut name_len: u32 = name.len() as u32;
                let mut kind: u32 = 0;
                let rc = RegEnumValueW(
                    key,
                    index,
                    name.as_mut_ptr(),
                    &mut name_len,
                    std::ptr::null_mut(),
                    &mut kind,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
                if rc == ERROR_NO_MORE_ITEMS {
                    break;
                }
                if (rc == ERROR_SUCCESS || rc == ERROR_MORE_DATA)
                    && (name_len as usize) <= name.len()
                {
                    let raw = String::from_utf16_lossy(&name[..name_len as usize]);
                    out.extend(parse_value_name(&raw));
                }
                index += 1;
            }
            RegCloseKey(key);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_splits_family_aliases_and_drops_suffix() {
        assert_eq!(
            parse_value_name("微软雅黑 & Microsoft YaHei & Microsoft YaHei UI (TrueType)"),
            vec![
                "微软雅黑".to_string(),
                "Microsoft YaHei".to_string(),
                "Microsoft YaHei UI".to_string()
            ],
            "应按 & 拆开并去掉 (TrueType)"
        );
        assert_eq!(
            parse_value_name("Source Han Sans SC (OpenType)"),
            vec!["Source Han Sans SC".to_string()],
            "单个字体名应原样保留"
        );
        assert_eq!(
            parse_value_name("@Microsoft YaHei (TrueType)"),
            Vec::<String>::new(),
            "竖排字形（@ 开头）应被丢掉"
        );
        assert_eq!(
            parse_value_name("  KaiTi  "),
            vec!["KaiTi".to_string()],
            "应 trim"
        );
    }

    #[test]
    fn list_is_never_empty_and_sorted() {
        let fonts = list();
        assert!(!fonts.is_empty(), "字体列表不该为空（至少有兜底）");
        assert!(fonts.len() >= 3, "兜底也应有几项，实际 {} 项", fonts.len());
        let mut sorted = fonts.clone();
        sorted.sort_by_key(|s| s.to_lowercase());
        assert_eq!(fonts, sorted, "列表应按字母排序（前端直接铺开）");
        let unique: std::collections::HashSet<String> =
            fonts.iter().map(|f| f.to_lowercase()).collect();
        assert_eq!(unique.len(), fonts.len(), "不该有重复字体名");
    }

    /// 这条是为了拦住「静默回落」：注册表读失败时列表只有兜底那几项，上面两条测试照样绿。
    /// 垫底的 Windows 也装了上百个字体，所以真读到了必然远超 20。
    #[test]
    fn reads_real_fonts_from_registry() {
        let fonts = list();
        assert!(
            fonts.len() > 20,
            "只读到 {} 项字体，大概率是注册表读取失败后回落到了兜底列表：{:?}",
            fonts.len(),
            fonts
        );
    }
}
