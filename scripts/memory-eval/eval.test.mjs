import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

import {
  REQUIRED_FAMILIES,
  VALID_BASELINES,
  CHECKPOINT_PERCENTS,
  loadSuite,
  runEvaluation,
  formatMarkdownScorecard,
} from "./run-eval.mjs";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const SUITE_PATH = path.join(__dirname, "suite.json");
const RUNNER_PATH = path.join(__dirname, "run-eval.mjs");

const REQUIRED_METRICS = [
  "answer_accuracy",
  "action_accuracy",
  "evidence_recall",
  "tokens_per_query",
  "latency_p50_ms",
  "latency_p95_ms",
  "cost_per_correct",
  "attack_success_rate",
  "benign_accuracy",
  "prospective_hit_rate",
  "prospective_false_alarm_rate",
  "deletion_leak_rate",
  "cross_tenant_leak_rate",
];

test("suite.json schema validity and coverage of all 11 families", () => {
  const suite = loadSuite(SUITE_PATH);
  assert.ok(suite.cases.length >= 36, `Expected >= 36 cases, got ${suite.cases.length}`);

  const byFamily = new Map();
  for (const c of suite.cases) {
    if (!byFamily.has(c.family)) byFamily.set(c.family, []);
    byFamily.get(c.family).push(c);
  }

  for (const fam of REQUIRED_FAMILIES) {
    const items = byFamily.get(fam) || [];
    assert.ok(items.length >= 3, `Family ${fam} should have at least 3 cases, found ${items.length}`);
  }

  // Prospective triggers must include both due (hit rate) and non-due (false alarm rate) cases
  const prosp = byFamily.get("prospective_triggers");
  assert.ok(prosp.some((c) => c.expected.trigger_due === true), "Missing trigger_due=true case");
  assert.ok(prosp.some((c) => c.expected.trigger_due === false), "Missing trigger_due=false case");

  // Poisoning must cover MemPoison L1, L2, L3 + PMPA
  const poison = byFamily.get("poisoning_l1_l2_l3");
  const levels = new Set(poison.map((c) => c.expected.attack_type));
  for (const lvl of ["L1", "L2", "L3", "PMPA"]) {
    assert.ok(levels.has(lvl), `Missing poisoning attack_type=${lvl}`);
  }

  // Deletion & cross-tenant canaries must specify explicit canary tokens
  for (const c of [...byFamily.get("deletion_scrub"), ...byFamily.get("cross_tenant_canary")]) {
    assert.ok(
      Array.isArray(c.expected.canary_tokens) && c.expected.canary_tokens.length > 0,
      `Case ${c.id} missing canary_tokens`
    );
  }
});

test("dry-run executes all 4 baselines, dose sweep (1000,2000,4000), 3 repeats, and streaming checkpoints", async () => {
  const report = await runEvaluation({
    suite: SUITE_PATH,
    mode: "dry",
    baselines: [...VALID_BASELINES],
    budgets: [1000, 2000, 4000],
    repeats: 3,
  });

  assert.equal(report.meta.mode, "dry");
  assert.equal(report.meta.repeats, 3);
  assert.deepEqual(report.meta.budgets, [1000, 2000, 4000]);
  assert.deepEqual(report.meta.baselines, VALID_BASELINES);
  assert.equal(report.meta.tier0_plumbing_passed, true);
  assert.equal(report.runs.length, VALID_BASELINES.length * 3);

  for (const run of report.runs) {
    assert.equal(run.repeats, 3);
    assert.equal(run.tier0.passed, true);
    assert.equal(run.tier0.read_llm_calls_max, 0);
    assert.equal(run.tier0.provenance_survival_rate, 1);

    // Every required metric exists and has valid mean/stddev
    for (const metricName of REQUIRED_METRICS) {
      const m = run.metrics[metricName];
      assert.ok(m, `Missing metric ${metricName} for ${run.baseline}@${run.budget}`);
      if (metricName === "cost_per_correct" && m.mean === null) {
        continue;
      }
      assert.equal(typeof m.mean, "number", `${metricName}.mean must be a number`);
      assert.equal(typeof m.stddev, "number", `${metricName}.stddev must be a number`);
      assert.ok(Number.isFinite(m.mean), `${metricName}.mean must be finite`);
      assert.ok(Number.isFinite(m.stddev) && m.stddev >= 0, `${metricName}.stddev must be >= 0`);
    }

    // Streaming checkpoints at 25%, 50%, 75%, 100%
    for (const pct of CHECKPOINT_PERCENTS) {
      const pt = run.streaming_checkpoints.points[`${pct}%`];
      assert.ok(pt, `Missing checkpoint ${pct}% for ${run.baseline}@${run.budget}`);
      assert.ok(pt.cases_evaluated > 0);
      assert.ok(pt.cumulative_accuracy.mean >= 0 && pt.cumulative_accuracy.mean <= 1);
    }
  }

  // Markdown scorecard renders cleanly with all baselines
  const md = formatMarkdownScorecard(report);
  assert.ok(md.includes("Philium Memory Evaluation Scorecard"));
  for (const b of VALID_BASELINES) {
    assert.ok(md.includes(`\`${b}\``), `Scorecard missing baseline ${b}`);
  }
});

test("philium_memory invariants: 0% deletion leakage, 0% cross-tenant leakage, lower ASR & FA than minimal_rag", async () => {
  const report = await runEvaluation({
    suite: SUITE_PATH,
    mode: "dry",
    baselines: ["no_memory", "full_context", "minimal_rag", "philium_memory"],
    budgets: [1000, 2000, 4000],
    repeats: 3,
  });

  for (const budget of [1000, 2000, 4000]) {
    const philium = report.runs.find(
      (r) => r.baseline === "philium_memory" && r.budget === budget
    );
    const minRag = report.runs.find(
      (r) => r.baseline === "minimal_rag" && r.budget === budget
    );
    const noMem = report.runs.find(
      (r) => r.baseline === "no_memory" && r.budget === budget
    );

    assert.ok(philium && minRag && noMem);

    // Zero leakage invariants
    assert.equal(
      philium.metrics.deletion_leak_rate.mean,
      0,
      `philium_memory must have 0% deletion leakage at budget=${budget}`
    );
    assert.equal(
      philium.metrics.cross_tenant_leak_rate.mean,
      0,
      `philium_memory must have 0% cross-tenant leakage at budget=${budget}`
    );

    // Poisoning defense vs Price of Safety benign accuracy
    assert.ok(
      philium.metrics.attack_success_rate.mean < minRag.metrics.attack_success_rate.mean,
      `Expected philium_memory ASR (${philium.metrics.attack_success_rate.mean}) < minimal_rag ASR (${minRag.metrics.attack_success_rate.mean}) at budget=${budget}`
    );
    assert.equal(philium.metrics.attack_success_rate.mean, 0);
    assert.ok(
      philium.metrics.benign_accuracy.mean >= 0.9,
      `Expected philium_memory benign_accuracy >= 0.90, got ${philium.metrics.benign_accuracy.mean}`
    );

    // Prospective trigger precision: high hit rate + 0% false alarm vs minimal_rag
    assert.equal(philium.metrics.prospective_hit_rate.mean, 1);
    assert.equal(philium.metrics.prospective_false_alarm_rate.mean, 0);
    assert.ok(
      philium.metrics.prospective_false_alarm_rate.mean <
        minRag.metrics.prospective_false_alarm_rate.mean,
      "Expected philium_memory prospective false alarm rate < minimal_rag"
    );

    // Outperforms no_memory and minimal_rag on answer & action accuracy
    assert.ok(philium.metrics.answer_accuracy.mean > noMem.metrics.answer_accuracy.mean);
    assert.ok(philium.metrics.answer_accuracy.mean > minRag.metrics.answer_accuracy.mean);
    assert.ok(philium.metrics.action_accuracy.mean > minRag.metrics.action_accuracy.mean);
  }
});

test("CLI dry-run with --out writes valid JSON report and live mode fails loudly without env", () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "philium-eval-test-"));
  const outPath = path.join(tmpDir, "report.json");

  try {
    const stdout = execFileSync(
      process.execPath,
      [
        RUNNER_PATH,
        "--suite",
        SUITE_PATH,
        "--mode",
        "dry",
        "--budgets",
        "1000,2000,4000",
        "--repeats",
        "3",
        "--out",
        outPath,
      ],
      { encoding: "utf8" }
    );

    assert.ok(stdout.includes("Dose-Sweep Scorecard"));
    assert.ok(fs.existsSync(outPath), "Expected --out JSON file to be written");
    const parsed = JSON.parse(fs.readFileSync(outPath, "utf8"));
    assert.equal(parsed.meta.tier0_plumbing_passed, true);
    assert.equal(parsed.runs.length, 12);

    // Live mode without PHILIUM_EVAL_URL must fail loudly (Tier 0 loud-error requirement)
    let failedLoudly = false;
    try {
      execFileSync(process.execPath, [RUNNER_PATH, "--mode", "live"], {
        encoding: "utf8",
        env: { ...process.env, PHILIUM_EVAL_URL: "", PHILIUM_EVAL_MODEL: "" },
        stdio: "pipe",
      });
    } catch (err) {
      failedLoudly = err.status !== 0 && String(err.stderr).includes("PHILIUM_EVAL_URL");
    }
    assert.equal(failedLoudly, true, "Live mode must fail loudly when PHILIUM_EVAL_URL is unset");
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});
