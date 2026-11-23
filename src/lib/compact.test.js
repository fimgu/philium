import test from "node:test";
import assert from "node:assert/strict";
import { compactFocus } from "./compact.js";
test("compact focus is the trimmed suffix, empty when absent", () => {
  assert.equal(compactFocus("/compact"), "");
  assert.equal(compactFocus("/COMPACT   keep the API notes  "), "keep the API notes");
});
