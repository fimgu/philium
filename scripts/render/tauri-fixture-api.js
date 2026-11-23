// Only used by the fixture build. Never call the host WebView's Tauri bridge.
export const invoke = (command, args) => window.__philiumFixtureBridge.invoke(command, args);

export async function listen(event, handler) {
  const bridge = window.__philiumFixtureBridge;
  const callback = bridge.transformCallback(handler);
  const eventId = await bridge.invoke("plugin:event|listen", { event, handler: callback });
  return () => {
    bridge.unregisterCallback(callback);
    return bridge.invoke("plugin:event|unlisten", { event, eventId });
  };
}
