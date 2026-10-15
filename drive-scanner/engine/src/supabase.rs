//! Publishes the file index (metadata, hashes, verdicts — never file contents) to a Supabase table through PostgREST.
//! Upsert on (machine, drive, path), so publishing again updates rows instead of duplicating them.
//! Table definition: drive-scanner/supabase/drive_files.sql.

use crate::index::Index;
use crate::model::Settings;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const BATCH: usize = 1000;

fn iso(ms: i64) -> serde_json::Value {
    if ms == 0 {
        return serde_json::Value::Null;
    }
    chrono::DateTime::from_timestamp_millis(ms).map(|t| serde_json::Value::String(t.to_rfc3339())).unwrap_or(serde_json::Value::Null)
}

/// Sends every live row of the chosen drives. `progress(rows_sent, rows_total)`. Returns rows sent.
pub fn publish(
    index: &Index,
    settings: &Settings,
    key: &str,
    ids: &[u32],
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<u64, String> {
    let base = settings.supabase_url.trim().trim_end_matches('/');
    if base.is_empty() || key.is_empty() {
        return Err("AUTH REQUIRED: add the Supabase URL and key in Settings".into());
    }
    let table = if settings.supabase_table.trim().is_empty() { "drive_files" } else { settings.supabase_table.trim() };
    let url = format!("{base}/rest/v1/{table}?on_conflict=machine,drive,path");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(120)))
        .http_status_as_error(false)
        .build()
        .into();
    let machine = crate::platform::machine_name();
    let scanned = iso(index.scanned_at_ms);
    let total = ids.len() as u64;
    let mut sent = 0u64;
    for chunk in ids.chunks(BATCH) {
        if cancel.load(Ordering::Relaxed) {
            return Err(format!("cancelled after {sent} rows"));
        }
        let rows: Vec<serde_json::Value> = chunk
            .iter()
            .map(|&id| {
                let r = index.row(id);
                serde_json::json!({
                    "machine": machine,
                    "drive": r.drive,
                    "path": r.path,
                    "name": r.name,
                    "ext": r.ext,
                    "is_dir": r.is_dir,
                    "size": r.size,
                    "created_at": iso(r.created_ms),
                    "modified_at": iso(r.modified_ms),
                    "accessed_at": iso(r.accessed_ms),
                    "attributes": r.attributes,
                    "links": r.links,
                    "blake3": r.blake3,
                    "sha256": r.sha256,
                    "sample_hash": r.sample,
                    "dup_group": r.dup_group,
                    "verdict": serde_json::to_value(r.verdict).unwrap_or_default(),
                    "reason": r.reason,
                    "scanned_at": scanned,
                })
            })
            .collect();
        let mut tries = 0;
        loop {
            tries += 1;
            let res = agent
                .post(&url)
                .header("apikey", key)
                .header("Authorization", &format!("Bearer {key}"))
                .header("Prefer", "resolution=merge-duplicates,return=minimal")
                .send_json(&rows);
            match res {
                Ok(mut resp) => {
                    let code = resp.status().as_u16();
                    if (200..300).contains(&code) {
                        break;
                    }
                    let body = resp.body_mut().read_to_string().unwrap_or_default();
                    if (code == 429 || code >= 500) && tries < 4 {
                        std::thread::sleep(Duration::from_secs(2u64 << tries));
                        continue;
                    }
                    let hint = match code {
                        401 | 403 => " — the key was refused (use the service role key)",
                        404 => " — table not found: run supabase/drive_files.sql in the Supabase SQL editor",
                        _ => "",
                    };
                    return Err(format!("Supabase answered {code}{hint}: {}", body.chars().take(300).collect::<String>()));
                }
                Err(e) if tries < 4 => {
                    std::thread::sleep(Duration::from_secs(2u64 << tries));
                    let _ = e;
                }
                Err(e) => return Err(format!("network error after {sent} rows: {e}")),
            }
        }
        sent += chunk.len() as u64;
        progress(sent, total);
    }
    Ok(sent)
}
