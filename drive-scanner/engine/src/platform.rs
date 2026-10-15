//! Operating-system calls: admin check and relaunch, Explorer, Recycle Bin, free space, DPAPI, raw volume access.
//! Windows is the real target; the non-Windows versions exist so the engine's tests run on the build machine.

use std::path::PathBuf;

pub fn local_app_data() -> PathBuf {
    if let Ok(p) = std::env::var("LOCALAPPDATA") {
        return PathBuf::from(p);
    }
    if let Ok(h) = std::env::var("HOME") {
        return PathBuf::from(h).join(".local").join("share");
    }
    std::env::temp_dir()
}

/// %LOCALAPPDATA%\Avni\DriveScanner (settings, history).
pub fn app_dir() -> PathBuf {
    local_app_data().join("Avni").join("DriveScanner")
}

/// %LOCALAPPDATA%\Avni\reports\DriveScanner (Excel/CSV and action logs).
pub fn default_reports_dir() -> PathBuf {
    local_app_data().join("Avni").join("reports").join("DriveScanner")
}

pub fn machine_name() -> String {
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| {
        std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string()).unwrap_or_else(|_| "unknown".into())
    })
}

pub fn system_drive() -> String {
    std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into())
}

#[cfg(windows)]
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
pub fn is_elevated() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut e = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut ret = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut e as *mut _ as *mut core::ffi::c_void,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret,
        );
        CloseHandle(token);
        ok != 0 && e.TokenIsElevated != 0
    }
}

#[cfg(not(windows))]
pub fn is_elevated() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("Uid:")).map(|l| l.split_whitespace().nth(1) == Some("0")))
        .unwrap_or(false)
}

#[cfg(windows)]
fn shell_execute(verb: &str, file: &str, params: &str) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let (v, f, p) = (wide(verb), wide(file), wide(params));
    let r = unsafe {
        ShellExecuteW(std::ptr::null_mut(), v.as_ptr(), f.as_ptr(), if params.is_empty() { std::ptr::null() } else { p.as_ptr() }, std::ptr::null(), SW_SHOWNORMAL)
    };
    if (r as isize) > 32 {
        Ok(())
    } else {
        Err(format!("Windows could not {verb} {file} (code {})", r as isize))
    }
}

/// Starts a second copy of this program as administrator (Windows shows the UAC prompt). Caller exits on Ok.
#[cfg(windows)]
pub fn restart_elevated() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    shell_execute("runas", &exe.to_string_lossy(), "")
}
#[cfg(not(windows))]
pub fn restart_elevated() -> Result<(), String> {
    Err("Restart as administrator is only available on Windows".into())
}

#[cfg(windows)]
pub fn open_path(path: &str) -> Result<(), String> {
    shell_execute("open", path, "")
}
#[cfg(not(windows))]
pub fn open_path(path: &str) -> Result<(), String> {
    std::process::Command::new("xdg-open").arg(path).spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(windows)]
pub fn reveal_path(path: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{}\"", path))
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}
#[cfg(not(windows))]
pub fn reveal_path(path: &str) -> Result<(), String> {
    let p = std::path::Path::new(path);
    open_path(&p.parent().unwrap_or(p).to_string_lossy())
}

/// (free bytes, total bytes) of the volume holding `path`.
#[cfg(windows)]
pub fn disk_space(path: &str) -> Option<(u64, u64)> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let w = wide(path);
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    let ok = unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut avail, &mut total, &mut free) };
    (ok != 0).then_some((avail, total))
}
#[cfg(not(windows))]
pub fn disk_space(path: &str) -> Option<(u64, u64)> {
    // `df -kP` is everywhere on the build machine; precise enough for tests.
    let out = std::process::Command::new("df").arg("-kP").arg(path).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let l = s.lines().nth(1)?;
    let f: Vec<&str> = l.split_whitespace().collect();
    Some((f.get(3)?.parse::<u64>().ok()? * 1024, f.get(1)?.parse::<u64>().ok()? * 1024))
}

/// Sends files to the Recycle Bin, one call per file so every failure is known. Ok(()) per file on success.
#[cfg(windows)]
pub fn recycle(paths: &[String]) -> Vec<Result<(), String>> {
    use windows_sys::Win32::UI::Shell::{
        SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING, FO_DELETE,
        SHFILEOPSTRUCTW,
    };
    paths
        .iter()
        .map(|p| {
            let mut from: Vec<u16> = p.replace('/', "\\").encode_utf16().collect();
            from.push(0);
            from.push(0);
            let mut op = SHFILEOPSTRUCTW {
                hwnd: std::ptr::null_mut(),
                wFunc: FO_DELETE,
                pFrom: from.as_ptr(),
                pTo: std::ptr::null(),
                // NUKEWARNING: if a file is too big for the Recycle Bin, Windows asks instead of deleting it for good.
                fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI | FOF_WANTNUKEWARNING) as _,
                fAnyOperationsAborted: 0,
                hNameMappings: std::ptr::null_mut(),
                lpszProgressTitle: std::ptr::null(),
            };
            let r = unsafe { SHFileOperationW(&mut op) };
            if r != 0 {
                Err(format!("Recycle Bin refused (code {r:#x})"))
            } else if op.fAnyOperationsAborted != 0 {
                Err("cancelled in the Windows dialog".into())
            } else if std::path::Path::new(p).exists() {
                Err("still there after recycling".into())
            } else {
                Ok(())
            }
        })
        .collect()
}

/// Test stand-in: moves files into <temp>/avni-recycle.
#[cfg(not(windows))]
pub fn recycle(paths: &[String]) -> Vec<Result<(), String>> {
    let bin = std::env::temp_dir().join("avni-recycle").join(crate::util::stamp_now_secs());
    paths
        .iter()
        .enumerate()
        .map(|(i, p)| {
            std::fs::create_dir_all(&bin).map_err(|e| e.to_string())?;
            let name = std::path::Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let dest = bin.join(format!("{i}-{name}"));
            std::fs::rename(p, &dest).or_else(|_| std::fs::copy(p, &dest).and_then(|_| std::fs::remove_file(p))).map_err(|e| e.to_string())
        })
        .collect()
}

/// (items, bytes) in the Recycle Bin of the drive `root` ("C:\").
#[cfg(windows)]
pub fn recycle_bin_size(root: &str) -> Option<(u64, u64)> {
    use windows_sys::Win32::UI::Shell::{SHQueryRecycleBinW, SHQUERYRBINFO};
    let w = wide(root);
    let mut info: SHQUERYRBINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHQUERYRBINFO>() as u32;
    let hr = unsafe { SHQueryRecycleBinW(w.as_ptr(), &mut info) };
    (hr >= 0).then(|| (info.i64NumItems.max(0) as u64, info.i64Size.max(0) as u64))
}
#[cfg(not(windows))]
pub fn recycle_bin_size(_root: &str) -> Option<(u64, u64)> {
    None
}

#[cfg(windows)]
pub fn empty_recycle_bin(root: &str) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::{SHEmptyRecycleBinW, SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND};
    let w = wide(root);
    let hr = unsafe { SHEmptyRecycleBinW(std::ptr::null_mut(), w.as_ptr(), SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND) };
    // 0x8000FFFF (E_UNEXPECTED) is what Windows returns when the bin is already empty.
    if hr >= 0 || hr as u32 == 0x8000_FFFF {
        Ok(())
    } else {
        Err(format!("Windows could not empty the Recycle Bin on {root} (code {:#x})", hr as u32))
    }
}
#[cfg(not(windows))]
pub fn empty_recycle_bin(_root: &str) -> Result<(), String> {
    Err("Empty Recycle Bin is only available on Windows".into())
}

/// Encrypts with Windows DPAPI for the current user (secrets in settings never sit on disk in clear text).
#[cfg(windows)]
pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
    dpapi(data, true)
}
#[cfg(windows)]
pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
    dpapi(data, false)
}
#[cfg(windows)]
fn dpapi(data: &[u8], encrypt: bool) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::Foundation::{LocalFree, HLOCAL};
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB};
    let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    let ok = unsafe {
        if encrypt {
            CryptProtectData(&input, std::ptr::null(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        } else {
            CryptUnprotectData(&input, std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        }
    };
    if ok == 0 {
        return Err("Windows DPAPI failed".into());
    }
    let v = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
    unsafe { LocalFree(out.pbData as HLOCAL) };
    Ok(v)
}
/// Non-Windows builds (tests only) keep the bytes as they are.
#[cfg(not(windows))]
pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
    Ok(data.to_vec())
}
#[cfg(not(windows))]
pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
    Ok(data.to_vec())
}

/// Opens the raw volume ("C:" -> \\.\C:) for reading. Needs administrator rights.
#[cfg(windows)]
pub fn open_volume(drive_id: &str) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
    let letter = drive_id.trim_end_matches(['\\', '/']).trim_end_matches(':');
    std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE).open(format!("\\\\.\\{letter}:"))
}
/// Non-Windows: a mount point opens its backing device (or image file); anything else opens as given.
#[cfg(not(windows))]
pub fn open_volume(path: &str) -> std::io::Result<std::fs::File> {
    if std::path::Path::new(path).is_dir() {
        let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
        for l in mounts.lines() {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() > 1 && f[1].replace("\\040", " ") == path.trim_end_matches('/') {
                return std::fs::File::open(f[0]);
            }
        }
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no device behind this folder"));
    }
    std::fs::File::open(path)
}

/// First cluster of a file on disk (for reading files in disk order on an HDD when the MFT was not used).
#[cfg(windows)]
pub fn first_lcn(path: &str) -> Option<u64> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Ioctl::{FSCTL_GET_RETRIEVAL_POINTERS, RETRIEVAL_POINTERS_BUFFER, STARTING_VCN_INPUT_BUFFER};
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use std::os::windows::fs::OpenOptionsExt;
    let f = std::fs::OpenOptions::new().access_mode(0x80).share_mode(7).open(crate::util::long_path(path)).ok()?;
    let input = STARTING_VCN_INPUT_BUFFER { StartingVcn: 0 };
    let mut out: RETRIEVAL_POINTERS_BUFFER = unsafe { std::mem::zeroed() };
    let mut ret = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            f.as_raw_handle() as _,
            FSCTL_GET_RETRIEVAL_POINTERS,
            &input as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<STARTING_VCN_INPUT_BUFFER>() as u32,
            &mut out as *mut _ as *mut core::ffi::c_void,
            std::mem::size_of::<RETRIEVAL_POINTERS_BUFFER>() as u32,
            &mut ret,
            std::ptr::null_mut(),
        )
    };
    // ERROR_MORE_DATA still fills the first extent, which is all we need.
    if (ok != 0 || ret as usize >= std::mem::size_of::<RETRIEVAL_POINTERS_BUFFER>()) && out.ExtentCount > 0 && out.Extents[0].Lcn >= 0 {
        Some(out.Extents[0].Lcn as u64)
    } else {
        None
    }
}
#[cfg(not(windows))]
pub fn first_lcn(_path: &str) -> Option<u64> {
    None
}
