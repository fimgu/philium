// Which hunk operations each diff layer allows. Mirrors src-tauri/src/hunks.rs.
export function hunkActions(layer) {
  if (layer === "unstaged") return [{ op: "stage", label: "Stage" }, { op: "revert", label: "Revert" }];
  if (layer === "staged") return [{ op: "unstage", label: "Unstage" }];
  return [];
}
