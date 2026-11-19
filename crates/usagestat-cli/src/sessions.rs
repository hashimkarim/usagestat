//! The costliest agent sessions from local logs, read from the running daemon.
use anyhow::{Context, Result};
use chrono::{DateTime, Duration as Days, Utc};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::time::Duration;

use crate::{daemon, project};

pub struct Options {
    pub limit: usize,
    pub days: Option<i64>,
    pub project: Option<String>,
    pub bind: Option<SocketAddr>,
    pub json: bool,
}

pub fn run(options: Options) -> Result<()> {
    let json = options.json;
    let report = fetch(options)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_text(&report);
    }
    Ok(())
}

/// The ranked sessions as JSON. Never prints, so the MCP server can reuse it.
pub fn fetch(options: Options) -> Result<Value> {
    let base = daemon::dashboard_base_url(options.bind)?;
    let client = daemon::local_client(Duration::from_secs(120))?;
    daemon::check_health(&base).with_context(|| {
        format!("the usagestat daemon is not responding at {base}; start it to read local usage")
    })?;
    let mut sessions = Vec::new();
    for (id, name) in project::SOURCES {
        let Ok(body) = client
            .get(format!("{base}/v1/local-usage/{id}/session"))
            .send()
            .and_then(|r| r.json::<Value>())
        else {
            continue;
        };
        for row in body["sessions"].as_array().into_iter().flatten() {
            sessions.push((name, row.clone()));
        }
    }
    Ok(rank(
        sessions,
        Utc::now(),
        options.days,
        options.project.as_deref(),
        options.limit,
    ))
}

fn rank(
    sessions: Vec<(&str, Value)>,
    now: DateTime<Utc>,
    days: Option<i64>,
    project: Option<&str>,
    limit: usize,
) -> Value {
    let since = days.map(|d| now - Days::days(d));
    let mut rows: Vec<Value> = sessions
        .into_iter()
        .filter(|(_, row)| project.is_none_or(|p| row["project"].as_str() == Some(p)))
        .filter(|(_, row)| {
            since.is_none_or(|since| {
                row["lastActivity"]
                    .as_str()
                    .and_then(|t| t.parse::<DateTime<Utc>>().ok())
                    .is_some_and(|t| t >= since)
            })
        })
        .map(|(provider, row)| {
            let first = row["firstActivity"]
                .as_str()
                .and_then(|t| t.parse::<DateTime<Utc>>().ok());
            let last = row["lastActivity"]
                .as_str()
                .and_then(|t| t.parse::<DateTime<Utc>>().ok());
            json!({
                "provider": provider,
                "sessionId": row["sessionId"],
                "project": row["project"],
                "branch": row["branch"].as_str().unwrap_or_default(),
                "mainModel": main_model(&row),
                "firstActivity": row["firstActivity"],
                "lastActivity": row["lastActivity"],
                "durationMinutes": first.zip(last).map(|(f, l)| (l - f).num_minutes()),
                "costUsd": row["costUsd"],
                "costKnown": row["costKnown"],
                "totalTokens": row["totalTokens"],
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        let cost = |v: &Value| v["costUsd"].as_f64().unwrap_or(0.0);
        cost(b).total_cmp(&cost(a))
    });
    let total = rows.len();
    rows.truncate(limit);
    json!({
        "days": days, "project": project, "matchingSessions": total,
        "costSource": "api-rate-estimate", "sessions": rows,
    })
}

/// The model with the most cost in a session (tokens when unpriced).
fn main_model(row: &Value) -> Option<String> {
    let key = |usage: &Value| {
        (
            usage["costUsd"].as_f64().unwrap_or(0.0),
            usage["totalTokens"].as_u64().unwrap_or(0),
        )
    };
    row["models"]
        .as_object()?
        .iter()
        .max_by(|a, b| {
            let (ca, ta) = key(a.1);
            let (cb, tb) = key(b.1);
            ca.total_cmp(&cb).then(ta.cmp(&tb))
        })
        .map(|(model, _)| model.clone())
}

fn duration(minutes: Option<i64>) -> String {
    match minutes {
        None => "—".into(),
        Some(m) if m < 1 => "<1m".into(),
        Some(m) if m < 60 => format!("{m}m"),
        Some(m) if m < 48 * 60 => format!("{}h{:02}m", m / 60, m % 60),
        Some(m) => format!("{}d", m / (24 * 60)),
    }
}

fn clip(text: &str, width: usize) -> String {
    if text.chars().count() > width {
        format!("{}…", text.chars().take(width - 1).collect::<String>())
    } else {
        text.to_string()
    }
}

fn print_text(report: &Value) {
    let rows = report["sessions"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let scope = match (report["days"].as_i64(), report["project"].as_str()) {
        (Some(d), Some(p)) => format!("last {d} days in {p}"),
        (Some(d), None) => format!("last {d} days"),
        (None, Some(p)) => format!("all time in {p}"),
        (None, None) => "all time".into(),
    };
    if rows.is_empty() {
        println!("No agent sessions recorded ({scope}).");
        return;
    }
    println!(
        "Costliest sessions · {scope} · {} of {} · API-equivalent estimates\n",
        rows.len(),
        report["matchingSessions"]
    );
    println!(
        "{:>10}  {:<7} {:<20} {:<24} {:<20} {:>8}  {}",
        "Cost", "Agent", "Project", "Branch", "Main model", "Length", "Last active"
    );
    for row in rows {
        let cost = row["costUsd"].as_f64().unwrap_or(0.0);
        let plus = if row["costKnown"] == false { "+" } else { "" };
        let text = |key: &str| row[key].as_str().filter(|s| !s.is_empty()).unwrap_or("—");
        println!(
            "{:>10}  {:<7} {:<20} {:<24} {:<20} {:>8}  {}",
            format!("${cost:.2}{plus}"),
            text("provider"),
            clip(text("project"), 20),
            clip(text("branch"), 24),
            clip(text("mainModel"), 20),
            duration(row["durationMinutes"].as_i64()),
            text("lastActivity")
                .get(..16)
                .unwrap_or("—")
                .replace('T', " "),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, project: &str, last: &str, cost: f64) -> Value {
        json!({"sessionId": id, "project": project, "firstActivity": "2026-11-18T09:00:00+00:00",
            "lastActivity": last, "costUsd": cost, "totalTokens": 10,
            "models": {"small": {"costUsd": cost / 4.0}, "big": {"costUsd": cost * 0.75}}})
    }

    #[test]
    fn ranks_recent_sessions_by_cost_within_a_project() {
        let now: DateTime<Utc> = "2026-11-19T00:00:00Z".parse().unwrap();
        let sessions = vec![
            (
                "Claude",
                session("a", "app", "2026-11-18T11:30:00+00:00", 2.0),
            ),
            (
                "Codex",
                session("b", "app", "2026-11-18T10:00:00+00:00", 5.0),
            ),
            (
                "Codex",
                session("c", "site", "2026-11-18T10:00:00+00:00", 9.0),
            ),
            (
                "Claude",
                session("d", "app", "2026-10-01T10:00:00+00:00", 50.0),
            ),
        ];
        let report = rank(sessions, now, Some(7), Some("app"), 10);
        let ids: Vec<_> = report["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["sessionId"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["b", "a"]);
        assert_eq!(report["sessions"][1]["durationMinutes"], 150);
        assert_eq!(report["sessions"][1]["mainModel"], "big");
        assert_eq!(report["sessions"][0]["provider"], "Codex");
        let top = rank(
            vec![(
                "Claude",
                session("d", "app", "2026-10-01T10:00:00+00:00", 50.0),
            )],
            now,
            None,
            None,
            1,
        );
        assert_eq!(top["matchingSessions"], 1);
        assert_eq!(duration(Some(150)), "2h30m");
    }
}
