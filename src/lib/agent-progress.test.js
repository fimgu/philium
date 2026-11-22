import test from "node:test";
import assert from "node:assert/strict";
import { currentStep, turnElapsed, fmtDuration } from "./agent-progress.js";

test("current step describes running tool in plain English", () => {
  const s = currentStep({ state: "running" }, { phase: "working", current_tool: { tool: "mcp_philium_shell_run", arguments: { description: "Run tests" } }, tools_running: 1 });
  assert.equal(s, "Shell: Run tests");
});
test("waiting and idle states", () => {
  assert.equal(currentStep({ state: "waiting" }, null), "Waiting for your approval");
  assert.equal(currentStep({ state: "running" }, { phase: "thinking" }), "Thinking");
  assert.equal(currentStep({ state: "idle" }, { phase: "idle" }), "Idle");
});
test("elapsed", () => {
  assert.equal(fmtDuration(65_000), "1m 05s");
  assert.equal(turnElapsed({ phase: "idle", turn_started_at: 0, turn_ended_at: 3_000 }), "");
  assert.equal(turnElapsed({ phase: "idle", turn_started_at: 1, turn_ended_at: 3_001 }), "3s");
  assert.equal(turnElapsed({ phase: "working", turn_started_at: 1000 }, 7_200_000), "1h 59m");
});
