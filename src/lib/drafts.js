import { createSignal } from "solid-js";
import { blobToBase64 } from "./images.js";

// Drafts live in private native storage: a small text index plus content-addressed
// image blobs. Typing rewrites only the index. Image bytes are written once, on attach.
// Draft images in memory: { sha256, media_type, name, size, blob, preview(object URL) }.
const SAVE_ERROR = "Draft could not be saved. Keep this window open and retry.";
export function createDraftStore(invoke, urls = globalThis.URL) {
  const drafts = new Map();
  let queue = Promise.resolve();
  const enqueue = (task) => { const result = queue.then(task); queue = result.catch(() => {}); return result; };
  const objectUrl = (blob) => urls?.createObjectURL ? urls.createObjectURL(blob) : "";
  const revoke = (image) => { if (image?.preview && urls?.revokeObjectURL) urls.revokeObjectURL(image.preview); };
  function get(key) {
    if (drafts.has(key)) return drafts.get(key);
    const record = newDraft();
    record.setReady(false);
    drafts.set(key, record);
    const rawText = record.setText, rawImages = record.setImages;
    let restored = false, operations = [], revision = 0, scheduled = false;
    const snapshot = () => ({ text: record.text(), images: record.images().map(({ sha256, media_type, name, size }) => ({ sha256, media_type, name, size })) });
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
        if (version === revision) { record.setPending(false); if (record.error() === SAVE_ERROR) record.setError(""); }
      }).catch(() => {
        record.setError(SAVE_ERROR);
        if (version === revision) record.setPending(false);
      });
    };
    const setImagesTracked = (value) => {
      const before = record.images();
      rawImages(value);
      const after = new Set(record.images());
      before.filter((image) => !after.has(image)).forEach(revoke);
    };
    const change = (setter, value) => {
      revision++;
      setter(value);
      if (!restored) operations.push(() => setter(value));
      save();
    };
    record.setText = (value) => change(rawText, value);
    record.setImages = (value) => change(setImagesTracked, value);
    record.retry = () => restored ? save() : restore();
    function restore() {
      record.setLoading(true);
      enqueue(async () => {
        const value = await invoke("drafts_load", { key });
        const images = [];
        let missing = 0;
        for (const ref of value?.images || []) {
          try {
            const bytes = await invoke("drafts_blob", { sha256: ref.sha256 });
            const blob = new Blob([bytes], { type: ref.media_type });
            images.push({ ...ref, blob, preview: objectUrl(blob) });
          } catch { missing++; }
        }
        return { text: value?.text || "", images, missing };
      }).then(({ text, images, missing }) => {
        rawText(text);
        rawImages(images);
        operations.forEach((operation) => operation());
        const changed = operations.length > 0 || missing > 0;
        operations = [];
        restored = true;
        record.setError(missing ? `${missing} saved image${missing > 1 ? "s" : ""} could not be restored and ${missing > 1 ? "were" : "was"} removed.` : "");
        record.setReady(true);
        record.setLoading(false);
        if (changed) save();
      }).catch(() => { record.setLoading(false); record.setError("Draft could not be restored. Retry before editing or sending."); });
    }
    restore();
    return record;
  }
  /** Store image bytes privately and return an in-memory draft image. */
  async function attach(blob, name) {
    const bytes = new Uint8Array(await blob.arrayBuffer());
    const stored = await invoke("drafts_put_blob", bytes);
    if (!stored?.sha256) throw new Error("Image could not be stored");
    const typed = blob.type === stored.media_type ? blob : new Blob([bytes], { type: stored.media_type });
    return { sha256: stored.sha256, media_type: stored.media_type, name: name || "image", size: stored.size ?? bytes.length, blob: typed, preview: objectUrl(typed) };
  }
  return { draftFor: get, attach };
}

/** HAL image parts for sending. Base64 exists only for the duration of the request. */
export async function sendableImages(images) {
  return Promise.all(images.map(async ({ blob, media_type, name }) => ({ data: await blobToBase64(blob), media_type, name })));
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
export const attachImage = store.attach;
export function clearSentDraft(target, text, images) {
  if (target.text() === text) target.setText("");
  target.setImages((current) => current.filter((image) => !images.includes(image)));
}
export function appendAgentDraft(agentId, text) {
  if (!agentId || !text) return;
  draftFor(`agent:${agentId}`).setText((old) => old ? `${old}\n\n${text}` : text);
}
