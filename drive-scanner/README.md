# AVNI Drive Scanner

One native Windows app, driven by clicks: scan every drive, get the metadata of every file, find duplicates under
**THE TREE LAW**, decide what stays and what goes to Google Drive, export Excel/CSV, and act (extract, remove duplicates,
transfer, clean junk, publish the index to Supabase). Nothing runs on its own. Every action shows a preview first, and
a person presses Run.

Ravi's pattern: **the laptop (C:) is the owner** and keeps only Windows and installed apps. **D:/E: are external** and
should stay empty. **Google Drive is the storage** (the AVNI upload account only). **Supabase holds processed data.**

## How fast, honestly

| What | How | Measured |
|---|---|---|
| File list of an NTFS drive | Reads the master file table (MFT) straight off the volume in 8 MB sequential chunks and parses records in parallel. Needs admin. A 1-million-file drive has a ~1 GB MFT, and an HDD reads that in about 8–10 s. | 3 GB NTFS test volume, 67,103 entries: **0.11 s**, against 2.69 s for a folder walk. **0 differences** between the two in name, size, folder flag or modified time (15-10-2026, cloud test machine). |
| exFAT / FAT32 / no admin / network | Parallel folder walk (FindFirstFileExW, basic info, large fetch). | Same volume: 0.45–2.69 s. |
| Duplicates | Same size → BLAKE3 of size + first and last 16 KB → full BLAKE3 only for matches. One reader per HDD, in on-disk order, so the head sweeps forward. | 60,256 files: 257 sets confirmed by full hash, 19.6 s in total. |
| **2 TB HDD under 15 min** | The Quick profile stops full verification at the time budget (default 15 min). Unverified sets stay "sampled" and are fully verified before anything moves. | **NOT MEASURED.** It needs a run on Ravi's own drive. |
| Every byte of 2 TB on an HDD | The Deep profile. The limit is the disk, not the hash: an HDD reads 120–200 MB/s, so 2 TB takes **3–4.5 hours** whatever hash is used. | not run |
| Excel report | Constant-memory xlsx, sheets split at 1,048,575 rows, plus a full CSV per drive. | 1,000,000 rows in 35.5 s, 129.8 MB xlsx. |

Why BLAKE3: it hashes at several GB/s per core, so the disk is always the bottleneck. SHA-256 is optional in Deep
mode, computed in the same read, because Google Drive publishes a sha256 for every file.

## The laws this app obeys

- **THE TREE LAW (Avni Clean, 29-08-2026)** decides every duplicate set. It is a word-for-word port of `AvniClean.MayBeRemoved`
  (AEBrain `a006d7f`, `software/Avni Copy/02_Source/AvniCopy/AvniClean.cs`), in `engine/src/treelaw.rs`.
  - A copy filed inside an organised tree is never removed.
  - Only a loose copy may go:
    - it sits in a dumping ground (Downloads, Temp, New folder…), or
    - its name marks it as a copy ("x - Copy", "x (2)"), or
    - its folder is at least 60% duplicates, or
    - it was pulled out of its own folder, or
    - it sits beside the kept copy, or
    - it was tipped into a flat heap (25+ files, no subfolders, filed twin 2+ levels deeper).
  - Never touched, under any setting:
    - system locations
    - protected and `.avniprotect` folders
    - programs (.exe/.dll/…)
    - package folders (node_modules, .venv, .git…)
    - project trees (package.json, *.csproj, Cargo.toml…)
  - The **home list** (Settings) only chooses the keeper. The tree law wins every collision.
  - **HOME CLEARS** is off unless Ravi ticks it, and it is fenced exactly as in Avni Clean.
  - The tests replay Avni Clean's own self-test cases.
- **C: = OVEN LAW (28-09-2026):**
  - C: keeps Windows and installed programs only.
  - Moved C: folders are junctions into `D:\AVNI DATA`, so that folder is protected by default.
  - Every transfer is written to `D:\AVNI DATA\_MOVED LEDGER\moved-ledger.csv` with the tag `MOVED`.
- **AVNI Cloud Upload Account Law:**
  - Transfer writes only to the Google Drive folder set in Settings, and only when its account is confirmed as
    the AVNI upload account.
  - It never auto-detects the folder, never falls back to another account, and never writes to the read-only account.
- **Google Drive is never downloaded.** Its files are listed from folder entries only. "Already on Google Drive"
  matches name + size, and the Transfer action reads the Drive copy only to compare before removing a local copy.
- **Protected by default:**
  - `copilotremap.ahk` (part of the keyboard) and every `.ahk` file
  - `D:\AE Brain`
  - `C:\02 - OUR FILES\Access to all drives\AVNI`
  - `%LOCALAPPDATA%\Avni`
  - `D:\AVNI DATA`
- **Nothing is deleted for good:**
  - Removals go to the Recycle Bin. Removable drives have no bin, so on those they go to `X:\AVNI Quarantine\<date>`.
  - Only "Empty Recycle Bin" frees space for real, and only when pressed.
  - A file that changed since the scan is skipped.
  - Every item is logged as CSV in `%LOCALAPPDATA%\Avni\reports\DriveScanner\logs`.

## Layout

```
engine/            Rust library + avni-scan verification CLI
  src/ntfs.rs        MFT reader (generic over Read+Seek; tested on real NTFS images)
  src/walk.rs        parallel folder walk
  src/treelaw.rs     THE TREE LAW (port of AvniClean)
  src/rules.rs       keep / transfer / junk / extract / Supabase / review, per Ravi's drive pattern
  src/hash.rs        sampled + full BLAKE3, SHA-256, disk-order lanes
  src/scan.rs        one scan end to end
  src/actions.rs     extract, remove duplicates, transfer, clean junk, empty bin, publish
  src/report.rs      Excel + CSV
  src/extract.rs     unzip / unblob (zip, 7z, tar, gz, bz2 native; rar/iso/xz via 7-Zip; zip-slip + zip-bomb guards)
app/               Tauri 2 shell + React UI (tabs, dark/light, tables). app/src/api.ts is the only bridge.
supabase/          drive_files.sql — the table for "Publish index to Supabase"
```

## Build

The Windows exe is built by `.github/workflows/drive-scanner.yml` on a Windows runner. The workflow also runs the
real-NTFS check (MFT vs Windows' own folder listing on C:) and a timed scan. The exe is uploaded as the workflow
artifact `AVNI-Drive-Scanner-windows-x64`. It is portable: no installer, no Desktop or Start Menu shortcut. It is
launched from the Avni Productivity Pack. Releases are never created here, because this repo's "latest" release
belongs to the AVNI Phone page.

Local: `cd app && npm ci && npx tauri build --no-bundle` → `target/release/avni-drive-scanner.exe`.

Status of every item: `TRACKER.xlsx` (mirror `TRACKER.md`). Handoff: `HANDOFF.md`.
