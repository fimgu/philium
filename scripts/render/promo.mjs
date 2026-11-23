// Builds self-contained promo HTML in docs/assets/src and renders PNGs at 2x.
// Usage: node scripts/render/promo.mjs  (from repo root)
import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";
import puppeteer from "puppeteer-core";

const root = path.resolve(path.dirname(new URL(import.meta.url).pathname), "../..");
const out = path.join(root, "docs/assets");
const srcDir = path.join(out, "src");
fs.mkdirSync(srcDir, { recursive: true });
const tmp = fs.mkdtempSync(path.join(process.env.TMPDIR || "/tmp", "promo-"));

function b64(file, size) {
  const t = path.join(tmp, `${size}-${path.basename(file)}`);
  execFileSync("sips", ["-Z", String(size), "-s", "format", "jpeg", "-s", "formatOptions", "82", file, "--out", t.replace(/\.png$/, ".jpg")], { stdio: "ignore" });
  return "data:image/jpeg;base64," + fs.readFileSync(t.replace(/\.png$/, ".jpg")).toString("base64");
}
const logoTmp = path.join(tmp, "logo.png");
execFileSync("sips", ["-Z", "320", path.join(root, "src-tauri/icons/source.png"), "--out", logoTmp], { stdio: "ignore" });
const LOGO = "data:image/png;base64," + fs.readFileSync(logoTmp).toString("base64");
const shotFile = path.join(out, "screenshots/agents.png");
const SHOT = fs.existsSync(shotFile) ? b64(shotFile, 1600) : null;

const base = (w, h, body) => `<!doctype html><html><head><meta charset="utf-8"><style>
:root{--bg:#0b0c0e;--raised:#111317;--surface:#15171c;--accent:#8b9cff;--text:#e8eaee;--dim:#9aa1ad;--faint:#6b7280}
*{box-sizing:border-box;margin:0;padding:0}
html,body{width:${w}px;height:${h}px;overflow:hidden;background:var(--bg);color:var(--text);
font-family:"PP Mori",-apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif;-webkit-font-smoothing:antialiased}
.stage{position:relative;width:100%;height:100%;overflow:hidden;
background:radial-gradient(ellipse 70% 60% at 85% 0%,rgba(139,156,255,.20),transparent 60%),
radial-gradient(ellipse 60% 50% at 0% 100%,rgba(139,156,255,.10),transparent 60%),var(--bg)}
.grid{position:absolute;inset:0;background-image:linear-gradient(rgba(255,255,255,.025) 1px,transparent 1px),linear-gradient(90deg,rgba(255,255,255,.025) 1px,transparent 1px);background-size:40px 40px;mask-image:radial-gradient(ellipse at center,#000 30%,transparent 80%)}
.logo{border-radius:22%;box-shadow:0 20px 60px rgba(139,156,255,.25),0 0 0 1px rgba(255,255,255,.06)}
.name{font-weight:700;letter-spacing:-.03em}
.tag{color:var(--dim);letter-spacing:-.01em}
.acc{color:var(--accent)}
.pill{display:inline-block;padding:6px 14px;border-radius:99px;border:1px solid rgba(139,156,255,.3);background:rgba(139,156,255,.08);color:var(--accent);font-size:13px;letter-spacing:.08em;text-transform:uppercase;font-weight:600}
</style></head><body><div class="stage"><div class="grid"></div>${body}</div></body></html>`;

const banner = base(1280, 400, `
<div style="position:absolute;inset:0;display:flex;align-items:center;justify-content:center;gap:44px">
 <img class="logo" src="${LOGO}" width="168" height="168">
 <div><div class="name" style="font-size:96px;line-height:1">Philium</div>
 <div class="tag" style="font-size:30px;margin-top:16px">One calm workspace for <span class="acc">all your coding agents</span>.</div></div>
</div>`);

const social = base(1280, 640, `
<div style="position:absolute;inset:0;display:flex;flex-direction:column;align-items:center;justify-content:center;text-align:center">
 <img class="logo" src="${LOGO}" width="150" height="150">
 <div class="name" style="font-size:104px;margin-top:34px;line-height:1">Philium</div>
 <div class="tag" style="font-size:34px;margin-top:20px">One calm workspace for <span class="acc">all your coding agents</span>.</div>
 <div style="margin-top:36px;display:flex;gap:12px"><span class="pill">Parallel agents</span><span class="pill">Local memory</span><span class="pill">Any model</span><span class="pill">Private</span></div>
</div>`);

const feats = [
  ["Parallel agents and swarms", "Run many agents side by side and let them coordinate on one plan."],
  ["Local memory that dreams", "Memory stays on your machine and consolidates itself between sessions."],
  ["Skills and MCP", "Plug in reusable skills and any MCP server, no glue code."],
  ["Review and PR flow", "Diffs, reviews and pull requests without leaving the workspace."],
  ["Any model provider", "Bring the models you already use and switch freely."],
  ["Sandboxed and private", "Agents run contained. Your code and keys stay yours."],
];
// Frame inner width is 1200 - 2*90 padding - 2px border. The screenshot is cut into two
// slices so the empty band at the top of the conversation (source y 470..640 of 1800) is skipped.
const FW = 1018, SW = 2880, k = FW / SW, CUT_A = 470, CUT_B = 640;
const slice = (y0, h) => `<div style="flex:none;height:${Math.round(h * k)}px;background:url(${SHOT}) no-repeat 0 ${-Math.round(y0 * k)}px/100% auto"></div>`;
const slot = SHOT
  ? `<div style="display:flex;flex-direction:column">${slice(0, CUT_A)}<div style="height:1px;background:rgba(255,255,255,.06)"></div>${slice(CUT_B, 1800 - CUT_B)}</div>`
  : `<div style="height:100%;display:flex;gap:14px;padding:44px 22px 22px">
      <div style="width:22%;border-radius:10px;background:var(--surface)"></div>
      ${[0, 1, 2].map(() => `<div style="flex:1;border-radius:10px;background:var(--surface);padding:16px;display:flex;flex-direction:column;gap:10px">${[70, 90, 55, 80, 40].map(w => `<div style="height:8px;width:${w}%;border-radius:4px;background:rgba(255,255,255,.07)"></div>`).join("")}</div>`).join("")}
     </div>`;
const poster = base(1200, 1500, `
<div style="position:absolute;inset:0;padding:84px 90px;display:flex;flex-direction:column;${SHOT ? "justify-content:center" : ""}">
 <div style="display:flex;align-items:center;gap:18px"><img class="logo" src="${LOGO}" width="64" height="64"><span class="name" style="font-size:34px">Philium</span></div>
 <h1 class="name" style="font-size:86px;line-height:1.02;margin-top:54px">All your coding agents.<br><span class="acc">One calm workspace.</span></h1>
 <div style="margin-top:56px;flex:none;${SHOT ? "" : "height:520px;"}border-radius:18px;padding:1px;background:linear-gradient(160deg,rgba(139,156,255,.5),rgba(255,255,255,.06) 40%,rgba(139,156,255,.15));box-shadow:0 40px 100px rgba(0,0,0,.55),0 0 80px rgba(139,156,255,.12)">
  <div style="position:relative;${SHOT ? "" : "height:100%;"}border-radius:17px;overflow:hidden;background:var(--raised)">
   <div style="position:absolute;top:0;left:0;right:0;height:30px;display:flex;gap:7px;align-items:center;padding-left:14px;z-index:1;${SHOT ? "" : "background:var(--raised)"}">${SHOT ? "" : ["#ff5f57", "#febc2e", "#28c840"].map(c => `<i style="width:11px;height:11px;border-radius:50%;background:${c};opacity:.7"></i>`).join("")}</div>
   ${slot}
  </div></div>
 <div style="margin-top:${SHOT ? "64px" : "auto"};display:grid;grid-template-columns:repeat(3,1fr);gap:40px 44px">
  ${feats.map(([t, d]) => `<div style="border-top:1px solid rgba(139,156,255,.35);padding-top:18px"><div style="font-size:23px;font-weight:650;letter-spacing:-.01em">${t}</div><div class="tag" style="font-size:17px;line-height:1.45;margin-top:8px">${d}</div></div>`).join("")}
 </div>
</div>`);

const jobs = [["banner", banner, 1280, 400], ["poster", poster, 1200, 1500], ["social-preview", social, 1280, 640]];
const browser = await puppeteer.launch({ executablePath: "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", headless: true });
for (const [name, html, w, h] of jobs) {
  const f = path.join(srcDir, `${name}.html`);
  fs.writeFileSync(f, html);
  const page = await browser.newPage();
  await page.setViewport({ width: w, height: h, deviceScaleFactor: 2 });
  await page.goto("file://" + f, { waitUntil: "load" });
  await page.screenshot({ path: path.join(out, `${name}.png`) });
  await page.close();
  console.log(name, fs.statSync(path.join(out, `${name}.png`)).size);
}
await browser.close();
fs.rmSync(tmp, { recursive: true, force: true });
console.log("screenshot:", SHOT ? "embedded" : "placeholder");
