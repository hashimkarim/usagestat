//! Amp thread files: `<data>/threads/**/*.json`, one JSON document per thread.
//! Usage comes from the thread's `usageLedger` (one event per inference) and
//! assistant `messages[].usage`; message content is never retained.
use super::local_usage::LocalUsageEvent;
use super::pricing;
use anyhow::Result;
use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::{Path, PathBuf};

pub(super) fn files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect(root, &mut out);
    out.sort();
    out
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("json") {
            out.push(path);
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Thread {
    id: Option<JsonValue>,
    created: Option<JsonValue>,
    #[serde(default)]
    messages: Vec<Message>,
    usage_ledger: Option<Ledger>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Message {
    role: Option<JsonValue>,
    message_id: Option<JsonValue>,
    usage: Option<JsonValue>,
    // Unlisted fields (content, tool calls) are not retained.
}

#[derive(Deserialize)]
struct Ledger {
    #[serde(default)]
    events: Vec<JsonValue>,
}

#[derive(Clone)]
struct Usage {
    ts: Option<DateTime<Utc>>,
    model: String,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    message_id: Option<i64>,
}

fn count(value: &JsonValue, key: &str) -> u64 {
    value.get(key).and_then(JsonValue::as_u64).unwrap_or(0)
}

fn text(value: Option<&JsonValue>) -> Option<String> {
    value
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// RFC 3339 strings, or epoch numbers (milliseconds when large enough).
fn timestamp(value: Option<&JsonValue>) -> Option<DateTime<Utc>> {
    match value? {
        JsonValue::String(s) => DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|dt| dt.with_timezone(&Utc)),
        JsonValue::Number(n) => {
            let n = n.as_i64().filter(|n| *n > 0)?;
            if n >= 100_000_000_000 {
                Utc.timestamp_millis_opt(n).single()
            } else {
                Utc.timestamp_opt(n, 0).single()
            }
        }
        _ => None,
    }
}

fn ledger_usage(event: &JsonValue) -> Option<(Option<String>, Usage)> {
    let tokens = event.get("tokens")?;
    let id = event.get("id").and_then(|id| {
        id.as_str()
            .map(str::to_owned)
            .or_else(|| id.as_i64().map(|n| n.to_string()))
    });
    Some((
        id,
        Usage {
            ts: timestamp(event.get("timestamp")),
            model: text(event.get("model"))?,
            input: count(tokens, "input"),
            output: count(tokens, "output"),
            cache_read: count(tokens, "cacheReadInputTokens"),
            cache_write: count(tokens, "cacheCreationInputTokens"),
            message_id: event.get("toMessageId").and_then(JsonValue::as_i64),
        },
    ))
}

fn message_usage(message: &Message) -> Option<Usage> {
    if message.role.as_ref().and_then(JsonValue::as_str) != Some("assistant") {
        return None;
    }
    let usage = message.usage.as_ref()?;
    Some(Usage {
        ts: timestamp(usage.get("timestamp")),
        model: text(usage.get("model"))?,
        input: count(usage, "inputTokens"),
        output: count(usage, "outputTokens"),
        cache_read: count(usage, "cacheReadInputTokens"),
        cache_write: count(usage, "cacheCreationInputTokens"),
        message_id: message.message_id.as_ref().and_then(JsonValue::as_i64),
    })
}

pub(super) fn scan_file(path: &Path, events: &mut Vec<LocalUsageEvent>) -> Result<()> {
    let Ok(thread) = serde_json::from_slice::<Thread>(&std::fs::read(path)?) else {
        // Partially written or unrelated JSON: nothing reliable to count.
        return Ok(());
    };
    let session = text(thread.id.as_ref()).unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string()
    });
    let created = timestamp(thread.created.as_ref());

    // Ledger events are one per inference; a repeated event id is the same
    // inference rewritten, so the last copy wins.
    let mut ledger: Vec<Usage> = Vec::new();
    let mut ledger_ids: HashMap<String, usize> = HashMap::new();
    let ledger_events = thread.usage_ledger.iter().flat_map(|l| l.events.iter());
    for (id, usage) in ledger_events.filter_map(ledger_usage) {
        match id {
            Some(id) => match ledger_ids.entry(id) {
                Entry::Occupied(entry) => ledger[*entry.get()] = usage,
                Entry::Vacant(entry) => {
                    entry.insert(ledger.len());
                    ledger.push(usage);
                }
            },
            None => ledger.push(usage),
        }
    }
    // Assistant message usage, last copy per message id.
    let mut messages: Vec<Usage> = Vec::new();
    let mut message_ids: HashMap<i64, usize> = HashMap::new();
    for usage in thread.messages.iter().filter_map(message_usage) {
        match usage
            .message_id
            .and_then(|id| message_ids.get(&id).copied())
        {
            Some(index) => messages[index] = usage,
            None => {
                if let Some(id) = usage.message_id {
                    message_ids.insert(id, messages.len());
                }
                messages.push(usage);
            }
        }
    }

    // A ledger event and the assistant message it produced describe the same
    // inference. Pair them by `toMessageId`, or for ledgers without one by an
    // identical model and token count, so each inference is counted once.
    // Unpaired records on either side are distinct inferences.
    let mut used = vec![false; ledger.len()];
    let mut rows = Vec::new();
    for message in messages {
        let paired = (0..ledger.len())
            .find(|&i| {
                !used[i]
                    && message.message_id.is_some()
                    && ledger[i].message_id == message.message_id
            })
            .or_else(|| {
                (0..ledger.len()).find(|&i| {
                    let l = &ledger[i];
                    !used[i]
                        && l.message_id.is_none()
                        && l.model == message.model
                        && (l.input, l.output) == (message.input, message.output)
                })
            });
        match paired {
            Some(i) => {
                used[i] = true;
                let l = &ledger[i];
                // Ledger tokens are uncached input/output; cache classes are
                // recorded on the message when the ledger omits them.
                let cached = l.cache_read != 0 || l.cache_write != 0;
                rows.push(Usage {
                    ts: l.ts.or(message.ts),
                    cache_read: if cached {
                        l.cache_read
                    } else {
                        message.cache_read
                    },
                    cache_write: if cached {
                        l.cache_write
                    } else {
                        message.cache_write
                    },
                    ..l.clone()
                });
            }
            None => rows.push(message),
        }
    }
    rows.extend(
        ledger
            .into_iter()
            .zip(used)
            .filter(|(_, used)| !used)
            .map(|(usage, _)| usage),
    );

    for usage in rows {
        // The thread creation time is a stable fallback; file mtime is not.
        let Some(ts) = usage.ts.or(created) else {
            continue;
        };
        if usage.input + usage.output + usage.cache_read + usage.cache_write == 0 {
            continue;
        }
        let mut event = LocalUsageEvent {
            ts,
            session_id: session.clone(),
            project: "unknown".into(),
            model: usage.model,
            input_tokens: usage.input,
            output_tokens: usage.output,
            cache_read_tokens: usage.cache_read,
            cache_creation_tokens: usage.cache_write,
            ..LocalUsageEvent::default()
        };
        pricing::apply(&mut event);
        events.push(event);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scan(value: &JsonValue) -> Vec<LocalUsageEvent> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("T-thread.json");
        std::fs::write(&path, value.to_string()).unwrap();
        let mut events = Vec::new();
        scan_file(&path, &mut events).unwrap();
        events
    }

    #[test]
    fn ledger_pairs_with_message_cache_and_unpaired_records_count_once() {
        let events = scan(&json!({
            "id": "T-1",
            "created": 1_780_000_000_000_i64,
            "messages": [
                {"role":"user","messageId":0,"content":[{"type":"text","text":"secret prompt"}]},
                {"role":"assistant","messageId":1,"content":[{"type":"text","text":"secret answer"}],
                 "usage":{"model":"claude-sonnet-4-5","inputTokens":100,"outputTokens":20,
                          "cacheReadInputTokens":1000,"cacheCreationInputTokens":50}},
                // A message missing from a partial ledger is still one inference.
                {"role":"assistant","messageId":2,
                 "usage":{"model":"claude-sonnet-4-5","inputTokens":7,"outputTokens":3,
                          "timestamp":"2026-06-01T10:05:00Z"}},
                // Rewritten copy of message 2.
                {"role":"assistant","messageId":2,
                 "usage":{"model":"claude-sonnet-4-5","inputTokens":7,"outputTokens":3,
                          "timestamp":"2026-06-01T10:05:00Z"}}
            ],
            "usageLedger": {"events": [
                {"id":"e1","timestamp":"2026-06-01T10:00:00Z","model":"claude-sonnet-4-5",
                 "credits":3,"tokens":{"input":100,"output":20},"toMessageId":1},
                {"id":"e1","timestamp":"2026-06-01T10:00:00Z","model":"claude-sonnet-4-5",
                 "credits":3,"tokens":{"input":100,"output":20},"toMessageId":1},
                {"id":"e3","timestamp":"2026-06-01T10:10:00Z","model":"unknown-model",
                 "tokens":{"input":5,"output":5}},
                {"id":"e4","model":"claude-sonnet-4-5","tokens":{"input":1,"output":1},"toMessageId":9},
                {"id":"e5","timestamp":"2026-06-01T10:20:00Z","tokens":{"input":1}},
                "not an event"
            ]}
        }));
        assert_eq!(events.len(), 4);
        let first = &events[0];
        assert_eq!(first.session_id, "T-1");
        assert_eq!(first.ts.to_rfc3339(), "2026-06-01T10:00:00+00:00");
        assert_eq!(
            (
                first.input_tokens,
                first.output_tokens,
                first.cache_read_tokens,
                first.cache_creation_tokens
            ),
            (100, 20, 1000, 50)
        );
        assert!(
            first.cost_known,
            "Claude IDs use the verified Claude API table"
        );
        assert_eq!(events[1].input_tokens, 7);
        assert_eq!(events[1].ts.to_rfc3339(), "2026-06-01T10:05:00+00:00");
        assert_eq!(events[2].model, "unknown-model");
        assert!(!events[2].cost_known);
        assert_eq!(
            events[3].ts,
            timestamp(Some(&json!(1_780_000_000_000_i64))).unwrap(),
            "ledger rows without a timestamp use the thread creation time"
        );
        let cache = serde_json::to_string(&events).unwrap();
        assert!(!cache.contains("secret"));
    }

    #[test]
    fn message_only_threads_and_ledgers_without_message_ids_pair_by_tokens() {
        let events = scan(&json!({
            "id": "T-2",
            "messages": [
                {"role":"assistant","messageId":1,
                 "usage":{"model":"claude-opus-4-6","inputTokens":10,"outputTokens":2,"cacheReadInputTokens":30}}
            ],
            "usageLedger": {"events": [
                {"timestamp":"2026-06-02T00:00:00Z","model":"claude-opus-4-6","tokens":{"input":10,"output":2}}
            ]}
        }));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].cache_read_tokens, 30);
        let undated = scan(&json!({"id":"T-3","messages":[
            {"role":"assistant","messageId":1,"usage":{"model":"claude-opus-4-6","inputTokens":1}}]}));
        assert!(undated.is_empty(), "no stable timestamp, no guessed day");
    }

    #[test]
    fn malformed_thread_files_yield_nothing() {
        assert!(scan(&json!([1, 2])).is_empty());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("partial.json");
        std::fs::write(&path, "{\"id\":\"T\",\"messages\":[").unwrap();
        let mut events = Vec::new();
        scan_file(&path, &mut events).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn discovery_reads_nested_thread_json_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("T-a.json"), "{}").unwrap();
        std::fs::write(dir.path().join("nested/T-b.json"), "{}").unwrap();
        std::fs::write(dir.path().join("T-c.json.tmp"), "{}").unwrap();
        assert_eq!(
            files(dir.path()),
            vec![
                dir.path().join("T-a.json"),
                dir.path().join("nested/T-b.json")
            ]
        );
    }
}
