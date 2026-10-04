use super::local_usage::{LocalUsageEvent, json_u64_value, parse_ts, project_label};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::BufRead;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Identity {
    response_id: Option<String>,
    row_key: String,
    mirrors: Vec<String>,
}

struct Request {
    thread: String,
    session: Option<String>,
    response: String,
    turn_total: Option<[u64; 5]>,
}

struct Row {
    ordinal: Option<u64>,
    total: Option<[u64; 5]>,
    event: LocalUsageEvent,
    usage: [u64; 5],
    turn: Option<String>,
    request: Option<Request>,
    has_last: bool,
}

fn nonempty(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn valid_usage(value: &Value) -> Option<[u64; 5]> {
    let usage = totals(value)?;
    usage[2]
        .checked_add(usage[4])
        .filter(|cache| *cache <= usage[0])?;
    usage[0].checked_add(usage[1])?;
    (usage[3] <= usage[1]).then_some(usage)
}

fn mirror(
    turn: &Option<String>,
    usage: [u64; 5],
    total: Option<[u64; 5]>,
    ts: Option<i64>,
) -> String {
    serde_json::to_string(&(turn, usage, total, ts)).unwrap()
}

// Reconcile archived copies and mirrors across files before report aggregation.
// A response identity is scoped to its owning thread; equal-sized requests alone
// never establish an alias. Ambiguous aliases leave legacy rows intact.
pub(super) fn unique<'a>(events: &[&'a LocalUsageEvent]) -> Vec<&'a LocalUsageEvent> {
    let mut aliases = HashMap::new();
    for event in events {
        if let Some(identity) = &event.codex_identity {
            if let Some(response) = &identity.response_id {
                for key in &identity.mirrors {
                    let entry = aliases
                        .entry((event.session_id.as_str(), key.as_str()))
                        .or_insert(Some(response.as_str()));
                    if *entry != Some(response.as_str()) {
                        *entry = None;
                    }
                }
            }
        }
    }
    let mut seen = HashSet::new();
    events
        .iter()
        .filter_map(|event| {
            let Some(identity) = &event.codex_identity else {
                return Some(*event);
            };
            if identity.response_id.is_none()
                && identity.mirrors.iter().any(|key| {
                    aliases
                        .get(&(event.session_id.as_str(), key.as_str()))
                        .is_some_and(Option::is_some)
                })
            {
                return None;
            }
            let key = identity
                .response_id
                .as_deref()
                .map(|id| (true, id))
                .unwrap_or((false, identity.row_key.as_str()));
            seen.insert((event.session_id.as_str(), key))
                .then_some(*event)
        })
        .collect()
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
    let mut execution_session = None;
    let mut captured_metadata = false;
    let mut turn = None;
    let mut turn_models = HashMap::new();
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
            "\"token_usage_record\"",
        ]
        .iter()
        .any(|marker| line.contains(marker))
        {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let is_request = value.get("type").and_then(Value::as_str) == Some("token_usage_record");
        if !captured_metadata && value.get("type").and_then(Value::as_str) == Some("session_meta") {
            captured_metadata = true;
            boundary = value
                .pointer("/payload/subagent_history_start_ordinal")
                .and_then(Value::as_u64);
            if let Some(id) = nonempty(value.pointer("/payload/id")) {
                session_id = id;
            }
            execution_session = nonempty(value.pointer("/payload/session_id"));
        }
        if !is_request {
            if let Some(name) = value
                .pointer("/payload/model")
                .or_else(|| value.pointer("/payload/model_slug"))
                .and_then(Value::as_str)
            {
                model = if name.trim().is_empty() {
                    "unknown".into()
                } else {
                    name.to_string()
                };
            }
        }
        if !is_request {
            if let Some(id) = nonempty(value.pointer("/payload/turn_id")) {
                turn = Some(id);
            }
        }
        if value.get("type").and_then(Value::as_str) == Some("turn_context") {
            if let Some(turn) = &turn {
                turn_models.insert(turn.clone(), model.clone());
            }
        }
        if !is_request {
            if let Some(cwd) = value.pointer("/payload/cwd").and_then(Value::as_str) {
                project = project_label(cwd);
            }
        }
        let last = value
            .pointer("/payload/info/last_token_usage")
            .or_else(|| value.pointer("/payload/last_token_usage"));
        let total_value = value
            .pointer("/payload/info/total_token_usage")
            .or_else(|| value.pointer("/payload/total_token_usage"));
        let total = total_value.and_then(totals);
        if is_request {
            let payload = &value["payload"];
            let Some(usage) = valid_usage(&payload["usage"]) else {
                continue;
            };
            let Some(total) = valid_usage(&payload["thread_token_usage"]) else {
                continue;
            };
            if usage.iter().zip(total).any(|(used, total)| *used > total) {
                continue;
            }
            let Some(thread) = nonempty(payload.get("thread_id")) else {
                continue;
            };
            let Some(response) = nonempty(payload.get("response_id")) else {
                continue;
            };
            let session = nonempty(payload.get("session_id"));
            if payload.get("session_id").is_some() && session.is_none() {
                continue;
            }
            let Some(ts) = parse_ts(value.get("timestamp")) else {
                continue;
            };
            let request_turn = nonempty(payload.get("turn_id")).or_else(|| turn.clone());
            let request_model = nonempty(payload.get("model"))
                .or_else(|| {
                    request_turn
                        .as_ref()
                        .and_then(|id| turn_models.get(id).cloned())
                })
                .unwrap_or_else(|| {
                    if request_turn == turn {
                        model.clone()
                    } else {
                        "unknown".into()
                    }
                });
            rows.push(Row {
                ordinal: value.get("ordinal").and_then(Value::as_u64),
                total: Some(total),
                usage,
                turn: request_turn,
                request: Some(Request {
                    thread,
                    session,
                    response,
                    turn_total: valid_usage(&payload["turn_token_usage"])
                        .filter(|total| usage.iter().zip(*total).all(|(u, t)| *u <= t)),
                }),
                has_last: true,
                event: LocalUsageEvent {
                    ts,
                    project: project.clone(),
                    model: request_model,
                    ..Default::default()
                },
            });
            continue;
        }
        if last.is_none() && total.is_none() {
            continue;
        }
        let Some(ts) = parse_ts(value.get("timestamp")) else {
            continue;
        };
        let has_last = last.is_some_and(Value::is_object);
        let last = last.or(total_value).unwrap_or(&Value::Null);
        let usage = [
            json_u64_value(last, &["input_tokens", "inputTokens"]),
            json_u64_value(last, &["output_tokens", "outputTokens"]),
            json_u64_value(
                last,
                &[
                    "cached_input_tokens",
                    "cachedInputTokens",
                    "cache_read_input_tokens",
                ],
            ),
            json_u64_value(last, &["reasoning_output_tokens", "reasoningOutputTokens"]),
            json_u64_value(
                last,
                &[
                    "cache_creation_input_tokens",
                    "cache_write_tokens",
                    "cacheCreationTokens",
                ],
            ),
        ];
        rows.push(Row {
            ordinal: value.get("ordinal").and_then(Value::as_u64),
            total,
            usage,
            turn: turn.clone(),
            request: None,
            has_last,
            event: LocalUsageEvent {
                ts,
                session_id: session_id.clone(),
                project: project.clone(),
                model: model.clone(),
                input_tokens: usage[0],
                output_tokens: usage[1],
                cache_read_tokens: usage[2],
                reasoning_output_tokens: usage[3],
                cache_creation_tokens: usage[4],
                ..LocalUsageEvent::default()
            },
        });
    }

    // Defer publication so an explicit boundary remains authoritative even if
    // malformed/prefixed input puts metadata after a usage record. A prefix-only
    // child contributes nothing; subsequent scans naturally include appended
    // child-owned rows without consulting another account's parent history.
    let mut previous = None;
    let mut accepted = Vec::new();
    let mut pending: Option<(bool, Vec<String>, usize)> = None;
    for (index, mut row) in rows.into_iter().enumerate() {
        if let Some(start) = boundary {
            if row.ordinal.is_none_or(|ordinal| ordinal < start) {
                if row.request.is_none() {
                    previous = row.total.or(previous);
                }
                pending = None;
                continue;
            }
        }
        if let Some(request) = &row.request {
            if request.thread != session_id
                || request
                    .session
                    .as_ref()
                    .is_some_and(|id| id != execution_session.as_ref().unwrap_or(&session_id))
            {
                pending = None;
                continue;
            }
        } else if let (Some(current), Some(before)) = (row.total, previous) {
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
        if row.request.is_none() {
            previous = row.total;
        } else {
            row.event.input_tokens = row.usage[0];
            row.event.output_tokens = row.usage[1];
            row.event.cache_read_tokens = row.usage[2];
            row.event.reasoning_output_tokens = row.usage[3];
            row.event.cache_creation_tokens = row.usage[4];
        }
        row.event.session_id = session_id.clone();
        let ts = row.event.ts.timestamp_millis();
        let mut mirrors = vec![
            mirror(&row.turn, row.usage, row.total, Some(ts)),
            mirror(&row.turn, row.usage, None, Some(ts)),
        ];
        if !row.has_last {
            let derived = [
                row.event.input_tokens,
                row.event.output_tokens,
                row.event.cache_read_tokens,
                row.event.reasoning_output_tokens,
                row.event.cache_creation_tokens,
            ];
            mirrors.push(mirror(&row.turn, derived, row.total, Some(ts)));
            mirrors.push(mirror(&row.turn, derived, None, Some(ts)));
        }
        let adjacent = vec![mirror(&row.turn, row.usage, row.total, None)];
        if let Some(total) = row.request.as_ref().and_then(|r| r.turn_total) {
            mirrors.push(mirror(&row.turn, row.usage, Some(total), Some(ts)));
        }
        // Only neighbouring usage observations may alias a counter without time.
        let is_request = row.request.is_some();
        let mut matched_adjacent = false;
        if let Some((was_request, keys, before)) = &pending {
            if *was_request != is_request && adjacent.iter().any(|key| keys.contains(key)) {
                matched_adjacent = true;
                let before: &mut LocalUsageEvent = &mut accepted[*before];
                if is_request {
                    mirrors.extend(before.codex_identity.as_ref().unwrap().mirrors.clone());
                } else {
                    before
                        .codex_identity
                        .as_mut()
                        .unwrap()
                        .mirrors
                        .extend(mirrors.clone());
                }
            }
        }
        row.event.codex_identity = Some(Identity {
            response_id: row.request.map(|r| r.response),
            row_key: serde_json::to_string(&(
                index,
                row.ordinal,
                ts,
                &row.turn,
                row.usage,
                row.total,
                &row.event.model,
            ))
            .unwrap(),
            mirrors,
        });
        let event = &mut row.event;
        if event.input_tokens == 0
            && event.output_tokens == 0
            && event.cache_read_tokens == 0
            && event.cache_creation_tokens == 0
            && event.reasoning_output_tokens == 0
        {
            pending = None;
            continue;
        }
        // Codex reports inclusive input; the shared schema stores exclusive
        // input/cache classes. Reasoning remains a subset of output.
        event.input_tokens = event
            .input_tokens
            .saturating_sub(event.cache_read_tokens)
            .saturating_sub(event.cache_creation_tokens);
        super::pricing::apply(event);
        pending = if matched_adjacent {
            None
        } else {
            Some((is_request, adjacent, accepted.len()))
        };
        accepted.push(row.event);
    }
    events.extend(
        unique(&accepted.iter().collect::<Vec<_>>())
            .into_iter()
            .cloned(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;
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

    fn record(id: &str, used: u64, total: u64) -> Value {
        json!({"type":"token_usage_record","timestamp":"2026-09-11T00:00:00Z","payload":{
            "thread_id":"child","session_id":"child","response_id":id,"turn_id":"turn",
            "usage":{"input_tokens":used,"output_tokens":10,"cached_input_tokens":2,"reasoning_output_tokens":3},
            "thread_token_usage":{"input_tokens":total,"output_tokens":20,"cached_input_tokens":4,"reasoning_output_tokens":6},
            "turn_token_usage":{"input_tokens":used,"output_tokens":10,"cached_input_tokens":2,"reasoning_output_tokens":3}}})
    }

    #[test]
    fn request_ledger_reconciles_both_mirror_orders_and_counter_domains() {
        for legacy_first in [false, true] {
            let mut rows = vec![
                meta(Value::Null),
                json!({"type":"turn_context","payload":{"turn_id":"turn","model":"gpt-5"}}),
            ];
            for (id, used, total, day) in [
                ("one", 100, 100, 11),
                ("two", 60, 160, 12),
                ("three", 60, 220, 13),
            ] {
                let mut ledger = record(id, used, total);
                ledger["timestamp"] = json!(format!("2026-09-{day}T00:00:00Z"));
                let mut legacy = usage(Value::Null, Some(used), used);
                legacy["timestamp"] = ledger["timestamp"].clone();
                legacy["payload"]["turn_id"] = json!("turn");
                legacy["payload"]["info"]["last_token_usage"] = ledger["payload"]["usage"].clone();
                legacy["payload"]["info"]["total_token_usage"] =
                    ledger["payload"]["turn_token_usage"].clone();
                rows.extend(if legacy_first {
                    [legacy, ledger]
                } else {
                    [ledger, legacy]
                });
            }
            let events = parse(&rows);
            assert_eq!(events.len(), 3);
            assert_eq!(
                events.iter().map(|e| e.metrics().total_tokens).sum::<u64>(),
                250
            );
            assert_eq!(
                events
                    .iter()
                    .map(|e| e.reasoning_output_tokens)
                    .sum::<u64>(),
                9
            );
            assert!(events.iter().all(|e| e.cost_known && e.ts.second() == 0));
        }
    }

    #[test]
    fn ledger_replay_is_once_but_equal_size_new_requests_and_legacy_prefix_are_retained() {
        let mut rows = vec![
            meta(Value::Null),
            usage(Value::Null, Some(40), 40),
            record("one", 60, 100),
        ];
        rows.push(record("one", 60, 160));
        rows.push(record("two", 60, 220));
        let events = parse(&rows);
        assert_eq!(events.len(), 3);
        assert_eq!(
            events.iter().map(|e| e.metrics().total_tokens).sum::<u64>(),
            180
        );
    }

    #[test]
    fn ledger_requires_thread_execution_session_and_explicit_ordinal_ownership() {
        let mut header = meta(json!(10));
        header["payload"]["session_id"] = json!("execution");
        let mut owned = record("owned", 60, 100);
        owned["payload"]["session_id"] = json!("execution");
        owned["ordinal"] = json!(11);
        let mut copied = owned.clone();
        copied["payload"]["thread_id"] = json!("parent");
        copied["payload"]["model"] = json!("wrong-model");
        let mut wrong_session = owned.clone();
        wrong_session["payload"]["session_id"] = json!("another-execution");
        let mut prefix = owned.clone();
        prefix["ordinal"] = json!(2);
        let mut no_ordinal = owned.clone();
        no_ordinal.as_object_mut().unwrap().remove("ordinal");
        let events = parse(&[header, copied, wrong_session, prefix, no_ordinal, owned]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].model, "unknown");
        assert_eq!(events[0].session_id, "child");
    }

    #[test]
    fn malformed_ledger_cannot_suppress_valid_legacy_or_create_zero_usage() {
        for field in [json!(true), json!(-1), json!(1.5), json!("60"), json!(null)] {
            let mut invalid = record("bad", 60, 100);
            invalid["payload"]["usage"]["input_tokens"] = field;
            let events = parse(&[meta(Value::Null), invalid, usage(Value::Null, Some(60), 60)]);
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].input_tokens, 60);
        }
        let mut invalid = record("bad", 60, 100);
        invalid["payload"]["usage"]["cached_input_tokens"] = json!(61);
        assert!(parse(&[meta(Value::Null), invalid]).is_empty());
    }

    #[test]
    fn neighbouring_counter_mirrors_reconcile_different_timestamps_both_orders() {
        for legacy_first in [false, true] {
            let mut ledger = record("one", 100, 100);
            ledger["payload"]["thread_token_usage"] = ledger["payload"]["usage"].clone();
            let legacy = json!({"type":"event_msg","timestamp":"2026-09-11T00:00:01Z",
                "payload":{"type":"token_count","turn_id":"turn","info":{
                    "last_token_usage":ledger["payload"]["usage"],"total_token_usage":ledger["payload"]["thread_token_usage"]}}});
            let mut rows = vec![meta(Value::Null)];
            rows.extend(if legacy_first {
                [legacy, ledger]
            } else {
                [ledger, legacy]
            });
            let events = parse(&rows);
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].metrics().total_tokens, 110);
            assert_eq!(events[0].ts.second(), 0);
        }
    }

    #[test]
    fn ledger_model_evidence_is_owned_by_its_turn_and_unknown_models_stay_unpriced() {
        let mut known = record("known", 60, 100);
        known["payload"]["turn_id"] = json!("earlier");
        let mut unknown = record("unknown", 60, 160);
        unknown["payload"]["turn_id"] = json!("unseen");
        let events = parse(&[
            meta(Value::Null),
            json!({"type":"turn_context","payload":{"turn_id":"earlier","model":"gpt-5"}}),
            json!({"type":"turn_context","payload":{"turn_id":"later","model":"gpt-6-astra"}}),
            known,
            unknown,
        ]);
        assert_eq!(events[0].model, "gpt-5");
        assert!(events[0].cost_known);
        assert_eq!(events[1].model, "unknown");
        assert!(!events[1].cost_known);
    }

    #[test]
    fn cached_request_identity_survives_reload_and_archived_copies() {
        let events = parse(&[
            meta(Value::Null),
            record("one", 60, 100),
            record("two", 60, 160),
        ]);
        let saved = serde_json::to_vec(&events).unwrap();
        let restored: Vec<LocalUsageEvent> = serde_json::from_slice(&saved).unwrap();
        let duplicates: Vec<_> = events.iter().chain(restored.iter()).collect();
        assert_eq!(unique(&duplicates).len(), 2);
        let mut other = restored[0].clone();
        other.session_id = "another-thread".into();
        let rows: Vec<_> = duplicates.into_iter().chain([&other]).collect();
        assert_eq!(unique(&rows).len(), 3);
    }

    #[test]
    fn totals_only_mirrors_and_nonmirrored_deltas_keep_the_legacy_baseline() {
        let mut first = record("one", 100, 100);
        first["payload"]["thread_token_usage"] = first["payload"]["usage"].clone();
        let mut second = record("two", 60, 160);
        second["timestamp"] = json!("2026-09-12T00:00:00Z");
        let mirror = |ledger: &Value| {
            json!({"type":"event_msg","timestamp":ledger["timestamp"],
            "payload":{"type":"token_count","turn_id":"turn","info":{"total_token_usage":ledger["payload"]["thread_token_usage"]}}})
        };
        let mut third = mirror(&second);
        third["timestamp"] = json!("2026-09-13T00:00:00Z");
        third["payload"]["info"]["total_token_usage"]["input_tokens"] = json!(170);
        let events = parse(&[
            meta(Value::Null),
            first.clone(),
            mirror(&first),
            mirror(&second),
            second,
            third,
        ]);
        assert_eq!(events.len(), 3);
        assert_eq!(
            events
                .iter()
                .map(|event| event.metrics().total_tokens)
                .sum::<u64>(),
            190
        );
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
