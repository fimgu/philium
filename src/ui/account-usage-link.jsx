import { Show } from "solid-js";
import { MarkdownLink } from "./markdown-media.jsx";

const dashboards = {
  claude_oauth: { href: "https://claude.ai/settings/usage", label: "Claude plan usage" },
  chatgpt_oauth: { href: "https://chatgpt.com/codex/settings/usage", label: "Codex plan usage" },
};

export function AccountUsageLink(props) {
  return (
    <Show when={dashboards[props.kind]}>
      {(dashboard) => <MarkdownLink href={dashboard().href}>{dashboard().label} ↗</MarkdownLink>}
    </Show>
  );
}
