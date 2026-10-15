# AVNI Drive Scanner — handoff (15-10-2026)

Branch `ccr-2aef8dcd-du1iqm`, repo `sehgalautoriders/avni`, folder `drive-scanner/`. Status of every item: `TRACKER.md`.

## What Ravi asked (15-10-2026)

1. An app that scans the drive in the least time possible. Any hash is allowed (BLAKE3, SHA-256, SHA-3).
2. Multi-drive: select all drives and get the metadata of every file.
3. On an HDD, 2 TB must finish in under 15 min.
4. Excel (or similar) output for evaluation.
5. Evaluate what to keep and what to transfer:
   - C: holds no more than Windows and installed apps need.
   - D: and E: are external and should be empty.
   - Google Drive is the storage, and processed data goes to Supabase.
6. All drives, with unzip, unblob and dedup like the Avni runtime, but clicked by the user, not automated.
7. Added mid-chat: **"Dedup should be as per the tree law. Please check our AVNI tree law before doing dedup."**

## What exists now

- **Rust engine** (`engine/`):
  - NTFS MFT reader, plus a parallel folder walk as fallback.
  - Sampled and full BLAKE3, with optional SHA-256.
  - THE TREE LAW (port of AvniClean) and the location rules.
  - Actions, Excel/CSV, Supabase publisher.
  - `avni-scan` CLI for verification.
- **Tauri 2 + React app** (`app/`): 8 tabs (Drives, Progress, Summary, Files, Duplicates, Plan & Actions, Reports, Settings),
  dark and light themes, tables, nothing automatic.
- **CI** (`.github/workflows/drive-scanner.yml`):
  - Builds the portable Windows exe.
  - Runs MFT vs Windows' own listing on a real NTFS C:.
  - Runs a timed Quick scan.

## Measured (cloud build machine, 15-10-2026, NTFS test images via ntfs-3g)

- **MFT vs walk:** 67,103 entries, **0 differences** (name, size, folder flag, modified time). MFT took 0.11 s, the walk 2.69 s.
- **Real app, scan by mouse click through the real IPC:** 60,256 files, metadata in 417 ms, 257 sets confirmed by full hash, total 19.6 s.
- **Tree-law run in the real app:** 17 test copies across 7 sets.
  - Exactly the 4 loose copies were removed: Downloads notes.pdf, the pulled-out return.pdf, and two Dump copies.
  - Every filed, project, program and `.avniprotect` copy is still on disk.
- **Earlier dedup run, BEFORE the tree law was added (test image only, not Ravi's data):** 7,250 copies removed.
  On disk, all of them were gone, all 257 keepers were present, and 40 of 40 sampled copies matched their keeper byte for byte.
  That run used my own keeper rule, which has since been deleted. The tree law is now the only keeper rule.
- **Excel:** 1,000,000 rows in 35.5 s.
- **Real Windows NTFS** (GitHub Windows runner, C:, CI run 2, 15-10-2026):
  - Raw MFT read: 1,201,506 files and 208,449 folders, 131 GB, in 10.3 s.
  - MFT vs Windows' own listing:
    - Program Files: 304,060 entries, 0 differences. The listing took 100 s; the whole-volume MFT read took 3.8 s.
    - System32: 23,053 entries, 13 live log files differ. Windows' folder listing is stale for files it is still writing, so the MFT is the true value.
  - Quick scan of all of C:: metadata 4.5 s, total 40.5 s. Excel + CSV: 1.41M rows in 21.9 s.
  - All 60 engine tests pass on Windows. The exe builds (15.5 MB).
- **NOT MEASURED:**
  - on Ravi's own PC
  - the 2 TB HDD target (the runner's C: is an SSD)
  - a real Google Drive for Desktop volume
  - Supabase

## Laws read and applied

| Law | Source | How the app obeys it |
|---|---|---|
| THE TREE LAW + home rule + HOME CLEARS (29-08-2026) | AEBrain `a006d7f` `software/Avni Copy/02_Source/AvniCopy/AvniClean.cs` | `engine/src/treelaw.rs`, rule for rule. AvniClean's self-test cases replayed as tests. |
| C: = OVEN LAW (28-09-2026) | AVNI_AI_COMMON_LAW.md | C: keeps Windows + apps. `D:\AVNI DATA` (junction targets) protected. MOVED ledger rows written. |
| AVNI Cloud Upload Account Law (26-09-2026) | AVNI_AI_COMMON_LAW.md §19 | Transfer blocked until the folder AND account the AVNI upload account are set in Settings. the read-only account refused. |
| AVNI Verification Law (27-09-2026) | AVNI_AI_COMMON_LAW.md §20 | Tracker separates proven (measured) from built and NOT RUN. V15 (a non-builder verifies before PROVEN) is still owed. |

## Decisions only Ravi can make (WAITING)

1. **Is D: empty, or is it the offload disk?**
   - Today's words say D: and E: should be empty.
   - The 28-09 OVEN LAW says `D:\AVNI DATA` is the local offload, and C: folders are junctions into it. `D:\AE Brain` is also a live git tree.
   - Emptying D: as it stands would break those C: paths. So `D:\AE Brain` and `D:\AVNI DATA` are **protected by default**, and the app will not move them.
   - If D: must be empty, the C: junctions need re-pointing first. That is a separate, planned migration, not a scanner click.
2. **Home places for the tree law:** which drive or folder is the "ultimate" home? Empty by default. HOME CLEARS stays off until you tick it.
3. **Unblob:** I took it to mean opening archives inside archives, as the open-source `unblob` tool does. If the Avni runtime means something else, say so.
4. **Placement law:** may I copy `drive-scanner/` into the AEBrain repo? This session may only push to `sehgalautoriders/avni`.

## CLAUDE RIGHT-HAND RECOMMENDATION

- **What the problem really is:** the slow part of "scan 2 TB" is reading bytes, not hashing them. An HDD reads
  120–200 MB/s, so hashing every byte of 2 TB takes 3–4.5 h with any hash.
  - The 15-min target is met by reading the MFT (seconds) and hashing only same-size files.
  - Hashing is sampled first and full only where the samples match. Reads go in on-disk order, and stop at a time budget.
  - Full verification always happens before anything is removed.
- **The risk the Boss may not be seeing:**
  - The tree law now exists in two codebases, C# (AvniClean) and Rust (this app). Its own note says
    "never write a second copy of the tree law". Two copies will drift.
  - **Recommendation:** freeze the rule tables (dump names, copy-name pattern, program folders, tree markers, walk
    boundaries, heap numbers) into one shared spec file that both read. Or move Avni Clean onto this engine.
  - Until then, AvniClean.cs is canonical, and `treelaw.rs` names the exact commit it was ported from.
- **Second risk:** Google Drive for Desktop caches every upload on C: until the upload finishes. A large transfer
  temporarily fills C:, which breaks the OVEN LAW in the other direction. The Transfer action therefore pauses
  whenever C: free space drops under the reserve (default 20 GB).
- **What I'd do if I owned it:**
  1. Let CI prove the Windows build and the MFT-vs-listing check.
  2. Run Quick on Ravi's real D: (read-only), and measure.
  3. Only then run Remove duplicates, on one folder, with the Recycle Bin.
  4. Decide the D: question before any Transfer.

## Persistence check

| What | Updated? |
|---|---|
| Repo: README, TRACKER.xlsx/.md, this HANDOFF.md | Updated |
| Desktop subject .md, HANDOFF_TO_CHATGPT_ARCHITECT.md, NEXT-CHAT-PROMPT.md, memory, Antigravity context | **CANNOT from the cloud session.** The next local session copies this file there. |
| Avni Productivity Pack + Avni Store registration | **CANNOT from the cloud.** Owed to the next local session. |
