"""
host/report_job.py - the report-job contract between the Omni / Agent Workbench guest and the host.

State on 21/22-10-2026 (measured, not assumed): the Hyper-V VM "AVNI Agent Workbench 01" exists on E:
with its VHDX, isolation applied, the guest hands service (guest/avni_hands.py) and the host driver
exist as code and were dry-run on host loopback only; the guest screenshot at 20:17 shows the boot
splash - no Windows, no Office in the guest. So guest UI automation is BLOCKED and this file runs the
same contract against a local sample file instead. When the guest is alive, step 1 is the only part
that changes (the guest produces the report and drops it in the exchange folder).

Contract (one job = one folder under %LOCALAPPDATA%\\Avni\\workbench\\jobs\\<job_id>):
  1. produce   : the ORIGINAL report file (guest later; tonight: a local sample) -> source/
  2. handoff   : manifest.json with sha256 of every file, recipe name, who produced it, when; the
                 host accepts a file only when its sha256 matches the manifest (signed = hash-checked)
  3. ingest    : the CRM head lane reads the head (headings, signature) -> ingest.sqlite envelope
  4. recipe    : a repeatable, named computation over the rows with row-level traceability
  5. publish   : report.json + report.md with every figure carrying its source rows

Recipes tonight:
  jc_due_match  - job cards (Reg No / JC No) matched against a due list (VIN / Customer / Due date) by
                  registration or VIN -> matched / unmatched counts, each with the source rows.
Never: regional / West-Zone retention or any figure whose source coverage is not in the job folder.

  python host/report_job.py new --recipe jc_due_match --source <jc.xlsx> --source <due.xlsx>
  python host/report_job.py run <job_id>
"""
from __future__ import annotations
import argparse, datetime as dt, hashlib, json, os, shutil, sys, uuid
from pathlib import Path

JOBS = Path(os.environ.get("LOCALAPPDATA", r"C:\Users\sehga\AppData\Local")) / "Avni" / "workbench" / "jobs"
HEAD_LANE_DIR = Path(r"C:\02 - OUR FILES\Access to all drives\AVNI\90 - WORKTREES\Active\AVNI-FILES-JOB-CRM-HEAD-LANE\software\Avni Excel Join and Clean\engine")


def sha256(p: Path) -> str:
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def now() -> str:
    return dt.datetime.now().strftime("%d-%m-%Y %H:%M:%S")


def new(recipe: str, sources: list[Path], produced_by: str = "local-sample (guest BLOCKED: no OS in the VM)") -> dict:
    job_id = dt.datetime.now().strftime("%Y%m%d-%H%M%S") + "-" + uuid.uuid4().hex[:6]
    d = JOBS / job_id; (d / "source").mkdir(parents=True)
    files = []
    for s in sources:
        dst = d / "source" / s.name
        shutil.copy2(s, dst)                       # the original is never moved; the job holds a copy
        files.append({"name": s.name, "sha256": sha256(dst), "size": dst.stat().st_size, "origin": str(s)})
    man = {"job_id": job_id, "recipe": recipe, "produced_by": produced_by, "produced_at": now(), "files": files, "guest_ui_automation": "BLOCKED - no guest OS yet",
           "handoff": "hash-checked: the host accepts a file only when its sha256 equals this manifest"}
    (d / "manifest.json").write_text(json.dumps(man, indent=1, ensure_ascii=False), encoding="utf-8")
    return man


def _rows(path: Path, limit: int = 20000) -> tuple[list, list, dict]:
    """(headings, rows, head-envelope) via the head lane for the head, openpyxl for the rows (local sample only)."""
    sys.path.insert(0, str(HEAD_LANE_DIR))
    import avni_head_lane as hl
    rd = hl.LocalRange(str(path))
    try:
        h = hl.head_of(rd, path.suffix.lower())
    finally:
        rd.close()
    hd = h.get("head") or {}
    hidx = hd.get("header_row", 0)
    import openpyxl
    wb = openpyxl.load_workbook(path, read_only=True, data_only=True)
    ws = wb[hd["sheet"]] if hd.get("sheet") in wb.sheetnames else wb[wb.sheetnames[0]]
    rows = []
    for i, r in enumerate(ws.iter_rows(values_only=True)):
        if i <= hidx:
            continue
        rows.append([("" if v is None else str(v)).strip() for v in r])
        if len(rows) >= limit:
            break
    wb.close()
    return hd.get("headings", []), rows, {"sig": hd.get("sig"), "sheet": hd.get("sheet"), "header_row": hidx, "bytes_read": h.get("bytes_read")}


def _col(headings: list, *names) -> int:
    norm = ["".join(ch for ch in h.lower() if ch.isalnum()) for h in headings]
    for n in names:
        if n in norm:
            return norm.index(n)
    return -1


def recipe_jc_due_match(job: Path, man: dict) -> dict:
    src = [job / "source" / f["name"] for f in man["files"]]
    for f in man["files"]:
        if sha256(job / "source" / f["name"]) != f["sha256"]:
            raise SystemExit("handoff refused: %s does not match its manifest hash" % f["name"])
    heads = [(p, *_rows(p)) for p in src]
    jc = next((h for h in heads if _col(h[1], "jcno", "jobcardno", "jcnumber") >= 0), None)
    due = next((h for h in heads if h is not jc and (_col(h[1], "vin", "chassisno") >= 0 or _col(h[1], "regno", "registrationno") >= 0)), None)
    if not jc or not due:
        return {"ok": False, "why": "need one job-card file (JC No) and one due list (VIN or Reg No)", "heads": [(p.name, h[:6]) for p, h, _, _ in heads]}
    jc_p, jc_h, jc_rows, jc_env = jc; due_p, due_h, due_rows, due_env = due
    jr = _col(jc_h, "regno", "registrationno", "vehicleno"); jv = _col(jc_h, "vin", "chassisno")
    dr = _col(due_h, "regno", "registrationno", "vehicleno"); dv = _col(due_h, "vin", "chassisno")
    key_kind = "vin" if (jv >= 0 and dv >= 0) else ("reg" if (jr >= 0 and dr >= 0) else "")
    if not key_kind:
        return {"ok": False, "why": "no common key (VIN or registration) between the two files", "jc_headings": jc_h[:10], "due_headings": due_h[:10]}
    jk, dk = (jv, dv) if key_kind == "vin" else (jr, dr)
    norm = lambda s: "".join(ch for ch in s.upper() if ch.isalnum())  # noqa: E731
    due_index = {}
    for i, r in enumerate(due_rows):
        k = norm(r[dk]) if dk < len(r) else ""
        if k:
            due_index.setdefault(k, []).append(i + due_env["header_row"] + 2)
    matched, unmatched = [], []
    for i, r in enumerate(jc_rows):
        k = norm(r[jk]) if jk < len(r) else ""
        if not k:
            continue
        row_no = i + jc_env["header_row"] + 2
        if k in due_index:
            matched.append({"key": k, "jc_row": row_no, "due_rows": due_index[k][:5]})
        else:
            unmatched.append({"key": k, "jc_row": row_no})
    rep = {"ok": True, "recipe": "jc_due_match", "key": key_kind, "job_cards": {"file": jc_p.name, "sheet": jc_env["sheet"], "sig": jc_env["sig"], "rows": len(jc_rows)},
           "due_list": {"file": due_p.name, "sheet": due_env["sheet"], "sig": due_env["sig"], "rows": len(due_rows), "distinct_keys": len(due_index)},
           "matched": len(matched), "unmatched": len(unmatched), "matched_examples": matched[:10], "unmatched_examples": unmatched[:10],
           "coverage_note": "figures cover ONLY the two files in this job folder; nothing regional or zonal is claimed", "at": now()}
    (job / "report.json").write_text(json.dumps(rep, indent=1, ensure_ascii=False), encoding="utf-8")
    md = ["# Job cards vs due list - %s" % man["job_id"], "", "Key: %s" % key_kind,
          "Job cards: %s (sheet %s, %d rows, signature %s)" % (jc_p.name, jc_env["sheet"], len(jc_rows), jc_env["sig"]),
          "Due list: %s (sheet %s, %d rows, %d distinct keys, signature %s)" % (due_p.name, due_env["sheet"], len(due_rows), len(due_index), due_env["sig"]), "",
          "| Result | Count |", "|---|---|", "| Matched | %d |" % len(matched), "| Unmatched | %d |" % len(unmatched), "",
          "Every matched line names its job-card row and the due-list rows; see report.json.", "",
          "Coverage: these two files only. Guest UI automation: BLOCKED (no OS in the VM)."]
    (job / "report.md").write_text("\n".join(md), encoding="utf-8")
    return rep


def run(job_id: str) -> dict:
    job = JOBS / job_id
    man = json.loads((job / "manifest.json").read_text(encoding="utf-8"))
    if man["recipe"] == "jc_due_match":
        return recipe_jc_due_match(job, man)
    return {"ok": False, "why": "unknown recipe " + man["recipe"]}


if __name__ == "__main__":
    ap = argparse.ArgumentParser(); sub = ap.add_subparsers(dest="cmd")
    n = sub.add_parser("new"); n.add_argument("--recipe", required=True); n.add_argument("--source", action="append", required=True)
    r = sub.add_parser("run"); r.add_argument("job_id")
    a = ap.parse_args()
    if a.cmd == "new":
        print(json.dumps(new(a.recipe, [Path(s) for s in a.source]), indent=1, ensure_ascii=False))
    elif a.cmd == "run":
        out = run(a.job_id); out.pop("matched_examples", None); out.pop("unmatched_examples", None)
        print(json.dumps(out, indent=1, ensure_ascii=False))
