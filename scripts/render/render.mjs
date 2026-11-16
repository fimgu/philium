// Headless render harness: serves ../../dist with a stubbed Tauri bridge and
// screenshots every screen and modal in EMPTY and POPULATED fixture states.
// usage: node render.mjs [outDir] [--only=agents,board] [--sets=empty,populated]
import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import puppeteer from "puppeteer-core";
import { EMPTY, POPULATED, FAILING, SLOW, HUGE } from "./fixtures.js";
const SETS = { empty: EMPTY, populated: POPULATED, failing: FAILING, slow: SLOW, huge: HUGE };

const here = path.dirname(fileURLToPath(import.meta.url));
const dist = path.resolve(here, "../../dist");
const args = process.argv.slice(2);
const out = path.resolve(args.find((a) => !a.startsWith("--")) ?? path.join(here, "out"));
const opt = (k, d) => (args.find((a) => a.startsWith(`--${k}=`)) ?? `--${k}=${d}`).split("=")[1];
const only = opt("only", "").split(",").filter(Boolean);
const sets = opt("sets", "empty,populated,failing,slow,huge").split(",");
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
  window.__permissionCalls = [];
  let cb = 1;
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
  window.__TAURI_INTERNALS__ = {
    metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main", windowLabel: "main" } },
    transformCallback(f) { const id = cb++; window[`_${id}`] = f; return id; },
    unregisterCallback() {},
    convertFileSrc: (p) => p,
    async invoke(cmd, a) {
      if (cmd.startsWith("plugin:")) return 1;
      if (fixtures.__delay) await new Promise((r) => setTimeout(r, fixtures.__delay));
      if (fixtures.__fail && (cmd === "rpc" || cmd === "hal")) {
        window.__n = (window.__n ?? 0) + 1;
        throw fixtures.__fail[window.__n % fixtures.__fail.length];
      }
      if (cmd === "rpc") {
        if (a.method === "agents.permission") window.__permissionCalls.push(a.args);
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

const problems = [];
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const browser = await puppeteer.launch({ executablePath: CHROME, headless: "new", args: ["--no-sandbox"] });

async function open(fx, setName, w, h, route) {
  const page = await browser.newPage();
  await page.setViewport({ width: w, height: h, deviceScaleFactor: 1 });
  await page.evaluateOnNewDocument(`(${stub.toString()})(${JSON.stringify(fx)}, ${JSON.stringify(setName)})`);
  await page.evaluateOnNewDocument((r) => localStorage.setItem("route", r), route);
  page.on("pageerror", (e) => { problems.push(`${setName} ${route} pageerror: ${e.message}`); });
  page.on("console", (m) => { if (m.type() === "error") problems.push(`${setName} ${route} console.error: ${m.text().slice(0, 200)}`); });
  await page.goto(`http://localhost:${port}/`);
  await page.waitForSelector(".sidebar");
  await sleep(fx.__delay ? 700 : 500);
  return page;
}
const clickText = (page, sel, text) => page.evaluate((s, t) => { const el = [...document.querySelectorAll(s)].find((e) => e.textContent.trim().includes(t)); el?.click(); return !!el; }, sel, text);

for (const setName of sets) {
  const fx = SETS[setName];
  for (const [w, h] of SIZES) {
    const dir = path.join(out, `${setName}-${w}x${h}`);
    fs.mkdirSync(dir, { recursive: true });
    const shot = async (page, name) => { await page.screenshot({ path: path.join(dir, `${name}.png`) }); console.log("shot", setName, w, name); };
    for (const s of SCREENS) {
      if (only.length && !only.includes(s)) continue;
      const page = await open(fx, setName, w, h, s);
      await sleep(setName === "slow" ? 300 : 0);
      await shot(page, s);
      const bad = await page.evaluate(() => {
        const out = [];
        const t = document.querySelector(".main")?.innerText ?? "";
        for (const w of ["undefined", "NaN", "[object Object]", "null"]) { const mm = t.match(new RegExp(`.{0,30}(^|[^\\w])${w.replace(/[\[\]]/g, "\\$&")}([^\\w]|$).{0,30}`)); if (mm) out.push("text contains " + w + " near: " + JSON.stringify(mm[0])); }
        if (!t.trim()) out.push("blank screen");
        const m = document.querySelector(".main");
        if (m && m.scrollWidth > m.clientWidth + 1) out.push("horizontal overflow of main " + m.scrollWidth + ">" + m.clientWidth);
        if (document.documentElement.scrollWidth > innerWidth + 1) out.push("document horizontal overflow");
        return out;
      });
      for (const b of bad) problems.push(`${setName} ${w}x${h} ${s}: ${b}`);
      if (s === "agents") {
        if (await clickText(page, ".list-item", "lion")) {
          await sleep(500); await shot(page, "agents-detail");
          if (setName === "populated") {
            const errors = await page.evaluate(async () => {
              const errors = [];
              const cards = [...document.querySelectorAll(".perm")];
              if (cards.length !== 3) return [`expected three browser permission cards, got ${cards.length}`];
              if (!cards[0].innerText.includes("https://example.com/permission-test")) errors.push("browser target URL missing");
              for (const [i, label] of ["Allow once", "Deny", "Allow for session"].entries()) {
                if (!cards[i].innerText.includes("Browser permission")) errors.push("browser permission title missing");
                const button = [...cards[i].querySelectorAll("button")].find((b) => b.textContent.trim() === label);
                if (!button) errors.push(`missing ${label}`);
                else button.click();
              }
              await new Promise((r) => setTimeout(r, 50));
              const calls = window.__permissionCalls;
              const expected = [["approve", "once"], ["deny", undefined], ["approve", "session"]];
              if (calls.length !== 3) errors.push(`expected three decisions, got ${calls.length}`);
              expected.forEach(([decision, scope], i) => {
                const c = calls[i];
                if (!c || c.id !== "a1" || c.request_id !== `browser-perm-${i}` || c.decision !== decision || c.scope !== scope) errors.push(`wrong approval payload ${i}`);
              });
              return errors;
            });
            errors.forEach((e) => problems.push(`${w} browser approval: ${e}`));
          }
        } else if (setName === "populated") problems.push(`${w} browser approval: agent detail did not open`);
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
      if (s === "agents" || s === "chat") {
        await page.evaluate((s) => window.__philiumGo(s, s === "agents" ? { id: "deleted-id" } : { session: "deleted-id" }), s);
        await sleep(600); await shot(page, `${s}-stale-id`);
        const t = await page.evaluate(() => document.querySelector(".main")?.innerText ?? "");
        if (setName === "populated" && !/not found/i.test(t)) problems.push(`${setName} ${w}x${h} ${s}: stale id shows no not-found state`);
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
if (problems.length) { console.log("\nPROBLEMS (" + problems.length + ")"); for (const p of [...new Set(problems)]) console.log(" - " + p); process.exitCode = 1; } else console.log("\nno problems");
server.close();
