import test from "node:test";
import assert from "node:assert/strict";
import { plainError } from "./plain-error.js";

test("provider and git failures read as one sentence with the fix, and keep the original", () => {
  const limited = plainError('upstream returned 429 Too Many Requests: {"type":"error","error":{"type":"rate_limit_error"}} retry-after: 7');
  assert.match(limited.plain, /limiting requests right now and asks to wait 7 seconds\. Send again in a minute, or switch model with \/model\./);
  assert.match(limited.raw, /rate_limit_error/);
  assert.match(plainError("HTTP 401: invalid x-api-key").plain, /refused the sign-in\. Open Providers/);
  assert.match(plainError('{"type":"overloaded_error"}').plain, /overloaded/);
  assert.match(plainError("prompt is too long: 210000 tokens > 200000 maximum").plain, /no longer fits the model/);
  assert.match(plainError("error sending request for url (http://127.0.0.1:9/v1)").plain, /could not reach the model provider/);
  assert.match(plainError("fatal: Needed a single revision").plain, /Git could not find the branch or commit/);
  assert.match(plainError("upstream status 502 Bad Gateway").plain, /error on its side/);
});

test("Philium's own messages and unknown text stay as they are", () => {
  const own = "Agents here start from release/v0.9.0, and this checkout has no such branch or commit, locally or from origin.";
  assert.deepEqual(plainError(own), { plain: own, raw: null });
  assert.deepEqual(plainError("used 500 tokens"), { plain: "used 500 tokens", raw: null });
  assert.deepEqual(plainError(null), { plain: "", raw: null });
});
