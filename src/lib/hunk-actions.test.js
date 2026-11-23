import test from "node:test";
import assert from "node:assert/strict";
import { hunkActions } from "./hunk-actions.js";

test("layers only offer operations the backend accepts", () => {
  assert.deepEqual(hunkActions("unstaged").map((a) => a.op), ["stage", "revert"]);
  assert.deepEqual(hunkActions("staged").map((a) => a.op), ["unstage"]);
  assert.deepEqual(hunkActions("base"), []);
});
