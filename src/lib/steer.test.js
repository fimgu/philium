import test from "node:test";
import assert from "node:assert/strict";
import { steerKey } from "./steer.js";

test("steer key is stable for a retry and distinct otherwise", () => {
  assert.equal(steerKey("a", "hi", 1_000), steerKey("a", "hi", 9_000));
  assert.notEqual(steerKey("a", "hi", 1_000), steerKey("b", "hi", 1_000));
  assert.notEqual(steerKey("a", "hi", 1_000), steerKey("a", "ho", 1_000));
  assert.notEqual(steerKey("a", "hi", 1_000), steerKey("a", "hi", 21_000));
});
