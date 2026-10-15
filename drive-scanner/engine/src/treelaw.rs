//! THE TREE LAW — which copy of a duplicate set is kept, and which copies may go.
//!
//! This is a port, rule for rule and word for word, of Avni Clean's removal gate:
//!   AEBrain a006d7f, software/Avni Copy/02_Source/AvniCopy/AvniClean.cs
//!   `MayBeRemoved`, `ScoreCopy`, `IsLooseInADump`, `WasPulledOutOfItsFolder`, `SitsBesideTheKeeper`,
//!   `TippedIntoAHeap`, `MustNeverRemove`, `IsInsideWorkingTree`, the 27-08-2026 guards and the
//!   29-08-2026 home rule. AvniClean.cs is the canonical text; if the two ever disagree, AvniClean wins
//!   and this file is corrected to match. Tests at the bottom replay AvniClean's own self-test cases.
//!
//! Ravi, 29-08-2026: "Tree rule stays no matter what. If there is a collision then tree rule is always followed."
//!
//! THE TREE LAW is the removal gate: a copy filed inside an organised tree is never the one removed; only a copy
//! lying loose may go (in a dumping ground, pulled out of its own folder, sitting beside the keeper, or tipped into a
//! flat heap). THE HOME RULE only chooses the keeper; a collision cancels his choice for that set. HOME CLEARS is the
//! one switch that can open the gate for filed copies, it is OFF unless he ticks it, and it is fenced four ways.
//!
//! One difference in mechanics, none in meaning: AvniClean asks the disk about each folder (how many files, any
//! subfolders, a marker file); this reads the same facts from the scan's index, which already holds every entry.
//! The 27-08 backup guard applies in AvniClean only to direct (permanent) delete; this app never deletes
//! permanently (Recycle Bin / quarantine), exactly like AvniClean's default mode, so it is not applied here either.

use crate::index::{Index, F_PLACEHOLDER};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The copy that stays.
    Keep,
    /// Loose copy: may go.
    Remove,
    /// A second copy the tree law (or an absolute guard) keeps.
    Protected,
}

#[derive(Clone, Debug)]
pub struct Decision {
    /// members[0] is the keeper.
    pub members: Vec<(u32, Role, String)>,
    pub home_used: bool,
    pub home_agreed: bool,
    pub home_overruled: bool,
}

// Folder names that mean "this is a pile, not a structure". (AvniClean: DumpFolder — scores a copy down.)
fn dump_folder() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(concat!(
            r"(?i)^(dump|dumps|temp|tmp|new folder.*|untitled.*|copy|copies|duplicate.*|misc|unsorted|",
            r"downloads?|desktop|recovered.*|restore.*|old|older|backup\d*|bak|archive\d*|",
            r"all[\s_-]?files.*|everything|final|latest|extra|junk|to[\s_-]?sort|inbox)$"
        ))
        .unwrap()
    })
}

// Folder names that are UNAMBIGUOUSLY dumping grounds — a licence to delete, so deliberately shorter: "Final",
// "Latest", "Old", "Backup", "Archive" are missing on purpose. (AvniClean: DumpFolderStrict.)
fn dump_folder_strict() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(concat!(
            r"(?i)^(dump|dumps|temp|tmp|new folder.*|untitled.*|copy|copies|duplicate.*|",
            r"misc|unsorted|junk|downloads?|recovered.*|to[\s_-]?sort|inbox|_inbox)$"
        ))
        .unwrap()
    })
}

// File names Windows/apps generate when something was copied. (AvniClean: CopyName.)
fn copy_name() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)(\s*[-(]\s*copy\s*[)]?\s*$|\s+copy(\s*\(\d+\))?$|\s*\(\d+\)$|_\d{1,3}$|\s*-\s*copy)").unwrap())
}

// Never touch anything here, whatever the user asked for. (AvniClean: SystemPaths.)
const SYSTEM_PATHS: &[&str] = &[
    r"\windows",
    r"\program files",
    r"\program files (x86)",
    r"\programdata\microsoft",
    r"\$recycle.bin",
    r"\system volume information",
    r"\appdata\local\microsoft\windows",
    r"\_avniclean_quarantine",
    r"\_avnicopy",
];

// Folders where a byte-identical twin is never a spare copy. (AvniClean: ProgramFolders, 27-08-2026.)
const PROGRAM_FOLDERS: &[&str] = &[
    r"\node_modules\", r"\site-packages\", r"\__pycache__\", r"\.venv\", r"\venv\",
    r"\.git\", r"\.hg\", r"\.svn\", r"\dotnet\", r"\.nuget\", r"\.cargo\",
    r"\.gradle\", r"\.npm\", r"\.pnpm\", r"\.yarn\", r"\.rustup\",
    r"\appdata\local\programs\", r"\appdata\local\python\", r"\pythoncore-",
    r"\appdata\roaming\npm\", r"\webview2\", r"\runtimes\", r"\platform-tools\",
    r"\.codex\", r"\.claude\", r"\.vscode\", r"\jdk", r"\jre",
    r"\bin\debug\", r"\bin\release\", r"\obj\debug\", r"\obj\release\",
];

// A file whose presence means the whole folder beneath it is a working tree. (AvniClean: TreeMarkerFiles.)
const TREE_MARKER_FILES: &[&str] = &[
    "pyvenv.cfg", "package.json", "go.mod", "cargo.toml", "requirements.txt",
    "pipfile", "pyproject.toml", "gemfile", "composer.json", "pom.xml", "build.gradle",
];

// A folder whose presence means the same. (AvniClean: TreeMarkerDirs.)
const TREE_MARKER_DIRS: &[&str] = &[".git", ".hg", ".svn", "node_modules"];

// Programs and libraries. A duplicate of one of these is NEVER surplus. (AvniClean: ProgramExtensions.)
const PROGRAM_EXTENSIONS: &[&str] = &[
    ".exe", ".dll", ".pyd", ".so", ".dylib", ".sys", ".drv", ".ocx", ".msi", ".msix",
    ".node", ".lib", ".a", ".jar", ".class", ".wasm", ".appx", ".cab",
];

// Where the working-tree walk STOPS: a marker in one of these is litter, not ownership. (AvniClean: WalkBoundaries.)
const WALK_BOUNDARIES: &[&str] = &[
    "appdata", "local", "locallow", "roaming", "temp", "tmp",
    "desktop", "documents", "downloads", "pictures", "music", "videos",
    "users", "public", "programdata",
];

/// Drop a file named .avniprotect in a folder and nothing inside it is ever removed.
pub const PROTECT_MARKER: &str = ".avniprotect";
/// Folders with no subfolders at all and at least this many files are heaps.
const BAG_FILE_COUNT: u32 = 25;
/// How much deeper the filed copy must be before this one counts as tipped.
const BAG_DEPTH_GAP: i64 = 2;

/// Facts about folders, read once from the index for a whole run.
pub struct TreeFacts<'a> {
    index: &'a Index,
    /// Files directly inside each folder (AvniClean: CountFiles).
    files_in: HashMap<u32, u32>,
    /// Folders that hold at least one folder.
    has_subdirs: HashSet<u32>,
    /// Folders holding a .avniprotect file.
    marker_protect: HashSet<u32>,
    /// Folders holding a working-tree marker (.git, package.json, *.csproj, ...).
    tree_marker: HashSet<u32>,
    /// Members of duplicate sets per folder (AvniClean: dupCountPerDir).
    dup_per_dir: HashMap<u32, u32>,
    /// The settings' protected list, as AvniClean's --protect: a path strictly INSIDE one of them.
    protect: Vec<String>,
    home: Vec<String>,
    home_clears: bool,
    now_ms: i64,
    tree_cache: std::cell::RefCell<HashMap<u32, bool>>,
}

/// Lowercased, with backslashes — AvniClean matches Windows paths, so every path is compared in that form.
fn win_lower(p: &str) -> String {
    p.replace('/', "\\").to_lowercase()
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// C#'s StringComparer.OrdinalIgnoreCase, closely enough for ordering two paths.
fn ordinal_ignore_case(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_uppercase().cmp(&b.to_uppercase())
}

fn stem(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    }
}

fn depth_of(path: &str) -> i64 {
    path.chars().filter(|&c| c == '\\' || c == '/').count() as i64
}

impl<'a> TreeFacts<'a> {
    /// `groups`: every duplicate set of the run (they all count towards "how dumpy is this folder").
    pub fn new(index: &'a Index, groups: &[Vec<u32>], protect: &[String], home: &[String], home_clears: bool) -> Self {
        let mut files_in: HashMap<u32, u32> = HashMap::new();
        let mut has_subdirs = HashSet::new();
        let mut marker_protect = HashSet::new();
        let mut tree_marker = HashSet::new();
        for (id, n) in index.nodes.iter().enumerate() {
            if n.gone() || n.parent as usize == id {
                continue;
            }
            let lname = n.name.to_lowercase();
            if n.is_dir() {
                has_subdirs.insert(n.parent);
                if TREE_MARKER_DIRS.contains(&lname.as_str()) {
                    tree_marker.insert(n.parent);
                }
            } else {
                *files_in.entry(n.parent).or_default() += 1;
                if lname == PROTECT_MARKER {
                    marker_protect.insert(n.parent);
                }
                if TREE_MARKER_FILES.contains(&lname.as_str()) || lname.ends_with(".csproj") || lname.ends_with(".sln") {
                    tree_marker.insert(n.parent);
                }
            }
        }
        let mut dup_per_dir: HashMap<u32, u32> = HashMap::new();
        for g in groups {
            for &m in g {
                *dup_per_dir.entry(index.nodes[m as usize].parent).or_default() += 1;
            }
        }
        // NormaliseHome: absolute, no trailing separator, no duplicates, order kept.
        let mut h: Vec<String> = Vec::new();
        for e in home {
            let t = e.trim().trim_end_matches(['\\', '/']).to_string();
            if !t.is_empty() && !h.iter().any(|x| x.eq_ignore_ascii_case(&t)) {
                h.push(t);
            }
        }
        TreeFacts {
            index,
            files_in,
            has_subdirs,
            marker_protect,
            tree_marker,
            dup_per_dir,
            protect: protect.iter().map(|p| win_lower(p.trim()).trim_end_matches('\\').to_string()).filter(|p| !p.is_empty()).collect(),
            home: h,
            home_clears,
            now_ms: crate::util::now_ms(),
            tree_cache: Default::default(),
        }
    }

    pub fn home(&self) -> &[String] {
        &self.home
    }

    fn files_here(&self, dir: u32) -> u32 {
        self.files_in.get(&dir).copied().unwrap_or(0)
    }

    fn is_system_path(&self, path: &str) -> bool {
        let lower = win_lower(path);
        SYSTEM_PATHS.iter().any(|s| lower.contains(s))
    }

    /// AvniClean IsProtected: inside a --protect folder, or below a .avniprotect marker. This app also treats its
    /// own "Protected" verdict (the protected list, AutoHotkey scripts) as protected.
    fn is_protected(&self, id: u32, path: &str) -> bool {
        if self.index.nodes[id as usize].verdict == crate::model::Verdict::Protected {
            return true;
        }
        let lower = win_lower(path);
        if self.protect.iter().any(|p| lower.starts_with(&format!("{p}\\"))) {
            return true;
        }
        let mut d = self.index.nodes[id as usize].parent;
        loop {
            if self.marker_protect.contains(&d) {
                return true;
            }
            let p = self.index.nodes[d as usize].parent;
            if p == d {
                return false;
            }
            d = p;
        }
    }

    fn is_program_folder(&self, path: &str) -> bool {
        let lower = win_lower(path);
        PROGRAM_FOLDERS.iter().any(|s| lower.contains(s))
    }

    fn is_program_file(&self, path: &str) -> bool {
        let lower = path.to_lowercase();
        PROGRAM_EXTENSIONS.iter().any(|e| {
            lower.ends_with(e) && {
                let name = lower.rsplit(['\\', '/']).next().unwrap_or("");
                name.len() > e.len() // ".exe" alone has no extension in .NET's sense
            }
        })
    }

    /// AvniClean IsWalkBoundary: a drive root, the user profile root C:\Users\<name>, or a container folder.
    fn is_walk_boundary(&self, dir: u32) -> bool {
        let n = &self.index.nodes[dir as usize];
        if n.parent == dir {
            return true;
        }
        let parent = &self.index.nodes[n.parent as usize];
        if parent.parent != n.parent {
            let gp = parent.parent;
            if self.index.nodes[gp as usize].parent == gp && parent.name.eq_ignore_ascii_case("Users") {
                return true;
            }
        }
        let leaf = n.name.to_lowercase();
        WALK_BOUNDARIES.contains(&leaf.as_str())
    }

    /// AvniClean IsInsideWorkingTree: walk up from the file's folder; stop BEFORE a container folder.
    fn is_inside_working_tree(&self, id: u32) -> bool {
        let mut dir = self.index.nodes[id as usize].parent;
        let mut walked = Vec::new();
        let mut guard = 0;
        let result = loop {
            if guard > 64 {
                break false;
            }
            guard += 1;
            if let Some(&c) = self.tree_cache.borrow().get(&dir) {
                break c;
            }
            walked.push(dir);
            if self.is_walk_boundary(dir) {
                break false;
            }
            if self.tree_marker.contains(&dir) {
                break true;
            }
            let up = self.index.nodes[dir as usize].parent;
            if up == dir {
                break false;
            }
            dir = up;
        };
        let mut cache = self.tree_cache.borrow_mut();
        for w in walked {
            cache.insert(w, result);
        }
        result
    }

    /// AvniClean MustNeverRemove (the 27-08-2026 guards). Some(reason) = must stay.
    fn must_never_remove(&self, id: u32, path: &str) -> Option<&'static str> {
        if self.is_program_file(path) {
            return Some("a program or library - identical bytes under a different name is still a different command");
        }
        if self.is_program_folder(path) {
            return Some("inside an installed program or package folder - the path is part of what it does");
        }
        if self.is_inside_working_tree(id) {
            return Some("inside a project or repository - a copy there is part of the project, not a spare");
        }
        None
    }

    /// AvniClean IsLooseInADump: folder named like a dump, copy-marker in the name, or a folder mostly made of
    /// duplicates (at least 4 files, 60% or more of them duplicated elsewhere).
    fn is_loose_in_a_dump(&self, id: u32) -> bool {
        let n = &self.index.nodes[id as usize];
        let dir = n.parent;
        if self.index.nodes[dir as usize].parent != dir && dump_folder_strict().is_match(&self.index.nodes[dir as usize].name) {
            return true;
        }
        if copy_name().is_match(stem(&n.name)) {
            return true;
        }
        let dups_here = self.dup_per_dir.get(&dir).copied().unwrap_or(0);
        let files_here = self.files_here(dir);
        files_here >= 4 && dups_here * 100 >= files_here * 60
    }

    /// Another copy of the same file is filed deeper inside THIS copy's own folder.
    fn was_pulled_out_of_its_folder(&self, c: u32, set: &[u32]) -> bool {
        let mine = self.index.dir_path(self.index.nodes[c as usize].parent).trim_end_matches(['\\', '/']).to_string();
        if mine.is_empty() {
            return false;
        }
        let mine_l = win_lower(&mine);
        let prefix = format!("{mine_l}\\");
        set.iter().filter(|&&o| o != c).any(|&o| {
            let theirs = win_lower(self.index.dir_path(self.index.nodes[o as usize].parent).trim_end_matches(['\\', '/']));
            theirs.len() > mine_l.len() && theirs.starts_with(&prefix)
        })
    }

    /// The copy being KEPT is in the very same folder as this one.
    fn sits_beside_the_keeper(&self, c: u32, keeper: u32) -> bool {
        self.index.nodes[c as usize].parent == self.index.nodes[keeper as usize].parent
    }

    /// A flat heap (no subfolders, 25+ files) while another copy is filed at least two levels deeper.
    fn tipped_into_a_heap(&self, c: u32, set: &[u32]) -> bool {
        let dir = self.index.nodes[c as usize].parent;
        if self.has_subdirs.contains(&dir) || self.files_here(dir) < BAG_FILE_COUNT {
            return false;
        }
        let mine = depth_of(&self.index.path_of(c));
        set.iter().filter(|&&o| o != c).any(|&o| depth_of(&self.index.path_of(o)) - mine >= BAG_DEPTH_GAP)
    }

    fn home_rank(&self, path: &str) -> usize {
        let p = path.replace('/', "\\");
        for (i, h) in self.home.iter().enumerate() {
            let h = h.replace('/', "\\");
            if p.len() > h.len() && p.as_bytes()[h.len()] == b'\\' && p[..h.len()].eq_ignore_ascii_case(&h) {
                return i;
            }
        }
        self.home.len()
    }

    /// AvniClean MayBeRemoved — every guard in one place. Ok(why) = may go; Err(why) = must stay.
    fn may_be_removed(&self, c: u32, set: &[u32], keeper: u32) -> Result<String, String> {
        let path = self.index.path_of(c);
        // These three are absolute. THE HOME LIST DOES NOT LIFT THEM, and nor does HOME CLEARS.
        if self.is_system_path(&path) {
            return Err("system location - never touched".into());
        }
        if self.is_protected(c, &path) {
            return Err("inside a protected folder - kept".into());
        }
        if let Some(why) = self.must_never_remove(c, &path) {
            return Err(why.into());
        }
        // THE TREE LAW - the last gate, and the strictest one.
        let in_dump = self.is_loose_in_a_dump(c);
        let pulled_out = self.was_pulled_out_of_its_folder(c, set);
        let beside_keeper = self.sits_beside_the_keeper(c, keeper);
        let tipped = self.tipped_into_a_heap(c, set);
        if !in_dump && !pulled_out && !beside_keeper && !tipped {
            // HOME CLEARS - the only clause that can open this gate, off unless he asked for it, fenced 4 ways.
            let keeper_path = self.index.path_of(keeper);
            let kr = self.home_rank(&keeper_path);
            if self.home_clears && !self.home.is_empty() && kr < self.home.len() && self.home_rank(&path) >= self.home.len() && !self.is_loose_in_a_dump(keeper) {
                return Ok(format!("outside your home - the copy you keep is filed in {}", self.home[kr]));
            }
            return Err("inside an organised tree - a filed copy is never the one removed".into());
        }
        Ok(if pulled_out && !in_dump {
            "pulled out of its own folder - the filed copy is deeper inside".into()
        } else if beside_keeper && !in_dump && !pulled_out {
            "a second copy in the very same folder - the survivor stays right here".into()
        } else if tipped {
            "tipped into a flat heap - the filed copy sits deeper inside a real structure".into()
        } else {
            String::new()
        })
    }

    /// AvniClean ScoreCopy: how much does this particular copy deserve to survive?
    fn score(&self, c: u32) -> (i64, Vec<String>) {
        let n = &self.index.nodes[c as usize];
        let path = self.index.path_of(c);
        let dir = n.parent;
        let leaf_dir = if self.index.nodes[dir as usize].parent == dir { "" } else { &self.index.nodes[dir as usize].name };
        let name = stem(&n.name);
        let mut why = Vec::new();
        let mut score: i64 = 0;
        if self.is_protected(c, &path) {
            score += 10000;
            why.push("in a protected folder".to_string());
        }
        if self.is_system_path(&path) {
            score += 9000;
            why.push("system location".to_string());
        }
        // 1. Depth = filing.
        let depth = depth_of(&path);
        score += depth * 40;
        if !self.has_subdirs.contains(&dir) && self.files_here(dir) >= BAG_FILE_COUNT {
            score -= 600;
            why.push(format!("in a flat heap of {} files", self.files_here(dir)));
        }
        why.push(format!("{depth} folders deep"));
        // 2. Is the folder it sits in a pile of duplicates?
        let dups_here = self.dup_per_dir.get(&dir).copied().unwrap_or(0);
        let files_here = self.files_here(dir);
        if files_here > 0 && dups_here >= 5 {
            let share = dups_here as f64 / files_here as f64;
            if share > 0.6 {
                score -= 500;
                why.push(format!("folder is {:.0}% duplicates - looks like a dump", share * 100.0));
            }
        }
        // 3. Folder named like a dump.
        if dump_folder().is_match(leaf_dir) {
            score -= 400;
            why.push(format!("folder named '{leaf_dir}'"));
        }
        // 4. File name that Windows generates for a copy.
        if copy_name().is_match(name) {
            score -= 300;
            why.push("filename looks like a copy".to_string());
        }
        // 5. The older file is usually the original that things point at.
        let days = (self.now_ms - n.modified) as f64 / 86_400_000.0;
        score += (days / 30.0).clamp(0.0, 60.0) as i64;
        // 6. A longer, more descriptive name usually means it was filed deliberately.
        score += (utf16_len(name).min(40) / 4) as i64;
        (score, why)
    }

    /// Decides one duplicate set: the keeper, and for every other copy whether it may go and why.
    pub fn decide(&self, set: &[u32]) -> Decision {
        let mut scored: Vec<(u32, i64, String)> = set.iter().map(|&c| {
            let (s, _) = self.score(c);
            (c, s, self.index.path_of(c))
        }).collect();
        scored.sort_by(|a, b| b.1.cmp(&a.1).then(utf16_len(&a.2).cmp(&utf16_len(&b.2))).then(ordinal_ignore_case(&a.2, &b.2)));
        let ordered: Vec<u32> = scored.iter().map(|x| x.0).collect();
        let mut keeper = ordered[0];
        let mut keeper_note: Option<String> = None;
        let mut homed_note: Option<(u32, String)> = None;
        let (mut used, mut agreed, mut overruled) = (false, false, false);
        if !self.home.is_empty() {
            let ranks: HashMap<u32, usize> = scored.iter().map(|x| (x.0, self.home_rank(&x.2))).collect();
            // Stable: inside one rank the score order survives.
            let homed = *ordered.iter().min_by_key(|c| ranks[c]).unwrap();
            if homed != keeper {
                // A COLLISION is when obeying him would put a copy THE TREE LAW PROTECTS onto the removal list.
                match self.may_be_removed(keeper, set, homed) {
                    Ok(_) => {
                        used = true;
                        keeper_note = Some(format!("YOUR HOME: {}", self.home[ranks[&homed]]));
                        keeper = homed;
                    }
                    Err(why) => {
                        overruled = true;
                        keeper_note = Some(format!("kept over your home choice - THE TREE LAW WINS: {why}"));
                        if ranks[&homed] < self.home.len() {
                            homed_note = Some((homed, format!("your home is {}, but the tree law keeps the filed copy", self.home[ranks[&homed]])));
                        }
                    }
                }
            } else if ranks[&homed] < self.home.len() {
                agreed = true;
                keeper_note = Some(format!("YOUR HOME: {}", self.home[ranks[&homed]]));
            }
        }
        let mut members = vec![(keeper, Role::Keep, keeper_note.unwrap_or_else(|| "the copy that is kept".into()))];
        for &c in &ordered {
            if c == keeper {
                continue;
            }
            let (role, mut why) = match self.may_be_removed(c, set, keeper) {
                Ok(w) => (Role::Remove, if w.is_empty() { "loose in a dumping ground".to_string() } else { w }),
                Err(w) => (Role::Protected, w),
            };
            if let Some((h, note)) = &homed_note {
                if *h == c {
                    why = format!("{why}; {note}");
                }
            }
            members.push((c, role, why));
        }
        Decision { members, home_used: used, home_agreed: agreed, home_overruled: overruled }
    }
}

/// Files the tree law must never offer as a duplicate candidate at all (AvniClean prunes system paths and never reads
/// cloud-only placeholders). Protected, program and project files ARE candidates: they make perfectly good keepers for a
/// loose copy elsewhere; the gate refuses them as victims.
pub fn excluded_from_candidates(index: &Index, id: u32) -> bool {
    let n = &index.nodes[id as usize];
    if n.flags & F_PLACEHOLDER != 0 {
        return true;
    }
    let lower = win_lower(&index.path_of(id));
    SYSTEM_PATHS.iter().any(|s| lower.contains(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::RawEntry;
    use crate::model::*;

    fn drive(id: &str) -> DriveInfo {
        DriveInfo {
            id: id.into(),
            root: format!("{id}\\"),
            label: String::new(),
            fs: "NTFS".into(),
            kind: DriveKind::Fixed,
            media: Media::Ssd,
            total_bytes: 0,
            free_bytes: 0,
            role: DriveRole::External,
            mft_capable: false,
            disk: None,
        }
    }

    /// One drive per first path component ("D\Projects\x" -> drive "D:"), folders created on the way.
    fn index_of(files: &[&str]) -> Index {
        let mut by_drive: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for f in files {
            let (d, rest) = f.split_once('\\').unwrap();
            by_drive.entry(format!("{d}:")).or_default().push(rest.to_string());
        }
        let mut idx = Index::new(ScanProfile::Quick);
        for (d, rels) in by_drive {
            let mut out: Vec<RawEntry> = Vec::new();
            let mut dirs: HashMap<String, u64> = HashMap::new();
            let mut next = 1u64;
            for rel in rels {
                let parts: Vec<&str> = rel.split('\\').collect();
                let mut parent = 0u64;
                let mut acc = String::new();
                for (i, part) in parts.iter().enumerate() {
                    acc.push('\\');
                    acc.push_str(part);
                    let last = i + 1 == parts.len();
                    if !last {
                        if let Some(&k) = dirs.get(&acc) {
                            parent = k;
                            continue;
                        }
                    }
                    let k = next;
                    next += 1;
                    out.push(RawEntry { key: k, parent, name: part.to_string(), is_dir: !last, size: 8192, created: 1, modified: 1, accessed: 1, attrs: 0x20, links: 1, lcn: 0, frn: k, primary: true });
                    if !last {
                        dirs.insert(acc.clone(), k);
                        parent = k;
                    }
                }
            }
            idx.add_drive(drive(&d), "test", out, 0, 0, None);
        }
        idx
    }

    fn id_of(idx: &Index, path: &str) -> u32 {
        (0..idx.nodes.len() as u32).find(|&i| idx.path_of(i).replace('/', "\\").eq_ignore_ascii_case(path)).unwrap_or_else(|| panic!("{path}"))
    }

    fn plan(idx: &Index, sets: &[Vec<&str>], protect: &[&str], home: &[&str], clears: bool) -> Vec<(String, Role, String)> {
        let groups: Vec<Vec<u32>> = sets.iter().map(|s| s.iter().map(|p| id_of(idx, p)).collect()).collect();
        let protect: Vec<String> = protect.iter().map(|s| s.to_string()).collect();
        let home: Vec<String> = home.iter().map(|s| s.to_string()).collect();
        let f = TreeFacts::new(idx, &groups, &protect, &home, clears);
        let mut out = Vec::new();
        for g in &groups {
            for (id, role, why) in f.decide(g).members {
                out.push((idx.path_of(id).replace('/', "\\"), role, why));
            }
        }
        out
    }

    fn role(p: &[(String, Role, String)], path: &str) -> Role {
        p.iter().find(|r| r.0.eq_ignore_ascii_case(path)).map(|r| r.1).unwrap_or_else(|| panic!("{path} not in plan"))
    }

    /// AvniClean SelfTest block 1: three identical files, one inside a protected tree, two in a Dump folder with
    /// copy names, plus one unique file. Plan: 1 KEEP (the protected copy) + 2 REMOVE.
    #[test]
    fn selftest_protected_keeper_and_dump_copies() {
        let idx = index_of(&[
            "R\\AE Brain Archive\\master_report_final.bin",
            "R\\Dump\\master_report_final - Copy.bin",
            "R\\Dump\\master_report_final (2).bin",
            "R\\Dump\\unique.bin",
        ]);
        let set = vec!["R:\\AE Brain Archive\\master_report_final.bin", "R:\\Dump\\master_report_final - Copy.bin", "R:\\Dump\\master_report_final (2).bin"];
        let p = plan(&idx, &[set], &["R:\\AE Brain Archive"], &[], false);
        assert_eq!(role(&p, "R:\\AE Brain Archive\\master_report_final.bin"), Role::Keep);
        assert_eq!(role(&p, "R:\\Dump\\master_report_final - Copy.bin"), Role::Remove);
        assert_eq!(role(&p, "R:\\Dump\\master_report_final (2).bin"), Role::Remove);
    }

    fn home_tree() -> (Index, Vec<Vec<&'static str>>) {
        let idx = index_of(&[
            "D\\Projects\\Income tax\\2025-26\\ravi.docx",
            "E\\Projects\\Income tax\\2025-26\\ravi.docx",
            "F\\Projects\\Income tax\\2025-26\\ravi.docx",
            "G\\Projects\\Income tax\\2025-26\\ravi.docx",
            "D\\Downloads\\notes.pdf",
            "F\\Papers\\IIMK\\Term3\\notes.pdf",
        ]);
        let sets = vec![
            vec![
                "D:\\Projects\\Income tax\\2025-26\\ravi.docx",
                "E:\\Projects\\Income tax\\2025-26\\ravi.docx",
                "F:\\Projects\\Income tax\\2025-26\\ravi.docx",
                "G:\\Projects\\Income tax\\2025-26\\ravi.docx",
            ],
            vec!["D:\\Downloads\\notes.pdf", "F:\\Papers\\IIMK\\Term3\\notes.pdf"],
        ];
        (idx, sets)
    }

    /// AvniClean SelfTest, THE HOME RULE 29-08-2026, --home D: (collision + home alone).
    #[test]
    fn selftest_home_collision_tree_law_wins() {
        let (idx, sets) = home_tree();
        let p = plan(&idx, &sets, &[], &["D:\\"], false);
        assert_eq!(role(&p, "F:\\Papers\\IIMK\\Term3\\notes.pdf"), Role::Keep, "COLLISION: the FILED copy survives even though HOME points at the other drive");
        assert_eq!(role(&p, "D:\\Downloads\\notes.pdf"), Role::Remove, "COLLISION: the loose copy in Downloads is the one removed");
        assert!(p.iter().any(|r| r.2.contains("THE TREE LAW WINS")), "COLLISION: the plan says out loud that the tree law overruled the home choice");
        assert!(!p.iter().any(|r| r.0.ends_with("ravi.docx") && r.1 == Role::Remove), "HOME ALONE never removes a filed copy - four filed twins are all kept");
    }

    /// --home D: --home-clears
    #[test]
    fn selftest_home_clears() {
        let (idx, sets) = home_tree();
        let p = plan(&idx, &sets, &[], &["D:\\"], true);
        assert_eq!(role(&p, "D:\\Projects\\Income tax\\2025-26\\ravi.docx"), Role::Keep, "HOME CLEARS: the copy in his home folder is the survivor");
        for d in ["E", "F", "G"] {
            assert_eq!(role(&p, &format!("{d}:\\Projects\\Income tax\\2025-26\\ravi.docx")), Role::Remove, "HOME CLEARS: the other drives' copies go");
        }
        assert_eq!(role(&p, "F:\\Papers\\IIMK\\Term3\\notes.pdf"), Role::Keep, "HOME CLEARS does NOT lift the collision rule - the tree law still wins");
    }

    /// No home list: the plan is exactly the plain tree-law plan.
    #[test]
    fn selftest_no_home_list() {
        let (idx, sets) = home_tree();
        let p = plan(&idx, &sets, &[], &[], false);
        assert_eq!(role(&p, "F:\\Papers\\IIMK\\Term3\\notes.pdf"), Role::Keep);
        assert!(!p.iter().any(|r| r.0.ends_with("ravi.docx") && r.1 == Role::Remove));
    }

    /// The four loose shapes, and the absolute guards that no shape can beat.
    #[test]
    fn loose_shapes_and_guards() {
        let mut files = vec![
            "C\\Income tax\\2025-26\\return.pdf".to_string(),
            "C\\Income tax\\return.pdf".to_string(),          // pulled out of its own folder
            "C\\Papers\\Term3\\a.pdf".to_string(),
            "C\\Papers\\Term3\\a copy.pdf".to_string(),       // beside the keeper (and a copy name)
            "C\\Office\\Clients\\Acme\\2024\\deed.pdf".to_string(),
            "C\\Office\\Clients\\Acme\\2024\\deed2.txt".to_string(),
            "C\\Code\\app\\package.json".to_string(),
            "C\\Code\\app\\src\\deed.pdf".to_string(),        // inside a project -> never removed
            "C\\Tools\\x\\tool.exe".to_string(),
            "C\\Tools\\y\\tool.exe".to_string(),              // program file -> never removed
            "C\\Filed\\Letters\\2023\\letter.pdf".to_string(),
            "C\\Another\\letter.pdf".to_string(),             // filed elsewhere, not loose -> kept
        ];
        // WhatsApp-export heap: no subfolders, 30 files, one of them a twin of a deeply filed paper.
        for i in 0..29 {
            files.push(format!("C\\WhatsApp Export\\img{i}.jpg"));
        }
        files.push("C\\WhatsApp Export\\paper.pdf".into());
        files.push("C\\Studies\\IIMK\\Q3\\04 - DBM\\paper.pdf".into());
        let refs: Vec<&str> = files.iter().map(|s| s.as_str()).collect();
        let idx = index_of(&refs);
        let sets = vec![
            vec!["C:\\Income tax\\2025-26\\return.pdf", "C:\\Income tax\\return.pdf"],
            vec!["C:\\Papers\\Term3\\a.pdf", "C:\\Papers\\Term3\\a copy.pdf"],
            vec!["C:\\Office\\Clients\\Acme\\2024\\deed.pdf", "C:\\Code\\app\\src\\deed.pdf"],
            vec!["C:\\Tools\\x\\tool.exe", "C:\\Tools\\y\\tool.exe"],
            vec!["C:\\Filed\\Letters\\2023\\letter.pdf", "C:\\Another\\letter.pdf"],
            vec!["C:\\WhatsApp Export\\paper.pdf", "C:\\Studies\\IIMK\\Q3\\04 - DBM\\paper.pdf"],
        ];
        let p = plan(&idx, &sets, &[], &[], false);
        assert_eq!(role(&p, "C:\\Income tax\\2025-26\\return.pdf"), Role::Keep);
        assert_eq!(role(&p, "C:\\Income tax\\return.pdf"), Role::Remove);
        assert_eq!(role(&p, "C:\\Papers\\Term3\\a.pdf"), Role::Keep);
        assert_eq!(role(&p, "C:\\Papers\\Term3\\a copy.pdf"), Role::Remove);
        let deed_proj = role(&p, "C:\\Code\\app\\src\\deed.pdf");
        assert_ne!(deed_proj, Role::Remove, "a copy inside a project is never removed");
        assert!(p.iter().all(|r| !(r.0.ends_with("tool.exe") && r.1 == Role::Remove)), "a program file is never removed");
        assert!(p.iter().all(|r| !(r.0.ends_with("letter.pdf") && r.1 == Role::Remove)), "two filed copies: both stay");
        assert_eq!(role(&p, "C:\\Studies\\IIMK\\Q3\\04 - DBM\\paper.pdf"), Role::Keep);
        assert_eq!(role(&p, "C:\\WhatsApp Export\\paper.pdf"), Role::Remove, "tipped into a flat heap");
    }

    #[test]
    fn copy_names_match_avniclean() {
        for n in ["x - Copy", "x (2)", "x copy", "x copy (3)", "x_1", "x (Copy)", "report-copy"] {
            assert!(copy_name().is_match(n), "{n}");
        }
        for n in ["x", "x_2024", "copyright", "Report v1"] {
            assert!(!copy_name().is_match(n), "{n}");
        }
        assert!(dump_folder_strict().is_match("Downloads") && dump_folder_strict().is_match("New folder (3)"));
        assert!(!dump_folder_strict().is_match("Final") && !dump_folder_strict().is_match("Backup"));
    }
}
