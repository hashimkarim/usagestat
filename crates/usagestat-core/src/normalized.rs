use crate::{MetricLine, ProgressFormat, UsageSnapshot};

#[derive(Debug, Clone, Default)]
pub struct NormalizedMetrics {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost: Option<f64>,
    pub reset_time: Option<String>,
    pub primary_percent: f64,
    /// Compact multi-quota summary for list tables (set when 2+ percent progress rows).
    pub list_quota_summary: Option<String>,
}

fn label_and_value<'a>(line: &'a MetricLine) -> Option<(&'a str, &'a str)> {
    match line {
        MetricLine::Text { label, value, .. } => Some((label.as_str(), value.as_str())),
        MetricLine::Badge { label, text, .. } => Some((label.as_str(), text.as_str())),
        MetricLine::Progress { .. } | MetricLine::BarChart { .. } => None,
    }
}

fn truncate_label(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}

fn format_quota_summary(provider_id: &str, percent_lines: &[(String, f64)]) -> String {
    const MAX_PARTS: usize = 6;
    const MAX_LABEL: usize = 18;
    const MAX_TOTAL: usize = 96;

    if provider_id == "mock" {
        return format!("Demo ({} %-rows)", percent_lines.len());
    }

    let take = percent_lines.len().min(MAX_PARTS);
    let segments: Vec<String> = percent_lines
        .iter()
        .take(take)
        .map(|(l, p)| format!("{} {:.0}%", truncate_label(l, MAX_LABEL), p))
        .collect();
    let mut s = segments.join(", ");
    if percent_lines.len() > take {
        s.push_str(&format!(", +{} more", percent_lines.len() - take));
    }
    if s.chars().count() > MAX_TOTAL {
        s = s.chars().take(MAX_TOTAL - 1).collect::<String>() + "…";
    }
    s
}

fn parse_u64_loose(s: &str) -> Option<u64> {
    let d: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if d.is_empty() {
        return None;
    }
    d.parse().ok()
}

fn parse_money(s: &str) -> Option<f64> {
    s.chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect::<String>()
        .parse()
        .ok()
}

impl NormalizedMetrics {
    pub fn from_snapshot(snapshot: &UsageSnapshot) -> Self {
        let mut m = Self::default();
        let mut primary_ratio: Option<f64> = None;
        let mut max_percent: f64 = 0.0;
        let mut percent_lines: Vec<(String, f64)> = Vec::new();
        let mut count_input_sum: u64 = 0;
        let mut count_input_lines: usize = 0;
        let mut count_output_sum: u64 = 0;
        let mut count_output_lines: usize = 0;

        for line in &snapshot.metrics {
            if let MetricLine::Progress {
                used,
                limit,
                format,
                label,
                resets_at,
                ..
            } = line
            {
                if *limit > 0.0 {
                    let r = (*used / *limit).clamp(0.0, 1.0);
                    if primary_ratio.is_none() {
                        primary_ratio = Some(r);
                    }
                }

                match format {
                    ProgressFormat::Percent => {
                        if *limit > 0.0 {
                            let pct = (*used / *limit * 100.0).clamp(0.0, 100.0);
                            if pct > max_percent {
                                max_percent = pct;
                            }
                            percent_lines.push((label.clone(), pct));
                        }
                    }
                    ProgressFormat::Dollars => {
                        if m.cost.is_none() && used.is_finite() && *used >= 0.0 {
                            m.cost = Some(*used);
                        }
                    }
                    ProgressFormat::Count { suffix } => {
                        let suf = suffix.to_lowercase();
                        let lab = label.to_lowercase();
                        let n = if used.is_finite() && *used >= 0.0 {
                            (*used).min(u64::MAX as f64) as u64
                        } else {
                            0
                        };
                        let is_tok = suf.contains("token") || lab.contains("token");
                        let is_req = suf.contains("request") || lab.contains("request");
                        let is_cred = suf.contains("credit") || lab.contains("credit");
                        if is_tok {
                            if lab.contains("output") {
                                count_output_sum = count_output_sum.saturating_add(n);
                                count_output_lines += 1;
                            } else {
                                count_input_sum = count_input_sum.saturating_add(n);
                                count_input_lines += 1;
                            }
                        } else if is_req || is_cred {
                            count_input_sum = count_input_sum.saturating_add(n);
                            count_input_lines += 1;
                        }
                    }
                }

                if let Some(dt) = resets_at {
                    if m.reset_time.is_none() {
                        m.reset_time = Some(dt.to_rfc3339());
                    }
                }

                let lab_lower = label.to_lowercase();
                if lab_lower.contains("input") && lab_lower.contains("token") {
                    m.input_tokens = parse_u64_loose(&used.to_string());
                }
                if lab_lower.contains("output") && lab_lower.contains("token") {
                    m.output_tokens = parse_u64_loose(&used.to_string());
                }
            }

            if let Some((label, value)) = label_and_value(line) {
                let lk = label.to_lowercase();
                let vk = value.to_lowercase();
                let blob = format!("{lk} {vk}");

                if blob.contains("input") && (blob.contains("token") || blob.contains("tok")) {
                    m.input_tokens = m.input_tokens.or_else(|| parse_u64_loose(value));
                }
                if blob.contains("output") && (blob.contains("token") || blob.contains("tok")) {
                    m.output_tokens = m.output_tokens.or_else(|| parse_u64_loose(value));
                }
                if lk.contains("cost") || vk.contains('$') || blob.contains("usd") {
                    m.cost = m.cost.or_else(|| parse_money(value));
                }
                if lk.contains("reset") || vk.contains("reset") || blob.contains("resets") {
                    if m.reset_time.is_none() {
                        m.reset_time = Some(value.to_string());
                    }
                }
            }
        }

        if m.input_tokens.is_none() && count_input_lines > 0 {
            m.input_tokens = Some(count_input_sum);
        }
        if m.output_tokens.is_none() && count_output_lines > 0 {
            m.output_tokens = Some(count_output_sum);
        }

        m.primary_percent = max_percent;
        if m.primary_percent <= 0.0 {
            if let Some(r) = primary_ratio {
                m.primary_percent = r * 100.0;
            }
        }

        if percent_lines.len() >= 2 {
            m.list_quota_summary =
                Some(format_quota_summary(&snapshot.provider_id, &percent_lines));
        }

        m
    }
}
