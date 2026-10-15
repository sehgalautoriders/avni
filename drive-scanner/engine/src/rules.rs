//! Evaluation: which files stay, which go to Google Drive, which are duplicates, junk, archives or processed output.
//!
//! Ravi's pattern: the laptop (C:) keeps only what Windows and installed apps need; D:/E: are externals that should
//! stay empty; Google Drive is the storage; Supabase holds processed output. Folders get a "zone" top-down (a folder
//! inherits its parent's zone unless its own name changes it); files take the verdict of their folder's zone with a
//! few file-level overrides. Protected paths always win.

use crate::index::{Index, F_PLACEHOLDER};
use crate::model::{DriveRole, Settings, Verdict};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Zone {
    SysRoot,
    System(&'static str),
    UsersRoot,
    Public,
    Profile,
    ProfileKeep(&'static str),
    AppData,
    AppDataLocal,
    AppDataSub,
    BrowserData,
    BrowserProfile,
    UserData,
    OneDrive,
    ProfileOther,
    Program,
    CData,
    ExternalRoot,
    External,
    GDrive,
    Junk(&'static str),
    Code,
    Processed,
    OldWindows,
    Other,
    Protected,
}

const SYSTEM_TOP: &[(&str, &str)] = &[
    ("windows", "Windows"),
    ("program files", "Installed programs"),
    ("program files (x86)", "Installed programs"),
    ("programdata", "Data for installed programs"),
    ("recovery", "Windows recovery"),
    ("system volume information", "Windows volume data"),
    ("$winreagent", "Windows update data"),
    ("$windows.~bt", "Windows update data"),
    ("$windows.~ws", "Windows update data"),
    ("$windows.~ls", "Windows update data"),
    ("$sysreset", "Windows reset data"),
    ("$getcurrent", "Windows update data"),
    ("boot", "Windows boot files"),
    ("efi", "Windows boot files"),
    ("perflogs", "Windows performance logs"),
    ("msocache", "Microsoft Office install cache"),
    ("config.msi", "Windows installer data"),
    ("documents and settings", "Windows link"),
    ("onedrivetemp", "OneDrive working folder"),
    ("inetpub", "Windows web server"),
    ("$extend", "NTFS metadata"),
];

const PROGRAM_TOP_PREFIXES: &[&str] = &[
    "python", "xampp", "msys64", "msys", "cygwin", "mingw", "strawberry", "tools", "flutter", "android", "sdk", "intel", "amd",
    "nvidia", "drivers", "dell", "hp", "lenovo", "oem", "swsetup", "vagrant", "docker", "ruby", "php", "nodejs", "go", "java",
    "jdk", "gradle", "maven", "chocolatey", "tdm-gcc", "llvm", "vcpkg", "wamp", "laragon", "esd", "dokan",
];

const USER_DATA: &[&str] = &[
    "desktop", "documents", "downloads", "pictures", "videos", "music", "favorites", "contacts", "links", "saved games",
    "searches", "3d objects", "my documents", "scans", "screenshots",
];

const PROFILE_LINKS: &[&str] = &[
    "application data", "cookies", "local settings", "nethood", "printhood", "recent", "sendto", "start menu", "templates",
];

const CACHE_DIRS: &[&str] = &[
    "cache", "code cache", "gpucache", "cache2", "shadercache", "grshadercache", "dawncache", "dawngraphitecache",
    "dawnwebgpucache", "cache_data",
];

const ROOT_SYSTEM_FILES: &[&str] = &[
    "pagefile.sys", "hiberfil.sys", "swapfile.sys", "dumpstack.log", "dumpstack.log.tmp", "bootmgr", "bootnxt",
    "bootsect.bak", "$winre_backup_partition.marker", "desktop.ini",
];

fn child_zone(parent: Zone, parent_name: &str, name: &str) -> Zone {
    use Zone::*;
    match parent {
        Protected | Junk(_) | Code | GDrive | OldWindows | Program => parent,
        SysRoot => {
            if let Some((_, r)) = SYSTEM_TOP.iter().find(|(n, _)| *n == name) {
                System(r)
            } else if name == "$recycle.bin" || name == "recycler" {
                Junk("Recycle Bin contents — Empty Recycle Bin frees the space")
            } else if name == "windows.old" {
                OldWindows
            } else if name == "users" {
                UsersRoot
            } else if PROGRAM_TOP_PREFIXES.iter().any(|p| name.starts_with(p)) {
                Program
            } else {
                CData // refined by the program-folder test in evaluate()
            }
        }
        System(r) => {
            if parent_name == "windows" && name == "temp" {
                Junk("Windows temp files")
            } else if parent_name == "windows" && name == "wer" {
                Junk("Windows error reports")
            } else {
                System(r)
            }
        }
        UsersRoot => match name {
            "public" => Public,
            "default" | "default user" | "all users" | "defaultapppool" | "wdagutilityaccount" => System("Windows user template"),
            _ => Profile,
        },
        Public => {
            if USER_DATA.iter().any(|n| name == *n || name == format!("public {n}")) {
                UserData
            } else {
                System("Windows shared folder")
            }
        }
        Profile => {
            if name == "appdata" {
                AppData
            } else if name.starts_with('.') {
                ProfileKeep("Tool and app data in your profile")
            } else if USER_DATA.contains(&name) {
                UserData
            } else if name.starts_with("onedrive") {
                OneDrive
            } else if PROFILE_LINKS.contains(&name) {
                System("Windows link")
            } else {
                ProfileOther
            }
        }
        ProfileKeep(r) => ProfileKeep(r),
        AppData => {
            if name == "local" {
                AppDataLocal
            } else {
                AppDataSub
            }
        }
        AppDataLocal => match name {
            "temp" => Junk("Temp files"),
            "crashdumps" => Junk("Crash dumps"),
            "npm-cache" => Junk("npm download cache — downloads again when needed"),
            _ => AppDataSub,
        },
        AppDataSub | BrowserData | BrowserProfile => {
            if name == "inetcache" {
                Junk("Internet cache")
            } else if parent_name == "windows" && name == "wer" {
                Junk("Windows error reports")
            } else if name == "cache" && matches!(parent_name, "pip" | "yarn" | "go-build" | "nuget") {
                Junk("Package download cache — downloads again when needed")
            } else if matches!(parent, BrowserData | BrowserProfile) && CACHE_DIRS.contains(&name) {
                Junk("Browser cache")
            } else if parent == BrowserData {
                BrowserProfile
            } else if name == "user data" || name == "profiles" {
                BrowserData
            } else {
                AppDataSub
            }
        }
        ExternalRoot => {
            if name == "system volume information" {
                System("Windows volume data")
            } else if name == "$recycle.bin" || name == "recycler" {
                Junk("Recycle Bin contents — Empty Recycle Bin frees the space")
            } else if name.starts_with("found.") {
                Other
            } else {
                External
            }
        }
        UserData | OneDrive | CData | External | ProfileOther | Other | Processed => parent,
    }
}

fn is_movable(z: Zone) -> bool {
    matches!(z, Zone::UserData | Zone::OneDrive | Zone::CData | Zone::External | Zone::ProfileOther | Zone::Other)
}

fn dir_verdict(z: Zone) -> (Verdict, &'static str) {
    use Zone::*;
    match z {
        Protected => (Verdict::Protected, "On the protected list — never touched"),
        GDrive => (Verdict::Keep, "In Google Drive — the storage"),
        System(r) | ProfileKeep(r) => (Verdict::Keep, r),
        AppData | AppDataLocal | AppDataSub | BrowserData | BrowserProfile => (Verdict::Keep, "App settings and data for installed apps"),
        Program => (Verdict::Keep, "Installed program folder"),
        Junk(r) => (Verdict::Junk, r),
        Code => (Verdict::Review, "Git working copy or node_modules — belongs in GitHub, not Google Drive"),
        OldWindows => (Verdict::Review, "Previous Windows install — remove it with Settings › System › Storage › Temporary files"),
        SysRoot | UsersRoot | Profile | Public => (Verdict::Keep, "Windows folder"),
        Other => (Verdict::Review, "Drive with no role — decide"),
        ProfileOther => (Verdict::Review, "Folder in your user profile — app data or your files? Decide"),
        Processed => (Verdict::Supabase, "Processed output — belongs in Supabase"),
        UserData => (Verdict::Transfer, "Your files on C: belong on Google Drive"),
        OneDrive => (Verdict::Transfer, "OneDrive folder on this disk — the storage is Google Drive"),
        CData => (Verdict::Transfer, "Folder on C: with your files — belongs on Google Drive"),
        ExternalRoot | External => (Verdict::Transfer, "External drive should stay empty — move to Google Drive"),
    }
}

fn is_junk_file(name: &str, ext: &str) -> bool {
    name == "thumbs.db" || name == "ehthumbs.db" || name.starts_with("~$") || ext == "tmp" || ext == "dmp"
}

fn norm(p: &str) -> String {
    p.replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// Sets verdict + reason on every node. Run after metadata; run `apply_duplicates` and `apply_in_drive` after hashing.
pub fn evaluate(index: &mut Index, settings: &Settings) {
    let protected: Vec<String> = settings.protected_paths.iter().map(|p| norm(p)).filter(|p| !p.is_empty()).collect();
    let protected_names: HashSet<String> = protected.iter().filter_map(|p| p.rsplit('\\').next().map(|s| s.to_string())).collect();
    let markers: HashSet<String> = settings.processed_markers.iter().map(|m| m.trim().to_lowercase()).filter(|m| !m.is_empty()).collect();

    // Folders that hold a .git folder are code checkouts; folders holding a .avniprotect file are never touched
    // (Avni Clean's marker — it sits on D:\AE Brain and the Manuals Project).
    let mut git_roots: HashSet<u32> = HashSet::new();
    let mut marker_dirs: HashSet<u32> = HashSet::new();
    for n in &index.nodes {
        if n.is_dir() && n.name.eq_ignore_ascii_case(".git") {
            git_roots.insert(n.parent);
        }
        if !n.is_dir() && n.name.eq_ignore_ascii_case(crate::treelaw::PROTECT_MARKER) {
            marker_dirs.insert(n.parent);
        }
    }

    let drives: Vec<(u32, u32, DriveRole)> = index.drives.iter().map(|d| (d.root, d.end, d.info.role)).collect();
    let mut reasons: HashMap<&'static str, u32> = HashMap::new();
    let mut rid = |index: &mut Index, r: &'static str| *reasons.entry(r).or_insert_with(|| index.reason_id(r));

    for (root, end, role) in drives {
        // Program-folder test for top-level folders on C: (installed apps put .exe/.dll there; data folders do not).
        let mut top_of: Vec<u32> = vec![u32::MAX; (end - root) as usize];
        let mut exe_bytes: HashMap<u32, (u64, u64)> = HashMap::new();
        if role == DriveRole::System {
            for id in root + 1..end {
                let n = &index.nodes[id as usize];
                let t = if n.parent == root { id } else { top_of[(n.parent - root) as usize] };
                top_of[(id - root) as usize] = t;
                if !n.is_dir() && t != u32::MAX {
                    let e = exe_bytes.entry(t).or_default();
                    let ext = crate::util::ext_of(&n.name);
                    if matches!(ext.as_str(), "exe" | "dll" | "sys" | "msi" | "ocx" | "pyd" | "jar") {
                        e.0 += n.size;
                    }
                    e.1 += n.size;
                }
            }
        }
        let root_zone = match role {
            DriveRole::System => Zone::SysRoot,
            DriveRole::External => Zone::ExternalRoot,
            DriveRole::GoogleDrive => Zone::GDrive,
            DriveRole::Other => Zone::Other,
        };
        let mut zones: HashMap<u32, Zone> = HashMap::new();
        zones.insert(root, root_zone);
        let mut prot_dirs: HashSet<u32> = HashSet::new();
        for id in root..end {
            let (is_dir, parent, name_lc) = {
                let n = &index.nodes[id as usize];
                (n.is_dir(), n.parent, n.name.to_lowercase())
            };
            let pz = if id == root { root_zone } else { zones.get(&parent).copied().unwrap_or(Zone::Other) };
            let parent_protected = id != root && prot_dirs.contains(&parent);
            if is_dir {
                let mut z = if id == root {
                    root_zone
                } else {
                    let pname = index.nodes[parent as usize].name.to_lowercase();
                    child_zone(pz, &pname, &name_lc)
                };
                if z == Zone::CData && role == DriveRole::System && index.nodes[parent as usize].parent == parent {
                    if let Some((exe, total)) = exe_bytes.get(&id) {
                        if *exe > 0 && *exe * 5 >= *total {
                            z = Zone::Program;
                        }
                    }
                }
                if is_movable(z) && (git_roots.contains(&id) || name_lc == "node_modules") {
                    z = Zone::Code;
                }
                if is_movable(z) && markers.contains(&name_lc) {
                    z = Zone::Processed;
                }
                let is_prot = parent_protected || marker_dirs.contains(&id) || {
                    let p = norm(index.dir_path(id));
                    protected.iter().any(|q| p == *q || (p.starts_with(q.as_str()) && p.as_bytes().get(q.len()) == Some(&b'\\')))
                };
                if is_prot {
                    z = Zone::Protected;
                    prot_dirs.insert(id);
                }
                zones.insert(id, z);
                let (v, r) = dir_verdict(z);
                let r = rid(index, r);
                let n = &mut index.nodes[id as usize];
                n.verdict = v;
                n.reason = r;
                continue;
            }
            let ext = crate::util::ext_of(&name_lc);
            let placeholder = index.nodes[id as usize].flags & F_PLACEHOLDER != 0;
            let file_protected = parent_protected
                || (protected_names.contains(&name_lc) && protected.iter().any(|q| norm(&index.path_of(id)) == *q));
            let (v, r): (Verdict, &'static str) = if file_protected {
                (Verdict::Protected, "On the protected list — never touched")
            } else if ext == "ahk" && pz != Zone::GDrive {
                (Verdict::Protected, "AutoHotkey script — part of the keyboard, never touched")
            } else {
                match pz {
                    Zone::SysRoot => {
                        if ROOT_SYSTEM_FILES.contains(&name_lc.as_str()) || name_lc.starts_with('$') {
                            (Verdict::Keep, "Windows system file")
                        } else {
                            (Verdict::Transfer, "Your file on C: — belongs on Google Drive")
                        }
                    }
                    Zone::UsersRoot | Zone::Profile => (Verdict::Keep, "Windows profile file"),
                    Zone::ExternalRoot if name_lc.starts_with('$') => (Verdict::Keep, "NTFS metadata"),
                    z if is_movable(z) || z == Zone::ExternalRoot || z == Zone::Processed => {
                        if placeholder {
                            (Verdict::Keep, "Cloud-only placeholder — not stored on this disk")
                        } else if is_junk_file(&name_lc, &ext) {
                            (Verdict::Junk, "Temp, lock or crash-dump file")
                        } else if z == Zone::Processed {
                            (Verdict::Supabase, "Processed output — belongs in Supabase")
                        } else if matches!(z, Zone::UserData | Zone::OneDrive | Zone::CData | Zone::External | Zone::ExternalRoot)
                            && crate::extract::archive_kind(&name_lc).is_some()
                        {
                            (Verdict::Extract, "Archive — extract it, dedup the contents, then transfer")
                        } else {
                            dir_verdict(if z == Zone::ExternalRoot { Zone::External } else { z })
                        }
                    }
                    z => dir_verdict(z),
                }
            };
            let r = rid(index, r);
            let n = &mut index.nodes[id as usize];
            n.verdict = v;
            n.reason = r;
        }
    }
    index.touch();
}

/// Copies THE TREE LAW lets go become Duplicate — unless the location rules say Windows/apps need them (Keep),
/// they are protected, or they are junk anyway. Both laws must agree before anything is marked for removal.
pub fn apply_duplicates(index: &mut Index) {
    let changes: Vec<(u32, u32)> = index
        .dup_role
        .iter()
        .filter(|(_, (role, _))| *role == crate::treelaw::Role::Remove)
        .map(|(&m, &(_, why))| (m, why))
        .filter(|(m, _)| matches!(index.nodes[*m as usize].verdict, Verdict::Transfer | Verdict::Review | Verdict::Extract | Verdict::Supabase | Verdict::InDrive))
        .collect();
    for (m, why) in changes {
        let n = &mut index.nodes[m as usize];
        n.verdict = Verdict::Duplicate;
        n.reason = why;
    }
    index.touch();
}

/// Local files that already exist on Google Drive (same name, same size) become InDrive.
pub fn apply_in_drive(index: &mut Index) {
    let mut on_drive: HashMap<(String, u64), u32> = HashMap::new();
    for d in index.drives.iter().filter(|d| d.info.role == DriveRole::GoogleDrive) {
        for id in d.root..d.end {
            let n = &index.nodes[id as usize];
            if !n.is_dir() && n.size > 0 {
                on_drive.entry((n.name.to_lowercase(), n.size)).or_insert(id);
            }
        }
    }
    index.in_drive.clear();
    if on_drive.is_empty() {
        return;
    }
    let r = index.reason_id("Same name and size on Google Drive");
    let mut hits = Vec::new();
    for (id, n) in index.nodes.iter().enumerate() {
        if !n.is_dir() && matches!(n.verdict, Verdict::Transfer | Verdict::Extract) {
            if let Some(&g) = on_drive.get(&(n.name.to_lowercase(), n.size)) {
                hits.push((id as u32, g));
            }
        }
    }
    for (id, g) in hits {
        index.in_drive.insert(id, g);
        let n = &mut index.nodes[id as usize];
        n.verdict = Verdict::InDrive;
        n.reason = r;
    }
    index.touch();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::RawEntry;
    use crate::model::*;

    fn drive(id: &str, role: DriveRole) -> DriveInfo {
        DriveInfo {
            id: id.into(),
            root: format!("{id}\\"),
            label: String::new(),
            fs: "NTFS".into(),
            kind: DriveKind::Fixed,
            media: Media::Ssd,
            total_bytes: 0,
            free_bytes: 0,
            role,
            mft_capable: false,
            disk: None,
        }
    }

    /// Builds entries from "a\b\c.ext:size" paths (folders created on the way).
    fn entries(files: &[(&str, u64)]) -> Vec<RawEntry> {
        let mut out: Vec<RawEntry> = Vec::new();
        let mut dirs: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        let mut next = 1u64;
        let mut mk = |name: &str, parent: u64, is_dir: bool, size: u64, out: &mut Vec<RawEntry>| {
            let k = next;
            next += 1;
            out.push(RawEntry { key: k, parent, name: name.into(), is_dir, size, created: 1, modified: 1, accessed: 1, attrs: if is_dir { 0x10 } else { 0x20 }, links: 1, lcn: 0, frn: k, primary: true });
            k
        };
        for (p, size) in files {
            let parts: Vec<&str> = p.split('\\').collect();
            let mut parent = 0u64;
            let mut acc = String::new();
            for (i, part) in parts.iter().enumerate() {
                acc.push('\\');
                acc.push_str(part);
                if i + 1 == parts.len() {
                    mk(part, parent, false, *size, &mut out);
                } else {
                    parent = match dirs.get(&acc) {
                        Some(&k) => k,
                        None => {
                            let k = mk(part, parent, true, 0, &mut out);
                            dirs.insert(acc.clone(), k);
                            k
                        }
                    };
                }
            }
        }
        out
    }

    fn verdict_of(idx: &Index, drive: &str, rel: &str) -> (Verdict, String) {
        let d = idx.drives.iter().find(|d| d.info.id == drive).unwrap();
        for id in d.root..d.end {
            let p = idx.path_of(id).replace('/', "\\");
            if p.eq_ignore_ascii_case(&format!("{drive}\\{rel}")) {
                let r = idx.row(id);
                return (r.verdict, r.reason);
            }
        }
        panic!("{drive}\\{rel} not in index");
    }

    #[test]
    fn ravis_pattern() {
        let c = [
            ("Windows\\System32\\kernel32.dll", 900_000),
            ("Windows\\Temp\\x.log", 10),
            ("pagefile.sys", 8 << 30),
            ("notes.txt", 50),
            ("$Recycle.Bin\\S-1-5-21\\$R123.pdf", 5000),
            ("Program Files\\App\\app.exe", 1000),
            ("Python313\\python.exe", 100_000),
            ("MyTool\\bin\\tool.exe", 900_000),
            ("MyTool\\readme.txt", 1000),
            ("02 - OUR FILES\\Access to all drives\\AVNI\\notes.md", 10),
            ("02 - OUR FILES\\Invoices\\inv.pdf", 10_000),
            ("Users\\sehga\\Desktop\\copilotremap.ahk", 300),
            ("Users\\sehga\\Desktop\\other.ahk", 300),
            ("Users\\sehga\\Documents\\report.docx", 20_000),
            ("Users\\sehga\\Downloads\\setup.zip", 20_000),
            ("Users\\sehga\\Documents\\processed\\out.csv", 999),
            ("Users\\sehga\\Documents\\repo\\.git\\HEAD", 20),
            ("Users\\sehga\\Documents\\repo\\src\\main.rs", 200),
            ("Users\\sehga\\AppData\\Local\\Temp\\a.tmp", 10),
            ("Users\\sehga\\AppData\\Local\\Google\\Chrome\\User Data\\Default\\Cache\\Cache_Data\\f_000001", 10),
            ("Users\\sehga\\AppData\\Local\\Google\\Chrome\\User Data\\Default\\History", 10),
            ("Users\\sehga\\AppData\\Roaming\\Code\\settings.json", 10),
            ("Users\\sehga\\.cargo\\bin\\cargo.exe", 10),
            ("Users\\sehga\\NTUSER.DAT", 10),
            ("Users\\sehga\\Documents\\Thumbs.db", 10),
            ("Users\\sehga\\source\\thing.txt", 10),
            ("Users\\Public\\Documents\\shared.pdf", 10),
        ];
        let d = [
            ("AE Brain\\src\\x.rs", 10),
            ("Movies\\film.mkv", 7_000_000),
            ("System Volume Information\\tracking.log", 10),
            ("$RECYCLE.BIN\\y.bin", 10),
            ("Backups\\old.7z", 10),
            ("Manuals Project\\.avniprotect", 0),
            ("Manuals Project\\Hero\\manual.pdf", 10),
        ];
        let g = [("My Drive\\Movies\\film.mkv", 7_000_000), ("My Drive\\x.ahk", 3)];
        let mut idx = Index::new(ScanProfile::Quick);
        idx.add_drive(drive("C:", DriveRole::System), "test", entries(&c), 0, 0, None);
        idx.add_drive(drive("D:", DriveRole::External), "test", entries(&d), 0, 0, None);
        idx.add_drive(drive("G:", DriveRole::GoogleDrive), "test", entries(&g), 0, 0, None);
        let settings = crate::settings::defaults();
        evaluate(&mut idx, &settings);
        apply_in_drive(&mut idx);
        use Verdict::*;
        let want = [
            ("C:", "Windows\\System32\\kernel32.dll", Keep),
            ("C:", "Windows\\Temp\\x.log", Junk),
            ("C:", "pagefile.sys", Keep),
            ("C:", "notes.txt", Transfer),
            ("C:", "$Recycle.Bin\\S-1-5-21\\$R123.pdf", Junk),
            ("C:", "Program Files\\App\\app.exe", Keep),
            ("C:", "Python313\\python.exe", Keep),
            ("C:", "MyTool\\readme.txt", Keep),
            ("C:", "02 - OUR FILES\\Access to all drives\\AVNI\\notes.md", Protected),
            ("C:", "02 - OUR FILES\\Invoices\\inv.pdf", Transfer),
            ("C:", "Users\\sehga\\Desktop\\copilotremap.ahk", Protected),
            ("C:", "Users\\sehga\\Desktop\\other.ahk", Protected),
            ("C:", "Users\\sehga\\Documents\\report.docx", Transfer),
            ("C:", "Users\\sehga\\Downloads\\setup.zip", Extract),
            ("C:", "Users\\sehga\\Documents\\processed\\out.csv", Supabase),
            ("C:", "Users\\sehga\\Documents\\repo\\.git\\HEAD", Review),
            ("C:", "Users\\sehga\\Documents\\repo\\src\\main.rs", Review),
            ("C:", "Users\\sehga\\AppData\\Local\\Temp\\a.tmp", Junk),
            ("C:", "Users\\sehga\\AppData\\Local\\Google\\Chrome\\User Data\\Default\\Cache\\Cache_Data\\f_000001", Junk),
            ("C:", "Users\\sehga\\AppData\\Local\\Google\\Chrome\\User Data\\Default\\History", Keep),
            ("C:", "Users\\sehga\\AppData\\Roaming\\Code\\settings.json", Keep),
            ("C:", "Users\\sehga\\.cargo\\bin\\cargo.exe", Keep),
            ("C:", "Users\\sehga\\NTUSER.DAT", Keep),
            ("C:", "Users\\sehga\\Documents\\Thumbs.db", Junk),
            ("C:", "Users\\sehga\\source\\thing.txt", Review),
            ("C:", "Users\\Public\\Documents\\shared.pdf", Transfer),
            ("D:", "AE Brain\\src\\x.rs", Protected),
            ("D:", "Movies\\film.mkv", InDrive),
            ("D:", "System Volume Information\\tracking.log", Keep),
            ("D:", "$RECYCLE.BIN\\y.bin", Junk),
            ("D:", "Backups\\old.7z", Extract),
            ("D:", "Manuals Project\\Hero\\manual.pdf", Protected),
            ("G:", "My Drive\\Movies\\film.mkv", Keep),
            ("G:", "My Drive\\x.ahk", Keep),
        ];
        let mut bad = Vec::new();
        for (dr, rel, v) in want {
            let (got, reason) = verdict_of(&idx, dr, rel);
            if got != v {
                bad.push(format!("{dr}\\{rel}: want {v:?}, got {got:?} ({reason})"));
            }
        }
        assert!(bad.is_empty(), "\n{}", bad.join("\n"));
        assert!(verdict_of(&idx, "D:", "Movies\\film.mkv").1.contains("My Drive"));
    }
}
