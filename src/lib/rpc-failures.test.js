import test from "node:test";
import assert from "node:assert/strict";
import { track, failures, clearFailures, toRpcError, isNotFound } from "./rpc.js";

const fail = (kind) => Promise.reject({ kind, message: "x" });

test("not-found and non-core failures never reach the banner", async () => {
  clearFailures();
  for (let i = 0; i < 3; i++) await track("agents.list", fail("not_found")).catch(() => {});
  for (let i = 0; i < 3; i++) await track("board.get", fail("failed")).catch(() => {});
  assert.deepEqual(failures(), {});
});

test("core failure shows only after two strikes and clears on success", async () => {
  clearFailures();
  await track("app.status", fail("failed")).catch(() => {});
  assert.deepEqual(Object.keys(failures()), []);
  await track("app.status", fail("failed")).catch(() => {});
  assert.deepEqual(Object.keys(failures()), ["app.status"]);
  await track("app.status", Promise.resolve(1));
  assert.deepEqual(failures(), {});
});

test("structured errors keep kind and message", () => {
  const e = toRpcError({ kind: "not_found", message: "Task not found" });
  assert.ok(isNotFound(e));
  assert.equal(e.message, "Task not found");
  assert.equal(toRpcError("boom").kind, "failed");
});
