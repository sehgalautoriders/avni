//! Excel + CSV evaluation reports.
//!
//! One pass over every row (`ReportInput::for_each_row`) feeds, at the same time:
//!   * the "Plan" sheet(s)   - files whose verdict is neither Keep nor Protected,
//!   * the "Files" sheet(s)  - every file and folder (only when `include_all_files`),
//!   * one UTF-8 (BOM) CSV per drive - every file and folder.
//!
//! The workbook uses rust_xlsxwriter's constant-memory worksheets, so rows go to a temp file as they arrive and the
//! process never holds the whole table. Standalone constant-memory worksheets (not the `&mut` ones handed out by
//! `Workbook::add_worksheet_with_constant_memory`) are used so several of them can be written to in an interleaved
//! way; they are pushed into the workbook in the final sheet order just before saving.
//! Times are shown in the local time zone of the machine that writes the report.

use crate::model::{
    DriveRole, DriveSummary, DupGroup, ExtTotal, FileRow, FolderTotal, Media, ReportFiles, ScanProfile, Summary,
    Verdict,
};
use anyhow::{Context, Result};
use chrono::{Datelike, Local, NaiveDateTime, TimeZone, Timelike};
use rust_xlsxwriter::{Color, Format, FormatAlign, FormatBorder, Workbook, Worksheet};
use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Streams every row (files and folders), drive by drive: rows of one drive are contiguous, drives in summary order.
pub type RowVisitor<'a> = dyn Fn(&mut dyn FnMut(&FileRow) -> anyhow::Result<()>) -> anyhow::Result<()> + Sync + 'a;

pub struct ReportInput<'a> {
    pub summary: &'a Summary,
    pub dup_groups: &'a [DupGroup],
    pub largest: &'a [FileRow],
    pub extensions: &'a [ExtTotal],
    pub folders: &'a [FolderTotal],
    pub for_each_row: &'a RowVisitor<'a>,
}

pub struct ReportOptions {
    /// Put every file in the workbook too ("Files", "Files 2", ... at 1,048,575 rows each). CSV always has every file.
    pub include_all_files: bool,
    /// File-name stamp, "dd-mm-yyyy HHmm".
    pub stamp: String,
}

/// Data rows per sheet. Excel allows 1,048,576 rows including the header row.
const MAX_DATA_ROWS: u32 = 1_048_575;
/// A progress line every this many rows.
const PROGRESS_EVERY: u64 = 100_000;
/// "GB" in the report is 1024^3 bytes, the same unit Windows shows.
const GIB: f64 = 1_073_741_824.0;
const MIB: f64 = 1_048_576.0;
/// Excel's limit for the text of one cell.
const MAX_CELL_CHARS: usize = 32_767;
const DATE_FORMAT: &str = "dd-mm-yyyy hh:mm";
const HEADER_FILL: u32 = 0x0015_181C;
/// Local 1900-03-01 (serial 61) is the first date Excel shows without its 1900 leap-year quirk.
const EXCEL_MIN_SERIAL: i64 = 61;
const EXCEL_MAX_SERIAL: i64 = 2_958_465;
/// `NaiveDate::num_days_from_ce()` of 1899-12-30, the day Excel calls serial 0.
const EXCEL_EPOCH_FROM_CE: i64 = 693_594;

struct Col {
    name: &'static str,
    width: f64,
}

const fn col(name: &'static str, width: f64) -> Col {
    Col { name, width }
}

const PLAN_COLS: &[Col] = &[
    col("Verdict", 24.0),
    col("Reason", 46.0),
    col("Drive", 8.0),
    col("Path", 90.0),
    col("Size bytes", 16.0),
    col("Size MB", 11.0),
    col("Modified", 17.0),
    col("Duplicate group", 16.0),
    col("BLAKE3", 68.0),
    col("Sample hash", 68.0),
];

const DUP_COLS: &[Col] = &[
    col("Group", 9.0),
    col("Confirmed", 11.0),
    col("Role", 18.0),
    col("Size bytes", 16.0),
    col("Reclaimable in group (bytes)", 26.0),
    col("Hash", 68.0),
    col("Drive", 8.0),
    col("Path", 90.0),
    col("Modified", 17.0),
    col("Why (tree law)", 70.0),
];

const LARGEST_COLS: &[Col] = &[
    col("Path", 90.0),
    col("Drive", 8.0),
    col("Size bytes", 16.0),
    col("Size MB", 11.0),
    col("Modified", 17.0),
    col("Verdict", 24.0),
    col("Reason", 46.0),
];

const EXT_COLS: &[Col] = &[col("Extension", 14.0), col("Files", 12.0), col("Size bytes", 18.0), col("Size GB", 12.0)];

const FOLDER_COLS: &[Col] = &[
    col("Drive", 8.0),
    col("Path", 90.0),
    col("Files", 12.0),
    col("Size bytes", 18.0),
    col("Size GB", 12.0),
    col("Verdict", 24.0),
];

const FILES_COLS: &[Col] = &[
    col("Drive", 8.0),
    col("Path", 90.0),
    col("Name", 40.0),
    col("Ext", 9.0),
    col("Type", 8.0),
    col("Size bytes", 16.0),
    col("Created", 17.0),
    col("Modified", 17.0),
    col("Accessed", 17.0),
    col("Attributes", 14.0),
    col("Links", 7.0),
    col("BLAKE3", 68.0),
    col("SHA-256", 68.0),
    col("Sample hash", 68.0),
    col("Duplicate group", 16.0),
    col("Verdict", 24.0),
    col("Reason", 46.0),
];

/// Writes the workbook and the per-drive CSV files into `dir` (created when missing).
pub fn write_reports(
    input: &ReportInput,
    dir: &Path,
    opts: &ReportOptions,
    progress: &mut dyn FnMut(&str),
) -> anyhow::Result<ReportFiles> {
    write_reports_with_limit(input, dir, opts, progress, MAX_DATA_ROWS)
}

fn write_reports_with_limit(
    input: &ReportInput,
    dir: &Path,
    opts: &ReportOptions,
    progress: &mut dyn FnMut(&str),
    max_rows: u32,
) -> Result<ReportFiles> {
    let started = Instant::now();
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create report folder {}", dir.display()))?;

    let stamp = sanitize_name(&opts.stamp, '-');
    let xlsx_path = dir.join(format!("Avni Drive Scan {stamp}.xlsx"));

    let fm = Fmts::new();
    let mut wb = Workbook::new();
    // The constant-memory temp files can be large; keep them next to the report instead of on the system drive.
    // If the folder refuses, the default temp folder is used.
    let _ = wb.set_tempdir(dir);

    // ---- small sheets first ----
    progress("Sheet: Summary");
    let mut summary_ws = wb.new_worksheet_with_constant_memory();
    summary_ws.set_name("Summary")?;
    write_summary(&mut summary_ws, &fm, input.summary)?;

    progress("Sheet: Duplicates");
    let dup_sheets = write_duplicates(&mut wb, &fm, max_rows, progress, input.dup_groups)?;
    progress("Sheet: Largest");
    let largest_sheets = write_largest(&mut wb, &fm, max_rows, progress, input.largest)?;
    progress("Sheet: Extensions");
    let ext_sheets = write_extensions(&mut wb, &fm, max_rows, progress, input.extensions)?;
    progress("Sheet: Folders");
    let folder_sheets = write_folders(&mut wb, &fm, max_rows, progress, input.folders)?;

    // ---- the big pass: Plan + Files + CSV in one go ----
    progress("Sheet: Plan");
    let plan = SheetSet::new(&mut wb, &fm, "Plan", PLAN_COLS)?;
    let files = if opts.include_all_files {
        progress("Sheet: Files");
        Some(SheetSet::new(&mut wb, &fm, "Files", FILES_COLS)?)
    } else {
        None
    };
    let mut sink = Sink {
        wb,
        fm: &fm,
        plan,
        files,
        csv: CsvOut::new(dir, &stamp),
        rows: 0,
        reported: 0,
        max_rows,
        progress,
        attr: String::new(),
    };
    let pass = (input.for_each_row)(&mut |r: &FileRow| sink.on_row(r));
    let Sink { wb, plan, files, mut csv, rows, reported, progress, .. } = sink;
    if let Err(e) = pass.and_then(|()| csv.finish()) {
        csv.discard();
        return Err(e);
    }
    if rows != reported {
        progress(&format!("Files: {} rows", thousands(rows)));
    }

    // ---- assemble in the final order and save ----
    progress("Saving workbook");
    let part_path = dir.join(format!("Avni Drive Scan {stamp}.xlsx.part"));
    let first = Sheets {
        summary: summary_ws,
        dups: dup_sheets,
        largest: largest_sheets,
        exts: ext_sheets,
        folders: folder_sheets,
    };
    if let Err(e) = assemble_and_save(wb, first, plan, files, &part_path, &xlsx_path) {
        let _ = std::fs::remove_file(&part_path);
        csv.discard();
        return Err(e.context("cannot save the Excel report"));
    }

    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    progress("Reports done");
    Ok(ReportFiles {
        folder: dir.to_string_lossy().into_owned(),
        xlsx: xlsx_path.to_string_lossy().into_owned(),
        csv: csv.paths,
        rows,
        ms,
    })
}

/// The sheets that are complete before the big pass.
struct Sheets {
    summary: Worksheet,
    dups: Vec<Worksheet>,
    largest: Vec<Worksheet>,
    exts: Vec<Worksheet>,
    folders: Vec<Worksheet>,
}

/// Puts the sheets into the workbook in their final order (Summary, Plan, Duplicates, Largest, Extensions, Folders,
/// Files) and saves it. The file is written under a ".part" name first, so a failed save never leaves a half file.
fn assemble_and_save(
    mut wb: Workbook,
    first: Sheets,
    plan: SheetSet,
    files: Option<SheetSet>,
    part_path: &Path,
    xlsx_path: &Path,
) -> Result<()> {
    let plan_sheets = plan.finish()?;
    let files_sheets = match files {
        Some(f) => f.finish()?,
        None => Vec::new(),
    };
    wb.push_worksheet(first.summary);
    for ws in plan_sheets
        .into_iter()
        .chain(first.dups)
        .chain(first.largest)
        .chain(first.exts)
        .chain(first.folders)
        .chain(files_sheets)
    {
        wb.push_worksheet(ws);
    }
    wb.save(part_path)?;
    drop(wb);
    std::fs::rename(part_path, xlsx_path).with_context(|| format!("cannot write {}", xlsx_path.display()))
}

// ---------------------------------------------------------------------------------------------------------------
// Formats
// ---------------------------------------------------------------------------------------------------------------

struct Fmts {
    header: Format,
    title: Format,
    bold: Format,
    int: Format,
    mb: Format,
    gb: Format,
    secs: Format,
    date: Format,
    /// Left aligned values next to the key-fact labels on the Summary sheet.
    fact_int: Format,
    fact_gb: Format,
    fact_secs: Format,
    fact_date: Format,
    fact_text: Format,
    total_text: Format,
    total_int: Format,
    total_gb: Format,
    total_secs: Format,
}

impl Fmts {
    fn new() -> Self {
        let num = |f: &str| Format::new().set_num_format(f);
        let total = |f: Option<&str>| {
            let base = Format::new().set_bold().set_border_top(FormatBorder::Thin);
            match f {
                Some(f) => base.set_num_format(f),
                None => base,
            }
        };
        let fact = |f: &str| Format::new().set_num_format(f).set_align(FormatAlign::Left);
        Fmts {
            header: Format::new()
                .set_bold()
                .set_font_color(Color::White)
                .set_background_color(Color::RGB(HEADER_FILL))
                .set_align(FormatAlign::Left),
            title: Format::new().set_bold().set_font_size(16),
            bold: Format::new().set_bold(),
            int: num("#,##0"),
            mb: num("0.00"),
            gb: num("#,##0.00"),
            secs: num("#,##0.0"),
            date: num(DATE_FORMAT),
            fact_int: fact("#,##0"),
            fact_gb: fact("#,##0.00"),
            fact_secs: fact("#,##0.0"),
            fact_date: fact(DATE_FORMAT),
            fact_text: Format::new().set_align(FormatAlign::Left),
            total_text: total(None),
            total_int: total(Some("#,##0")),
            total_gb: total(Some("#,##0.00")),
            total_secs: total(Some("#,##0.0")),
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Small cell helpers
// ---------------------------------------------------------------------------------------------------------------

/// Text of one cell, cut to Excel's limit (a longer string makes the library return an error).
fn clip(s: &str) -> &str {
    if s.len() <= MAX_CELL_CHARS {
        return s;
    }
    match s.char_indices().nth(MAX_CELL_CHARS) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

fn put_str(ws: &mut Worksheet, row: u32, col: u16, s: &str) -> Result<()> {
    if !s.is_empty() {
        ws.write_string(row, col, clip(s))?;
    }
    Ok(())
}

fn put_str_fmt(ws: &mut Worksheet, row: u32, col: u16, s: &str, f: &Format) -> Result<()> {
    ws.write_string_with_format(row, col, clip(s), f)?;
    Ok(())
}

fn put_num(ws: &mut Worksheet, row: u32, col: u16, v: f64, f: &Format) -> Result<()> {
    ws.write_number_with_format(row, col, v, f)?;
    Ok(())
}

fn put_plain_num(ws: &mut Worksheet, row: u32, col: u16, v: f64) -> Result<()> {
    ws.write_number(row, col, v)?;
    Ok(())
}

fn put_opt_str(ws: &mut Worksheet, row: u32, col: u16, s: Option<&str>) -> Result<()> {
    match s {
        Some(s) => put_str(ws, row, col, s),
        None => Ok(()),
    }
}

fn put_stamp(ws: &mut Worksheet, row: u32, col: u16, st: Option<Stamp>, f: &Format) -> Result<()> {
    match st {
        Some(st) => put_num(ws, row, col, st.serial, f),
        None => Ok(()),
    }
}

fn put_group(ws: &mut Worksheet, row: u32, col: u16, g: Option<u32>) -> Result<()> {
    match g {
        Some(g) => put_plain_num(ws, row, col, f64::from(g)),
        None => Ok(()),
    }
}

fn bytes_f(b: u64) -> f64 {
    b as f64
}

fn gb_f(b: u64) -> f64 {
    b as f64 / GIB
}

fn mb_f(b: u64) -> f64 {
    b as f64 / MIB
}

// ---------------------------------------------------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------------------------------------------------

/// A moment in local time, both as a calendar value (CSV) and as an Excel serial number (workbook).
#[derive(Clone, Copy)]
struct Stamp {
    dt: NaiveDateTime,
    serial: f64,
}

/// Unix ms UTC -> local time. `None` for 0 / unknown and for moments Excel cannot show (before 1900-03-01, after 9999).
fn local_stamp(ms: i64) -> Option<Stamp> {
    if ms <= 0 {
        return None;
    }
    let dt = Local.timestamp_millis_opt(ms).single()?.naive_local();
    let serial = excel_serial(&dt)?;
    Some(Stamp { dt, serial })
}

/// Excel serial number (days since 1899-12-30, time of day as the fraction) of a calendar value.
fn excel_serial(dt: &NaiveDateTime) -> Option<f64> {
    let days = i64::from(dt.date().num_days_from_ce()) - EXCEL_EPOCH_FROM_CE;
    if !(EXCEL_MIN_SERIAL..=EXCEL_MAX_SERIAL).contains(&days) {
        return None;
    }
    let secs = f64::from(dt.time().num_seconds_from_midnight());
    Some(days as f64 + secs / 86_400.0)
}

/// "dd-mm-yyyy HH:MM:SS"
fn push_csv_date(out: &mut String, dt: &NaiveDateTime) {
    let d = dt.date();
    let t = dt.time();
    let _ = write!(
        out,
        "{:02}-{:02}-{:04} {:02}:{:02}:{:02}",
        d.day(),
        d.month(),
        d.year(),
        t.hour(),
        t.minute(),
        t.second()
    );
}

// ---------------------------------------------------------------------------------------------------------------
// Attributes, names, numbers
// ---------------------------------------------------------------------------------------------------------------

/// Windows FILE_ATTRIBUTE_* bits as short letters: R H S D A C E P(sparse) L(reparse) O(offline), then "cloud" when
/// the file is a cloud placeholder (RECALL_ON_DATA_ACCESS 0x400000 or RECALL_ON_OPEN 0x40000).
fn attr_string(attr: u32, out: &mut String) {
    const LETTERS: [(u32, char); 10] = [
        (0x1, 'R'),
        (0x2, 'H'),
        (0x4, 'S'),
        (0x10, 'D'),
        (0x20, 'A'),
        (0x800, 'C'),
        (0x4000, 'E'),
        (0x200, 'P'),
        (0x400, 'L'),
        (0x1000, 'O'),
    ];
    out.clear();
    for (bit, letter) in LETTERS {
        if attr & bit != 0 {
            out.push(letter);
        }
    }
    if attr & (0x40_0000 | 0x4_0000) != 0 {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str("cloud");
    }
}

/// "1,234,567"
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).rem_euclid(3) == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Drops control characters and everything Windows does not allow in a file name; `replacement` (or nothing when
/// `'\0'`) stands in for them.
fn sanitize_name(s: &str, replacement: char) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_control() || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            if replacement != '\0' {
                out.push(replacement);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// "C:" -> "C"; a mount path like "/mnt/d" -> "mntd"; never empty, never ends in a dot or space.
fn drive_file_part(drive: &str) -> String {
    let cleaned = sanitize_name(drive, '\0');
    let cleaned = cleaned.trim_matches(|c: char| c == ' ' || c == '.');
    if cleaned.is_empty() {
        "drive".to_string()
    } else {
        cleaned.to_string()
    }
}

fn role_text(r: DriveRole) -> &'static str {
    match r {
        DriveRole::System => "System",
        DriveRole::External => "External",
        DriveRole::GoogleDrive => "Google Drive",
        DriveRole::Other => "Other",
    }
}

fn media_text(m: Media) -> &'static str {
    match m {
        Media::Hdd => "HDD",
        Media::Ssd => "SSD",
        Media::Unknown => "Unknown",
    }
}

fn profile_text(p: ScanProfile) -> &'static str {
    match p {
        ScanProfile::Quick => "Quick",
        ScanProfile::Deep => "Deep",
    }
}

fn verdict_index(v: Verdict) -> usize {
    Verdict::ALL.iter().position(|x| *x == v).unwrap_or(0)
}

// ---------------------------------------------------------------------------------------------------------------
// Sheets that continue on "<name> 2", "<name> 3", ... when they run out of rows
// ---------------------------------------------------------------------------------------------------------------

fn new_sheet(wb: &mut Workbook, fm: &Fmts, name: &str, cols: &[Col]) -> Result<Worksheet> {
    let mut ws = wb.new_worksheet_with_constant_memory();
    ws.set_name(name)?;
    for (i, c) in cols.iter().enumerate() {
        let i = u16::try_from(i)?;
        ws.set_column_width(i, c.width)?;
        ws.write_string_with_format(0, i, c.name, &fm.header)?;
    }
    ws.set_freeze_panes(1, 0)?;
    Ok(ws)
}

fn finish_sheet(ws: &mut Worksheet, data_rows: u32, cols: &[Col]) -> Result<()> {
    let last_col = u16::try_from(cols.len().saturating_sub(1))?;
    ws.autofilter(0, 0, data_rows, last_col)?;
    Ok(())
}

struct SheetSet {
    base: &'static str,
    cols: &'static [Col],
    cur: Worksheet,
    done: Vec<Worksheet>,
    /// Data rows already used on `cur` (the header is row 0, so this is also the index of the last row written).
    data_rows: u32,
    sheets: u32,
}

impl SheetSet {
    fn new(wb: &mut Workbook, fm: &Fmts, base: &'static str, cols: &'static [Col]) -> Result<Self> {
        let cur = new_sheet(wb, fm, base, cols)?;
        Ok(SheetSet { base, cols, cur, done: Vec::new(), data_rows: 0, sheets: 1 })
    }

    /// The worksheet to write the next row to, and that row's index. Rows must be written in order, one per call.
    fn slot(
        &mut self,
        wb: &mut Workbook,
        fm: &Fmts,
        max_rows: u32,
        progress: &mut dyn FnMut(&str),
    ) -> Result<(&mut Worksheet, u32)> {
        if self.data_rows >= max_rows {
            self.roll(wb, fm, progress)?;
        }
        self.data_rows += 1;
        Ok((&mut self.cur, self.data_rows))
    }

    fn roll(&mut self, wb: &mut Workbook, fm: &Fmts, progress: &mut dyn FnMut(&str)) -> Result<()> {
        self.sheets += 1;
        let name = format!("{} {}", self.base, self.sheets);
        progress(&format!("Sheet: {name}"));
        let mut next = new_sheet(wb, fm, &name, self.cols)?;
        finish_sheet(&mut self.cur, self.data_rows, self.cols)?;
        std::mem::swap(&mut self.cur, &mut next);
        self.done.push(next);
        self.data_rows = 0;
        Ok(())
    }

    fn finish(mut self) -> Result<Vec<Worksheet>> {
        finish_sheet(&mut self.cur, self.data_rows, self.cols)?;
        self.done.push(self.cur);
        Ok(self.done)
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Summary
// ---------------------------------------------------------------------------------------------------------------

enum Fact {
    When(i64),
    Text(&'static str),
    Int(u64),
    Secs(u64),
    Gb(u64),
}

fn write_summary(ws: &mut Worksheet, fm: &Fmts, s: &Summary) -> Result<()> {
    let mut r: u32 = 0;
    put_str_fmt(ws, r, 0, "AVNI Drive Scanner — evaluation", &fm.title)?;
    r += 2;

    let facts: [(&str, Fact); 13] = [
        ("Scanned at", Fact::When(s.scanned_at_ms)),
        ("Profile", Fact::Text(profile_text(s.profile))),
        ("Total time s", Fact::Secs(s.total_ms)),
        ("Metadata time s", Fact::Secs(s.metadata_ms)),
        ("Hash time s", Fact::Secs(s.hash_ms)),
        ("Files", Fact::Int(s.files)),
        ("Folders", Fact::Int(s.dirs)),
        ("Total size GB", Fact::Gb(s.bytes)),
        ("Hashed files", Fact::Int(s.hashed_files)),
        ("Hashed GB", Fact::Gb(s.hashed_bytes)),
        ("Duplicate groups", Fact::Int(s.dup_groups)),
        ("Confirmed groups", Fact::Int(s.dup_groups_confirmed)),
        ("Space wasted by duplicates GB", Fact::Gb(s.dup_wasted_bytes)),
    ];
    for (label, fact) in &facts {
        put_str_fmt(ws, r, 0, label, &fm.bold)?;
        match fact {
            Fact::When(ms) => put_stamp(ws, r, 1, local_stamp(*ms), &fm.fact_date)?,
            Fact::Text(t) => put_str_fmt(ws, r, 1, t, &fm.fact_text)?,
            Fact::Int(n) => put_num(ws, r, 1, bytes_f(*n), &fm.fact_int)?,
            Fact::Secs(ms) => put_num(ws, r, 1, *ms as f64 / 1000.0, &fm.fact_secs)?,
            Fact::Gb(b) => put_num(ws, r, 1, gb_f(*b), &fm.fact_gb)?,
        }
        r += 1;
    }
    r += 1;

    // ---- per-drive table ----
    let mut heads: Vec<String> = [
        "Drive",
        "Label",
        "Role",
        "File system",
        "Media",
        "Method",
        "Size GB",
        "Used GB",
        "Free GB",
        "Files",
        "Folders",
        "Scanned GB",
        "Scan time s",
    ]
    .iter()
    .map(|h| (*h).to_string())
    .collect();
    let first_verdict_col = heads.len();
    for v in Verdict::ALL {
        heads.push(format!("{} GB", v.label()));
    }
    heads.push("Error".to_string());
    let error_col = heads.len() - 1;

    // Column widths. Column A also carries the key-fact labels, column B their values.
    for (i, h) in heads.iter().enumerate() {
        let width = match i {
            0 => 32.0,
            1 => 24.0,
            2 => 13.0,
            3 => 12.0,
            4 => 9.0,
            5 => 30.0,
            _ if i == error_col => 60.0,
            _ => (h.chars().count() as f64 + 3.0).max(12.0),
        };
        ws.set_column_width(u16::try_from(i)?, width)?;
    }
    for (i, h) in heads.iter().enumerate() {
        put_str_fmt(ws, r, u16::try_from(i)?, h, &fm.header)?;
    }
    r += 1;

    let mut tot_size = 0u64;
    let mut tot_used = 0u64;
    let mut tot_free = 0u64;
    let mut tot_files = 0u64;
    let mut tot_dirs = 0u64;
    let mut tot_scanned = 0u64;
    let mut tot_ms = 0u64;
    let mut tot_verdict = [0u64; Verdict::ALL.len()];

    for ds in &s.drives {
        let d = &ds.drive;
        let used = d.total_bytes.saturating_sub(d.free_bytes);
        let vb = verdict_bytes(ds);
        put_str_fmt(ws, r, 0, &d.id, &fm.bold)?;
        put_str(ws, r, 1, &d.label)?;
        put_str(ws, r, 2, role_text(d.role))?;
        put_str(ws, r, 3, &d.fs)?;
        put_str(ws, r, 4, media_text(d.media))?;
        put_str(ws, r, 5, &ds.method)?;
        put_num(ws, r, 6, gb_f(d.total_bytes), &fm.gb)?;
        put_num(ws, r, 7, gb_f(used), &fm.gb)?;
        put_num(ws, r, 8, gb_f(d.free_bytes), &fm.gb)?;
        put_num(ws, r, 9, bytes_f(ds.files), &fm.int)?;
        put_num(ws, r, 10, bytes_f(ds.dirs), &fm.int)?;
        put_num(ws, r, 11, gb_f(ds.bytes), &fm.gb)?;
        put_num(ws, r, 12, ds.scan_ms as f64 / 1000.0, &fm.secs)?;
        for (i, b) in vb.iter().enumerate() {
            put_num(ws, r, u16::try_from(first_verdict_col + i)?, gb_f(*b), &fm.gb)?;
            tot_verdict[i] += *b;
        }
        if let Some(e) = &ds.error {
            put_str(ws, r, u16::try_from(error_col)?, e)?;
        }
        tot_size += d.total_bytes;
        tot_used += used;
        tot_free += d.free_bytes;
        tot_files += ds.files;
        tot_dirs += ds.dirs;
        tot_scanned += ds.bytes;
        tot_ms += ds.scan_ms;
        r += 1;
    }

    // TOTAL row (computed numbers).
    put_str_fmt(ws, r, 0, "TOTAL", &fm.total_text)?;
    for c in 1..=5u16 {
        ws.write_blank(r, c, &fm.total_text)?;
    }
    put_num(ws, r, 6, gb_f(tot_size), &fm.total_gb)?;
    put_num(ws, r, 7, gb_f(tot_used), &fm.total_gb)?;
    put_num(ws, r, 8, gb_f(tot_free), &fm.total_gb)?;
    put_num(ws, r, 9, bytes_f(tot_files), &fm.total_int)?;
    put_num(ws, r, 10, bytes_f(tot_dirs), &fm.total_int)?;
    put_num(ws, r, 11, gb_f(tot_scanned), &fm.total_gb)?;
    put_num(ws, r, 12, tot_ms as f64 / 1000.0, &fm.total_secs)?;
    for (i, b) in tot_verdict.iter().enumerate() {
        put_num(ws, r, u16::try_from(first_verdict_col + i)?, gb_f(*b), &fm.total_gb)?;
    }
    ws.write_blank(r, u16::try_from(error_col)?, &fm.total_text)?;
    r += 2;

    // ---- notes ----
    if !s.notes.is_empty() {
        put_str_fmt(ws, r, 0, "Notes", &fm.bold)?;
        r += 1;
        for n in &s.notes {
            put_str(ws, r, 0, n)?;
            r += 1;
        }
        r += 1;
    }

    // ---- legend ----
    put_str_fmt(ws, r, 0, "What the verdicts mean", &fm.bold)?;
    r += 1;
    for v in Verdict::ALL {
        put_str_fmt(ws, r, 0, v.label(), &fm.bold)?;
        put_str(ws, r, 1, verdict_legend(v))?;
        r += 1;
    }
    r += 1;
    put_str(ws, r, 0, "GB = 1024 x 1024 x 1024 bytes, the same unit Windows shows.")?;
    Ok(())
}

fn verdict_legend(v: Verdict) -> &'static str {
    match v {
        Verdict::Keep => "Needed by Windows or installed apps, or already in Google Drive.",
        Verdict::Protected => "On the protected list. Never touched.",
        Verdict::Transfer => "User data that belongs on Google Drive.",
        Verdict::InDrive => "Same name and size found on Google Drive.",
        Verdict::Duplicate => "Extra copy. One copy is kept.",
        Verdict::Junk => "Temp files, cache and crash dumps.",
        Verdict::Extract => "Archive to unzip and dedup.",
        Verdict::Supabase => "Processed output.",
        Verdict::Review => "No rule matched. A person decides.",
    }
}

/// Bytes per verdict (in `Verdict::ALL` order) for one drive; totals without a verdict are ignored.
fn verdict_bytes(ds: &DriveSummary) -> [u64; Verdict::ALL.len()] {
    let mut out = [0u64; Verdict::ALL.len()];
    for t in &ds.totals {
        if let Some(v) = t.verdict {
            out[verdict_index(v)] += t.bytes;
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------------------------
// Duplicates, Largest, Extensions, Folders
// ---------------------------------------------------------------------------------------------------------------

fn write_duplicates(
    wb: &mut Workbook,
    fm: &Fmts,
    max_rows: u32,
    progress: &mut dyn FnMut(&str),
    groups: &[DupGroup],
) -> Result<Vec<Worksheet>> {
    let mut set = SheetSet::new(wb, fm, "Duplicates", DUP_COLS)?;
    for g in groups {
        for (i, m) in g.members.iter().enumerate() {
            let (ws, r) = set.slot(wb, fm, max_rows, progress)?;
            put_plain_num(ws, r, 0, f64::from(g.id))?;
            put_str(ws, r, 1, if g.confirmed { "Yes" } else { "No" })?;
            // THE TREE LAW: members[0] is kept; only copies it marked loose carry the Duplicate verdict.
            put_str(ws, r, 2, dup_role_text(i, m.verdict))?;
            put_num(ws, r, 3, bytes_f(m.size), &fm.int)?;
            if i == 0 {
                put_num(ws, r, 4, bytes_f(g.wasted), &fm.int)?;
            }
            put_str(ws, r, 5, &g.hash)?;
            put_str(ws, r, 6, &m.drive)?;
            put_str(ws, r, 7, &m.path)?;
            put_stamp(ws, r, 8, local_stamp(m.modified_ms), &fm.date)?;
            put_str(ws, r, 9, &m.reason)?;
        }
    }
    set.finish()
}

fn dup_role_text(position: usize, verdict: Verdict) -> &'static str {
    if position == 0 {
        "Keep"
    } else if verdict == Verdict::Duplicate {
        "Remove"
    } else {
        "Kept (tree law)"
    }
}

fn write_largest(
    wb: &mut Workbook,
    fm: &Fmts,
    max_rows: u32,
    progress: &mut dyn FnMut(&str),
    rows: &[FileRow],
) -> Result<Vec<Worksheet>> {
    let mut set = SheetSet::new(wb, fm, "Largest", LARGEST_COLS)?;
    for m in rows {
        let (ws, r) = set.slot(wb, fm, max_rows, progress)?;
        put_str(ws, r, 0, &m.path)?;
        put_str(ws, r, 1, &m.drive)?;
        put_num(ws, r, 2, bytes_f(m.size), &fm.int)?;
        put_num(ws, r, 3, mb_f(m.size), &fm.mb)?;
        put_stamp(ws, r, 4, local_stamp(m.modified_ms), &fm.date)?;
        put_str(ws, r, 5, m.verdict.label())?;
        put_str(ws, r, 6, &m.reason)?;
    }
    set.finish()
}

fn write_extensions(
    wb: &mut Workbook,
    fm: &Fmts,
    max_rows: u32,
    progress: &mut dyn FnMut(&str),
    rows: &[ExtTotal],
) -> Result<Vec<Worksheet>> {
    let mut set = SheetSet::new(wb, fm, "Extensions", EXT_COLS)?;
    for e in rows {
        let (ws, r) = set.slot(wb, fm, max_rows, progress)?;
        put_str(ws, r, 0, if e.ext.is_empty() { "(none)" } else { &e.ext })?;
        put_num(ws, r, 1, bytes_f(e.files), &fm.int)?;
        put_num(ws, r, 2, bytes_f(e.bytes), &fm.int)?;
        put_num(ws, r, 3, gb_f(e.bytes), &fm.gb)?;
    }
    set.finish()
}

fn write_folders(
    wb: &mut Workbook,
    fm: &Fmts,
    max_rows: u32,
    progress: &mut dyn FnMut(&str),
    rows: &[FolderTotal],
) -> Result<Vec<Worksheet>> {
    let mut set = SheetSet::new(wb, fm, "Folders", FOLDER_COLS)?;
    for f in rows {
        let (ws, r) = set.slot(wb, fm, max_rows, progress)?;
        put_str(ws, r, 0, &f.drive)?;
        put_str(ws, r, 1, &f.path)?;
        put_num(ws, r, 2, bytes_f(f.files), &fm.int)?;
        put_num(ws, r, 3, bytes_f(f.bytes), &fm.int)?;
        put_num(ws, r, 4, gb_f(f.bytes), &fm.gb)?;
        put_str(ws, r, 5, f.verdict.label())?;
    }
    set.finish()
}

// ---------------------------------------------------------------------------------------------------------------
// The streaming pass: Plan + Files + CSV
// ---------------------------------------------------------------------------------------------------------------

struct Sink<'a> {
    wb: Workbook,
    fm: &'a Fmts,
    plan: SheetSet,
    files: Option<SheetSet>,
    csv: CsvOut,
    rows: u64,
    /// Row count of the last "Files: N rows" progress line.
    reported: u64,
    max_rows: u32,
    progress: &'a mut dyn FnMut(&str),
    attr: String,
}

impl Sink<'_> {
    fn on_row(&mut self, r: &FileRow) -> Result<()> {
        self.rows += 1;
        let created = local_stamp(r.created_ms);
        let modified = local_stamp(r.modified_ms);
        let accessed = local_stamp(r.accessed_ms);
        let fm = self.fm;

        if !r.is_dir && !matches!(r.verdict, Verdict::Keep | Verdict::Protected) {
            let (ws, row) = self.plan.slot(&mut self.wb, fm, self.max_rows, &mut *self.progress)?;
            write_plan_row(ws, row, r, modified, fm)?;
        }

        attr_string(r.attributes, &mut self.attr);
        if let Some(files) = self.files.as_mut() {
            let (ws, row) = files.slot(&mut self.wb, fm, self.max_rows, &mut *self.progress)?;
            write_files_row(ws, row, r, [created, modified, accessed], &self.attr, fm)?;
        }
        self.csv.write_row(r, [created, modified, accessed], &self.attr)?;

        if self.rows - self.reported >= PROGRESS_EVERY {
            self.reported = self.rows;
            (self.progress)(&format!("Files: {} rows", thousands(self.rows)));
        }
        Ok(())
    }
}

fn write_plan_row(ws: &mut Worksheet, row: u32, r: &FileRow, modified: Option<Stamp>, fm: &Fmts) -> Result<()> {
    put_str(ws, row, 0, r.verdict.label())?;
    put_str(ws, row, 1, &r.reason)?;
    put_str(ws, row, 2, &r.drive)?;
    put_str(ws, row, 3, &r.path)?;
    put_num(ws, row, 4, bytes_f(r.size), &fm.int)?;
    put_num(ws, row, 5, mb_f(r.size), &fm.mb)?;
    put_stamp(ws, row, 6, modified, &fm.date)?;
    put_group(ws, row, 7, r.dup_group)?;
    put_opt_str(ws, row, 8, r.blake3.as_deref())?;
    put_opt_str(ws, row, 9, r.sample.as_deref())?;
    Ok(())
}

fn write_files_row(
    ws: &mut Worksheet,
    row: u32,
    r: &FileRow,
    dates: [Option<Stamp>; 3],
    attr: &str,
    fm: &Fmts,
) -> Result<()> {
    put_str(ws, row, 0, &r.drive)?;
    put_str(ws, row, 1, &r.path)?;
    put_str(ws, row, 2, &r.name)?;
    put_str(ws, row, 3, &r.ext)?;
    put_str(ws, row, 4, if r.is_dir { "Folder" } else { "File" })?;
    put_num(ws, row, 5, bytes_f(r.size), &fm.int)?;
    put_stamp(ws, row, 6, dates[0], &fm.date)?;
    put_stamp(ws, row, 7, dates[1], &fm.date)?;
    put_stamp(ws, row, 8, dates[2], &fm.date)?;
    put_str(ws, row, 9, attr)?;
    put_plain_num(ws, row, 10, f64::from(r.links))?;
    put_opt_str(ws, row, 11, r.blake3.as_deref())?;
    put_opt_str(ws, row, 12, r.sha256.as_deref())?;
    put_opt_str(ws, row, 13, r.sample.as_deref())?;
    put_group(ws, row, 14, r.dup_group)?;
    put_str(ws, row, 15, r.verdict.label())?;
    put_str(ws, row, 16, &r.reason)?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------
// CSV: one file per drive, UTF-8 with BOM so Excel reads Hindi / Marathi names correctly
// ---------------------------------------------------------------------------------------------------------------

struct CsvOut {
    dir: PathBuf,
    stamp: String,
    cur_drive: Option<String>,
    wtr: Option<csv::Writer<BufWriter<File>>>,
    paths: Vec<String>,
    used_names: HashSet<String>,
    num: String,
    date: String,
    safe: String,
}

/// Starts a text field with a quote when Excel would otherwise read it as a formula ("=...", "+...", "-...", "@...").
fn csv_text<W: std::io::Write>(w: &mut csv::Writer<W>, s: &str, safe: &mut String) -> csv::Result<()> {
    if matches!(s.as_bytes().first(), Some(b'=' | b'+' | b'-' | b'@' | b'\t' | b'\r')) {
        safe.clear();
        safe.push('\'');
        safe.push_str(s);
        w.write_field(safe.as_bytes())
    } else {
        w.write_field(s.as_bytes())
    }
}

fn csv_u64<W: std::io::Write>(w: &mut csv::Writer<W>, n: u64, buf: &mut String) -> csv::Result<()> {
    buf.clear();
    let _ = write!(buf, "{n}");
    w.write_field(buf.as_bytes())
}

fn csv_date<W: std::io::Write>(w: &mut csv::Writer<W>, st: Option<Stamp>, buf: &mut String) -> csv::Result<()> {
    buf.clear();
    if let Some(st) = st {
        push_csv_date(buf, &st.dt);
    }
    w.write_field(buf.as_bytes())
}

impl CsvOut {
    fn new(dir: &Path, stamp: &str) -> Self {
        CsvOut {
            dir: dir.to_path_buf(),
            stamp: stamp.to_string(),
            cur_drive: None,
            wtr: None,
            paths: Vec::new(),
            used_names: HashSet::new(),
            num: String::new(),
            date: String::new(),
            safe: String::new(),
        }
    }

    /// Closes the current file and starts the CSV of `drive`.
    fn open(&mut self, drive: &str) -> Result<()> {
        self.close()?;
        let part = drive_file_part(drive);
        let mut name = format!("Avni Drive Scan {} - {}.csv", self.stamp, part);
        let mut n = 2;
        while !self.used_names.insert(name.to_lowercase()) {
            name = format!("Avni Drive Scan {} - {} ({}).csv", self.stamp, part, n);
            n += 1;
        }
        let path = self.dir.join(&name);
        let file = File::create(&path).with_context(|| format!("cannot create {}", path.display()))?;
        let mut bw = BufWriter::with_capacity(1 << 20, file);
        bw.write_all(&[0xEF, 0xBB, 0xBF])?;
        let mut w = csv::WriterBuilder::new().has_headers(false).terminator(csv::Terminator::CRLF).from_writer(bw);
        w.write_record(FILES_COLS.iter().map(|c| c.name))?;
        self.wtr = Some(w);
        self.cur_drive = Some(drive.to_string());
        self.paths.push(path.to_string_lossy().into_owned());
        Ok(())
    }

    fn close(&mut self) -> Result<()> {
        if let Some(mut w) = self.wtr.take() {
            w.flush()?;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        self.close()
    }

    /// The report failed: close and delete the CSV files of this run, so no half-written file looks complete.
    fn discard(&mut self) {
        let _ = self.close();
        for p in self.paths.drain(..) {
            let _ = std::fs::remove_file(p);
        }
    }

    fn write_row(&mut self, r: &FileRow, dates: [Option<Stamp>; 3], attr: &str) -> Result<()> {
        if self.cur_drive.as_deref() != Some(r.drive.as_str()) {
            self.open(&r.drive)?;
        }
        let Self { wtr, num, date, safe, .. } = self;
        let Some(w) = wtr.as_mut() else {
            anyhow::bail!("CSV writer is not open");
        };
        csv_text(w, &r.drive, safe)?;
        csv_text(w, &r.path, safe)?;
        csv_text(w, &r.name, safe)?;
        csv_text(w, &r.ext, safe)?;
        w.write_field(if r.is_dir { "Folder" } else { "File" })?;
        csv_u64(w, r.size, num)?;
        csv_date(w, dates[0], date)?;
        csv_date(w, dates[1], date)?;
        csv_date(w, dates[2], date)?;
        w.write_field(attr)?;
        csv_u64(w, u64::from(r.links), num)?;
        w.write_field(r.blake3.as_deref().unwrap_or(""))?;
        w.write_field(r.sha256.as_deref().unwrap_or(""))?;
        w.write_field(r.sample.as_deref().unwrap_or(""))?;
        match r.dup_group {
            Some(g) => csv_u64(w, u64::from(g), num)?,
            None => w.write_field("")?,
        }
        w.write_field(r.verdict.label())?;
        csv_text(w, &r.reason, safe)?;
        w.write_record(None::<&[u8]>)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DriveInfo, DriveKind, VerdictTotal};
    use chrono::NaiveDate;
    use std::io::Read;

    fn drive_info(id: &str, role: DriveRole, total_gb: u64, free_gb: u64) -> DriveInfo {
        DriveInfo {
            id: id.to_string(),
            root: format!("{id}\\"),
            label: format!("Disk {id}"),
            fs: "NTFS".to_string(),
            kind: DriveKind::Fixed,
            media: Media::Ssd,
            total_bytes: total_gb << 30,
            free_bytes: free_gb << 30,
            role,
            mft_capable: true,
            disk: Some(0),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn row(id: u32, drv: &str, path: &str, name: &str, is_dir: bool, size: u64, verdict: Verdict) -> FileRow {
        FileRow {
            id,
            drive: drv.to_string(),
            path: path.to_string(),
            name: name.to_string(),
            ext: name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default(),
            is_dir,
            size,
            created_ms: 1_700_000_000_000 + i64::from(id) * 1000,
            modified_ms: 1_760_000_000_000 + i64::from(id) * 60_000,
            accessed_ms: if id & 1 == 0 { 0 } else { 1_761_000_000_000 },
            attributes: if is_dir { 0x10 } else { 0x20 },
            links: 1,
            blake3: if is_dir { None } else { Some(format!("{:064x}", u64::from(id) * 7919)) },
            sha256: None,
            sample: if is_dir { None } else { Some(format!("{:064x}", u64::from(id) * 104_729)) },
            dup_group: None,
            verdict,
            reason: format!("rule for {name}"),
        }
    }

    struct Fixture {
        rows: Vec<FileRow>,
        summary: Summary,
        dups: Vec<DupGroup>,
        largest: Vec<FileRow>,
        exts: Vec<ExtTotal>,
        folders: Vec<FolderTotal>,
    }

    /// Two drives, folders and files, every verdict, two duplicate groups, awkward names.
    fn fixture() -> Fixture {
        let mut rows = Vec::new();
        let mut id = 0u32;
        let mut add = |rows: &mut Vec<FileRow>, drv: &str, path: &str, name: &str, dir: bool, size: u64, v: Verdict| {
            rows.push(row(id, drv, path, name, dir, size, v));
            id += 1;
        };
        add(&mut rows, "C:", "C:\\Users", "Users", true, 0, Verdict::Keep);
        add(&mut rows, "C:", "C:\\Users\\ravi", "ravi", true, 0, Verdict::Review);
        add(&mut rows, "C:", "C:\\Windows\\notepad.exe", "notepad.exe", false, 200_000, Verdict::Keep);
        add(&mut rows, "C:", "C:\\Users\\ravi\\keys.kdbx", "keys.kdbx", false, 5_000, Verdict::Protected);
        add(&mut rows, "C:", "C:\\Users\\ravi\\रवि.txt", "रवि.txt", false, 1_234, Verdict::Transfer);
        add(&mut rows, "C:", "C:\\Users\\ravi\\photo.jpg", "photo.jpg", false, 3_000_000, Verdict::InDrive);
        add(&mut rows, "C:", "C:\\Users\\ravi\\a.pdf", "a.pdf", false, 700_000, Verdict::Transfer);
        add(&mut rows, "C:", "C:\\Users\\ravi\\copy of a.pdf", "copy of a.pdf", false, 700_000, Verdict::Duplicate);
        add(&mut rows, "C:", "C:\\Temp\\x.tmp", "x.tmp", false, 42, Verdict::Junk);
        add(&mut rows, "C:", "C:\\Users\\ravi\\old.zip", "old.zip", false, 9_000_000, Verdict::Extract);
        add(&mut rows, "C:", "C:\\Users\\ravi\\processed\\out.csv", "out.csv", false, 77, Verdict::Supabase);
        add(&mut rows, "C:", "C:\\Users\\ravi\\notes.docx", "notes.docx", false, 18_000, Verdict::Review);
        add(
            &mut rows,
            "C:",
            "C:\\Users\\ravi\\report, \"final\".docx",
            "report, \"final\".docx",
            false,
            20_000,
            Verdict::Transfer,
        );
        add(&mut rows, "C:", "C:\\Users\\ravi\\=cmd.txt", "=cmd.txt", false, 9, Verdict::Review);
        add(&mut rows, "D:", "D:\\Backup", "Backup", true, 0, Verdict::Review);
        add(&mut rows, "D:", "D:\\Backup\\a.pdf", "a.pdf", false, 700_000, Verdict::Duplicate);
        add(&mut rows, "D:", "D:\\Backup\\b.mp4", "b.mp4", false, 50_000_000, Verdict::Transfer);
        add(&mut rows, "D:", "D:\\Backup\\b (1).mp4", "b (1).mp4", false, 50_000_000, Verdict::Duplicate);
        add(&mut rows, "D:", "D:\\Backup\\नोंद.docx", "नोंद.docx", false, 15_000, Verdict::Review);
        add(&mut rows, "D:", "D:\\Backup\\readme", "readme", false, 12, Verdict::Review);
        add(&mut rows, "D:", "D:\\Backup\\gone.dll", "gone.dll", false, 800, Verdict::Keep);
        add(&mut rows, "D:", "D:\\Backup\\z.7z", "z.7z", false, 2_000, Verdict::Extract);
        add(&mut rows, "D:", "D:\\Backup\\p.json", "p.json", false, 99, Verdict::Supabase);
        add(&mut rows, "D:", "D:\\Backup\\t.tmp", "t.tmp", false, 5, Verdict::Junk);
        add(&mut rows, "D:", "D:\\Backup\\g.png", "g.png", false, 66_000, Verdict::InDrive);
        add(&mut rows, "D:", "D:\\Backup\\q.png", "q.png", false, 1_000, Verdict::Transfer);
        add(&mut rows, "D:", "D:\\Backup\\sub", "sub", true, 0, Verdict::Review);
        add(&mut rows, "D:", "D:\\Backup\\sub\\deep.txt", "deep.txt", false, 3, Verdict::Review);
        add(&mut rows, "D:", "D:\\Backup\\sub\\deeper.txt", "deeper.txt", false, 4, Verdict::Protected);

        for i in [6, 7, 15] {
            rows[i].dup_group = Some(1);
        }
        for i in [16, 17] {
            rows[i].dup_group = Some(2);
        }
        let g1 = vec![rows[6].clone(), rows[7].clone(), rows[15].clone()];
        let g2 = vec![rows[16].clone(), rows[17].clone()];
        let dups = vec![
            DupGroup { id: 1, size: 700_000, confirmed: true, hash: "ab".repeat(32), members: g1, wasted: 1_400_000 },
            DupGroup {
                id: 2,
                size: 50_000_000,
                confirmed: false,
                hash: "cd".repeat(32),
                members: g2,
                wasted: 50_000_000,
            },
        ];

        let mut largest: Vec<FileRow> = rows.iter().filter(|r| !r.is_dir).cloned().collect();
        largest.sort_by_key(|r| std::cmp::Reverse(r.size));
        largest.truncate(5);

        let totals_c = vec![
            VerdictTotal { verdict: Some(Verdict::Keep), files: 1, bytes: 200_000 },
            VerdictTotal { verdict: Some(Verdict::Transfer), files: 3, bytes: 721_234 },
            VerdictTotal { verdict: Some(Verdict::Extract), files: 1, bytes: 9_000_000 },
            VerdictTotal { verdict: None, files: 99, bytes: 999_999_999 },
        ];
        let totals_d = vec![
            VerdictTotal { verdict: Some(Verdict::Transfer), files: 2, bytes: 50_001_000 },
            VerdictTotal { verdict: Some(Verdict::Duplicate), files: 2, bytes: 50_700_000 },
        ];
        let nc = rows.iter().filter(|r| r.drive == "C:" && !r.is_dir).count() as u64;
        let nd = rows.iter().filter(|r| r.drive == "D:" && !r.is_dir).count() as u64;
        let summary = Summary {
            scanned_at_ms: 1_760_000_000_000,
            profile: ScanProfile::Deep,
            total_ms: 125_500,
            metadata_ms: 25_500,
            hash_ms: 100_000,
            drives: vec![
                DriveSummary {
                    drive: drive_info("C:", DriveRole::System, 476, 120),
                    method: "MFT".to_string(),
                    files: nc,
                    dirs: 2,
                    bytes: 13_000_000,
                    scan_ms: 12_000,
                    totals: totals_c,
                    error: None,
                },
                DriveSummary {
                    drive: drive_info("D:", DriveRole::External, 931, 900),
                    method: "Folder walk".to_string(),
                    files: nd,
                    dirs: 2,
                    bytes: 101_000_000,
                    scan_ms: 9_000,
                    totals: totals_d,
                    error: Some("access denied in D:\\System Volume Information".to_string()),
                },
            ],
            totals: vec![],
            files: nc + nd,
            dirs: 4,
            bytes: 114_000_000,
            hashed_files: 20,
            hashed_bytes: 100_000_000,
            dup_groups: 2,
            dup_groups_confirmed: 1,
            dup_wasted_bytes: 51_400_000,
            notes: vec!["Hashes were sampled for 1 group.".to_string(), "Second note, with a comma.".to_string()],
        };
        let exts = vec![
            ExtTotal { ext: "mp4".to_string(), files: 2, bytes: 100_000_000 },
            ExtTotal { ext: String::new(), files: 1, bytes: 12 },
        ];
        let folders = vec![FolderTotal {
            drive: "D:".to_string(),
            path: "D:\\Backup".to_string(),
            files: 12,
            bytes: 101_000_000,
            verdict: Verdict::Transfer,
        }];
        Fixture { rows, summary, dups, largest, exts, folders }
    }

    fn run(fx: &Fixture, dir: &Path, include_all: bool, max_rows: u32, msgs: &mut Vec<String>) -> Result<ReportFiles> {
        let rows = &fx.rows;
        let visit = |cb: &mut dyn FnMut(&FileRow) -> Result<()>| -> Result<()> {
            for r in rows {
                cb(r)?;
            }
            Ok(())
        };
        let input = ReportInput {
            summary: &fx.summary,
            dup_groups: &fx.dups,
            largest: &fx.largest,
            extensions: &fx.exts,
            folders: &fx.folders,
            for_each_row: &visit,
        };
        let opts = ReportOptions { include_all_files: include_all, stamp: "15-10-2026 1042".to_string() };
        let mut progress = |m: &str| msgs.push(m.to_string());
        write_reports_with_limit(&input, dir, &opts, &mut progress, max_rows)
    }

    fn zip_text(path: &str, name: &str) -> String {
        let f = File::open(path).unwrap();
        let mut z = zip::ZipArchive::new(f).unwrap();
        let mut s = String::new();
        z.by_name(name).unwrap().read_to_string(&mut s).unwrap();
        s
    }

    fn sheet_names(xlsx: &str) -> Vec<String> {
        let xml = zip_text(xlsx, "xl/workbook.xml");
        let mut out = Vec::new();
        let mut rest = xml.as_str();
        while let Some(i) = rest.find("<sheet name=\"") {
            rest = &rest[i + "<sheet name=\"".len()..];
            let j = rest.find('"').unwrap();
            out.push(rest[..j].to_string());
            rest = &rest[j..];
        }
        out
    }

    /// Number of `<row ` elements on sheet `n` (1-based, in workbook order), header included.
    fn sheet_rows(xlsx: &str, n: usize) -> usize {
        zip_text(xlsx, &format!("xl/worksheets/sheet{n}.xml")).matches("<row ").count()
    }

    fn read_csv(path: &str) -> (Vec<u8>, Vec<Vec<String>>) {
        let bytes = std::fs::read(path).unwrap();
        let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(&bytes);
        let mut rdr = csv::ReaderBuilder::new().has_headers(false).from_reader(body);
        let recs = rdr.records().map(|r| r.unwrap().iter().map(str::to_string).collect::<Vec<_>>()).collect();
        (bytes, recs)
    }

    #[test]
    fn full_report_sheets_csv_and_unicode() {
        let fx = fixture();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("nested").join("reports");
        let mut msgs = Vec::new();
        let out = run(&fx, &dir, true, MAX_DATA_ROWS, &mut msgs).unwrap();

        assert_eq!(out.rows, fx.rows.len() as u64);
        assert_eq!(out.folder, dir.to_string_lossy());
        assert!(out.xlsx.ends_with("Avni Drive Scan 15-10-2026 1042.xlsx"), "{}", out.xlsx);
        assert!(Path::new(&out.xlsx).exists());
        assert!(!dir.join("Avni Drive Scan 15-10-2026 1042.xlsx.part").exists());

        // Sheets, exactly and in order.
        assert_eq!(
            sheet_names(&out.xlsx),
            ["Summary", "Plan", "Duplicates", "Largest", "Extensions", "Folders", "Files"]
        );

        // Row counts per sheet (header included).
        let plan_rows =
            fx.rows.iter().filter(|r| !r.is_dir && !matches!(r.verdict, Verdict::Keep | Verdict::Protected)).count();
        assert_eq!(sheet_rows(&out.xlsx, 2), plan_rows + 1);
        assert_eq!(sheet_rows(&out.xlsx, 3), 5 + 1);
        assert_eq!(sheet_rows(&out.xlsx, 4), fx.largest.len() + 1);
        assert_eq!(sheet_rows(&out.xlsx, 5), 2 + 1);
        assert_eq!(sheet_rows(&out.xlsx, 6), 1 + 1);
        assert_eq!(sheet_rows(&out.xlsx, 7), fx.rows.len() + 1);

        // Plan: filtered, with autofilter and a frozen header; Keep / Protected / folders are not in it.
        let plan_xml = zip_text(&out.xlsx, "xl/worksheets/sheet2.xml");
        assert!(plan_xml.contains("<autoFilter ref=\"A1:J"), "autofilter missing");
        assert!(plan_xml.contains("state=\"frozen\""), "freeze panes missing");
        assert!(plan_xml.contains("रवि.txt") || plan_xml.contains("C:\\Users\\ravi\\रवि.txt"));
        assert!(!plan_xml.contains("notepad.exe"));
        assert!(!plan_xml.contains("keys.kdbx"));
        assert!(plan_xml.contains("Transfer to Google Drive"));

        // Summary sheet content.
        let sum_xml = zip_text(&out.xlsx, "xl/worksheets/sheet1.xml");
        for needle in [
            "AVNI Drive Scanner — evaluation",
            "Scanned at",
            "Space wasted by duplicates GB",
            "Transfer to Google Drive GB",
            "Processed -&gt; Supabase GB",
            "TOTAL",
            "Hashes were sampled for 1 group.",
            "access denied",
            "Needed by Windows or installed apps",
        ] {
            assert!(sum_xml.contains(needle), "Summary lacks {needle:?}");
        }

        // Duplicates: Keep / Remove, Wasted on the first row of each group only.
        let dup_xml = zip_text(&out.xlsx, "xl/worksheets/sheet3.xml");
        assert_eq!(dup_xml.matches(">Keep<").count(), 2);
        assert_eq!(dup_xml.matches(">Remove<").count(), 3);
        assert_eq!(dup_xml.matches(">Yes<").count(), 3);
        assert_eq!(dup_xml.matches(">No<").count(), 2);
        // Extensions: empty extension shown as (none).
        assert!(zip_text(&out.xlsx, "xl/worksheets/sheet5.xml").contains("(none)"));
        // Files: unicode names and decoded attributes.
        let files_xml = zip_text(&out.xlsx, "xl/worksheets/sheet7.xml");
        assert!(files_xml.contains("नोंद.docx"));
        assert!(files_xml.contains(">Folder<"));

        // CSV: one per drive, BOM, header + rows.
        assert_eq!(out.csv.len(), 2);
        assert!(out.csv[0].ends_with("Avni Drive Scan 15-10-2026 1042 - C.csv"), "{}", out.csv[0]);
        assert!(out.csv[1].ends_with("Avni Drive Scan 15-10-2026 1042 - D.csv"), "{}", out.csv[1]);
        let header: Vec<&str> = FILES_COLS.iter().map(|c| c.name).collect();
        assert_eq!(header.len(), 17);
        let n_c = fx.rows.iter().filter(|r| r.drive == "C:").count();
        let n_d = fx.rows.iter().filter(|r| r.drive == "D:").count();
        let (bytes_c, recs_c) = read_csv(&out.csv[0]);
        let (bytes_d, recs_d) = read_csv(&out.csv[1]);
        assert_eq!(&bytes_c[..3], &[0xEF, 0xBB, 0xBF]);
        assert_eq!(&bytes_d[..3], &[0xEF, 0xBB, 0xBF]);
        assert_eq!(recs_c.len(), n_c + 1);
        assert_eq!(recs_d.len(), n_d + 1);
        assert_eq!(recs_c[0], header);
        assert!(recs_c.iter().all(|r| r.len() == 17));
        assert!(recs_c[1..].iter().all(|r| r[0] == "C:"));
        assert!(recs_d[1..].iter().all(|r| r[0] == "D:"));

        // Unicode name round trip, quoting, formula guard.
        let ravi = recs_c.iter().find(|r| r[2] == "रवि.txt").expect("Hindi name in CSV");
        assert_eq!(ravi[1], "C:\\Users\\ravi\\रवि.txt");
        assert_eq!(ravi[3], "txt");
        assert_eq!(ravi[4], "File");
        assert_eq!(ravi[5], "1234");
        assert_eq!(ravi[15], "Transfer to Google Drive");
        assert!(recs_d.iter().any(|r| r[2] == "नोंद.docx"));
        assert!(recs_c.iter().any(|r| r[2] == "report, \"final\".docx"));
        assert!(recs_c.iter().any(|r| r[2] == "'=cmd.txt"));
        let folder = recs_c.iter().find(|r| r[2] == "Users").unwrap();
        assert_eq!(folder[4], "Folder");
        assert_eq!(folder[9], "D");

        // Dates: local time, dd-mm-yyyy HH:MM:SS; blank when 0.
        let src = fx.rows.iter().find(|r| r.name == "रवि.txt").unwrap();
        let want = Local.timestamp_millis_opt(src.modified_ms).unwrap().format("%d-%m-%Y %H:%M:%S").to_string();
        assert_eq!(ravi[7], want);
        let want_created = Local.timestamp_millis_opt(src.created_ms).unwrap().format("%d-%m-%Y %H:%M:%S").to_string();
        assert_eq!(ravi[6], want_created);
        // Accessed is unknown (0) for even ids: blank. For odd ids it is a formatted date.
        assert_eq!(src.accessed_ms, 0);
        assert_eq!(ravi[8], "");
        let photo_src = fx.rows.iter().find(|r| r.name == "photo.jpg").unwrap();
        let photo = recs_c.iter().find(|r| r[2] == "photo.jpg").unwrap();
        let want_acc =
            Local.timestamp_millis_opt(photo_src.accessed_ms).unwrap().format("%d-%m-%Y %H:%M:%S").to_string();
        assert_eq!(photo[8], want_acc);
        assert_eq!(photo[9], "A");
        // Hashes and duplicate group.
        let dup_member = recs_c.iter().find(|r| r[2] == "copy of a.pdf").unwrap();
        assert_eq!(dup_member[14], "1");
        assert_eq!(dup_member[11].len(), 64);

        // Progress lines.
        for sheet in ["Sheet: Summary", "Sheet: Plan", "Sheet: Files", "Sheet: Duplicates", "Saving workbook"] {
            assert!(msgs.iter().any(|m| m == sheet), "missing progress {sheet:?} in {msgs:?}");
        }
        assert!(msgs.iter().any(|m| m == &format!("Files: {} rows", fx.rows.len())), "{msgs:?}");
    }

    #[test]
    fn no_files_sheet_without_include_all_files() {
        let fx = fixture();
        let tmp = tempfile::tempdir().unwrap();
        let mut msgs = Vec::new();
        let out = run(&fx, tmp.path(), false, MAX_DATA_ROWS, &mut msgs).unwrap();
        assert_eq!(sheet_names(&out.xlsx), ["Summary", "Plan", "Duplicates", "Largest", "Extensions", "Folders"]);
        // The CSV still has every row.
        assert_eq!(out.csv.len(), 2);
        let (_, recs) = read_csv(&out.csv[0]);
        assert_eq!(recs.len(), fx.rows.iter().filter(|r| r.drive == "C:").count() + 1);
        assert!(!msgs.iter().any(|m| m == "Sheet: Files"));
        // Nothing but the report files is left in the folder (no temp or .part files).
        let mut names: Vec<String> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "Avni Drive Scan 15-10-2026 1042 - C.csv",
                "Avni Drive Scan 15-10-2026 1042 - D.csv",
                "Avni Drive Scan 15-10-2026 1042.xlsx"
            ]
        );
    }

    fn many_rows(n: u32) -> Fixture {
        let mut fx = fixture();
        fx.rows = (0..n)
            .map(|i| {
                row(
                    i,
                    "C:",
                    &format!("C:\\data\\f{i}.txt"),
                    &format!("f{i}.txt"),
                    false,
                    u64::from(i),
                    Verdict::Transfer,
                )
            })
            .collect();
        fx.dups.clear();
        fx.largest.clear();
        fx
    }

    #[test]
    fn sheets_split_at_the_row_limit() {
        let fx = many_rows(2500);
        let tmp = tempfile::tempdir().unwrap();
        let mut msgs = Vec::new();
        let out = run(&fx, tmp.path(), true, 1000, &mut msgs).unwrap();
        assert_eq!(
            sheet_names(&out.xlsx),
            [
                "Summary",
                "Plan",
                "Plan 2",
                "Plan 3",
                "Duplicates",
                "Largest",
                "Extensions",
                "Folders",
                "Files",
                "Files 2",
                "Files 3"
            ]
        );
        assert_eq!(out.rows, 2500);
        // 1000 data rows + header, 1000 + header, 500 + header; every sheet has its own header and filter.
        assert_eq!(sheet_rows(&out.xlsx, 9), 1001);
        assert_eq!(sheet_rows(&out.xlsx, 10), 1001);
        assert_eq!(sheet_rows(&out.xlsx, 11), 501);
        for n in [9, 10, 11] {
            let xml = zip_text(&out.xlsx, &format!("xl/worksheets/sheet{n}.xml"));
            assert!(xml.contains(">Drive<") && xml.contains(">Reason<"), "header missing on sheet {n}");
            assert!(xml.contains("<autoFilter ref=\"A1:Q"), "autofilter missing on sheet {n}");
        }
        assert!(msgs.iter().any(|m| m == "Sheet: Files 3"));
        let (_, recs) = read_csv(&out.csv[0]);
        assert_eq!(recs.len(), 2501);
    }

    #[test]
    fn exact_fit_does_not_add_an_empty_sheet() {
        let fx = many_rows(2000);
        let tmp = tempfile::tempdir().unwrap();
        let out = run(&fx, tmp.path(), true, 1000, &mut Vec::new()).unwrap();
        let names = sheet_names(&out.xlsx);
        assert_eq!(names.iter().filter(|n| n.starts_with("Files")).count(), 2, "{names:?}");
        assert_eq!(names.iter().filter(|n| n.starts_with("Plan")).count(), 2, "{names:?}");
    }

    #[test]
    fn empty_input_still_writes_a_workbook() {
        let mut fx = fixture();
        fx.rows.clear();
        fx.dups.clear();
        fx.largest.clear();
        fx.exts.clear();
        fx.folders.clear();
        fx.summary.drives.clear();
        fx.summary.notes.clear();
        let tmp = tempfile::tempdir().unwrap();
        let out = run(&fx, tmp.path(), true, MAX_DATA_ROWS, &mut Vec::new()).unwrap();
        assert_eq!(out.rows, 0);
        assert!(out.csv.is_empty());
        assert_eq!(
            sheet_names(&out.xlsx),
            ["Summary", "Plan", "Duplicates", "Largest", "Extensions", "Folders", "Files"]
        );
        assert_eq!(sheet_rows(&out.xlsx, 2), 1);
    }

    #[test]
    fn csv_names_are_safe_and_unique() {
        let mut fx = many_rows(3);
        fx.rows[0].drive = "C:".to_string();
        fx.rows[1].drive = "C?".to_string();
        fx.rows[2].drive = "C".to_string();
        let tmp = tempfile::tempdir().unwrap();
        let out = run(&fx, tmp.path(), false, MAX_DATA_ROWS, &mut Vec::new()).unwrap();
        let names: Vec<String> =
            out.csv.iter().map(|p| Path::new(p).file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(
            names,
            [
                "Avni Drive Scan 15-10-2026 1042 - C.csv",
                "Avni Drive Scan 15-10-2026 1042 - C (2).csv",
                "Avni Drive Scan 15-10-2026 1042 - C (3).csv"
            ]
        );
        assert_eq!(drive_file_part("D:\\"), "D");
        assert_eq!(drive_file_part("/mnt/usb disk"), "mntusb disk");
        assert_eq!(drive_file_part(":"), "drive");
        assert_eq!(sanitize_name("15/10/2026 10:42", '-'), "15-10-2026 10-42");
    }

    #[test]
    fn failed_pass_leaves_no_partial_files() {
        let fx = fixture();
        let tmp = tempfile::tempdir().unwrap();
        let rows = &fx.rows;
        let visit = |cb: &mut dyn FnMut(&FileRow) -> Result<()>| -> Result<()> {
            for (i, r) in rows.iter().enumerate() {
                if i == 20 {
                    anyhow::bail!("scan cancelled");
                }
                cb(r)?;
            }
            Ok(())
        };
        let input = ReportInput {
            summary: &fx.summary,
            dup_groups: &fx.dups,
            largest: &fx.largest,
            extensions: &fx.exts,
            folders: &fx.folders,
            for_each_row: &visit,
        };
        let opts = ReportOptions { include_all_files: true, stamp: "15-10-2026 1042".to_string() };
        let err = write_reports(&input, tmp.path(), &opts, &mut |_| {}).unwrap_err();
        assert!(err.to_string().contains("scan cancelled"), "{err:#}");
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0, "partial files were left behind");
    }

    #[test]
    fn attribute_letters() {
        let mut s = String::new();
        attr_string(0, &mut s);
        assert_eq!(s, "");
        attr_string(0x1 | 0x2 | 0x4 | 0x10 | 0x20, &mut s);
        assert_eq!(s, "RHSDA");
        attr_string(0x800 | 0x4000 | 0x200 | 0x400 | 0x1000, &mut s);
        assert_eq!(s, "CEPLO");
        attr_string(0x40_0000, &mut s);
        assert_eq!(s, "cloud");
        attr_string(0x20 | 0x4_0000, &mut s);
        assert_eq!(s, "A cloud");
        attr_string(0x80, &mut s);
        assert_eq!(s, "");
    }

    #[test]
    fn excel_serials_and_blank_dates() {
        let dt = NaiveDate::from_ymd_opt(2026, 10, 15).unwrap().and_hms_opt(10, 42, 0).unwrap();
        let want = 46310.0 + 38_520.0 / 86_400.0;
        assert!((excel_serial(&dt).unwrap() - want).abs() < 1e-9);
        let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap().and_hms_opt(0, 0, 0).unwrap();
        assert_eq!(excel_serial(&epoch), Some(25_569.0));
        let too_early = NaiveDate::from_ymd_opt(1899, 1, 1).unwrap().and_hms_opt(0, 0, 0).unwrap();
        assert_eq!(excel_serial(&too_early), None);
        assert!(local_stamp(0).is_none());
        assert!(local_stamp(-1).is_none());
        assert!(local_stamp(i64::MAX).is_none());
        let st = local_stamp(1_760_000_000_000).unwrap();
        assert!((excel_serial(&st.dt).unwrap() - st.serial).abs() < 1e-12);
        let mut out = String::new();
        push_csv_date(&mut out, &dt);
        assert_eq!(out, "15-10-2026 10:42:00");
    }

    #[test]
    fn thousands_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(300_000), "300,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn long_text_is_clipped_not_fatal() {
        let long = "x".repeat(40_000);
        assert_eq!(clip(&long).chars().count(), MAX_CELL_CHARS);
        let hindi = "र".repeat(40_000);
        assert_eq!(clip(&hindi).chars().count(), MAX_CELL_CHARS);
        let mut fx = many_rows(2);
        fx.rows[0].path = format!("C:\\{long}");
        fx.rows[1].reason = hindi;
        let tmp = tempfile::tempdir().unwrap();
        let out = run(&fx, tmp.path(), true, MAX_DATA_ROWS, &mut Vec::new()).unwrap();
        assert_eq!(out.rows, 2);
    }

    /// 1,000,000 rows into Plan + Files + CSV. Run with:
    /// `cargo test --release -- --ignored --nocapture perf_one_million_rows`
    #[test]
    #[ignore]
    fn perf_one_million_rows() {
        const N: u32 = 1_000_000;
        const SPLIT: u32 = 700_000;
        let mut fx = fixture();
        fx.rows.clear();
        fx.summary.files = u64::from(N);
        let tmp = tempfile::tempdir().unwrap();
        let rows_done = std::cell::Cell::new(0u64);
        let visit = |cb: &mut dyn FnMut(&FileRow) -> Result<()>| -> Result<()> {
            let mut r = row(0, "C:", "", "", false, 0, Verdict::Keep);
            r.ext = "pdf".to_string();
            r.sha256 = Some("0".repeat(64));
            for i in 0..N {
                let drv = if i < SPLIT { "C:" } else { "D:" };
                r.id = i;
                r.drive.clear();
                r.drive.push_str(drv);
                r.name.clear();
                let _ = write!(r.name, "file_{i:07}.pdf");
                r.path.clear();
                let _ = write!(
                    r.path,
                    "{drv}\\Users\\ravi\\Documents\\Project {}\\Folder {}\\{}",
                    i / 1000,
                    i / 100,
                    r.name
                );
                r.is_dir = i % 20 == 0;
                r.size = (u64::from(i) * 7919) % 50_000_000;
                r.created_ms = 1_600_000_000_000 + i64::from(i) * 60_000;
                r.modified_ms = r.created_ms + 1_000_000;
                r.accessed_ms = if i % 3 == 0 { 0 } else { r.created_ms + 5_000_000 };
                r.attributes = if r.is_dir { 0x10 } else { 0x20 };
                r.blake3 = if i % 2 == 0 {
                    Some(format!("{:016x}{:016x}{:016x}{:016x}", i, i + 1, i + 2, i + 3))
                } else {
                    None
                };
                r.sample = Some(format!("{:016x}{:016x}{:016x}{:016x}", i + 4, i + 5, i + 6, i + 7));
                r.dup_group = if i % 50 == 0 { Some(i / 50) } else { None };
                r.verdict = Verdict::ALL[(i % 9) as usize];
                r.reason = "documents folder: belongs on Google Drive".to_string();
                cb(&r)?;
            }
            Ok(())
        };
        let input = ReportInput {
            summary: &fx.summary,
            dup_groups: &fx.dups,
            largest: &fx.largest,
            extensions: &fx.exts,
            folders: &fx.folders,
            for_each_row: &visit,
        };
        let opts = ReportOptions { include_all_files: true, stamp: "15-10-2026 1042".to_string() };
        let t = Instant::now();
        let mut progress = |m: &str| {
            if m.starts_with("Files:") && m.contains("00,000 rows") {
                rows_done.set(rows_done.get() + 1);
            }
            println!("[{:6.1}s] {m}", t.elapsed().as_secs_f64());
        };
        let out = write_reports(&input, tmp.path(), &opts, &mut progress).unwrap();
        let secs = t.elapsed().as_secs_f64();
        let size_mb = |p: &str| std::fs::metadata(p).unwrap().len() as f64 / MIB;
        println!(
            "PERF rows={} elapsed={secs:.1}s (report ms={}) xlsx={:.1} MB csv={:?} MB sheets={:?}",
            out.rows,
            out.ms,
            size_mb(&out.xlsx),
            out.csv.iter().map(|p| size_mb(p).round() as u64).collect::<Vec<_>>(),
            sheet_names(&out.xlsx),
        );
        assert_eq!(out.rows, u64::from(N));
    }
}
