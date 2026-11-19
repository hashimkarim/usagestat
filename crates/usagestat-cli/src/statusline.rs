//! One-line quota and cost summary for coding-agent statuslines.
//!
//! Reads the running daemon only, so each call is a single local request and
//! never probes providers or touches credentials.
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::io::{IsTerminal, Read};
use std::net::SocketAddr;
use std::sync::mpsc;
use std::time::Duration;

use crate::daemon;

pub struct Options {
    pub providers: Vec<String>,
    pub bind: Option<SocketAddr>,
    pub color: bool,
    pub json: bool,
}

pub fn run(mut options: Options) -> Result<()> {
    if options.providers.is_empty()
        && let Some(id) = detect_provider(&read_hook_input())
    {
        options.providers.push(id.to_string());
    }
    let snapshots = daemon::dashboard_base_url(options.bind).and_then(|base| {
        let body: Value = daemon::local_client(Duration::from_millis(800))?
            .get(format!("{base}/v1/usage"))
            .send()?
            .error_for_status()?
            .json()?;
        Ok(body)
    });
    let Ok(Value::Array(snapshots)) = snapshots else {
        // A statusline must not fail the agent's prompt; say why in place.
        println!("usagestat: daemon offline");
        return Ok(());
    };
    let now = Utc::now();
    let selected: Vec<&Value> = snapshots
        .iter()
        .filter(|s| {
            options.providers.is_empty()
                || options
                    .providers
                    .iter()
                    .any(|id| s["providerId"].as_str() == Some(id.as_str()))
        })
        .filter(|s| options.providers.len() == 1 || !progress(s).is_empty())
        .collect();
    if options.json {
        let rows: Vec<Value> = selected.iter().map(|s| summary_json(s, now)).collect();
        println!("{}", serde_json::to_string(&rows)?);
        return Ok(());
    }
    let line = selected
        .iter()
        .map(|s| summary_text(s, now, options.color))
        .collect::<Vec<_>>()
        .join(" | ");
    println!(
        "{}",
        if line.is_empty() {
            "usagestat: no quota data".into()
        } else {
            line
        }
    );
    Ok(())
}

/// Claude Code pipes a JSON description of the session on stdin. Read it
/// briefly in the background so an open, silent stdin cannot stall the prompt.
fn read_hook_input() -> Value {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Value::Null;
    }
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::stdin().take(256 * 1024).read_to_string(&mut text);
        let _ = tx.send(text);
    });
    rx.recv_timeout(Duration::from_millis(200))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null)
}

pub fn detect_provider(input: &Value) -> Option<&'static str> {
    let model = input
        .pointer("/model/id")
        .or_else(|| input.get("model"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if model.contains("claude") || input.get("transcript_path").is_some() {
        Some("claude")
    } else if model.starts_with("gpt") || model.contains("codex") {
        Some("codex")
    } else {
        None
    }
}

fn progress(snapshot: &Value) -> Vec<&Value> {
    snapshot["metrics"]
        .as_array()
        .map(|m| {
            m.iter()
                .filter(|m| m["type"] == "progress" && m["limit"].as_f64().is_some_and(|l| l > 0.0))
                .collect()
        })
        .unwrap_or_default()
}

fn percent(metric: &Value) -> f64 {
    metric["used"].as_f64().unwrap_or(0.0) / metric["limit"].as_f64().unwrap_or(1.0) * 100.0
}

fn reset_in(metric: &Value, now: DateTime<Utc>) -> Option<String> {
    until(metric["resetsAt"].as_str()?.parse().ok()?, now)
}

/// When the quota runs out before its reset at the window's average rate.
fn exhausts_at(metric: &Value, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    usagestat_core::model::quota_exhaustion(
        percent(metric),
        metric["resetsAt"].as_str()?.parse().ok()?,
        metric["periodDurationMs"].as_u64()?,
        now,
    )
}

fn until(at: DateTime<Utc>, now: DateTime<Utc>) -> Option<String> {
    let minutes = (at - now).num_minutes();
    (minutes > 0).then(|| match minutes {
        m if m < 60 => format!("{m}m"),
        m if m < 48 * 60 => format!("{}h{:02}m", m / 60, m % 60),
        m => format!("{}d", m / (24 * 60)),
    })
}

fn today_cost(snapshot: &Value) -> Option<&str> {
    snapshot["metrics"]
        .as_array()?
        .iter()
        .find(|m| m["type"] == "text" && m["label"] == "Today")?["value"]
        .as_str()?
        .split(" · ")
        .next()
        .filter(|cost| cost.starts_with('$'))
}

fn stale(snapshot: &Value, now: DateTime<Utc>) -> bool {
    snapshot["fetchedAt"]
        .as_str()
        .and_then(|t| t.parse::<DateTime<Utc>>().ok())
        .is_none_or(|at| now - at > chrono::Duration::minutes(15))
}

fn summary_text(snapshot: &Value, now: DateTime<Utc>, color: bool) -> String {
    let name = snapshot["displayName"]
        .as_str()
        .or(snapshot["providerId"].as_str())
        .unwrap_or("?");
    let mut parts = vec![name.to_string()];
    if snapshot["state"].as_str().is_some_and(|s| s != "ready") {
        parts.push(snapshot["state"].as_str().unwrap_or_default().to_string());
    }
    for metric in progress(snapshot) {
        let value = percent(metric);
        let mut text = format!(
            "{} {}",
            metric["label"].as_str().unwrap_or("Quota"),
            paint(value, color)
        );
        let out = exhausts_at(metric, now).and_then(|at| until(at, now));
        match (reset_in(metric, now), out) {
            (Some(reset), Some(out)) => text.push_str(&format!(" ({reset}, out in {out})")),
            (Some(reset), None) => text.push_str(&format!(" ({reset})")),
            _ => {}
        }
        parts.push(text);
    }
    if let Some(cost) = today_cost(snapshot) {
        parts.push(format!("today {cost}"));
    }
    if stale(snapshot, now) {
        parts.push("stale".into());
    }
    parts.join(" · ")
}

fn paint(value: f64, color: bool) -> String {
    let text = format!("{value:.0}%");
    let code = match value {
        v if v >= 90.0 => "31",
        v if v >= 75.0 => "33",
        _ => return text,
    };
    if color {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text
    }
}

fn summary_json(snapshot: &Value, now: DateTime<Utc>) -> Value {
    serde_json::json!({
        "providerId": snapshot["providerId"],
        "displayName": snapshot["displayName"],
        "state": snapshot["state"],
        "stale": stale(snapshot, now),
        "todayCost": today_cost(snapshot),
        "quotas": progress(snapshot).iter().map(|m| serde_json::json!({
            "label": m["label"],
            "usedPercent": (percent(m) * 10.0).round() / 10.0,
            "resetsAt": m["resetsAt"],
            "exhaustsAt": exhausts_at(m, now),
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snapshot() -> Value {
        json!({"providerId":"claude","displayName":"Claude","state":"ready","fetchedAt":"2026-11-18T20:00:00Z",
            "metrics":[
                {"type":"progress","label":"Session","used":23.0,"limit":100.0,"resetsAt":"2026-11-18T23:00:00Z"},
                {"type":"progress","label":"Weekly","used":91.0,"limit":100.0,"resetsAt":"2026-11-21T20:00:00Z"},
                {"type":"text","label":"Today","value":"$312.70 · 625M tokens"}]})
    }

    #[test]
    fn summarises_quotas_resets_and_today_cost() {
        let now: DateTime<Utc> = "2026-11-18T20:05:00Z".parse().unwrap();
        assert_eq!(
            summary_text(&snapshot(), now, false),
            "Claude · Session 23% (2h55m) · Weekly 91% (2d) · today $312.70"
        );
        assert!(summary_text(&snapshot(), now, true).contains("\x1b[31m91%\x1b[0m"));
        let later: DateTime<Utc> = "2026-11-18T21:00:00Z".parse().unwrap();
        assert!(summary_text(&snapshot(), later, false).ends_with(" · stale"));
        let row = summary_json(&snapshot(), now);
        assert_eq!(row["quotas"][1]["usedPercent"], 91.0);
        assert_eq!(row["todayCost"], "$312.70");
    }

    #[test]
    fn warns_when_a_window_runs_out_before_reset() {
        let mut snap = snapshot();
        // 3h into a 5h session at 75%: out in about an hour, before the 2h reset.
        snap["metrics"][0]["used"] = json!(75.0);
        snap["metrics"][0]["periodDurationMs"] = json!(5 * 3_600_000u64);
        let now: DateTime<Utc> = "2026-11-18T21:00:00Z".parse().unwrap();
        snap["fetchedAt"] = json!("2026-11-18T21:00:00Z");
        assert!(
            summary_text(&snap, now, false).contains("Session 75% (2h00m, out in 1h00m)"),
            "{}",
            summary_text(&snap, now, false)
        );
        assert_eq!(
            summary_json(&snap, now)["quotas"][0]["exhaustsAt"],
            "2026-11-18T22:00:00Z"
        );
    }

    #[test]
    fn detects_the_agent_from_statusline_input() {
        assert_eq!(
            detect_provider(&json!({"model":{"id":"claude-opus-5-5"}})),
            Some("claude")
        );
        assert_eq!(
            detect_provider(&json!({"transcript_path":"/x.jsonl"})),
            Some("claude")
        );
        assert_eq!(
            detect_provider(&json!({"model":"gpt-6.1-sol"})),
            Some("codex")
        );
        assert_eq!(detect_provider(&Value::Null), None);
    }
}
