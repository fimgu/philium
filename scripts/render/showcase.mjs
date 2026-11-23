// README marketing screenshots from the SHOWCASE fixtures.
// usage: node showcase.mjs <outDir>   (needs ../../dist from `npx vite build`)
// Writes <outDir>/<screen>.png at 1440x900 @2x and <outDir>/frames/NN.png for the demo GIF.
import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import puppeteer from "puppeteer-core";
import { SHOWCASE, SHOWCASE_EVENTS } from "./showcase-fixtures.js";

const here = path.dirname(fileURLToPath(import.meta.url));
const dist = path.resolve(here, "../../dist");
const out = path.resolve(process.argv[2] ?? path.join(here, "out-showcase"));
const CHROME = process.env.CHROME ?? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
fs.mkdirSync(path.join(out, "frames"), { recursive: true });

const mime = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".woff2": "font/woff2", ".png": "image/png", ".svg": "image/svg+xml" };
const server = http.createServer((req, res) => {
  let p = path.join(dist, decodeURIComponent(req.url.split("?")[0]));
  if (!fs.existsSync(p) || fs.statSync(p).isDirectory()) p = path.join(dist, "index.html");
  res.setHeader("content-type", mime[path.extname(p)] ?? "application/octet-stream");
  fs.createReadStream(p).pipe(res);
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const port = server.address().port;

const stub = (fixtures) => {
  let cb = 1;
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
  window.__TAURI_INTERNALS__ = {
    metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main", windowLabel: "main" } },
    transformCallback(f) { const id = cb++; window[`_${id}`] = f; return id; },
    unregisterCallback() {},
    convertFileSrc: (p) => p,
    async invoke(cmd, a) {
      if (cmd === "plugin:event|listen") return a.handler;
      if (cmd.startsWith("plugin:")) return 1;
      if (cmd === "drafts_load") return { text: "", images: [] };
      if (cmd === "rpc") {
        const m = a.method;
        if (m === "selfdev.get") return structuredClone(fixtures[`selfdev.get:${a.args.id}`]);
        if (m === "agents.get") return structuredClone(fixtures["agents.list"].find((x) => x.id === a.args?.id) ?? fixtures["agents.get"]);
        if (m === "agents.events") return structuredClone(fixtures["agents.events"].filter((e) => e.seq > (a.args?.after ?? 0)));
        if (fixtures[m] !== undefined) return structuredClone(fixtures[m]);
        if (/\.(list|events|proposals|ledger)$/.test(m)) return [];
        return null;
      }
      if (cmd === "hal") return fixtures[`hal ${a.method} ${a.path.split("?")[0]}`] ?? (a.method === "GET" ? [] : {});
      return null;
    },
  };
};

const browser = await puppeteer.launch({ executablePath: CHROME, headless: "new", args: ["--hide-scrollbars", "--force-color-profile=srgb"] });
async function open(route, fx = SHOWCASE, extra = {}) {
  const page = await browser.newPage();
  await page.setViewport({ width: 1440, height: 900, deviceScaleFactor: 2 });
  await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "dark" }]);
  await page.evaluateOnNewDocument(`(${stub})(${JSON.stringify(fx)})`);
  await page.evaluateOnNewDocument((r, e) => {
    localStorage.setItem("route", r);
    localStorage.setItem("philium.activeRepo", "r1");
    localStorage.setItem("philium.conversation.view", "conversation");
    localStorage.setItem("agents.viewMode", "split");
    for (const [k, v] of Object.entries(e)) localStorage.setItem(k, v);
  }, route, extra);
  await page.goto(`http://127.0.0.1:${port}/`);
  await page.waitForSelector(".sidebar");
  await sleep(900);
  return page;
}
const clickText = (page, sel, text) => page.evaluate((s, t) => { const el = [...document.querySelectorAll(s)].find((e) => e.textContent.trim().includes(t)); el?.click(); return !!el; }, sel, text);
const shot = async (page, name) => { await page.screenshot({ path: path.join(out, `${name}.png`) }); console.log("shot", name); };
const openAgent = async (page) => {
  await clickText(page, ".list-item", "lion"); await sleep(900);
  await page.evaluate(() => { for (const el of document.querySelectorAll(".conversation, .scroll, [class*=conversation]")) el.scrollTop = el.scrollHeight; });
  await sleep(200);
};

for (const s of ["home", "board", "memory", "skills", "stats", "terminals"]) {
  const page = await open(s);
  await sleep(400);
  if (s === "skills") { await clickText(page, "button", "nextjs-conventions"); await sleep(500); }
  if (s === "memory") { await page.evaluate(() => [...document.querySelectorAll("summary")].find((e) => e.textContent.includes("Changes"))?.click()); await sleep(300); }
  if (s === "terminals") { await clickText(page, "button", "lion: pnpm"); await sleep(1200); }
  await shot(page, s);
  await page.close();
}
{ const page = await open("agents"); await openAgent(page); await shot(page, "agents"); await page.close(); }
{
  const page = await open("review");
  await shot(page, "review");
  await page.close();
  const p2 = await open("agents"); await openAgent(p2);
  await clickText(p2, "button", "Changes"); await sleep(900);
  await shot(p2, "diff"); await p2.close();
}
{
  const page = await open("home");
  await page.evaluate(() => document.querySelector('[title="Improve Philium"]')?.click());
  await sleep(900);
  await shot(page, "improve");
  await page.close();
}
// Demo frames: the lion conversation growing one step at a time.
const cuts = [1, 3, 4, 6, 8, 10, 12, 13, 16, 17, 19, 22];
for (const [i, n] of cuts.entries()) {
  const done = n >= 22;
  const fx = { ...SHOWCASE, "agents.events": SHOWCASE_EVENTS.filter((e) => e.seq <= n) };
  if (done) fx["agents.list"] = fx["agents.list"].map((a) => a.id === "a1" ? { ...a, state: "idle" } : a);
  const page = await open("agents", fx); await openAgent(page);
  await page.screenshot({ path: path.join(out, "frames", `${String(i).padStart(2, "0")}.png`) });
  await page.close();
}
console.log("frames", cuts.length);
await browser.close();
server.close();
