// Idempotency key for one steering message, so a retried click is not applied twice.
// Same agent and text within the same 10 second window gives the same key.
export function steerKey(agentId, text, now = Date.now()) {
  let h = 2166136261;
  for (const c of `${agentId}\n${text}`) { h ^= c.codePointAt(0); h = Math.imul(h, 16777619) >>> 0; }
  return `steer-${h.toString(16)}-${Math.floor(now / 10_000)}`;
}
