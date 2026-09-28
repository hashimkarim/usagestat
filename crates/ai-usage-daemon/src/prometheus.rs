use usagestat_core::{MetricLine, UsageSnapshot, model::ProviderState};

fn label(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\n', "\\n").replace('"', "\\\"")
}

/// No account names, credentials, or diagnostic messages become metric labels.
pub fn render(snapshots: &[&UsageSnapshot]) -> String {
    let mut out = String::from("# HELP usagestat_provider_up Last provider probe succeeded.\n# TYPE usagestat_provider_up gauge\n\
# HELP usagestat_usage_ratio Used fraction of a provider allowance.\n# TYPE usagestat_usage_ratio gauge\n\
# HELP usagestat_fetched_timestamp_seconds Original measurement time.\n# TYPE usagestat_fetched_timestamp_seconds gauge\n\
# HELP usagestat_reset_timestamp_seconds Provider allowance reset time.\n# TYPE usagestat_reset_timestamp_seconds gauge\n");
    for snapshot in snapshots {
        let id = label(&snapshot.provider_id);
        let ready = snapshot.source.as_deref() != Some("error")
            && snapshot.state.is_none_or(|state| state == ProviderState::Ready);
        out.push_str(&format!("usagestat_provider_up{{provider=\"{id}\"}} {}\n", u8::from(ready)));
        out.push_str(&format!("usagestat_fetched_timestamp_seconds{{provider=\"{id}\"}} {}\n", snapshot.fetched_at.timestamp()));
        if !ready { continue; }
        for metric in &snapshot.metrics {
            if let MetricLine::Progress { label: name, used, limit, resets_at, .. } = metric {
                if !used.is_finite() || !limit.is_finite() || *used < 0.0 || *limit <= 0.0 { continue; }
                let ratio = used / limit;
                if !ratio.is_finite() { continue; }
                let labels = format!("provider=\"{id}\",window=\"{}\"", label(name));
                out.push_str(&format!("usagestat_usage_ratio{{{labels}}} {ratio}\n"));
                if let Some(reset) = resets_at {
                    out.push_str(&format!("usagestat_reset_timestamp_seconds{{{labels}}} {}\n", reset.timestamp()));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_probe_exports_no_zero_allowance_or_secret_diagnostic() {
        let snapshot = UsageSnapshot::error("test", "Private account", "credential secret");
        let output = render(&[&snapshot]);
        assert!(output.contains("usagestat_provider_up{provider=\"test\"} 0"));
        assert!(!output.contains("secret") && !output.contains("Private account"));
        assert!(!output.contains("usagestat_usage_ratio{provider="));
    }
    #[test]
    fn labels_escape_prometheus_delimiters() {
        assert_eq!(label("a\\b\"\nc"), "a\\\\b\\\"\\nc");
    }
}
