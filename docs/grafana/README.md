# Grafana dashboard

[`usagestat-dashboard.json`](usagestat-dashboard.json) charts the daemon's
Prometheus endpoint (`GET /metrics`, see
[`prometheus.rs`](../../crates/usagestat-daemon/src/prometheus.rs)):

| Metric | Labels | Meaning |
| --- | --- | --- |
| `usagestat_provider_up` | `provider` | 1 when the latest probe succeeded |
| `usagestat_usage_ratio` | `provider`, `window` | Used fraction (0–1) of an allowance window |
| `usagestat_fetched_timestamp_seconds` | `provider` | When the provider was last measured |
| `usagestat_reset_timestamp_seconds` | `provider`, `window` | When the allowance window resets |

Panels show providers up and failing, the most-used window, windows above 90%,
measurement age, the next reset, quota used per provider and window (current
and over time), a reset table with countdowns, and probe status history.
Failing providers export only `provider_up` and the measurement time, so their
quota panels show gaps instead of zero usage.

The `instance` and `provider` variables filter every panel; the `datasource`
variable picks any Prometheus data source.

## Scrape the daemon

`/metrics` only includes enabled providers and never exports account names,
credentials or error messages. Add a job to `prometheus.yml`:

```yaml
scrape_configs:
  - job_name: usagestat
    scrape_interval: 60s
    static_configs:
      - targets: ["127.0.0.1:6736"]
```

The daemon listens on loopback by default. To scrape it from another host,
bind it to a reachable address (`usagestatd --bind`) behind your own network
access controls, or run Prometheus on the same machine.

## Import

- **UI:** Dashboards → New → Import, upload `usagestat-dashboard.json`, then
  choose your Prometheus data source.
- **Provisioning:** copy
  [`provisioning/usagestat-dashboards.yaml`](provisioning/usagestat-dashboards.yaml)
  to `/etc/grafana/provisioning/dashboards/` and the dashboard JSON to the
  `path` it names (`/var/lib/grafana/dashboards/usagestat`). Grafana uses the
  default Prometheus data source until another is selected.

The dashboard has the fixed uid `usagestat-overview`, so re-importing or
re-provisioning updates it in place.
