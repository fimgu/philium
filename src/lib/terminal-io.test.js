import { test } from "node:test";
import assert from "node:assert/strict";
import xterm from "@xterm/xterm";
import { terminalIO } from "./terminal-io.js";

const bytes = (text) => new TextEncoder().encode(text);
const da1 = "\x1b[c";
const da2 = "\x1b[>c";
const osc11 = "\x1b]11;?\x1b\\";
const da1Reply = "\x1b[?1;2c";
const osc11Reply = "\x1b]11;rgb:1111/1313/1818\x1b\\";

function harness(t) {
  const term = new xterm.Terminal();
  const sent = [];
  let active = true;
  const io = terminalIO(term, () => active, (data) => sent.push(data));
  t.after(() => { io.dispose(); term.dispose(); });
  // A DOM-less xterm has no theme service. Simulate its RGB response at
  // the parser boundary; DA replies below come from xterm itself.
  term.parser.registerOscHandler(11, (data) => {
    if (data === "?") term.input(osc11Reply, false);
    return true;
  });
  return { term, io, sent, setActive: (value) => { active = value; } };
}

const drain = (term) => new Promise((resolve) => term.write(bytes(""), resolve));

test("scrollback renders without echoing OSC RGB or DA, then live queries and user input still work", async (t) => {
  const { term, io, sent } = harness(t);
  io.write(bytes(`old output\r\n${osc11}${da1}${da2}`), { replay: true });
  await drain(term);
  assert.equal(term.buffer.active.getLine(0).translateToString(true), "old output");
  assert.deepEqual(sent, []);

  io.write(bytes(`${osc11}${da1}`));
  await drain(term);
  term.input("echo café\r");
  term.input("\x1b[A");
  assert.deepEqual(sent, [osc11Reply, da1Reply, "echo café\r", "\x1b[A"]);
});

test("interleaved delayed replay mutes only its own replies and handles split escape sequences", async (t) => {
  const { term, io, sent, setActive } = harness(t);
  io.write(bytes(da1));
  setActive(false);
  io.write(bytes("\x1b]11;?"), { replay: true });
  io.write(bytes(`\x1b\\${da2}`), { replay: true });
  setActive(true); // Screen is visible again before xterm parses the writes.
  io.write(bytes(da1));
  await drain(term);
  assert.deepEqual(sent, [da1Reply, da1Reply]);
});

test("inactive views and disposed IO cannot send input or parser replies", async (t) => {
  const { term, io, sent, setActive } = harness(t);
  setActive(false);
  io.write(bytes(da1));
  await drain(term);
  term.input("inactive");
  assert.deepEqual(sent, []);
  setActive(true);
  io.dispose();
  term.write(bytes(da1));
  await drain(term);
  term.input("disposed");
  assert.deepEqual(sent, []);
});
