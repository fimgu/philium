import test from "node:test";
import assert from "node:assert/strict";
import { conversationActivity, streamLabel, STALL_HINT_MS, STALL_MS } from "./conversation-activity.js";

const started = [{ type: "turn.started", turn_id: "t1" }, { type: "model.started" }];
const at = (idle, stream) => ({ now: 1_000_000, lastByteAt: 1_000_000 - idle, stream });

test("inactive when not busy or read-only", () => {
  assert.equal(conversationActivity(started, false), null);
  assert.equal(conversationActivity(started, true, true), null);
});

test("waiting with no bytes yet and no hint before 30s", () => {
  const a = conversationActivity(started, true, false, at(10_000));
  assert.equal(a.kind, "waiting");
  assert.equal(a.hint, null);
});

test("thinking stream shows Thinking with token meter", () => {
  const a = conversationActivity(started, true, false, at(500, { phase: "thinking", bytes: 4000, tokens: 812 }));
  assert.equal(a.label, "Thinking…");
  assert.equal(a.meter, "~812 tokens");
  assert.equal(a.hint, null);
});

test("long tool-call streaming over 100s is not a stall while bytes flow", () => {
  const a = conversationActivity(started, true, false, at(1_000, { phase: "tool", tool: "mcp_philium_write", file: "gateway.rs", bytes: 42000, tokens: 10500 }));
  assert.equal(a.label, "Writing edit to gateway.rs…");
  assert.equal(a.kind, "streaming");
  assert.equal(a.hint, null);
});

test("tool without file shows Preparing <tool>", () => {
  assert.equal(streamLabel({ phase: "tool", tool: "mcp_philium_shell_run" }), "Preparing shell_run…");
  assert.equal(streamLabel({ phase: "tool", tool: "read", file: "a.js" }), "Preparing read for a.js…");
});

test("hint only after 30s with no bytes", () => {
  assert.equal(conversationActivity(started, true, false, at(STALL_HINT_MS - 1)).hint, null);
  assert.match(conversationActivity(started, true, false, at(STALL_HINT_MS + 1)).hint, /Still waiting/);
  const s = conversationActivity(started, true, false, at(STALL_HINT_MS + 1, { phase: "tool", tool: "write", bytes: 10 }));
  assert.match(s.hint, /Still waiting/);
});

test("stall after 90s without bytes, or when gateway flags it", () => {
  assert.equal(conversationActivity(started, true, false, at(STALL_MS + 1)).kind, "stalled");
  assert.equal(conversationActivity(started, true, false, at(0, { stalled: true })).kind, "stalled");
});

test("running tool and permissions take precedence; tools are not stalls", () => {
  const ev = [...started, { type: "model.finished" }, { type: "tool.started", call_id: "c", tool: "mcp_philium_shell_run" }];
  assert.equal(conversationActivity(ev, true, false, at(STALL_MS + 5)).label, "Running shell_run");
  const perm = [...ev, { type: "permission.requested", request_id: "r" }];
  assert.equal(conversationActivity(perm, true).kind, "permission");
});

test("done stream falls back to lifecycle and ended turns hide", () => {
  const a = conversationActivity([...started, { type: "model.delta", delta: "hi" }], true, false, at(0, { phase: "tool", done: true }));
  assert.equal(a.kind, "responding");
  assert.equal(conversationActivity([...started, { type: "assistant.completed" }], true), null);
});
