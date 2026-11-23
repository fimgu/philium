import test from "node:test";
import assert from "node:assert/strict";
import { safeAnsi } from "./ansi-diff.js";
test("SGR and literal HTML remain data", () => {
  const s = '\x1b[38;2;1;2;3m<script>alert(1)</script>\x1b[0m\n';
  assert.equal(safeAnsi(s), s);
});
test("OSC clipboard, links, DCS and cursor controls are removed", () => {
  assert.equal(safeAnsi('\x1b]52;c;secret\x07hello\x1b]8;;https://invalid\x1b\\world\x1b]8;;\x07'), 'helloworld');
  assert.equal(safeAnsi('\x1bPpayload\x1b\\\x1b[2J\x1b[?25ltext\x07'), 'text');
  assert.equal(safeAnsi('before\x1b]52;c;incomplete'), 'before');
});
