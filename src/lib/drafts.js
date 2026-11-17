import { createSignal } from "solid-js";

// One serialized queue covers restores and writes. No browser storage or agent RPC.
export function createDraftStore(invoke) {
  const drafts = new Map();
  let queue = Promise.resolve();
  const enqueue = (task) => { const result = queue.then(task); queue = result.catch(() => {}); return result; };
  function get(key) {
    if (drafts.has(key)) return drafts.get(key);
    const record = newDraft();
    record.setReady(false);
    drafts.set(key, record);
    const rawText = record.setText, rawImages = record.setImages;
    let restored = false, operations = [], revision = 0, scheduled = false;
    const snapshot = () => ({ text: record.text(), images: record.images().map(({ data, media_type, name }) => ({ data, media_type, name })) });
    const save = () => {
      if (!restored || scheduled) return;
      scheduled = true;
      let version;
      record.setPending(true);
      enqueue(() => {
        scheduled = false;
        version = revision;
        return invoke("drafts_save", { key, draft: snapshot() });
      }).then(() => {
        if (version === revision) { record.setPending(false); record.setError(""); }
      }).catch(() => {
        record.setError("Draft could not be saved. Keep this window open and retry.");
        if (version === revision) record.setPending(false);
      });
    };
    const change = (setter, value) => {
      revision++;
      setter(value);
      if (!restored) operations.push(() => setter(value));
      save();
    };
    record.setText = (value) => change(rawText, value);
    record.setImages = (value) => change(rawImages, value);
    record.retry = () => restored ? save() : restore();
    function restore() {
      record.setLoading(true);
      enqueue(() => invoke("drafts_load", { key })).then((value) => {
        rawText(value?.text || "");
        rawImages((value?.images || []).map((im) => ({ ...im, preview: `data:${im.media_type};base64,${im.data}` })));
        operations.forEach((operation) => operation());
        const changed = operations.length > 0;
        operations = [];
        restored = true;
        record.setError("");
        record.setReady(true);
        record.setLoading(false);
        if (changed) save();
      }).catch(() => { record.setLoading(false); record.setError("Draft could not be restored. Retry before editing or sending."); });
    }
    restore();
    return record;
  }
  return { draftFor: get };
}
export function validateDraftImages(files) {
  const supported = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);
  if (files.some((file) => !supported.has(file.type))) throw new Error("Unsupported image type. Use PNG, JPEG, GIF or WebP.");
  if (files.some((file) => file.size > 5 * 1024 * 1024)) throw new Error("Images must be 5 MiB or smaller.");
}

export function newDraft() {
  const [text, setText] = createSignal("");
  const [images, setImages] = createSignal([]);
  const [sending, setSending] = createSignal(false);
  const [ready, setReady] = createSignal(true);
  const [loading, setLoading] = createSignal(false);
  const [pending, setPending] = createSignal(false);
  const [error, setError] = createSignal("");
  return { text, setText, images, setImages, sending, setSending, ready, setReady, loading, setLoading, pending, setPending, error, setError };
}
const store = createDraftStore(async (command, args) => {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke(command, args);
});
export const draftFor = store.draftFor;
export function clearSentDraft(target, text, images) {
  if (target.text() === text) target.setText("");
  target.setImages((current) => current.filter((image) => !images.includes(image)));
}
export function appendAgentDraft(agentId, text) {
  if (!agentId || !text) return;
  draftFor(`agent:${agentId}`).setText((old) => old ? `${old}\n\n${text}` : text);
}
