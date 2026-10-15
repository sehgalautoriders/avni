//! Small helpers shared by the engine: time conversion, path joins, file-name stamps.

use std::time::{SystemTime, UNIX_EPOCH};

/// Windows FILETIME (100 ns ticks since 1601-01-01) -> unix ms. 0 stays 0 (= unknown).
pub fn filetime_to_unix_ms(ft: u64) -> i64 {
    const EPOCH_DIFF: u64 = 116_444_736_000_000_000;
    if ft == 0 {
        0
    } else {
        (ft as i128 - EPOCH_DIFF as i128).div_euclid(10_000) as i64
    }
}

pub fn system_time_ms(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(e) => -(e.duration().as_millis() as i64),
    }
}

pub fn now_ms() -> i64 {
    system_time_ms(SystemTime::now())
}

/// "dd-mm-yyyy HHmm" in local time, for file names.
pub fn stamp_now() -> String {
    chrono::Local::now().format("%d-%m-%Y %H%M").to_string()
}

/// "dd-mm-yyyy HHmmss" in local time, for log file names.
pub fn stamp_now_secs() -> String {
    chrono::Local::now().format("%d-%m-%Y %H%M%S").to_string()
}

/// "dd-mm-yyyy HH:MM:SS" local, for log lines.
pub fn local_time_text(ms: i64) -> String {
    use chrono::TimeZone;
    if ms == 0 {
        return String::new();
    }
    match chrono::Local.timestamp_millis_opt(ms) {
        chrono::LocalResult::Single(t) | chrono::LocalResult::Ambiguous(t, _) => t.format("%d-%m-%Y %H:%M:%S").to_string(),
        chrono::LocalResult::None => String::new(),
    }
}

pub const SEP: char = std::path::MAIN_SEPARATOR;

/// Joins a parent path and a child name with the platform separator, without doubling it after a root like "C:\".
pub fn join(parent: &str, name: &str) -> String {
    let mut s = String::with_capacity(parent.len() + 1 + name.len());
    s.push_str(parent);
    if !parent.ends_with(['\\', '/']) {
        s.push(SEP);
    }
    s.push_str(name);
    s
}

/// Lowercased extension without the dot; "" for none, for dot-files like ".gitignore", and for folders.
pub fn ext_of(name: &str) -> String {
    match name.rfind('.') {
        Some(0) | None => String::new(),
        Some(i) => {
            let e = &name[i + 1..];
            if e.is_empty() || e.len() > 16 || e.contains(' ') {
                String::new()
            } else {
                e.to_ascii_lowercase()
            }
        }
    }
}

/// Path prefix test that respects component boundaries and ignores case and separator style.
/// `starts_with_path("C:\\Users\\a\\b", "c:/users/a")` is true; `"C:\\Users\\ab"` vs `"C:\\Users\\a"` is false.
pub fn starts_with_path(path: &str, prefix: &str) -> bool {
    let norm = |s: &str| s.replace('/', "\\").trim_end_matches('\\').to_lowercase();
    let p = norm(path);
    let q = norm(prefix);
    if q.is_empty() {
        return false;
    }
    p == q || (p.starts_with(&q) && p.as_bytes().get(q.len()) == Some(&b'\\'))
}

/// Windows long-path form for opening files ("\\?\C:\..."). Leaves other paths alone.
pub fn long_path(path: &str) -> String {
    if cfg!(windows) && path.len() >= 2 && path.as_bytes()[1] == b':' && !path.starts_with("\\\\?\\") {
        format!("\\\\?\\{}", path.replace('/', "\\"))
    } else {
        path.to_string()
    }
}

pub fn human_bytes(b: u64) -> String {
    const U: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

pub fn hex(bytes: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(H[(b >> 4) as usize] as char);
        s.push(H[(b & 15) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime() {
        // 01-01-2020 00:00:00 UTC
        assert_eq!(filetime_to_unix_ms(132_223_104_000_000_000), 1_577_836_800_000);
        assert_eq!(filetime_to_unix_ms(0), 0);
    }

    #[test]
    fn prefixes() {
        assert!(starts_with_path("C:\\Users\\a\\b.txt", "c:/users/a"));
        assert!(starts_with_path("C:\\Users\\a", "C:\\Users\\a\\"));
        assert!(!starts_with_path("C:\\Users\\ab", "C:\\Users\\a"));
        assert!(!starts_with_path("C:\\x", ""));
    }

    #[test]
    fn exts() {
        assert_eq!(ext_of("a.TXT"), "txt");
        assert_eq!(ext_of(".gitignore"), "");
        assert_eq!(ext_of("noext"), "");
        assert_eq!(ext_of("x.tar.gz"), "gz");
        assert_eq!(ext_of("Report v1. final"), "");
    }
}
