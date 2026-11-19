//! Optional multi-machine totals: publish this machine's daily cost per project
//! to a shared folder and read the other machines' files. Nothing runs unless
//! `[sync]` names a directory.
//!
//! Files carry project labels and daily totals only: no paths, prompts,
//! session ids or account details.
use chrono::{Duration as Days, NaiveDate, Utc};
use serde_json::{Value as JsonValue, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};
use usagestat_core::sync::{SCHEMA, SyncConfig};

use super::local_usage_cache;

const SOURCES: [&str; 4] = ["claude", "codex", "gemini", "amp"];
/// Publish at most this often; usage changes slowly and sync tools dislike churn.
const PUBLISH_EVERY: Duration = Duration::from_secs(10 * 60);
const KEEP_DAYS: i64 = 400;
const MAX_FILE: u64 = 20 * 1024 * 1024;
const MAX_MACHINES: usize = 64;

#[derive(Default)]
pub struct Publisher {
    last: Option<(Instant, String, u64)>,
    warned: bool,
}

/// A random id for this installation, created once in the data directory, so
/// two machines that share a name never overwrite each other's file.
fn install_id() -> Option<String> {
    let path = usagestat_core::paths::data_dir()
        .ok()?
        .join("sync-install-id");
    if let Ok(id) = std::fs::read_to_string(&path) {
        let id = id.trim().to_string();
        if !id.is_empty() {
            return Some(id);
        }
    }
    let id = format!("{:032x}", rand_u128());
    usagestat_core::storage::create_once(&path, id.as_bytes()).ok()?;
    std::fs::read_to_string(&path)
        .ok()
        .map(|s| s.trim().to_string())
}

fn rand_u128() -> u128 {
    use std::hash::{BuildHasher, Hasher};
    let mut out = 0u128;
    for _ in 0..2 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        h.write_u32(std::process::id());
        out = (out << 64) | h.finish() as u128;
    }
    out
}

/// Whether `path` holds another installation's file under the same name.
fn owned_by_another(path: &Path, id: &str) -> bool {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<JsonValue>(&bytes).ok())
        .and_then(|doc| doc["installId"].as_str().map(str::to_owned))
        .is_some_and(|other| other != id)
}

impl Publisher {
    /// Called after each poll; writes `<dir>/<machine>.json` when due and changed.
    pub fn tick(&mut self, config: Option<&SyncConfig>) {
        let Some((config, dir)) = config.and_then(|c| c.directory().map(|d| (c, d))) else {
            self.last = None;
            return;
        };
        let target = format!("{}|{}", dir.display(), config.machine_name());
        if self
            .last
            .as_ref()
            .is_some_and(|(at, t, _)| *t == target && at.elapsed() < PUBLISH_EVERY)
        {
            return;
        }
        let rows = local_rows();
        let hash = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            rows.to_string().hash(&mut h);
            h.finish()
        };
        let unchanged = self
            .last
            .as_ref()
            .is_some_and(|(_, t, previous)| *t == target && *previous == hash);
        if !unchanged {
            let machine = config.machine_name();
            let Some(id) = install_id() else {
                log::warn!("sync is paused: could not create this installation's id");
                return;
            };
            let path = dir.join(format!("{machine}.json"));
            if owned_by_another(&path, &id) {
                if !self.warned {
                    log::warn!(
                        "sync is paused: another machine already publishes as `{machine}`; set [sync] machine to a unique name"
                    );
                    self.warned = true;
                }
                return;
            }
            self.warned = false;
            let document = json!({
                "schema": SCHEMA, "machine": machine, "installId": id,
                "updatedAt": Utc::now().to_rfc3339(),
                "timeZone": "UTC", "costSource": "api-rate-estimate", "rows": rows,
            });
            if let Err(error) =
                usagestat_core::storage::write_atomic(&path, document.to_string().as_bytes())
            {
                log::warn!("could not publish sync file: {error}");
                return;
            }
        }
        self.last = Some((Instant::now(), target, hash));
    }
}

/// This machine's daily cost per source and project, newest `KEEP_DAYS` only.
fn local_rows() -> JsonValue {
    let since = (Utc::now() - Days::days(KEEP_DAYS))
        .format("%Y-%m-%d")
        .to_string();
    let mut rows = Vec::new();
    for source in SOURCES {
        let Ok(body) = local_usage_cache::report(source, "projects") else {
            continue;
        };
        let Ok(report) = serde_json::from_str::<JsonValue>(&body) else {
            continue;
        };
        for row in report["projects"].as_array().into_iter().flatten() {
            if row["date"].as_str().is_none_or(|d| d < since.as_str()) {
                continue;
            }
            rows.push(json!({
                "source": source, "date": row["date"], "project": row["project"],
                "costUsd": row["costUsd"], "costKnown": row["costKnown"],
                "totalTokens": row["totalTokens"],
            }));
        }
    }
    JsonValue::Array(rows)
}

/// `/v1/machines`: per-machine totals from the shared folder.
pub fn serve(config: Option<&SyncConfig>) -> JsonValue {
    let Some((config, dir)) = config.and_then(|c| c.directory().map(|d| (c, d))) else {
        return json!({"enabled": false});
    };
    let this = config.machine_name();
    let id = install_id();
    let today = Utc::now().date_naive();
    let mut machines: Vec<JsonValue> = read_documents(&dir)
        .iter()
        .map(|doc| {
            let mut summary = summarize(doc, today);
            // Match by installation id; a name alone may be shared.
            summary["thisMachine"] = json!(match (doc["installId"].as_str(), id.as_deref()) {
                (Some(theirs), Some(ours)) => theirs == ours,
                _ => doc["machine"].as_str() == Some(this.as_str()),
            });
            summary
        })
        .collect();
    machines.sort_by(|a, b| {
        let cost = |v: &JsonValue| v["last30Usd"].as_f64().unwrap_or(0.0);
        cost(b).total_cmp(&cost(a))
    });
    json!({"enabled": true, "machine": this, "timeZone": "UTC",
        "costSource": "api-rate-estimate", "machines": machines})
}

fn read_documents(dir: &Path) -> Vec<JsonValue> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut documents: BTreeMap<String, JsonValue> = BTreeMap::new();
    for entry in entries.flatten().take(1024) {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || !name.ends_with(".json") {
            continue;
        }
        if entry
            .metadata()
            .map_or(true, |m| !m.is_file() || m.len() > MAX_FILE)
        {
            continue;
        }
        let Some(doc) = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<JsonValue>(&bytes).ok())
            .filter(|doc| doc["schema"] == SCHEMA && doc["machine"].is_string())
        else {
            continue;
        };
        let machine = doc["machine"].as_str().unwrap_or_default().to_string();
        // Two files claiming one machine: keep the newest.
        let newer = documents
            .get(&machine)
            .is_none_or(|old| doc["updatedAt"].as_str() > old["updatedAt"].as_str());
        if newer && (documents.contains_key(&machine) || documents.len() < MAX_MACHINES) {
            documents.insert(machine, doc);
        }
    }
    documents.into_values().collect()
}

fn summarize(doc: &JsonValue, today: NaiveDate) -> JsonValue {
    let day = |offset: i64| (today - Days::days(offset)).format("%Y-%m-%d").to_string();
    let (today_key, week, month_ago) = (day(0), day(6), day(29));
    let month = today.format("%Y-%m").to_string();
    let mut totals = [0.0f64; 4];
    let mut projects: BTreeMap<String, f64> = BTreeMap::new();
    let mut known = true;
    for row in doc["rows"].as_array().into_iter().flatten() {
        let Some(date) = row["date"].as_str() else {
            continue;
        };
        if date > today_key.as_str() {
            continue;
        }
        let cost = row["costUsd"].as_f64().unwrap_or(0.0);
        let windows = [
            date == today_key,
            date >= week.as_str(),
            date >= month_ago.as_str(),
            date.starts_with(&month),
        ];
        for (total, inside) in totals.iter_mut().zip(windows) {
            if inside {
                *total += cost;
            }
        }
        if date >= month_ago.as_str() {
            known &= row["costKnown"] != false;
            *projects
                .entry(row["project"].as_str().unwrap_or("unknown").to_string())
                .or_default() += cost;
        }
    }
    let mut top: Vec<_> = projects.into_iter().collect();
    top.sort_by(|a, b| b.1.total_cmp(&a.1));
    top.truncate(5);
    json!({
        "machine": doc["machine"], "updatedAt": doc["updatedAt"],
        "todayUsd": totals[0], "last7Usd": totals[1], "last30Usd": totals[2], "monthUsd": totals[3],
        "costKnown": known,
        "topProjects30d": top.iter().map(|(p, c)| json!({"project": p, "costUsd": c})).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(machine: &str, updated: &str, rows: JsonValue) -> JsonValue {
        json!({"schema": SCHEMA, "machine": machine, "updatedAt": updated, "rows": rows})
    }

    #[test]
    fn a_file_from_another_installation_with_the_same_name_is_left_alone() {
        let dir = usagestat_core::storage::temporary_directory().unwrap();
        let path = dir.path().join("fedora.json");
        assert!(!owned_by_another(&path, "mine"));
        std::fs::write(&path, doc("fedora", "x", json!([])).to_string()).unwrap();
        assert!(
            !owned_by_another(&path, "mine"),
            "files without an id are adoptable"
        );
        let mut theirs = doc("fedora", "x", json!([]));
        theirs["installId"] = json!("theirs");
        std::fs::write(&path, theirs.to_string()).unwrap();
        assert!(owned_by_another(&path, "mine"));
        assert!(!owned_by_another(&path, "theirs"));
        assert_ne!(rand_u128(), rand_u128());
    }

    #[test]
    fn machines_are_read_from_the_folder_and_summarised_by_window() {
        let dir = usagestat_core::storage::temporary_directory().unwrap();
        let today = Utc::now().date_naive();
        let day = |offset: i64| (today - Days::days(offset)).format("%Y-%m-%d").to_string();
        let rows = json!([
            {"date": day(0), "project": "app", "costUsd": 2.0, "costKnown": true},
            {"date": day(3), "project": "site", "costUsd": 5.0, "costKnown": true},
            {"date": day(20), "project": "app", "costUsd": 10.0, "costKnown": false},
            {"date": day(90), "project": "old", "costUsd": 99.0, "costKnown": true},
        ]);
        let write = |name: &str, value: &JsonValue| {
            std::fs::write(dir.path().join(name), value.to_string()).unwrap()
        };
        write(
            "laptop.json",
            &doc("laptop", "2026-11-18T10:00:00Z", rows.clone()),
        );
        write(
            "laptop-copy.json",
            &doc("laptop", "2026-11-17T10:00:00Z", json!([])),
        );
        write("desk.json", &doc("desk", "2026-11-18T09:00:00Z", json!([])));
        write(
            ".usagestat-state-tmp",
            &doc("partial", "2026-11-18T09:00:00Z", json!([])),
        );
        write("notes.json", &json!({"hello": "world"}));
        let config = SyncConfig {
            dir: Some(dir.path().to_string_lossy().into_owned()),
            machine: Some("desk".into()),
        };
        let body = serve(Some(&config));
        let machines = body["machines"].as_array().unwrap();
        assert_eq!(machines.len(), 2);
        let laptop = &machines[0];
        assert_eq!(laptop["machine"], "laptop");
        assert_eq!(laptop["todayUsd"], 2.0);
        assert_eq!(laptop["last7Usd"], 7.0);
        assert_eq!(laptop["last30Usd"], 17.0);
        assert_eq!(laptop["costKnown"], false);
        assert_eq!(laptop["topProjects30d"][0]["project"], "app");
        assert_eq!(laptop["thisMachine"], false);
        assert_eq!(machines[1]["thisMachine"], true);
        assert_eq!(serve(None), json!({"enabled": false}));
    }
}
