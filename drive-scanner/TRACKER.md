# AVNI Drive Scanner - tracker

Mirror of TRACKER.xlsx (same rows). Statuses: Done / Partial / Pending / Can be done / Cannot do.

| Item | Status | Time taken | ETA | Notes |
|---|---|---|---|---|
| 1. Drive picker: every drive, Select all, type, filesystem, size, free, HDD/SSD | Partial | 15-10-2026 | next Windows run | Built. Proven in the real app on Linux (real IPC, mouse clicks). Windows API code compiles; NOT RUN on Windows yet. |
| 2. Fast metadata scan: NTFS MFT reader (needs admin) | Done | 15-10-2026 |  | 3 GB NTFS image, 67,103 entries: 0 differences vs folder walk; 0.11 s vs 2.69 s. Hard cases proven: 221 hard links via $ATTRIBUTE_LIST, DOS names hidden, Hindi/Marathi names, sparse, fragmented. REAL WINDOWS (GitHub runner C:, run 2): 1,201,506 files + 208,449 folders, 131 GB, MFT read in 10.3 s. |
| 3. Fallback scan: parallel folder walk (exFAT/FAT32, no admin, network) | Done | 15-10-2026 |  | REAL WINDOWS: Program Files 304,060 entries, walk 100 s vs MFT 3.8 s (whole volume), 0 differences. System32 23,053 entries: 13 live log files differ (folder listing stale while Windows writes them; MFT is the true value). |
| 4. Google Drive volume: metadata only, never reads file content | Partial | 15-10-2026 | Ravi's PC | Walk only, no file opened. NOT TESTED against a real Google Drive for Desktop volume. |
| 5. Per-file metadata: path, name, ext, size, created/modified/accessed, attributes, hard links | Done | 15-10-2026 |  | In Files tab, Excel Files sheets, CSV per drive. |
| 6. Duplicates: size -> sampled BLAKE3 -> full BLAKE3, disk order on HDD | Done | 15-10-2026 |  | 60,256 files: 257 sets confirmed by full hash. Near-duplicate (same size, same first/last 16 KB) correctly rejected. |
| 7. Deep profile: full BLAKE3 of every file, optional SHA-256 | Partial | 15-10-2026 | Ravi's PC | Built, hashing unit-tested; not timed on a real drive. |
| 8. 2 TB HDD under 15 min (Quick profile) | Pending |  | Ravi's PC | NOT MEASURED. Quick stops full verification at the time budget by design. Full hash of 2 TB on HDD = 3-4.5 h (physics). |
| 9. Evaluation rules: keep / transfer / duplicate / junk / extract / protected / Supabase | Done | 15-10-2026 |  | 34 cases on a Windows-shaped C:/D:/G: tree, incl. .avniprotect. |
| 10. Protected list (copilotremap.ahk, .ahk, D:\AE Brain, AVNI worktrees, %LOCALAPPDATA%\Avni, D:\AVNI DATA) | Done | 15-10-2026 |  | Never selected by any action, even when asked by id (tested). |
| 11. Excel report: Summary, Plan, Duplicates, Largest, Extensions, Folders, Files | Done | 15-10-2026 |  | 1,000,000 rows in 35.5 s, 129.8 MB. Duplicates sheet shows Keep / Remove / Kept (tree law) + why. |
| 12. CSV full listing per drive | Done | 15-10-2026 |  | UTF-8 BOM, no row limit. |
| 13. Action: Extract archives (zip, 7z, tar, gz, bz2 native; rar/iso via 7-Zip) | Partial | 15-10-2026 | Ravi's PC | 34 tests on Linux incl. zip-slip and zip-bomb. NOT RUN on Windows. |
| 14. Action: Unblob = recursive nested extraction with zip-bomb guard | Partial | 15-10-2026 | Ravi's PC | Tested (nested depth, shared budget). Meaning of 'unblob' assumed = nested archives; confirm. |
| 15. Action: Remove duplicates under THE TREE LAW (full verify, then Recycle Bin) | Done | 15-10-2026 |  | Real app run on the NTFS volume: exactly the 4 loose copies removed; filed, project, program, protected copies all kept. Recycle Bin itself: Windows NOT RUN. |
| 16. Action: Transfer to Google Drive (copy, verify, source to Recycle Bin) | Partial | 15-10-2026 | Ravi's PC | Tested end to end with a stand-in folder. Real Google Drive for Desktop NOT TESTED. Account law enforced (blocked until the AVNI upload account is confirmed). |
| 17. Action: Clean junk (to Recycle Bin) + Empty Recycle Bin | Partial | 15-10-2026 | Ravi's PC | Windows-only calls compile; NOT RUN on Windows. |
| 18. Action: Publish index to Supabase | Partial | 15-10-2026 |  | AUTH REQUIRED: needs URL + service key in Settings and supabase/drive_files.sql run once. Not run. |
| 19. Google Drive hash match + appProperties avni_moved=1 through Drive API | Pending |  |  | AUTH REQUIRED (Drive API sign-in). |
| 20. Native app: Tauri 2 + React, tabs, dark/light, tables | Partial | 15-10-2026 | Ravi's PC | Proven on Linux build with real engine (mouse click-through). Windows exe BUILT (15.5 MB); not yet opened on Ravi's PC. |
| 21. Windows build (GitHub Actions, portable exe, no shortcuts) | Done | 15-10-2026 |  | Run 2 green: artifact AVNI-Drive-Scanner-windows-x64, 15.5 MB, sha256 53BCF921F67C9CB0815DB9103E96356B716B75C169E6FA2A1775AE252E930EE9. All 60 engine tests pass on Windows. |
| 22. Real-runtime check on Windows NTFS (CI runner C:) | Done | 15-10-2026 |  | Quick scan of C: (1.2M files, 131 GB, SSD): metadata 4.5 s, total 40.5 s, 467 duplicate sets confirmed, 800 copies kept by the tree law. Excel + CSV 1.41M rows in 21.9 s. |
| 23. Measured on Ravi's own 2 TB HDD | Pending |  | Ravi's PC | NOT MEASURED. |
| 24. Register in Avni Productivity Pack + Avni Store | Cannot do |  | local session | Not reachable from the cloud session; owed to the next session on the laptop. |
| 25. Mirror into AEBrain repo | Pending |  | Ravi's OK | This session may push only to sehgalautoriders/avni. Needs Ravi's OK to write into AEBrain. |
| 26. Desktop subject .md + HANDOFF_TO_CHATGPT_ARCHITECT.md | Partial | 15-10-2026 | local session | HANDOFF.md written in this repo. Desktop not reachable from the cloud. |
| 27. THE TREE LAW in dedup (port of AvniClean.MayBeRemoved, home rule, HOME CLEARS off) | Done | 15-10-2026 |  | 6 tests replay AvniClean's self-test cases; all pass. |
| 28. Google Drive account law in Transfer | Done | 15-10-2026 |  | Explicit folder + account required; any other account refused. Which account a DriveFS letter belongs to: CONFIGURED - UNVERIFIED (Ravi confirms in Settings). |
| 30. Finished files, in-app preview, in-app report/log viewer (Ravi: show everything inside the app) | Partial | 15-10-2026 | today | Engine + commands done and tested (3 tests). Screens in progress. |
| 29. MOVED ledger (D:\AVNI DATA\_MOVED LEDGER\moved-ledger.csv) | Partial | 15-10-2026 | Ravi's PC | Written when that folder exists. NOT RUN on Ravi's PC. |
