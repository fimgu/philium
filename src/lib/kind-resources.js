// kind_list returns registered resource rows, not CLI installation status.
export function kindResources(rows) {
  if (!Array.isArray(rows)) throw new Error("Unexpected kind resource response");
  return rows.map(resource => {
    let meta = resource?.meta;
    if (typeof meta === "string") {
      try { meta = JSON.parse(meta); } catch { meta = {}; }
    }
    if (!meta || typeof meta !== "object" || Array.isArray(meta)) meta = {};
    return { ...resource, meta };
  });
}
