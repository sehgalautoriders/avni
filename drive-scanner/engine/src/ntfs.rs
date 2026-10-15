//! NTFS master file table (MFT) reader.
//!
//! Instead of asking Windows for every folder (millions of calls, seek-bound on an HDD), this reads the $MFT straight off
//! the volume in large sequential chunks and parses every file record in parallel. That is how WizTree / Everything list a
//! whole drive in seconds: a drive with 1 million files has a ~1 GB MFT, which an HDD reads in ~8-10 s.
//!
//! Generic over `Read + Seek`: on Windows the source is the raw volume (`\\.\C:`, needs admin); tests feed it NTFS
//! images made with mkntfs + ntfs-3g and compare the result with a normal folder walk of the same image.

use crate::util::filetime_to_unix_ms;
use rayon::prelude::*;
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};

pub const ROOT_FRN: u64 = 5;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const CHUNK: usize = 8 << 20;

#[derive(Clone, Debug)]
pub struct Boot {
    pub bytes_per_sector: u32,
    pub cluster_size: u64,
    pub mft_lcn: u64,
    pub record_size: usize,
    pub total_sectors: u64,
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

pub fn parse_boot(b: &[u8]) -> Result<Boot, String> {
    if b.len() < 512 || &b[3..11] != b"NTFS    " {
        return Err("not an NTFS volume (boot sector signature missing)".into());
    }
    let bps = u16_at(b, 0x0B) as u32;
    if !(256..=4096).contains(&bps) || !bps.is_power_of_two() {
        return Err(format!("bad bytes-per-sector {bps}"));
    }
    let spc_raw = b[0x0D];
    let spc: u64 = if spc_raw <= 0x80 { spc_raw as u64 } else { 1u64 << (256 - spc_raw as u32) };
    if spc == 0 {
        return Err("bad sectors-per-cluster 0".into());
    }
    let cluster_size = bps as u64 * spc;
    let cpr = b[0x40] as i8;
    let record_size: u64 = if cpr > 0 { cpr as u64 * cluster_size } else { 1u64 << (-(cpr as i32)) as u32 };
    if !(512..=65536).contains(&record_size) || !record_size.is_multiple_of(512) {
        return Err(format!("bad MFT record size {record_size}"));
    }
    Ok(Boot {
        bytes_per_sector: bps,
        cluster_size,
        mft_lcn: u64_at(b, 0x30),
        record_size: record_size as usize,
        total_sectors: u64_at(b, 0x28),
    })
}

/// Applies the update-sequence fixups (NTFS writes the last 2 bytes of every 512-byte block elsewhere to detect torn
/// writes). Returns false when the record is torn or malformed.
pub fn apply_fixup(rec: &mut [u8]) -> bool {
    if rec.len() < 48 {
        return false;
    }
    let off = u16_at(rec, 4) as usize;
    let cnt = u16_at(rec, 6) as usize;
    if cnt < 2 || off + 2 * cnt > rec.len() || (cnt - 1) * 512 > rec.len() {
        return false;
    }
    let usn = [rec[off], rec[off + 1]];
    for i in 1..cnt {
        let pos = i * 512 - 2;
        if rec[pos] != usn[0] || rec[pos + 1] != usn[1] {
            return false;
        }
        rec[pos] = rec[off + 2 * i];
        rec[pos + 1] = rec[off + 2 * i + 1];
    }
    true
}

/// One data run: `lcn` None = sparse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    pub lcn: Option<u64>,
    pub len: u64,
}

pub fn decode_runs(r: &[u8]) -> Vec<Run> {
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut lcn: i64 = 0;
    while i < r.len() {
        let h = r[i];
        if h == 0 {
            break;
        }
        let ls = (h & 0x0F) as usize;
        let os = (h >> 4) as usize;
        i += 1;
        if ls == 0 || ls > 8 || os > 8 || i + ls + os > r.len() {
            break;
        }
        let mut len: u64 = 0;
        for k in 0..ls {
            len |= (r[i + k] as u64) << (8 * k);
        }
        i += ls;
        if os == 0 {
            out.push(Run { lcn: None, len });
        } else {
            let mut d: i64 = 0;
            for k in 0..os {
                d |= (r[i + k] as i64) << (8 * k);
            }
            let shift = 64 - 8 * os as u32;
            d = (d << shift) >> shift; // sign-extend
            lcn += d;
            out.push(Run { lcn: Some(lcn as u64), len });
        }
        i += os;
    }
    out
}

#[derive(Clone, Debug, Default)]
pub struct StdInfo {
    pub created: u64,
    pub modified: u64,
    pub accessed: u64,
    pub attrs: u32,
}

#[derive(Clone, Debug)]
pub struct FileName {
    pub parent: u64,
    pub namespace: u8,
    pub name: String,
    pub created: u64,
    pub modified: u64,
    pub accessed: u64,
}

#[derive(Clone, Debug, Default)]
pub struct DataInfo {
    pub size: u64,
    pub alloc: u64,
    pub first_lcn: Option<u64>,
    pub resident: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Record {
    pub frn: u64,
    pub seq: u16,
    /// Some(base frn) for an extension record (attributes that did not fit in the base record).
    pub base: Option<u64>,
    pub is_dir: bool,
    pub si: Option<StdInfo>,
    pub names: Vec<FileName>,
    pub data: Option<DataInfo>,
    /// $ATTRIBUTE_LIST value when resident (only needed for record 0, $MFT itself).
    pub attr_list: Option<Vec<u8>>,
    /// $ATTRIBUTE_LIST runs when non-resident.
    pub attr_list_runs: Option<(Vec<Run>, u64)>,
    /// Runs of the unnamed $DATA piece in this record, with its starting VCN (record 0 / its extensions only).
    pub data_runs: Option<(u64, Vec<Run>)>,
}

fn resident_value(rec: &[u8], off: usize, alen: usize) -> Option<&[u8]> {
    let vlen = u32_at(rec, off + 0x10) as usize;
    let voff = u16_at(rec, off + 0x14) as usize;
    if voff + vlen > alen {
        return None;
    }
    rec.get(off + voff..off + voff + vlen)
}

/// Parses one MFT record (fixups already applied or not; this applies them). None = unused / not a file record.
pub fn parse_record(rec: &mut [u8], frn: u64, keep_runs: bool) -> Option<Record> {
    if rec.len() < 48 || &rec[0..4] != b"FILE" {
        return None;
    }
    if !apply_fixup(rec) {
        return None;
    }
    let flags = u16_at(rec, 0x16);
    if flags & 1 == 0 {
        return None;
    }
    let base_ref = u64_at(rec, 0x20);
    let mut r = Record {
        frn,
        seq: u16_at(rec, 0x10),
        base: if base_ref != 0 { Some(base_ref & 0x0000_FFFF_FFFF_FFFF) } else { None },
        is_dir: flags & 2 != 0,
        ..Default::default()
    };
    let used = (u32_at(rec, 0x18) as usize).min(rec.len());
    let mut off = u16_at(rec, 0x14) as usize;
    while off + 16 <= used {
        let ty = u32_at(rec, off);
        if ty == 0xFFFF_FFFF {
            break;
        }
        let alen = u32_at(rec, off + 4) as usize;
        if alen < 16 || off + alen > used {
            break;
        }
        let nonres = rec[off + 8] != 0;
        let name_len = rec[off + 9];
        match ty {
            0x10 if !nonres => {
                if let Some(v) = resident_value(rec, off, alen) {
                    if v.len() >= 0x24 {
                        r.si = Some(StdInfo {
                            created: u64_at(v, 0x00),
                            modified: u64_at(v, 0x08),
                            accessed: u64_at(v, 0x18),
                            attrs: u32_at(v, 0x20),
                        });
                    }
                }
            }
            0x20 if keep_runs => {
                if nonres {
                    if alen >= 0x40 {
                        let ro = u16_at(rec, off + 0x20) as usize;
                        if ro < alen {
                            let size = u64_at(rec, off + 0x30);
                            r.attr_list_runs = Some((decode_runs(&rec[off + ro..off + alen]), size));
                        }
                    }
                } else if let Some(v) = resident_value(rec, off, alen) {
                    r.attr_list = Some(v.to_vec());
                }
            }
            0x30 if !nonres => {
                if let Some(v) = resident_value(rec, off, alen) {
                    if v.len() >= 0x42 {
                        let nlen = v[0x40] as usize;
                        if 0x42 + nlen * 2 <= v.len() {
                            let units: Vec<u16> =
                                (0..nlen).map(|k| u16_at(v, 0x42 + 2 * k)).collect();
                            r.names.push(FileName {
                                parent: u64_at(v, 0) & 0x0000_FFFF_FFFF_FFFF,
                                namespace: v[0x41],
                                name: String::from_utf16_lossy(&units),
                                created: u64_at(v, 0x08),
                                modified: u64_at(v, 0x10),
                                accessed: u64_at(v, 0x20),
                            });
                        }
                    }
                }
            }
            0x80 if name_len == 0 => {
                if !nonres {
                    let vlen = u32_at(rec, off + 0x10) as u64;
                    r.data = Some(DataInfo { size: vlen, alloc: vlen, first_lcn: None, resident: true });
                } else if alen >= 0x40 {
                    let start_vcn = u64_at(rec, off + 0x10);
                    let ro = u16_at(rec, off + 0x20) as usize;
                    let runs = if ro < alen { decode_runs(&rec[off + ro..off + alen]) } else { Vec::new() };
                    if start_vcn == 0 {
                        r.data = Some(DataInfo {
                            size: u64_at(rec, off + 0x30),
                            alloc: u64_at(rec, off + 0x28),
                            first_lcn: runs.iter().find_map(|x| x.lcn),
                            resident: false,
                        });
                    }
                    if keep_runs {
                        r.data_runs = Some((start_vcn, runs));
                    }
                }
            }
            _ => {}
        }
        off += alen;
    }
    Some(r)
}

/// One name of one file = one row (hard links give several rows for one file; only the first is `primary`).
#[derive(Clone, Debug)]
pub struct MftEntry {
    pub frn: u64,
    pub parent: u64,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub alloc: u64,
    pub created: i64,
    pub modified: i64,
    pub accessed: i64,
    pub attrs: u32,
    pub links: u16,
    /// First cluster on disk (resident files: the cluster of their MFT record). Used to read in disk order.
    pub lcn: u64,
    pub primary: bool,
}

pub struct MftScan {
    pub boot: Boot,
    pub entries: Vec<MftEntry>,
    pub records_total: u64,
    pub records_in_use: u64,
    pub mft_bytes: u64,
}

fn read_at<V: Read + Seek>(vol: &mut V, off: u64, buf: &mut [u8]) -> std::io::Result<()> {
    vol.seek(SeekFrom::Start(off))?;
    vol.read_exact(buf)
}

/// Reads clusters covering `[vcn_byte, vcn_byte+len)` of a run list.
fn read_runs_range<V: Read + Seek>(vol: &mut V, boot: &Boot, runs: &[Run], start: u64, len: usize) -> std::io::Result<Vec<u8>> {
    let cs = boot.cluster_size;
    let mut out = vec![0u8; len];
    let mut pos = 0usize;
    let mut vcn_byte = 0u64;
    for run in runs {
        let run_bytes = run.len * cs;
        let run_end = vcn_byte + run_bytes;
        let want = start + pos as u64;
        if pos < len && want < run_end && want >= vcn_byte {
            let in_run = want - vcn_byte;
            let take = ((run_end - want) as usize).min(len - pos);
            if let Some(lcn) = run.lcn {
                // Align to clusters for raw volume reads.
                let first_c = in_run / cs;
                let last_c = (in_run + take as u64).div_ceil(cs);
                let mut tmp = vec![0u8; ((last_c - first_c) * cs) as usize];
                read_at(vol, (lcn + first_c) * cs, &mut tmp)?;
                let s = (in_run - first_c * cs) as usize;
                out[pos..pos + take].copy_from_slice(&tmp[s..s + take]);
            }
            pos += take;
        }
        vcn_byte = run_end;
        if pos >= len {
            break;
        }
    }
    if pos < len {
        return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "range beyond run list"));
    }
    Ok(out)
}

/// Works out where the whole $MFT lives on disk (its own $DATA runs, following $ATTRIBUTE_LIST when the MFT is so
/// fragmented that its run list spilled into extension records).
fn mft_layout<V: Read + Seek>(vol: &mut V, boot: &Boot) -> Result<(Vec<Run>, u64), String> {
    let cs = boot.cluster_size;
    let rs = boot.record_size as u64;
    let read_len = rs.max(cs).div_ceil(cs) * cs;
    let mut buf = vec![0u8; read_len as usize];
    read_at(vol, boot.mft_lcn * cs, &mut buf).map_err(|e| format!("read $MFT record 0: {e}"))?;
    let rec0 = parse_record(&mut buf[..rs as usize], 0, true).ok_or("record 0 ($MFT) unreadable")?;
    let data = rec0.data.clone().ok_or("$MFT has no $DATA")?;
    let (vcn0, runs0) = rec0.data_runs.clone().ok_or("$MFT $DATA is not non-resident")?;
    if vcn0 != 0 {
        return Err("$MFT first $DATA piece does not start at VCN 0".into());
    }
    let list_bytes = if let Some(v) = rec0.attr_list.clone() {
        Some(v)
    } else if let Some((runs, size)) = rec0.attr_list_runs.clone() {
        Some(read_runs_range(vol, boot, &runs, 0, size as usize).map_err(|e| format!("read $MFT attribute list: {e}"))?)
    } else {
        None
    };
    let mut pieces: Vec<(u64, Vec<Run>)> = vec![(0, runs0.clone())];
    if let Some(list) = list_bytes {
        // Entries: type u32, length u16, name_len u8, name_off u8, start_vcn u64, segment ref u64, id u16.
        let mut o = 0usize;
        while o + 0x1A <= list.len() {
            let ty = u32_at(&list, o);
            let len = u16_at(&list, o + 4) as usize;
            if len == 0 {
                break;
            }
            let name_len = list[o + 6];
            let start_vcn = u64_at(&list, o + 8);
            let seg = u64_at(&list, o + 0x10) & 0x0000_FFFF_FFFF_FFFF;
            if ty == 0x80 && name_len == 0 && seg != 0 && start_vcn != 0 {
                // Extension records of $MFT sit inside the part of the MFT that record 0 already maps.
                let rec = read_runs_range(vol, boot, &runs0, seg * rs, rs as usize)
                    .map_err(|e| format!("read $MFT extension record {seg}: {e}"))?;
                let mut rec = rec;
                let ext = parse_record(&mut rec, seg, true).ok_or(format!("$MFT extension record {seg} unreadable"))?;
                if let Some((v, runs)) = ext.data_runs {
                    pieces.push((v, runs));
                }
            }
            o += len;
        }
    }
    pieces.sort_by_key(|p| p.0);
    let mut runs = Vec::new();
    for (_, r) in pieces {
        runs.extend(r);
    }
    Ok((runs, data.size))
}

/// Reads and parses the whole MFT. `progress(records_done, records_total)`.
pub fn read_mft<V: Read + Seek + Send>(
    mut vol: V,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &AtomicBool,
) -> Result<MftScan, String> {
    let mut first = vec![0u8; 4096];
    read_at(&mut vol, 0, &mut first).map_err(|e| format!("read boot sector: {e}"))?;
    let boot = parse_boot(&first)?;
    let (runs, mft_size) = mft_layout(&mut vol, &boot)?;
    let rs = boot.record_size;
    let cs = boot.cluster_size;
    let total = mft_size / rs as u64;

    // Reader thread: streams the MFT in disk order in large chunks; this thread parses them in parallel.
    let (tx, rx) = crossbeam_channel::bounded::<Result<(u64, Vec<u8>), String>>(3);
    let records: Vec<Record> = std::thread::scope(|s| {
        let reader = s.spawn(move || {
            let mut carry: Vec<u8> = Vec::new();
            let mut next_frn: u64 = 0;
            let mut stream_pos: u64 = 0;
            'outer: for run in &runs {
                let Some(lcn) = run.lcn else {
                    // Sparse MFT region: nothing to read; skip its records.
                    stream_pos += run.len * cs;
                    carry.clear();
                    next_frn = stream_pos / rs as u64;
                    continue;
                };
                let mut c = 0u64;
                while c < run.len {
                    if cancel.load(Ordering::Relaxed) || next_frn >= total {
                        break 'outer;
                    }
                    let n = ((CHUNK as u64 / cs).max(1)).min(run.len - c);
                    let mut buf = vec![0u8; (n * cs) as usize];
                    if let Err(e) = read_at(&mut vol, (lcn + c) * cs, &mut buf) {
                        let _ = tx.send(Err(format!("read MFT at cluster {}: {e}", lcn + c)));
                        return;
                    }
                    c += n;
                    stream_pos += n * cs;
                    let mut data = if carry.is_empty() { buf } else { [std::mem::take(&mut carry), buf].concat() };
                    let whole = data.len() / rs * rs;
                    if whole < data.len() {
                        carry = data.split_off(whole);
                    }
                    if !data.is_empty() {
                        let frn = next_frn;
                        next_frn += (data.len() / rs) as u64;
                        if tx.send(Ok((frn, data))).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        let mut out: Vec<Record> = Vec::new();
        let mut err: Option<String> = None;
        for msg in rx.iter() {
            match msg {
                Ok((frn0, mut data)) => {
                    let parsed: Vec<Record> = data
                        .par_chunks_mut(rs)
                        .enumerate()
                        .filter_map(|(i, rec)| {
                            let frn = frn0 + i as u64;
                            if frn >= total {
                                None
                            } else {
                                parse_record(rec, frn, false)
                            }
                        })
                        .collect();
                    out.extend(parsed);
                    progress(((frn0 as usize + data.len() / rs) as u64).min(total), total);
                }
                Err(e) => {
                    err = Some(e);
                    break;
                }
            }
        }
        drop(rx);
        let _ = reader.join();
        match err {
            Some(e) => Err(e),
            None => Ok(out),
        }
    })?;
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    let in_use = records.len() as u64;
    let entries = build_entries(records, &boot);
    Ok(MftScan { boot, entries, records_total: total, records_in_use: in_use, mft_bytes: mft_size })
}

/// Folds extension records into their base records and emits one entry per (non-DOS) name.
fn build_entries(mut records: Vec<Record>, boot: &Boot) -> Vec<MftEntry> {
    // records are in frn order (chunks arrive in order; par collect keeps order).
    let mut ext: Vec<Record> = Vec::new();
    records.retain_mut(|r| {
        if r.base.is_some() {
            ext.push(std::mem::take(r));
            false
        } else {
            true
        }
    });
    for e in ext {
        let base = e.base.unwrap();
        if let Ok(i) = records.binary_search_by_key(&base, |r| r.frn) {
            let b = &mut records[i];
            b.names.extend(e.names);
            if b.data.is_none() {
                b.data = e.data;
            }
            if b.si.is_none() {
                b.si = e.si;
            }
        }
    }
    let rs = boot.record_size as u64;
    let cs = boot.cluster_size;
    let mut out = Vec::with_capacity(records.len() + records.len() / 16);
    for r in records {
        let has_long = r.names.iter().any(|n| n.namespace != 2);
        let mut seen: Vec<(u64, &str)> = Vec::new();
        let names: Vec<&FileName> = r
            .names
            .iter()
            .filter(|n| !has_long || n.namespace != 2)
            .filter(|n| {
                if seen.iter().any(|(p, s)| *p == n.parent && *s == n.name) {
                    false
                } else {
                    seen.push((n.parent, &n.name));
                    true
                }
            })
            .collect();
        if names.is_empty() {
            continue;
        }
        let links = names.len().min(u16::MAX as usize) as u16;
        let (created, modified, accessed, mut attrs) = match &r.si {
            Some(si) => (si.created, si.modified, si.accessed, si.attrs),
            None => (names[0].created, names[0].modified, names[0].accessed, 0),
        };
        if r.is_dir {
            attrs |= FILE_ATTRIBUTE_DIRECTORY;
        }
        let (size, alloc, lcn) = match (&r.data, r.is_dir) {
            (_, true) => (0, 0, 0),
            (Some(d), false) => (d.size, d.alloc, d.first_lcn.unwrap_or(boot.mft_lcn + r.frn * rs / cs)),
            (None, false) => (0, 0, boot.mft_lcn + r.frn * rs / cs),
        };
        for (k, n) in names.iter().enumerate() {
            out.push(MftEntry {
                frn: r.frn,
                parent: n.parent,
                name: n.name.clone(),
                is_dir: r.is_dir,
                size,
                alloc,
                created: filetime_to_unix_ms(created),
                modified: filetime_to_unix_ms(modified),
                accessed: filetime_to_unix_ms(accessed),
                attrs,
                links,
                lcn,
                primary: k == 0,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_decode() {
        // 0x21: len 1 byte, off 2 bytes. len=0x18, lcn=0x5634; then 0x11 len 0x10 off -0x10 (0xF0); then sparse 0x01 len 5.
        let r = [0x21, 0x18, 0x34, 0x56, 0x11, 0x10, 0xF0, 0x01, 0x05, 0x00];
        let v = decode_runs(&r);
        assert_eq!(
            v,
            vec![
                Run { lcn: Some(0x5634), len: 0x18 },
                Run { lcn: Some(0x5634 - 0x10), len: 0x10 },
                Run { lcn: None, len: 5 }
            ]
        );
    }

    #[test]
    fn boot_rejects_non_ntfs() {
        assert!(parse_boot(&[0u8; 512]).is_err());
    }
}
