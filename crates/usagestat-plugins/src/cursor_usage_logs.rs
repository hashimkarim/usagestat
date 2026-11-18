use chrono::{DateTime, Duration, Local, NaiveDate, Utc};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const CHARS_PER_TOKEN: u64 = 4;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorDailyUsage {
    pub date: String,
    pub total_tokens: u64,
    pub estimated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorLogsStatus {
    Ok,
    NoData,
}

pub fn query_daily_since(since: &str) -> (CursorLogsStatus, Vec<CursorDailyUsage>) {
    let since_date =
        parse_since_day(since).unwrap_or_else(|| Utc::now().date_naive() - Duration::days(30));
    let Some(home) = cursor_agent_home() else {
        return (CursorLogsStatus::NoData, Vec::new());
    };
    let projects = home.join("projects");
    let mut files = Vec::new();
    collect_transcript_jsonl(&projects, &mut files);

    let mut by_day: BTreeMap<String, u64> = BTreeMap::new();
    for path in files {
        let Some(day) = day_key_from_mtime(&path) else {
            continue;
        };
        if day < since_date {
            continue;
        }
        let tokens = estimate_tokens_in_jsonl(&path);
        if tokens == 0 {
            continue;
        }
        *by_day
            .entry(day.format("%Y-%m-%d").to_string())
            .or_insert(0) += tokens;
    }

    if by_day.is_empty() {
        return (CursorLogsStatus::NoData, Vec::new());
    }

    let daily = by_day
        .into_iter()
        .map(|(date, total_tokens)| CursorDailyUsage {
            date,
            total_tokens,
            estimated: true,
        })
        .collect();
    (CursorLogsStatus::Ok, daily)
}

fn cursor_agent_home() -> Option<PathBuf> {
    if let Some(value) = std::env::var("CURSOR_AGENT_HOME")
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        return Some(expand_tilde(&value));
    }
    usagestat_core::paths::home_dir().map(|home| home.join(".cursor"))
}

fn expand_tilde(path: &str) -> PathBuf {
    usagestat_core::paths::expand_home(path.trim())
}

fn parse_since_day(since: &str) -> Option<NaiveDate> {
    let digits: String = since.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() >= 8 {
        return NaiveDate::parse_from_str(&digits[0..8], "%Y%m%d").ok();
    }
    NaiveDate::parse_from_str(since.trim(), "%Y-%m-%d").ok()
}

fn day_key_from_mtime(path: &Path) -> Option<NaiveDate> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let dt: DateTime<Local> = DateTime::from(modified);
    Some(dt.date_naive())
}

fn estimate_tokens_in_jsonl(path: &Path) -> u64 {
    let Ok(content) = std::fs::read_to_string(path) else {
        return 0;
    };
    let mut chars = 0u64;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        chars += extract_message_chars(&value);
    }
    chars / CHARS_PER_TOKEN
}

fn extract_message_chars(value: &serde_json::Value) -> u64 {
    let mut chars = 0u64;
    if let Some(content) = value
        .pointer("/message/content")
        .and_then(|content| content.as_array())
    {
        for item in content {
            if let Some(text) = item.get("text").and_then(|text| text.as_str()) {
                chars += text.chars().count() as u64;
            }
        }
    }
    chars
}

fn collect_transcript_jsonl(projects_dir: &Path, out: &mut Vec<PathBuf>) {
    if !projects_dir.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(projects_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let transcripts = entry.path().join("agent-transcripts");
        if transcripts.is_dir() {
            walk_transcripts_dir(&transcripts, out);
        }
    }
}

fn walk_transcripts_dir(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_transcripts_dir(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "jsonl") {
            out.push(path);
        }
    }
}
