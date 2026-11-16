// Global backend alerts: agent HAL crashes and desk HAL failures become toasts
// no matter which screen is open. Imported once from main.jsx.
import { listen } from "@tauri-apps/api/event";
import { rpc } from "./rpc.js";
import { toast } from "../ui/kit.jsx";

const reason = (e) => String(e || "unknown reason").replace(/\.?\s*Press Resume to restart it\.?$/, "");

listen("agents:error", async (e) => {
  const { id, error } = e.payload ?? {};
  let name = id;
  try { name = (await rpc("agents.get", { id }))?.name || id; } catch {}
  toast(`${name} stopped: ${reason(error)}. Press Resume to restart it.`, "bad");
});

let lastDesk = "";
listen("app:desk", (e) => {
  const err = e.payload?.error;
  if (!err) { lastDesk = ""; return; }
  if (err === lastDesk) return;
  lastDesk = err;
  toast(`Chat HAL stopped: ${err}. Restarting.`, "bad");
});
