import { createSignal } from "solid-js";

export const CONVERSATION_VIEW_KEY = "philium.conversation.view";
export const isConversationView = (value) => value === "conversation" || value === "terminal";

/** Storage is optional: private browsing, SSR and quota failures retain an in-memory preference. */
export function createConversationView(storage = () => globalThis.window?.localStorage) {
  let initial = "conversation";
  try {
    const saved = storage()?.getItem(CONVERSATION_VIEW_KEY);
    if (isConversationView(saved)) initial = saved;
  } catch { /* Storage access itself may throw. */ }
  const [view, update] = createSignal(initial);
  const setView = (value) => {
    if (!isConversationView(value)) return view();
    update(value);
    try { storage()?.setItem(CONVERSATION_VIEW_KEY, value); }
    catch { /* Keep the selected view for this session when persistence is unavailable. */ }
    return value;
  };
  return [view, setView];
}

// Shared across Chat and Agents. Switching presentation never changes session identity.
export const [conversationView, setConversationView] = createConversationView();
