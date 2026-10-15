//! Everything the user needs to SEE, shown inside the app (Ravi, 15-10-2026: "Shows all files in the app only there
//! itself and does not get the user looking for the finished files").
//!
//! - Finished files: every file an action finished, read back from the action logs, with where it is now.
//! - Preview: what a file or folder looks like (image/pdf/video/audio via the window, text, folder listing, tables).
//! - Tables: a page of a CSV report, an action log or an Excel sheet, streamed so a million-row file stays quick.
//!
//! Google Drive and cloud-only files are never opened for a preview: reading them would download them.

use crate::model::*;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

// ------------------------------------------------------------------------------------------------------------------
// Finished files

fn action_from_log_name(name: &str) -> Option<ActionKind> {
    let first = name.split(' ').next().unwrap_or("");
    match first {
        "Extract" => Some(ActionKind::Extract),
        "Dedup" => Some(ActionKind::Dedup),
        "Transfer" => Some(ActionKind::Transfer),
        "CleanJunk" => Some(ActionKind::CleanJunk),
        _ => None, // Empty Recycle Bin and Publish to Supabase do not finish files
    }
}

fn parse_local(text: &str) -> i64 {
    use chrono::TimeZone;
    match chrono::NaiveDateTime::parse_from_str(text.trim(), "%d-%m-%Y %H:%M:%S") {
        Ok(n) => match chrono::Local.from_local_datetime(&n) {
            chrono::LocalResult::Single(t) | chrono::LocalResult::Ambiguous(t, _) => t.timestamp_millis(),
            chrono::LocalResult::None => 0,
        },
        Err(_) => 0,
    }
}

fn file_name(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_string()
}

/// Which log files exist (path, size, modified) — changes whenever an action writes a row.
pub fn logs_signature(logs_dir: &Path) -> Vec<(String, u64, i64)> {
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir(logs_dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().map(|x| x.eq_ignore_ascii_case("csv")).unwrap_or(false) {
                if let Ok(md) = e.metadata() {
                    v.push((p.to_string_lossy().into_owned(), md.len(), md.modified().map(crate::util::system_time_ms).unwrap_or(0)));
                }
            }
        }
    }
    v.sort();
    v
}

/// Every finished file in every action log, latest first. `exists` is filled in per page.
pub fn load_finished(logs_dir: &Path) -> Vec<FinishedRow> {
    let mut logs: Vec<(String, i64, ActionKind)> = logs_signature(logs_dir)
        .into_iter()
        .filter_map(|(p, _, m)| action_from_log_name(&file_name(&p)).map(|a| (p, m, a)))
        .collect();
    logs.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.cmp(&a.0)));
    let mut out = Vec::new();
    for (path, _, action) in logs {
        let Ok(mut r) = csv::ReaderBuilder::new().flexible(true).has_headers(true).from_path(&path) else { continue };
        let mut rows: Vec<FinishedRow> = Vec::new();
        for rec in r.records().flatten() {
            let f = |i: usize| rec.get(i).unwrap_or("").to_string();
            let (time, result, source, target, bytes, message) = (f(0), f(1), f(2), f(3), f(4), f(5));
            if source.is_empty() || message == "empty folder removed" {
                continue;
            }
            let result = match result.as_str() {
                "ok" => FinishedResult::Ok,
                "skipped" => FinishedResult::Skipped,
                _ => FinishedResult::Failed,
            };
            let now_at = match result {
                FinishedResult::Ok => match action {
                    ActionKind::Transfer | ActionKind::Extract => target.clone(),
                    _ => target.strip_prefix("moved to ").map(|s| s.to_string()).unwrap_or_default(),
                },
                _ => source.clone(),
            };
            let kept_at = if action == ActionKind::Dedup && result == FinishedResult::Ok {
                message
                    .strip_prefix("identical to ")
                    .map(|rest| rest.split(" — tree law:").next().unwrap_or(rest).trim().to_string())
                    .unwrap_or_default()
            } else {
                String::new()
            };
            rows.push(FinishedRow {
                id: 0,
                when_ms: parse_local(&time),
                action,
                result,
                name: file_name(&source),
                source,
                target,
                now_at,
                exists: false,
                kept_at,
                bytes: bytes.parse().unwrap_or(0),
                message,
                log_path: path.clone(),
            });
        }
        rows.reverse();
        out.extend(rows);
    }
    for (i, r) in out.iter_mut().enumerate() {
        r.id = i as u32;
    }
    out
}

pub fn finished_page(all: &[FinishedRow], q: &FinishedQuery) -> FinishedPage {
    let needle = q.search.as_ref().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
    let hits: Vec<&FinishedRow> = all
        .iter()
        .filter(|r| q.action.is_none_or(|a| r.action == a))
        .filter(|r| q.result.is_none_or(|x| r.result == x))
        .filter(|r| q.log_path.as_ref().is_none_or(|l| r.log_path.eq_ignore_ascii_case(l)))
        .filter(|r| {
            needle.as_ref().is_none_or(|n| {
                r.name.to_lowercase().contains(n.as_str())
                    || r.source.to_lowercase().contains(n.as_str())
                    || r.target.to_lowercase().contains(n.as_str())
                    || r.message.to_lowercase().contains(n.as_str())
            })
        })
        .collect();
    FinishedPage {
        total: hits.len() as u64,
        bytes: hits.iter().filter(|r| r.result == FinishedResult::Ok).map(|r| r.bytes).sum(),
        rows: hits
            .iter()
            .skip(q.offset as usize)
            .take(q.limit.clamp(1, 1000) as usize)
            .map(|r| {
                let mut r = (*r).clone();
                r.exists = !r.now_at.is_empty() && Path::new(&crate::util::long_path(&r.now_at)).exists();
                r
            })
            .collect(),
    }
}

// ------------------------------------------------------------------------------------------------------------------
// Preview

const TEXT_LIMIT: u64 = 256 * 1024;
const ENTRY_LIMIT: usize = 1000;

fn kind_for_ext(ext: &str) -> Option<PreviewKind> {
    Some(match ext {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "svg" | "ico" | "avif" => PreviewKind::Image,
        "pdf" => PreviewKind::Pdf,
        "mp4" | "webm" | "m4v" | "mov" => PreviewKind::Video,
        "mp3" | "wav" | "ogg" | "oga" | "m4a" | "aac" | "flac" | "opus" => PreviewKind::Audio,
        "csv" | "xlsx" => PreviewKind::Table,
        "txt" | "md" | "log" | "json" | "xml" | "yml" | "yaml" | "ini" | "cfg" | "conf" | "toml" | "ps1" | "bat" | "cmd" | "py"
        | "rs" | "ts" | "tsx" | "js" | "jsx" | "html" | "htm" | "css" | "sql" | "ahk" | "sh" | "cs" | "java" | "kt" | "c" | "h"
        | "cpp" | "go" | "rb" | "php" | "reg" | "inf" | "nfo" | "srt" | "vtt" | "tsv" | "env" => PreviewKind::Text,
        _ => return None,
    })
}

/// Cloud-only placeholder (OneDrive / Google Drive for Desktop): reading it downloads it.
fn is_cloud_only(md: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        md.file_attributes() & (0x1000 | 0x40000 | 0x400000) != 0
    }
    #[cfg(not(windows))]
    {
        let _ = md;
        false
    }
}

/// Text from bytes: UTF-16 with a BOM (common for Windows text files), else UTF-8 (lossy).
fn decode_text(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        let units: Vec<u16> = bytes[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&units);
    }
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let units: Vec<u16> = bytes[2..].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&units);
    }
    let b = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    String::from_utf8_lossy(b).into_owned()
}

fn looks_like_text(sample: &[u8]) -> bool {
    if sample.is_empty() {
        return true;
    }
    if sample.starts_with(&[0xFF, 0xFE]) || sample.starts_with(&[0xFE, 0xFF]) {
        return true;
    }
    if sample.contains(&0) {
        return false;
    }
    let s = String::from_utf8_lossy(sample);
    let bad = s.chars().filter(|&c| c == '\u{FFFD}' || (c.is_control() && !matches!(c, '\n' | '\r' | '\t' | '\x0C'))).count();
    bad * 100 <= s.chars().count().max(1) * 2
}

/// `cloud_roots`: roots of Google Drive volumes (never read for a preview).
pub fn preview(path: &str, cloud_roots: &[String]) -> FilePreview {
    let name = file_name(path);
    let mut p = FilePreview {
        path: path.to_string(),
        name: if name.is_empty() { path.to_string() } else { name.clone() },
        kind: PreviewKind::None,
        exists: false,
        is_dir: false,
        size: 0,
        modified_ms: 0,
        text: None,
        truncated: false,
        entries: None,
        entries_total: 0,
        note: None,
    };
    let lp = crate::util::long_path(path);
    let Ok(md) = std::fs::metadata(&lp) else {
        p.note = Some("This file is not there any more.".into());
        return p;
    };
    p.exists = true;
    p.is_dir = md.is_dir();
    p.size = if md.is_dir() { 0 } else { md.len() };
    p.modified_ms = md.modified().map(crate::util::system_time_ms).unwrap_or(0);
    if md.is_dir() {
        // Listing a folder never downloads anything, Google Drive included.
        match crate::walk::list_dir(path) {
            Ok(mut items) => {
                items.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
                p.entries_total = items.len() as u64;
                p.entries = Some(
                    items
                        .into_iter()
                        .take(ENTRY_LIMIT)
                        .map(|it| PreviewEntry {
                            path: crate::util::join(path, &it.name),
                            name: it.name,
                            is_dir: it.is_dir,
                            size: it.size,
                            modified_ms: it.modified,
                        })
                        .collect(),
                );
                p.kind = PreviewKind::Folder;
            }
            Err(e) => p.note = Some(format!("This folder cannot be read: {e}")),
        }
        return p;
    }
    let on_gdrive = cloud_roots.iter().any(|r| crate::util::starts_with_path(path, r));
    if on_gdrive || is_cloud_only(&md) {
        p.note = Some(if on_gdrive {
            "On Google Drive — showing it here would download it. Use Open in Windows if you want it.".into()
        } else {
            "Cloud-only file — showing it here would download it. Use Open in Windows if you want it.".into()
        });
        return p;
    }
    let ext = crate::util::ext_of(&name);
    let kind = kind_for_ext(&ext);
    match kind {
        Some(PreviewKind::Text) | None => {
            let mut f = match std::fs::File::open(&lp) {
                Ok(f) => f,
                Err(e) => {
                    p.note = Some(format!("Cannot open it: {e}"));
                    return p;
                }
            };
            let mut buf = Vec::with_capacity(TEXT_LIMIT.min(p.size) as usize);
            let _ = (&mut f).take(TEXT_LIMIT).read_to_end(&mut buf);
            if kind.is_some() || looks_like_text(&buf[..buf.len().min(8192)]) {
                p.kind = PreviewKind::Text;
                p.truncated = p.size > TEXT_LIMIT;
                p.text = Some(decode_text(&buf));
            } else {
                p.note = Some(format!(
                    "No preview for {} files — use Open in Windows.",
                    if ext.is_empty() { "these".to_string() } else { format!(".{ext}") }
                ));
            }
        }
        Some(k) => p.kind = k,
    }
    p
}

// ------------------------------------------------------------------------------------------------------------------
// Tables

/// Remembers where every row of a CSV starts, so any page is one seek away.
#[derive(Default)]
pub struct TableCache {
    csv: HashMap<String, CsvIndex>,
    search: Option<(String, String, Vec<csv::Position>)>,
    xlsx_counts: HashMap<(String, String, String), u64>,
}

struct CsvIndex {
    sig: (u64, i64),
    headers: Vec<String>,
    rows: Vec<csv::Position>,
}

fn file_sig(path: &str) -> (u64, i64) {
    std::fs::metadata(crate::util::long_path(path))
        .map(|m| (m.len(), m.modified().map(crate::util::system_time_ms).unwrap_or(0)))
        .unwrap_or((0, 0))
}

fn record_strings(r: &csv::StringRecord) -> Vec<String> {
    r.iter().map(|s| s.to_string()).collect()
}

fn csv_reader(path: &str) -> Result<csv::Reader<std::fs::File>, String> {
    csv::ReaderBuilder::new()
        .flexible(true)
        .has_headers(true)
        .from_path(crate::util::long_path(path))
        .map_err(|e| format!("Cannot open {path}: {e}"))
}

fn read_csv(q: &TableQuery, cache: &mut TableCache) -> Result<TablePage, String> {
    let sig = file_sig(&q.path);
    if cache.csv.get(&q.path).is_none_or(|c| c.sig != sig) {
        let mut r = csv_reader(&q.path)?;
        let mut headers = record_strings(r.headers().map_err(|e| e.to_string())?);
        if let Some(h) = headers.first_mut() {
            *h = h.trim_start_matches('\u{feff}').to_string();
        }
        let mut rows = Vec::new();
        let mut rec = csv::StringRecord::new();
        loop {
            let pos = r.position().clone();
            match r.read_record(&mut rec) {
                Ok(true) => rows.push(pos),
                Ok(false) => break,
                Err(_) => continue,
            }
        }
        cache.csv.insert(q.path.clone(), CsvIndex { sig, headers, rows });
        cache.search = None;
    }
    let idx = &cache.csv[&q.path];
    let needle = q.search.as_ref().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
    let positions: Vec<csv::Position> = match &needle {
        None => idx.rows.clone(),
        Some(n) => {
            let reuse = matches!(&cache.search, Some((p, s, _)) if *p == q.path && s == n);
            if !reuse {
                let mut r = csv_reader(&q.path)?;
                let mut hits = Vec::new();
                let mut rec = csv::StringRecord::new();
                loop {
                    let pos = r.position().clone();
                    match r.read_record(&mut rec) {
                        Ok(true) => {
                            if rec.iter().any(|f| f.to_lowercase().contains(n.as_str())) {
                                hits.push(pos);
                            }
                        }
                        Ok(false) => break,
                        Err(_) => continue,
                    }
                }
                cache.search = Some((q.path.clone(), n.clone(), hits));
            }
            cache.search.as_ref().map(|s| s.2.clone()).unwrap_or_default()
        }
    };
    let mut r = csv_reader(&q.path)?;
    let mut out = Vec::new();
    let mut rec = csv::StringRecord::new();
    for pos in positions.iter().skip(q.offset as usize).take(q.limit.clamp(1, 1000) as usize) {
        r.seek(pos.clone()).map_err(|e| e.to_string())?;
        if r.read_record(&mut rec).map_err(|e| e.to_string())? {
            out.push(record_strings(&rec));
        }
    }
    Ok(TablePage {
        path: q.path.clone(),
        sheets: Vec::new(),
        sheet: None,
        headers: idx.headers.clone(),
        total: positions.len() as u64,
        rows: out,
        note: None,
    })
}

fn cell_text(v: &calamine::DataRef) -> String {
    use calamine::DataRef::*;
    match v {
        Int(i) => i.to_string(),
        Float(f) => {
            if f.fract() == 0.0 && f.abs() < 1e15 {
                format!("{}", *f as i64)
            } else {
                let s = format!("{f:.6}");
                s.trim_end_matches('0').trim_end_matches('.').to_string()
            }
        }
        String(s) => s.clone(),
        SharedString(s) => s.to_string(),
        Bool(b) => if *b { "TRUE".into() } else { "FALSE".into() },
        DateTime(d) => {
            // Excel serial days since 1899-12-30 (the workbooks this app writes use the 1900 system).
            let secs = (d.as_f64() * 86_400.0).round() as i64;
            chrono::NaiveDate::from_ymd_opt(1899, 12, 30)
                .and_then(|base| base.and_hms_opt(0, 0, 0))
                .map(|b| (b + chrono::Duration::seconds(secs)).format("%d-%m-%Y %H:%M").to_string())
                .unwrap_or_default()
        }
        DateTimeIso(s) | DurationIso(s) => s.clone(),
        Error(e) => format!("#{e:?}"),
        Empty => std::string::String::new(),
    }
}

fn read_xlsx(q: &TableQuery, cache: &mut TableCache) -> Result<TablePage, String> {
    use calamine::Reader;
    let file = std::fs::File::open(crate::util::long_path(&q.path)).map_err(|e| format!("Cannot open {}: {e}", q.path))?;
    let mut wb: calamine::Xlsx<_> = calamine::Xlsx::new(std::io::BufReader::new(file)).map_err(|e| format!("Not a readable Excel file: {e}"))?;
    let sheets = wb.sheet_names();
    let sheet = q.sheet.clone().filter(|s| sheets.contains(s)).or_else(|| sheets.first().cloned()).ok_or("The workbook has no sheets.")?;
    let needle = q.search.as_ref().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
    let mut reader = wb.worksheet_cells_reader(&sheet).map_err(|e| e.to_string())?;
    let dims = reader.dimensions();
    let ncols = (dims.end.1.saturating_sub(dims.start.1) + 1) as usize;
    let first_row = dims.start.0;
    let want_from = q.offset as u64;
    let want = q.limit.clamp(1, 1000) as u64;
    let count_key = (q.path.clone(), sheet.clone(), needle.clone().unwrap_or_default());
    let known_total = match &needle {
        None => Some((dims.end.0.saturating_sub(dims.start.0)) as u64),
        Some(_) => cache.xlsx_counts.get(&count_key).copied(),
    };
    let mut headers: Vec<String> = Vec::new();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut cur_row: Option<u32> = None;
    let mut cur: Vec<String> = vec![String::new(); ncols];
    let mut matched: u64 = 0;
    let finish_row = |r: u32, cells: &mut Vec<String>, headers: &mut Vec<String>, rows: &mut Vec<Vec<String>>, matched: &mut u64| {
        let row = std::mem::replace(cells, vec![String::new(); ncols]);
        if r == first_row {
            *headers = row;
            return;
        }
        if let Some(n) = &needle {
            if !row.iter().any(|c| c.to_lowercase().contains(n.as_str())) {
                return;
            }
        }
        if *matched >= want_from && *matched < want_from + want {
            rows.push(row);
        }
        *matched += 1;
    };
    loop {
        // Enough rows, and the total is already known: stop reading.
        if known_total.is_some() && matched >= want_from + want {
            break;
        }
        let Some(cell) = reader.next_cell().map_err(|e| e.to_string())? else { break };
        let (r, c) = cell.get_position();
        if cur_row != Some(r) {
            if let Some(prev) = cur_row {
                finish_row(prev, &mut cur, &mut headers, &mut rows, &mut matched);
            }
            cur_row = Some(r);
        }
        let ci = c.saturating_sub(dims.start.1) as usize;
        if ci < ncols {
            cur[ci] = cell_text(cell.get_value());
        }
    }
    if let Some(prev) = cur_row {
        if !(known_total.is_some() && matched >= want_from + want) {
            finish_row(prev, &mut cur, &mut headers, &mut rows, &mut matched);
        }
    }
    let total = match known_total {
        Some(t) => t,
        None => {
            cache.xlsx_counts.insert(count_key, matched);
            matched
        }
    };
    Ok(TablePage {
        path: q.path.clone(),
        sheets,
        sheet: Some(sheet),
        headers,
        total,
        rows,
        note: if total > 100_000 { Some("A big sheet: pages further in take a few seconds. The CSV next to it opens faster.".into()) } else { None },
    })
}

pub fn read_table(q: &TableQuery, cache: &mut TableCache) -> Result<TablePage, String> {
    match crate::util::ext_of(&q.path).as_str() {
        "csv" | "tsv" => read_csv(q, cache),
        "xlsx" => read_xlsx(q, cache),
        other => Err(format!("Tables open for .csv and .xlsx files, not .{other}.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finished_rows_from_logs() {
        let d = tempfile::tempdir().unwrap();
        let logs = d.path().join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(
            logs.join("Dedup 15-10-2026 053856.csv"),
            "\u{feff}time,result,source,target,bytes,message\n\
             15-10-2026 05:38:57,ok,D:\\Downloads\\notes.pdf,Recycle Bin,300,identical to F:\\Papers\\notes.pdf — tree law: loose in a dumping ground\n\
             15-10-2026 05:38:58,skipped,D:\\x\\a.pdf,,10,changed since the scan — rescan\n",
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(
            logs.join("Transfer 15-10-2026 060000.csv"),
            "time,result,source,target,bytes,message\n\
             15-10-2026 06:00:01,ok,E:\\Photos\\a.jpg,G:\\My Drive\\Laptop Offload\\E\\Photos\\a.jpg,5000,copied and checked\n\
             15-10-2026 06:00:02,ok,E:\\Photos,,0,empty folder removed\n",
        )
        .unwrap();
        std::fs::write(logs.join("EmptyRecycleBin 15-10-2026 070000.csv"), "time,result,source,target,bytes,message\n15-10-2026 07:00:00,ok,C:\\,,0,emptied\n").unwrap();
        let all = load_finished(&logs);
        assert_eq!(all.len(), 3, "folder-removal rows and Empty Recycle Bin are not files");
        assert_eq!(all[0].action, ActionKind::Transfer, "latest log first");
        assert_eq!(all[0].now_at, "G:\\My Drive\\Laptop Offload\\E\\Photos\\a.jpg");
        let dedup_ok = all.iter().find(|r| r.result == FinishedResult::Ok && r.action == ActionKind::Dedup).unwrap();
        assert_eq!(dedup_ok.now_at, "", "in the Recycle Bin");
        assert_eq!(dedup_ok.kept_at, "F:\\Papers\\notes.pdf");
        let skipped = all.iter().find(|r| r.result == FinishedResult::Skipped).unwrap();
        assert_eq!(skipped.now_at, "D:\\x\\a.pdf", "a skipped file is still where it was");
        assert!(skipped.when_ms > 0);
        let q = FinishedQuery { action: None, result: Some(FinishedResult::Ok), search: Some("notes".into()), log_path: None, offset: 0, limit: 50 };
        let page = finished_page(&all, &q);
        assert_eq!(page.total, 1);
        assert_eq!(page.bytes, 300);
    }

    #[test]
    fn preview_kinds() {
        let d = tempfile::tempdir().unwrap();
        let p = |n: &str| d.path().join(n).to_string_lossy().into_owned();
        std::fs::write(d.path().join("a.txt"), "hello\nरवि").unwrap();
        std::fs::write(d.path().join("u16.txt"), [0xFF, 0xFE, b'h', 0, b'i', 0]).unwrap();
        std::fs::write(d.path().join("b.jpg"), [0xFF, 0xD8, 0xFF]).unwrap();
        std::fs::write(d.path().join("blob.bin"), [0u8, 1, 2, 3, 0, 0]).unwrap();
        std::fs::write(d.path().join("notes"), "plain words without an extension").unwrap();
        std::fs::create_dir_all(d.path().join("sub")).unwrap();
        let t = preview(&p("a.txt"), &[]);
        assert_eq!((t.kind, t.text.as_deref()), (PreviewKind::Text, Some("hello\nरवि")));
        assert_eq!(preview(&p("u16.txt"), &[]).text.as_deref(), Some("hi"));
        assert_eq!(preview(&p("b.jpg"), &[]).kind, PreviewKind::Image);
        let b = preview(&p("blob.bin"), &[]);
        assert_eq!(b.kind, PreviewKind::None);
        assert!(b.note.unwrap().contains(".bin"));
        assert_eq!(preview(&p("notes"), &[]).kind, PreviewKind::Text);
        let f = preview(&d.path().to_string_lossy(), &[]);
        assert_eq!(f.kind, PreviewKind::Folder);
        assert_eq!(f.entries.as_ref().unwrap()[0].name, "sub", "folders first");
        assert_eq!(f.entries_total, 6);
        let g = preview(&p("b.jpg"), &[d.path().to_string_lossy().into_owned()]);
        assert_eq!(g.kind, PreviewKind::None, "never read a Google Drive file");
        assert!(!preview(&p("gone.txt"), &[]).exists);
    }

    #[test]
    fn csv_and_xlsx_tables() {
        let d = tempfile::tempdir().unwrap();
        let csvp = d.path().join("r.csv");
        let mut s = String::from("\u{feff}Name,Size\n");
        for i in 0..2500 {
            s.push_str(&format!("file{i}.txt,{i}\n"));
        }
        std::fs::write(&csvp, s).unwrap();
        let mut cache = TableCache::default();
        let q = |offset, search: Option<&str>| TableQuery { path: csvp.to_string_lossy().into_owned(), sheet: None, search: search.map(|s| s.into()), offset, limit: 200 };
        let p = read_table(&q(2400, None), &mut cache).unwrap();
        assert_eq!(p.headers, vec!["Name", "Size"]);
        assert_eq!(p.total, 2500);
        assert_eq!(p.rows.len(), 100);
        assert_eq!(p.rows[0], vec!["file2400.txt", "2400"]);
        let p = read_table(&q(0, Some("FILE24")), &mut cache).unwrap();
        assert_eq!(p.total, 111, "file24, file240-249, file2400-2499");

        // An xlsx written by rust_xlsxwriter, read back page by page.
        let xp = d.path().join("w.xlsx");
        let mut wb = rust_xlsxwriter::Workbook::new();
        let ws = wb.add_worksheet().set_name("Summary").unwrap();
        ws.write_string(0, 0, "Fact").unwrap();
        ws.write_string(0, 1, "Value").unwrap();
        ws.write_string(1, 0, "Files").unwrap();
        ws.write_number(1, 1, 60256.0).unwrap();
        let ws2 = wb.add_worksheet().set_name("Files").unwrap();
        ws2.write_string(0, 0, "Path").unwrap();
        for i in 0..1500u32 {
            ws2.write_string(i + 1, 0, format!("C:\\x\\{i}.txt")).unwrap();
        }
        wb.save(&xp).unwrap();
        let xq = |sheet: Option<&str>, offset, search: Option<&str>| TableQuery {
            path: xp.to_string_lossy().into_owned(),
            sheet: sheet.map(|s| s.into()),
            search: search.map(|s| s.into()),
            offset,
            limit: 200,
        };
        let s1 = read_table(&xq(None, 0, None), &mut cache).unwrap();
        assert_eq!(s1.sheets, vec!["Summary", "Files"]);
        assert_eq!(s1.headers, vec!["Fact", "Value"]);
        assert_eq!(s1.rows, vec![vec!["Files".to_string(), "60256".to_string()]]);
        let f = read_table(&xq(Some("Files"), 1400, None), &mut cache).unwrap();
        assert_eq!(f.total, 1500);
        assert_eq!(f.rows.len(), 100);
        assert_eq!(f.rows[0], vec!["C:\\x\\1400.txt".to_string()]);
        let s = read_table(&xq(Some("Files"), 0, Some("\\14")), &mut cache).unwrap();
        assert_eq!(s.total, 111, "14, 140-149, 1400-1499");
    }
}
