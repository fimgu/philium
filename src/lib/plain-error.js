// Errors from model providers, the network and git, said as one sentence with what fixes it.
// The original text stays available as details. Text that matches nothing is shown as it is:
// Philium's own messages are already written for a person.
const RULES = [
  [/\b429\b|rate[ _-]?limit|too many requests|usage limit|quota exceeded/i,
    (m) => `The model provider is limiting requests right now${m.retry ? ` and asks to wait ${m.retry} seconds` : ""}. Send again in a minute, or switch model with /model.`],
  [/\b529\b|overloaded/i,
    () => "The model provider is overloaded right now. Send again in a minute, or switch model with /model."],
  [/\b401\b|unauthori[sz]ed|invalid (x-)?api[ _-]?key|authentication[ _]error|not signed in|signed out|token (has )?expired|refresh token/i,
    () => "The model provider refused the sign-in. Open Providers and sign in again, then send again."],
  [/context[ _-](length|window)|maximum context|prompt is too long|too many (input )?tokens/i,
    () => "This conversation no longer fits the model. Start a fresh agent for the next step, or switch to a model with a larger window with /model."],
  [/econnrefused|connection refused|could not connect|error sending request|dns error|network is unreachable|timed out|operation timed out/i,
    () => "Philium could not reach the model provider. Check the network or VPN, then send again."],
  [/needed a single revision|unknown revision|not a valid object name|bad revision/i,
    () => "Git could not find the branch or commit this agent starts from. Fetch it in the checkout, or choose another starting branch under Repos."],
  [/(status|http|code)\D{0,3}5\d\d\b|internal server error|bad gateway|service unavailable/i,
    () => "The model provider had an error on its side. Send again in a minute."],
];

/** `{plain, raw}`: `raw` is the original text when `plain` replaced it, otherwise null. */
export function plainError(error) {
  const raw = String(error ?? "").trim();
  for (const [re, say] of RULES) {
    if (re.test(raw)) {
      const retry = raw.match(/retry[- _]after\D{0,4}(\d{1,5})/i)?.[1] || "";
      return { plain: say({ retry }), raw };
    }
  }
  return { plain: raw, raw: null };
}
