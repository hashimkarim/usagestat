import json
import tempfile
import unittest
from pathlib import Path

import sync_litellm as sync

CATALOG = {
    "schema": 1,
    "asOf": "2026-11-16",
    "models": [{"ids": ["gpt-6.1-sol"], "input": 2, "output": 10, "cacheWrite": 2.5, "cacheRead": 0.1}],
}


def entry(provider, **costs):
    return {"litellm_provider": provider, "mode": "chat", **costs}


SOURCE = {
    # New OpenAI model with a long-context tier and no explicit cache-write price.
    "gpt-6.2-sol": entry("openai", input_cost_per_token=2e-06, output_cost_per_token=1e-05,
                         cache_read_input_token_cost=1e-07,
                         input_cost_per_token_above_272k_tokens=4e-06,
                         output_cost_per_token_above_272k_tokens=1.5e-05,
                         cache_read_input_token_cost_above_272k_tokens=2e-07),
    # Already curated, but LiteLLM disagrees: reported, never overwritten.
    "gpt-6.1-sol": entry("openai", input_cost_per_token=3e-06, output_cost_per_token=1e-05,
                         cache_read_input_token_cost=1e-07, cache_creation_input_token_cost=2.5e-06),
    # Anthropic needs an explicit cache-write price.
    "claude-sonnet-6": entry("anthropic", input_cost_per_token=3e-06, output_cost_per_token=1.5e-05,
                             cache_read_input_token_cost=3e-07, cache_creation_input_token_cost=3.75e-06),
    "claude-opus-6": entry("anthropic", input_cost_per_token=5e-06, output_cost_per_token=2.5e-05,
                           cache_read_input_token_cost=5e-07),
    # Never imported: snapshots, other providers, prefixed keys, other families, non-chat.
    "gpt-6.2-sol-2026-11-01": entry("openai", input_cost_per_token=2e-06, output_cost_per_token=1e-05,
                                    cache_read_input_token_cost=1e-07),
    "azure/gpt-7": entry("azure", input_cost_per_token=1e-06, output_cost_per_token=1e-06,
                         cache_read_input_token_cost=1e-07),
    "anthropic.claude-sonnet-6": entry("bedrock_converse", input_cost_per_token=3e-06,
                                       output_cost_per_token=1.5e-05, cache_read_input_token_cost=3e-07,
                                       cache_creation_input_token_cost=3.75e-06),
    "gpt-4o": entry("openai", input_cost_per_token=2.5e-06, output_cost_per_token=1e-05,
                    cache_read_input_token_cost=1.25e-06),
    "gpt-image-2": {"litellm_provider": "openai", "mode": "image_generation"},
}


class SyncTest(unittest.TestCase):
    def run_sync(self):
        return sync.sync(json.loads(json.dumps(CATALOG)), SOURCE, "2026-11-20")

    def test_adds_only_exact_new_ids_from_mapped_families(self):
        catalog, added, differs = self.run_sync()
        self.assertEqual(added, ["claude-sonnet-6", "gpt-6.2-sol"])
        self.assertEqual(differs, ["gpt-6.1-sol"])
        self.assertEqual(catalog["asOf"], "2026-11-20")
        sol = next(e for e in catalog["models"] if e["ids"] == ["gpt-6.2-sol"])
        self.assertEqual(sol["cacheWrite"], 2.0)
        self.assertEqual(sol["above"], {"tokens": 272000, "input": 4.0, "output": 15.0,
                                        "cacheRead": 0.2, "cacheWrite": 4.0})

    def test_curated_entries_are_never_changed(self):
        catalog, _, _ = self.run_sync()
        self.assertEqual(catalog["models"][0], CATALOG["models"][0])

    def test_report_lists_additions_and_disagreements(self):
        catalog, added, differs = self.run_sync()
        body = sync.report(catalog, SOURCE, added, differs)
        self.assertIn("`gpt-6.2-sol`: $2 in · $10 out", body)
        self.assertIn("`gpt-6.1-sol`: catalog $2 in", body)

    def test_cli_leaves_an_unchanged_catalog_untouched(self):
        with tempfile.TemporaryDirectory() as tmp:
            catalog = Path(tmp, "models.json")
            source = Path(tmp, "source.json")
            catalog.write_text(json.dumps(CATALOG))
            source.write_text(json.dumps({"gpt-6.1-sol": SOURCE["gpt-6.1-sol"]}))
            before = catalog.read_text()
            sync.main(["--catalog", str(catalog), "--source", str(source), "--report", str(Path(tmp, "r.md"))])
            self.assertEqual(catalog.read_text(), before)
            self.assertTrue(Path(tmp, "r.md").exists(), "a disagreement is still reported")

    def test_bundled_catalog_matches_litellm_shape(self):
        bundled = json.loads(sync.CATALOG.read_text())
        self.assertEqual(bundled["schema"], 1)
        for model in bundled["models"]:
            self.assertTrue(set(model) <= {"ids", "input", "output", "cacheRead", "cacheWrite", "above"})


if __name__ == "__main__":
    unittest.main()
