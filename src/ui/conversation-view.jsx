import { createUniqueId } from "solid-js";
import { conversationView, setConversationView } from "../lib/conversation-view.js";
import "./conversation-view.css";

/** Presentation only. Keep the existing Conversation and Composer mounted when this changes. */
export function ViewToggle() {
  const hintId = createUniqueId();
  return (
    <div class="conversation-view-control">
      <div class="conversation-view-toggle" role="group" aria-label="Conversation view" aria-describedby={hintId}>
        <button type="button" aria-pressed={conversationView() === "conversation"} onClick={() => setConversationView("conversation")}>Conversation</button>
        <button type="button" aria-pressed={conversationView() === "terminal"} onClick={() => setConversationView("terminal")}>Terminal</button>
      </div>
      <span id={hintId} class="conversation-view-hint">Terminal is a dense transcript, not a command shell. Your session and draft stay the same.</span>
    </div>
  );
}
