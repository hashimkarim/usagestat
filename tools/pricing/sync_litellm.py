#!/usr/bin/env python3
"""Add newly published model prices from LiteLLM to pricing/models.json.

Only exact model IDs from the mapped providers and families are added. An
existing entry is never edited. When LiteLLM's rates differ from a model's
latest catalog rates, a new period starting today is added, so usage before
today keeps the old price; the pull request lists it for review (LiteLLM may
have corrected an error rather than the provider changing its price). Prices
are USD per million tokens.

    python3 tools/pricing/sync_litellm.py --source prices.json --report body.md
"""
from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import sys
import urllib.request
from pathlib import Path

LITELLM_URL = (
    "https://raw.githubusercontent.com/BerriAI/litellm/main/"
    "model_prices_and_context_window.json"
)
CATALOG = Path(__file__).resolve().parents[2] / "pricing" / "models.json"
MAX_SOURCE_BYTES = 32 * 1024 * 1024

# The model families UsageStat reads from local Codex and Claude Code logs.
FAMILIES = {
    "openai": re.compile(r"^gpt-[5-9](\.\d+)?(-[a-z]+)*$"),
    "anthropic": re.compile(r"^claude-(opus|sonnet|haiku)-\d+(-\d+)?$"),
}
# A dated snapshot is matched by its base ID at runtime, so it needs no entry.
SNAPSHOT = re.compile(r"-(\d{8}|\d{4}-\d{2}-\d{2})$")
TIER = re.compile(r"^input_cost_per_token_above_(\d+)k_tokens$")


def per_million(value: float) -> float:
    return round(value * 1_000_000, 6)


def rates_from(entry: dict, provider: str) -> dict | None:
    """Catalog rates for one LiteLLM entry, or None when they are incomplete."""
    try:
        base = {
            "input": per_million(entry["input_cost_per_token"]),
            "output": per_million(entry["output_cost_per_token"]),
            "cacheRead": per_million(entry["cache_read_input_token_cost"]),
        }
    except (KeyError, TypeError):
        return None
    write = entry.get("cache_creation_input_token_cost")
    if write is None:
        # OpenAI bills prompt caching writes as ordinary input.
        if provider != "openai":
            return None
        write = entry["input_cost_per_token"]
    base["cacheWrite"] = per_million(write)

    tiers = sorted({int(m.group(1)) for key in entry if (m := TIER.match(key))})
    if len(tiers) > 1:
        return None  # Several thresholds: not expressible, leave for review.
    if tiers:
        k = tiers[0]
        suffix = f"_above_{k}k_tokens"
        try:
            above = {
                "tokens": k * 1000,
                "input": per_million(entry["input_cost_per_token" + suffix]),
                "output": per_million(entry["output_cost_per_token" + suffix]),
                "cacheRead": per_million(entry["cache_read_input_token_cost" + suffix]),
            }
        except KeyError:
            return None
        tier_write = entry.get("cache_creation_input_token_cost" + suffix)
        if tier_write is None:
            if provider != "openai":
                return None
            tier_write = entry["input_cost_per_token" + suffix]
        above["cacheWrite"] = per_million(tier_write)
        base["above"] = above
    for value in [base[k] for k in ("input", "output", "cacheRead", "cacheWrite")]:
        if not 0 <= value <= 10_000:
            return None
    return base


def candidates(source: dict) -> dict[str, dict]:
    found = {}
    for model, entry in source.items():
        if not isinstance(entry, dict) or entry.get("mode") not in (None, "chat", "responses"):
            continue
        provider = entry.get("litellm_provider")
        family = FAMILIES.get(provider)
        if not family or not family.match(model) or SNAPSHOT.search(model):
            continue
        rates = rates_from(entry, provider)
        if rates is not None:
            found[model] = rates
    return found


def same(a: dict, b: dict) -> bool:
    keys = ("input", "output", "cacheRead", "cacheWrite")
    if any(abs(a[k] - b[k]) > 1e-9 for k in keys):
        return False
    if (a.get("above") is None) != (b.get("above") is None):
        return False
    return a.get("above") is None or (
        a["above"]["tokens"] == b["above"]["tokens"] and same(a["above"], b["above"])
    )


def latest(catalog: dict) -> dict[str, dict]:
    """Each model's most recent price period."""
    found = {}
    for entry in catalog["models"]:
        for model_id in entry["ids"]:
            current = found.get(model_id)
            if current is None or entry.get("from", "") > current.get("from", ""):
                found[model_id] = entry
    return found


def sync(catalog: dict, source: dict, today: str) -> tuple[dict, list[str], list[str]]:
    known = latest(catalog)
    added, changed = [], []
    for model, rates in sorted(candidates(source).items()):
        entry = known.get(model)
        if entry is None:
            catalog["models"].append({"ids": [model], **rates})
            added.append(model)
        elif not same(entry, rates) and entry.get("from") != today:
            catalog["models"].append({"ids": [model], "from": today, **rates})
            changed.append(model)
    if added or changed:
        catalog["asOf"] = max(catalog["asOf"], today)
    return catalog, added, changed


def describe(rates: dict) -> str:
    text = (f"${rates['input']:g} in · ${rates['output']:g} out · "
            f"${rates['cacheRead']:g} cache read · ${rates['cacheWrite']:g} cache write")
    if "above" in rates:
        text += f"; above {rates['above']['tokens']:,} input tokens: {describe(rates['above'])}"
    return text


def report(catalog: dict, source: dict, added: list[str], changed: list[str]) -> str:
    rates = candidates(source)
    lines = ["Model prices from LiteLLM's `model_prices_and_context_window.json`, USD per million tokens.", ""]
    if added:
        lines.append("Added:")
        lines += [f"- `{m}`: {describe(rates[m])}" for m in added]
        lines.append("")
    if changed:
        lines.append("Price changes, as a new period from today (usage before today keeps the earlier rate):")
        for model in changed:
            periods = sorted((e for e in catalog["models"] if model in e["ids"]), key=lambda e: e.get("from", ""))
            lines.append(f"- `{model}`: was {describe(periods[-2])}; now {describe(rates[model])}")
        lines.append("")
        lines.append("If LiteLLM corrected an error rather than the provider changing its price, "
                     "edit the original entry instead and drop the dated one.")
        lines.append("")
    lines.append("Check each price against the provider's own pricing page before merging.")
    return "\n".join(lines) + "\n"


def load_source(location: str) -> dict:
    if re.match(r"^https://", location):
        with urllib.request.urlopen(location, timeout=60) as response:
            data = response.read(MAX_SOURCE_BYTES + 1)
    else:
        data = Path(location).read_bytes()
    if len(data) > MAX_SOURCE_BYTES:
        raise SystemExit("LiteLLM price list is unexpectedly large")
    return json.loads(data)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", default=LITELLM_URL, help="LiteLLM price list URL or file")
    parser.add_argument("--catalog", type=Path, default=CATALOG)
    parser.add_argument("--report", type=Path, help="write a Markdown summary here")
    parser.add_argument("--today", default=dt.date.today().isoformat())
    args = parser.parse_args(argv)

    catalog = json.loads(args.catalog.read_text())
    source = load_source(args.source)
    catalog, added, changed = sync(catalog, source, args.today)
    if added or changed:
        args.catalog.write_text(json.dumps(catalog, indent=2) + "\n")
        if args.report:
            args.report.write_text(report(catalog, source, added, changed))
    print(f"added {len(added)}, price changes {len(changed)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
