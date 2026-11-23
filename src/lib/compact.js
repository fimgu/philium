// "/compact keep the API notes" -> "keep the API notes"; bare "/compact" -> "".
export function compactFocus(text) {
  return String(text || "").replace(/^\/compact\b/i, "").trim();
}
