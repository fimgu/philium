import { render } from "solid-js/web";
import { Icon } from "./ui/icons.jsx";
import "./styles/tokens.css";
import "./styles/components.css";

// An embedded tab may have Tauri metadata but cannot use the desktop IPC bridge.
// Don't start stores, subscriptions, or the application shell in that tab.
const webview = window.__TAURI_INTERNALS__?.metadata?.currentWebview;
const fixture = import.meta.env.MODE === "fixture" && !!window.__philiumFixtureBridge;
if (fixture || webview?.label === "main") {
  import("./main.jsx");
} else {
  render(() => (
    <main class="desktop-notice">
      <Icon name="globe" size={28} />
      <h1>Open Philium in the desktop app</h1>
      <p>This page can’t connect to your workspace from a browser tab.</p>
    </main>
  ), document.getElementById("root"));
}
