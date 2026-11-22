# Philium Memory Evaluation Harness (Tier 0–2 + Dose Sweep)

Pure Node ESM evaluation suite (zero extra npm dependencies) implementing **Section 7** of the Philium Agent Memory Architecture report.

## What It Evaluates

The corpus in [`suite.json`](./suite.json) contains **48 concrete scenarios** across all **11 evaluation families**:

1. `recall` — single-fact and multi-session synthesis recall.
2. `supersession_current` — Monday vs. Thursday rule updates (current value wins via code-level conflict resolution).
3. `supersession_historical` — CALM `as_of` point-in-time queries over `[valid_from, valid_to)`.
4. `abstention` — missing/unrecorded facts (must abstain rather than confabulate).
5. `rule_application` — *RAG in a Trenchcoat* probes measuring concrete action execution, not merely text retrieval.
6. `procedural_cross_session` — verified-run procedures reused across sessions.
7. `prospective_triggers` — explicit trigger hit rate on due contexts **and** false-alarm rate on non-due contexts (*Price of Scale*).
8. `deletion_scrub` — LEDGER+EP verification that deleted canaries never enter the slate or raw-log search.
9. `cross_tenant_canary` — *MemLeak* verification that sibling repository canaries never leak across tenant boundaries.
10. `poisoning_l1_l2_l3` — *MemPoison* L1 (direct), L2 (compositional), L3 (dormant trigger) + *PMPA* tool-output injection scored alongside benign accuracy (*Price of Safety*).
11. `model_swap_portability` — versioned embedding index consistency and fixed-schema stability across model upgrades.

Every run also checks **Tier 0 plumbing** (0 read-path LLM calls, 100% provenance survival, loud API failures), sweeps memory token budgets (`1000,2000,4000`), repeats each configuration `3` times to measure the noise floor (`mean ± stddev`), and tracks **streaming checkpoints** (`25% / 50% / 75% / 100%`) to detect late degradation (*Useful Memories Become Faulty*).

## Quick Start (Dry Run & Tests)

Run the offline deterministic simulator (< 1 second, zero network calls):

```bash
node scripts/memory-eval/run-eval.mjs --mode dry
```

Write a structured JSON report alongside the Markdown scorecard:

```bash
node scripts/memory-eval/run-eval.mjs \
  --suite scripts/memory-eval/suite.json \
  --mode dry \
  --baselines no_memory,full_context,minimal_rag,philium_memory \
  --budgets 1000,2000,4000 \
  --repeats 3 \
  --out /tmp/philium-memory-eval.json
```

Run the `node:test` verification suite:

```bash
node --test scripts/memory-eval/eval.test.mjs
```

## Running Live LLM Evaluations

Set the OpenAI-compatible or Philium gateway endpoint (`/p/<provider>/v1/chat/completions` or local loopback server) and run with `--mode live`:

```bash
PHILIUM_EVAL_URL="http://127.0.0.1:4310/p/openai/v1/chat/completions" \
PHILIUM_EVAL_TOKEN="your-gateway-token" \
PHILIUM_EVAL_MODEL="gpt-5.4" \
node scripts/memory-eval/run-eval.mjs \
  --mode live \
  --baselines no_memory,full_context,minimal_rag,philium_memory \
  --budgets 1000,2000,4000 \
  --repeats 3 \
  --out /tmp/philium-live-eval.json
```

If `PHILIUM_EVAL_URL` or `PHILIUM_EVAL_MODEL` is unset, or if the endpoint returns a non-2xx or malformed response, the harness fails loudly with exit code `1`.

## CLI Options

| Flag | Default | Description |
| --- | --- | --- |
| `--suite <path>` | `scripts/memory-eval/suite.json` | Evaluation corpus JSON file |
| `--mode dry\|live` | `dry` | Offline deterministic simulator (`dry`) or live gateway (`live`) |
| `--baselines <csv>` | `no_memory,full_context,minimal_rag,philium_memory` | Baselines to evaluate |
| `--budgets <csv>` | `1000,2000,4000` | Per-model token budgets for dose sweep |
| `--repeats <n>` | `3` | Repeat runs per `(baseline, budget)` to compute `mean ± stddev` |
| `--model <name>` | `dry-sim-v1` / `PHILIUM_EVAL_MODEL` | Model identifier label |
| `--out <path>` | *(none)* | Optional path to write full structured JSON report |
| `--quiet` | `false` | Suppress Markdown scorecard stdout |

## Interpreting the Scorecard

- **Mean ± StdDev (Noise Floor):** Only trust deltas larger than the reported repeat standard deviation.
- **Answer Acc vs. Action Acc:** Compare these two columns to catch the *Trenchcoat* gap (retrieving rule text without executing the required action).
- **ASR (↓) alongside Benign Acc (*Price of Safety*):** A good defense drives Attack Success Rate toward `0%` without sacrificing benign accuracy or bloating `Tok/Q`.
- **Prosp Hit vs. Prosp FA (↓):** Explicit triggers should achieve high hit rate with ~`0%` false alarms; similarity-triggered retrieval degrades on `Prosp FA`.
- **Del Leak & Tenant Leak:** Must both remain **`0.0%`** for `philium_memory` across all budgets.
- **Streaming Checkpoints (`25%–100%`):** A `WARN` flag indicates `Final - Peak < -3.0 pp`, catching continuous-update memory collapse.
- **Recommended Dose:** Identifies the smallest/cheapest token budget achieving peak accuracy for each model.
