import { createSignal } from "solid-js";

// In-memory only: preserve navigation drafts without persisting source or images
// to browser storage. Each agent keeps its own draft and in-flight send guard.
const drafts = new Map();
export function newDraft() {
  const [text, setText] = createSignal("");
  const [images, setImages] = createSignal([]);
  const [sending, setSending] = createSignal(false);
  return { text, setText, images, setImages, sending, setSending };
}
export function draftFor(key) {
  if (!drafts.has(key)) drafts.set(key, newDraft());
  return drafts.get(key);
}
export function appendAgentDraft(agentId, text) {
  if (!agentId || !text) return;
  draftFor(`agent:${agentId}`).setText((old) => old ? `${old}\n\n${text}` : text);
}
