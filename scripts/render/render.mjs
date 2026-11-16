// Headless render harness: serves ../../dist with a stubbed Tauri bridge and
// screenshots every screen and modal in EMPTY and POPULATED fixture states.
// usage: node render.mjs [outDir] [--only=agents,board] [--sets=empty,populated]
import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import puppeteer from "puppeteer-core";
import { EMPTY, POPULATED } from "./fixtures.js";

const here = path.dirname(fileURLToPath(import.meta.url));
const dist = path.resolve(here, "../../dist");
const args = process.argv.slice(2);
const out = path.resolve(args.find((a) => !a.startsWith("--")) ?? path.join(here, "out"));
const opt = (k, d) => (args.find((a) => a.startsWith(`--${k}=`)) ?? `--${k}=${d}`).split("=")[1];
const only = opt("only", "").split(",").filter(Boolean);
const sets = opt("sets", "empty,populated").split(",");
const CHROME = process.env.CHROME ?? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const SIZES = [[1440, 900], [1280, 800]];
const SCREENS = ["home", "agents", "board", "chat", "terminals", "browser", "repos", "code", "activity", "stats", "providers", "settings"];

const mime = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".woff2": "font/woff2", ".png": "image/png", ".svg": "image/svg+xml" };
const server = http.createServer((req, res) => {
  let p = path.join(dist, decodeURIComponent(req.url.split("?")[0]));
  if (!fs.existsSync(p) || fs.statSync(p).isDirectory()) p = path.join(dist, "index.html");
  res.setHeader("content-type", mime[path.extname(p)] ?? "application/octet-stream");
  fs.createReadStream(p).pipe(res);
}).listen(0);
const port = server.address().port;

const stub = (fx, setName) => {
  const fixtures = fx;
  let cb = 1;
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
  window.__TAURI_INTERNALS__ = {
    metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main", windowLabel: "main" } },
    transformCallback(f) { const id = cb++; window[`_${id}`] = f; return id; },
    unregisterCallback() {},
    convertFileSrc: (p) => p,
    async invoke(cmd, a) {
      if (cmd.startsWith("plugin:")) return 1;
      if (cmd === "rpc") {
        const v = fixtures[a.method];
        if (v !== undefined) return JSON.parse(JSON.stringify(v));
        if (/\.(list|tabs|names|proposals|ledger|inbox|threads|playbooks|branches|worktrees|models)$|repos_list/.test(a.method)) return [];
        if (/\.(set|save|send|open|navigate|show|hide|close|create|spawn)/.test(a.method)) return {};
        return {};
      }
      if (cmd === "hal") {
        const key = `hal ${a.method} ${a.path.split("?")[0]}`;
        if (fixtures[key] !== undefined) return JSON.parse(JSON.stringify(fixtures[key]));
        return a.method === "GET" ? [] : {};
      }
      return null;
    },
  };
};

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const browser = await puppeteer.launch({ executablePath: CHROME, headless: "new", args: ["--no-sandbox"] });

async function open(fx, setName, w, h, route) {
  const page = await browser.newPage();
  await page.setViewport({ width: w, height: h, deviceScaleFactor: 1 });
  await page.evaluateOnNewDocument(`(${stub.toString()})(${JSON.stringify(fx)}, ${JSON.stringify(setName)})`);
  await page.evaluateOnNewDocument((r) => localStorage.setItem("route", r), route);
  page.on("pageerror", (e) => console.log("  pageerror:", e.message));
  await page.goto(`http://localhost:${port}/`);
  await page.waitForSelector(".sidebar");
  await sleep(500);
  return page;
}
const clickText = (page, sel, text) => page.evaluate((s, t) => { const el = [...document.querySelectorAll(s)].find((e) => e.textContent.trim().includes(t)); el?.click(); return !!el; }, sel, text);

for (const setName of sets) {
  const fx = setName === "empty" ? EMPTY : POPULATED;
  for (const [w, h] of SIZES) {
    const dir = path.join(out, `${setName}-${w}x${h}`);
    fs.mkdirSync(dir, { recursive: true });
    const shot = async (page, name) => { await page.screenshot({ path: path.join(dir, `${name}.png`) }); console.log("shot", setName, w, name); };
    for (const s of SCREENS) {
      if (only.length && !only.includes(s)) continue;
      const page = await open(fx, setName, w, h, s);
      await shot(page, s);
      if (s === "agents") {
        if (await clickText(page, ".list-item", "lion")) { await sleep(500); await shot(page, "agents-detail"); }
        if (await clickText(page, "button", "New agent")) { await sleep(300); await shot(page, "modal-new-agent"); }
      }
      if (s === "providers") {
        if (await clickText(page, "button", "Add provider")) { await sleep(300); await shot(page, "modal-add-provider"); }
      }
      if (s === "chat" && setName === "populated") {
        if (await clickText(page, ".list-item", "Gateway retry")) { await sleep(600); await shot(page, "chat-conversation"); }
      }
      if (s === "board" && setName === "populated") {
        if (await clickText(page, "[class*=card], article", "Refactor worktree")) { await sleep(400); await shot(page, "board-detail"); }
      }
      if (s === "home") {
        if (await clickText(page, "button", "Improve Philium")) { await sleep(400); await shot(page, "modal-improve"); }
        await page.keyboard.press("Escape");
        await page.keyboard.down("Meta"); await page.keyboard.press("k"); await page.keyboard.up("Meta");
        await sleep(300); await shot(page, "modal-palette");
      }
      await page.close();
    }
  }
}
await browser.close();
server.close();
