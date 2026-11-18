//! Gemini CLI chat logs: `<tmp>/<project>/chats/**/*.jsonl` (current) and
//! legacy whole-document `*.json`. Only token, model, timestamp and session
//! metadata are read; message content is never retained.
use super::local_usage::{LocalUsageEvent, parse_ts};
use super::pricing;
use anyhow::Result;
use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// Session files under each project's `chats` directory, including nested
/// subagent sessions. A legacy `X.json` is dropped when Gemini CLI has
/// migrated it to `X.jsonl` (resume copies every message, keeping the original).
pub(super) fn files(root: &Path) -> Vec<PathBuf> {
    let mut out = BTreeSet::new();
    let Ok(projects) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    for project in projects.flatten() {
        collect(&project.path().join("chats"), &mut out);
    }
    out.iter()
        .filter(|path| {
            path.extension().and_then(|s| s.to_str()) != Some("json")
                || !out.contains(&path.with_extension("jsonl"))
        })
        .cloned()
        .collect()
}

fn collect(dir: &Path, out: &mut BTreeSet<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if matches!(
            path.extension().and_then(|s| s.to_str()),
            Some("json" | "jsonl")
        ) {
            out.insert(path);
        }
    }
}

#[derive(Deserialize)]
struct Record {
    #[serde(rename = "type")]
    kind: Option<JsonValue>,
    id: Option<JsonValue>,
    timestamp: Option<JsonValue>,
    model: Option<JsonValue>,
    tokens: Option<JsonValue>,
    #[serde(rename = "sessionId")]
    session_id: Option<JsonValue>,
    #[serde(rename = "$set")]
    set: Option<SessionUpdate>,
    // Unlisted fields (content, thoughts, tool calls) are skipped without being retained.
}

#[derive(Deserialize)]
struct SessionUpdate {
    #[serde(rename = "sessionId")]
    session_id: Option<JsonValue>,
}

#[derive(Deserialize)]
struct LegacyDocument {
    #[serde(rename = "sessionId")]
    session_id: Option<JsonValue>,
    #[serde(default)]
    messages: Vec<JsonValue>,
}

fn text(value: Option<&JsonValue>) -> Option<String> {
    value
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

pub(super) fn scan_file(path: &Path, events: &mut Vec<LocalUsageEvent>) -> Result<()> {
    let mut session = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let mut records = Vec::new();
    if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
        // Split bytes, not UTF-8 lines: a record cut mid-character (a failed
        // append is never retried) must not hide the rest of the file.
        for line in std::fs::read(path)?.split(|byte| *byte == b'\n') {
            // Malformed or truncated (still being written) lines are skipped.
            let Ok(record) = serde_json::from_slice::<Record>(line) else {
                continue;
            };
            if let Some(id) = text(record.session_id.as_ref())
                .or_else(|| text(record.set.as_ref().and_then(|s| s.session_id.as_ref())))
            {
                session = id;
            }
            records.push(record);
        }
    } else {
        let Ok(document) = serde_json::from_slice::<LegacyDocument>(&std::fs::read(path)?) else {
            return Ok(());
        };
        if let Some(id) = text(document.session_id.as_ref()) {
            session = id;
        }
        records.extend(
            document
                .messages
                .into_iter()
                .filter_map(|message| serde_json::from_value::<Record>(message).ok()),
        );
    }
    let project = project_for(path);
    // A message is re-appended whenever it changes; the last version that
    // carries tokens is the request's final usage. Tokenless versions (written
    // before the response metadata arrived) never replace it.
    let mut by_id: HashMap<String, usize> = HashMap::new();
    let mut found: Vec<LocalUsageEvent> = Vec::new();
    for record in records {
        if record.kind.as_ref().and_then(JsonValue::as_str) != Some("gemini") {
            continue;
        }
        let Some(event) = event(&record, &session, &project) else {
            continue;
        };
        match text(record.id.as_ref()) {
            Some(id) => match by_id.get(&id) {
                Some(&index) => found[index] = event,
                None => {
                    by_id.insert(id, found.len());
                    found.push(event);
                }
            },
            None => found.push(event),
        }
    }
    for mut event in found {
        pricing::apply(&mut event);
        events.push(event);
    }
    Ok(())
}

fn event(record: &Record, session: &str, project: &str) -> Option<LocalUsageEvent> {
    let tokens = record.tokens.as_ref()?.as_object()?;
    let ts = parse_ts(record.timestamp.as_ref().filter(|v| v.is_string()))?;
    let count = |key: &str| {
        tokens
            .get(key)
            .and_then(|v| {
                v.as_u64().or_else(|| {
                    v.as_f64()
                        .filter(|f| f.is_finite() && *f >= 0.0)
                        .map(|f| f as u64)
                })
            })
            .unwrap_or(0)
    };
    let (input, output, cached, thoughts, tool) = (
        count("input"),
        count("output"),
        count("cached"),
        count("thoughts"),
        count("tool"),
    );
    let total = tokens.get("total").and_then(JsonValue::as_u64);
    // `input` is promptTokenCount, which includes cachedContentTokenCount.
    // Treat it as already uncached only when `total` proves the cache was
    // counted separately (matching ccusage and tokscale).
    let inclusive = input + output + thoughts + tool;
    let already_net = cached > 0 && total == Some(inclusive + cached) && total != Some(inclusive);
    let (uncached, cache_read) = if already_net {
        (input, cached)
    } else {
        let cached = cached.min(input);
        (input - cached, cached)
    };
    let event = LocalUsageEvent {
        ts,
        session_id: session.to_string(),
        project: project.to_string(),
        model: text(record.model.as_ref()).unwrap_or_else(|| "unknown".into()),
        // toolUsePromptTokenCount is separately billed prompt input.
        input_tokens: uncached + tool,
        cache_read_tokens: cache_read,
        cache_creation_tokens: 0,
        // Output includes reasoning, as for the other local sources.
        output_tokens: output + thoughts,
        reasoning_output_tokens: thoughts,
        ..LocalUsageEvent::default()
    };
    (event.input_tokens + event.cache_read_tokens + event.output_tokens > 0).then_some(event)
}

/// The project directory name under `tmp` (Gemini's short project id, or a
/// SHA-256 of the project root in older versions, shortened for display).
fn project_for(path: &Path) -> String {
    let id = path
        .ancestors()
        .find(|dir| dir.file_name().and_then(|s| s.to_str()) == Some("chats"))
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    if id.len() == 64 && id.chars().all(|c| c.is_ascii_hexdigit()) {
        id[..12].to_string()
    } else {
        id.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write(path: &Path, rows: &[JsonValue], extra: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut text = rows
            .iter()
            .map(JsonValue::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        text.push_str(extra);
        std::fs::write(path, text).unwrap();
    }

    fn message(id: &str, ts: &str, tokens: Option<JsonValue>) -> JsonValue {
        let mut row = json!({"id":id,"timestamp":ts,"type":"gemini","model":"gemini-3-flash-preview",
            "content":"private response text","thoughts":[{"subject":"secret","description":"plan"}]});
        if let Some(tokens) = tokens {
            row["tokens"] = tokens;
        }
        row
    }

    fn scan(path: &Path) -> Vec<LocalUsageEvent> {
        let mut events = Vec::new();
        scan_file(path, &mut events).unwrap();
        events
    }

    #[test]
    fn jsonl_keeps_last_tokened_version_and_separates_cached_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("my-project/chats/session-2026-05-17T11-07-abc.jsonl");
        let tokens =
            json!({"input":15327,"output":23,"cached":11526,"thoughts":919,"tool":7,"total":16276});
        write(
            &path,
            &[
                json!({"sessionId":"session-a","projectHash":"hash","startTime":"2026-05-17T11:07:00.000Z"}),
                json!({"id":"user-1","timestamp":"2026-05-17T11:07:01.000Z","type":"user","content":"secret prompt"}),
                message("msg-a", "2026-05-17T11:07:30.000Z", None),
                message("msg-a", "2026-05-17T11:07:32.000Z", Some(tokens.clone())),
                // A later tokenless rewrite (tool result patch) keeps the usage.
                message("msg-a", "2026-05-17T11:07:33.000Z", None),
                json!({"$rewindTo":"msg-a"}),
                message(
                    "msg-b",
                    "2026-05-17T11:08:00.000Z",
                    Some(
                        json!({"input":10,"output":1,"cached":0,"thoughts":0,"tool":0,"total":11}),
                    ),
                ),
                message(
                    "msg-b",
                    "2026-05-17T11:08:01.000Z",
                    Some(
                        json!({"input":20,"output":2,"cached":5,"thoughts":3,"tool":0,"total":25}),
                    ),
                ),
                message("bad-time", "not a time", Some(tokens)),
            ],
            "\n{\"id\":\"msg-c\",\"type\":\"gemini\",\"tokens\":{\"input\":9", // truncated tail
        );
        let events = scan(&path);
        assert_eq!(events.len(), 2);
        let first = &events[0];
        assert_eq!(first.session_id, "session-a");
        assert_eq!(first.project, "my-project");
        assert_eq!(first.model, "gemini-3-flash-preview");
        assert_eq!(first.input_tokens, 15327 - 11526 + 7);
        assert_eq!(first.cache_read_tokens, 11526);
        assert_eq!(first.output_tokens, 23 + 919);
        assert_eq!(first.reasoning_output_tokens, 919);
        assert_eq!(
            first.metrics().total_tokens,
            16276,
            "classes sum to Gemini's total"
        );
        assert_eq!(events[1].input_tokens, 15);
        assert_eq!(events[1].cache_read_tokens, 5);
        assert_eq!(events[1].output_tokens, 5);
        assert!(!first.cost_known, "Gemini models stay unpriced");
        let cache = serde_json::to_string(&events).unwrap();
        assert!(!cache.contains("private") && !cache.contains("secret"));
    }

    #[test]
    fn already_net_input_is_kept_when_total_counts_cache_separately() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p/chats/s.jsonl");
        write(
            &path,
            &[message(
                "m",
                "2026-05-17T11:07:32Z",
                Some(json!({"input":7,"output":20,"cached":5,"thoughts":0,"tool":0,"total":32})),
            )],
            "",
        );
        let events = scan(&path);
        assert_eq!(
            (events[0].input_tokens, events[0].cache_read_tokens),
            (7, 5)
        );
    }

    #[test]
    fn legacy_json_document_and_migrated_pair_discovery() {
        let dir = tempfile::tempdir().unwrap();
        let hash = "a".repeat(64);
        let chats = dir.path().join(&hash).join("chats");
        let doc = json!({"sessionId":"legacy","projectHash":hash,"startTime":"2026-01-01T00:00:00Z",
            "messages":[message("m1","2026-01-01T00:00:01Z",Some(json!({"input":100,"output":10,"cached":40,"thoughts":0,"tool":0,"total":110}))),
                {"id":"u","type":"user","timestamp":"2026-01-01T00:00:00Z","content":[{"text":"prompt"}]}, 7]});
        std::fs::create_dir_all(&chats).unwrap();
        std::fs::write(chats.join("session-old.json"), doc.to_string()).unwrap();
        std::fs::write(chats.join("session-kept.json"), doc.to_string()).unwrap();
        std::fs::write(chats.join("session-old.jsonl"), "").unwrap();
        write(&chats.join("parent-session/sub.jsonl"), &[], "");
        std::fs::write(dir.path().join(&hash).join("logs.json"), "[]").unwrap();
        std::fs::write(chats.join("notes.txt"), "x").unwrap();
        let found = files(dir.path());
        assert_eq!(
            found,
            vec![
                chats.join("parent-session/sub.jsonl"),
                chats.join("session-kept.json"),
                chats.join("session-old.jsonl"),
            ]
        );
        let legacy = scan(&chats.join("session-kept.json"));
        assert_eq!(legacy.len(), 1, "non-object and user messages are ignored");
        assert_eq!(legacy[0].session_id, "legacy");
        assert_eq!(legacy[0].project, "aaaaaaaaaaaa");
        assert_eq!(
            (legacy[0].input_tokens, legacy[0].cache_read_tokens),
            (60, 40)
        );
        std::fs::write(chats.join("broken.json"), "{not json").unwrap();
        assert!(scan(&chats.join("broken.json")).is_empty());
    }

    #[test]
    fn invalid_utf8_lines_and_cut_multibyte_tails_do_not_hide_other_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p/chats/session-bytes.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let tokens = json!({"input":4,"output":1,"cached":0,"total":5});
        let mut bytes = message("a", "2026-05-17T11:07:32Z", Some(tokens.clone()))
            .to_string()
            .into_bytes();
        bytes.extend_from_slice(b"\n{\"id\":\"bad\",\"content\":\"\xff\xfe\"}\n");
        bytes.extend(
            message("b", "2026-05-17T11:08:32Z", Some(tokens))
                .to_string()
                .into_bytes(),
        );
        bytes.extend_from_slice(b"\n{\"id\":\"x\",\"content\":\"\xe6\x97");
        std::fs::write(&path, bytes).unwrap();
        let events = scan(&path);
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].ts.to_rfc3339(), "2026-05-17T11:08:32+00:00");
    }

    #[test]
    fn resumed_session_id_update_applies_to_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p/chats/session-x.jsonl");
        write(
            &path,
            &[
                message(
                    "m",
                    "2026-05-17T11:07:32Z",
                    Some(json!({"input":3,"output":1,"cached":0,"total":4})),
                ),
                json!({"$set":{"sessionId":"resumed"}}),
            ],
            "",
        );
        assert_eq!(scan(&path)[0].session_id, "resumed");
    }
}
