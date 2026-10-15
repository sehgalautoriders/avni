//! Lists the drives on the machine with what the scanner needs to know about each one.

use crate::model::{DriveInfo, DriveKind, DriveRole, Media};

/// Role in Ravi's pattern: the Windows drive is the laptop, D:/E: (and any USB drive) are externals that should stay
/// empty, the Google Drive for Desktop volume is the storage.
pub fn role_for(id: &str, label: &str, kind: DriveKind, usb: bool) -> DriveRole {
    let l = label.to_lowercase();
    if l.contains("google drive") || l == "googledrivefs" {
        return DriveRole::GoogleDrive;
    }
    let letter = id.trim_end_matches(['\\', '/']).trim_end_matches(':').to_ascii_uppercase();
    if letter.eq_ignore_ascii_case(crate::platform::system_drive().trim_end_matches(':')) {
        return DriveRole::System;
    }
    if kind == DriveKind::Removable || usb || letter == "D" || letter == "E" {
        return DriveRole::External;
    }
    DriveRole::Other
}

#[cfg(windows)]
pub fn list_drives() -> Vec<DriveInfo> {
    use crate::platform::wide;
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW};
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let id = format!("{letter}:");
        let root = format!("{letter}:\\");
        let w = wide(&root);
        let kind = match unsafe { GetDriveTypeW(w.as_ptr()) } {
            2 => DriveKind::Removable,
            3 => DriveKind::Fixed,
            4 => DriveKind::Network,
            5 => DriveKind::Optical,
            6 => DriveKind::Ram,
            _ => DriveKind::Unknown,
        };
        let mut label = [0u16; 261];
        let mut fsname = [0u16; 261];
        let (mut serial, mut maxc, mut flags) = (0u32, 0u32, 0u32);
        let ok = unsafe {
            GetVolumeInformationW(w.as_ptr(), label.as_mut_ptr(), 261, &mut serial, &mut maxc, &mut flags, fsname.as_mut_ptr(), 261)
        };
        if ok == 0 {
            continue; // not ready (empty card reader / optical drive)
        }
        let s = |b: &[u16]| String::from_utf16_lossy(&b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())]);
        let label = s(&label);
        let fs = s(&fsname);
        let (free, total) = crate::platform::disk_space(&root).unwrap_or((0, 0));
        let probe = probe_device(&id);
        let mft_capable = fs.eq_ignore_ascii_case("NTFS") && crate::platform::open_volume(&id).is_ok();
        let role = role_for(&id, &label, kind, probe.usb);
        out.push(DriveInfo {
            id,
            root,
            label,
            fs,
            kind,
            media: probe.media,
            total_bytes: total,
            free_bytes: free,
            role,
            mft_capable: mft_capable && role != DriveRole::GoogleDrive,
            disk: probe.disk,
        });
    }
    out
}

#[cfg(windows)]
struct Probe {
    media: Media,
    usb: bool,
    disk: Option<u32>,
}

/// Asks the storage driver: seek penalty (HDD vs SSD), bus type (USB), physical disk number. Needs no admin rights.
#[cfg(windows)]
fn probe_device(id: &str) -> Probe {
    use crate::platform::wide;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows_sys::Win32::System::Ioctl::{
        PropertyStandardQuery, StorageDeviceProperty, StorageDeviceSeekPenaltyProperty, DEVICE_SEEK_PENALTY_DESCRIPTOR,
        IOCTL_STORAGE_GET_DEVICE_NUMBER, IOCTL_STORAGE_QUERY_PROPERTY, STORAGE_DEVICE_DESCRIPTOR, STORAGE_DEVICE_NUMBER,
        STORAGE_PROPERTY_QUERY,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    let mut p = Probe { media: Media::Unknown, usb: false, disk: None };
    let path = wide(&format!("\\\\.\\{}", id.trim_end_matches('\\')));
    let h = unsafe { CreateFileW(path.as_ptr(), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut()) };
    if h == INVALID_HANDLE_VALUE {
        return p;
    }
    unsafe {
        let mut ret = 0u32;
        let mut q = STORAGE_PROPERTY_QUERY { PropertyId: StorageDeviceSeekPenaltyProperty, QueryType: PropertyStandardQuery, AdditionalParameters: [0] };
        let mut sp: DEVICE_SEEK_PENALTY_DESCRIPTOR = std::mem::zeroed();
        if DeviceIoControl(
            h,
            IOCTL_STORAGE_QUERY_PROPERTY,
            &q as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            &mut sp as *mut _ as *mut core::ffi::c_void,
            std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32,
            &mut ret,
            std::ptr::null_mut(),
        ) != 0
        {
            p.media = if sp.IncursSeekPenalty { Media::Hdd } else { Media::Ssd };
        }
        q.PropertyId = StorageDeviceProperty;
        let mut buf = [0u8; 1024];
        if DeviceIoControl(
            h,
            IOCTL_STORAGE_QUERY_PROPERTY,
            &q as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            buf.as_mut_ptr() as *mut core::ffi::c_void,
            buf.len() as u32,
            &mut ret,
            std::ptr::null_mut(),
        ) != 0
            && ret as usize >= std::mem::size_of::<STORAGE_DEVICE_DESCRIPTOR>()
        {
            let d = &*(buf.as_ptr() as *const STORAGE_DEVICE_DESCRIPTOR);
            p.usb = d.BusType == 7; // BusTypeUsb
        }
        let mut num: STORAGE_DEVICE_NUMBER = std::mem::zeroed();
        if DeviceIoControl(
            h,
            IOCTL_STORAGE_GET_DEVICE_NUMBER,
            std::ptr::null(),
            0,
            &mut num as *mut _ as *mut core::ffi::c_void,
            std::mem::size_of::<STORAGE_DEVICE_NUMBER>() as u32,
            &mut ret,
            std::ptr::null_mut(),
        ) != 0
        {
            p.disk = Some(num.DeviceNumber);
        }
        CloseHandle(h);
    }
    p
}

/// Non-Windows (development/tests): real block-device mounts from /proc/mounts.
#[cfg(not(windows))]
pub fn list_drives() -> Vec<DriveInfo> {
    let text = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
    let mut out = Vec::new();
    for l in text.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 3 {
            continue;
        }
        let (dev, mnt, fs) = (f[0], f[1].replace("\\040", " "), f[2]);
        if !dev.starts_with('/') || !["ext4", "xfs", "btrfs", "fuse", "fuseblk", "ntfs", "ntfs3", "vfat", "exfat"].contains(&fs) {
            continue;
        }
        // ntfs-3g mounts show up as fuse/fuseblk; check the boot sector to be sure it is NTFS.
        let is_ntfs = matches!(fs, "ntfs" | "ntfs3")
            || (fs.starts_with("fuse") && {
                let mut b = [0u8; 11];
                std::fs::File::open(dev).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut b)).is_ok() && &b[3..11] == b"NTFS    "
            });
        if fs.starts_with("fuse") && !is_ntfs {
            continue;
        }
        let mut d = drive_for_path(&mnt, if is_ntfs { "NTFS" } else { fs });
        d.mft_capable = is_ntfs && crate::platform::open_volume(&mnt).is_ok();
        if mnt != "/" {
            d.role = DriveRole::External;
        }
        out.push(d);
    }
    out
}

/// A DriveInfo for any folder (tests, CLI). Role is External unless the path looks like the system root.
pub fn drive_for_path(path: &str, fs: &str) -> DriveInfo {
    let (free, total) = crate::platform::disk_space(path).unwrap_or((0, 0));
    DriveInfo {
        id: path.to_string(),
        root: path.to_string(),
        label: std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        fs: fs.to_string(),
        kind: DriveKind::Fixed,
        media: Media::Unknown,
        total_bytes: total,
        free_bytes: free,
        role: if path == "/" { DriveRole::System } else { DriveRole::Other },
        mft_capable: false,
        disk: None,
    }
}
