//! Standard API-equivalent estimates, not subscription invoices. Table updated
//! 2026-09-29 using the model pages and pricing tables linked in
//! docs/history-accounting.md. Unknown model IDs never inherit a family's price.
use super::local_usage::LocalUsageEvent;
use usagestat_core::usage_daily::UsageCostComponents;

pub(super) const AS_OF: &str = "2026-09-29";

#[derive(Clone, Copy)]
struct Rates {
    input: f64,
    output: f64,
    write: f64,
    read: f64,
    long_context: bool,
}

fn rates(model: &str) -> Option<Rates> {
    let model = model.to_ascii_lowercase();
    // Match an exact ID, optionally followed by an ISO/compact snapshot date.
    let matches = |id: &str| {
        model == id
            || model
                .strip_prefix(id)
                .and_then(|s| s.strip_prefix('-'))
                .is_some_and(|suffix| {
                    (suffix.len() == 8 || suffix.len() == 10)
                        && suffix.chars().all(|c| c.is_ascii_digit() || c == '-')
                })
    };
    let (input, output, write, read, long_context) = if matches("gpt-6-astra") {
        (10.0, 50.0, 12.5, 1.0, true)
    } else if matches("gpt-6-sol") {
        (2.0, 10.0, 2.5, 0.2, true)
    } else if matches("gpt-6-luna") {
        (0.1, 0.5, 0.125, 0.01, true)
    } else if matches("gpt-5.6-sol") {
        (4.0, 20.0, 5.0, 0.4, true)
    } else if matches("gpt-5.6-terra") {
        (2.0, 12.0, 2.5, 0.2, true)
    } else if matches("gpt-5.6-luna") {
        (0.2, 1.2, 0.25, 0.02, true)
    } else if matches("gpt-5.5") {
        (5.0, 30.0, 5.0, 0.5, true)
    } else if matches("gpt-5.4") {
        (2.5, 15.0, 2.5, 0.25, true)
    } else if matches("gpt-5.3-codex") {
        (1.75, 14.0, 1.75, 0.175, false)
    } else if matches("gpt-5") {
        (1.25, 10.0, 1.25, 0.125, false)
    } else if matches("claude-opus-5-5") {
        (4.0, 20.0, 5.0, 0.2, false)
    } else if matches("claude-sonnet-5") || matches("claude-sonnet-5-5") {
        (2.0, 10.0, 2.5, 0.2, false)
    } else if [
        "claude-opus-4-5",
        "claude-opus-4-6",
        "claude-opus-4-7",
        "claude-opus-4-8",
        "claude-opus-5",
    ]
    .iter()
    .any(|id| matches(id))
    {
        (5.0, 25.0, 6.25, 0.5, false)
    } else if ["claude-opus-4", "claude-opus-4-1"]
        .iter()
        .any(|id| matches(id))
    {
        (15.0, 75.0, 18.75, 1.5, false)
    } else if ["claude-sonnet-4", "claude-sonnet-4-5", "claude-sonnet-4-6"]
        .iter()
        .any(|id| matches(id))
    {
        (3.0, 15.0, 3.75, 0.3, false)
    } else if matches("claude-haiku-4-5") {
        (1.0, 5.0, 1.25, 0.1, false)
    } else if matches("claude-3-5-haiku") {
        (0.8, 4.0, 1.0, 0.08, false)
    } else {
        return None;
    };
    Some(Rates {
        input,
        output,
        write,
        read,
        long_context,
    })
}

pub(super) fn apply(event: &mut LocalUsageEvent) {
    let Some(mut rates) = rates(&event.model) else {
        event.cost_usd = 0.0;
        event.cost_components = UsageCostComponents::default();
        event.cost_known = false;
        event.cache_savings_usd = None;
        return;
    };
    let input = event
        .input_tokens
        .saturating_add(event.cache_read_tokens)
        .saturating_add(event.cache_creation_tokens);
    if rates.long_context && input > 272_000 {
        rates.input *= 2.0;
        rates.write *= 2.0;
        rates.read *= 2.0;
        rates.output *= 1.5;
    }
    let input = event.input_tokens as f64 * rates.input / 1_000_000.0;
    let read = event.cache_read_tokens as f64 * rates.read / 1_000_000.0;
    let write = event.cache_creation_tokens as f64 * rates.write / 1_000_000.0;
    let output = event.output_tokens as f64 * rates.output / 1_000_000.0;
    event.cost_usd = input + read + write + output;
    event.cost_known = event.cost_usd.is_finite();
    event.cost_components = if event.cost_known {
        UsageCostComponents {
            input_cost_usd: Some(input),
            cache_read_cost_usd: Some(read),
            cache_write_cost_usd: Some(write),
            output_cost_usd: Some(output),
        }
    } else {
        UsageCostComponents::default()
    };
    // Read discount only. Cache-write premiums are included in cost_usd.
    event.cache_savings_usd =
        Some(event.cache_read_tokens as f64 * (rates.input - rates.read) / 1_000_000.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sonnet_55_has_explicit_rates_without_pricing_unknown_variants() {
        for model in ["claude-sonnet-5-5", "claude-sonnet-5-5-20260928"] {
            let mut event = LocalUsageEvent {
                model: model.into(),
                input_tokens: 1_000_000,
                output_tokens: 1_000_000,
                cache_read_tokens: 1_000_000,
                cache_creation_tokens: 1_000_000,
                ..Default::default()
            };
            apply(&mut event);
            assert!(event.cost_known);
            assert!((event.cost_usd - 14.7).abs() < 1e-9);
            assert_eq!(event.cost_components.cache_read_cost_usd, Some(0.2));
            assert_eq!(event.cost_components.cache_write_cost_usd, Some(2.5));
            event.model = "claude-sonnet-5-5-future".into();
            apply(&mut event);
            assert!(!event.cost_known);
        }
    }

    #[test]
    fn astra_is_priced_unknowns_are_not_guessed_and_long_context_is_per_request() {
        let mut event = LocalUsageEvent {
            model: "gpt-6-astra".into(),
            input_tokens: 100_000,
            cache_read_tokens: 100_000,
            output_tokens: 1_000,
            ..Default::default()
        };
        apply(&mut event);
        assert!((event.cost_usd - 1.15).abs() < 1e-9);
        assert_eq!(event.cache_savings_usd, Some(0.9));
        assert_eq!(event.cost_components.input_cost_usd, Some(1.0));
        assert_eq!(event.cost_components.cache_read_cost_usd, Some(0.1));
        assert_eq!(event.cost_components.cache_write_cost_usd, Some(0.0));
        assert_eq!(event.cost_components.output_cost_usd, Some(0.05));
        event.input_tokens = 200_000;
        apply(&mut event);
        assert!((event.cost_usd - 4.275).abs() < 1e-9);
        assert_eq!(event.cost_components.input_cost_usd, Some(4.0));
        assert_eq!(event.cost_components.cache_read_cost_usd, Some(0.2));
        assert_eq!(event.cost_components.output_cost_usd, Some(0.075));
        for model in ["unknown", "gpt-5-future", "some-codex-model"] {
            event.model = model.into();
            apply(&mut event);
            assert!(!event.cost_known);
            assert_eq!(event.cache_savings_usd, None);
            assert_eq!(event.cost_components, UsageCostComponents::default());
        }
    }

    #[test]
    fn cache_writes_and_reasoning_are_priced_once_before_aggregation() {
        let mut event = LocalUsageEvent {
            model: "gpt-6-astra".into(),
            input_tokens: 100_000,
            cache_read_tokens: 100_000,
            cache_creation_tokens: 2_000,
            output_tokens: 1_000,
            reasoning_output_tokens: 500,
            ..Default::default()
        };
        apply(&mut event);
        assert_eq!(event.cost_components.cache_write_cost_usd, Some(0.025));
        assert_eq!(event.cost_components.output_cost_usd, Some(0.05));
        assert!((event.cost_usd - 1.175).abs() < 1e-9);
        let mut total = event.metrics();
        event.input_tokens = 200_000;
        apply(&mut event);
        assert_eq!(event.cost_components.cache_write_cost_usd, Some(0.05));
        assert!((event.cost_usd - 4.325).abs() < 1e-9);
        total.add(&event.metrics());
        assert!((total.cost_usd - 5.5).abs() < 1e-9);
        let c = total.cost_components;
        let sum = c.input_cost_usd.unwrap()
            + c.cache_read_cost_usd.unwrap()
            + c.cache_write_cost_usd.unwrap()
            + c.output_cost_usd.unwrap();
        assert!((sum - total.cost_usd).abs() < 1e-9);
    }
}
