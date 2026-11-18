//! Live provider incident status from public Statuspage.io pages.
//!
//! Only status pages whose host is in [`STATUSPAGE_HOSTS`] are polled. Those
//! hosts were verified to serve the Statuspage `api/v2/summary.json` document
//! at their own origin; anything else is reported as not monitored instead of
//! being probed or guessed. Requests are anonymous GETs: no provider
//! credentials, cookies or account identifiers are ever attached, and one
//! request is made per distinct status origin regardless of how many
//! providers share it.

use chrono::{DateTime, Utc};
use reqwest::{Url, blocking::Client, redirect::Policy};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;
use std::time::Duration;
use usagestat_core::ProviderSummary;

/// Healthy pages are re-read this often, independent of quota polling.
pub const POLL_INTERVAL_SECS: i64 = 300;
/// Consecutive failures back off exponentially up to this delay.
pub const MAX_BACKOFF_SECS: i64 = 3600;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_BODY_BYTES: u64 = 2 * 1024 * 1024;
const MAX_INCIDENTS: usize = 10;
const MAX_TEXT_CHARS: usize = 200;

/// Status page host → canonical Statuspage origin. A retired host maps to the
/// origin it permanently redirects to, so no redirect is ever followed.
const STATUSPAGE_HOSTS: &[(&str, &str)] = &[
    ("status.anthropic.com", "https://status.claude.com"),
    ("status.claude.com", "https://status.claude.com"),
    ("status.openai.com", "https://status.openai.com"),
    ("www.githubstatus.com", "https://www.githubstatus.com"),
    ("githubstatus.com", "https://www.githubstatus.com"),
    ("status.factory.ai", "https://status.factory.ai"),
    ("status.augmentcode.com", "https://status.augmentcode.com"),
    ("status.deepgram.com", "https://status.deepgram.com"),
    ("status.elevenlabs.io", "https://status.elevenlabs.io"),
    ("www.zoomstatus.com", "https://www.zoomstatus.com"),
];

/// Returns the Statuspage origin to poll for a provider's status page URL,
/// or `None` when the page is not a verified Statuspage.io site.
pub fn statuspage_origin(page_url: &str) -> Option<&'static str> {
    let url = Url::parse(page_url).ok()?;
    if url.scheme() != "https" {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    STATUSPAGE_HOSTS
        .iter()
        .find(|(known, _)| *known == host)
        .map(|(_, origin)| *origin)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Indicator {
    None,
    Minor,
    Major,
    Critical,
    Maintenance,
}

impl Indicator {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "none" => Self::None,
            "minor" => Self::Minor,
            "major" => Self::Major,
            "critical" => Self::Critical,
            "maintenance" => Self::Maintenance,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Incident {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub impact: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PageStatus {
    pub indicator: Indicator,
    pub description: Option<String>,
    pub incidents: Vec<Incident>,
    pub updated_at: Option<DateTime<Utc>>,
}

/// Parses a Statuspage `summary.json` (or `status.json`) document. Anything
/// that lacks the Statuspage `page` object or a known `status.indicator` is
/// rejected rather than read as "operational".
pub fn parse_summary(body: &[u8]) -> Result<PageStatus, &'static str> {
    let root: Value = serde_json::from_slice(body).map_err(|_| "invalid_json")?;
    let page = root
        .get("page")
        .and_then(Value::as_object)
        .ok_or("not_statuspage")?;
    let status = root
        .get("status")
        .and_then(Value::as_object)
        .ok_or("not_statuspage")?;
    let indicator = status
        .get("indicator")
        .and_then(Value::as_str)
        .and_then(Indicator::parse)
        .ok_or("unknown_indicator")?;

    let mut incidents = Vec::new();
    let unresolved = root
        .get("incidents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| {
            !matches!(
                item.get("status").and_then(Value::as_str),
                Some("resolved" | "postmortem" | "completed")
            )
        });
    let maintenance = root
        .get("scheduled_maintenances")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("status").and_then(Value::as_str) == Some("in_progress"));
    for item in unresolved.chain(maintenance) {
        if incidents.len() == MAX_INCIDENTS {
            break;
        }
        let Some(name) = item.get("name").and_then(Value::as_str).and_then(text) else {
            continue;
        };
        incidents.push(Incident {
            name,
            status: item.get("status").and_then(Value::as_str).and_then(text),
            impact: item.get("impact").and_then(Value::as_str).and_then(text),
            updated_at: item.get("updated_at").and_then(timestamp),
        });
    }

    Ok(PageStatus {
        indicator,
        description: status
            .get("description")
            .and_then(Value::as_str)
            .and_then(text),
        incidents,
        updated_at: page.get("updated_at").and_then(timestamp),
    })
}

fn text(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.chars().take(MAX_TEXT_CHARS).collect())
}

fn timestamp(value: &Value) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value.as_str()?)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

#[derive(Debug, Default)]
struct OriginEntry {
    last: Option<PageStatus>,
    checked_at: Option<DateTime<Utc>>,
    error: Option<String>,
    failures: u32,
    next_due: Option<DateTime<Utc>>,
}

/// Cached status per Statuspage origin with success interval and failure backoff.
#[derive(Debug)]
pub struct StatusMonitor {
    enabled: bool,
    origins: HashMap<&'static str, OriginEntry>,
}

impl Default for StatusMonitor {
    fn default() -> Self {
        Self::new(checks_enabled(
            std::env::var("USAGESTAT_STATUS_CHECKS").ok(),
        ))
    }
}

/// `USAGESTAT_STATUS_CHECKS=0|false|off|no` disables all status page requests.
fn checks_enabled(value: Option<String>) -> bool {
    !value.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        )
    })
}

/// Delay before the next request after `failures` consecutive failures.
pub fn retry_delay_secs(failures: u32) -> i64 {
    if failures == 0 {
        return POLL_INTERVAL_SECS;
    }
    let factor = 1_i64 << failures.saturating_sub(1).min(16);
    (POLL_INTERVAL_SECS.saturating_mul(factor)).min(MAX_BACKOFF_SECS)
}

impl StatusMonitor {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            origins: HashMap::new(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Distinct origins to monitor for the enabled providers.
    pub fn wanted_origins(&self, providers: &[ProviderSummary]) -> Vec<&'static str> {
        if !self.enabled {
            return Vec::new();
        }
        let mut origins: Vec<_> = providers
            .iter()
            .filter(|provider| provider.enabled)
            .filter_map(|provider| provider.status_page_url.as_deref())
            .filter_map(statuspage_origin)
            .collect();
        origins.sort_unstable();
        origins.dedup();
        origins
    }

    /// Forgets origins no longer wanted and returns those due for a request.
    pub fn due(&mut self, wanted: &[&'static str], now: DateTime<Utc>) -> Vec<&'static str> {
        self.origins.retain(|origin, _| wanted.contains(origin));
        wanted
            .iter()
            .copied()
            .filter(|origin| {
                self.origins
                    .get(origin)
                    .and_then(|entry| entry.next_due)
                    .is_none_or(|due| due <= now)
            })
            .collect()
    }

    pub fn record(
        &mut self,
        origin: &'static str,
        result: Result<PageStatus, String>,
        now: DateTime<Utc>,
    ) {
        let entry = self.origins.entry(origin).or_default();
        entry.checked_at = Some(now);
        match result {
            Ok(status) => {
                entry.last = Some(status);
                entry.error = None;
                entry.failures = 0;
            }
            Err(error) => {
                entry.error = Some(error);
                entry.failures = entry.failures.saturating_add(1);
            }
        }
        entry.next_due = Some(now + chrono::Duration::seconds(retry_delay_secs(entry.failures)));
    }

    /// One row per enabled provider for `GET /v1/status`.
    pub fn report(&self, providers: &[ProviderSummary]) -> Vec<ProviderStatusView> {
        providers
            .iter()
            .filter(|provider| provider.enabled)
            .map(|provider| self.view(provider))
            .collect()
    }

    fn view(&self, provider: &ProviderSummary) -> ProviderStatusView {
        let page = provider.status_page_url.clone();
        let origin = page.as_deref().and_then(statuspage_origin);
        let mut view = ProviderStatusView {
            provider_id: provider.id.clone(),
            status_page_url: page,
            monitored: false,
            state: None,
            indicator: None,
            description: None,
            incidents: None,
            page_updated_at: None,
            checked_at: None,
            error: None,
        };
        let Some(origin) = origin.filter(|_| self.enabled) else {
            return view;
        };
        view.monitored = true;
        let Some(entry) = self.origins.get(origin) else {
            view.state = Some(StatusState::Pending);
            return view;
        };
        view.checked_at = entry.checked_at;
        view.error = entry.error.clone();
        view.state = Some(match (&entry.last, &entry.error) {
            (None, None) => StatusState::Pending,
            (None, Some(_)) => StatusState::Unavailable,
            (Some(_), Some(_)) => StatusState::Stale,
            (Some(_), None) => StatusState::Ok,
        });
        if let Some(last) = &entry.last {
            view.indicator = Some(last.indicator);
            view.description = last.description.clone();
            view.incidents = Some(last.incidents.clone());
            view.page_updated_at = last.updated_at;
        }
        view
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StatusState {
    /// Monitored, not fetched yet.
    Pending,
    /// The latest request succeeded.
    Ok,
    /// The latest request failed; the previous result is still shown.
    Stale,
    /// No request has succeeded yet.
    Unavailable,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStatusView {
    pub provider_id: String,
    pub status_page_url: Option<String>,
    pub monitored: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<StatusState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indicator: Option<Indicator>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incidents: Option<Vec<Incident>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_updated_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Anonymous client: no cookie store, no redirects, HTTPS only.
pub fn client() -> Option<Client> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    Client::builder()
        .redirect(Policy::none())
        .https_only(true)
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .user_agent(concat!("usagestat/", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()
}

/// Reads `<origin>/api/v2/summary.json`. Errors are short category codes and
/// never echo response bodies.
pub fn fetch(client: &Client, origin: &str) -> Result<PageStatus, String> {
    let response = client
        .get(format!("{origin}/api/v2/summary.json"))
        .header("accept", "application/json")
        .send()
        .map_err(|error| {
            if error.is_timeout() {
                "timeout".to_string()
            } else {
                "network".to_string()
            }
        })?;
    let status = response.status();
    if status.is_redirection() {
        return Err("redirect".into());
    }
    if !status.is_success() {
        return Err(format!("http_{}", status.as_u16()));
    }
    let mut body = Vec::new();
    response
        .take(MAX_BODY_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|_| "network".to_string())?;
    if body.len() as u64 > MAX_BODY_BYTES {
        return Err("too_large".into());
    }
    parse_summary(&body).map_err(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const SUMMARY: &str = r#"{
      "page": {"id": "x", "name": "Claude", "url": "https://status.claude.com", "updated_at": "2026-11-18T17:14:55.192Z"},
      "components": [],
      "incidents": [
        {"name": "Elevated errors on Opus", "status": "investigating", "impact": "minor", "updated_at": "2026-11-18T17:00:00Z"},
        {"name": "Old outage", "status": "resolved", "impact": "major"},
        {"name": "  ", "status": "identified"}
      ],
      "scheduled_maintenances": [
        {"name": "Database upgrade", "status": "in_progress", "impact": "maintenance"},
        {"name": "Future window", "status": "scheduled", "impact": "maintenance"}
      ],
      "status": {"indicator": "minor", "description": "Partially Degraded Service"}
    }"#;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 18, 12, 0, 0).unwrap()
    }

    fn provider(id: &str, enabled: bool, url: Option<&str>) -> ProviderSummary {
        ProviderSummary {
            id: id.into(),
            plugin_id: None,
            tab_parent: None,
            name: id.into(),
            enabled,
            supported_modes: Vec::new(),
            auto_mode: "auto".into(),
            web_url: None,
            status_page_url: url.map(str::to_string),
            usage_dashboard_url: None,
            icon: None,
        }
    }

    fn healthy() -> PageStatus {
        parse_summary(
            br#"{"page":{},"status":{"indicator":"none","description":"All Systems Operational"}}"#,
        )
        .unwrap()
    }

    #[test]
    fn summary_keeps_only_active_incidents_and_maintenance() {
        let status = parse_summary(SUMMARY.as_bytes()).unwrap();
        assert_eq!(status.indicator, Indicator::Minor);
        assert_eq!(
            status.description.as_deref(),
            Some("Partially Degraded Service")
        );
        let names: Vec<_> = status.incidents.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["Elevated errors on Opus", "Database upgrade"]);
        assert_eq!(status.incidents[0].impact.as_deref(), Some("minor"));
        assert_eq!(
            status.incidents[0].updated_at,
            Some(Utc.with_ymd_and_hms(2026, 11, 18, 17, 0, 0).unwrap())
        );
        assert!(status.updated_at.is_some());
    }

    #[test]
    fn status_json_without_incident_lists_parses() {
        let status = parse_summary(
            br#"{"page":{"updated_at":"not a date"},"status":{"indicator":"maintenance"}}"#,
        )
        .unwrap();
        assert_eq!(status.indicator, Indicator::Maintenance);
        assert!(status.incidents.is_empty() && status.description.is_none());
        assert!(status.updated_at.is_none());
    }

    #[test]
    fn malformed_and_foreign_documents_are_rejected_not_read_as_operational() {
        for (body, error) in [
            (&b"<html>status</html>"[..], "invalid_json"),
            (b"{\"page\":{}", "invalid_json"),
            (b"[]", "not_statuspage"),
            (b"{\"status\":{\"indicator\":\"none\"}}", "not_statuspage"),
            (b"{\"page\":{},\"status\":\"ok\"}", "not_statuspage"),
            (b"{\"page\":{},\"status\":{}}", "unknown_indicator"),
            (
                b"{\"page\":{},\"status\":{\"indicator\":\"degraded\"}}",
                "unknown_indicator",
            ),
            (
                b"{\"page\":{},\"status\":{\"indicator\":7}}",
                "unknown_indicator",
            ),
        ] {
            assert_eq!(
                parse_summary(body),
                Err(error),
                "{}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn incident_lists_are_bounded() {
        let incidents: Vec<_> = (0..25)
            .map(|i| serde_json::json!({"name": format!("{i}{}", "x".repeat(500)), "status": "monitoring"}))
            .collect();
        let body =
            serde_json::json!({"page":{},"status":{"indicator":"major"},"incidents":incidents});
        let status = parse_summary(body.to_string().as_bytes()).unwrap();
        assert_eq!(status.incidents.len(), MAX_INCIDENTS);
        assert!(
            status
                .incidents
                .iter()
                .all(|i| i.name.chars().count() == MAX_TEXT_CHARS)
        );
    }

    #[test]
    fn only_verified_https_statuspage_hosts_are_monitored() {
        assert_eq!(
            statuspage_origin("https://status.anthropic.com/"),
            Some("https://status.claude.com")
        );
        assert_eq!(
            statuspage_origin("https://STATUS.OPENAI.COM/incidents"),
            Some("https://status.openai.com")
        );
        for page in [
            "http://status.openai.com/",
            "https://status.cursor.com/",
            "https://status.cloud.google.com",
            "https://status.openai.com.evil.example/",
            "https://evil.example/?status.openai.com",
            "not a url",
        ] {
            assert_eq!(statuspage_origin(page), None, "{page}");
        }
    }

    #[test]
    fn providers_sharing_a_status_host_cause_one_request() {
        let monitor = StatusMonitor::new(true);
        let providers = [
            provider("codex", true, Some("https://status.openai.com/")),
            provider("openai-api", true, Some("https://status.openai.com")),
            provider("claude", true, Some("https://status.anthropic.com/")),
            provider("cursor", true, Some("https://status.cursor.com/")),
            provider("droid", false, Some("https://status.factory.ai")),
        ];
        assert_eq!(
            monitor.wanted_origins(&providers),
            ["https://status.claude.com", "https://status.openai.com"]
        );
        assert!(
            StatusMonitor::new(false)
                .wanted_origins(&providers)
                .is_empty()
        );
    }

    #[test]
    fn success_waits_the_poll_interval_and_failures_back_off_to_a_cap() {
        assert_eq!(
            (1..=6).map(retry_delay_secs).collect::<Vec<_>>(),
            [300, 600, 1200, 2400, 3600, 3600]
        );
        assert_eq!(retry_delay_secs(u32::MAX), MAX_BACKOFF_SECS);
        let origin = "https://status.openai.com";
        let mut monitor = StatusMonitor::new(true);
        assert_eq!(monitor.due(&[origin], now()), [origin]);
        monitor.record(origin, Ok(healthy()), now());
        let at = |secs| now() + chrono::Duration::seconds(secs);
        assert!(monitor.due(&[origin], at(299)).is_empty());
        assert_eq!(monitor.due(&[origin], at(300)), [origin]);

        monitor.record(origin, Err("timeout".into()), at(300));
        monitor.record(origin, Err("timeout".into()), at(600));
        assert!(monitor.due(&[origin], at(600 + 599)).is_empty());
        assert_eq!(monitor.due(&[origin], at(1200)), [origin]);
        monitor.record(origin, Ok(healthy()), at(1200));
        assert!(monitor.due(&[origin], at(1499)).is_empty());
        assert_eq!(monitor.due(&[origin], at(1500)), [origin]);
    }

    #[test]
    fn report_keeps_last_good_status_as_stale_and_never_invents_one() {
        let providers = [
            provider("codex", true, Some("https://status.openai.com/")),
            provider("claude", true, Some("https://status.claude.com/")),
            provider("droid", true, Some("https://status.factory.ai")),
            provider("cursor", true, Some("https://status.cursor.com/")),
            provider("local", true, None),
            provider("paused", false, Some("https://status.openai.com/")),
        ];
        let mut monitor = StatusMonitor::new(true);
        monitor.record(
            "https://status.openai.com",
            Ok(parse_summary(SUMMARY.as_bytes()).unwrap()),
            now(),
        );
        monitor.record("https://status.openai.com", Err("http_503".into()), now());
        monitor.record("https://status.claude.com", Err("redirect".into()), now());
        let report = serde_json::to_value(monitor.report(&providers)).unwrap();
        let rows = report.as_array().unwrap();
        assert_eq!(rows.len(), 5, "disabled providers are omitted");

        assert_eq!(rows[0]["state"], "stale");
        assert_eq!(rows[0]["indicator"], "minor");
        assert_eq!(rows[0]["error"], "http_503");
        assert_eq!(rows[0]["incidents"][0]["name"], "Elevated errors on Opus");

        assert_eq!(rows[1]["state"], "unavailable");
        assert!(rows[1].get("indicator").is_none());

        assert_eq!(rows[2]["state"], "pending");
        assert_eq!(rows[2]["monitored"], true);

        for row in &rows[3..] {
            assert_eq!(row["monitored"], false);
            assert!(row.get("state").is_none() && row.get("indicator").is_none());
        }
        assert_eq!(rows[3]["statusPageUrl"], "https://status.cursor.com/");
        assert!(rows[4]["statusPageUrl"].is_null());

        let disabled = StatusMonitor::new(false).report(&providers[..1]);
        assert!(!disabled[0].monitored);
    }

    #[test]
    fn unwanted_origins_are_forgotten() {
        let origin = "https://status.openai.com";
        let mut monitor = StatusMonitor::new(true);
        monitor.record(origin, Ok(healthy()), now());
        assert!(monitor.due(&[], now()).is_empty());
        assert_eq!(monitor.due(&[origin], now()), [origin]);
    }

    #[test]
    fn status_check_switch() {
        assert!(checks_enabled(None));
        assert!(checks_enabled(Some("1".into())));
        for value in ["0", "false", " OFF ", "no"] {
            assert!(!checks_enabled(Some(value.into())), "{value}");
        }
    }
}
