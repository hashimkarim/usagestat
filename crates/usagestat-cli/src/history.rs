use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use usagestat_core::{NormalizedMetrics, UsageSnapshot, paths};

/// Flat export record: one row per probe snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotRecord {
    pub ts: String,
    pub provider_id: String,
    pub display_name: String,
    pub plan: Option<String>,
    pub primary_percent: f64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost: Option<f64>,
    pub reset_time: Option<String>,
}

pub fn history_jsonl_path() -> Result<PathBuf> {
    Ok(paths::data_dir()?.join("history.jsonl"))
}

pub fn record_from_snapshot(snapshot: &UsageSnapshot) -> SnapshotRecord {
    let m = NormalizedMetrics::from_snapshot(snapshot);
    SnapshotRecord {
        ts: snapshot.fetched_at.to_rfc3339(),
        provider_id: snapshot.provider_id.clone(),
        display_name: snapshot.display_name.clone(),
        plan: snapshot.plan.clone(),
        primary_percent: m.primary_percent,
        input_tokens: m.input_tokens,
        output_tokens: m.output_tokens,
        cost: m.cost,
        reset_time: m.reset_time,
    }
}

pub fn append_jsonl(rec: &SnapshotRecord) -> Result<()> {
    let path = history_jsonl_path()?;
    let mut line = serde_json::to_vec(rec).context("serialize history record")?;
    line.push(10);
    usagestat_core::storage::append_private(&path, &line)
        .with_context(|| format!("append history {}", path.display()))
}

/// Read all records from a JSONL file, skipping malformed lines.
pub fn read_jsonl(path: &std::path::Path) -> Result<Vec<SnapshotRecord>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {:?}", path))?;
    Ok(text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<SnapshotRecord>(l).ok())
        .collect())
}

pub fn print_csv(records: &[SnapshotRecord]) -> Result<()> {
    use std::io;
    let mut w = io::stdout().lock();
    writeln!(
        w,
        "ts,provider_id,display_name,plan,primary_percent,input_tokens,output_tokens,cost,reset_time"
    )?;
    for r in records {
        writeln!(
            w,
            "{},{},{},{},{:.2},{},{},{},{}",
            csv_cell(&r.ts),
            csv_cell(&r.provider_id),
            csv_cell(&r.display_name),
            csv_opt(&r.plan),
            r.primary_percent,
            r.input_tokens.map(|n| n.to_string()).unwrap_or_default(),
            r.output_tokens.map(|n| n.to_string()).unwrap_or_default(),
            r.cost.map(|c| format!("{c:.6}")).unwrap_or_default(),
            csv_opt(&r.reset_time),
        )?;
    }
    Ok(())
}

fn csv_cell(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn csv_opt(s: &Option<String>) -> String {
    s.as_deref().map(csv_cell).unwrap_or_default()
}
