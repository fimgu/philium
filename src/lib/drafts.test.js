import test from "node:test";
import assert from "node:assert/strict";
import { createDraftStore, clearSentDraft, sendableImages } from "./drafts.js";
import { validateImages, createImageCache } from "./images.js";
const tick = () => new Promise((resolve) => setImmediate(resolve));
const image = { sha256: "a".repeat(64), media_type: "image/png", name: "test.png", size: 2 };
test("image admission matches backend MIME allowlist, byte boundary and count", () => {
  const limit = 5 * 1024 * 1024;
  for (const type of ["image/png", "image/jpeg", "image/gif", "image/webp"]) {
    assert.doesNotThrow(() => validateImages([{ type, size: limit }]));
  }
  for (const type of ["image/svg+xml", "image/heic", "image/bmp", "", "text/plain"]) {
    assert.throws(() => validateImages([{ type: "image/png", size: 1 }, { type, size: 1 }]), /Unsupported image type/);
  }
  assert.throws(() => validateImages([{ type: "image/png", size: limit + 1 }]), /5 MiB/);
  assert.throws(() => validateImages([{ type: "image/png", size: 1 }], 8), /Up to 8/);
});
function backend() {
  const disk = new Map();
  const blobs = new Map([["a".repeat(64), new Uint8Array([104, 105])]]);
  return { disk, blobs, invoke: async (cmd, args) => {
    if (cmd === "drafts_blob") { if (!blobs.has(args.sha256)) throw new Error("missing"); return blobs.get(args.sha256); }
    if (cmd === "drafts_put_blob") { const sha256 = String(args.length).padStart(64, "b"); blobs.set(sha256, args); return { sha256, media_type: "image/png", size: args.length }; }
    if (cmd === "drafts_load") return structuredClone(disk.get(args.key) || { text: "", images: [] });
    disk.set(args.key, structuredClone(args.draft));
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
  assert.equal(restored.images()[0].sha256, image.sha256);
  assert.match(restored.images()[0].preview, /^blob:/);
  assert.deepEqual([...new Uint8Array(await restored.images()[0].blob.arrayBuffer())], [104, 105]);
  assert.equal(b.disk.get("agent:b").text, "private b");
  assert.deepEqual(Object.keys(b.disk.get("agent:a").images[0]).sort(), ["media_type", "name", "sha256", "size"]);
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

test("text edits persist only references, never image bytes", async () => {
  const b = backend(), store = createDraftStore(b.invoke), a = store.draftFor("agent:a");
  await tick();
  const stored = await store.attach(new Blob([new Uint8Array(3000)], { type: "image/png" }), "big.png");
  a.setImages([stored]);
  for (let i = 0; i < 5; i++) { a.setText(`edit ${i}`); await tick(); }
  const json = JSON.stringify(b.disk.get("agent:a"));
  assert.ok(json.length < 300, `index payload ${json.length} bytes`);
  assert.equal(json.includes("data"), false);
});
test("missing blob on restore drops the image visibly and persists the repair", async () => {
  const b = backend();
  b.disk.set("agent:a", { text: "keep", images: [{ ...image, sha256: "c".repeat(64) }] });
  const a = createDraftStore(b.invoke).draftFor("agent:a");
  await tick(); await tick();
  assert.equal(a.text(), "keep"); assert.equal(a.images().length, 0);
  assert.match(a.error(), /could not be restored/);
  await tick();
  assert.deepEqual(b.disk.get("agent:a").images, []);
});
test("removing images revokes their object URLs", async () => {
  const revoked = [];
  const urls = { createObjectURL: () => `blob:${revoked.length}-${Math.random()}`, revokeObjectURL: (u) => revoked.push(u) };
  const b = backend(), store = createDraftStore(b.invoke, urls), a = store.draftFor("agent:a");
  await tick();
  const one = await store.attach(new Blob([new Uint8Array([1])], { type: "image/png" }), "one.png");
  a.setImages([one]); a.setImages([]);
  assert.deepEqual(revoked, [one.preview]);
});
test("send payload is built from blobs only at send time", async () => {
  const parts = await sendableImages([{ blob: new Blob([new Uint8Array([104, 105])], { type: "image/png" }), media_type: "image/png", name: "x.png", sha256: "s" }]);
  assert.deepEqual(parts, [{ data: "aGk=", media_type: "image/png", name: "x.png" }]);
});
test("transcript image cache is bounded and revokes evicted URLs", async () => {
  const revoked = [];
  const original = { create: URL.createObjectURL, revoke: URL.revokeObjectURL };
  let n = 0;
  URL.createObjectURL = () => `blob:${n++}`; URL.revokeObjectURL = (u) => revoked.push(u);
  try {
    const cache = createImageCache(async () => new Blob(["x"]), 2);
    await cache.get("a"); await cache.get("b"); await cache.get("a"); await cache.get("c");
    await tick();
    assert.equal(cache.size(), 2); assert.deepEqual(revoked, ["blob:1"]);
    cache.clear(); await tick();
    assert.equal(cache.size(), 0); assert.equal(revoked.length, 3);
  } finally { URL.createObjectURL = original.create; URL.revokeObjectURL = original.revoke; }
});
