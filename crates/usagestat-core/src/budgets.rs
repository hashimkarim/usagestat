//! Optional monthly API-equivalent budgets for local agent usage, per project
//! or overall. They do nothing until a hook subscribes to `budget_threshold`.

use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;

/// Local log sources a budget can count.
pub const SOURCES: [&str; 4] = ["claude", "codex", "gemini", "amp"];

/// One `[[budgets]]` entry. Parsing is lenient like hooks: a malformed entry is
/// kept as an invalid budget so it never breaks the rest of the configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct BudgetConfig {
    /// Project name as `usagestat project` shows it. Empty counts all usage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// API-equivalent USD per UTC calendar month.
    #[serde(default)]
    pub monthly_usd: f64,
    /// Local log sources to count. Empty counts every source.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    #[serde(skip)]
    pub parse_error: Option<String>,
}

impl BudgetConfig {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(error) = &self.parse_error {
            return Err(error.clone());
        }
        if !(self.monthly_usd.is_finite() && self.monthly_usd > 0.0) {
            return Err("monthlyUsd must be a positive number".into());
        }
        if let Some(unknown) = self.sources.iter().find(|s| !SOURCES.contains(&s.as_str())) {
            return Err(format!(
                "unknown source `{unknown}`; use claude, codex, gemini or amp"
            ));
        }
        if self.project.as_deref().is_some_and(str::is_empty) {
            return Err("project must not be empty; leave it out to count all usage".into());
        }
        Ok(())
    }

    pub fn counts(&self, source: &str) -> bool {
        self.sources.is_empty() || self.sources.iter().any(|s| s == source)
    }

    /// Stable identity for tracking state across config reloads.
    pub fn key(&self) -> String {
        format!(
            "{}|{}|{}",
            self.project.as_deref().unwrap_or("*"),
            self.monthly_usd,
            self.sources.join(",")
        )
    }

    pub fn label(&self) -> String {
        match &self.project {
            Some(project) => project.clone(),
            None => "all local usage".into(),
        }
    }
}

pub(crate) fn deserialize_budgets<'de, D>(deserializer: D) -> Result<Vec<BudgetConfig>, D::Error>
where
    D: Deserializer<'de>,
{
    let entries = Vec::<toml::Value>::deserialize(deserializer)?;
    Ok(entries
        .into_iter()
        .map(|entry| {
            entry.try_into::<BudgetConfig>().unwrap_or_else(|_| BudgetConfig {
                parse_error: Some(
                    "invalid budget entry: use project (text), monthlyUsd (number) and sources (array of text)"
                        .into(),
                ),
                ..BudgetConfig::default()
            })
        })
        .collect())
}

/// Per-hook crossing state: the highest threshold already reported for each
/// budget in its current month.
#[derive(Debug, Clone, Default)]
pub struct BudgetTracker {
    levels: HashMap<String, (String, f64)>,
}

impl BudgetTracker {
    /// Record a budget's month-to-date spend and return the threshold it newly
    /// crossed, if any. The first reading after start only sets a baseline; a
    /// new month starts from zero.
    pub fn observe(
        &mut self,
        budget: &BudgetConfig,
        month: &str,
        spent: f64,
        thresholds: &[f64],
    ) -> Option<f64> {
        let used = spent / budget.monthly_usd * 100.0;
        let level = thresholds
            .iter()
            .copied()
            .filter(|t| *t <= used)
            .fold(f64::NEG_INFINITY, f64::max);
        let Some((seen_month, seen)) = self.levels.get_mut(&budget.key()) else {
            self.levels.insert(budget.key(), (month.to_string(), level));
            return None;
        };
        if seen_month != month {
            *seen_month = month.to_string();
            *seen = f64::NEG_INFINITY;
        }
        (level > *seen).then(|| {
            *seen = level;
            level
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget() -> BudgetConfig {
        BudgetConfig {
            project: Some("app".into()),
            monthly_usd: 100.0,
            ..BudgetConfig::default()
        }
    }

    #[test]
    fn crossings_fire_once_per_month_after_a_baseline() {
        let thresholds = [75.0, 90.0, 100.0];
        let mut tracker = BudgetTracker::default();
        let b = budget();
        assert_eq!(tracker.observe(&b, "2026-11", 80.0, &thresholds), None);
        assert_eq!(tracker.observe(&b, "2026-11", 85.0, &thresholds), None);
        assert_eq!(
            tracker.observe(&b, "2026-11", 95.0, &thresholds),
            Some(90.0)
        );
        assert_eq!(tracker.observe(&b, "2026-11", 96.0, &thresholds), None);
        assert_eq!(tracker.observe(&b, "2026-12", 1.0, &thresholds), None);
        assert_eq!(
            tracker.observe(&b, "2026-12", 120.0, &thresholds),
            Some(100.0)
        );
    }

    #[test]
    fn entries_parse_leniently_and_validate() {
        let config: crate::AppConfig = toml::from_str(
            r#"
[[budgets]]
project = "app"
monthlyUsd = 50
sources = ["claude"]

[[budgets]]
monthlyUsd = "lots"

[[budgets]]
monthlyUsd = 10
sources = ["cursor"]
"#,
        )
        .unwrap();
        assert_eq!(config.budgets.len(), 3);
        assert!(config.budgets[0].validate().is_ok());
        assert!(config.budgets[0].counts("claude") && !config.budgets[0].counts("codex"));
        assert!(config.budgets[1].validate().is_err());
        assert!(config.budgets[2].validate().unwrap_err().contains("cursor"));
    }
}
