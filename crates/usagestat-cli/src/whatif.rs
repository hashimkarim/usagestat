//! Recost local agent usage as if it had run on another model.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::time::Duration;

use crate::{daemon, project};

pub struct Options {
    pub model: String,
    pub provider: Option<String>,
    pub from: Option<String>,
    pub days: Option<u32>,
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

/// The log source whose own models share the target model's vendor.
fn default_provider(model: &str) -> Option<&'static str> {
    let model = model.to_ascii_lowercase();
    if model.starts_with("claude") {
        Some("claude")
    } else if model.starts_with("gpt") {
        Some("codex")
    } else if model.starts_with("gemini") {
        Some("gemini")
    } else {
        None
    }
}

/// The recost as JSON. Never prints, so the MCP server can reuse it.
pub fn fetch(options: Options) -> Result<Value> {
    let provider = match options.provider.as_deref() {
        Some(id) => project::SOURCES
            .iter()
            .find(|(source, _)| *source == id)
            .map(|(source, _)| *source)
            .with_context(|| {
                format!("unknown local log source `{id}`; use claude, codex, gemini or amp")
            })?,
        None => default_provider(&options.model).with_context(|| {
            format!(
                "choose the usage to recost with --provider claude|codex|gemini|amp for `{}`",
                options.model
            )
        })?,
    };
    let base = daemon::dashboard_base_url(options.bind)?;
    let client = daemon::local_client(Duration::from_secs(120))?;
    daemon::check_health(&base).with_context(|| {
        format!("the usagestat daemon is not responding at {base}; start it to read local usage")
    })?;
    let mut url = format!(
        "{base}/v1/local-usage/{provider}/whatif?model={}",
        options.model
    );
    if let Some(days) = options.days {
        url.push_str(&format!("&days={days}"));
    }
    if let Some(from) = &options.from {
        url.push_str(&format!("&from={from}"));
    }
    let mut body: Value = client.get(url).send()?.json()?;
    if let Some(message) = body["error"]["message"].as_str() {
        bail!("{message}");
    }
    if body.get("error").is_some() {
        bail!("the daemon could not recost usage: {}", body["error"]);
    }
    body["provider"] = json!(provider);
    body["days"] = json!(options.days);
    Ok(body)
}

fn print_text(report: &Value) {
    let money = |v: &Value| format!("${:.2}", v.as_f64().unwrap_or(0.0));
    let target = report["model"].as_str().unwrap_or("?");
    let scope = match report["days"].as_u64() {
        Some(d) => format!("last {d} days"),
        None => "all time".into(),
    };
    println!(
        "What if {} usage ({scope}) had run on {target} · API-equivalent estimates\n",
        report["provider"].as_str().unwrap_or("?")
    );
    println!(
        "{:<28}{:>14}{:>14}{:>16}",
        "Model used", "Tokens", "Cost", "On target"
    );
    for row in report["models"].as_array().into_iter().flatten() {
        let known = if row["costKnown"] == false { "+" } else { "" };
        println!(
            "{:<28}{:>14}{:>14}{:>16}",
            row["model"].as_str().unwrap_or("?"),
            project::tokens(row["totalTokens"].as_u64().unwrap_or(0)),
            format!("{}{known}", money(&row["costUsd"])),
            money(&row["whatIfCostUsd"]),
        );
    }
    let actual = report["costUsd"].as_f64().unwrap_or(0.0);
    let recost = report["whatIfCostUsd"].as_f64().unwrap_or(0.0);
    let change = if actual > 0.0 {
        format!(" ({:+.0}%)", (recost - actual) / actual * 100.0)
    } else {
        String::new()
    };
    println!(
        "\nTotal {} → {}{change}",
        money(&report["costUsd"]),
        money(&report["whatIfCostUsd"])
    );
    if report["crossProvider"] == true {
        println!("Models from another vendor tokenize and cache differently; treat this as rough.");
    }
    if let Some(note) = report["note"].as_str() {
        println!("{note}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_model_picks_its_vendors_log_source() {
        assert_eq!(default_provider("claude-sonnet-5-5"), Some("claude"));
        assert_eq!(default_provider("gpt-6.1-sol"), Some("codex"));
        assert_eq!(default_provider("mystery"), None);
    }
}
