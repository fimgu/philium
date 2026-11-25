// A screen that is open must show a backend change as soon as its event arrives, without a manual refresh.
// Each case changes what the backend would return, emits the event the backend emits, and waits for the new text.
import assert from "node:assert/strict";

const until = (page, fn, arg, what) =>
  page.waitForFunction(fn, { timeout: 4000 }, arg).catch(() => { throw new Error(`no live update: ${what}`); });

export async function checkMemoryLive(page) {
  const marker = "Live memory marker";
  await page.evaluate((marker) => {
    const row = { id: "live-1", content: marker, scope: "global", status: "active", kind: "fact", created_at: Date.now() / 1000, updated_at: Date.now() / 1000 };
    window.__fixtures["library.list"] = [row, ...(window.__fixtures["library.list"] ?? [])];
    window.__emit("library", {});
  }, marker);
  await until(page, (m) => document.querySelector(".main")?.innerText.includes(m), marker, "Memory after a library change");
}

export async function checkMcpLive(page) {
  // The section stays collapsed, so the live signal is its "N custom" count; opening it would reload on its own.
  const name = "live-mcp-marker";
  const count = () => page.evaluate(() => [...document.querySelectorAll("summary")].find((s) => s.textContent.includes("MCP servers"))?.querySelector(".badge")?.textContent.trim());
  const before = await count();
  await page.evaluate((name) => {
    window.__fixtures["mcp.servers_list"] = [...(window.__fixtures["mcp.servers_list"] ?? []), { name, command: "true", transport: "stdio", enabled: true, approval: "ask", repos: [] }];
    window.__emit("mcp:changed", {});
  }, name);
  await until(page, (b) => { const t = [...document.querySelectorAll("summary")].find((s) => s.textContent.includes("MCP servers"))?.querySelector(".badge")?.textContent.trim(); return t && t !== b && /^1 custom$/.test(t); }, before, "Settings MCP count after mcp:changed");
}

export async function checkSurfacesLive(page) {
  const name = "Live app marker";
  await page.evaluate((name) => {
    const row = { id: "live-app", name, category: "custom", description: "Added elsewhere", image: "example/app" };
    window.__fixtures["surfaces.catalog"] = [...(window.__fixtures["surfaces.catalog"] ?? []), row];
    window.__emit("surfaces:changed", { recipe: "live-app" });
  }, name);
  await until(page, (n) => document.querySelector(".main")?.innerText.includes(n), name, "Apps catalog after surfaces:changed");
}

export async function checkHomeSetupLive(page) {
  const want = "0 skills, 1 MCP servers.";
  if (!(await page.evaluate(() => !!document.querySelector(".home-setup")))) throw new Error("setup card not shown");
  await page.evaluate(() => {
    window.__fixtures["mcp.servers_list"] = [{ name: "live-mcp", command: "true", transport: "stdio", enabled: true }];
    window.__emit("mcp:changed", {});
  });
  await until(page, (w) => document.querySelector(".home-setup")?.innerText.includes(w), want, "Home setup MCP count after mcp:changed");
}

export async function checkSkillsLive(page) {
  const name = "live-skill-marker";
  const before = await page.evaluate(() => document.querySelector(".main")?.innerText ?? "");
  assert.ok(!before.includes(name));
  await page.evaluate((name) => {
    const cur = window.__fixtures["skills.list"] ?? {};
    const row = { name, description: "Added while the screen was open", scope: "global", enabled: true };
    window.__fixtures["skills.list"] = { ...cur, skills: [...(cur.skills ?? []), row] };
    window.__emit("library", {});
  }, name);
  await until(page, (n) => document.querySelector(".main")?.innerText.includes(n), name, "Skills after a library change");
}
