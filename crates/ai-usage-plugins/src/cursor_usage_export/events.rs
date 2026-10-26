//! Complete, account-scoped Cursor event history. List-price amounts are never billing.
use super::{CsvUsageRow, CursorSession};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const ENDPOINT: &str = "https://cursor.com/api/dashboard/get-filtered-usage-events";
const PAGE_SIZE: usize = 1000;
const BUDGET: Duration = Duration::from_secs(20);
static FAILED: Mutex<Option<(String, Instant)>> = Mutex::new(None);
static RECENT: Mutex<Option<(String, Instant, Vec<CsvUsageRow>)>> = Mutex::new(None);

#[derive(Default, Serialize, Deserialize)]
struct Store {
    version: u32,
    account: String,
    days: BTreeMap<String, Day>,
}
#[derive(Serialize, Deserialize)]
struct Day {
    complete: bool,
    rows: Vec<CsvUsageRow>,
}

fn number(value: &Value) -> Option<f64> {
    let value = value
        .as_f64()
        .or_else(|| value.as_str()?.parse::<f64>().ok())?;
    (value.is_finite() && value >= 0.0).then_some(value)
}
fn counter(value: Option<&Value>) -> Result<u64> {
    let value = value.context("Missing token measurement")?;
    value
        .as_u64()
        .or_else(|| value.as_str()?.parse().ok())
        .context("Invalid token measurement")
}

fn parse_page(body: &[u8], day: &str, start: i64, end: i64) -> Result<(usize, Vec<CsvUsageRow>)> {
    let payload: Value = serde_json::from_slice(body).context("Invalid Cursor event page")?;
    let total = payload["totalUsageEventsCount"]
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .context("Missing event count")?;
    let events = payload["usageEventsDisplay"]
        .as_array()
        .context("Missing event list")?;
    if events.len() > PAGE_SIZE {
        bail!("Oversized event page")
    }
    let mut rows = Vec::with_capacity(events.len());
    for event in events {
        let timestamp = event["timestamp"]
            .as_i64()
            .or_else(|| event["timestamp"].as_str()?.parse().ok())
            .context("Invalid event time")?;
        let local = DateTime::<Utc>::from_timestamp_millis(timestamp)
            .context("Invalid event time")?
            .with_timezone(&Local);
        if timestamp < start || timestamp > end || local.format("%Y-%m-%d").to_string() != day {
            bail!("Event outside requested day")
        }
        let model = event["model"]
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .context("Missing event model")?
            .to_owned();
        let tokens = event["tokenUsage"]
            .as_object()
            .context("Missing token measurements")?;
        let input_no_cache = counter(tokens.get("inputTokens"))?;
        let input_cache_write = counter(tokens.get("cacheWriteTokens"))?;
        let cache_read = counter(tokens.get("cacheReadTokens"))?;
        let output_tokens = counter(tokens.get("outputTokens"))?;
        let total_tokens = [input_no_cache, input_cache_write, cache_read, output_tokens]
            .into_iter()
            .try_fold(0u64, |sum, value| sum.checked_add(value))
            .context("Token total overflow")?;
        // tokenUsage.totalCents is API-equivalent list pricing; only chargedCents
        // establishes the amount actually deducted by Cursor, including known zero.
        let cost_usd = number(&event["chargedCents"])
            .context("Event omits the billed charge; use CSV")?
            / 100.0;
        rows.push(CsvUsageRow {
            day_key: day.into(),
            model,
            input_no_cache,
            input_cache_write,
            cache_read,
            output_tokens,
            total_tokens,
            cost_usd,
        });
    }
    Ok((total, rows))
}

fn fetch_day(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    cookie: &str,
    date: NaiveDate,
    now: DateTime<Local>,
    deadline: Instant,
) -> Result<Day> {
    let next = date.succ_opt().context("Invalid next calendar day")?;
    let midnight = |day: NaiveDate| {
        Local
            .from_local_datetime(&day.and_hms_opt(0, 0, 0).unwrap())
            .earliest()
            .context("Unavailable local midnight")
    };
    let start = midnight(date)?.timestamp_millis();
    let next = midnight(next)?.timestamp_millis();
    let end = next.min(now.timestamp_millis()) - 1;
    if end < start {
        bail!("Future or incomplete event interval")
    }
    let day = date.format("%Y-%m-%d").to_string();
    let mut rows = Vec::new();
    let mut expected = None;
    let mut pages = HashSet::new();
    let mut events = HashSet::new();
    let origin = reqwest::Url::parse(endpoint)?
        .origin()
        .ascii_serialization();
    for page in 1..=100 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("Cursor history refresh budget exceeded")
        }
        let response=client.post(endpoint).header("Cookie",cookie).header("Origin",&origin)
            .header("Accept","application/json").timeout(remaining.min(Duration::from_secs(5)))
            .json(&json!({"startDate":start.to_string(),"endDate":end.to_string(),"page":page,"pageSize":PAGE_SIZE})).send()?;
        if !response.status().is_success() {
            bail!("Cursor event history is unavailable")
        }
        let mut body = Vec::new();
        response.take(4 * 1024 * 1024 + 1).read_to_end(&mut body)?;
        if body.len() > 4 * 1024 * 1024 {
            bail!("Cursor event response exceeds its limit")
        }
        if !pages.insert(Sha256::digest(&body)) {
            bail!("Cursor repeated a history page")
        }
        let (total, parsed) = parse_page(&body, &day, start, end)?;
        // Reject overlapping pages even when counts and response bodies differ.
        // A byte-identical event without a unique ID is ambiguous: prefer CSV.
        let payload: Value = serde_json::from_slice(&body)?;
        for event in payload["usageEventsDisplay"].as_array().unwrap() {
            let identity = ["id", "requestId", "usageEventId"].iter().find_map(|key| {
                event
                    .get(*key)
                    .filter(|value| value.is_string() || value.is_number())
                    .map(|value| format!("{key}:{value}"))
            });
            let fingerprint = identity.unwrap_or(serde_json::to_string(event)?);
            if !events.insert(fingerprint) {
                bail!("Cursor returned overlapping history pages")
            }
        }
        if expected.is_some_and(|previous| previous != total) {
            bail!("Cursor event count changed during pagination")
        }
        expected = Some(total);
        if parsed.is_empty() && rows.len() < total {
            bail!("Cursor returned incomplete history pages")
        }
        rows.extend(parsed);
        if rows.len() == total {
            // Prove that downstream sums remain representable before publication.
            rows.iter()
                .try_fold(0u64, |sum, row| sum.checked_add(row.total_tokens))
                .context("Token total overflow")?;
            return Ok(Day {
                complete: next <= now.timestamp_millis(),
                rows,
            });
        }
        if rows.len() > total {
            bail!("Cursor returned too many events")
        }
    }
    bail!("Cursor history pagination exceeded its limit")
}

fn collect(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    session: &CursorSession,
    first: NaiveDate,
    last: NaiveDate,
    now: DateTime<Local>,
    path: &Path,
) -> Result<Vec<CsvUsageRow>> {
    let mut store = usagestat_core::storage::read_private(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Store>(&text).ok())
        .filter(|store| store.version == 1 && store.account == session.account_key)
        .unwrap_or_else(|| Store {
            version: 1,
            account: session.account_key.clone(),
            ..Store::default()
        });
    let deadline = Instant::now() + BUDGET;
    let today = now.date_naive();
    let yesterday = today.pred_opt().context("Invalid current day")?;
    let dates: Vec<_> = first.iter_days().take_while(|date| *date <= last).collect();
    let mut changed = false;
    let mut today_refreshed = false;
    for date in dates.iter().rev() {
        let key = date.format("%Y-%m-%d").to_string();
        if *date < yesterday && store.days.get(&key).is_some_and(|day| day.complete) {
            continue;
        }
        if Instant::now() >= deadline {
            break;
        }
        if let Ok(day) = fetch_day(client, endpoint, &session.cookie, *date, now, deadline) {
            store.days.insert(key, day);
            changed = true;
            if *date == today {
                today_refreshed = true;
            }
        } else {
            // Authentication/schema failures apply to the whole source. Avoid
            // repeating the same failed request for every day before CSV.
            break;
        }
    }
    if changed {
        // Only derived accounting rows are persisted; no cookies or raw events.
        usagestat_core::storage::write_atomic(path, &serde_json::to_vec(&store)?)?;
    }
    let mut rows = Vec::new();
    for date in dates {
        let key = date.format("%Y-%m-%d").to_string();
        let day = store
            .days
            .get(&key)
            .filter(|day| {
                if date == today {
                    today_refreshed
                } else {
                    day.complete
                }
            })
            .context("Cursor history backfill is incomplete; use CSV")?;
        rows.extend(day.rows.iter().cloned());
    }
    rows.iter()
        .try_fold(0u64, |sum, row| sum.checked_add(row.total_tokens))
        .context("Token total overflow")?;
    if !rows.iter().map(|row| row.cost_usd).sum::<f64>().is_finite() {
        bail!("Cost total overflow")
    }
    Ok(rows)
}

pub(super) fn fetch(session: &CursorSession, since: &str, until: &str) -> Result<Vec<CsvUsageRow>> {
    let first = NaiveDate::parse_from_str(since, "%Y%m%d")?;
    let last = NaiveDate::parse_from_str(until, "%Y%m%d")?;
    if last < first || (last - first).num_days() > 366 {
        bail!("Use the CSV export for extended ranges")
    }
    let key = format!("{}:{since}:{until}", session.credential_key);
    if let Some((stored, at, rows)) = RECENT
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
    {
        if stored == &key && at.elapsed() < Duration::from_secs(45) {
            return Ok(rows.clone());
        }
    }
    if FAILED
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .is_some_and(|(stored, at)| stored == &key && at.elapsed() < Duration::from_secs(45))
    {
        bail!("Cursor event history temporarily unavailable; use CSV")
    }
    let path = usagestat_core::paths::data_dir()?
        .join("cursor-usage-history")
        .join(format!("{}.json", session.account_key));
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let rows = match collect(&client, ENDPOINT, session, first, last, Local::now(), &path) {
        Ok(rows) => rows,
        Err(error) => {
            *FAILED.lock().unwrap_or_else(|error| error.into_inner()) = Some((key, Instant::now()));
            return Err(error);
        }
    };
    *RECENT.lock().unwrap_or_else(|error| error.into_inner()) =
        Some((key, Instant::now(), rows.clone()));
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use std::thread;

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 3).unwrap()
    }
    fn now() -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 9, 5, 12, 0, 0)
            .earliest()
            .unwrap()
    }
    fn event(model: &str) -> Value {
        json!({"timestamp":Local.from_local_datetime(&day().and_hms_opt(12,0,0).unwrap()).earliest().unwrap().timestamp_millis(),
            "model":model,"chargedCents":"125","tokenUsage":{"inputTokens":100,"cacheWriteTokens":20,"cacheReadTokens":10,"outputTokens":5,"totalCents":999}})
    }
    fn page(total: usize, event: Value) -> Value {
        json!({"totalUsageEventsCount":total,"usageEventsDisplay":[event]})
    }
    fn session(account: &str) -> CursorSession {
        CursorSession {
            cookie: "session=synthetic".into(),
            account_key: account.into(),
            credential_key: account.into(),
        }
    }
    fn client() -> reqwest::blocking::Client {
        super::super::install_rustls_provider();
        reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
    }
    fn server(replies: Vec<Value>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/events", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let join = thread::spawn(move || {
            for (index, reply) in replies.into_iter().enumerate() {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(5))
                        }
                        Err(error) => panic!("Fixture request missing: {error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = Vec::new();
                let (offset, length) = loop {
                    let mut buffer = [0; 4096];
                    let count = stream.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&buffer[..count]);
                    if let Some(end) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
                        let headers = std::str::from_utf8(&bytes[..end]).unwrap();
                        assert!(headers.starts_with("POST /events HTTP/1.1"));
                        assert!(
                            headers
                                .to_ascii_lowercase()
                                .contains("cookie: session=synthetic")
                        );
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.split_once(':')
                                    .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                                    .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap();
                        break (end + 4, length);
                    }
                };
                while bytes.len() < offset + length {
                    let mut buffer = [0; 4096];
                    let count = stream.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&buffer[..count]);
                }
                let request: Value =
                    serde_json::from_slice(&bytes[offset..offset + length]).unwrap();
                assert_eq!(request["page"], index + 1);
                let body = reply.to_string();
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
            }
        });
        (endpoint, join)
    }

    #[test]
    fn billed_charges_do_not_become_list_price_estimates_or_unknown_zeroes() {
        let sample = event("unknown-model");
        let timestamp = sample["timestamp"].as_i64().unwrap();
        let (_, rows) = parse_page(
            &page(1, sample.clone()).to_string().into_bytes(),
            "2026-09-03",
            timestamp - 1,
            timestamp + 1,
        )
        .unwrap();
        assert_eq!(rows[0].cost_usd, 1.25);
        assert_eq!(rows[0].model, "unknown-model");
        assert_eq!(rows[0].total_tokens, 135);
        let mut unknown = sample.clone();
        unknown.as_object_mut().unwrap().remove("chargedCents");
        assert!(
            parse_page(
                &page(1, unknown).to_string().into_bytes(),
                "2026-09-03",
                timestamp - 1,
                timestamp + 1
            )
            .is_err()
        );
        let mut zero = sample.clone();
        zero["chargedCents"] = json!(0);
        assert_eq!(
            parse_page(
                &page(1, zero).to_string().into_bytes(),
                "2026-09-03",
                timestamp - 1,
                timestamp + 1
            )
            .unwrap()
            .1[0]
                .cost_usd,
            0.0
        );
        let mut invalid = sample;
        invalid["tokenUsage"]["inputTokens"] = json!(false);
        assert!(
            parse_page(
                &page(1, invalid).to_string().into_bytes(),
                "2026-09-03",
                timestamp - 1,
                timestamp + 1
            )
            .is_err()
        );
        assert!(
            parse_page(
                &page(1, event("m")).to_string().into_bytes(),
                "2026-09-04",
                timestamp - 1,
                timestamp + 1
            )
            .is_err()
        );
    }

    #[test]
    fn actual_http_pagination_rejects_repeats_and_count_drift() {
        let cases = [
            (vec![page(2, event("one")), page(2, event("two"))], true),
            (vec![page(2, event("one")), page(2, event("one"))], false),
            (vec![page(2, event("one")), page(3, event("two"))], false),
            (
                vec![
                    page(2, event("one")),
                    json!({"totalUsageEventsCount":2,"usageEventsDisplay":[]}),
                ],
                false,
            ),
            (
                vec![
                    json!({"totalUsageEventsCount":4,"usageEventsDisplay":[event("one"),event("two")]}),
                    json!({"totalUsageEventsCount":4,"usageEventsDisplay":[event("two"),event("three")]}),
                ],
                false,
            ),
        ];
        for (replies, valid) in cases {
            let (url, join) = server(replies);
            let result = fetch_day(
                &client(),
                &url,
                "session=synthetic",
                day(),
                now(),
                Instant::now() + BUDGET,
            );
            join.join().unwrap();
            assert_eq!(result.is_ok(), valid);
            if let Ok(day) = result {
                assert!(day.complete);
                assert_eq!(day.rows.iter().map(|row| row.cost_usd).sum::<f64>(), 2.5);
            }
        }
    }

    #[test]
    fn finished_days_survive_restart_but_incomplete_or_foreign_days_cannot_publish() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("history.json");
        let (url, join) = server(vec![page(1, event("one"))]);
        let first = collect(
            &client(),
            &url,
            &session("account-a"),
            day(),
            day(),
            now(),
            &path,
        );
        join.join().unwrap();
        assert_eq!(first.unwrap()[0].cost_usd, 1.25);
        let unavailable = "http://127.0.0.1:1/events";
        let cached = collect(
            &client(),
            unavailable,
            &session("account-a"),
            day(),
            day(),
            now(),
            &path,
        )
        .unwrap();
        assert_eq!(cached[0].total_tokens, 135);
        assert!(
            collect(
                &client(),
                unavailable,
                &session("account-b"),
                day(),
                day(),
                now(),
                &path
            )
            .is_err()
        );
        let mut store: Store =
            serde_json::from_str(&usagestat_core::storage::read_private(&path).unwrap()).unwrap();
        store.days.get_mut("2026-09-03").unwrap().complete = false;
        usagestat_core::storage::write_atomic(&path, &serde_json::to_vec(&store).unwrap()).unwrap();
        assert!(
            collect(
                &client(),
                unavailable,
                &session("account-a"),
                day(),
                day(),
                now(),
                &path
            )
            .is_err()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    #[test]
    fn failed_today_refresh_cannot_publish_a_persisted_partial_day() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("history.json");
        let mut sample = event("today");
        sample["timestamp"] = json!(now().timestamp_millis() - 60000);
        let (url, join) = server(vec![page(1, sample)]);
        let date = now().date_naive();
        assert!(
            collect(
                &client(),
                &url,
                &session("account-a"),
                date,
                date,
                now(),
                &path
            )
            .is_ok()
        );
        join.join().unwrap();
        assert!(
            collect(
                &client(),
                "http://127.0.0.1:1/events",
                &session("account-a"),
                date,
                date,
                now(),
                &path
            )
            .is_err()
        );
    }
}
