//! Standard API-equivalent estimates, not subscription invoices. Table updated
//! 2026-11-16 (historical Codex and Haiku coverage) using the model pages and pricing tables linked in
//! docs/history-accounting.md. Unknown model IDs never inherit a family's price.
use super::local_usage::LocalUsageEvent;
use usagestat_core::usage_daily::UsageCostComponents;

pub(super) const AS_OF: &str = "2026-11-16";

#[derive(Clone, Copy)]
struct Rates {
    input: f64,
    output: f64,
    write: f64,
    read: f64,
    long_context: bool,
    haiku_tier: bool,
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
    } else if matches("gpt-6.1-sol") {
        (2.0, 10.0, 2.5, 0.1, true)
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
    } else if matches("gpt-5.4-mini") {
        (0.75, 4.5, 0.75, 0.075, false)
    } else if matches("gpt-5.3-codex") || matches("gpt-5.2") {
        (1.75, 14.0, 1.75, 0.175, false)
    } else if matches("gpt-5") || matches("gpt-5-codex") {
        (1.25, 10.0, 1.25, 0.125, false)
    } else if matches("claude-opus-5-5") {
        (4.0, 20.0, 5.0, 0.2, false)
    } else if matches("claude-haiku-5-5") {
        (0.1, 0.5, 0.125, 0.01, false)
    } else if matches("claude-sonnet-5-5") {
        (2.0, 10.0, 2.5, 0.1, false)
    } else if matches("claude-sonnet-5") {
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
        haiku_tier: matches("claude-haiku-5-5"),
    })
}

// Compacted Claude rows must be replayed for tiered request pricing. Codex
// retains request counters, so its new rates can be applied without log replay.
pub(super) fn needs_reprice(event: &LocalUsageEvent) -> bool {
    rates(&event.model).is_some_and(|r| {
        !event.cost_known
            || (!r.haiku_tier
                && event
                    .model
                    .to_ascii_lowercase()
                    .starts_with("claude-sonnet-5-5"))
    })
}

pub(super) fn apply(event: &mut LocalUsageEvent) {
    price(event, true);
}

fn price(event: &mut LocalUsageEvent, request_context: bool) {
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
    if request_context && rates.long_context && input > 272_000 {
        rates.input *= 2.0;
        rates.write *= 2.0;
        rates.read *= 2.0;
        rates.output *= 1.5;
    }
    if request_context && rates.haiku_tier && input > 100_000 {
        rates.input *= 5.0;
        rates.read *= 5.0;
        rates.write *= 5.0;
        rates.output *= 5.0;
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

// Retained daily counters have no request boundaries. Use base rates explicitly,
// never treat a whole day's input as one long-context request.
pub(super) fn backfill_daily(row: &mut usagestat_core::usage_daily::UsageDailyRow) -> bool {
    let reported = (row.source == "ccusage").then_some((
        row.cost_usd,
        row.cost_known,
        row.cost_components,
        row.cache_savings_usd,
    ));
    let mut changed = false;
    for (name, model) in &mut row.models {
        if model.cost_known
            || !model.tokens_known
            || (row.source == "ccusage" && model.cost_usd != 0.0)
        {
            continue;
        }
        let mut event = LocalUsageEvent {
            model: name.clone(),
            input_tokens: model.input_tokens,
            output_tokens: model.output_tokens,
            cache_read_tokens: model.cache_read_tokens,
            cache_creation_tokens: model.cache_creation_tokens,
            ..Default::default()
        };
        price(&mut event, false);
        if !event.cost_known {
            continue;
        }
        row.cost_usd += event.cost_usd - model.cost_usd;
        model.cost_usd = event.cost_usd;
        model.cost_known = true;
        model.cost_components = event.cost_components;
        model.cache_savings_usd = event.cache_savings_usd;
        changed = true;
    }
    if changed {
        let tokens = row
            .models
            .values()
            .try_fold(0u64, |n, m| n.checked_add(m.total_tokens));
        row.cost_known =
            tokens == Some(row.total_tokens) && row.models.values().all(|m| m.cost_known);
        row.cost_components = UsageCostComponents::zero();
        row.cache_savings_usd = Some(0.0);
        for model in row.models.values() {
            row.cost_components.add(&model.cost_components);
            row.cache_savings_usd = row
                .cache_savings_usd
                .zip(model.cache_savings_usd)
                .map(|(a, b)| a + b);
        }
        row.pricing_as_of = Some(AS_OF.into());
        row.cost_source = Some("api-rate-aggregate-estimate".into());
        if let Some((cost, known, components, savings)) = reported {
            row.cost_usd = cost;
            row.cost_known = known;
            row.cost_components = components;
            row.cache_savings_usd = savings;
            row.cost_source = Some("reported-with-api-model-estimates".into());
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haiku_55_prompt_tier_counts_all_input_classes_but_not_output() {
        let mut event = LocalUsageEvent {
            model: "claude-haiku-5-5".into(),
            input_tokens: 10_000,
            cache_read_tokens: 80_000,
            cache_creation_tokens: 10_000,
            output_tokens: 200_000,
            ..Default::default()
        };
        apply(&mut event);
        assert!(event.cost_known);
        assert!((event.cost_usd - 0.10305).abs() < 1e-10);
        event.input_tokens += 1;
        apply(&mut event);
        assert!((event.cost_usd - 0.5152505).abs() < 1e-10);
        assert!((event.cache_savings_usd.unwrap() - 0.036).abs() < 1e-10);
    }

    #[test]
    fn historical_codex_ids_have_verified_rates_but_spark_has_no_api_price() {
        for (model, expected) in [
            ("gpt-5-codex", 11.375),
            ("gpt-5.2", 15.925),
            ("gpt-5.4-mini", 5.325),
        ] {
            let mut event = LocalUsageEvent {
                model: model.into(),
                input_tokens: 1_000_000,
                output_tokens: 1_000_000,
                cache_read_tokens: 1_000_000,
                ..Default::default()
            };
            apply(&mut event);
            assert!(event.cost_known, "{model}");
            assert!((event.cost_usd - expected).abs() < 1e-10, "{model}");
        }
        let mut event = LocalUsageEvent {
            model: "gpt-5.3-codex-spark".into(),
            input_tokens: 1000,
            ..Default::default()
        };
        apply(&mut event);
        assert!(!event.cost_known);
        assert_eq!(event.cache_savings_usd, None);
    }

    #[test]
    fn ccusage_model_backfill_preserves_reported_daily_cost() {
        let mut row: usagestat_core::usage_daily::UsageDailyRow=serde_json::from_value(serde_json::json!({
            "providerId":"codex","displayName":"Codex","date":"2026-11-07","source":"ccusage",
            "inputTokens":1177,"outputTokens":54,"cacheReadTokens":201344,"cacheCreationTokens":0,
            "reasoningOutputTokens":0,"totalTokens":202575,"costUsd":42,"costKnown":true,"ingestedAt":"old",
            "models":{"gpt-6.1-sol":{"inputTokens":1177,"outputTokens":54,"cacheReadTokens":201344,
            "cacheCreationTokens":0,"reasoningOutputTokens":0,"totalTokens":202575,"costUsd":0,"costKnown":false,"tokensKnown":true}}
        })).unwrap();
        row.models.insert(
            "gpt-6-sol".into(),
            usagestat_core::usage_daily::UsageModelDaily {
                cost_usd: -1.0,
                cost_known: false,
                input_tokens: 1000,
                total_tokens: 1000,
                ..Default::default()
            },
        );
        assert!(backfill_daily(&mut row));
        assert_eq!(row.models["gpt-6-sol"].cost_usd, -1.0);
        assert!(!row.models["gpt-6-sol"].cost_known);
        assert_eq!(row.cost_usd, 42.0);
        assert!(row.cost_known);
        let model = &row.models["gpt-6.1-sol"];
        assert!(model.cost_known);
        assert!((model.cost_usd - 0.0230284).abs() < 1e-9);
        assert_eq!(
            row.cost_source.as_deref(),
            Some("reported-with-api-model-estimates")
        );
        assert!(!backfill_daily(&mut row));
    }

    #[test]
    fn retained_daily_usage_gets_base_rate_backfill_without_request_context() {
        let mut row: usagestat_core::usage_daily::UsageDailyRow = serde_json::from_value(serde_json::json!({
            "providerId":"codex","displayName":"Codex","date":"2026-10-01","source":"local-transcript-v2",
            "inputTokens":1000000,"outputTokens":0,"cacheReadTokens":1000000,"cacheCreationTokens":0,
            "reasoningOutputTokens":0,"totalTokens":2000000,"costUsd":0,"costKnown":false,"ingestedAt":"old",
            "models":{"gpt-6.1-sol":{"inputTokens":1000000,"outputTokens":0,"cacheReadTokens":1000000,
            "cacheCreationTokens":0,"reasoningOutputTokens":0,"totalTokens":2000000,"costUsd":0,"costKnown":false,"tokensKnown":true}}
        })).unwrap();
        assert!(backfill_daily(&mut row));
        assert!(row.cost_known);
        assert!((row.cost_usd - 2.1).abs() < 1e-9);
        assert_eq!(
            row.cost_source.as_deref(),
            Some("api-rate-aggregate-estimate")
        );
        assert!(!backfill_daily(&mut row));
    }

    #[test]
    fn sol_61_prices_requests_and_preserves_previous_sol_cache_rates() {
        for model in ["gpt-6.1-sol", "gpt-6.1-sol-20260929"] {
            let mut event = LocalUsageEvent {
                model: model.into(),
                input_tokens: 170_000,
                cache_read_tokens: 100_000,
                cache_creation_tokens: 2_000,
                output_tokens: 1_000,
                reasoning_output_tokens: 500,
                ..Default::default()
            };
            apply(&mut event);
            assert!(event.cost_known);
            assert!((event.cost_usd - 0.365).abs() < 1e-9);
            assert_eq!(event.cost_components.cache_read_cost_usd, Some(0.01));
            assert_eq!(event.cost_components.cache_write_cost_usd, Some(0.005));
            assert_eq!(event.cost_components.output_cost_usd, Some(0.01));
            event.input_tokens += 1;
            apply(&mut event);
            assert!((event.cost_usd - 0.725004).abs() < 1e-9);
            assert_eq!(event.cost_components.cache_read_cost_usd, Some(0.02));
            assert_eq!(event.cost_components.output_cost_usd, Some(0.015));
            event.model = "gpt-6-sol".into();
            apply(&mut event);
            assert_eq!(event.cost_components.cache_read_cost_usd, Some(0.04));
            event.model = "gpt-6.1-sol-future".into();
            apply(&mut event);
            assert!(!event.cost_known);
        }
    }

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
            assert!((event.cost_usd - 14.6).abs() < 1e-9);
            assert_eq!(event.cost_components.cache_read_cost_usd, Some(0.1));
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
