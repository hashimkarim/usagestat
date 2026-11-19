use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ProgressFormat {
    Percent,
    Dollars,
    Count { suffix: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BarChartPoint {
    pub label: String,
    pub value: f64,
    #[serde(rename = "valueLabel", skip_serializing_if = "Option::is_none")]
    pub value_label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum MetricLine {
    Text {
        label: String,
        value: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        color: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        subtitle: Option<String>,
    },
    Progress {
        label: String,
        used: f64,
        limit: f64,
        format: ProgressFormat,
        #[serde(rename = "resetsAt", skip_serializing_if = "Option::is_none")]
        resets_at: Option<DateTime<Utc>>,
        #[serde(rename = "periodDurationMs", skip_serializing_if = "Option::is_none")]
        period_duration_ms: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        color: Option<String>,
    },
    Badge {
        label: String,
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        color: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        subtitle: Option<String>,
    },
    #[serde(rename = "barChart")]
    BarChart {
        label: String,
        points: Vec<BarChartPoint>,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        color: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Pace {
    pub stage: String,
    pub delta_percent: f64,
    pub expected_used_percent: f64,
    pub actual_used_percent: f64,
    pub will_last_to_reset: bool,
    pub eta_seconds: Option<f64>,
}

/// When a quota window runs out if use continues at its average rate so far,
/// provided that comes before the window resets. `None` while on pace to last,
/// already exhausted, or too early in the window (first 5% and 10 minutes) for
/// the rate to mean anything.
pub fn quota_exhaustion(
    used_percent: f64,
    resets_at: DateTime<Utc>,
    period_ms: u64,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let until_reset = (resets_at - now).num_milliseconds() as f64;
    let period = period_ms as f64;
    let elapsed = period - until_reset;
    if !(5.0..100.0).contains(&used_percent)
        || until_reset <= 0.0
        || elapsed < (period * 0.05).max(10.0 * 60_000.0)
    {
        return None;
    }
    let ms_left = (100.0 - used_percent) * elapsed / used_percent;
    (ms_left < until_reset).then(|| now + chrono::Duration::milliseconds(ms_left as i64))
}

fn pace_stage(delta: f64) -> &'static str {
    if delta < -20.0 {
        "well_under"
    } else if delta < -10.0 {
        "under"
    } else if delta < -3.0 {
        "slightly_under"
    } else if delta <= 3.0 {
        "on_track"
    } else if delta <= 10.0 {
        "slightly_over"
    } else if delta <= 20.0 {
        "over"
    } else {
        "well_over"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageSnapshot {
    pub provider_id: String,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    pub metrics: Vec<MetricLine>,
    pub fetched_at: DateTime<Utc>,
    /// Static status page URL for the provider. null if unknown.
    #[serde(default)]
    pub status_page_url: Option<String>,
    /// Pace tracking relative to reset window. null if no reset window on primary metric.
    #[serde(default)]
    pub pace: Option<Pace>,
    /// Additive machine-readable state. Old caches/clients may omit or ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<ProviderState>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderState {
    Ready,
    Unsupported,
    MissingAuth,
    NoData,
    CredentialDenied,
    CredentialUnavailable,
    CredentialAccountMismatch,
    CredentialMalformed,
    TimedOut,
    Failed,
}
impl ProviderState {
    pub fn from_message(message: &str) -> Self {
        let message = message.trim();
        for (prefix, state) in [
            ("unsupported:", Self::Unsupported),
            ("credential-unsupported:", Self::Unsupported),
            ("missing-auth:", Self::MissingAuth),
            ("credential-missing:", Self::MissingAuth),
            ("no-data:", Self::NoData),
            ("credential-denied:", Self::CredentialDenied),
            ("credential-unavailable:", Self::CredentialUnavailable),
            (
                "credential-account-mismatch:",
                Self::CredentialAccountMismatch,
            ),
            ("credential-malformed:", Self::CredentialMalformed),
            ("timed-out:", Self::TimedOut),
        ] {
            if message.starts_with(prefix)
                || message.starts_with(&format!("keychain read failed: {prefix}"))
            {
                return state;
            }
        }
        // Preserve explicit legacy provider and deadline messages. Unknown
        // errors stay failed; never infer authentication from an empty dataset.
        if message.starts_with("Not logged in.") {
            Self::MissingAuth
        } else if message.starts_with("Probe timed out") {
            Self::TimedOut
        } else {
            Self::Failed
        }
    }
}

impl UsageSnapshot {
    pub fn error(
        provider_id: impl Into<String>,
        display_name: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        let message = message.into();
        let state = ProviderState::from_message(&message);
        Self {
            provider_id: provider_id.into(),
            display_name: display_name.into(),
            source: Some("error".to_string()),
            plan: None,
            metrics: vec![MetricLine::Badge {
                label: "Error".to_string(),
                text: message,
                color: Some("red".to_string()),
                subtitle: None,
            }],
            fetched_at: Utc::now(),
            status_page_url: None,
            pace: None,
            state: Some(state),
        }
    }

    /// Compute pace from the primary percent metric's reset window.
    /// Returns None if there is no percent metric with both resetsAt and periodDurationMs.
    pub fn compute_pace(&self) -> Option<Pace> {
        if matches!(self.source.as_deref(), Some("local-estimate" | "cached")) {
            return None;
        }
        let now = Utc::now();

        let (used, limit, resets_at, period_ms) = self.metrics.iter().find_map(|m| {
            if let MetricLine::Progress {
                used,
                limit,
                format: ProgressFormat::Percent,
                resets_at: Some(resets_at),
                period_duration_ms: Some(period_ms),
                ..
            } = m
            {
                if *limit > 0.0 {
                    return Some((*used, *limit, *resets_at, *period_ms));
                }
            }
            None
        })?;

        let ms_until_reset = (resets_at - now).num_milliseconds().max(0) as f64;
        // If reset has already passed, pace is not meaningful
        if ms_until_reset <= 0.0 {
            return None;
        }

        let period_ms_f = period_ms as f64;
        let ms_elapsed = period_ms_f - ms_until_reset;
        if ms_elapsed <= 0.0 {
            return None;
        }

        let actual_used_percent = (used / limit * 100.0).clamp(0.0, 100.0);
        let expected_used_percent = (ms_elapsed / period_ms_f * 100.0).clamp(0.0, 100.0);
        let delta_percent = actual_used_percent - expected_used_percent;
        let stage = pace_stage(delta_percent);

        let burn_rate_per_ms = if actual_used_percent > 0.0 {
            actual_used_percent / ms_elapsed
        } else {
            0.0
        };

        let (will_last_to_reset, eta_seconds) = if burn_rate_per_ms <= 0.0 {
            (true, None)
        } else {
            let remaining = 100.0 - actual_used_percent;
            let ms_to_exhaustion = remaining / burn_rate_per_ms;
            let will_last = ms_to_exhaustion >= ms_until_reset;
            let eta = if stage == "well_under" {
                None
            } else {
                Some(ms_to_exhaustion / 1000.0)
            };
            (will_last, eta)
        };

        Some(Pace {
            stage: stage.to_string(),
            delta_percent,
            expected_used_percent,
            actual_used_percent,
            will_last_to_reset,
            eta_seconds,
        })
    }
}

#[cfg(test)]
mod provider_sync_tests {
    use super::*;

    #[test]
    fn pace_requires_live_authoritative_quota_windows() {
        let mut snapshot = UsageSnapshot::error("test", "Test", "unused");
        snapshot.metrics = vec![MetricLine::Progress {
            label: "Session".into(),
            used: 25.0,
            limit: 100.0,
            format: ProgressFormat::Percent,
            resets_at: Some(Utc::now() + chrono::Duration::hours(2)),
            period_duration_ms: Some(5 * 60 * 60 * 1000),
            detail: None,
            color: None,
        }];
        snapshot.source = Some("api".into());
        assert!(snapshot.compute_pace().is_some());
        for source in ["local-estimate", "cached"] {
            snapshot.source = Some(source.into());
            assert!(snapshot.compute_pace().is_none(), "{source}");
        }
    }

    #[test]
    fn exhaustion_is_forecast_only_when_the_rate_runs_out_before_reset() {
        let now = Utc::now();
        let hours = |h: i64| chrono::Duration::hours(h);
        let five_hours = 5 * 3_600_000;
        // 3h into a 5h window at 75%: 100% in one more hour, before the reset.
        assert_eq!(
            quota_exhaustion(75.0, now + hours(2), five_hours, now),
            Some(now + hours(1))
        );
        // 3h in at 30%: lasts to the reset.
        assert_eq!(
            quota_exhaustion(30.0, now + hours(2), five_hours, now),
            None
        );
        // Too early in the window, too little use, or already out.
        let minutes = chrono::Duration::minutes(295);
        assert_eq!(quota_exhaustion(50.0, now + minutes, five_hours, now), None);
        assert_eq!(quota_exhaustion(4.0, now + hours(4), five_hours, now), None);
        assert_eq!(
            quota_exhaustion(100.0, now + hours(2), five_hours, now),
            None
        );
    }
}
