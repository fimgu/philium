import { appendAgentDraft } from "./drafts.js";
import { formatCodeContext } from "./code-context-format.js";
import { go } from "./nav.js";

// Deliberately does not call HAL or agents.send. The human reviews and sends.
export function openCodeDraft(agentId, context) {
  if (!agentId || !context?.path) throw new Error("Choose an agent and a file first.");
  appendAgentDraft(agentId, formatCodeContext(context));
  go("agents", { id: agentId });
}
