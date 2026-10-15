//! The in-memory file index for every scanned drive. Nodes are stored in depth-first order with folders before their
//! contents and names sorted, so a node's parent always comes before it (top-down passes are one forward loop) and
//! "sort by path" is simply "sort by id".

use crate::model::*;
use crate::util::{ext_of, join};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

pub const F_DIR: u16 = 1;
/// First hard link of a file: the one whose bytes are counted.
pub const F_PRIMARY: u16 = 2;
/// Moved or removed by an action: hidden from queries and totals.
pub const F_GONE: u16 = 4;
/// Cloud-only / offline placeholder: content is not on this disk (reading it would download it).
pub const F_PLACEHOLDER: u16 = 8;
pub const F_REPARSE: u16 = 16;

pub const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
pub const FILE_ATTRIBUTE_OFFLINE: u32 = 0x1000;
pub const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x40000;
pub const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x400000;

/// Common shape produced by both the MFT reader and the folder walker.
#[derive(Clone, Debug)]
pub struct RawEntry {
    /// Unique for folders (MFT: file record number; walk: running number). Files may repeat it (hard links).
    pub key: u64,
    pub parent: u64,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub created: i64,
    pub modified: i64,
    pub accessed: i64,
    pub attrs: u32,
    pub links: u16,
    pub lcn: u64,
    pub frn: u64,
    pub primary: bool,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub name: Box<str>,
    pub size: u64,
    pub created: i64,
    pub modified: i64,
    pub accessed: i64,
    pub lcn: u64,
    pub frn: u64,
    pub parent: u32,
    pub attrs: u32,
    pub reason: u32,
    pub drive: u16,
    pub flags: u16,
    pub links: u16,
    pub verdict: Verdict,
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.flags & F_DIR != 0
    }
    pub fn gone(&self) -> bool {
        self.flags & F_GONE != 0
    }
    /// Bytes this node really occupies on its drive (0 for folders, extra hard links and cloud placeholders).
    pub fn counted_bytes(&self) -> u64 {
        if self.flags & (F_DIR | F_GONE | F_PLACEHOLDER) != 0 || self.flags & F_PRIMARY == 0 {
            0
        } else {
            self.size
        }
    }
}

#[derive(Clone, Debug)]
pub struct DriveScan {
    pub info: DriveInfo,
    pub method: String,
    pub root: u32,
    /// Node ids of this drive are root..end.
    pub end: u32,
    pub ms: u64,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Hashes {
    pub sample: Option<[u8; 32]>,
    pub full: Option<[u8; 32]>,
    pub sha256: Option<[u8; 32]>,
}

#[derive(Clone, Debug)]
pub struct Group {
    pub id: u32,
    pub size: u64,
    pub confirmed: bool,
    pub hash: [u8; 32],
    /// members[0] is kept.
    pub members: Vec<u32>,
}

type QueryCache = Option<(String, u64, Arc<Vec<u32>>, u64)>;

pub struct Index {
    pub drives: Vec<DriveScan>,
    pub nodes: Vec<Node>,
    pub hashes: HashMap<u32, Hashes>,
    pub groups: Vec<Group>,
    pub group_of: HashMap<u32, u32>,
    /// Local file -> Google Drive file with the same name and size.
    pub in_drive: HashMap<u32, u32>,
    /// Tree-law decision for every member of a duplicate set: role + why (reason id).
    pub dup_role: HashMap<u32, (crate::treelaw::Role, u32)>,
    reasons: Vec<String>,
    reason_ids: HashMap<String, u32>,
    dir_paths: HashMap<u32, Box<str>>,
    pub profile: ScanProfile,
    pub scanned_at_ms: i64,
    pub total_ms: u64,
    pub metadata_ms: u64,
    pub hash_ms: u64,
    pub notes: Vec<String>,
    /// Bumped on every change, so cached query results are never stale.
    pub generation: u64,
    cache: Mutex<QueryCache>,
}

impl Default for Index {
    fn default() -> Self {
        Self::new(ScanProfile::Quick)
    }
}

impl Index {
    pub fn new(profile: ScanProfile) -> Self {
        let mut s = Index {
            drives: Vec::new(),
            nodes: Vec::new(),
            hashes: HashMap::new(),
            groups: Vec::new(),
            group_of: HashMap::new(),
            in_drive: HashMap::new(),
            dup_role: HashMap::new(),
            reasons: Vec::new(),
            reason_ids: HashMap::new(),
            dir_paths: HashMap::new(),
            profile,
            scanned_at_ms: crate::util::now_ms(),
            total_ms: 0,
            metadata_ms: 0,
            hash_ms: 0,
            notes: Vec::new(),
            generation: 0,
            cache: Mutex::new(None),
        };
        s.reason_id("");
        s
    }

    pub fn reason_id(&mut self, r: &str) -> u32 {
        if let Some(&i) = self.reason_ids.get(r) {
            return i;
        }
        let i = self.reasons.len() as u32;
        self.reasons.push(r.to_string());
        self.reason_ids.insert(r.to_string(), i);
        i
    }

    pub fn reason(&self, id: u32) -> &str {
        self.reasons.get(id as usize).map(|s| s.as_str()).unwrap_or("")
    }

    pub fn touch(&mut self) {
        self.generation += 1;
        *self.cache.lock().unwrap() = None;
    }

    /// Adds one drive's entries. `root_key` is the key of the drive's root folder (its own entry, if present, only
    /// supplies the root's times).
    pub fn add_drive(&mut self, info: DriveInfo, method: &str, mut raw: Vec<RawEntry>, root_key: u64, ms: u64, error: Option<String>) {
        let drive = self.drives.len() as u16;
        let base = self.nodes.len() as u32;
        let mut root = Node {
            name: info.root.clone().into_boxed_str(),
            size: 0,
            created: 0,
            modified: 0,
            accessed: 0,
            lcn: 0,
            frn: root_key,
            parent: base,
            attrs: 0x10,
            reason: 0,
            drive,
            flags: F_DIR | F_PRIMARY,
            links: 1,
            verdict: Verdict::Review,
        };
        if let Some(pos) = raw.iter().position(|e| e.key == root_key && e.is_dir) {
            let r = raw.swap_remove(pos);
            root.created = r.created;
            root.modified = r.modified;
            root.accessed = r.accessed;
        }
        raw.retain(|e| !(e.is_dir && e.key == root_key));
        // Children of one folder become contiguous, sorted by name.
        raw.sort_by_cached_key(|e| (e.parent, e.name.to_lowercase()));
        let mut ranges: HashMap<u64, (usize, usize)> = HashMap::new();
        let mut i = 0;
        while i < raw.len() {
            let p = raw[i].parent;
            let s = i;
            while i < raw.len() && raw[i].parent == p {
                i += 1;
            }
            ranges.insert(p, (s, i));
        }
        let mut used = vec![false; raw.len()];
        let mut visited: HashSet<u64> = HashSet::new();
        visited.insert(root_key);
        self.nodes.push(root);
        let mut t = Tree { ranges: &ranges, raw: &mut raw, used: &mut used, visited: &mut visited, drive };
        t.dfs(root_key, base, &mut self.nodes);
        // Anything not reachable from the root (broken parent links) goes under "$Orphaned" so nothing is lost.
        if t.used.iter().any(|u| !u) {
            let orphan_dirs: HashSet<u64> = t.raw.iter().zip(t.used.iter()).filter(|(e, u)| !**u && e.is_dir).map(|(e, _)| e.key).collect();
            let mut tops: Vec<usize> = (0..t.raw.len()).filter(|&k| !t.used[k] && !orphan_dirs.contains(&t.raw[k].parent)).collect();
            if tops.is_empty() {
                // A cycle of folders: break it at the first one.
                tops = (0..t.raw.len()).filter(|&k| !t.used[k]).take(1).collect();
            }
            let oid = self.nodes.len() as u32;
            let mut o = self.nodes[base as usize].clone();
            o.name = "$Orphaned".into();
            o.parent = base;
            o.frn = u64::MAX;
            self.nodes.push(o);
            for k in tops {
                t.take(k, oid, &mut self.nodes);
            }
            // Whatever is still left (deeper cycles) hangs straight under $Orphaned.
            for k in 0..t.raw.len() {
                if !t.used[k] {
                    t.take(k, oid, &mut self.nodes);
                }
            }
        }
        let end = self.nodes.len() as u32;
        // Folder paths, top-down (parents come first).
        self.dir_paths.insert(base, info.root.clone().into_boxed_str());
        for id in base + 1..end {
            let n = &self.nodes[id as usize];
            if n.is_dir() {
                let p = join(&self.dir_paths[&n.parent], &n.name);
                self.dir_paths.insert(id, p.into_boxed_str());
            }
        }
        self.drives.push(DriveScan { info, method: method.to_string(), root: base, end, ms, error });
        self.touch();
    }

    pub fn dir_path(&self, id: u32) -> &str {
        self.dir_paths.get(&id).map(|s| &**s).unwrap_or("")
    }

    pub fn path_of(&self, id: u32) -> String {
        let n = &self.nodes[id as usize];
        if n.is_dir() {
            self.dir_path(id).to_string()
        } else {
            join(self.dir_path(n.parent), &n.name)
        }
    }

    pub fn drive_of(&self, id: u32) -> &DriveScan {
        &self.drives[self.nodes[id as usize].drive as usize]
    }

    pub fn row(&self, id: u32) -> FileRow {
        let n = &self.nodes[id as usize];
        let h = self.hashes.get(&id);
        let mut reason = self.reason(n.reason).to_string();
        if n.verdict == Verdict::InDrive {
            if let Some(&g) = self.in_drive.get(&id) {
                reason = format!("Same name and size on Google Drive: {}", self.path_of(g));
            }
        }
        if let Some(&(role, why)) = self.dup_role.get(&id) {
            use crate::treelaw::Role;
            match role {
                Role::Remove if n.verdict == Verdict::Duplicate => reason = format!("Extra copy — {}", self.reason(why)),
                Role::Remove => reason = format!("{reason} · loose duplicate ({})", self.reason(why)),
                Role::Keep => reason = format!("{reason} · the copy kept from its duplicate set"),
                Role::Protected => reason = format!("{reason} · duplicate kept by the tree law: {}", self.reason(why)),
            }
        }
        FileRow {
            id,
            drive: self.drives[n.drive as usize].info.id.clone(),
            path: self.path_of(id),
            name: n.name.to_string(),
            ext: if n.is_dir() { String::new() } else { ext_of(&n.name) },
            is_dir: n.is_dir(),
            size: n.size,
            created_ms: n.created,
            modified_ms: n.modified,
            accessed_ms: n.accessed,
            attributes: n.attrs,
            links: n.links,
            blake3: h.and_then(|h| h.full).map(|x| crate::util::hex(&x)),
            sha256: h.and_then(|h| h.sha256).map(|x| crate::util::hex(&x)),
            sample: h.and_then(|h| h.sample).map(|x| crate::util::hex(&x)),
            dup_group: self.group_of.get(&id).copied(),
            verdict: n.verdict,
            reason,
        }
    }

    fn totals_for(&self, range: std::ops::Range<u32>) -> Vec<VerdictTotal> {
        let mut t: HashMap<Verdict, (u64, u64)> = HashMap::new();
        for id in range {
            let n = &self.nodes[id as usize];
            if n.is_dir() || n.gone() {
                continue;
            }
            let e = t.entry(n.verdict).or_default();
            e.0 += 1;
            e.1 += n.counted_bytes();
        }
        Verdict::ALL
            .iter()
            .map(|v| {
                let (f, b) = t.get(v).copied().unwrap_or_default();
                VerdictTotal { verdict: Some(*v), files: f, bytes: b }
            })
            .collect()
    }

    /// Members the tree law lets go (and that are still on disk).
    pub fn removable(&self, g: &Group) -> Vec<u32> {
        g.members
            .iter()
            .copied()
            .filter(|m| !self.nodes[*m as usize].gone() && self.nodes[*m as usize].verdict == Verdict::Duplicate)
            .collect()
    }

    pub fn summary(&self) -> Summary {
        let mut drives = Vec::new();
        let (mut files, mut dirs, mut bytes) = (0u64, 0u64, 0u64);
        let mut all: Vec<VerdictTotal> = Verdict::ALL.iter().map(|v| VerdictTotal { verdict: Some(*v), files: 0, bytes: 0 }).collect();
        for d in &self.drives {
            let (mut f, mut di, mut b) = (0u64, 0u64, 0u64);
            for id in d.root..d.end {
                let n = &self.nodes[id as usize];
                if n.gone() {
                    continue;
                }
                if n.is_dir() {
                    di += 1;
                } else {
                    f += 1;
                    b += n.counted_bytes();
                }
            }
            let totals = self.totals_for(d.root..d.end);
            for (a, t) in all.iter_mut().zip(&totals) {
                a.files += t.files;
                a.bytes += t.bytes;
            }
            files += f;
            dirs += di;
            bytes += b;
            drives.push(DriveSummary {
                drive: d.info.clone(),
                method: d.method.clone(),
                files: f,
                dirs: di.saturating_sub(1),
                bytes: b,
                scan_ms: d.ms,
                totals,
                error: d.error.clone(),
            });
        }
        let hashed: Vec<&Hashes> = self.hashes.values().collect();
        let hashed_files = hashed.iter().filter(|h| h.full.is_some()).count() as u64;
        let hashed_bytes = self.hashes.iter().filter(|(_, h)| h.full.is_some()).map(|(id, _)| self.nodes[*id as usize].size).sum();
        let live_groups: Vec<&Group> = self.groups.iter().filter(|g| g.members.iter().filter(|m| !self.nodes[**m as usize].gone()).count() > 1).collect();
        Summary {
            scanned_at_ms: self.scanned_at_ms,
            profile: self.profile,
            total_ms: self.total_ms,
            metadata_ms: self.metadata_ms,
            hash_ms: self.hash_ms,
            drives,
            totals: all,
            files,
            dirs: dirs.saturating_sub(self.drives.len() as u64),
            bytes,
            hashed_files,
            hashed_bytes,
            dup_groups: live_groups.len() as u64,
            dup_groups_confirmed: live_groups.iter().filter(|g| g.confirmed).count() as u64,
            // Under the tree law only loose copies count as reclaimable; filed twins are kept on purpose.
            dup_wasted_bytes: live_groups.iter().map(|g| self.removable(g).len() as u64 * g.size).sum(),
            notes: self.notes.clone(),
        }
    }

    pub fn dup_group(&self, g: &Group) -> DupGroup {
        let members: Vec<FileRow> = g.members.iter().filter(|m| !self.nodes[**m as usize].gone()).map(|&m| self.row(m)).collect();
        DupGroup {
            id: g.id,
            size: g.size,
            confirmed: g.confirmed,
            hash: crate::util::hex(&g.hash),
            wasted: self.removable(g).len() as u64 * g.size,
            members,
        }
    }

    pub fn dup_page(&self, offset: u32, limit: u32, confirmed_only: bool) -> DupPage {
        let mut gs: Vec<&Group> = self
            .groups
            .iter()
            .filter(|g| (!confirmed_only || g.confirmed) && g.members.iter().filter(|m| !self.nodes[**m as usize].gone()).count() > 1)
            .collect();
        gs.sort_by(|a, b| (b.size * (b.members.len() as u64 - 1)).cmp(&(a.size * (a.members.len() as u64 - 1))).then(a.id.cmp(&b.id)));
        DupPage {
            total: gs.len() as u64,
            groups: gs.iter().skip(offset as usize).take(limit as usize).map(|g| self.dup_group(g)).collect(),
        }
    }

    pub fn largest(&self, n: usize) -> Vec<FileRow> {
        let mut ids: Vec<u32> = (0..self.nodes.len() as u32).filter(|&i| { let x = &self.nodes[i as usize]; !x.is_dir() && !x.gone() }).collect();
        if ids.len() > n {
            ids.select_nth_unstable_by(n, |a, b| self.nodes[*b as usize].size.cmp(&self.nodes[*a as usize].size));
            ids.truncate(n);
        }
        ids.sort_by(|a, b| self.nodes[*b as usize].size.cmp(&self.nodes[*a as usize].size));
        ids.into_iter().map(|i| self.row(i)).collect()
    }

    pub fn extensions(&self) -> Vec<ExtTotal> {
        let mut m: HashMap<String, (u64, u64)> = HashMap::new();
        for n in &self.nodes {
            if n.is_dir() || n.gone() {
                continue;
            }
            let e = m.entry(ext_of(&n.name)).or_default();
            e.0 += 1;
            e.1 += n.counted_bytes();
        }
        let mut v: Vec<ExtTotal> = m.into_iter().map(|(ext, (files, bytes))| ExtTotal { ext, files, bytes }).collect();
        v.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.ext.cmp(&b.ext)));
        v
    }

    /// Per-folder subtree totals: (files, bytes) for every node (folders get their whole subtree).
    pub fn rollups(&self) -> Vec<(u64, u64)> {
        let mut r = vec![(0u64, 0u64); self.nodes.len()];
        for id in (0..self.nodes.len()).rev() {
            let n = &self.nodes[id];
            if n.gone() {
                continue;
            }
            if !n.is_dir() {
                r[id].0 += 1;
                r[id].1 += n.counted_bytes();
            }
            let p = n.parent as usize;
            if p != id {
                let (f, b) = r[id];
                r[p].0 += f;
                r[p].1 += b;
            }
        }
        r
    }

    /// Folders worth showing: depth 1-2 everywhere, and deeper folders (to depth 5) of at least 1 GB.
    pub fn folders(&self) -> Vec<FolderTotal> {
        let roll = self.rollups();
        let mut depth = vec![0u8; self.nodes.len()];
        let mut out = Vec::new();
        for d in &self.drives {
            for id in d.root + 1..d.end {
                let n = &self.nodes[id as usize];
                depth[id as usize] = depth[n.parent as usize].saturating_add(1);
                if !n.is_dir() || n.gone() {
                    continue;
                }
                let dp = depth[id as usize];
                let (files, bytes) = roll[id as usize];
                if dp <= 2 || (dp <= 5 && bytes >= 1 << 30) {
                    out.push(FolderTotal { drive: d.info.id.clone(), path: self.dir_path(id).to_string(), files, bytes, verdict: n.verdict });
                }
            }
        }
        out
    }

    /// Visits every live row, drive by drive, in path order.
    pub fn for_each_row(&self, f: &mut dyn FnMut(&FileRow) -> anyhow::Result<()>) -> anyhow::Result<()> {
        for d in &self.drives {
            for id in d.root..d.end {
                if self.nodes[id as usize].gone() {
                    continue;
                }
                f(&self.row(id))?;
            }
        }
        Ok(())
    }

    pub fn query(&self, q: &FileQuery) -> FilePage {
        let key = format!(
            "{:?}|{:?}|{:?}|{:?}|{:?}|{}|{:?}|{}",
            q.drive, q.verdict, q.search, q.ext, q.min_size, q.include_dirs, q.sort, q.desc
        );
        let cached = {
            let c = self.cache.lock().unwrap();
            match &*c {
                Some((k, g, ids, bytes)) if *k == key && *g == self.generation => Some((ids.clone(), *bytes)),
                _ => None,
            }
        };
        let (ids, bytes) = match cached {
            Some(x) => x,
            None => {
                let (ids, bytes) = self.run_query(q);
                let ids = Arc::new(ids);
                *self.cache.lock().unwrap() = Some((key, self.generation, ids.clone(), bytes));
                (ids, bytes)
            }
        };
        FilePage {
            total: ids.len() as u64,
            bytes,
            rows: ids.iter().skip(q.offset as usize).take(q.limit.min(5000) as usize).map(|&i| self.row(i)).collect(),
        }
    }

    fn run_query(&self, q: &FileQuery) -> (Vec<u32>, u64) {
        let drive = q.drive.as_ref().and_then(|d| self.drives.iter().position(|x| x.info.id.eq_ignore_ascii_case(d)));
        if q.drive.is_some() && drive.is_none() {
            return (Vec::new(), 0);
        }
        let needle = q.search.as_ref().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
        let path_search = needle.as_ref().map(|s| s.contains('\\') || s.contains('/')).unwrap_or(false);
        let needle = needle.map(|s| if path_search && cfg!(windows) { s.replace('/', "\\") } else { s });
        let ext = q.ext.as_ref().map(|e| e.trim().trim_start_matches('.').to_lowercase()).filter(|e| !e.is_empty());
        let range = match drive {
            Some(d) => self.drives[d].root..self.drives[d].end,
            None => 0..self.nodes.len() as u32,
        };
        let mut ids: Vec<u32> = Vec::new();
        let mut bytes = 0u64;
        for id in range {
            let n = &self.nodes[id as usize];
            if n.gone() || (n.is_dir() && !q.include_dirs) {
                continue;
            }
            if let Some(v) = q.verdict {
                if n.verdict != v {
                    continue;
                }
            }
            if let Some(m) = q.min_size {
                if n.size < m {
                    continue;
                }
            }
            if let Some(e) = &ext {
                if n.is_dir() || ext_of(&n.name) != *e {
                    continue;
                }
            }
            if let Some(s) = &needle {
                let hit = if path_search { self.path_of(id).to_lowercase().contains(s.as_str()) } else { n.name.to_lowercase().contains(s.as_str()) };
                if !hit {
                    continue;
                }
            }
            bytes += n.counted_bytes();
            ids.push(id);
        }
        let nodes = &self.nodes;
        match q.sort {
            SortKey::Path => {}
            SortKey::Name => ids.sort_by_cached_key(|&i| nodes[i as usize].name.to_lowercase()),
            SortKey::Size => ids.sort_by_key(|&i| nodes[i as usize].size),
            SortKey::Modified => ids.sort_by_key(|&i| nodes[i as usize].modified),
            SortKey::Created => ids.sort_by_key(|&i| nodes[i as usize].created),
            SortKey::Ext => ids.sort_by_cached_key(|&i| ext_of(&nodes[i as usize].name)),
            SortKey::Verdict => ids.sort_by_key(|&i| nodes[i as usize].verdict),
        }
        if q.desc {
            ids.reverse();
        }
        (ids, bytes)
    }
}

fn node_from(name: String, e: &RawEntry, parent: u32, drive: u16) -> Node {
    let mut flags = 0u16;
    if e.is_dir {
        flags |= F_DIR;
    }
    if e.primary {
        flags |= F_PRIMARY;
    }
    if e.attrs & (FILE_ATTRIBUTE_OFFLINE | FILE_ATTRIBUTE_RECALL_ON_OPEN | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS) != 0 {
        flags |= F_PLACEHOLDER;
    }
    if e.attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        flags |= F_REPARSE;
    }
    Node {
        name: name.into_boxed_str(),
        size: e.size,
        created: e.created,
        modified: e.modified,
        accessed: e.accessed,
        lcn: e.lcn,
        frn: e.frn,
        parent,
        attrs: e.attrs,
        reason: 0,
        drive,
        flags,
        links: e.links,
        verdict: Verdict::Review,
    }
}

struct Tree<'a> {
    ranges: &'a HashMap<u64, (usize, usize)>,
    raw: &'a mut Vec<RawEntry>,
    used: &'a mut Vec<bool>,
    visited: &'a mut HashSet<u64>,
    drive: u16,
}

impl Tree<'_> {
    /// Places entry k under `parent` and, for a folder seen for the first time, its whole subtree.
    fn take(&mut self, k: usize, parent: u32, nodes: &mut Vec<Node>) {
        if self.used[k] {
            return;
        }
        self.used[k] = true;
        let id = nodes.len() as u32;
        let e = &mut self.raw[k];
        nodes.push(node_from(std::mem::take(&mut e.name), e, parent, self.drive));
        if e.is_dir && self.visited.insert(e.key) {
            let key = e.key;
            self.dfs(key, id, nodes);
        }
    }

    /// Depth-first, children in name order, without recursion (deep trees cannot overflow the stack).
    fn dfs(&mut self, start_key: u64, start_node: u32, nodes: &mut Vec<Node>) {
        let mut stack: Vec<(usize, usize, u32)> = Vec::new();
        if let Some(&(s, e)) = self.ranges.get(&start_key) {
            stack.push((s, e, start_node));
        }
        while let Some(top) = stack.last_mut() {
            if top.0 >= top.1 {
                stack.pop();
                continue;
            }
            let k = top.0;
            top.0 += 1;
            let parent = top.2;
            if self.used[k] {
                continue;
            }
            self.used[k] = true;
            let id = nodes.len() as u32;
            let e = &mut self.raw[k];
            nodes.push(node_from(std::mem::take(&mut e.name), e, parent, self.drive));
            if e.is_dir && self.visited.insert(e.key) {
                if let Some(&(s, en)) = self.ranges.get(&e.key) {
                    stack.push((s, en, id));
                }
            }
        }
    }
}
