import test from "node:test";
import assert from "node:assert/strict";
import { publishSteps } from "./publish-steps.js";

test("steps follow commit, push, then PR", () => {
  const s = { branch: "f", remote: "origin", ahead: "2", files: [{ path: "a" }] };
  assert.deepEqual(publishSteps(s, ["a"]), { canCommit: true, canPush: true, canPr: false });
  assert.equal(publishSteps(s, ["zzz"]).canCommit, false);
  assert.equal(publishSteps(s, []).canCommit, false);
  assert.deepEqual(publishSteps({ ...s, ahead: "0" }, []), { canCommit: false, canPush: false, canPr: true });
  assert.equal(publishSteps({ ...s, ahead: null }, []).canPush, true);
  assert.equal(publishSteps({ ...s, remote: null }, []).canPush, false);
});
