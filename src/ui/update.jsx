// In-app updates. The backend checks GitHub on launch and every few hours and pushes
// its state on "app:update". Install downloads and verifies the signed bundle, then a
// restart switches to the new version.
import { Show, createSignal } from "solid-js";
import { rpc, useEvent } from "../lib/rpc.js";

const [state, setState] = createSignal({ phase: "idle" });
const [dismissed, setDismissed] = createSignal("");
let started = false;

export function useUpdate() {
  if (!started) {
    started = true;
    rpc("app.update_state", {}, { local: true }).then((s) => s && setState(s)).catch(() => {});
  }
  useEvent("app:update", (s) => s && setState(s));
  return {
    state,
    check: () => rpc("app.update_check", {}, { action: true }).then(setState).catch((e) => setState({ ...state(), phase: "error", error: String(e) })),
    install: () => rpc("app.update_install", {}, { action: true }).then(setState).catch((e) => setState({ ...state(), phase: "error", error: String(e) })),
    restart: () => rpc("app.update_restart", {}, { action: true }).catch((e) => setState({ ...state(), phase: "error", error: String(e) })),
  };
}

const pct = (s) => (s.total ? Math.min(100, Math.round((100 * (s.downloaded || 0)) / s.total)) : null);

/** Status line plus the one action that makes sense now. Used by the banner and Settings. */
export function UpdateControls(props) {
  const u = props.update;
  const s = () => u.state();
  return (
    <>
      <span class="ellipsis grow">
        {{
          checking: "Checking for updates…",
          available: `Philium ${s().version} is available. You have ${s().current}.`,
          downloading: `Downloading Philium ${s().version}${pct(s()) != null ? ` (${pct(s())}%)` : "…"}`,
          ready: `Philium ${s().version} is installed. Restart to use it.`,
          error: s().error || "Update failed",
        }[s().phase] ?? (s().latest ? `Philium ${s().current} is up to date.` : `Philium ${s().current || props.version || ""}`)}
      </span>
      <Show when={s().phase === "available"}><button class="btn primary sm" onClick={u.install}>Update now</button></Show>
      <Show when={s().phase === "ready"}><button class="btn primary sm" onClick={u.restart}>Restart</button></Show>
      <Show when={props.showCheck && ["idle", "error"].includes(s().phase)}><button class="btn sm" onClick={u.check}>Check for updates</button></Show>
      <Show when={props.inBanner && s().phase === "error"}><button class="btn sm" onClick={u.install}>Retry</button></Show>
    </>
  );
}

/** Top banner, shown only when there is something to act on. */
export function UpdateBanner() {
  const u = useUpdate();
  const key = () => `${u.state().phase}:${u.state().version ?? ""}`;
  const visible = () => {
    const s = u.state();
    if (!s.version) return false;
    if (s.phase === "available") return dismissed() !== key();
    return ["downloading", "ready", "error"].includes(s.phase);
  };
  return (
    <Show when={visible()}>
      <div class={`update-banner ${u.state().phase === "error" ? "error" : ""}`} role="status">
        <UpdateControls update={u} inBanner />
        <Show when={u.state().phase === "available"}>
          <button class="btn ghost sm" onClick={() => setDismissed(key())} aria-label="Later">Later</button>
        </Show>
      </div>
    </Show>
  );
}
