const SAMPLE_MS = 500;
const IDLE_MS = 2_000;

// Gateway token counts are cumulative estimates for a single request. Start at
// the first output sample so time spent waiting for the model is not counted.
export function sampleLiveTps(previous, progress, now) {
  if (previous && progress.id < previous.id) return previous;
  if (progress.done || progress.stalled) return { id: progress.id, ended: !!progress.done };
  const prior = progress.id === previous?.id ? previous : null;
  if (prior?.ended || !Number.isFinite(progress.tokens) || progress.tokens <= 0) {
    return prior ?? { id: progress.id };
  }
  if (prior?.tokens === progress.tokens) return prior;
  if (!prior?.tokens || now - prior.lastOutput >= IDLE_MS || progress.tokens < prior.tokens) {
    return { id: progress.id, tokens: progress.tokens, lastOutput: now, baseAt: now, baseTokens: progress.tokens };
  }
  const elapsed = now - prior.baseAt;
  const next = { ...prior, tokens: progress.tokens, lastOutput: now };
  if (elapsed >= SAMPLE_MS) {
    next.tps = (progress.tokens - prior.baseTokens) * 1000 / elapsed;
    next.baseAt = now;
    next.baseTokens = progress.tokens;
  }
  return next;
}

export function liveTps(sample, now) {
  return sample?.tps > 0 && now - sample.lastOutput < IDLE_MS ? sample.tps : null;
}
