//! `usagestat top`: a live terminal view of quotas, today's cost and active
//! sessions, read from the running daemon. Plain ANSI, no extra dependencies.
use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use serde_json::Value;
use std::io::{IsTerminal, Write};
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::{daemon, project, statusline};

/// Sessions with activity this recent count as active.
const ACTIVE_MINUTES: i64 = 30;

pub struct Options {
    pub interval: u64,
    pub once: bool,
    pub bind: Option<SocketAddr>,
}

struct Frame {
    quotas: Value,
    today: Vec<(&'static str, f64)>,
    sessions: Vec<(&'static str, Value)>,
}

pub fn run(options: Options) -> Result<()> {
    let base = daemon::dashboard_base_url(options.bind)?;
    daemon::check_health(&base).with_context(|| {
        format!("the usagestat daemon is not responding at {base}; start it first")
    })?;
    let mut stdout = std::io::stdout();
    if options.once || !stdout.is_terminal() {
        print!("{}", render(&fetch(&base, options.bind)?, Utc::now(), None));
        return Ok(());
    }
    let stop = usagestat_core::signals::register()?;
    // Alternate screen and hidden cursor, restored however the loop ends.
    write!(stdout, "\x1b[?1049h\x1b[?25l")?;
    let result = (|| -> Result<()> {
        while !stop.load(Ordering::SeqCst) {
            let frame = match fetch(&base, options.bind) {
                Ok(frame) => render(&frame, Utc::now(), Some(options.interval)),
                Err(error) => format!("usagestat top · daemon unavailable: {error:#}\n"),
            };
            write!(stdout, "\x1b[H\x1b[2J{}", frame.replace('\n', "\r\n"))?;
            stdout.flush()?;
            let deadline = Instant::now() + Duration::from_secs(options.interval);
            while Instant::now() < deadline && !stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        Ok(())
    })();
    write!(stdout, "\x1b[?25h\x1b[?1049l")?;
    stdout.flush()?;
    result
}

fn fetch(base: &str, bind: Option<SocketAddr>) -> Result<Frame> {
    let client = daemon::local_client(Duration::from_secs(30))?;
    let get = |path: String| -> Option<Value> {
        client
            .get(format!("{base}{path}"))
            .send()
            .ok()?
            .json::<Value>()
            .ok()
            .filter(|v| v.get("error").is_none())
    };
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let mut costs = Vec::new();
    let mut sessions = Vec::new();
    for (id, name) in project::SOURCES {
        // Saved daily history: served without rescanning logs.
        if let Some(body) = get(format!("/v1/history/daily/{id}/1")) {
            let cost = body["daily"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|r| r["date"].as_str() == Some(today.as_str()))
                .map(|r| r["costUsd"].as_f64().unwrap_or(0.0))
                .fold(0.0, |a, b| a + b);
            if cost > 0.0 {
                costs.push((name, cost));
            }
        }
        if let Some(body) = get(format!(
            "/v1/local-usage/{id}/session?limit=15&order=recent"
        )) {
            for row in body["sessions"].as_array().into_iter().flatten() {
                sessions.push((name, row.clone()));
            }
        }
    }
    Ok(Frame {
        quotas: statusline::quotas(bind, &[])?,
        today: costs,
        sessions,
    })
}

fn until(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let m = (at - now).num_minutes().max(0);
    match m {
        m if m < 60 => format!("{m}m"),
        m if m < 48 * 60 => format!("{}h{:02}m", m / 60, m % 60),
        m => format!("{}d", m / (24 * 60)),
    }
}

fn bar(percent: f64) -> String {
    let filled = ((percent / 100.0) * 12.0).round().clamp(0.0, 12.0) as usize;
    format!("{}{}", "█".repeat(filled), "░".repeat(12 - filled))
}

fn clip(text: &str, width: usize) -> String {
    if text.chars().count() > width {
        format!("{}…", text.chars().take(width - 1).collect::<String>())
    } else {
        text.to_string()
    }
}

fn render(frame: &Frame, now: DateTime<Utc>, interval: Option<u64>) -> String {
    let mut out = format!(
        "usagestat top · {}{}\n\nQUOTAS\n",
        now.with_timezone(&Local).format("%H:%M:%S"),
        interval
            .map(|s| format!(" · every {s}s · Ctrl-C to quit"))
            .unwrap_or_default()
    );
    let parse = |v: &Value| v.as_str().and_then(|t| t.parse::<DateTime<Utc>>().ok());
    let mut any = false;
    for provider in frame.quotas.as_array().into_iter().flatten() {
        let name = provider["displayName"].as_str().unwrap_or("?");
        for (i, quota) in provider["quotas"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            any = true;
            let used = quota["usedPercent"].as_f64().unwrap_or(0.0);
            let mut line = format!(
                "  {:<12} {:<16} {:>4.0}% {}",
                if i == 0 {
                    clip(name, 12)
                } else {
                    String::new()
                },
                clip(quota["label"].as_str().unwrap_or("Quota"), 16),
                used,
                bar(used)
            );
            if let Some(reset) = parse(&quota["resetsAt"]) {
                line.push_str(&format!("  resets in {}", until(reset, now)));
            }
            if let Some(out_at) = parse(&quota["exhaustsAt"]) {
                line.push_str(&format!(" · out in {}", until(out_at, now)));
            }
            out.push_str(&line);
            out.push('\n');
        }
    }
    if !any {
        out.push_str("  No quota data.\n");
    }
    let total: f64 = frame.today.iter().map(|(_, c)| c).sum::<f64>() + 0.0;
    out.push_str("\nTODAY (UTC) · API-equivalent\n  ");
    if frame.today.is_empty() {
        out.push_str("No local agent cost yet.");
    } else {
        let parts: Vec<String> = frame
            .today
            .iter()
            .map(|(name, cost)| format!("{name} ${cost:.2}"))
            .collect();
        out.push_str(&parts.join(" · "));
        if frame.today.len() > 1 {
            out.push_str(&format!(" · total ${total:.2}"));
        }
    }
    let mut active: Vec<&(&str, Value)> = frame
        .sessions
        .iter()
        .filter(|(_, s)| {
            parse(&s["lastActivity"]).is_some_and(|t| (now - t).num_minutes() < ACTIVE_MINUTES)
        })
        .collect();
    active.sort_by(|a, b| {
        b.1["lastActivity"]
            .as_str()
            .cmp(&a.1["lastActivity"].as_str())
    });
    out.push_str(&format!(
        "\n\nACTIVE SESSIONS · last {ACTIVE_MINUTES} minutes\n"
    ));
    if active.is_empty() {
        out.push_str("  None.\n");
        return out;
    }
    out.push_str(&format!(
        "  {:>9}  {:>8}  {:<7} {:<18} {:<20} {:>7}  {}\n",
        "Cost", "$/hour", "Agent", "Project", "Branch", "Length", "Last"
    ));
    for (agent, session) in active.iter().take(12) {
        let cost = session["costUsd"].as_f64().unwrap_or(0.0);
        let (first, last) = (
            parse(&session["firstActivity"]),
            parse(&session["lastActivity"]),
        );
        let minutes = first.zip(last).map(|(f, l)| (l - f).num_minutes());
        let rate = minutes
            .filter(|m| *m >= 5)
            .map(|m| format!("${:.2}", cost / (m as f64 / 60.0)))
            .unwrap_or_else(|| "—".into());
        let length = minutes.map_or("—".into(), |m| {
            if m < 60 {
                format!("{m}m")
            } else {
                format!("{}h{:02}m", m / 60, m % 60)
            }
        });
        let ago = last.map_or("—".into(), |l| {
            format!("{}m ago", (now - l).num_minutes().max(0))
        });
        let text = |key: &str| {
            session[key]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("—")
        };
        out.push_str(&format!(
            "  {:>9}  {:>8}  {:<7} {:<18} {:<20} {:>7}  {}\n",
            format!("${cost:.2}"),
            rate,
            agent,
            clip(text("project"), 18),
            clip(text("branch"), 20),
            length,
            ago
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn frame_shows_quotas_forecasts_today_and_only_active_sessions() {
        let now: DateTime<Utc> = "2026-11-18T21:00:00Z".parse().unwrap();
        let frame = Frame {
            quotas: json!([{"displayName":"Claude","quotas":[
                {"label":"Session","usedPercent":75.0,"resetsAt":"2026-11-18T23:00:00Z","exhaustsAt":"2026-11-18T22:00:00Z"},
                {"label":"Weekly","usedPercent":40.0,"resetsAt":"2026-11-20T21:00:00Z"}]}]),
            today: vec![("Claude", 12.5), ("Codex", 2.5)],
            sessions: vec![
                (
                    "Claude",
                    json!({"project":"app","branch":"feat/x","costUsd":6.0,
                    "firstActivity":"2026-11-18T19:00:00Z","lastActivity":"2026-11-18T20:58:00Z"}),
                ),
                (
                    "Codex",
                    json!({"project":"old","costUsd":99.0,
                    "firstActivity":"2026-11-18T10:00:00Z","lastActivity":"2026-11-18T11:00:00Z"}),
                ),
            ],
        };
        let text = render(&frame, now, None);
        assert!(
            text.contains("Session            75% █████████░░░  resets in 2h00m · out in 1h00m"),
            "{text}"
        );
        assert!(text.contains("Weekly             40%"));
        assert!(text.contains("Claude $12.50 · Codex $2.50 · total $15.00"));
        // $6 over 1h58m is about $3.05 an hour; the idle Codex session is hidden.
        assert!(
            text.contains("$6.00     $3.05  Claude  app                feat/x"),
            "{text}"
        );
        assert!(!text.contains("old"));
    }
}
