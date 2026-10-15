//! Unzip / unblob: extract archives, nested archives too, with zip-slip and zip-bomb guards.
//!
//! Formats: zip (`zip`), tar / tar.gz / tar.bz2 (`tar` + `flate2` / `bzip2`), single-file gz / bz2,
//! 7z (`sevenz-rust2`) and everything else (rar, iso, cab, xz, ...) through the 7-Zip command line.
//!
//! Safety rules every format follows:
//! * nothing is ever written outside `dest` (absolute, `..`, drive and UNC names are skipped and reported),
//! * symlinks and hard links are never created, permissions and ownership are not restored,
//! * one byte budget covers the whole recursive call and is enforced while streaming,
//! * password protected entries are skipped (no prompt, nothing waits on input),
//! * damaged input never panics; it ends in `ExtractOutcome::error`.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveKind {
    Zip,
    Tar,
    TarGz,
    TarBz2,
    Gz,
    Bz2,
    SevenZ,
    /// Needs 7-Zip (7z.exe): rar, iso, cab, xz, tar.xz, lzh, arj, wim, ...
    External,
}

/// Archive suffixes, longest first within a family (".tar.gz" must be tried before ".gz").
/// Office files (.docx .xlsx .pptx .odt), .jar and .apk are zips on disk but are documents / apps: not listed.
const SUFFIXES: &[(&str, ArchiveKind)] = &[
    (".tar.gz", ArchiveKind::TarGz),
    (".tgz", ArchiveKind::TarGz),
    (".tar.bz2", ArchiveKind::TarBz2),
    (".tbz2", ArchiveKind::TarBz2),
    (".tbz", ArchiveKind::TarBz2),
    (".tar.xz", ArchiveKind::External),
    (".txz", ArchiveKind::External),
    (".tar.zst", ArchiveKind::External),
    (".tar", ArchiveKind::Tar),
    (".zip", ArchiveKind::Zip),
    (".gz", ArchiveKind::Gz),
    (".bz2", ArchiveKind::Bz2),
    (".7z", ArchiveKind::SevenZ),
    (".rar", ArchiveKind::External),
    (".iso", ArchiveKind::External),
    (".cab", ArchiveKind::External),
    (".xz", ArchiveKind::External),
    (".lzh", ArchiveKind::External),
    (".lha", ArchiveKind::External),
    (".arj", ArchiveKind::External),
    (".wim", ArchiveKind::External),
    (".zst", ArchiveKind::External),
];

/// Message used whenever an `External` archive is met and no 7-Zip is configured.
const NEEDS_7ZIP: &str = "Needs 7-Zip (install from 7-zip.org)";

/// Copy buffer. The byte budget is enforced per read, so the overshoot is zero; this is also the most that
/// is ever written between two budget checks.
const BUF_SIZE: usize = 64 * 1024;
/// Cancel flag and progress are looked at about every this many bytes inside a copy loop.
const CHECK_EVERY: u64 = 1024 * 1024;
/// `skipped` is capped so a hostile archive with millions of bad entries cannot eat memory.
const MAX_SKIPPED: usize = 1000;
/// Windows `CREATE_NO_WINDOW`: 7z.exe never flashes a console.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Splits a file name into (name without archive extension, kind). Case-insensitive; a path is reduced to its
/// last component first.
fn split_archive_name(name: &str) -> Option<(&str, ArchiveKind)> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let lower = base.to_ascii_lowercase();
    SUFFIXES
        .iter()
        .find(|(ext, _)| lower.ends_with(ext))
        // ASCII lowercasing keeps byte lengths, and the suffix starts with an ASCII '.', so this is a char boundary.
        .map(|(ext, kind)| (&base[..base.len() - ext.len()], *kind))
}

/// By file name (case-insensitive). None = not an archive we handle.
pub fn archive_kind(name: &str) -> Option<ArchiveKind> {
    split_archive_name(name).map(|(_, kind)| kind)
}

/// `configured` if it exists, else C:\Program Files\7-Zip\7z.exe, C:\Program Files (x86)\7-Zip\7z.exe, then `7z` on PATH.
pub fn find_sevenzip(configured: &str) -> Option<PathBuf> {
    let configured = configured.trim();
    if !configured.is_empty() {
        let p = PathBuf::from(configured);
        if p.is_file() {
            return Some(p);
        }
    }
    if cfg!(windows) {
        let mut candidates = vec![
            PathBuf::from(r"C:\Program Files\7-Zip\7z.exe"),
            PathBuf::from(r"C:\Program Files (x86)\7-Zip\7z.exe"),
        ];
        // Windows installed on another drive.
        for var in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
            if let Some(root) = std::env::var_os(var) {
                let p = Path::new(&root).join("7-Zip").join("7z.exe");
                if !candidates.contains(&p) {
                    candidates.push(p);
                }
            }
        }
        if let Some(p) = candidates.into_iter().find(|p| p.is_file()) {
            return Some(p);
        }
    }
    find_in_path(std::env::var_os("PATH")?.as_os_str())
}

/// First `7z` / `7z.exe` found in the directories of a PATH-style list.
fn find_in_path(path_var: &std::ffi::OsStr) -> Option<PathBuf> {
    for dir in std::env::split_paths(path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for exe in ["7z", "7z.exe"] {
            let candidate = dir.join(exe);
            if is_runnable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(unix)]
fn is_runnable_file(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

#[cfg(not(unix))]
fn is_runnable_file(p: &Path) -> bool {
    p.is_file()
}

/// "<dir>/<name without archive extension> (extracted)"; if taken, " (extracted 2)", " (extracted 3)", ...
pub fn dest_for(archive: &Path) -> PathBuf {
    let dir = archive.parent().unwrap_or_else(|| Path::new(""));
    let name = file_name_of(archive);
    let stem = match split_archive_name(&name) {
        Some((stem, _)) => stem.to_string(),
        // Not an archive name: drop a plain extension, keep the rest.
        None => Path::new(&name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
    };
    let stem = if stem.is_empty() { "archive".to_string() } else { stem };
    let mut n = 1u64;
    loop {
        let candidate = if n == 1 {
            dir.join(format!("{stem} (extracted)"))
        } else {
            dir.join(format!("{stem} (extracted {n})"))
        };
        // symlink_metadata: a dangling symlink also counts as taken.
        if fs::symlink_metadata(&candidate).is_err() {
            return candidate;
        }
        n += 1;
    }
}

pub struct ExtractOptions<'a> {
    /// Nested archives inside the extracted tree are opened up to this depth (1 = only the archive itself).
    pub max_depth: u32,
    /// Stop when extracted bytes > archive bytes * max_ratio.
    pub max_ratio: u64,
    /// Hard ceiling on bytes written for this archive (free space minus reserve).
    pub max_total_bytes: u64,
    pub sevenzip: Option<PathBuf>,
    pub cancel: &'a AtomicBool,
}

#[derive(Clone, Debug, Default)]
pub struct ExtractOutcome {
    pub archive: PathBuf,
    pub dest: PathBuf,
    pub files: u64,
    pub bytes: u64,
    /// Nested archives opened.
    pub nested: u64,
    pub skipped: Vec<String>,
    pub error: Option<String>,
}

/// Extract `archive` into `dest` (created), then open nested archives found inside, each into its own
/// "(extracted)" folder next to it, up to `max_depth`. `progress(current_name, bytes_written_so_far)`.
pub fn extract_recursive(
    archive: &Path,
    dest: &Path,
    opts: &ExtractOptions,
    progress: &mut dyn FnMut(&str, u64),
) -> ExtractOutcome {
    let archive_size = fs::metadata(archive).map(|m| m.len()).unwrap_or(0);
    let mut st = State {
        opts,
        progress,
        out: ExtractOutcome { archive: archive.to_path_buf(), dest: dest.to_path_buf(), ..Default::default() },
        ratio_limit: archive_size.saturating_mul(opts.max_ratio),
        buf: vec![0u8; BUF_SIZE],
        since_check: 0,
        skipped_more: 0,
        root: dest.to_path_buf(),
    };

    let name = file_name_of(archive);
    let result = match archive_kind(&name) {
        None => Err(Halt::Fail(format!("not a supported archive type: {name}"))),
        Some(kind) => process(&mut st, archive, kind, dest, 1),
    };
    if let Err(halt) = result {
        st.out.error = Some(halt.message());
    }
    if st.skipped_more > 0 {
        let more = st.skipped_more;
        st.out.skipped.push(format!("... and {more} more skipped"));
    }
    st.out
}

// ---------------------------------------------------------------------------------------------------------------
// internals
// ---------------------------------------------------------------------------------------------------------------

/// Why an extraction stopped early.
enum Halt {
    Cancelled,
    /// A byte budget was used up ("stopped: ...").
    Limit(String),
    /// This archive could not be read or written.
    Fail(String),
}

impl Halt {
    fn message(self) -> String {
        match self {
            Halt::Cancelled => "cancelled".to_string(),
            Halt::Limit(m) | Halt::Fail(m) => m,
        }
    }
    /// Cancel and budget stops end the whole recursive call; a failure only ends one archive.
    fn is_fatal(&self) -> bool {
        !matches!(self, Halt::Fail(_))
    }
}

/// Failure while copying one stream into one file.
enum CopyErr {
    Halt(Halt),
    Read(io::Error),
    Write(io::Error),
}

impl From<Halt> for CopyErr {
    fn from(h: Halt) -> Self {
        CopyErr::Halt(h)
    }
}

/// Failure while extracting one entry.
enum EntryErr {
    Halt(Halt),
    /// The file or its folders could not be created (bad name, name clash, permissions): skip this entry only.
    Create(io::Error),
    /// The archive data could not be read (damaged, CRC mismatch).
    Read(io::Error),
    /// Writing to disk failed (disk full ...): nothing more will work.
    Write(io::Error),
}

struct State<'a> {
    opts: &'a ExtractOptions<'a>,
    progress: &'a mut dyn FnMut(&str, u64),
    out: ExtractOutcome,
    /// archive size * max_ratio for the top archive: the one ratio budget of the whole recursive call.
    ratio_limit: u64,
    buf: Vec<u8>,
    since_check: u64,
    skipped_more: u64,
    /// Destination of the top archive, to show nested paths relative to it.
    root: PathBuf,
}

impl State<'_> {
    fn check_cancel(&self) -> Result<(), Halt> {
        if self.opts.cancel.load(Ordering::Relaxed) {
            Err(Halt::Cancelled)
        } else {
            Ok(())
        }
    }

    fn note(&mut self, name: &str) {
        (self.progress)(name, self.out.bytes);
    }

    fn skip(&mut self, name: &str, why: &str) {
        if self.out.skipped.len() < MAX_SKIPPED {
            self.out.skipped.push(format!("{name}: {why}"));
        } else {
            self.skipped_more += 1;
        }
    }

    /// Bytes that may still be written, and whether the ratio limit (rather than the total) is the tighter one.
    fn budget_left(&self) -> (u64, bool) {
        let ratio_left = self.ratio_limit.saturating_sub(self.out.bytes);
        let total_left = self.opts.max_total_bytes.saturating_sub(self.out.bytes);
        (ratio_left.min(total_left), ratio_left <= total_left)
    }

    fn limit_halt(&self) -> Halt {
        if self.budget_left().1 {
            Halt::Limit(format!("stopped: extracted size passed {} x the archive size", self.opts.max_ratio))
        } else {
            Halt::Limit("stopped: would pass the free-space limit".to_string())
        }
    }

    /// Streams `r` into `w`, counting every byte against the budget as it is written.
    fn copy(&mut self, name: &str, r: &mut dyn Read, w: &mut dyn Write) -> Result<(), CopyErr> {
        let mut buf = std::mem::take(&mut self.buf);
        if buf.len() < BUF_SIZE {
            buf = vec![0u8; BUF_SIZE];
        }
        let res = self.copy_loop(name, r, w, &mut buf);
        self.buf = buf;
        res
    }

    fn copy_loop(&mut self, name: &str, r: &mut dyn Read, w: &mut dyn Write, buf: &mut [u8]) -> Result<(), CopyErr> {
        loop {
            let n = match r.read(buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(CopyErr::Read(e)),
            };
            let (left, _) = self.budget_left();
            if n as u64 > left {
                // Write what still fits so the output ends exactly at the limit, then stop.
                let fits = left as usize;
                if fits > 0 {
                    w.write_all(&buf[..fits]).map_err(CopyErr::Write)?;
                    self.out.bytes += fits as u64;
                }
                return Err(CopyErr::Halt(self.limit_halt()));
            }
            w.write_all(&buf[..n]).map_err(CopyErr::Write)?;
            self.out.bytes += n as u64;
            self.since_check += n as u64;
            if self.since_check >= CHECK_EVERY {
                self.since_check = 0;
                self.check_cancel()?;
                self.note(name);
            }
        }
        Ok(())
    }

    /// Creates `target` (and its folders), streams `r` into it and stamps the modification time.
    fn write_file(
        &mut self,
        name: &str,
        target: &Path,
        r: &mut dyn Read,
        mtime: Option<SystemTime>,
    ) -> Result<(), EntryErr> {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(EntryErr::Create)?;
        }
        let mut f = File::create(target).map_err(EntryErr::Create)?;
        self.out.files += 1;
        match self.copy(name, r, &mut f) {
            Ok(_) => {}
            Err(CopyErr::Halt(h)) => return Err(EntryErr::Halt(h)),
            Err(CopyErr::Read(e)) => return Err(EntryErr::Read(e)),
            Err(CopyErr::Write(e)) => return Err(EntryErr::Write(e)),
        }
        if let Some(t) = mtime {
            let _ = f.set_modified(t); // best effort
        }
        Ok(())
    }

    /// Creates a directory entry. A failure only skips that entry.
    fn make_dir(&mut self, name: &str, target: &Path) {
        if let Err(e) = fs::create_dir_all(target) {
            self.skip(name, &format!("cannot create folder: {e}"));
        }
    }
}

/// Extracts one archive, then (below `max_depth`) every nested archive found in what it produced.
/// `Err` is returned for the archive itself failing, and for any cancel / budget stop in a nested one.
fn process(st: &mut State, archive: &Path, kind: ArchiveKind, dest: &Path, depth: u32) -> Result<(), Halt> {
    let soft = extract_one(st, archive, kind, dest)?;
    if let Some(msg) = soft {
        if depth == 1 {
            st.out.error = Some(msg);
        } else {
            let label = display_rel(&st.root, archive);
            st.skip(&label, &msg);
        }
    }
    if depth >= st.opts.max_depth {
        return Ok(());
    }
    for (file, nested_kind) in find_archives(dest) {
        st.check_cancel()?;
        let nested_dest = dest_for(&file);
        match process(st, &file, nested_kind, &nested_dest, depth + 1) {
            Ok(()) => st.out.nested += 1,
            Err(h) if h.is_fatal() => return Err(h),
            Err(h) => {
                let label = display_rel(&st.root, &file);
                st.skip(&label, &format!("nested archive not opened: {}", h.message()));
                let _ = fs::remove_dir(&nested_dest); // only succeeds when empty
            }
        }
    }
    Ok(())
}

/// Extracts one archive by kind. `Ok(Some(msg))`: extracted, but something went wrong on the way.
fn extract_one(st: &mut State, archive: &Path, kind: ArchiveKind, dest: &Path) -> Result<Option<String>, Halt> {
    st.check_cancel()?;
    if kind == ArchiveKind::External && st.opts.sevenzip.is_none() {
        return Err(Halt::Fail(NEEDS_7ZIP.to_string()));
    }
    fs::create_dir_all(dest).map_err(|e| Halt::Fail(format!("cannot create {}: {e}", dest.display())))?;
    let label = file_name_of(archive);
    st.note(&label);

    // A malformed archive must never take the process down, whatever the format crates make of it.
    let run = catch_unwind(AssertUnwindSafe(|| -> Result<Option<String>, Halt> {
        match kind {
            ArchiveKind::Zip => extract_zip(st, archive, dest),
            ArchiveKind::Tar => extract_tar(st, open(archive)?, dest),
            ArchiveKind::TarGz => {
                extract_tar(st, flate2::read::MultiGzDecoder::new(BufReader::new(open(archive)?)), dest)
            }
            ArchiveKind::TarBz2 => extract_tar(st, bzip2::read::MultiBzDecoder::new(open(archive)?), dest),
            ArchiveKind::Gz | ArchiveKind::Bz2 => extract_single(st, archive, kind, dest),
            ArchiveKind::SevenZ => extract_sevenz(st, archive, dest),
            ArchiveKind::External => extract_external(st, archive, dest),
        }
    }));
    let result = match run {
        Ok(r) => r,
        Err(_) => Err(Halt::Fail("internal error while reading this archive (damaged?)".to_string())),
    };
    if result.is_ok() {
        st.note(&label); // closing call: the total after this archive
    }
    result
}

fn open(archive: &Path) -> Result<File, Halt> {
    File::open(archive).map_err(|e| Halt::Fail(format!("cannot open archive: {e}")))
}

// --- zip ---------------------------------------------------------------------------------------------------------

struct ZipMeta {
    name: String,
    dir: bool,
    symlink: bool,
    encrypted: bool,
}

fn extract_zip(st: &mut State, archive: &Path, dest: &Path) -> Result<Option<String>, Halt> {
    use zip::result::ZipError;

    let mut za = zip::ZipArchive::new(BufReader::new(open(archive)?))
        .map_err(|e| Halt::Fail(format!("not a valid zip archive: {e}")))?;
    let mut damaged = 0u64;
    for i in 0..za.len() {
        st.check_cancel()?;
        let meta = match za.by_index_data(i) {
            Ok(e) => ZipMeta {
                name: match e.name() {
                    Ok(n) => n.into_owned(),
                    Err(_) => String::from_utf8_lossy(e.name_raw()).into_owned(),
                },
                dir: e.is_dir(),
                symlink: e.is_symlink(),
                encrypted: e.encrypted(),
            },
            Err(e) => {
                damaged += 1;
                st.skip(&format!("entry #{i}"), &format!("unreadable header: {e}"));
                continue;
            }
        };
        st.note(&meta.name);
        let rel = match safe_relative(&meta.name) {
            Ok(Some(rel)) => rel,
            Ok(None) => continue,
            Err(why) => {
                st.skip(&meta.name, why);
                continue;
            }
        };
        let target = dest.join(&rel);
        if meta.dir {
            st.make_dir(&meta.name, &target);
            continue;
        }
        if meta.symlink {
            st.skip(&meta.name, "symlink");
            continue;
        }
        if meta.encrypted {
            st.skip(&meta.name, "password protected");
            continue;
        }
        let mut zf = match za.by_index(i) {
            Ok(z) => z,
            Err(ZipError::UnsupportedArchive(m)) if m == ZipError::PASSWORD_REQUIRED => {
                st.skip(&meta.name, "password protected");
                continue;
            }
            Err(ZipError::InvalidPassword) => {
                st.skip(&meta.name, "password protected");
                continue;
            }
            Err(ZipError::CompressionMethodNotSupported(_) | ZipError::UnsupportedArchive(_)) => {
                st.skip(&meta.name, "unsupported zip compression method");
                continue;
            }
            Err(e) => {
                damaged += 1;
                st.skip(&meta.name, &format!("damaged: {e}"));
                continue;
            }
        };
        let mtime = zip_mtime(&zf);
        match st.write_file(&meta.name, &target, &mut zf, mtime) {
            Ok(()) => {}
            Err(EntryErr::Halt(h)) => return Err(h),
            Err(EntryErr::Create(e)) => st.skip(&meta.name, &format!("cannot create file: {e}")),
            Err(EntryErr::Write(e)) => return Err(Halt::Fail(format!("write failed: {e}"))),
            Err(EntryErr::Read(e)) => {
                damaged += 1;
                st.skip(&meta.name, &format!("damaged: {e}"));
            }
        }
    }
    Ok((damaged > 0).then(|| format!("{damaged} entries could not be read (damaged archive?)")))
}

/// Modification time of a zip entry: UTC extra fields first (Info-ZIP / NTFS), else the DOS date and time.
/// The DOS time carries no time zone, so it is taken as UTC (off by the writer's UTC offset at worst).
fn zip_mtime<R: Read>(zf: &zip::read::ZipFile<'_, R>) -> Option<SystemTime> {
    for field in zf.extra_data_fields() {
        match field {
            zip::extra_fields::ExtraField::ExtendedTimestamp(ts) => {
                if let Some(secs) = ts.mod_time() {
                    return UNIX_EPOCH.checked_add(Duration::from_secs(u64::from(secs)));
                }
            }
            zip::extra_fields::ExtraField::Ntfs(n) => {
                if let Some(t) = filetime_to_system(n.mtime()) {
                    return Some(t);
                }
            }
            _ => {}
        }
    }
    let dt = zf.last_modified()?;
    let days = days_from_civil(i64::from(dt.year()), i64::from(dt.month()), i64::from(dt.day()));
    let secs = days * 86_400 + i64::from(dt.hour()) * 3600 + i64::from(dt.minute()) * 60 + i64::from(dt.second());
    u64::try_from(secs).ok().and_then(|s| UNIX_EPOCH.checked_add(Duration::from_secs(s)))
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Windows FILETIME (100 ns since 1601-01-01) to `SystemTime`; None before 1970 or when out of range.
fn filetime_to_system(raw: u64) -> Option<SystemTime> {
    const UNIX_EPOCH_AS_FILETIME: u64 = 116_444_736_000_000_000;
    let since_unix = raw.checked_sub(UNIX_EPOCH_AS_FILETIME)?;
    UNIX_EPOCH.checked_add(Duration::new(since_unix / 10_000_000, ((since_unix % 10_000_000) * 100) as u32))
}

// --- tar ---------------------------------------------------------------------------------------------------------

fn extract_tar<R: Read>(st: &mut State, reader: R, dest: &Path) -> Result<Option<String>, Halt> {
    let mut ar = tar::Archive::new(reader);
    let entries = ar.entries().map_err(|e| Halt::Fail(format!("not a valid tar archive: {e}")))?;
    for entry in entries {
        st.check_cancel()?;
        let mut entry = entry.map_err(|e| Halt::Fail(format!("damaged tar archive: {e}")))?;
        let name = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions() {
            continue;
        }
        st.note(&name);
        let rel = match safe_relative(&name) {
            Ok(Some(rel)) => rel,
            Ok(None) => continue,
            Err(why) => {
                st.skip(&name, why);
                continue;
            }
        };
        let target = dest.join(&rel);
        if kind.is_dir() {
            st.make_dir(&name, &target);
            continue;
        }
        if kind.is_symlink() {
            st.skip(&name, "symlink");
            continue;
        }
        if kind.is_hard_link() {
            st.skip(&name, "hard link");
            continue;
        }
        if !(kind.is_file() || kind.is_contiguous() || kind.is_gnu_sparse()) {
            st.skip(&name, "special file");
            continue;
        }
        let mtime = entry
            .header()
            .mtime()
            .ok()
            .and_then(|secs| UNIX_EPOCH.checked_add(Duration::from_secs(secs)));
        match st.write_file(&name, &target, &mut entry, mtime) {
            Ok(()) => {}
            Err(EntryErr::Halt(h)) => return Err(h),
            Err(EntryErr::Create(e)) => st.skip(&name, &format!("cannot create file: {e}")),
            Err(EntryErr::Write(e)) => return Err(Halt::Fail(format!("write failed: {e}"))),
            // A broken stream cannot be resynchronised: the rest of the tar is lost.
            Err(EntryErr::Read(e)) => return Err(Halt::Fail(format!("damaged tar archive: {e}"))),
        }
    }
    Ok(None)
}

// --- gz / bz2 (single file) ---------------------------------------------------------------------------------------

fn extract_single(st: &mut State, archive: &Path, kind: ArchiveKind, dest: &Path) -> Result<Option<String>, Halt> {
    let name = file_name_of(archive);
    let stem = split_archive_name(&name).map(|(s, _)| s).unwrap_or(name.as_str());
    let out_name = if stem.is_empty() { "extracted" } else { stem };
    let target = dest.join(out_name);
    let file = open(archive)?;
    let (files_before, bytes_before) = (st.out.files, st.out.bytes);

    let mut gz_mtime: Option<SystemTime> = None;
    let res = if kind == ArchiveKind::Gz {
        let mut dec = flate2::read::MultiGzDecoder::new(BufReader::new(file));
        let r = st.write_file(out_name, &target, &mut dec, None);
        gz_mtime = dec
            .header()
            .map(|h| h.mtime())
            .filter(|&m| m > 0)
            .and_then(|m| UNIX_EPOCH.checked_add(Duration::from_secs(u64::from(m))));
        r
    } else {
        let mut dec = bzip2::read::MultiBzDecoder::new(file);
        st.write_file(out_name, &target, &mut dec, None)
    };
    match res {
        Ok(()) => {
            if let Some(t) = gz_mtime {
                let _ = fs::OpenOptions::new().write(true).open(&target).and_then(|f| f.set_modified(t));
            }
            Ok(None)
        }
        Err(EntryErr::Halt(h)) => Err(h),
        Err(EntryErr::Create(e)) => Err(Halt::Fail(format!("cannot create {out_name}: {e}"))),
        Err(EntryErr::Write(e)) => Err(Halt::Fail(format!("write failed: {e}"))),
        Err(EntryErr::Read(e)) => {
            if st.out.bytes == bytes_before && fs::remove_file(&target).is_ok() {
                st.out.files = files_before; // nothing was decoded: do not leave an empty husk behind
            }
            Err(Halt::Fail(format!("damaged {} data: {e}", if kind == ArchiveKind::Gz { "gzip" } else { "bzip2" })))
        }
    }
}

// --- 7z ----------------------------------------------------------------------------------------------------------

fn extract_sevenz(st: &mut State, archive: &Path, dest: &Path) -> Result<Option<String>, Halt> {
    use sevenz_rust2::{Archive, BlockDecoder, Error as SzError, Password};

    let label = file_name_of(archive);
    let password = Password::empty();
    let mut src = BufReader::new(open(archive)?);
    let sz = match Archive::read(&mut src, &password) {
        Ok(a) => a,
        // Encrypted file names: nothing can be listed without the password.
        Err(SzError::PasswordRequired | SzError::MaybeBadPassword(_)) => {
            st.skip(&label, "password protected");
            return Ok(None);
        }
        Err(e) => return Err(Halt::Fail(format!("not a valid 7z archive: {e}"))),
    };

    for block in 0..sz.blocks.len() {
        st.check_cancel()?;
        let dec = BlockDecoder::new(1, block, &sz, &password, &mut src);
        let first = sz.stream_map.block_first_file_index.get(block).copied().unwrap_or(0);
        let count = dec.entry_count();
        let mut halt: Option<Halt> = None;
        let res = dec.for_each_entries(&mut |entry, reader| match sevenz_entry(st, entry, reader, dest) {
            Ok(()) => Ok(true),
            Err(h) => {
                halt = Some(h);
                Err(SzError::Other("stopped".into()))
            }
        });
        if let Some(h) = halt {
            return Err(h);
        }
        match res {
            Ok(_) => {}
            // Content encrypted: skip every entry of this block, keep going with the others.
            Err(SzError::PasswordRequired | SzError::MaybeBadPassword(_)) => {
                let range = first..first.saturating_add(count);
                for f in sz.files.get(range).unwrap_or(&[]) {
                    st.skip(&f.name, "password protected");
                }
            }
            Err(e) => return Err(Halt::Fail(format!("damaged 7z data: {e}"))),
        }
    }

    // Folders and empty files have no data block.
    for (i, entry) in sz.files.iter().enumerate() {
        if sz.stream_map.file_block_index.get(i).copied().flatten().is_some() {
            continue;
        }
        let mut empty: &[u8] = &[];
        sevenz_entry(st, entry, &mut empty, dest)?;
    }
    Ok(None)
}

/// One 7z entry. `Err` only for stops that end the archive (cancel, budget, disk failure, broken data).
fn sevenz_entry(
    st: &mut State,
    entry: &sevenz_rust2::ArchiveEntry,
    reader: &mut dyn Read,
    dest: &Path,
) -> Result<(), Halt> {
    st.check_cancel()?;
    if entry.is_anti_item() {
        return Ok(());
    }
    let name = entry.name().to_string();
    st.note(&name);
    let rel = match safe_relative(&name) {
        Ok(Some(rel)) => rel,
        Ok(None) => return Ok(()),
        Err(why) => {
            st.skip(&name, why);
            return Ok(());
        }
    };
    let target = dest.join(&rel);
    if entry.is_directory() {
        st.make_dir(&name, &target);
        return Ok(());
    }
    if entry.has_windows_attributes && is_sevenz_link(entry.windows_attributes()) {
        st.skip(&name, "symlink");
        return Ok(());
    }
    let mtime = if entry.has_last_modified_date {
        filetime_to_system(u64::from(entry.last_modified_date()))
    } else {
        None
    };
    match st.write_file(&name, &target, reader, mtime) {
        Ok(()) => Ok(()),
        Err(EntryErr::Halt(h)) => Err(h),
        Err(EntryErr::Create(e)) => {
            st.skip(&name, &format!("cannot create file: {e}"));
            Ok(())
        }
        Err(EntryErr::Write(e)) => Err(Halt::Fail(format!("write failed: {e}"))),
        Err(EntryErr::Read(e)) => Err(Halt::Fail(format!("damaged 7z data: {e}"))),
    }
}

/// 7z keeps unix modes in the top 16 bits of the attributes (flag 0x8000); also reparse points.
fn is_sevenz_link(attrs: u32) -> bool {
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    const FILE_ATTRIBUTE_UNIX_EXTENSION: u32 = 0x8000;
    const S_IFMT: u32 = 0o170_000;
    const S_IFLNK: u32 = 0o120_000;
    attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || (attrs & FILE_ATTRIBUTE_UNIX_EXTENSION != 0 && (attrs >> 16) & S_IFMT == S_IFLNK)
}

// --- everything else: the 7-Zip command line -------------------------------------------------------------------------

fn extract_external(st: &mut State, archive: &Path, dest: &Path) -> Result<Option<String>, Halt> {
    let Some(exe) = st.opts.sevenzip.clone() else {
        return Err(Halt::Fail(NEEDS_7ZIP.to_string()));
    };
    let label = file_name_of(archive);
    let (before_files, before_bytes) = dir_stats(dest);
    let (budget, _) = st.budget_left();

    // 7z x -y -bd -p<dummy> -o<dest> -- <archive>
    // stdin is null and a (wrong) password is always given, so an encrypted archive fails fast instead of asking.
    let mut out_arg = OsString::from("-o");
    out_arg.push(dest.as_os_str());
    let mut cmd = Command::new(&exe);
    cmd.arg("x").arg("-y").arg("-bd").arg("-pavni-no-password").arg(out_arg).arg("--").arg(archive);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(Halt::Fail(NEEDS_7ZIP.to_string())),
        Err(e) => return Err(Halt::Fail(format!("cannot run 7-Zip: {e}"))),
    };
    let readers: Vec<thread::JoinHandle<String>> = [
        child.stdout.take().map(drain_tail),
        child.stderr.take().map(drain_tail),
    ]
    .into_iter()
    .flatten()
    .collect();

    // 7z writes straight to disk, so the budget is watched by measuring the destination while it runs.
    let started = Instant::now();
    let mut next_measure = started + Duration::from_millis(500);
    let mut stop: Option<Halt> = None;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(e) => {
                stop = Some(Halt::Fail(format!("cannot wait for 7-Zip: {e}")));
                break None;
            }
        }
        if st.check_cancel().is_err() {
            stop = Some(Halt::Cancelled);
            break None;
        }
        let now = Instant::now();
        if now >= next_measure {
            let written = dir_stats(dest).1.saturating_sub(before_bytes);
            (st.progress)(&label, st.out.bytes + written);
            if written > budget {
                stop = Some(Halt::Limit(String::new())); // message filled in below
                break None;
            }
            // Back off when the tree is big so measuring never costs more than ~1/4 of the time.
            next_measure = Instant::now() + Duration::from_millis(500).max(now.elapsed() * 4);
        }
        thread::sleep(Duration::from_millis(25));
    };
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let tail: String = readers.into_iter().filter_map(|h| h.join().ok()).collect::<Vec<_>>().join("\n");

    let (files, bytes) = dir_stats(dest);
    let new_bytes = bytes.saturating_sub(before_bytes);
    st.out.files += files.saturating_sub(before_files);
    st.out.bytes += new_bytes;
    st.note(&label);

    match stop {
        Some(Halt::Limit(_)) => return Err(st.limit_halt()),
        Some(h) => return Err(h),
        None => {}
    }
    if new_bytes > budget {
        return Err(st.limit_halt());
    }
    let Some(status) = status else {
        return Err(Halt::Fail("7-Zip did not finish".to_string()));
    };
    match status.code() {
        Some(0) => Ok(None),
        Some(1) => Ok(Some("7-Zip finished with warnings".to_string())),
        _ => {
            let lower = tail.to_ascii_lowercase();
            if lower.contains("wrong password") || lower.contains("enter password") || lower.contains("encrypted") {
                st.skip(&label, "password protected");
                return Ok(None);
            }
            Err(Halt::Fail(format!("7-Zip failed: {}", last_error_line(&tail))))
        }
    }
}

/// Reads a pipe to the end on a thread (so 7z never blocks on a full pipe) and keeps only the last few KB.
fn drain_tail<P: Read + Send + 'static>(mut pipe: P) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut tail: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        while let Ok(n) = pipe.read(&mut chunk) {
            if n == 0 {
                break;
            }
            tail.extend_from_slice(&chunk[..n]);
            if tail.len() > 16 * 1024 {
                let cut = tail.len() - 8 * 1024;
                tail.drain(..cut);
            }
        }
        String::from_utf8_lossy(&tail).into_owned()
    })
}

/// The most useful line of 7z's output for an error message.
fn last_error_line(text: &str) -> String {
    let line = text
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| l.to_ascii_uppercase().contains("ERROR"))
        .or_else(|| text.lines().rev().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or("unknown error");
    line.chars().take(300).collect()
}

// --- helpers -----------------------------------------------------------------------------------------------------

fn file_name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// `p` relative to `root` for messages (falls back to the file name).
fn display_rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).map(|r| r.display().to_string()).unwrap_or_else(|_| file_name_of(p))
}

/// Turns an archive entry name into a relative path that is guaranteed to stay inside the destination.
/// `Ok(None)`: nothing to extract (empty name, "./"). `Err(reason)`: unsafe, to be skipped and reported.
/// Both `/` and `\` separate components, whatever the host OS.
fn safe_relative(name: &str) -> Result<Option<PathBuf>, &'static str> {
    if name.contains('\0') {
        return Err("unsafe path (NUL character)");
    }
    let norm = name.replace('\\', "/");
    if norm.starts_with('/') {
        return Err("unsafe path (absolute or UNC)");
    }
    let b = norm.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return Err("unsafe path (drive letter)");
    }
    let mut rel = PathBuf::new();
    for comp in norm.split('/') {
        match comp {
            "" | "." => {}
            ".." => return Err("unsafe path (..)"),
            c => {
                // "...", ". ." and the like: Windows strips trailing dots and spaces, which could turn it into "..".
                if c.trim_matches(['.', ' ']).is_empty() {
                    return Err("unsafe path (invalid component)");
                }
                if cfg!(windows) {
                    if c.contains(':') {
                        return Err("unsafe path (':' in name)"); // NTFS alternate data stream / drive
                    }
                    if is_reserved_windows_name(c) {
                        return Err("unsafe path (reserved Windows device name)");
                    }
                }
                rel.push(c);
            }
        }
    }
    Ok(if rel.as_os_str().is_empty() { None } else { Some(rel) })
}

/// Windows device names (CON, NUL, COM1, LPT1 ...), with or without an extension: creating such a file opens
/// the device instead.
fn is_reserved_windows_name(component: &str) -> bool {
    let stem = component.split('.').next().unwrap_or(component).trim_end_matches(' ').to_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$") {
        return true;
    }
    let mut chars = stem.chars();
    let head: String = chars.by_ref().take(3).collect();
    matches!(head.as_str(), "COM" | "LPT")
        && matches!(chars.next(), Some('0'..='9' | '\u{b9}' | '\u{b2}' | '\u{b3}'))
        && chars.next().is_none()
}

/// Archive files below `root` (symlinks are never followed), in a stable order.
fn find_archives(root: &Path) -> Vec<(PathBuf, ArchiveKind)> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_file() {
                if let Some(kind) = archive_kind(&e.file_name().to_string_lossy()) {
                    found.push((e.path(), kind));
                }
            }
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

/// (file count, total bytes) of the regular files below `root`; symlinks are not followed.
fn dir_stats(root: &Path) -> (u64, u64) {
    let (mut files, mut bytes) = (0u64, 0u64);
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_file() {
                files += 1;
                bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    (files, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tempfile::tempdir;

    // ---- helpers ----------------------------------------------------------------------------------------------

    fn opts(cancel: &AtomicBool) -> ExtractOptions<'_> {
        ExtractOptions { max_depth: 4, max_ratio: 1000, max_total_bytes: 1 << 40, sevenzip: find_sevenzip(""), cancel }
    }

    fn run(archive: &Path, dest: &Path, o: &ExtractOptions) -> ExtractOutcome {
        extract_recursive(archive, dest, o, &mut |_, _| {})
    }

    fn zip_opts() -> zip::write::SimpleFileOptions {
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)
    }

    /// Entry names ending in '/' are folders.
    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in entries {
            if name.ends_with('/') {
                w.add_directory(*name, zip_opts()).unwrap();
            } else {
                w.start_file(*name, zip_opts()).unwrap();
                w.write_all(data).unwrap();
            }
        }
        w.finish().unwrap().into_inner()
    }

    fn tar_bytes(entries: &[(&str, &[u8])], mtime: u64) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (name, data) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_mtime(mtime);
            h.set_cksum();
            b.append_data(&mut h, name, *data).unwrap();
        }
        b.into_inner().unwrap()
    }

    fn gz(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    fn bz2(data: &[u8]) -> Vec<u8> {
        let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    fn junk(n: usize) -> Vec<u8> {
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
                (x >> 33) as u8
            })
            .collect()
    }

    fn all_files(root: &Path) -> Vec<PathBuf> {
        let mut v = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if e.file_type().unwrap().is_dir() {
                    stack.push(p);
                } else {
                    v.push(p);
                }
            }
        }
        v.sort();
        v
    }

    fn mtime_secs(p: &Path) -> i64 {
        fs::metadata(p).unwrap().modified().unwrap().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
    }

    fn seven() -> Option<PathBuf> {
        let p = find_sevenzip("");
        if p.is_none() {
            eprintln!("SKIPPED: 7z is not on PATH");
        }
        p
    }

    /// Runs `7z <args>` inside `dir`.
    fn run_7z(exe: &Path, dir: &Path, args: &[&str]) {
        let out = Command::new(exe).current_dir(dir).args(args).stdin(Stdio::null()).output().unwrap();
        assert!(out.status.success(), "7z {args:?} failed: {}", String::from_utf8_lossy(&out.stdout));
    }

    // ---- 1. archive_kind -------------------------------------------------------------------------------------------

    #[test]
    fn archive_kind_table() {
        use ArchiveKind::*;
        let table: &[(&str, Option<ArchiveKind>)] = &[
            ("a.zip", Some(Zip)),
            ("A.ZIP", Some(Zip)),
            ("a.tar", Some(Tar)),
            ("a.tar.gz", Some(TarGz)),
            ("a.TGZ", Some(TarGz)),
            ("a.tgz", Some(TarGz)),
            ("A.Tar.Gz", Some(TarGz)),
            ("a.tar.bz2", Some(TarBz2)),
            ("a.tbz2", Some(TarBz2)),
            ("a.tbz", Some(TarBz2)),
            ("a.gz", Some(Gz)),
            ("notes.txt.gz", Some(Gz)),
            ("a.bz2", Some(Bz2)),
            ("a.7z", Some(SevenZ)),
            ("a.7Z", Some(SevenZ)),
            ("a.rar", Some(External)),
            ("a.iso", Some(External)),
            ("a.cab", Some(External)),
            ("a.xz", Some(External)),
            ("a.txz", Some(External)),
            ("a.tar.xz", Some(External)),
            ("a.lzh", Some(External)),
            ("a.lha", Some(External)),
            ("a.arj", Some(External)),
            ("a.wim", Some(External)),
            ("a.zst", Some(External)),
            ("a.tar.zst", Some(External)),
            // zips on disk, but documents / apps
            ("report.docx", None),
            ("book.xlsx", None),
            ("deck.pptx", None),
            ("doc.odt", None),
            ("app.jar", None),
            ("app.apk", None),
            ("DOC.DOCX", None),
            // not archives
            ("a.txt", None),
            ("zip", None),
            ("a.zip.txt", None),
            ("a.gzip", None),
            ("", None),
            ("photo.tar.gz.jpg", None),
        ];
        for (name, want) in table {
            assert_eq!(archive_kind(name), *want, "archive_kind({name:?})");
        }
        // a path is judged by its last component
        assert_eq!(archive_kind("C:\\x\\y.tar.gz"), Some(TarGz));
        assert_eq!(archive_kind("/x/y.docx"), None);
    }

    // ---- 2. dest_for -------------------------------------------------------------------------------------------------

    #[test]
    fn dest_for_plain_and_collisions() {
        let tmp = tempdir().unwrap();
        let d = tmp.path();
        assert_eq!(dest_for(&d.join("photos.tar.gz")), d.join("photos (extracted)"));
        assert_eq!(dest_for(&d.join("Photos.TGZ")), d.join("Photos (extracted)"));
        assert_eq!(dest_for(&d.join("a.b.zip")), d.join("a.b (extracted)"));
        assert_eq!(dest_for(&d.join("notes.txt.gz")), d.join("notes.txt (extracted)"));

        fs::create_dir(d.join("x (extracted)")).unwrap();
        assert_eq!(dest_for(&d.join("x.zip")), d.join("x (extracted 2)"));
        fs::create_dir(d.join("x (extracted 2)")).unwrap();
        // a plain file in the way counts too
        fs::write(d.join("x (extracted 3)"), b"").unwrap();
        assert_eq!(dest_for(&d.join("x.7z")), d.join("x (extracted 4)"));
    }

    // ---- 3. zip ------------------------------------------------------------------------------------------------------

    #[test]
    fn zip_round_trip_with_folders_and_mtime() {
        let tmp = tempdir().unwrap();
        let when = zip::DateTime::from_date_and_time(2020, 5, 17, 12, 30, 10).unwrap();
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        w.add_directory("empty/", zip_opts()).unwrap();
        for (name, data) in [("top.txt", "top"), ("a/b/c.txt", "deep"), ("a/d.txt", "mid")] {
            w.start_file(name, zip_opts().last_modified_time(when)).unwrap();
            w.write_all(data.as_bytes()).unwrap();
        }
        let zip_path = tmp.path().join("t.zip");
        fs::write(&zip_path, w.finish().unwrap().into_inner()).unwrap();

        let dest = tmp.path().join("out");
        let cancel = AtomicBool::new(false);
        let mut seen = Vec::new();
        let o = extract_recursive(&zip_path, &dest, &opts(&cancel), &mut |n, b| seen.push((n.to_string(), b)));

        assert_eq!(o.error, None);
        assert_eq!(o.files, 3);
        assert_eq!(o.bytes, 3 + 4 + 3);
        assert_eq!(o.nested, 0);
        assert!(o.skipped.is_empty(), "{:?}", o.skipped);
        assert_eq!(fs::read_to_string(dest.join("top.txt")).unwrap(), "top");
        assert_eq!(fs::read_to_string(dest.join("a/b/c.txt")).unwrap(), "deep");
        assert_eq!(fs::read_to_string(dest.join("a/d.txt")).unwrap(), "mid");
        assert!(dest.join("empty").is_dir());
        // 2020-05-17 12:30:10 (taken as UTC) = 1589718610
        assert!((mtime_secs(&dest.join("a/b/c.txt")) - 1_589_718_610).abs() <= 2);
        // progress at least once per entry
        for name in ["empty/", "top.txt", "a/b/c.txt", "a/d.txt"] {
            assert!(seen.iter().any(|(n, _)| n == name), "no progress call for {name}: {seen:?}");
        }
        assert_eq!(seen.last().unwrap().1, o.bytes);
    }

    #[test]
    fn zip_unicode_and_odd_but_safe_names() {
        let tmp = tempdir().unwrap();
        let zip_path = tmp.path().join("u.zip");
        fs::write(
            &zip_path,
            zip_bytes(&[("./dot/ok.txt", b"1"), ("caf\u{e9}/\u{4e2d}\u{6587}.txt", b"2"), ("win\\style\\f.txt", b"3")]),
        )
        .unwrap();
        let dest = tmp.path().join("out");
        let cancel = AtomicBool::new(false);
        let o = run(&zip_path, &dest, &opts(&cancel));
        assert_eq!(o.error, None);
        assert_eq!(o.files, 3);
        assert!(dest.join("dot/ok.txt").is_file());
        assert!(dest.join("caf\u{e9}/\u{4e2d}\u{6587}.txt").is_file());
        assert!(dest.join("win/style/f.txt").is_file());
    }

    // ---- 4. tar.gz / tar.bz2 / tar ----------------------------------------------------------------------------------

    #[test]
    fn tar_gz_and_bz2_and_plain_round_trip() {
        let tmp = tempdir().unwrap();
        let tar = tar_bytes(&[("one.txt", b"first"), ("sub/dir/two.txt", b"second")], 1_600_000_000);
        let cancel = AtomicBool::new(false);
        for (name, bytes) in [
            ("p.tar.gz", gz(&tar)),
            ("p.tgz", gz(&tar)),
            ("p.tar.bz2", bz2(&tar)),
            ("p.tbz2", bz2(&tar)),
            ("p.tar", tar.clone()),
        ] {
            let path = tmp.path().join(name);
            fs::write(&path, &bytes).unwrap();
            let dest = dest_for(&path);
            let o = run(&path, &dest, &opts(&cancel));
            assert_eq!(o.error, None, "{name}");
            assert_eq!(o.files, 2, "{name}");
            assert_eq!(o.bytes, 5 + 6, "{name}");
            assert_eq!(fs::read_to_string(dest.join("one.txt")).unwrap(), "first", "{name}");
            assert_eq!(fs::read_to_string(dest.join("sub/dir/two.txt")).unwrap(), "second", "{name}");
            assert!((mtime_secs(&dest.join("one.txt")) - 1_600_000_000).abs() <= 2, "{name}");
        }
    }

    #[test]
    fn tar_symlinks_and_unsafe_names_are_skipped() {
        let tmp = tempdir().unwrap();
        let mut b = tar::Builder::new(Vec::new());
        // symlink to somewhere outside
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        h.set_link_name("/etc").unwrap();
        h.set_cksum();
        b.append_data(&mut h, "link", std::io::empty()).unwrap();
        // raw names that tar::Header::set_path refuses
        for raw in ["../evil.txt", "/abs.txt", "ok/../../evil2.txt"] {
            let mut h = tar::Header::new_gnu();
            h.set_size(1);
            h.set_mode(0o644);
            h.as_old_mut().name[..raw.len()].copy_from_slice(raw.as_bytes());
            h.set_cksum();
            b.append(&h, &b"x"[..]).unwrap();
        }
        let mut h = tar::Header::new_gnu();
        h.set_size(2);
        h.set_cksum();
        b.append_data(&mut h, "good.txt", &b"ok"[..]).unwrap();
        let path = tmp.path().join("bad.tar");
        fs::write(&path, b.into_inner().unwrap()).unwrap();

        let dest = tmp.path().join("work").join("out");
        let cancel = AtomicBool::new(false);
        let o = run(&path, &dest, &opts(&cancel));
        assert_eq!(o.error, None);
        assert_eq!(o.files, 1);
        assert_eq!(o.skipped.len(), 4, "{:?}", o.skipped);
        assert!(o.skipped.iter().any(|s| s.starts_with("link:") && s.contains("symlink")));
        assert!(o.skipped.iter().any(|s| s.starts_with("../evil.txt")));
        assert!(o.skipped.iter().any(|s| s.starts_with("/abs.txt")));
        assert_eq!(all_files(tmp.path()).len(), 2, "only the tar and good.txt may exist");
        assert!(dest.join("good.txt").is_file());
        assert!(!dest.join("link").exists());
    }

    // ---- 5. gz / bz2 single file ------------------------------------------------------------------------------------

    #[test]
    fn gz_and_bz2_single_file() {
        let tmp = tempdir().unwrap();
        let cancel = AtomicBool::new(false);

        let g = tmp.path().join("hello.txt.gz");
        fs::write(&g, gz(b"hello gz")).unwrap();
        let dest = dest_for(&g);
        let o = run(&g, &dest, &opts(&cancel));
        assert_eq!(o.error, None);
        assert_eq!((o.files, o.bytes), (1, 8));
        assert_eq!(fs::read_to_string(dest.join("hello.txt")).unwrap(), "hello gz");

        let b = tmp.path().join("DATA.CSV.BZ2");
        fs::write(&b, bz2(b"a,b\n1,2\n")).unwrap();
        let dest = dest_for(&b);
        let o = run(&b, &dest, &opts(&cancel));
        assert_eq!(o.error, None);
        assert_eq!(fs::read_to_string(dest.join("DATA.CSV")).unwrap(), "a,b\n1,2\n");

        // concatenated gzip members are read to the end
        let multi = tmp.path().join("multi.txt.gz");
        let mut two = gz(b"AAA");
        two.extend(gz(b"BBB"));
        fs::write(&multi, two).unwrap();
        let dest = dest_for(&multi);
        let o = run(&multi, &dest, &opts(&cancel));
        assert_eq!(o.error, None);
        assert_eq!(fs::read_to_string(dest.join("multi.txt")).unwrap(), "AAABBB");
    }

    // ---- 6. 7z (built with sevenz-rust2's own compressor) -----------------------------------------------------------

    fn make_7z_source(root: &Path) -> PathBuf {
        let src = root.join("src");
        fs::create_dir_all(src.join("sub/deeper")).unwrap();
        fs::write(src.join("a.txt"), b"alpha").unwrap();
        fs::write(src.join("sub/b.txt"), b"bravo!").unwrap();
        fs::write(src.join("sub/deeper/c.txt"), b"charlie").unwrap();
        fs::create_dir_all(src.join("emptydir")).unwrap();
        fs::write(src.join("empty.bin"), b"").unwrap();
        src
    }

    #[test]
    fn sevenz_round_trip() {
        let tmp = tempdir().unwrap();
        let src = make_7z_source(tmp.path());
        let arc = tmp.path().join("pack.7z");
        sevenz_rust2::compress_to_path(&src, &arc).unwrap();

        let dest = tmp.path().join("out");
        let cancel = AtomicBool::new(false);
        let o = run(&arc, &dest, &opts(&cancel));
        assert_eq!(o.error, None, "{:?}", o.skipped);
        assert!(o.skipped.is_empty(), "{:?}", o.skipped);
        assert_eq!(o.files, 4, "{:?}", all_files(&dest));
        assert_eq!(o.bytes, 5 + 6 + 7);
        let find = |name: &str| all_files(&dest).into_iter().find(|p| p.file_name().unwrap() == name).unwrap();
        assert_eq!(fs::read_to_string(find("a.txt")).unwrap(), "alpha");
        assert_eq!(fs::read_to_string(find("b.txt")).unwrap(), "bravo!");
        assert_eq!(fs::read_to_string(find("c.txt")).unwrap(), "charlie");
        assert_eq!(fs::metadata(find("empty.bin")).unwrap().len(), 0);
        assert!(find("c.txt").parent().unwrap().ends_with("deeper"));
    }

    #[test]
    fn sevenz_made_by_the_7z_cli_solid_with_times() {
        let Some(exe) = seven() else { return };
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("tree");
        fs::create_dir_all(src.join("docs/inner")).unwrap();
        fs::write(src.join("docs/one.txt"), "one ".repeat(1000)).unwrap();
        fs::write(src.join("docs/inner/two.txt"), "two ".repeat(1000)).unwrap();
        fs::write(src.join("three.bin"), junk(30_000)).unwrap();
        fs::write(src.join("zero.txt"), b"").unwrap();
        fs::create_dir_all(src.join("emptydir")).unwrap();
        let stamp = UNIX_EPOCH + Duration::from_secs(1_500_000_000);
        File::options().write(true).open(src.join("docs/one.txt")).unwrap().set_modified(stamp).unwrap();
        run_7z(&exe, tmp.path(), &["a", "-bd", "-ms=on", "-mx=3", "cli.7z", "tree"]);

        let dest = tmp.path().join("out");
        let cancel = AtomicBool::new(false);
        let o = run(&tmp.path().join("cli.7z"), &dest, &opts(&cancel));
        assert_eq!(o.error, None, "{:?}", o.skipped);
        assert!(o.skipped.is_empty(), "{:?}", o.skipped);
        assert_eq!(o.files, 4);
        assert_eq!(fs::read_to_string(dest.join("tree/docs/one.txt")).unwrap(), "one ".repeat(1000));
        assert_eq!(fs::read_to_string(dest.join("tree/docs/inner/two.txt")).unwrap(), "two ".repeat(1000));
        assert_eq!(fs::read(dest.join("tree/three.bin")).unwrap(), junk(30_000));
        assert_eq!(fs::metadata(dest.join("tree/zero.txt")).unwrap().len(), 0);
        assert!(dest.join("tree/emptydir").is_dir());
        assert!((mtime_secs(&dest.join("tree/docs/one.txt")) - 1_500_000_000).abs() <= 2);
    }

    #[test]
    fn sevenz_header_encrypted_by_the_cli_is_skipped() {
        let Some(exe) = seven() else { return };
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("s.txt"), b"secret").unwrap();
        run_7z(&exe, tmp.path(), &["a", "-psecret", "-mhe=on", "-bd", "hdr.7z", "s.txt"]);
        let dest = tmp.path().join("out");
        let cancel = AtomicBool::new(false);
        let o = run(&tmp.path().join("hdr.7z"), &dest, &opts(&cancel));
        assert_eq!(o.error, None);
        assert_eq!(o.files, 0);
        assert_eq!(o.skipped, vec!["hdr.7z: password protected".to_string()]);
    }

    #[test]
    fn sevenz_encrypted_is_skipped_without_hanging() {
        let tmp = tempdir().unwrap();
        let src = make_7z_source(tmp.path());
        let arc = tmp.path().join("secret.7z");
        sevenz_rust2::compress_to_path_encrypted(&src, &arc, sevenz_rust2::Password::from("pw")).unwrap();
        let dest = tmp.path().join("out");
        let cancel = AtomicBool::new(false);
        let o = run(&arc, &dest, &opts(&cancel));
        assert_eq!(o.error, None);
        assert_eq!(o.files, 0);
        assert!(o.skipped.iter().any(|s| s.contains("password protected")), "{:?}", o.skipped);
    }

    // ---- 7. External via the 7z command line ------------------------------------------------------------------------

    #[test]
    fn external_xz_and_tar_xz_with_nested_tar() {
        let Some(exe) = seven() else { return };
        let tmp = tempdir().unwrap();
        let cancel = AtomicBool::new(false);

        // plain .xz
        fs::write(tmp.path().join("data.txt"), b"xz payload").unwrap();
        run_7z(&exe, tmp.path(), &["a", "-txz", "-bd", "data.txt.xz", "data.txt"]);
        let xz = tmp.path().join("data.txt.xz");
        let dest = dest_for(&xz);
        let o = run(&xz, &dest, &opts(&cancel));
        assert_eq!(o.error, None);
        assert_eq!(o.files, 1);
        assert_eq!(o.bytes, 10);
        assert_eq!(fs::read_to_string(dest.join("data.txt")).unwrap(), "xz payload");

        // .tar.xz: 7z leaves the .tar, the unblob step opens it
        fs::write(tmp.path().join("inner.tar"), tar_bytes(&[("d/x.txt", b"in tar")], 1_600_000_000)).unwrap();
        run_7z(&exe, tmp.path(), &["a", "-txz", "-bd", "pack.tar.xz", "inner.tar"]);
        let txz = tmp.path().join("pack.tar.xz");
        let dest = dest_for(&txz);
        let o = run(&txz, &dest, &opts(&cancel));
        assert_eq!(o.error, None, "{:?}", o.skipped);
        assert_eq!(o.nested, 1);
        // an .xz carries no file name: 7-Zip names the output after the archive ("pack.tar")
        assert_eq!(fs::read_to_string(dest.join("pack (extracted)/d/x.txt")).unwrap(), "in tar");
    }

    #[test]
    fn external_password_protected_is_skipped_not_hung() {
        let Some(exe) = seven() else { return };
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("s.txt"), b"secret").unwrap();
        run_7z(&exe, tmp.path(), &["a", "-psecret", "-tzip", "-bd", "enc.zip", "s.txt"]);
        // 7-Zip goes by content, so an encrypted zip with a .cab name still takes the External path
        fs::rename(tmp.path().join("enc.zip"), tmp.path().join("enc.cab")).unwrap();
        let arc = tmp.path().join("enc.cab");
        let dest = tmp.path().join("out");
        let cancel = AtomicBool::new(false);
        let o = run(&arc, &dest, &opts(&cancel));
        assert_eq!(o.error, None, "{:?}", o.skipped);
        assert!(o.skipped.iter().any(|s| s.contains("password protected")), "{:?}", o.skipped);
    }

    #[test]
    fn external_without_sevenzip_is_a_clear_error() {
        let tmp = tempdir().unwrap();
        let arc = tmp.path().join("x.rar");
        fs::write(&arc, b"Rar!").unwrap();
        let cancel = AtomicBool::new(false);
        let mut o = opts(&cancel);
        o.sevenzip = None;
        let dest = tmp.path().join("out");
        let out = run(&arc, &dest, &o);
        assert_eq!(out.error.as_deref(), Some("Needs 7-Zip (install from 7-zip.org)"));
        assert!(!dest.exists(), "nothing is created when 7-Zip is missing");
    }

    #[test]
    fn external_bomb_is_stopped() {
        let Some(exe) = seven() else { return };
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("zeros.bin"), vec![0u8; 30 << 20]).unwrap();
        run_7z(&exe, tmp.path(), &["a", "-txz", "-bd", "zeros.bin.xz", "zeros.bin"]);
        let arc = tmp.path().join("zeros.bin.xz");
        let dest = tmp.path().join("out");
        let cancel = AtomicBool::new(false);
        let mut o = opts(&cancel);
        o.max_ratio = 20;
        let out = run(&arc, &dest, &o);
        assert!(out.error.as_deref().unwrap_or("").contains("stopped"), "{:?}", out.error);
    }

    // ---- 8. zip-slip -------------------------------------------------------------------------------------------------

    #[test]
    fn zip_slip_names_are_skipped_and_nothing_escapes() {
        let tmp = tempdir().unwrap();
        let zip_path = tmp.path().join("evil.zip");
        fs::write(
            &zip_path,
            zip_bytes(&[
                ("../evil.txt", b"x"),
                ("/abs.txt", b"x"),
                ("a/../../up.txt", b"x"),
                ("..\\back.txt", b"x"),
                ("C:\\win.txt", b"x"),
                ("c:rel.txt", b"x"),
                ("\\\\server\\share\\unc.txt", b"x"),
                ("//server/share/unc2.txt", b"x"),
                ("good/ok.txt", b"fine"),
                ("../", b""),
            ]),
        )
        .unwrap();

        let dest = tmp.path().join("work").join("out");
        let cancel = AtomicBool::new(false);
        let o = run(&zip_path, &dest, &opts(&cancel));
        assert_eq!(o.error, None);
        assert_eq!(o.files, 1);
        for bad in [
            "../evil.txt",
            "/abs.txt",
            "a/../../up.txt",
            "..\\back.txt",
            "C:\\win.txt",
            "c:rel.txt",
            "\\\\server\\share\\unc.txt",
            "//server/share/unc2.txt",
        ] {
            assert!(o.skipped.iter().any(|s| s.starts_with(&format!("{bad}:"))), "{bad} not reported: {:?}", o.skipped);
        }
        // the only files anywhere: the zip itself and the one good entry
        let files = all_files(tmp.path());
        let mut expect = vec![dest.join("good/ok.txt"), zip_path.clone()];
        expect.sort();
        assert_eq!(files, expect);
        assert!(!Path::new("/abs.txt").exists());
    }

    #[test]
    fn safe_relative_table() {
        let ok = |s: &str| safe_relative(s).unwrap().map(|p| p.to_string_lossy().replace('\\', "/"));
        assert_eq!(ok("a/b.txt"), Some("a/b.txt".into()));
        assert_eq!(ok("./a//b.txt"), Some("a/b.txt".into()));
        assert_eq!(ok("a\\b\\c.txt"), Some("a/b/c.txt".into()));
        assert_eq!(ok("dir/"), Some("dir".into()));
        assert_eq!(ok(""), None);
        assert_eq!(ok("./"), None);
        assert_eq!(ok("..a/b..c/d.e."), Some("..a/b..c/d.e.".into()));
        for bad in [
            "..", "../x", "x/..", "x/../y", "..\\x", "/x", "\\x", "//srv/s/x", "\\\\srv\\s\\x", "C:x", "c:\\x", "Z:/x",
            "a\0b", "...", "a/ . /b", "a/.. /b",
        ] {
            assert!(safe_relative(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    // ---- 9. zip bomb -------------------------------------------------------------------------------------------------

    fn bomb_zip(path: &Path, megabytes: usize) {
        let mut w = zip::ZipWriter::new(File::create(path).unwrap());
        w.start_file("zeros.bin", zip_opts()).unwrap();
        let chunk = vec![0u8; 1 << 20];
        for _ in 0..megabytes {
            w.write_all(&chunk).unwrap();
        }
        w.finish().unwrap();
    }

    #[test]
    fn zip_bomb_is_stopped_by_ratio() {
        let tmp = tempdir().unwrap();
        let zip_path = tmp.path().join("bomb.zip");
        bomb_zip(&zip_path, 50);
        let size = fs::metadata(&zip_path).unwrap().len();
        assert!(size < 200_000, "a zero-filled zip should be tiny, got {size}");

        let dest = tmp.path().join("out");
        let cancel = AtomicBool::new(false);
        let mut o = opts(&cancel);
        o.max_ratio = 20;
        let out = run(&zip_path, &dest, &o);
        let err = out.error.clone().unwrap_or_default();
        assert!(err.contains("stopped"), "{err}");
        assert_eq!(err, "stopped: extracted size passed 20 x the archive size");
        let limit = size * 20;
        assert!(out.bytes <= limit + BUF_SIZE as u64, "wrote {} > limit {limit}", out.bytes);
        assert!(out.bytes > 0, "partial output stays");
        assert_eq!(fs::metadata(dest.join("zeros.bin")).unwrap().len(), out.bytes, "partial file is kept");
    }

    #[test]
    fn total_byte_ceiling_is_enforced_while_streaming() {
        let tmp = tempdir().unwrap();
        let zip_path = tmp.path().join("bomb.zip");
        bomb_zip(&zip_path, 8);
        let dest = tmp.path().join("out");
        let cancel = AtomicBool::new(false);
        let mut o = opts(&cancel);
        o.max_total_bytes = 3 * (1 << 20) + 17;
        let out = run(&zip_path, &dest, &o);
        assert_eq!(out.error.as_deref(), Some("stopped: would pass the free-space limit"));
        assert_eq!(out.bytes, o.max_total_bytes);
    }

    #[test]
    fn budget_is_shared_with_nested_archives() {
        let tmp = tempdir().unwrap();
        // outer.zip holds inner.zip which holds 4 MB of zeros; the ceiling is smaller than the sum
        let inner = tmp.path().join("inner.zip");
        bomb_zip(&inner, 4);
        let inner_len = fs::metadata(&inner).unwrap().len();
        let outer = tmp.path().join("outer.zip");
        fs::write(&outer, zip_bytes(&[("inner.zip", &fs::read(&inner).unwrap())])).unwrap();

        let dest = tmp.path().join("out");
        let cancel = AtomicBool::new(false);
        let mut o = opts(&cancel);
        o.max_total_bytes = inner_len + (1 << 20);
        o.max_ratio = 1 << 30; // only the byte ceiling may bind here
        let out = run(&outer, &dest, &o);
        assert_eq!(out.error.as_deref(), Some("stopped: would pass the free-space limit"));
        assert_eq!(out.bytes, o.max_total_bytes);
    }

    // ---- 10. nested ----------------------------------------------------------------------------------------------------

    fn nested_zip(path: &Path) {
        let inner = zip_bytes(&[("a.txt", b"deep text")]);
        fs::write(path, zip_bytes(&[("inner.zip", &inner), ("note.txt", b"n")])).unwrap();
    }

    #[test]
    fn nested_zip_is_opened_up_to_max_depth() {
        let tmp = tempdir().unwrap();
        let outer = tmp.path().join("outer.zip");
        nested_zip(&outer);
        let cancel = AtomicBool::new(false);

        let dest2 = tmp.path().join("depth2");
        let mut o = opts(&cancel);
        o.max_depth = 2;
        let out = run(&outer, &dest2, &o);
        assert_eq!(out.error, None, "{:?}", out.skipped);
        assert_eq!(out.nested, 1);
        assert_eq!(fs::read_to_string(dest2.join("inner (extracted)/a.txt")).unwrap(), "deep text");
        assert!(dest2.join("inner.zip").is_file(), "the nested archive itself is kept");
        assert_eq!(out.files, 2 + 1, "inner.zip + note.txt + a.txt");
        assert_eq!(out.bytes, fs::metadata(dest2.join("inner.zip")).unwrap().len() + 1 + 9);

        let dest1 = tmp.path().join("depth1");
        o.max_depth = 1;
        let out = run(&outer, &dest1, &o);
        assert_eq!(out.error, None);
        assert_eq!(out.nested, 0);
        assert!(dest1.join("inner.zip").is_file());
        assert!(!dest1.join("inner (extracted)").exists());
    }

    #[test]
    fn nested_chain_mixed_formats_and_documents_left_alone() {
        let tmp = tempdir().unwrap();
        // zip -> tar.gz -> gz -> text ; a .docx (zip on disk) and a .jar must stay closed
        let leaf = gz(b"the leaf");
        let tgz = gz(&tar_bytes(&[("deeper/leaf.txt.gz", &leaf)], 1_500_000_000));
        let docx = zip_bytes(&[("word/document.xml", b"<x/>")]);
        let outer = tmp.path().join("outer.zip");
        fs::write(
            &outer,
            zip_bytes(&[("pack.tar.gz", &tgz), ("report.DOCX", &docx), ("lib.jar", &docx), ("keep/pack.tar.gz", &tgz)]),
        )
        .unwrap();
        let cancel = AtomicBool::new(false);

        let dest = tmp.path().join("out");
        let mut o = opts(&cancel);
        o.max_depth = 5;
        let out = run(&outer, &dest, &o);
        assert_eq!(out.error, None, "{:?}", out.skipped);
        // two tar.gz + their two leaf.txt.gz
        assert_eq!(out.nested, 4);
        assert_eq!(
            fs::read_to_string(dest.join("pack (extracted)/deeper/leaf.txt (extracted)/leaf.txt")).unwrap(),
            "the leaf"
        );
        assert_eq!(
            fs::read_to_string(dest.join("keep/pack (extracted)/deeper/leaf.txt (extracted)/leaf.txt")).unwrap(),
            "the leaf"
        );
        assert!(!dest.join("report (extracted)").exists());
        assert!(!dest.join("lib (extracted)").exists());

        // depth 3 reaches the tar.gz contents but not the leaf gz
        let dest3 = tmp.path().join("out3");
        o.max_depth = 3;
        let out = run(&outer, &dest3, &o);
        assert_eq!(out.nested, 2 + 2);
        assert!(dest3.join("pack (extracted)/deeper/leaf.txt.gz").is_file());
        assert!(dest3.join("pack (extracted)/deeper/leaf.txt (extracted)/leaf.txt").is_file());
        let dest2 = tmp.path().join("out2");
        o.max_depth = 2;
        let out = run(&outer, &dest2, &o);
        assert_eq!(out.nested, 2);
        assert!(dest2.join("pack (extracted)/deeper/leaf.txt.gz").is_file());
        assert!(!dest2.join("pack (extracted)/deeper/leaf.txt (extracted)").exists());
    }

    #[test]
    fn nested_name_collision_gets_numbered_folder() {
        let tmp = tempdir().unwrap();
        let inner = zip_bytes(&[("f.txt", b"1")]);
        let outer = tmp.path().join("outer.zip");
        // a real folder already called "inner (extracted)" inside the same archive
        fs::write(&outer, zip_bytes(&[("inner.zip", &inner), ("inner (extracted)/keep.txt", b"k")])).unwrap();
        let cancel = AtomicBool::new(false);
        let dest = tmp.path().join("out");
        let out = run(&outer, &dest, &opts(&cancel));
        assert_eq!(out.error, None);
        assert_eq!(out.nested, 1);
        assert_eq!(fs::read_to_string(dest.join("inner (extracted)/keep.txt")).unwrap(), "k");
        assert_eq!(fs::read_to_string(dest.join("inner (extracted 2)/f.txt")).unwrap(), "1");
    }

    #[test]
    fn nested_corrupt_archive_is_reported_and_others_continue() {
        let tmp = tempdir().unwrap();
        let good = zip_bytes(&[("g.txt", b"good")]);
        let outer = tmp.path().join("outer.zip");
        fs::write(&outer, zip_bytes(&[("a_bad.zip", &junk(300)), ("b_good.zip", &good), ("c_bad.tar.gz", &junk(300))]))
            .unwrap();
        let cancel = AtomicBool::new(false);
        let dest = tmp.path().join("out");
        let out = run(&outer, &dest, &opts(&cancel));
        assert_eq!(out.error, None, "a damaged inner archive does not fail the outer one");
        assert_eq!(out.nested, 1);
        assert!(dest.join("b_good (extracted)/g.txt").is_file());
        assert_eq!(out.skipped.len(), 2, "{:?}", out.skipped);
        assert!(out.skipped.iter().any(|s| s.starts_with("a_bad.zip:") && s.contains("nested archive not opened")));
        assert!(out.skipped.iter().any(|s| s.starts_with("c_bad.tar.gz:")));
        assert!(!dest.join("a_bad (extracted)").exists(), "empty folders of failed archives are removed");
    }

    #[test]
    fn nested_external_without_sevenzip_is_skipped_not_fatal() {
        let tmp = tempdir().unwrap();
        let outer = tmp.path().join("outer.zip");
        fs::write(&outer, zip_bytes(&[("old.rar", b"Rar!"), ("t.txt", b"t")])).unwrap();
        let cancel = AtomicBool::new(false);
        let mut o = opts(&cancel);
        o.sevenzip = None;
        let out = run(&outer, &tmp.path().join("out"), &o);
        assert_eq!(out.error, None);
        assert_eq!(out.nested, 0);
        assert!(out.skipped.iter().any(|s| s.starts_with("old.rar:") && s.contains("Needs 7-Zip")), "{:?}", out.skipped);
    }

    // ---- 11. cancel --------------------------------------------------------------------------------------------------

    #[test]
    fn cancel_before_start() {
        let tmp = tempdir().unwrap();
        let outer = tmp.path().join("outer.zip");
        nested_zip(&outer);
        let cancel = AtomicBool::new(true);
        let dest = tmp.path().join("out");
        let out = run(&outer, &dest, &opts(&cancel));
        assert_eq!(out.error.as_deref(), Some("cancelled"));
        assert_eq!((out.files, out.bytes), (0, 0));
    }

    #[test]
    fn cancel_in_the_middle_of_a_big_entry() {
        let tmp = tempdir().unwrap();
        let zip_path = tmp.path().join("bomb.zip");
        bomb_zip(&zip_path, 50);
        let cancel = AtomicBool::new(false);
        let dest = tmp.path().join("out");
        let out = extract_recursive(&zip_path, &dest, &opts(&cancel), &mut |_, bytes| {
            if bytes >= (1 << 20) {
                cancel.store(true, Ordering::Relaxed);
            }
        });
        assert_eq!(out.error.as_deref(), Some("cancelled"));
        assert!(out.bytes >= (1 << 20) && out.bytes < (8 << 20), "stopped within a few MB, wrote {}", out.bytes);
    }

    #[test]
    fn cancel_between_nested_archives() {
        let tmp = tempdir().unwrap();
        let inner = zip_bytes(&[("f.txt", b"1")]);
        let outer = tmp.path().join("outer.zip");
        fs::write(&outer, zip_bytes(&[("one.zip", &inner), ("two.zip", &inner)])).unwrap();
        let cancel = AtomicBool::new(false);
        let dest = tmp.path().join("out");
        let out = extract_recursive(&outer, &dest, &opts(&cancel), &mut |name, _| {
            if name == "f.txt" {
                cancel.store(true, Ordering::Relaxed);
            }
        });
        assert_eq!(out.error.as_deref(), Some("cancelled"));
        assert!(!dest.join("two (extracted)/f.txt").exists());
    }

    // ---- 12. corrupt input ------------------------------------------------------------------------------------------

    #[test]
    fn corrupt_archives_report_an_error_and_never_panic() {
        let tmp = tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let noise = junk(4096);
        for name in
            ["bad.zip", "bad.tar", "bad.tar.gz", "bad.tgz", "bad.tar.bz2", "bad.gz", "bad.bz2", "bad.7z"]
        {
            let path = tmp.path().join(name);
            fs::write(&path, &noise).unwrap();
            let dest = tmp.path().join(format!("out-{name}"));
            let out = run(&path, &dest, &opts(&cancel));
            assert!(out.error.is_some(), "{name}: expected an error");
            assert_eq!((out.files, out.bytes), (0, 0), "{name}");
            assert_eq!(out.archive, path);
            assert_eq!(out.dest, dest);
        }
        // a missing file and an empty file
        let out = run(&tmp.path().join("nope.zip"), &tmp.path().join("o1"), &opts(&cancel));
        assert!(out.error.is_some());
        let empty = tmp.path().join("empty.zip");
        fs::write(&empty, b"").unwrap();
        assert!(run(&empty, &tmp.path().join("o2"), &opts(&cancel)).error.is_some());
        // not an archive name at all
        let txt = tmp.path().join("a.txt");
        fs::write(&txt, b"x").unwrap();
        assert!(run(&txt, &tmp.path().join("o3"), &opts(&cancel)).error.is_some());
    }

    #[test]
    fn truncated_archives_keep_partial_output_and_report_error() {
        let tmp = tempdir().unwrap();
        let cancel = AtomicBool::new(false);

        // zip cut in half: central directory gone
        let z = zip_bytes(&[("a.txt", b"first"), ("b.txt", &junk(5000))]);
        let zp = tmp.path().join("cut.zip");
        fs::write(&zp, &z[..z.len() / 2]).unwrap();
        assert!(run(&zp, &tmp.path().join("o1"), &opts(&cancel)).error.is_some());

        // tar.gz cut in half: the first entries are already on disk
        let big = junk(200_000);
        let tgz = gz(&tar_bytes(&[("first.bin", &big), ("second.bin", &big)], 1_600_000_000));
        let tp = tmp.path().join("cut.tar.gz");
        fs::write(&tp, &tgz[..tgz.len() * 3 / 4]).unwrap();
        let out = run(&tp, &tmp.path().join("o2"), &opts(&cancel));
        assert!(out.error.is_some());
        assert!(out.files >= 1 && out.bytes > 0, "partial counts are kept");
        assert!(tmp.path().join("o2/first.bin").is_file());

        // gz cut: output stays, error is set
        let gp = tmp.path().join("cut.bin.gz");
        let g = gz(&big);
        fs::write(&gp, &g[..g.len() / 2]).unwrap();
        let out = run(&gp, &tmp.path().join("o3"), &opts(&cancel));
        assert!(out.error.is_some());
    }

    // ---- encrypted zip -------------------------------------------------------------------------------------------------

    #[test]
    fn encrypted_zip_entry_is_skipped_plain_entry_is_extracted() {
        let Some(exe) = seven() else { return };
        let tmp = tempdir().unwrap();
        let zip_path = tmp.path().join("mixed.zip");
        fs::write(&zip_path, zip_bytes(&[("plain.txt", b"visible")])).unwrap();
        fs::write(tmp.path().join("secret.txt"), b"hidden").unwrap();
        // adds an encrypted (ZipCrypto) entry next to the plain one
        run_7z(&exe, tmp.path(), &["a", "-psecret", "-tzip", "-bd", "mixed.zip", "secret.txt"]);

        let dest = tmp.path().join("out");
        let cancel = AtomicBool::new(false);
        let out = run(&zip_path, &dest, &opts(&cancel));
        assert_eq!(out.error, None, "{:?}", out.skipped);
        assert_eq!(fs::read_to_string(dest.join("plain.txt")).unwrap(), "visible");
        assert!(!dest.join("secret.txt").exists());
        assert_eq!(out.skipped, vec!["secret.txt: password protected".to_string()]);
    }

    // ---- find_sevenzip -------------------------------------------------------------------------------------------------

    #[test]
    fn find_sevenzip_configured_and_path_search() {
        let tmp = tempdir().unwrap();
        let fake = tmp.path().join("my7z.exe");
        fs::write(&fake, b"").unwrap();
        assert_eq!(find_sevenzip(fake.to_str().unwrap()), Some(fake.clone()));
        assert_eq!(find_sevenzip(&format!("  {}  ", fake.display())), Some(fake.clone()));
        // a configured path that does not exist is ignored (falls back to the PATH search)
        let missing = tmp.path().join("nothing-here");
        assert_eq!(
            find_sevenzip(missing.to_str().unwrap()),
            find_sevenzip(""),
            "a bad configured path behaves like no configured path"
        );
        // a directory is not a file
        assert_ne!(find_sevenzip(tmp.path().to_str().unwrap()), Some(tmp.path().to_path_buf()));

        // PATH search, on a private PATH-style list
        let bin1 = tmp.path().join("bin1");
        let bin2 = tmp.path().join("bin2");
        fs::create_dir_all(&bin1).unwrap();
        fs::create_dir_all(&bin2).unwrap();
        let exe_name = if cfg!(windows) { "7z.exe" } else { "7z" };
        let tool = bin2.join(exe_name);
        fs::write(&tool, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let list = std::env::join_paths([&bin1, &bin2]).unwrap();
        assert_eq!(find_in_path(&list), Some(tool.clone()));
        let only_first = std::env::join_paths([&bin1]).unwrap();
        assert_eq!(find_in_path(&only_first), None);
        assert_eq!(find_in_path(std::ffi::OsStr::new("")), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tool, fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(find_in_path(&list), None, "a non-executable file is not a 7-Zip");
        }
    }

    #[test]
    fn reserved_windows_names() {
        for bad in ["CON", "con", "Nul.txt", "aux.tar.gz", "COM1", "lpt9.log", "com\u{b2}", "PRN ", "CONIN$", "CON.", "NUL.x.y"] {
            assert!(is_reserved_windows_name(bad), "{bad:?}");
        }
        for fine in ["CONSOLE", "COM10", "COM", "LPT", "COMX", "xcon", "con-1", "a.con", "NULL", "AUXILIARY", "", "COM1x"] {
            assert!(!is_reserved_windows_name(fine), "{fine:?}");
        }
    }

    #[test]
    fn dos_time_conversion_matches_known_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(2020, 5, 17) * 86_400 + 12 * 3600 + 30 * 60 + 10, 1_589_718_610);
        assert_eq!(days_from_civil(1980, 1, 1), 3_652);
        assert_eq!(filetime_to_system(116_444_736_000_000_000), Some(UNIX_EPOCH));
        assert_eq!(filetime_to_system(0), None);
        assert_eq!(filetime_to_system(u64::MAX).map(|_| ()).unwrap_or(()), ());
    }
}
