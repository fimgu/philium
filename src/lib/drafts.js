import { createSignal } from "solid-js";
import { blobToBase64 } from "./images.js";

// Drafts live in private native storage: a small text index plus content-addressed
// image blobs. Typing rewrites only the index. Image bytes are written once, on attach.
// Draft images in memory: { sha256, media_type, name, size, blob, preview(object URL) }.
const SAVE_ERROR = "Draft could not be saved. Keep this window open and retry.";

export function createDraftStore(invoke, urls = globalThis.URL, options = {}) {
  if (urls && typeof urls === "object" && "debounceMs" in urls && !("createObjectURL" in urls)) {
    options = urls;
    urls = globalThis.URL;
  }
  const debounceMs = options?.debounceMs ?? (typeof window !== "undefined" ? 300 : 0);
  const drafts = new Map();
  const objectUrl = (blob) => (urls?.createObjectURL ? urls.createObjectURL(blob) : "");
  const revoke = (image) => {
    if (image?.preview && urls?.revokeObjectURL) urls.revokeObjectURL(image.preview);
  };

  function get(key) {
    if (drafts.has(key)) return drafts.get(key);
    const record = newDraft();
    record.key = key;
    record.setReady(false);
    drafts.set(key, record);

    // Per-key promise queue so one draft never blocks another.
    let queue = Promise.resolve();
    const enqueue = (task) => {
      const result = queue.then(task);
      queue = result.catch(() => {});
      return result;
    };

    const rawText = record.setText;
    const rawImages = record.setImages;
    let restored = false;
    let operations = [];
    let revision = 0;
    let scheduled = false;
    let debounceTimer = null;

    const cancelDebounce = () => {
      if (debounceTimer !== null) {
        clearTimeout(debounceTimer);
        debounceTimer = null;
      }
    };

    const snapshot = () => ({
      text: record.text(),
      images: record.images().map(({ sha256, media_type, name, size }) => ({
        sha256,
        media_type,
        name,
        size,
      })),
    });

    const commitSave = () => {
      cancelDebounce();
      if (!restored || scheduled) return queue;
      scheduled = true;
      let version;
      record.setPending(true);
      return enqueue(() => {
        scheduled = false;
        version = revision;
        return invoke("drafts_save", { key, draft: snapshot() });
      })
        .then(() => {
          if (version === revision && debounceTimer === null) {
            record.setPending(false);
            if (record.error() === SAVE_ERROR) record.setError("");
          }
        })
        .catch(() => {
          record.setError(SAVE_ERROR);
          if (version === revision && debounceTimer === null) record.setPending(false);
        });
    };

    const save = (immediate = false) => {
      if (!restored) return queue;
      record.setPending(true);
      if (immediate || !(debounceMs > 0)) {
        return commitSave();
      }
      cancelDebounce();
      debounceTimer = setTimeout(() => {
        debounceTimer = null;
        commitSave();
      }, debounceMs);
      return queue;
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
      save(false);
    };

    record.setText = (value) => change(rawText, value);
    record.setImages = (value) => change(setImagesTracked, value);
    record.flush = () => {
      if (!restored) return queue;
      if (debounceTimer !== null || record.pending()) return commitSave();
      return queue;
    };
    record.cancelDebounce = cancelDebounce;
    record.retry = () => (restored ? save(true) : restore());

    function restore() {
      record.setLoading(true);
      enqueue(async () => {
        const value = await invoke("drafts_load", { key });
        let missing = 0;
        const loaded = await Promise.all(
          (value?.images || []).map(async (ref) => {
            try {
              const bytes = await invoke("drafts_blob", { sha256: ref.sha256 });
              const blob = new Blob([bytes], { type: ref.media_type });
              return { ...ref, blob, preview: objectUrl(blob) };
            } catch {
              missing++;
              return null;
            }
          }),
        );
        const images = loaded.filter(Boolean);
        return { text: value?.text || "", images, missing };
      })
        .then(({ text, images, missing }) => {
          rawText(text);
          rawImages(images);
          operations.forEach((operation) => operation());
          const changed = operations.length > 0 || missing > 0;
          operations = [];
          restored = true;
          record.setError(
            missing
              ? `${missing} saved image${missing > 1 ? "s" : ""} could not be restored and ${missing > 1 ? "were" : "was"} removed.`
              : "",
          );
          record.setReady(true);
          record.setLoading(false);
          if (changed) save(true);
        })
        .catch(() => {
          record.setLoading(false);
          record.setError("Draft could not be restored. Retry before editing or sending.");
        });
    }

    restore();
    return record;
  }

  function flushDraft(keyOrRecord) {
    if (keyOrRecord === undefined) {
      return Promise.all([...drafts.values()].map((r) => r.flush?.()));
    }
    const record = typeof keyOrRecord === "string" ? drafts.get(keyOrRecord) : keyOrRecord;
    return record?.flush?.() ?? Promise.resolve();
  }

  function evictDraft(keyOrRecord) {
    const key = typeof keyOrRecord === "string" ? keyOrRecord : keyOrRecord?.key;
    const record = typeof keyOrRecord === "string" ? drafts.get(key) : keyOrRecord;
    if (!record) return false;
    record.cancelDebounce?.();
    for (const image of record.images?.() || []) revoke(image);
    if (key) drafts.delete(key);
    return true;
  }

  if (typeof window !== "undefined" && typeof window.addEventListener === "function") {
    window.addEventListener("beforeunload", () => {
      for (const record of drafts.values()) {
        record.flush?.();
      }
    });
  }

  /** Store image bytes privately and return an in-memory draft image. */
  async function attach(blob, name) {
    const bytes = new Uint8Array(await blob.arrayBuffer());
    const stored = await invoke("drafts_put_blob", bytes);
    if (!stored?.sha256) throw new Error("Image could not be stored");
    const typed = blob.type === stored.media_type ? blob : new Blob([bytes], { type: stored.media_type });
    return {
      sha256: stored.sha256,
      media_type: stored.media_type,
      name: name || "image",
      size: stored.size ?? bytes.length,
      blob: typed,
      preview: objectUrl(typed),
    };
  }

  return { draftFor: get, attach, flushDraft, evictDraft };
}

/** HAL image parts for sending. Base64 exists only for the duration of the request. */
export async function sendableImages(images) {
  return Promise.all(
    images.map(async ({ blob, media_type, name }) => ({
      data: await blobToBase64(blob),
      media_type,
      name,
    })),
  );
}

export function newDraft() {
  const [text, setText] = createSignal("");
  const [images, setImages] = createSignal([]);
  const [sending, setSending] = createSignal(false);
  const [ready, setReady] = createSignal(true);
  const [loading, setLoading] = createSignal(false);
  const [pending, setPending] = createSignal(false);
  const [error, setError] = createSignal("");
  return {
    text,
    setText,
    images,
    setImages,
    sending,
    setSending,
    ready,
    setReady,
    loading,
    setLoading,
    pending,
    setPending,
    error,
    setError,
  };
}

const store = createDraftStore(
  async (command, args) => {
    const { invoke } = await import("@tauri-apps/api/core");
    return invoke(command, args);
  },
  globalThis.URL,
  { debounceMs: 300 },
);

export const draftFor = store.draftFor;
export const attachImage = store.attach;
export const flushDraft = store.flushDraft;
export const evictDraft = store.evictDraft;

export function clearSentDraft(targetOrKey, text, images) {
  const target = typeof targetOrKey === "string" ? store.draftFor(targetOrKey) : targetOrKey;
  if (!target) return;
  if (text === undefined || target.text() === text) target.setText("");
  if (images === undefined) target.setImages([]);
  else target.setImages((current) => current.filter((image) => !images.includes(image)));
  target.flush?.();
}

export function appendAgentDraft(agentId, text) {
  if (!agentId || !text) return;
  draftFor(`agent:${agentId}`).setText((old) => (old ? `${old}\n\n${text}` : text));
}
