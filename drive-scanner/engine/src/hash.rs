//! Hashing for duplicates. BLAKE3 everywhere: it runs at several GB/s per core, so the disk, never the hash, is the
//! limit (SHA-256 ~0.5-2 GB/s, SHA-3 slower still). Optional SHA-256 is computed in the same read for matching against
//! Google Drive (which publishes sha256 for each file).
//!
//! Speed on an HDD comes from what we do NOT read and from read order:
//! - sampled hash = size + first 16 KB + last 16 KB (whole file when it is 64 KB or less);
//! - each physical disk gets one "lane"; an HDD lane uses ONE reader and visits files in on-disk order (first cluster
//!   from the MFT), so the head sweeps forward instead of thrashing; SSD lanes use several readers.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

pub const SAMPLE: u64 = 16 * 1024;
/// Files up to this size are hashed whole by the sampler (the "sample" is then the full hash).
pub const SMALL: u64 = 4 * SAMPLE;

fn open_read(path: &str, sequential: bool) -> std::io::Result<File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Share everything so files open in other programs can still be read; sequential scan doubles read-ahead.
        let mut o = std::fs::OpenOptions::new();
        o.read(true).share_mode(7);
        if sequential {
            o.custom_flags(0x0800_0000);
        }
        o.open(crate::util::long_path(path))
    }
    #[cfg(not(windows))]
    {
        let _ = sequential;
        File::open(path)
    }
}

/// (hash, whole_file): whole_file = true when the file was small enough to hash completely.
pub fn sample_hash(path: &str, size: u64) -> std::io::Result<([u8; 32], bool)> {
    let mut f = open_read(path, false)?;
    if size <= SMALL {
        let mut buf = Vec::with_capacity(size as usize);
        f.read_to_end(&mut buf)?;
        if buf.len() as u64 != size {
            return Err(std::io::Error::other("file changed size while reading"));
        }
        return Ok((*blake3::hash(&buf).as_bytes(), true));
    }
    let mut h = blake3::Hasher::new();
    h.update(&size.to_le_bytes());
    let mut buf = vec![0u8; SAMPLE as usize];
    f.read_exact(&mut buf)?;
    h.update(&buf);
    f.seek(SeekFrom::Start(size - SAMPLE))?;
    f.read_exact(&mut buf)?;
    h.update(&buf);
    Ok((*h.finalize().as_bytes(), false))
}

/// Full-content BLAKE3 (+ SHA-256 when asked) in one pass. `on_bytes` is told about every buffer read.
pub fn full_hash(
    path: &str,
    sha256: bool,
    buf: &mut [u8],
    cancel: &AtomicBool,
    on_bytes: &mut dyn FnMut(u64),
) -> std::io::Result<([u8; 32], Option<[u8; 32]>)> {
    use sha2::Digest;
    let mut f = open_read(path, true)?;
    let mut b3 = blake3::Hasher::new();
    let mut s2 = if sha256 { Some(sha2::Sha256::new()) } else { None };
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("cancelled"));
        }
        let n = f.read(buf)?;
        if n == 0 {
            break;
        }
        b3.update(&buf[..n]);
        if let Some(s) = s2.as_mut() {
            s.update(&buf[..n]);
        }
        on_bytes(n as u64);
    }
    Ok((*b3.finalize().as_bytes(), s2.map(|s| s.finalize().into())))
}

#[derive(Clone, Debug)]
pub struct Job {
    pub id: u32,
    pub path: String,
    pub size: u64,
    pub lcn: u64,
}

/// All jobs of one physical disk.
pub struct Lane {
    pub name: String,
    pub hdd: bool,
    pub jobs: Vec<Job>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Sample,
    Full { sha256: bool },
}

#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub id: u32,
    pub sample: Option<[u8; 32]>,
    pub full: Option<[u8; 32]>,
    pub sha256: Option<[u8; 32]>,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct Stats {
    pub files_done: AtomicU64,
    pub bytes_done: AtomicU64,
}

/// Runs every lane in parallel (one thread per disk; HDD = one reader, SSD = up to 8). Stops picking up new files at
/// `deadline` or on cancel; files not started are simply absent from the result. `tick()` is called often.
pub fn run_lanes(lanes: Vec<Lane>, mode: Mode, cancel: &AtomicBool, deadline: Option<Instant>, stats: &Stats, tick: &(dyn Fn() + Sync)) -> Vec<Outcome> {
    let out: Mutex<Vec<Outcome>> = Mutex::new(Vec::new());
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    std::thread::scope(|s| {
        for mut lane in lanes {
            let out = &out;
            s.spawn(move || {
                if lane.hdd {
                    lane.jobs.sort_by_key(|j| j.lcn);
                }
                let readers = if lane.hdd { 1 } else { cores.clamp(2, 8) };
                let next = AtomicUsize::new(0);
                let jobs = &lane.jobs;
                std::thread::scope(|s2| {
                    for _ in 0..readers {
                        s2.spawn(|| {
                            let mut buf = vec![0u8; if lane.hdd { 4 << 20 } else { 1 << 20 }];
                            let mut local = Vec::new();
                            loop {
                                if cancel.load(Ordering::Relaxed) || deadline.is_some_and(|d| Instant::now() >= d) {
                                    break;
                                }
                                let i = next.fetch_add(1, Ordering::Relaxed);
                                let Some(j) = jobs.get(i) else { break };
                                let mut o = Outcome { id: j.id, ..Default::default() };
                                match mode {
                                    Mode::Sample => match sample_hash(&j.path, j.size) {
                                        Ok((h, whole)) => {
                                            o.sample = Some(h);
                                            if whole {
                                                o.full = Some(h);
                                            }
                                            stats.bytes_done.fetch_add(j.size.min(2 * SAMPLE), Ordering::Relaxed);
                                        }
                                        Err(e) => o.error = Some(e.to_string()),
                                    },
                                    Mode::Full { sha256 } => {
                                        let mut cb = |n: u64| {
                                            stats.bytes_done.fetch_add(n, Ordering::Relaxed);
                                        };
                                        match full_hash(&j.path, sha256, &mut buf, cancel, &mut cb) {
                                            Ok((h, s)) => {
                                                o.full = Some(h);
                                                o.sha256 = s;
                                                if j.size <= SMALL {
                                                    o.sample = Some(h);
                                                }
                                            }
                                            Err(e) => o.error = Some(e.to_string()),
                                        }
                                    }
                                }
                                stats.files_done.fetch_add(1, Ordering::Relaxed);
                                local.push(o);
                                tick();
                            }
                            out.lock().unwrap().extend(local);
                        });
                    }
                });
            });
        }
    });
    out.into_inner().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_and_full() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        let c = dir.path().join("c.bin");
        let mut data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&a, &data).unwrap();
        std::fs::write(&b, &data).unwrap();
        data[150_000] ^= 1; // differs only in the middle: same sample, different full hash
        std::fs::write(&c, &data).unwrap();
        let p = |x: &std::path::Path| x.to_string_lossy().into_owned();
        let sa = sample_hash(&p(&a), 300_000).unwrap();
        let sc = sample_hash(&p(&c), 300_000).unwrap();
        assert_eq!(sa, sc);
        assert!(!sa.1);
        let cancel = AtomicBool::new(false);
        let mut buf = vec![0u8; 4096];
        let fa = full_hash(&p(&a), true, &mut buf, &cancel, &mut |_| {}).unwrap();
        let fb = full_hash(&p(&b), true, &mut buf, &cancel, &mut |_| {}).unwrap();
        let fc = full_hash(&p(&c), true, &mut buf, &cancel, &mut |_| {}).unwrap();
        assert_eq!(fa, fb);
        assert_ne!(fa.0, fc.0);
        assert_eq!(fa.0, *blake3::hash(&std::fs::read(&a).unwrap()).as_bytes());
        // small file: sample is the full hash
        std::fs::write(dir.path().join("s"), b"hello").unwrap();
        let s = sample_hash(&p(&dir.path().join("s")), 5).unwrap();
        assert_eq!(s, (*blake3::hash(b"hello").as_bytes(), true));
    }
}
