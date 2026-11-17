// Image intake shared by paste, drop, file picker and screenshot capture.
export const IMAGE_TYPES = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);
export const MAX_IMAGE_BYTES = 5 * 1024 * 1024;
export const MAX_IMAGES = 8;
/** Models downsample beyond roughly this edge, so larger images only cost memory and tokens. */
export const MAX_EDGE = 2048;

/** Downscale oversized still images. GIFs keep animation and are only size-checked.
 * Never throws for decode problems: falls back to the original bytes. */
export async function normalizeImage(file, env = globalThis) {
  if (!IMAGE_TYPES.has(file.type) || file.type === "image/gif" || typeof env.createImageBitmap !== "function") return file;
  let bitmap;
  try { bitmap = await env.createImageBitmap(file); } catch { return file; }
  try {
    const edge = Math.max(bitmap.width, bitmap.height);
    if (edge <= MAX_EDGE && file.size <= MAX_IMAGE_BYTES) return file;
    const scale = Math.min(1, MAX_EDGE / edge);
    const width = Math.max(1, Math.round(bitmap.width * scale)), height = Math.max(1, Math.round(bitmap.height * scale));
    const canvas = env.document.createElement("canvas");
    canvas.width = width; canvas.height = height;
    canvas.getContext("2d").drawImage(bitmap, 0, 0, width, height);
    const encode = (type, quality) => new Promise((resolve) => canvas.toBlob(resolve, type, quality));
    let out = await encode("image/png");
    if (!out || out.size > MAX_IMAGE_BYTES) out = await encode("image/jpeg", 0.9);
    if (!out) return file;
    const base = (file.name || "image").replace(/\.[a-z0-9]+$/i, "");
    return new File([out], `${base}.${out.type === "image/png" ? "png" : "jpg"}`, { type: out.type });
  } finally { bitmap.close?.(); }
}

export function validateImages(files, existing = 0) {
  if (files.some((file) => !IMAGE_TYPES.has(file.type))) throw new Error("Unsupported image type. Use PNG, JPEG, GIF or WebP.");
  if (files.some((file) => file.size > MAX_IMAGE_BYTES)) throw new Error("Images must be 5 MiB or smaller.");
  if (existing + files.length > MAX_IMAGES) throw new Error(`Up to ${MAX_IMAGES} images per message.`);
}

export async function blobToBase64(blob) {
  const bytes = new Uint8Array(await blob.arrayBuffer());
  let binary = "";
  for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
  return btoa(binary);
}

/** Bounded cache of object URLs for transcript thumbnails. Evicted URLs are revoked. */
export function createImageCache(load, limit = 48) {
  const entries = new Map();
  return {
    get(key) {
      if (entries.has(key)) { const v = entries.get(key); entries.delete(key); entries.set(key, v); return v; }
      const promise = load(key).then((blob) => URL.createObjectURL(blob));
      entries.set(key, promise);
      promise.catch(() => entries.delete(key));
      while (entries.size > limit) {
        const [oldest, value] = entries.entries().next().value;
        entries.delete(oldest);
        value.then((url) => URL.revokeObjectURL(url), () => {});
      }
      return promise;
    },
    size: () => entries.size,
    clear() { for (const value of entries.values()) value.then((url) => URL.revokeObjectURL(url), () => {}); entries.clear(); },
  };
}
