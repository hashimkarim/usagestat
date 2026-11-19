//! Local cost and token usage for one repository, from the running daemon.
//!
//! The repository is named the same way the daemon labels local log events,
//! so a worktree's usage counts toward the repository it belongs to.
use anyhow::{Context, Result};
use chrono::{Duration as Days, NaiveDate, Utc};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::daemon;

/// Providers whose local logs carry a working directory.
pub(crate) const SOURCES: [(&str, &str); 4] = [
    ("claude", "Claude"),
    ("codex", "Codex"),
    ("gemini", "Gemini"),
    ("amp", "Amp"),
];
const WINDOWS: [(&str, Option<i64>); 4] = [
    ("Today", Some(1)),
    ("7 days", Some(7)),
    ("30 days", Some(30)),
    ("All time", None),
];

pub struct Options {
    pub path: Option<PathBuf>,
    pub name: Option<String>,
    pub bind: Option<SocketAddr>,
    pub branches: bool,
    pub json: bool,
}

pub fn run(options: Options) -> Result<()> {
    let report = fetch(options.path.as_deref(), options.name, options.bind)?;
    if options.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    let project = report["project"].as_str().unwrap_or_default();
    print_text(project, &report["providers"]);
    if options.branches {
        print_branches(&report["branches"]);
    }
    Ok(())
}

/// The project's usage as JSON. Never prints, so the MCP server can reuse it.
pub fn fetch(path: Option<&Path>, name: Option<String>, bind: Option<SocketAddr>) -> Result<Value> {
    let dir = match path {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir()?,
    };
    let project = name.unwrap_or_else(|| project_for(&dir));
    let base = daemon::dashboard_base_url(bind)?;
    let client = daemon::local_client(Duration::from_secs(120))?;
    daemon::check_health(&base).with_context(|| {
        format!("the usagestat daemon is not responding at {base}; start it to read local usage")
    })?;
    let rows = |report: &str| {
        let mut rows: Vec<(String, Value)> = Vec::new();
        for (id, name) in SOURCES {
            let Ok(body) = client
                .get(format!("{base}/v1/local-usage/{id}/{report}"))
                .send()
                .and_then(|r| r.json::<Value>())
            else {
                continue;
            };
            for row in body[report].as_array().into_iter().flatten() {
                if row["project"].as_str() == Some(project.as_str()) {
                    rows.push((name.to_string(), row.clone()));
                }
            }
        }
        rows
    };
    let today = Utc::now().date_naive();
    Ok(json!({
        "project": project, "directory": dir, "timeZone": "UTC",
        "costSource": "api-rate-estimate",
        "providers": summarize(&rows("projects"), today),
        "branches": branches(&rows("branches"), today),
    }))
}

/// The repository name for a directory, matching how the daemon labels log
/// events: a subfolder or linked worktree counts toward the main checkout.
pub fn project_for(dir: &Path) -> String {
    usagestat_core::project::resolve(&dir.to_string_lossy())
}

#[derive(Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Window {
    cost_usd: f64,
    cost_known: bool,
    total_tokens: u64,
    active_days: usize,
}

fn summarize(rows: &[(String, Value)], today: NaiveDate) -> Value {
    let mut providers: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for (name, row) in rows {
        providers.entry(name.clone()).or_default().push(row);
    }
    let window = |rows: &[&Value], days: Option<i64>| {
        let start = days.map(|d| today - Days::days(d - 1));
        let mut w = Window {
            cost_known: true,
            ..Default::default()
        };
        for row in rows {
            let Some(date) = row["date"]
                .as_str()
                .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
            else {
                continue;
            };
            if start.is_some_and(|s| date < s) || date > today {
                continue;
            }
            w.cost_usd += row["costUsd"].as_f64().unwrap_or(0.0);
            w.cost_known &= row["costKnown"].as_bool() != Some(false);
            w.total_tokens += row["totalTokens"].as_u64().unwrap_or(0);
            w.active_days += 1;
        }
        w
    };
    let mut out = serde_json::Map::new();
    let all: Vec<&Value> = rows.iter().map(|(_, r)| r).collect();
    for (name, rows) in providers
        .iter()
        .map(|(n, r)| (n.as_str(), r.as_slice()))
        .chain([("Total", all.as_slice())])
    {
        if name == "Total" && providers.len() < 2 {
            continue;
        }
        let mut models: BTreeMap<String, f64> = BTreeMap::new();
        for row in rows.iter().filter(|r| {
            r["date"]
                .as_str()
                .is_some_and(|d| d >= (today - Days::days(29)).to_string().as_str())
        }) {
            for (model, usage) in row["models"].as_object().into_iter().flatten() {
                *models.entry(model.clone()).or_default() +=
                    usage["costUsd"].as_f64().unwrap_or(0.0);
            }
        }
        let mut top: Vec<_> = models.into_iter().collect();
        top.sort_by(|a, b| b.1.total_cmp(&a.1));
        top.truncate(3);
        let windows: serde_json::Map<String, Value> = WINDOWS
            .iter()
            .map(|(label, days)| (label.to_string(), json!(window(rows, *days))))
            .collect();
        let last = rows.iter().filter_map(|r| r["date"].as_str()).max();
        out.insert(
            name.to_string(),
            json!({"windows": windows, "lastActive": last,
                "topModels30d": top.iter().map(|(m, c)| json!({"model": m, "costUsd": c})).collect::<Vec<_>>()}),
        );
    }
    Value::Object(out)
}

/// One row per branch and provider, costliest over 30 days first.
fn branches(rows: &[(String, Value)], today: NaiveDate) -> Value {
    let mut by: BTreeMap<(String, String), Vec<&Value>> = BTreeMap::new();
    for (provider, row) in rows {
        let branch = row["branch"].as_str().unwrap_or_default().to_string();
        by.entry((branch, provider.clone())).or_default().push(row);
    }
    let sum = |rows: &[&Value], days: Option<i64>, key: &str| -> f64 {
        let start = days.map(|d| (today - Days::days(d - 1)).to_string());
        rows.iter()
            .filter(|r| {
                start
                    .as_deref()
                    .is_none_or(|s| r["date"].as_str().is_some_and(|d| d >= s))
            })
            .map(|r| r[key].as_f64().unwrap_or(0.0))
            // An empty f64 sum is -0.0, which would print as "$-0.00".
            .fold(0.0, |a, b| a + b)
    };
    let mut out: Vec<Value> = by
        .iter()
        .map(|((branch, provider), rows)| {
            json!({
                "branch": branch, "provider": provider,
                "costUsd7d": sum(rows, Some(7), "costUsd"),
                "costUsd30d": sum(rows, Some(30), "costUsd"),
                "costUsd": sum(rows, None, "costUsd"),
                "totalTokens": sum(rows, None, "totalTokens") as u64,
                "lastActive": rows.iter().filter_map(|r| r["date"].as_str()).max(),
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b["costUsd30d"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&a["costUsd30d"].as_f64().unwrap_or(0.0))
            .then(
                b["costUsd"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .total_cmp(&a["costUsd"].as_f64().unwrap_or(0.0)),
            )
    });
    Value::Array(out)
}

fn print_branches(rows: &Value) {
    let rows = rows.as_array().map(Vec::as_slice).unwrap_or_default();
    println!();
    if rows.is_empty() {
        println!(
            "No git branch recorded. Claude Code and Codex logs record the branch; Gemini CLI and Amp do not."
        );
        return;
    }
    println!(
        "{:<36}{:<8}{:>12}{:>12}{:>12}  {}",
        "Branch", "Agent", "7 days", "30 days", "All time", "Last active"
    );
    for row in rows {
        let branch = row["branch"]
            .as_str()
            .filter(|b| !b.is_empty())
            .unwrap_or("(none)");
        let branch = if branch.chars().count() > 34 {
            format!("{}…", branch.chars().take(33).collect::<String>())
        } else {
            branch.to_string()
        };
        let cost = |key: &str| format!("${:.2}", row[key].as_f64().unwrap_or(0.0));
        println!(
            "{branch:<36}{:<8}{:>12}{:>12}{:>12}  {}",
            row["provider"].as_str().unwrap_or("?"),
            cost("costUsd7d"),
            cost("costUsd30d"),
            cost("costUsd"),
            row["lastActive"].as_str().unwrap_or("—"),
        );
    }
}

fn money(window: &Value) -> String {
    let cost = window["costUsd"].as_f64().unwrap_or(0.0);
    let suffix = if window["costKnown"] == false {
        "+"
    } else {
        ""
    };
    if window["activeDays"] == 0 {
        "—".into()
    } else {
        format!("${cost:.2}{suffix}")
    }
}

pub(crate) fn tokens(n: u64) -> String {
    match n {
        n if n >= 1_000_000_000 => format!("{:.2}B", n as f64 / 1e9),
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n if n >= 1_000 => format!("{:.1}K", n as f64 / 1e3),
        n => n.to_string(),
    }
}

fn print_text(project: &str, summary: &Value) {
    let Some(providers) = summary.as_object().filter(|p| !p.is_empty()) else {
        println!("No local agent usage recorded for project `{project}`.");
        println!(
            "Names come from the working directory in Claude Code, Codex, Gemini CLI and Amp logs; use --name to choose another."
        );
        return;
    };
    println!("Project {project} · API-equivalent estimates from local logs · UTC days\n");
    print!("{:<10}", "");
    for (label, _) in WINDOWS {
        print!("{label:>22}");
    }
    println!();
    for (name, data) in providers {
        print!("{name:<10}");
        for (label, _) in WINDOWS {
            let w = &data["windows"][label];
            let cell = format!(
                "{} · {}",
                money(w),
                tokens(w["totalTokens"].as_u64().unwrap_or(0))
            );
            print!("{cell:>22}");
        }
        println!();
    }
    println!();
    for (name, data) in providers {
        if name == "Total" {
            continue;
        }
        let models: Vec<String> = data["topModels30d"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|m| {
                format!(
                    "{} ${:.2}",
                    m["model"].as_str().unwrap_or("?"),
                    m["costUsd"].as_f64().unwrap_or(0.0)
                )
            })
            .collect();
        println!(
            "{name}: last active {}{}",
            data["lastActive"].as_str().unwrap_or("—"),
            if models.is_empty() {
                String::new()
            } else {
                format!(" · top models (30 days): {}", models.join(", "))
            }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn row(date: &str, cost: f64, tokens: u64, known: bool) -> Value {
        json!({"date": date, "project": "app", "costUsd": cost, "costKnown": known, "totalTokens": tokens,
            "models": {"m": {"costUsd": cost}}})
    }

    #[test]
    fn summarises_windows_per_provider_and_total() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 18).unwrap();
        let rows = vec![
            ("Claude".to_string(), row("2026-11-18", 2.0, 100, true)),
            ("Claude".to_string(), row("2026-11-12", 3.0, 50, true)),
            ("Codex".to_string(), row("2026-10-01", 5.0, 10, false)),
        ];
        let s = summarize(&rows, today);
        assert_eq!(s["Claude"]["windows"]["Today"]["costUsd"], 2.0);
        assert_eq!(s["Claude"]["windows"]["7 days"]["costUsd"], 5.0);
        assert_eq!(s["Claude"]["windows"]["7 days"]["activeDays"], 2);
        assert_eq!(s["Codex"]["windows"]["30 days"]["activeDays"], 0);
        assert_eq!(s["Total"]["windows"]["All time"]["costUsd"], 10.0);
        assert_eq!(s["Total"]["windows"]["All time"]["costKnown"], false);
        assert_eq!(s["Claude"]["lastActive"], "2026-11-18");
        assert_eq!(money(&s["Codex"]["windows"]["Today"]), "—");
        assert_eq!(money(&s["Total"]["windows"]["All time"]), "$10.00+");
        assert!(summarize(&rows[..1], today).get("Total").is_none());
    }

    #[test]
    fn branches_rank_by_recent_cost_per_agent() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 18).unwrap();
        let mut old = row("2026-09-01", 50.0, 10, true);
        old["branch"] = json!("main");
        let mut recent = row("2026-11-17", 4.0, 10, true);
        recent["branch"] = json!("feat/x");
        let mut also = row("2026-11-01", 1.0, 10, true);
        also["branch"] = json!("feat/x");
        let rows = vec![
            ("Claude".to_string(), old),
            ("Claude".to_string(), recent),
            ("Claude".to_string(), also),
        ];
        let b = branches(&rows, today);
        assert_eq!(b[0]["branch"], "feat/x");
        assert_eq!(b[0]["costUsd7d"], 4.0);
        assert_eq!(b[0]["costUsd30d"], 5.0);
        assert_eq!(b[1]["branch"], "main");
        assert_eq!(b[1]["costUsd30d"], 0.0);
        assert!(b[1]["costUsd30d"].as_f64().unwrap().is_sign_positive());
        assert_eq!(b[1]["costUsd"], 50.0);
    }

    #[test]
    fn names_a_git_worktree_after_its_repository() {
        let dir = usagestat_core::storage::temporary_directory().unwrap();
        let repo = dir.path().join("my-app");
        let tree = dir.path().join("t3-1234abcd");
        let git = |args: &[&str], cwd: &Path| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(cwd)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        };
        std::fs::create_dir(&repo).unwrap();
        git(&["init", "-q"], &repo);
        git(
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "x",
            ],
            &repo,
        );
        git(&["worktree", "add", "-q", tree.to_str().unwrap()], &repo);
        assert_eq!(project_for(&repo), "my-app");
        assert_eq!(project_for(&tree), "my-app");
        assert_eq!(project_for(&repo.join("missing/sub")), "my-app");
    }
}
