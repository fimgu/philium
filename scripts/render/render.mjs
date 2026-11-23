// Headless render harness: serves ../../dist with a stubbed Tauri bridge and
// screenshots every screen and modal in EMPTY and POPULATED fixture states.
// usage: node render.mjs [outDir] [--only=agents,board] [--sets=empty,populated]
import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import puppeteer from "puppeteer-core";
import { EMPTY, POPULATED, FAILING, SLOW, HUGE } from "./fixtures.js";
import { checkAgentMessaging, checkChatMessaging } from "./robustness.mjs";
import { checkStoppedHistory } from "./stopped-history.mjs";
import { checkEffortProgress } from "./effort-progress.mjs";
import { checkCodeGraph } from "./code-review.mjs";
import { checkReviewPreview } from "./review-preview.mjs";
const SETS = { empty: EMPTY, populated: POPULATED, failing: FAILING, slow: SLOW, huge: HUGE };

const here = path.dirname(fileURLToPath(import.meta.url));
const dist = process.env.PHILIUM_RENDER_DIST ?? path.resolve(here, "../../dist");
const args = process.argv.slice(2);
const out = path.resolve(args.find((a) => !a.startsWith("--")) ?? path.join(here, "out"));
const opt = (k, d) => (args.find((a) => a.startsWith(`--${k}=`)) ?? `--${k}=${d}`).split("=")[1];
const only = opt("only", "").split(",").filter(Boolean);
const sets = opt("sets", "empty,populated,failing,slow,huge").split(",");
const CHROME = process.env.CHROME ?? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const SIZES = [[1440, 900], [1280, 800]];
const SCREENS = ["home", "agents", "board", "review", "environments", "chat", "terminals", "browser", "repos", "code", "activity", "stats", "providers", "settings"];

const mime = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".woff2": "font/woff2", ".png": "image/png", ".svg": "image/svg+xml" };
const server = http.createServer((req, res) => {
  let p = path.join(dist, decodeURIComponent(req.url.split("?")[0]));
  if (!fs.existsSync(p) || fs.statSync(p).isDirectory()) p = path.join(dist, "index.html");
  res.setHeader("content-type", mime[path.extname(p)] ?? "application/octet-stream");
  fs.createReadStream(p).pipe(res);
});
await new Promise((resolve,reject) => server.once('error',reject).listen(0, '127.0.0.1', resolve));
const port = server.address().port;

const stub = (fx, setName) => {
  const fixtures = fx;
  window.__permissionCalls = [];
  const listeners = new Map();
  window.__test = { sends: [], mode: "reject", holdShell: false, codeCalls: [] };
  // 1x1 PNG referenced by the populated agent transcript.
  window.__test.blobs = new Map([["abababababababababababababababababababababababababababababababab", Uint8Array.from(atob("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="), (c) => c.charCodeAt(0))]]);
  if (setName === "populated") {
    fixtures["workspace.worktrees"].forEach((w) => { w.state = "active"; });
    fixtures["codeindex.tree"].files.unshift({ path: "src/review.py", syms: [{ name: "review", kind: "function", line: 3 }] });
    fixtures["codeindex.graph"].nodes.push({ id: "src/review.py", path: "src/review.py", label: "review.py", kind: "file", group: "src", weight: 8, changed_by: [{ worktree_id: "w1", agent: "lion" }] });
  }
  const nativeScale = CanvasRenderingContext2D.prototype.scale;
  CanvasRenderingContext2D.prototype.scale = function(x, y) { this.canvas.__graphScale = x; return nativeScale.call(this, x, y); };
  window.__emit = (topic, payload) => {
    for (const [id, subscribed] of listeners) {
      if (subscribed === topic) window[`_${id}`]?.({ event: topic, id, payload });
    }
  };
  window.__agentState = (state) => {
    fixtures["agents.get"].state = state;
    fixtures["agents.list"].find((a) => a.id === "a1").state = state;
    window.__emit("agents:changed", { id: "a1" });
  };
  let permissionSeq = 2000;
  let cb = 1;
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
  window.__TAURI_INTERNALS__ = {
    metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main", windowLabel: "main" } },
    transformCallback(f) { const id = cb++; window[`_${id}`] = f; return id; },
    unregisterCallback() {},
    convertFileSrc: (p) => p,
    async invoke(cmd, a) {
      if (cmd === "plugin:event|listen") {
        listeners.set(a.handler, a.event);
        return a.handler;
      }
      if (cmd === "plugin:event|unlisten") {
        listeners.delete(a.eventId);
        return;
      }
      if (cmd.startsWith("plugin:")) return 1;
      if (cmd === "drafts_put_blob") {
        const bytes = new Uint8Array(a);
        const digest = [...new Uint8Array(await crypto.subtle.digest("SHA-256", bytes))].map((b) => b.toString(16).padStart(2, "0")).join("");
        const sig = bytes.slice(0, 4).join();
        const media = sig === "137,80,78,71" ? "image/png" : sig.startsWith("255,216,255") ? "image/jpeg" : null;
        if (!media) throw "Unsupported image. Use PNG, JPEG, GIF or WebP.";
        (window.__test.blobs ??= new Map()).set(digest, bytes);
        return { sha256: digest, media_type: media, size: bytes.length };
      }
      if (cmd === "drafts_blob") { const b = window.__test.blobs?.get(a.sha256); if (!b) throw "Draft image is missing"; return b; }
      if (cmd === "drafts_load") return structuredClone(window.__test.drafts?.[a.key] ?? { text: "", images: [] });
      if (cmd === "drafts_save") { (window.__test.drafts ??= {})[a.key] = structuredClone(a.draft); return null; }
      if (cmd === "agent_attachment") { (window.__test.attachmentCalls ??= []).push(a); const b = window.__test.blobs?.get(a.sha256); if (!b) throw "attachment not found in this conversation"; return b; }
      if (cmd === "capture_screenshot") { window.__test.captures = (window.__test.captures ?? 0) + 1; if (window.__test.screenshot === "denied") throw "screen-permission"; return window.__test.screenshot ?? new Uint8Array(); }
      if (cmd === "open_screen_settings") { window.__test.settingsOpened = (window.__test.settingsOpened ?? 0) + 1; return null; }
      if (fixtures.__delay) await new Promise((r) => setTimeout(r, fixtures.__delay));
      if (fixtures.__fail && (cmd === "rpc" || cmd === "hal")) {
        window.__n = (window.__n ?? 0) + 1;
        throw fixtures.__fail[window.__n % fixtures.__fail.length];
      }
      if (cmd === "rpc") {
        if(a.method==='managed.list' && a.args.task_id==='t2' && a.args.repo_id==='r1')return {tasks:[{id:'t2',agent:'a1',owner:'lion',title:'Refactor worktree cleanup'}],jobs:[{id:'board-job',task:'t2',agent:'a1',cid:'c'.repeat(64),state:'active',expires:Date.now()+60000}]};
        if(a.method==='managed.logs' && a.args.id==='board-job') {if(a.args.task_id!=='t2'||a.args.repo_id!=='r1')throw Error('Unscoped task logs');return {text:'board task fixture output'};}
        if(a.method==='managed.list') return {tasks:[{id:'t1',agent:'a1',owner:'lion',title:'Offline check'}],jobs:window.__test.managed?[{...window.__test.managed}]:[]};
        if(a.method==='managed.launch') {if(a.args.repo_id!=='r1'||a.args.task_id!=='t1'||a.args.agent_id!=='a1'||a.args.profile!=='alpha'||a.args.command!=='echo marker')throw Error('Incorrect preview binding');return window.__test.managed={id:'j1',task_name:'Offline check',agent_name:'lion',profile:'alpha',cid:'b'.repeat(64),expires:Date.now()+60000,state:'active'};}
        if(a.method==='managed.logs') return {text:'fixture marker',truncated:false};
        if(a.method==='managed.release') {if(a.args.id!=='j1'||a.args.repo_id!=='r1')throw Error('Wrong release');Object.assign(window.__test.managed,{state:'released',receipt:'removal verified'});return window.__test.managed;}
        if(a.method==='environments.profiles') return ['alpha','beta'];
        if(a.method==='environments.containers') return [{id:'a'.repeat(64),name:`${a.args.profile}-node`,cluster:'sandbox',image:'fixture:local',status:'Up'}];
        if(a.method==='environments.logs') {if(window.__test.holdLogs)await new Promise(resolve=>window.__test.releaseLogs=resolve);return {text:`${a.args.profile} fixture log`,truncated:false};}
        if (a.method === "agents.get_effort") return window.__test.efforts?.[a.args.id] ?? "default";
        if (a.method === "agents.set_effort") {
          if (window.__test.holdEffort) await new Promise((resolve) => { window.__test.releaseEffort = resolve; });
          (window.__test.efforts ??= {})[a.args.id] = a.args.effort;
          return a.args.effort;
        }
        if (window.__test.historyMode) {
          const t = window.__test;
          if (a.method === "agents.get") return structuredClone(t.historyAgents[a.args.id]);
          if (a.method === "agents.events") {
            const id = a.args.id;
            t.historyCalls.push(id);
            (t.historyPages ??= []).push({ id, after: a.args.after, limit: a.args.limit });
            const response = structuredClone((t.historyEvents[id] ?? []).filter((e) => e.seq > (a.args.after ?? 0)).slice(0, a.args.limit ?? 5000));
            if (t.holdHistory) await new Promise((resolve, reject) => t.historyPending.push({ id, response, resolve, reject }));
            if (t.failHistory) throw "Saved history unreadable";
            return response;
          }
          if (["agents.resume", "agents.stop"].includes(a.method)) {
            t.historyActions.push(a.method);
            t.historyAgents[a.args.id].live = a.method === "agents.resume";
            t.historyAgents[a.args.id].state = a.method === "agents.resume" ? "idle" : "stopped";
            window.__emit("agents:changed", { id: a.args.id });
            return {};
          }
        }
        if (["codeindex.source", "codeindex.changes", "codeindex.file_diff"].includes(a.method)) {
          window.__test.codeCalls.push({ method: a.method, ...a.args });
          const live = !!(a.args.worktree_id || a.args.working_tree);
          const base = { base_commit: "abc123base", revision: live ? `working-tree:${a.args.worktree_id || 'local'}` : "abc123base", worktree_id: a.args.worktree_id || null, path: a.args.path, truncated: false };
          if (window.__test.slowSource && a.method === "codeindex.source") await new Promise((r) => setTimeout(r, 400));
          if (a.method === "codeindex.changes") return { ...base, files: live ? [
            { path: "src/review.py", status: "M", added: 1, removed: 1 },
            { path: "src/new.py", status: "?", added: 2, removed: 0 },
            { path: "src/deleted.py", status: "D", added: 0, removed: 3 },
            { path: "image.bin", status: "M", added: null, removed: null },
          ] : [], note: live ? "" : "Base commit has no working-tree changes." };
          const deleted = a.args.path === "src/deleted.py";
          const binary = a.args.path === "image.bin";
          if (a.method === "codeindex.source") return { ...base, exists: !deleted, binary, language: "python", text: deleted || binary ? "" : `# ${base.revision}\n\ndef review():\n    return ${live ? 42 : 1}\n`, note: deleted ? "File was deleted in this worktree." : binary ? "Binary file." : "" };
          return { ...base, binary, patch: !live || binary ? "" : `diff --git a/${a.args.path} b/${a.args.path}\n--- a/${a.args.path}\n+++ b/${a.args.path}\n@@ -3,2 +3,2 @@\n def review():\n-    return 1\n+    return 42\n`, note: binary ? "Binary files differ." : !live ? "No changes in base commit view." : "" };
        }
        if (["agents.cancel", "agents.stop"].includes(a.method) && window.__test.failCancel) throw 'Cancellation unavailable';
        if (a.method === "agents.shell" && window.__test.holdShell) {
          await new Promise((resolve) => { window.__test.releaseShell = resolve; });
        }
        if (a.method === "agents.send") {
          window.__test.sends.push(a.args);
          await new Promise((resolve) => { window.__test.releaseSend = resolve; });
          if (window.__test.mode === "reject") throw 'HAL 409 Conflict: {"type":"busy","message":"session is busy"}';
          return {};
        }
        if (a.method === "agents.permission") {
          window.__permissionCalls.push(a.args);
          queueMicrotask(() => {
            const event = { type: "permission.resolved", seq: permissionSeq++, request_id: a.args.request_id, decision: a.args.decision };
            for (const [id, topic] of listeners) {
              if (topic === "agents:events") window[`_${id}`]?.({ event: topic, id, payload: { id: a.args.id, events: [event] } });
            }
          });
        }
        if (a.method === 'workspace.overview') {
          const id=a.args.repo_id;
          if (window.__failOverview) throw 'Fixture database unavailable';
          if (window.__holdOverview && id==='r1') await new Promise(r=>window.__resumeOverview=r);
          return {repo_id:id,path:`/fixture/${id}/same`,tasks:[{title:`task-${id}`,status:'open'}],agents:[],worktrees:[],resources:[{owner:`agent-${id}`,kind:'kind_cluster',handle:`cluster-${id}`,state:'live'}],pilot:true};
        }
        const v = fixtures[a.method];
        if (v !== undefined) return JSON.parse(JSON.stringify(v));
        if (/\.(list|tabs|names|proposals|ledger|inbox|threads|playbooks|branches|worktrees|models)$|repos_list/.test(a.method)) return [];
        if (/\.(set|save|send|open|navigate|show|hide|close|create|spawn)/.test(a.method)) return {};
        return {};
      }
      if (cmd === "hal") {
        if (a.path.endsWith("/cancel") && window.__test.failCancel) throw 'Cancellation unavailable';
        if (a.method === "POST" && a.path.endsWith("/messages")) {
          window.__test.sends.push(a.body);
          if (window.__test.mode === "reject") throw 'HAL 409 Conflict: {"type":"busy"}';
          const key = `hal GET ${a.path.replace(/\/messages$/, "/events")}`;
          const events = fixtures[key] ?? [];
          const seq = (events.at(-1)?.seq ?? 0) + 1;
          fixtures[key] = [...events, { type: "turn.started", seq }, ...(window.__test.chatWorking ? [{ type: "tool.started", seq: seq + 1, call_id: "chat-progress-fixture", tool: "fixture_tool", arguments: {} }] : [{ type: "assistant.completed", seq: seq + 1, text: "Accepted" }])];
          return {};
        }
        const key = `hal ${a.method} ${a.path.split("?")[0]}`;
        if (fixtures[key] !== undefined) {
          const value = JSON.parse(JSON.stringify(fixtures[key]));
          if (a.path.includes("/events")) return value.filter((e) => e.seq > Number(new URLSearchParams(a.path.split("?")[1]).get("after") ?? 0));
          return value;
        }
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
  await page.evaluateOnNewDocument((r) => {localStorage.setItem("route", r);if(r==='board')localStorage.setItem('agents.viewMode','grid');if(r==='environments')localStorage.setItem('philium.activeRepo','r1');}, route);
  page.on("pageerror", (e) => { problems.push(`${setName} ${route} pageerror: ${e.message}`); });
  page.on("console", (m) => { if (m.type() === "error") problems.push(`${setName} ${route} console.error: ${m.text().slice(0, 200)}`); });
  await page.goto(`http://127.0.0.1:${port}/`);
  await page.waitForSelector(".sidebar");
  await sleep(fx.__delay ? 700 : 500);
  return page;
}
const clickText = (page, sel, text) => page.evaluate((s, t) => { const el = [...document.querySelectorAll(s)].find((e) => e.textContent.trim().includes(t)); el?.click(); return !!el; }, sel, text);

for (const setName of sets) {
  const fx = SETS[setName];
  for (const [w, h] of (only.length === 1 && ['review','home'].includes(only[0]) ? [...SIZES,[800,600]] : SIZES)) {
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
      if (s === "stats" && setName === "populated") {
        try {
          await page.waitForFunction(() => {
            const table = document.querySelector('table[aria-label="Provider usage"]');
            const row = (name) => [...(table?.tBodies[0]?.rows ?? [])]
              .find((r) => r.cells[0]?.textContent.includes(name));
            return row("ChatGPT")?.cells[1]?.textContent.includes("7d: 28% used · resets in")
              && Number(row("ChatGPT")?.cells[2]?.textContent) > 0
              && row("Claude")?.cells[1]?.textContent.includes("No quota reported")
              && row("OpenRouter")?.textContent.includes("No requests in this range");
          }, { timeout: 3000 });
        } catch (e) { problems.push(`${w} Stats provider quota and traffic: ${e.message}`); }
      }
      if(s==='environments' && setName==='populated') {
        try {
          await clickText(page,'button','Inspect local machine'); await page.waitForSelector('label select option[value="alpha"]');
          const sel='label select:has(option[value="alpha"])'; await page.select(sel,'alpha');
          await page.waitForSelector('[aria-label="Runtime inventory"] summary'); await page.click('[aria-label="Runtime inventory"] summary');
          await clickText(page,'button','View logs'); await page.waitForFunction(()=>document.querySelector('aside pre')?.textContent==='alpha fixture log');
          await page.evaluate(()=>window.__test.holdLogs=true);await clickText(page,'button','Refresh logs');await page.waitForFunction(()=>!!window.__test.releaseLogs);
          await page.select(sel,'beta');await page.evaluate(()=>window.__test.releaseLogs());await page.waitForFunction(()=>document.querySelector('[aria-label="Runtime inventory"]')?.innerText.includes('beta'));
          if(await page.$('aside[aria-label="Container logs"]'))throw Error('Stale peer logs remained visible');
          await clickText(page,'button','This checkout');if(await page.$('[aria-label="Runtime inventory"]')) throw Error('Machine inventory leaked into checkout');
          await clickText(page,'button','New task sandbox');await page.waitForSelector('fieldset select option[value="t1"]');await page.select('fieldset select:has(option[value="t1"])','t1');await page.select('fieldset select:has(option:not([value]))','alpha');await page.type('fieldset textarea','echo marker');
          await clickText(page,'button','Preview sandbox');await page.waitForFunction(()=>document.body.innerText.includes('Start offline sandbox')&&!window.__test.managed);await clickText(page,'button','Start offline sandbox');await page.waitForSelector('[aria-label="Managed task sandboxes"] summary');await page.click('[aria-label="Managed task sandboxes"] summary');await clickText(page,'button','Sandbox logs');await page.waitForFunction(()=>document.body.innerText.includes('fixture marker'));
          await clickText(page,'button','Refresh sandboxes');await clickText(page,'button','Release sandbox');await clickText(page,'button','Keep sandbox');if(await page.evaluate(()=>window.__test.managed.state)!=='active')throw Error('Dismissed release removed sandbox');await clickText(page,'button','Release sandbox');await clickText(page,'button','Confirm release');await page.waitForFunction(()=>document.body.innerText.includes('removal verified'));
        } catch(e){problems.push(`${w} environments: ${e.message}`);}
      }
      if (s === 'review' && setName === 'populated') {
        try { await checkReviewPreview(page); } catch (e) { problems.push(`${w} review: ${e.message}`); }
      }
      if (s === "agents") {
        if (setName === "populated") await page.evaluate(() => { window.__test.holdShell = true; });
        if (await clickText(page, ".list-item", "lion")) {
          await sleep(500); await shot(page, "agents-detail");
          if (setName === "populated") {
            if (!(await page.$(".composer textarea"))) problems.push(`${w} delayed shell hid conversation`);
            await page.evaluate(() => { window.__test.holdShell = false; window.__test.releaseShell?.(); });
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
              const resolved = [...document.querySelectorAll(".perm.done")];
              if (resolved.length !== 3 || resolved.some((c) => c.querySelector("button"))) errors.push("resolved permission controls remain actionable");
              return errors;
            });
            errors.forEach((e) => problems.push(`${w} browser approval: ${e}`));
            await shot(page, "agents-permissions-resolved");
            for (const error of await checkEffortProgress(page, shot)) problems.push(`${w} effort/progress: ${error}`);
            for (const error of await checkAgentMessaging(page)) problems.push(`${w} messaging: ${error}`);
            await shot(page, "agents-messaging-checked");
            for (const error of await checkStoppedHistory(page, shot)) problems.push(`${w} stopped history: ${error}`);
          }
        } else if (setName === "populated") problems.push(`${w} browser approval: agent detail did not open`);
        if (await clickText(page, "button", "New agent")) { await sleep(300); await shot(page, "modal-new-agent"); }
      }
      if (s === "code" && setName === "populated") {
        for (const error of await checkCodeGraph(page, shot)) problems.push(`${w} code review: ${error}`);
      }
      if (s === "providers") {
        if (await clickText(page, "button", "Add provider")) { await sleep(300); await shot(page, "modal-add-provider"); }
      }
      if (s === "chat" && setName === "populated") {
        if (await clickText(page, ".list-item", "Gateway retry")) {
          await sleep(600); await shot(page, "chat-conversation");
          for (const error of await checkChatMessaging(page)) problems.push(`${w} chat messaging: ${error}`);
        }
      }
      if (s === "board" && setName === "populated") {
        try {await clickText(page,'.board-task-title','Refactor worktree');await page.waitForSelector('.board-drawer details summary');await page.click('.board-drawer summary');await clickText(page,'.board-drawer button','Sandbox logs');await page.waitForFunction(()=>document.querySelector('.board-drawer pre')?.textContent==='board task fixture output');await shot(page,'board-detail');await clickText(page,'.board-drawer button','Open agent');await page.waitForFunction(()=>!document.querySelector('.board-drawer')&&document.querySelector('.agent-head .agent-chip')?.textContent==='lion'&&localStorage.getItem('agents.viewMode')==='grid',{timeout:4000});}catch(e){problems.push(`${w} board inspection: ${e.message} ${await page.evaluate(()=>[localStorage.getItem('route'),document.querySelector('.main')?.innerText.slice(0,420),document.querySelector('.toast')?.textContent].join(' '))}`);}
      }
      if (s === "agents" || s === "chat") {
        await page.evaluate((s) => window.__philiumGo(s, s === "agents" ? { id: "deleted-id" } : { session: "deleted-id" }), s);
        await sleep(600); await shot(page, `${s}-stale-id`);
        const t = await page.evaluate(() => document.querySelector(".main")?.innerText ?? "");
        if (setName === "populated" && !/not found/i.test(t)) problems.push(`${setName} ${w}x${h} ${s}: stale id shows no not-found state`);
      }
      if (s === "home") {
        if (setName==='populated') {
          const picker='select[aria-label="Active workspace repository"]';
          await page.evaluate(()=>window.__holdOverview=true);
          await page.select(picker,'r1'); await page.waitForFunction(()=>!!window.__resumeOverview);
          await page.select(picker,'r3'); await page.waitForFunction(()=>document.querySelector('.checkout-home')?.innerText.includes('cluster-r3'));
          await page.evaluate(()=>window.__resumeOverview()); await sleep(80);
          const info=await page.$eval('.checkout-home',el=>el.innerText);
          if (info.includes('cluster-r1') || !info.includes('task-r3') || !info.includes('not a live Docker')) problems.push(`home ${w}: checkout scope or resource attribution`);
          await page.evaluate(()=>window.__failOverview=true); await clickText(page,'button','Refresh');
          await page.waitForSelector('[role="alert"]');
          if (await page.$('.checkout-home')) problems.push(`home ${w}: stale records after error`);
          await page.evaluate(()=>window.__failOverview=false); await clickText(page,'button','Refresh'); await page.waitForSelector('.checkout-home');
          await shot(page,'selected-checkout'); await page.select(picker,'');
        }
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
