use super::local_usage::{LocalUsageEvent, json_u64_value, parse_ts, project_label};
use serde_json::Value;
use std::io::BufRead;

struct Row {
    ordinal: Option<u64>,
    total: Option<[u64; 5]>,
    event: LocalUsageEvent,
}

// Cumulative counters are usable only when their input/output fields are
// present and integral. Missing optional components are zero, not a reset.
fn totals(value: &Value) -> Option<[u64; 5]> {
    let optional = |names: &[&str]| -> Option<u64> {
        names
            .iter()
            .find_map(|name| value.get(name))
            .map_or(Some(0), Value::as_u64)
    };
    Some([
        value
            .get("input_tokens")
            .or_else(|| value.get("inputTokens"))?
            .as_u64()?,
        value
            .get("output_tokens")
            .or_else(|| value.get("outputTokens"))?
            .as_u64()?,
        optional(&[
            "cached_input_tokens",
            "cachedInputTokens",
            "cache_read_input_tokens",
        ])?,
        optional(&["reasoning_output_tokens", "reasoningOutputTokens"])?,
        optional(&[
            "cache_creation_input_tokens",
            "cache_write_tokens",
            "cacheCreationTokens",
        ])?,
    ])
}

pub(super) fn scan(
    reader: impl BufRead,
    fallback_session: String,
    events: &mut Vec<LocalUsageEvent>,
) {
    let mut session_id = fallback_session;
    let mut model = "unknown".to_string();
    let mut project = "unknown".to_string();
    let mut boundary = None;
    let mut rows = Vec::new();
    for line in reader.lines().map_while(Result::ok) {
        // Most rollout bytes are conversation/tool payloads. These cannot
        // affect accounting, and parsing their full JSON dominates cold scans.
        if ![
            "\"session_meta\"",
            "\"turn_context\"",
            "\"token_count\"",
            "\"last_token_usage\"",
            "\"total_token_usage\"",
        ]
        .iter()
        .any(|marker| line.contains(marker))
        {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if boundary.is_none() && value.get("type").and_then(Value::as_str) == Some("session_meta") {
            boundary = value
                .pointer("/payload/subagent_history_start_ordinal")
                .and_then(Value::as_u64);
        }
        if value.get("type").and_then(Value::as_str) == Some("session_meta") {
            if let Some(id) = value.pointer("/payload/id").and_then(Value::as_str) {
                session_id = id.to_string();
            }
        }
        if let Some(name) = value
            .pointer("/payload/model")
            .or_else(|| value.pointer("/payload/model_slug"))
            .and_then(Value::as_str)
        {
            model = name.to_string();
        }
        if let Some(cwd) = value.pointer("/payload/cwd").and_then(Value::as_str) {
            project = project_label(cwd);
        }
        let last = value
            .pointer("/payload/info/last_token_usage")
            .or_else(|| value.pointer("/payload/last_token_usage"));
        let total_value = value
            .pointer("/payload/info/total_token_usage")
            .or_else(|| value.pointer("/payload/total_token_usage"));
        let total = total_value.and_then(totals);
        if last.is_none() && total.is_none() {
            continue;
        }
        let Some(ts) = parse_ts(value.get("timestamp")) else {
            continue;
        };
        let last = last.or(total_value).unwrap_or(&Value::Null);
        rows.push(Row {
            ordinal: value.get("ordinal").and_then(Value::as_u64),
            total,
            event: LocalUsageEvent {
                ts,
                session_id: session_id.clone(),
                project: project.clone(),
                model: model.clone(),
                input_tokens: json_u64_value(last, &["input_tokens", "inputTokens"]),
                output_tokens: json_u64_value(last, &["output_tokens", "outputTokens"]),
                cache_read_tokens: json_u64_value(
                    last,
                    &[
                        "cached_input_tokens",
                        "cachedInputTokens",
                        "cache_read_input_tokens",
                    ],
                ),
                cache_creation_tokens: json_u64_value(
                    last,
                    &[
                        "cache_creation_input_tokens",
                        "cache_write_tokens",
                        "cacheCreationTokens",
                    ],
                ),
                reasoning_output_tokens: json_u64_value(
                    last,
                    &["reasoning_output_tokens", "reasoningOutputTokens"],
                ),
                ..LocalUsageEvent::default()
            },
        });
    }

    // Defer publication so an explicit boundary remains authoritative even if
    // malformed/prefixed input puts metadata after a usage record. A prefix-only
    // child contributes nothing; subsequent scans naturally include appended
    // child-owned rows without consulting another account's parent history.
    let mut previous = None;
    for mut row in rows {
        if let Some(start) = boundary {
            if row.ordinal.is_none_or(|ordinal| ordinal < start) {
                previous = row.total.or(previous);
                continue;
            }
        }
        if let (Some(current), Some(before)) = (row.total, previous) {
            if let Some(delta) = current
                .iter()
                .zip(before)
                .map(|(n, p)| n.checked_sub(p))
                .collect::<Option<Vec<_>>>()
            {
                row.event.input_tokens = delta[0];
                row.event.output_tokens = delta[1];
                row.event.cache_read_tokens = delta[2];
                row.event.reasoning_output_tokens = delta[3];
                row.event.cache_creation_tokens = delta[4];
            }
        }
        // A missing total or counter restart uses last_token_usage. Clear a
        // gap's baseline to avoid including those tokens again later.
        previous = row.total;
        let event = &mut row.event;
        if event.input_tokens == 0
            && event.output_tokens == 0
            && event.cache_read_tokens == 0
            && event.cache_creation_tokens == 0
            && event.reasoning_output_tokens == 0
        {
            continue;
        }
        // Codex reports inclusive input; the shared schema stores exclusive
        // input/cache classes. Reasoning remains a subset of output.
        event.input_tokens = event
            .input_tokens
            .saturating_sub(event.cache_read_tokens)
            .saturating_sub(event.cache_creation_tokens);
        super::pricing::apply(event);
        events.push(row.event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn meta(boundary: Value) -> Value {
        json!({"type":"session_meta","ordinal":0,"payload":{"id":"child","forked_from_id":"parent",
            "subagent_history_start_ordinal":boundary,"source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent"}}}}})
    }

    fn usage(ordinal: Value, total: Option<u64>, last: u64) -> Value {
        json!({"type":"event_msg","ordinal":ordinal,"timestamp":"2026-09-11T00:00:00Z",
            "payload":{"type":"token_count","info":{"total_token_usage":total.map(|input| json!({"input_tokens":input,"output_tokens":0})),
                "last_token_usage":{"input_tokens":last,"output_tokens":0}}}})
    }

    fn parse(rows: &[Value]) -> Vec<LocalUsageEvent> {
        let text = rows
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let mut events = Vec::new();
        scan(std::io::Cursor::new(text), "fallback".into(), &mut events);
        events
    }

    #[test]
    fn explicit_boundary_excludes_inherited_prefix_even_with_delivery_markers() {
        let mut prefix = vec![
            meta(json!(210)),
            usage(json!(2), Some(1000), 1000),
            json!({"type":"inter_agent_communication_metadata","ordinal":11,"payload":{"trigger_turn":true}}),
            usage(json!(19), Some(1050), 50),
            usage(json!(208), Some(1070), 20),
        ];
        assert!(parse(&prefix).is_empty());
        prefix.push(usage(json!(211), Some(1100), 999));
        prefix.push(usage(json!(212), Some(1100), 999));
        let events = parse(&prefix);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].input_tokens, 30);
        assert_eq!(events[0].session_id, "child");
        assert_eq!(parse(&prefix)[0].input_tokens, 30);
    }

    #[test]
    fn explicit_boundary_never_treats_missing_ordinals_as_owned() {
        assert!(parse(&[meta(json!(10)), usage(Value::Null, Some(100), 100)]).is_empty());
        assert!(parse(&[usage(json!(2), Some(100), 100), meta(json!(10))]).is_empty());
    }

    #[test]
    fn ordinary_history_and_invalid_boundaries_keep_last_usage_semantics() {
        for boundary in [
            Value::Null,
            json!(-1),
            json!(1.5),
            json!("10"),
            json!(false),
        ] {
            let events = parse(&[meta(boundary), usage(Value::Null, Some(100), 7)]);
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].input_tokens, 7);
        }
        assert_eq!(
            parse(&[meta(json!(0)), usage(json!(0), Some(100), 7)])[0].input_tokens,
            7
        );
    }

    #[test]
    fn missing_totals_and_counter_restarts_do_not_recount_previous_deltas() {
        let events = parse(&[
            meta(json!(10)),
            usage(json!(1), Some(100), 100),
            usage(json!(10), Some(110), 10),
            usage(json!(11), None, 5),
            usage(json!(12), Some(120), 5),
            usage(json!(13), Some(2), 2),
            usage(json!(14), Some(5), 3),
        ]);
        assert_eq!(
            events
                .iter()
                .map(|event| event.input_tokens)
                .collect::<Vec<_>>(),
            [10, 5, 5, 2, 3]
        );
    }

    #[test]
    fn child_delta_preserves_model_timestamp_cost_and_token_components() {
        let mut before = usage(json!(2), Some(100), 100);
        let mut after = usage(json!(20), Some(140), 40);
        before["payload"]["info"]["total_token_usage"] = json!({"input_tokens":100,"output_tokens":10,"cached_input_tokens":20,"reasoning_output_tokens":1});
        after["payload"]["info"]["total_token_usage"] = json!({"input_tokens":140,"output_tokens":15,"cached_input_tokens":30,"reasoning_output_tokens":3});
        let events = parse(&[
            meta(json!(10)),
            before,
            json!({"type":"turn_context","ordinal":10,"payload":{"model":"gpt-5","cwd":"/fixture/project"}}),
            after,
        ]);
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(
            (
                event.input_tokens,
                event.output_tokens,
                event.cache_read_tokens,
                event.reasoning_output_tokens
            ),
            (30, 5, 10, 2)
        );
        assert_eq!(event.model, "gpt-5");
        assert_eq!(event.ts.to_rfc3339(), "2026-09-11T00:00:00+00:00");
        assert!(event.cost_usd > 0.0);
    }

    #[test]
    fn ordinary_cumulative_duplicates_and_message_ids_do_not_inflate_usage_or_sessions() {
        let mut first = usage(json!(1), Some(100), 100);
        first["payload"]["info"]["last_token_usage"] = json!({"input_tokens":100,"cached_input_tokens":80,"output_tokens":10,"reasoning_output_tokens":4});
        first["payload"]["info"]["total_token_usage"] = json!({"input_tokens":100,"cached_input_tokens":80,"output_tokens":10,"reasoning_output_tokens":4});
        let mut next = first.clone();
        next["timestamp"] = json!("2026-09-12T00:01:00Z");
        next["payload"]["info"]["total_token_usage"] = json!({"input_tokens":150,"cached_input_tokens":120,"output_tokens":20,"reasoning_output_tokens":8});
        let events = parse(&[
            json!({"type":"session_meta","payload":{"id":"real-session"}}),
            json!({"type":"turn_context","payload":{"model":"gpt-6-astra"}}),
            first.clone(),
            first,
            json!({"type":"response_item","payload":{"id":"message-id"}}),
            next,
        ]);
        assert_eq!(events.len(), 2);
        assert!(
            events
                .iter()
                .all(|event| event.session_id == "real-session")
        );
        assert_eq!(events[0].input_tokens, 20);
        assert_eq!(events[0].metrics().total_tokens, 110);
        assert_eq!(events[1].metrics().total_tokens, 60);
        assert!(events.iter().all(|event| event.cost_known));
        let days = super::super::local_usage::aggregate_usage(
            &events,
            super::super::local_usage::Bucket::Day,
        );
        assert_eq!(days.len(), 2);
        assert!(days.iter().all(|day| day.usage.sessions == Some(1)));
    }
}
