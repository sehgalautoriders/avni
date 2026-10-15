//! Parallel folder walk: the fallback when the MFT cannot be read (exFAT / FAT32 drives, no admin rights, network
//! drives, Google Drive). On Windows it uses FindFirstFileExW with FindExInfoBasic (skips 8.3 short names) and
//! FIND_FIRST_EX_LARGE_FETCH (bigger directory buffers); every folder is one job on a work-stealing pool.
//! It never opens a file: everything comes from the folder listing, so Google Drive files are never downloaded.

use crate::index::RawEntry;
use crate::util::join;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

pub struct DirItem {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub created: i64,
    pub modified: i64,
    pub accessed: i64,
    pub attrs: u32,
    pub links: u16,
    pub frn: u64,
    /// Do not descend (symlink / junction / mount point).
    pub reparse: bool,
}

pub struct WalkResult {
    pub entries: Vec<RawEntry>,
    pub root_key: u64,
    pub errors: Vec<String>,
    pub error_count: u64,
    /// Folders that could not be listed (no permission, in use). Their contents are missing from `entries`.
    pub failed_dirs: Vec<String>,
}

struct Ctx<'a> {
    next: AtomicU64,
    files: AtomicU64,
    dirs: AtomicU64,
    bytes: AtomicU64,
    out: Mutex<Vec<RawEntry>>,
    errors: Mutex<Vec<String>>,
    error_count: AtomicU64,
    failed_dirs: Mutex<Vec<String>>,
    seen_inodes: Mutex<std::collections::HashSet<u64>>,
    cancel: &'a AtomicBool,
    progress: &'a (dyn Fn(u64, u64, u64) + Sync),
    last_report: AtomicU64,
}

/// Walks `root` with `threads` workers. `progress(files, dirs, bytes)` is called every ~2000 entries.
pub fn walk(root: &str, threads: usize, progress: &(dyn Fn(u64, u64, u64) + Sync), cancel: &AtomicBool) -> Result<WalkResult, String> {
    let ctx = Ctx {
        next: AtomicU64::new(1),
        files: AtomicU64::new(0),
        dirs: AtomicU64::new(0),
        bytes: AtomicU64::new(0),
        out: Mutex::new(Vec::new()),
        errors: Mutex::new(Vec::new()),
        error_count: AtomicU64::new(0),
        failed_dirs: Mutex::new(Vec::new()),
        seen_inodes: Mutex::new(Default::default()),
        cancel,
        progress,
        last_report: AtomicU64::new(0),
    };
    list_dir(root).map_err(|e| format!("cannot open {root}: {e}"))?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads.max(1)).build().map_err(|e| e.to_string())?;
    pool.scope(|s| visit(s, root.to_string(), 0, &ctx));
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    progress(ctx.files.load(Ordering::Relaxed), ctx.dirs.load(Ordering::Relaxed), ctx.bytes.load(Ordering::Relaxed));
    Ok(WalkResult {
        entries: ctx.out.into_inner().unwrap(),
        root_key: 0,
        errors: ctx.errors.into_inner().unwrap(),
        error_count: ctx.error_count.load(Ordering::Relaxed),
        failed_dirs: ctx.failed_dirs.into_inner().unwrap(),
    })
}

fn visit<'s>(s: &rayon::Scope<'s>, path: String, key: u64, ctx: &'s Ctx<'s>) {
    if ctx.cancel.load(Ordering::Relaxed) {
        return;
    }
    let items = match list_dir(&path) {
        Ok(v) => v,
        Err(e) => {
            ctx.error_count.fetch_add(1, Ordering::Relaxed);
            ctx.failed_dirs.lock().unwrap().push(path.clone());
            let mut errs = ctx.errors.lock().unwrap();
            if errs.len() < 200 {
                errs.push(format!("{path}: {e}"));
            }
            return;
        }
    };
    let mut batch = Vec::with_capacity(items.len());
    let (mut nf, mut nd, mut nb) = (0u64, 0u64, 0u64);
    for it in items {
        let k = ctx.next.fetch_add(1, Ordering::Relaxed);
        let primary = if !it.is_dir && it.links > 1 && it.frn != 0 { ctx.seen_inodes.lock().unwrap().insert(it.frn) } else { true };
        if it.is_dir {
            nd += 1;
            if !it.reparse {
                let p = join(&path, &it.name);
                s.spawn(move |s| visit(s, p, k, ctx));
            }
        } else {
            nf += 1;
            if primary {
                nb += it.size;
            }
        }
        batch.push(RawEntry {
            key: k,
            parent: key,
            name: it.name,
            is_dir: it.is_dir,
            size: if it.is_dir { 0 } else { it.size },
            created: it.created,
            modified: it.modified,
            accessed: it.accessed,
            attrs: it.attrs,
            links: it.links,
            lcn: 0,
            frn: it.frn,
            primary,
        });
    }
    ctx.out.lock().unwrap().extend(batch);
    let f = ctx.files.fetch_add(nf, Ordering::Relaxed) + nf;
    let d = ctx.dirs.fetch_add(nd, Ordering::Relaxed) + nd;
    let b = ctx.bytes.fetch_add(nb, Ordering::Relaxed) + nb;
    let total = f + d;
    let last = ctx.last_report.load(Ordering::Relaxed);
    if total >= last + 2000 && ctx.last_report.compare_exchange(last, total, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
        (ctx.progress)(f, d, b);
    }
}

#[cfg(windows)]
pub fn list_dir(path: &str) -> std::io::Result<Vec<DirItem>> {
    use crate::util::{filetime_to_unix_ms, long_path};
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW, FIND_FIRST_EX_LARGE_FETCH,
        WIN32_FIND_DATAW,
    };
    let ft = |f: windows_sys::Win32::Foundation::FILETIME| filetime_to_unix_ms(((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64);
    let lp = long_path(path);
    let pattern: Vec<u16> = format!("{}\\*", lp.trim_end_matches('\\')).encode_utf16().chain(std::iter::once(0)).collect();
    let mut out = Vec::new();
    unsafe {
        let mut data: WIN32_FIND_DATAW = std::mem::zeroed();
        let h = FindFirstFileExW(
            pattern.as_ptr(),
            FindExInfoBasic,
            &mut data as *mut _ as *mut core::ffi::c_void,
            FindExSearchNameMatch,
            std::ptr::null(),
            FIND_FIRST_EX_LARGE_FETCH,
        );
        if h == INVALID_HANDLE_VALUE {
            let err = GetLastError();
            if err == ERROR_FILE_NOT_FOUND {
                return Ok(out);
            }
            return Err(std::io::Error::from_raw_os_error(err as i32));
        }
        loop {
            let len = data.cFileName.iter().position(|&c| c == 0).unwrap_or(data.cFileName.len());
            let name = String::from_utf16_lossy(&data.cFileName[..len]);
            if name != "." && name != ".." {
                let attrs = data.dwFileAttributes;
                let is_dir = attrs & 0x10 != 0;
                out.push(DirItem {
                    name,
                    is_dir,
                    size: ((data.nFileSizeHigh as u64) << 32) | data.nFileSizeLow as u64,
                    created: ft(data.ftCreationTime),
                    modified: ft(data.ftLastWriteTime),
                    accessed: ft(data.ftLastAccessTime),
                    attrs,
                    links: 1,
                    frn: 0,
                    reparse: attrs & 0x400 != 0,
                });
            }
            if FindNextFileW(h, &mut data) == 0 {
                let err = GetLastError();
                FindClose(h);
                if err != ERROR_NO_MORE_FILES {
                    return Err(std::io::Error::from_raw_os_error(err as i32));
                }
                break;
            }
        }
    }
    Ok(out)
}

#[cfg(not(windows))]
pub fn list_dir(path: &str) -> std::io::Result<Vec<DirItem>> {
    use crate::util::system_time_ms;
    use std::os::unix::fs::MetadataExt;
    let mut out = Vec::new();
    for e in std::fs::read_dir(path)? {
        let e = e?;
        let md = match e.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let ft = md.file_type();
        let is_link = ft.is_symlink();
        let is_dir = ft.is_dir();
        let mut attrs = if is_dir { 0x10 } else { 0x20 };
        if is_link {
            attrs |= 0x400;
        }
        out.push(DirItem {
            name: e.file_name().to_string_lossy().into_owned(),
            is_dir: is_dir || (is_link && std::fs::metadata(e.path()).map(|m| m.is_dir()).unwrap_or(false)),
            size: if is_dir { 0 } else { md.len() },
            created: md.created().map(system_time_ms).unwrap_or(0),
            modified: md.modified().map(system_time_ms).unwrap_or(0),
            accessed: md.accessed().map(system_time_ms).unwrap_or(0),
            attrs,
            links: md.nlink().min(u16::MAX as u64) as u16,
            frn: md.ino(),
            reparse: is_link,
        });
    }
    Ok(out)
}
