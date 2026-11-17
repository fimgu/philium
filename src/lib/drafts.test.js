import test from "node:test";
import assert from "node:assert/strict";
import { createDraftStore, clearSentDraft, validateDraftImages } from "./drafts.js";
const tick = () => new Promise((resolve) => setImmediate(resolve));
const image = { data: "aGk=", media_type: "image/png", name: "test.png" };
test("image admission matches backend MIME allowlist and byte boundary", () => {
  const limit = 5 * 1024 * 1024;
  for (const type of ["image/png", "image/jpeg", "image/gif", "image/webp"]) {
    assert.doesNotThrow(() => validateDraftImages([{ type, size: limit }]));
  }
  for (const type of ["image/svg+xml", "image/heic", "image/bmp", "", "text/plain"]) {
    assert.throws(() => validateDraftImages([{ type: "image/png", size: 1 }, { type, size: 1 }]), /Unsupported image type/);
  }
  assert.throws(() => validateDraftImages([{ type: "image/png", size: limit + 1 }]), /5 MiB/);
});
function backend() {
  const disk = new Map();
  return { disk, invoke: async (cmd, { key, draft }) => {
    if (cmd === "drafts_load") return structuredClone(disk.get(key) || { text: "", images: [] });
    disk.set(key, structuredClone(draft));
  } };
}
test("restart restores text, attachments and preview without leaking across agents", async () => {
  const b = backend(), store = createDraftStore(b.invoke);
  const a = store.draftFor("agent:a"), peer = store.draftFor("agent:b");
  await tick();
  a.setText("private a"); a.setImages([image]); peer.setText("private b");
  await tick();
  const restart = createDraftStore(b.invoke), restored = restart.draftFor("agent:a");
  await tick();
  assert.equal(restored.text(), "private a");
  assert.equal(restored.images()[0].preview, "data:image/png;base64,aGk=");
  assert.equal(b.disk.get("agent:b").text, "private b");
  assert.equal("preview" in b.disk.get("agent:a").images[0], false);
});
test("late restore replays edits and append operations without overwriting saved content", async () => {
  let resolve;
  const saved = [];
  const store = createDraftStore((cmd, args) => cmd === "drafts_load" ? new Promise((r) => { resolve = r; }) : (saved.push(args.draft), Promise.resolve()));
  const a = store.draftFor("agent:a");
  a.setText((old) => `${old} appended`);
  await tick();
  resolve({ text: "saved", images: [image] });
  await tick();
  assert.equal(a.text(), "saved appended");
  assert.equal(a.images().length, 1);
  assert.equal(saved.at(-1).text, "saved appended");
});
test("successful send clears only unchanged sent content and persists clear", async () => {
  const b = backend(), a = createDraftStore(b.invoke).draftFor("agent:a");
  await tick();
  a.setText("sent"); a.setImages([image]);
  const text = a.text(), attached = a.images();
  const later = { ...image, name: "later.png" };
  a.setText("new edit"); a.setImages([...attached, later]);
  clearSentDraft(a, text, attached);
  await tick();
  assert.equal(a.text(), "new edit"); assert.deepEqual(a.images(), [later]);
  clearSentDraft(a, a.text(), a.images());
  await tick();
  assert.deepEqual(b.disk.get("agent:a"), { text: "", images: [] });
});
test("writes serialize, pending is visible, failures are visible and retryable", async () => {
  const writes = [], releases = [];
  let fail = false;
  const a = createDraftStore(async (cmd, args) => {
    if (cmd === "drafts_load") return { text: "", images: [] };
    writes.push(args.draft.text);
    await new Promise((r) => releases.push(r));
    if (fail) throw new Error("disk failed");
  }).draftFor("agent:a");
  await tick(); a.setText("one"); await tick(); a.setText("two"); await tick();
  assert.deepEqual(writes, ["one"]); assert.equal(a.pending(), true);
  releases.shift()(); await tick();
  assert.deepEqual(writes, ["one", "two"]);
  fail = true; releases.shift()(); await tick();
  assert.match(a.error(), /could not be saved/);
  fail = false; a.retry(); await tick(); releases.shift()(); await tick();
  assert.equal(a.error(), ""); assert.equal(a.pending(), false);
});
test("failed restore never writes over unread draft and retry preserves queued additions", async () => {
  let fail = true, writes = 0;
  const a = createDraftStore(async (cmd) => {
    if (cmd === "drafts_save") { writes++; return; }
    if (fail) throw new Error("unavailable");
    return { text: "saved", images: [] };
  }).draftFor("agent:a");
  a.setText((old) => `${old} append`); await tick();
  assert.equal(a.ready(), false); assert.match(a.error(), /restored/); assert.equal(writes, 0);
  fail = false; a.retry(); await tick();
  assert.equal(a.text(), "saved append"); assert.equal(a.ready(), true); assert.equal(writes, 1);
});
