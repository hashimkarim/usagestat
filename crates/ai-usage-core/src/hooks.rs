//! Event hooks: configuration, event payloads and the transition detector.
//!
//! The detector is pure and in-memory. It turns successive fresh provider
//! snapshots into edge-triggered events and never fetches or runs anything.
//! The first observation of a provider or quota lane only sets a baseline, so
//! a daemon restart never replays old crossings.

use crate::model::{MetricLine, ProviderState, UsageSnapshot};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fmt;

pub const PAYLOAD_SCHEMA: &str = "usagestat.hook-event.v1";
pub const DEFAULT_THRESHOLDS: [f64; 3] = [75.0, 90.0, 100.0];
pub const DEFAULT_TIMEOUT_SEC: u64 = 10;
pub const MAX_TIMEOUT_SEC: u64 = 60;
/// A drop of at least this many percentage points counts as a quota reset.
pub const RESET_DROP_PERCENT: f64 = 20.0;
/// A fired threshold re-arms only after usage falls this far below it.
pub const REARM_HYSTERESIS_PERCENT: f64 = 5.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    /// A progress metric's used percent crossed a configured threshold upward.
    QuotaThreshold,
    /// A progress metric's window reset (usage dropped sharply or the reset time passed).
    QuotaReset,
    /// A provider that was working started returning an error or auth state.
    ProviderFailing,
    /// A failing provider returned usable data again.
    ProviderRecovered,
}

impl HookEvent {
    pub const ALL: [Self; 4] = [
        Self::QuotaThreshold,
        Self::QuotaReset,
        Self::ProviderFailing,
        Self::ProviderRecovered,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::QuotaThreshold => "quota_threshold",
            Self::QuotaReset => "quota_reset",
            Self::ProviderFailing => "provider_failing",
            Self::ProviderRecovered => "provider_recovered",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|event| event.as_str() == name)
    }
}

impl fmt::Display for HookEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One `[[hooks]]` entry. Parsing is lenient: an entry with the wrong shape is
/// kept as an invalid hook (see [`HookConfig::validate`]) so one typo never
/// makes the whole configuration unreadable for provider polling.
#[derive(Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct HookConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Event names, such as `quota_threshold`. Unknown names invalidate the hook.
    #[serde(default)]
    pub events: Vec<String>,
    /// Provider ids or instance ids to watch. Empty watches every provider.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<String>,
    /// Used percents for `quota_threshold`. Defaults to 75, 90 and 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thresholds: Option<Vec<f64>>,
    /// Program and arguments, run directly without a shell.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// http or https endpoint that receives the payload as a JSON POST.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_sec: Option<u64>,
    /// Why the entry could not be parsed. Never serialized.
    #[serde(skip)]
    pub parse_error: Option<String>,
}

// Header values and URLs can carry tokens (Slack/Discord webhooks keep the
// secret in the path), so Debug output never includes them.
impl fmt::Debug for HookConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HookConfig")
            .field("enabled", &self.enabled)
            .field("events", &self.events)
            .field("providers", &self.providers)
            .field("thresholds", &self.thresholds)
            .field("program", &self.program_name())
            .field("url", &self.url.as_ref().map(|_| "<redacted>"))
            .field("headers", &self.headers.keys().collect::<Vec<_>>())
            .field("timeout_sec", &self.timeout_sec)
            .field("parse_error", &self.parse_error)
            .finish()
    }
}

impl HookConfig {
    /// Check the entry before it is used; invalid hooks are skipped with a warning.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(error) = &self.parse_error {
            return Err(error.clone());
        }
        if self.events.is_empty() {
            return Err("no events configured".into());
        }
        if let Some(unknown) = self.events.iter().find(|e| HookEvent::parse(e).is_none()) {
            let known = HookEvent::ALL.map(HookEvent::as_str).join(", ");
            return Err(format!(
                "unknown event {unknown:?}; expected one of {known}"
            ));
        }
        if self.command.is_empty() && self.url.is_none() {
            return Err("set `command` and/or `url`".into());
        }
        if self.command.first().is_some_and(|p| p.trim().is_empty()) {
            return Err("command program is empty".into());
        }
        if self.command.iter().any(|arg| arg.contains('\0')) {
            return Err("command arguments cannot contain NUL".into());
        }
        if let Some(url) = &self.url {
            let lower = url.trim().to_ascii_lowercase();
            if !(lower.starts_with("http://") || lower.starts_with("https://")) {
                return Err("url must use http or https".into());
            }
        }
        if let Some(thresholds) = &self.thresholds
            && thresholds.iter().any(|t| !t.is_finite() || *t <= 0.0)
        {
            return Err("thresholds must be positive percents".into());
        }
        Ok(())
    }

    pub fn subscribes(&self, event: HookEvent) -> bool {
        self.events.iter().any(|name| name == event.as_str())
    }

    /// Match the provider filter against the snapshot id (an instance id when
    /// one is configured) and the plugin id it came from.
    pub fn watches(&self, provider_id: &str, plugin_id: &str) -> bool {
        self.providers.is_empty()
            || self
                .providers
                .iter()
                .any(|p| p == provider_id || p == plugin_id)
    }

    pub fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(
            self.timeout_sec
                .unwrap_or(DEFAULT_TIMEOUT_SEC)
                .clamp(1, MAX_TIMEOUT_SEC),
        )
    }

    pub fn threshold_list(&self) -> Vec<f64> {
        let mut list: Vec<f64> = self
            .thresholds
            .clone()
            .unwrap_or_else(|| DEFAULT_THRESHOLDS.to_vec())
            .into_iter()
            .filter(|t| t.is_finite() && *t > 0.0)
            .collect();
        list.sort_by(f64::total_cmp);
        list.dedup();
        list
    }

    /// Program basename, safe to log (arguments may carry tokens).
    pub fn program_name(&self) -> Option<String> {
        self.command.first().map(|program| {
            std::path::Path::new(program)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| program.clone())
        })
    }
}

/// Deserialize `[[hooks]]` one entry at a time, keeping malformed entries as
/// invalid hooks instead of failing the whole configuration.
pub(crate) fn deserialize_hooks<'de, D>(deserializer: D) -> Result<Vec<HookConfig>, D::Error>
where
    D: Deserializer<'de>,
{
    let entries = Vec::<toml::Value>::deserialize(deserializer)?;
    Ok(entries
        .into_iter()
        .map(|entry| {
            // toml messages can quote the offending value (a header or URL),
            // so the stored reason names the problem without echoing it.
            entry
                .try_into::<HookConfig>()
                .unwrap_or_else(|_| HookConfig {
                    parse_error: Some(
                        "invalid hook entry: check field names and types (events and command are arrays of strings)"
                            .into(),
                    ),
                    ..HookConfig::default()
                })
        })
        .collect())
}

/// The JSON document delivered to hooks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HookPayload {
    pub schema: String,
    pub event: HookEvent,
    /// Snapshot id: the configured instance id, or the plugin id.
    pub provider_id: String,
    pub plugin_id: String,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metric: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_state: Option<String>,
    pub timestamp: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub test: bool,
}

impl HookPayload {
    /// `USAGESTAT_*` variables for command hooks. Absent fields are omitted.
    pub fn environment(&self, json: &str) -> Vec<(String, String)> {
        let clean = |value: &str| value.replace('\0', "");
        let mut env = vec![
            ("USAGESTAT_EVENT".into(), self.event.as_str().to_string()),
            ("USAGESTAT_PROVIDER".into(), clean(&self.provider_id)),
            ("USAGESTAT_PLUGIN".into(), clean(&self.plugin_id)),
            ("USAGESTAT_DISPLAY_NAME".into(), clean(&self.display_name)),
            ("USAGESTAT_TIMESTAMP".into(), rfc3339(self.timestamp)),
            ("USAGESTAT_PAYLOAD".into(), clean(json)),
        ];
        let optional = [
            ("USAGESTAT_METRIC", self.metric.as_deref().map(clean)),
            ("USAGESTAT_USED_PERCENT", self.used_percent.map(number)),
            (
                "USAGESTAT_PREVIOUS_PERCENT",
                self.previous_percent.map(number),
            ),
            ("USAGESTAT_THRESHOLD", self.threshold.map(number)),
            ("USAGESTAT_RESETS_AT", self.resets_at.map(rfc3339)),
            ("USAGESTAT_STATE", self.state.clone()),
            ("USAGESTAT_PREVIOUS_STATE", self.previous_state.clone()),
            ("USAGESTAT_TEST", self.test.then(|| "1".to_string())),
        ];
        env.extend(
            optional
                .into_iter()
                .filter_map(|(name, value)| value.map(|v| (name.to_string(), v))),
        );
        env
    }

    /// A representative payload for `usagestat hooks test`.
    pub fn sample(event: HookEvent, provider_id: &str, now: DateTime<Utc>) -> Self {
        let quota = matches!(event, HookEvent::QuotaThreshold | HookEvent::QuotaReset);
        let (used, previous) = match event {
            HookEvent::QuotaThreshold => (Some(91.0), Some(84.0)),
            HookEvent::QuotaReset => (Some(0.0), Some(97.0)),
            _ => (None, None),
        };
        let (state, previous_state) = match event {
            HookEvent::ProviderFailing => (Some("failed"), Some("ready")),
            HookEvent::ProviderRecovered => (Some("ready"), Some("failed")),
            _ => (None, None),
        };
        Self {
            schema: PAYLOAD_SCHEMA.into(),
            event,
            provider_id: provider_id.into(),
            plugin_id: provider_id.into(),
            display_name: "Sample provider".into(),
            metric: quota.then(|| "Session".into()),
            used_percent: used,
            previous_percent: previous,
            threshold: (event == HookEvent::QuotaThreshold).then_some(90.0),
            resets_at: quota.then(|| now + chrono::Duration::hours(5)),
            state: state.map(str::to_string),
            previous_state: previous_state.map(str::to_string),
            timestamp: now,
            test: true,
        }
    }
}

fn rfc3339(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn number(value: f64) -> String {
    let rounded = (value * 100.0).round() / 100.0;
    if rounded.fract() == 0.0 {
        format!("{rounded:.0}")
    } else {
        format!("{rounded}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Health {
    Healthy,
    Failing,
}

fn health(snapshot: &UsageSnapshot) -> Option<Health> {
    match snapshot.state {
        Some(
            ProviderState::Failed
            | ProviderState::TimedOut
            | ProviderState::MissingAuth
            | ProviderState::CredentialDenied
            | ProviderState::CredentialUnavailable
            | ProviderState::CredentialAccountMismatch
            | ProviderState::CredentialMalformed,
        ) => Some(Health::Failing),
        // Unsupported and empty datasets are neither a failure nor a recovery.
        Some(ProviderState::Unsupported | ProviderState::NoData) => None,
        Some(ProviderState::Ready) | None if snapshot.source.as_deref() == Some("error") => {
            Some(Health::Failing)
        }
        Some(ProviderState::Ready) | None => Some(Health::Healthy),
    }
}

fn state_name(snapshot: &UsageSnapshot, health: Health) -> String {
    snapshot
        .state
        .and_then(|state| serde_json::to_value(state).ok())
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| match health {
            Health::Healthy => "ready".into(),
            Health::Failing => "failed".into(),
        })
}

#[derive(Debug, Clone)]
struct Lane {
    used: f64,
    resets_at: Option<DateTime<Utc>>,
    /// Highest threshold that has fired (or was already passed at baseline).
    armed_above: f64,
}

#[derive(Debug, Clone)]
struct ProviderTrack {
    fetched_at: DateTime<Utc>,
    health: Option<(Health, String)>,
    lanes: HashMap<String, Lane>,
}

/// Per-hook transition state. Each hook owns one so its thresholds and
/// re-arming are independent of other hooks.
#[derive(Debug, Clone)]
pub struct HookDetector {
    thresholds: Vec<f64>,
    providers: HashMap<String, ProviderTrack>,
}

impl HookDetector {
    pub fn new(thresholds: Vec<f64>) -> Self {
        Self {
            thresholds,
            providers: HashMap::new(),
        }
    }

    fn level(&self, used: f64) -> f64 {
        self.thresholds
            .iter()
            .copied()
            .filter(|t| *t <= used)
            .fold(f64::NEG_INFINITY, f64::max)
    }

    /// Observe one freshly probed snapshot and return the events it caused.
    /// Cached or replayed snapshots (same or older `fetched_at`) change nothing.
    pub fn observe(
        &mut self,
        plugin_id: &str,
        snapshot: &UsageSnapshot,
        now: DateTime<Utc>,
    ) -> Vec<HookPayload> {
        if matches!(
            snapshot.source.as_deref(),
            Some("cached" | "local-estimate")
        ) {
            return Vec::new();
        }
        let base = |event: HookEvent| HookPayload {
            schema: PAYLOAD_SCHEMA.into(),
            event,
            provider_id: snapshot.provider_id.clone(),
            plugin_id: plugin_id.to_string(),
            display_name: snapshot.display_name.clone(),
            metric: None,
            used_percent: None,
            previous_percent: None,
            threshold: None,
            resets_at: None,
            state: None,
            previous_state: None,
            timestamp: now,
            test: false,
        };
        let mut events = Vec::new();
        let first = !self.providers.contains_key(&snapshot.provider_id);
        let mut track = self
            .providers
            .remove(&snapshot.provider_id)
            .unwrap_or_else(|| ProviderTrack {
                fetched_at: snapshot.fetched_at,
                health: None,
                lanes: HashMap::new(),
            });
        if !first && snapshot.fetched_at <= track.fetched_at {
            self.providers.insert(snapshot.provider_id.clone(), track);
            return events;
        }
        track.fetched_at = snapshot.fetched_at;

        let current_health = health(snapshot);
        if let Some(current) = current_health {
            let name = state_name(snapshot, current);
            if let Some((previous, previous_name)) = &track.health
                && *previous != current
            {
                let event = match current {
                    Health::Failing => HookEvent::ProviderFailing,
                    Health::Healthy => HookEvent::ProviderRecovered,
                };
                events.push(HookPayload {
                    state: Some(name.clone()),
                    previous_state: Some(previous_name.clone()),
                    ..base(event)
                });
            }
            track.health = Some((current, name));
        }

        // Error snapshots carry no quota data: keep lane baselines so a
        // recovery compares against the last real reading.
        if current_health == Some(Health::Healthy) {
            let mut seen = HashMap::new();
            for metric in &snapshot.metrics {
                let MetricLine::Progress {
                    label,
                    used,
                    limit,
                    resets_at,
                    ..
                } = metric
                else {
                    continue;
                };
                if !(*limit > 0.0 && used.is_finite() && limit.is_finite())
                    || seen.contains_key(label)
                {
                    continue;
                }
                let used = used / limit * 100.0;
                let lane = match track.lanes.remove(label) {
                    None => Lane {
                        used,
                        resets_at: *resets_at,
                        armed_above: self.level(used),
                    },
                    Some(previous) => {
                        let mut lane = Lane {
                            used,
                            resets_at: *resets_at,
                            armed_above: previous.armed_above,
                        };
                        let dropped = previous.used - used >= RESET_DROP_PERCENT;
                        let window_rolled = previous.used > 0.0
                            && previous.used > used
                            && matches!(
                                (previous.resets_at, *resets_at),
                                (Some(before), Some(after)) if before <= now && after > before
                            );
                        let quota = |event| HookPayload {
                            metric: Some(label.clone()),
                            used_percent: Some(round(used)),
                            previous_percent: Some(round(previous.used)),
                            resets_at: *resets_at,
                            ..base(event)
                        };
                        if dropped || window_rolled {
                            // A reset suppresses threshold events in the same poll.
                            events.push(quota(HookEvent::QuotaReset));
                            lane.armed_above = self.level(used);
                        } else {
                            let level = self.level(used);
                            if level > previous.armed_above {
                                events.push(HookPayload {
                                    threshold: Some(level),
                                    ..quota(HookEvent::QuotaThreshold)
                                });
                                lane.armed_above = level;
                            } else {
                                lane.armed_above = previous
                                    .armed_above
                                    .min(self.level(used + REARM_HYSTERESIS_PERCENT));
                            }
                        }
                        lane
                    }
                };
                seen.insert(label.clone(), lane);
            }
            // Lanes missing from a successful snapshot start fresh if they return.
            track.lanes = seen;
        }
        self.providers.insert(snapshot.provider_id.clone(), track);
        events
    }
}

fn round(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProgressFormat;
    use chrono::{Duration, TimeZone};

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 18, 12, 0, 0).unwrap()
    }

    struct Probe {
        detector: HookDetector,
        tick: i64,
    }

    impl Probe {
        fn new() -> Self {
            Self {
                detector: HookDetector::new(DEFAULT_THRESHOLDS.to_vec()),
                tick: 0,
            }
        }

        fn at(&mut self, snapshot: UsageSnapshot) -> Vec<HookPayload> {
            self.tick += 1;
            let now = t0() + Duration::minutes(self.tick);
            let mut snapshot = snapshot;
            snapshot.fetched_at = now;
            self.detector.observe("claude", &snapshot, now)
        }

        fn used(&mut self, used: f64) -> Vec<HookPayload> {
            self.at(quota(used, Some(t0() + Duration::hours(5))))
        }
    }

    fn quota(used: f64, resets_at: Option<DateTime<Utc>>) -> UsageSnapshot {
        let mut snapshot = UsageSnapshot::error("claude", "Claude", "unused");
        snapshot.source = Some("oauth".into());
        snapshot.state = Some(ProviderState::Ready);
        snapshot.metrics = vec![MetricLine::Progress {
            label: "Session".into(),
            used,
            limit: 100.0,
            format: ProgressFormat::Percent,
            resets_at,
            period_duration_ms: None,
            detail: None,
            color: None,
        }];
        snapshot
    }

    fn names(events: &[HookPayload]) -> Vec<(&'static str, Option<f64>)> {
        events
            .iter()
            .map(|e| (e.event.as_str(), e.threshold))
            .collect()
    }

    #[test]
    fn first_observation_only_sets_a_baseline() {
        let mut probe = Probe::new();
        assert!(probe.used(95.0).is_empty());
        let mut failing = Probe::new();
        assert!(
            failing
                .at(UsageSnapshot::error("claude", "Claude", "boom"))
                .is_empty()
        );
    }

    #[test]
    fn upward_crossing_fires_the_highest_threshold_once() {
        let mut probe = Probe::new();
        probe.used(50.0);
        assert!(probe.used(70.0).is_empty());
        let events = probe.used(92.0);
        assert_eq!(names(&events), [("quota_threshold", Some(90.0))]);
        assert_eq!(events[0].previous_percent, Some(70.0));
        assert_eq!(events[0].used_percent, Some(92.0));
        assert_eq!(events[0].metric.as_deref(), Some("Session"));
        assert!(events[0].resets_at.is_some());
        // Staying above, or rising within the same band, does not refire.
        assert!(probe.used(95.0).is_empty());
        assert!(probe.used(93.0).is_empty());
        assert_eq!(
            names(&probe.used(100.0)),
            [("quota_threshold", Some(100.0))]
        );
    }

    #[test]
    fn jitter_around_a_threshold_is_debounced_until_it_re_arms() {
        let mut probe = Probe::new();
        probe.used(70.0);
        assert_eq!(names(&probe.used(75.5)), [("quota_threshold", Some(75.0))]);
        assert!(probe.used(74.0).is_empty());
        assert!(
            probe.used(76.0).is_empty(),
            "within hysteresis must not refire"
        );
        assert!(probe.used(68.0).is_empty());
        assert_eq!(names(&probe.used(80.0)), [("quota_threshold", Some(75.0))]);
    }

    #[test]
    fn sharp_drop_fires_reset_and_suppresses_thresholds() {
        let mut probe = Probe::new();
        probe.used(96.0);
        let events = probe.used(3.0);
        assert_eq!(names(&events), [("quota_reset", None)]);
        assert_eq!(events[0].previous_percent, Some(96.0));
        assert!(probe.used(2.0).is_empty(), "reset fires once");
        assert_eq!(names(&probe.used(91.0)), [("quota_threshold", Some(90.0))]);
    }

    #[test]
    fn passed_reset_time_fires_reset_even_for_small_drops() {
        let mut probe = Probe::new();
        let first_reset = t0() + Duration::minutes(2);
        probe.at(quota(12.0, Some(first_reset)));
        // Window rolled: old reset time is now in the past, new one is later.
        let events = probe.at(quota(1.0, Some(first_reset + Duration::hours(5))));
        assert_eq!(names(&events), [("quota_reset", None)]);
        // A drifting reset time without a usage drop is not a reset.
        let mut drift = Probe::new();
        drift.at(quota(12.0, Some(first_reset)));
        assert!(
            drift
                .at(quota(12.0, Some(first_reset + Duration::hours(5))))
                .is_empty()
        );
    }

    #[test]
    fn failure_and_recovery_fire_on_edges_only() {
        let mut probe = Probe::new();
        probe.used(10.0);
        let events = probe.at(UsageSnapshot::error("claude", "Claude", "boom"));
        assert_eq!(names(&events), [("provider_failing", None)]);
        assert_eq!(events[0].state.as_deref(), Some("failed"));
        assert_eq!(events[0].previous_state.as_deref(), Some("ready"));
        assert!(
            probe
                .at(UsageSnapshot::error(
                    "claude",
                    "Claude",
                    "missing-auth: login"
                ))
                .is_empty(),
            "still failing"
        );
        let events = probe.used(12.0);
        assert_eq!(names(&events), [("provider_recovered", None)]);
        assert_eq!(events[0].previous_state.as_deref(), Some("missing-auth"));
        assert!(probe.used(13.0).is_empty());
    }

    #[test]
    fn crossing_during_an_outage_fires_on_recovery_against_the_last_reading() {
        let mut probe = Probe::new();
        probe.used(60.0);
        probe.at(UsageSnapshot::error("claude", "Claude", "boom"));
        let events = probe.used(91.0);
        assert_eq!(
            names(&events),
            [
                ("provider_recovered", None),
                ("quota_threshold", Some(90.0))
            ]
        );
    }

    #[test]
    fn neutral_states_and_stale_snapshots_change_nothing() {
        let mut probe = Probe::new();
        probe.used(10.0);
        let mut no_data = UsageSnapshot::error("claude", "Claude", "no-data: empty");
        no_data.source = Some("error".into());
        assert!(probe.at(no_data).is_empty());
        let mut cached = quota(99.0, None);
        cached.source = Some("cached".into());
        assert!(probe.at(cached).is_empty());
        // A replay with an older fetch time is ignored.
        let mut old = quota(99.0, None);
        old.fetched_at = t0();
        assert!(probe.detector.observe("claude", &old, t0()).is_empty());
        assert_eq!(names(&probe.used(91.0)), [("quota_threshold", Some(90.0))]);
    }

    #[test]
    fn hook_entries_parse_leniently_and_validate() {
        let config: crate::AppConfig = toml::from_str(
            r#"
            [[hooks]]
            events = ["quota_threshold", "provider_failing"]
            providers = ["claude"]
            command = ["notify-send", "usagestat"]
            timeoutSec = 5

            [[hooks]]
            events = ["quota_reset"]
            url = "https://example.invalid/hook"
            headers = { Authorization = "Bearer secret-token" }

            [[hooks]]
            events = "quota_threshold"

            [[hooks]]
            events = ["quota_low"]
            command = ["true"]
            "#,
        )
        .unwrap();
        assert_eq!(config.hooks.len(), 4);
        assert!(config.hooks[0].validate().is_ok());
        assert!(config.hooks[0].watches("claude-work", "claude"));
        assert!(!config.hooks[0].watches("codex", "codex"));
        assert!(config.hooks[1].validate().is_ok());
        assert!(
            config.hooks[2]
                .validate()
                .unwrap_err()
                .contains("invalid hook")
        );
        assert!(
            config.hooks[3]
                .validate()
                .unwrap_err()
                .contains("quota_low")
        );
        let debug = format!("{config:?}");
        assert!(!debug.contains("secret-token") && !debug.contains("example.invalid"));
        assert_eq!(config.hooks[0].timeout().as_secs(), 5);
        assert_eq!(config.hooks[1].timeout().as_secs(), DEFAULT_TIMEOUT_SEC);
    }

    #[test]
    fn environment_mirrors_the_payload() {
        let payload = HookPayload::sample(HookEvent::QuotaThreshold, "claude", t0());
        let json = serde_json::to_string(&payload).unwrap();
        let env: HashMap<_, _> = payload.environment(&json).into_iter().collect();
        assert_eq!(env["USAGESTAT_EVENT"], "quota_threshold");
        assert_eq!(env["USAGESTAT_PROVIDER"], "claude");
        assert_eq!(env["USAGESTAT_USED_PERCENT"], "91");
        assert_eq!(env["USAGESTAT_THRESHOLD"], "90");
        assert_eq!(env["USAGESTAT_TEST"], "1");
        assert_eq!(env["USAGESTAT_PAYLOAD"], json);
        assert!(!env.contains_key("USAGESTAT_STATE"));
    }
}
