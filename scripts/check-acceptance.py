#!/usr/bin/env python3
"""Acceptance of a Pilot bundle through its own window, in a disposable profile.

    python3 scripts/check-acceptance.py '/path/to/Philium Pilot.app' [--out DIR] [--only coord,review,...]

What this harness does and does not do
- It starts the bundle's executable with a throwaway HOME and `--pilot-ui-script`. The window is
  hidden and cannot take focus. Each scenario clicks, types and reads the rendered page, exactly
  what a person can do. The bundle refuses scripts that reach for the backend bridge.
- It supplies stand-ins a person would otherwise supply: a model endpoint, an origin, a
  deployment observer and Git checkouts. All of them are synthetic and local.
- It observes: the page text, the stand-ins' request logs, the profile's files and the process
  tree. It never edits the profile's database and never performs a step the interface lacks.

Nothing outside the scratch folder is created. No LaunchServices, no keystrokes, no network
beyond loopback (the `--public` phase, off by default, reads public GitHub metadata).
"""
import sqlite3
import argparse, json, os, plistlib, shutil, signal, stat, subprocess, sys, tempfile, threading, time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE / 'acceptance'))
import synthetic as syn  # noqa: E402

LIB = (HERE / 'acceptance/ui-lib.js').read_text()
KEY = 'synthetic-key-not-a-secret-0001'


class Run:
    def __init__(self, app, root, out):
        self.app, self.root, self.out = Path(app), Path(root), out
        info = plistlib.loads((self.app / 'Contents/Info.plist').read_bytes())
        assert info['CFBundleIdentifier'] == 'dev.gunnarsson.philium.pilot', 'Not a Pilot bundle'
        subprocess.run(['/usr/bin/codesign', '--verify', '--deep', '--strict', str(self.app)], check=True)
        self.exe = self.app / 'Contents/MacOS' / info['CFBundleExecutable']
        self.home = self.root / 'home'; self.tmp = self.root / 'tmp'
        self.home.mkdir(parents=True); self.tmp.mkdir()
        (self.home / '.philium-pilot-fixture').write_text('disposable\n')
        # Decoys: another harness's variables, tool directory and private files. None may be used.
        decoy = self.home / '.jcode/bin'; decoy.mkdir(parents=True)
        self.decoy_marker = self.root / 'decoy-tool-ran'
        (decoy / 'jcode').write_text(f'#!/bin/sh\necho ran >> "{self.decoy_marker}"\n'); (decoy / 'jcode').chmod(0o755)
        (self.home / '.jcode/secret').write_text('decoy\n')
        (self.home / '.ssh').mkdir(); (self.home / '.ssh/id_probe').write_text('decoy\n')
        self.env = {'HOME': str(self.home), 'TMPDIR': str(self.tmp), 'PATH': f'{decoy}:/usr/bin:/bin:/usr/sbin:/sbin', 'SHELL': '/bin/zsh',
                    'GIT_CONFIG_NOSYSTEM': '1', 'JCODE_SESSION_ID': 'decoy', 'HERDR_SOCKET': 'decoy'}
        self.rows, self.children, self.n = [], set(), 0
        self.profile = self.home / 'Philium Pilot'

    # -- evidence ---------------------------------------------------------------------------
    def redact(self, v):
        s = v if isinstance(v, str) else json.dumps(v, default=str)
        return s.replace(str(self.root), '<scratch>').replace(str(Path.home()), '<home>')[:700]

    def check(self, row, what, ok, observed=''):
        self.rows.append({'row': row, 'check': what, 'ok': bool(ok), 'observed': self.redact(observed)})
        print(f"{'PASS' if ok else 'FAIL'} {row} {what}" + (f" :: {self.redact(observed)[:220]}" if observed != '' else ''), flush=True)
        return bool(ok)

    # -- the packaged window ----------------------------------------------------------------
    def ui(self, body, params=None, seconds=150, kill_when=None, extra_env=None):
        """One app lifetime: start hidden, run the scenario, quit normally (or be killed)."""
        self.n += 1
        script = self.root / f'ui-{self.n:02d}.js'
        script.write_text(LIB.replace('/*SCENARIO*/', body).replace('__PARAMS__', json.dumps(params or {})))
        proc = subprocess.Popen([str(self.exe), '--pilot-ui-script', str(script), str(seconds)], env={**self.env, **(extra_env or {})},
                                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        stop = threading.Event()

        def watch():
            while not stop.is_set():
                rows = subprocess.run(['/bin/ps', '-axo', 'pid=,ppid=,comm='], capture_output=True, text=True).stdout.splitlines()
                tree, kids = {}, {proc.pid}
                for r in rows:
                    p = r.split(None, 2)
                    if len(p) == 3:
                        tree[int(p[0])] = (int(p[1]), p[2])
                grew = True
                while grew:
                    grew = False
                    for pid, (ppid, _) in tree.items():
                        if ppid in kids and pid not in kids:
                            kids.add(pid); grew = True
                self.children.update(tree[k][1] for k in kids if k in tree)
                if kill_when and kill_when():
                    proc.send_signal(signal.SIGKILL); return
                stop.wait(0.4)
        t = threading.Thread(target=watch, daemon=True); t.start()
        try:
            out, err = proc.communicate(timeout=seconds + 40)
        except subprocess.TimeoutExpired:
            proc.kill(); out, err = proc.communicate()
        stop.set(); t.join(3)
        res = None
        for line in out.decode(errors='replace').splitlines():
            if line.startswith('{"ui_result"'):
                res = json.loads(line)['ui_result']
        if res is None:
            res = {'ok': False, 'error': f'no result (exit {proc.returncode}): {err.decode(errors="replace")[-300:]}', 'result': None, 'log': []}
        res['exit'] = proc.returncode
        return res

    def value(self, res):
        return (res or {}).get('result') or {}


# ============================== scenarios (page scripts) ==================================
SETUP = r'''
await go('Repos');
for (const p of P.repos) {
  await click('Add local repo');
  const m = await until(modal, 'add dialog');
  await fill('Folder', p, m);
  await click('Register repository', m);
  await until(() => !modal(), 'dialog closed', 25000);
}
const labels = await until(() => { const o = Array.from(picker().options).map(txt); return o.length > P.repos.length ? o : null; }, 'checkouts in the picker', 15000).catch(() => Array.from(picker().options).map(txt));
await go('Providers');
const add = async (name, key, trusted, url = P.model_url) => {
  await click('Add provider');
  const m = await until(modal, 'provider dialog');
  await fill('Name', name, m);
  await fill('Base URL', url, m);
  if (key) await fill('API key', key, m);
  const t = all('label.toggle', m).find((x) => txt(x).includes('Untrusted provider'));
  if (trusted && t.classList.contains('on')) { t.click(); await sleep(80); }
  await click('Save', m);
  const outcome = await until(() => (!modal() ? 'saved' : (all('.shield', m).map(txt)[0] || null)), 'save result', 20000);
  if (outcome !== 'saved') { await click('Cancel', modal()); await until(() => !modal(), 'closed'); }
  return outcome;
};
const keyed = await add('Synthetic', P.key, true);
const keyless = keyed === 'saved' ? null : await add('Synthetic', '', true);
const strict = await add('Synthetic Strict', keyed === 'saved' ? P.key : '', false);
const second = P.model_url2 && keyed === 'saved' ? await add('Second', P.key, true, P.model_url2) : null;
await until(() => all('.card', pane()).filter((c) => /1 models/.test(txt(c))).length >= 2, 'models discovered', 20000).catch(() => null);
return { labels, keyed, keyless, strict, second, cards: all('.card', pane()).map(txt).filter((c) => c.startsWith('Synthetic')) };
'''

COORDINATE = r'''
await workspace(P.checkout);
await go('Home');
const coord = await until(() => all('section[aria-label="Coordinator"]')[0], 'coordinator section', 15000).catch(() => null);
const entry = coord ? txt(coord) : null;
if (coord && button('Start coordinator', coord)) {
  await click('Start coordinator', coord);
} else {
  await go('Agents'); await click('New agent');
}
let m = await until(modal, 'spawn dialog');
await fill('Name', P.lead, m);
choose(field('Role', m), 'orchestrator');
const preset = { role: txt(field('Role', m).selectedOptions[0]), repo: txt(field('Repo', m).selectedOptions[0]) };
await click('Start Agent', m);
const started = await until(() => (!modal() ? 'started' : (all('.err', m).map(txt)[0] || null)), 'spawn result', 45000);
if (started !== 'started') return { entry, preset, started };
const head = () => txt(document.querySelector('.agent-head'));
await until(() => /Idle/.test(head()), 'coordinator idle', 45000);
const composer = await until(() => document.querySelector('textarea.composer-input'), 'composer');
const send = () => document.querySelector('button.composer-send[aria-label="Send"]');
const ready = await until(() => !composer.disabled, 'composer ready', 12000).then(() => true, () => false);
if (!ready) return { entry, preset, started, composer: txt(composer.closest('.composer') || composer.parentElement).slice(-160) };
const say = async (text, expect, ms = 60000) => {
  type(composer, text); await until(() => !send().disabled, 'send enabled', 8000); send().click();
  return until(() => { const t = txt(document.querySelector('.agent-main')); const i = t.lastIndexOf(expect); return i >= 0 ? t.slice(i, i + 260) : null; }, `reply "${expect}"`, ms);
};
const hello = await say('Hello coordinator, are you there?', 'SYNTHETIC REPLY from');
const delegated = await say(`DELEGATE "${P.task}" TO ${P.worker}`, 'DELEGAT');
const pills = await until(() => { const p = all('.cluster-subpill').map(txt); return p.length ? p : null; }, 'helper listed under the coordinator', 20000).catch(() => []);
const report = await until(() => { const t = txt(document.querySelector('.agent-main')); const i = t.indexOf('Mail delivered by Philium'); return i >= 0 && t.includes(`From ${P.worker}:`) ? t.slice(i, i + 220) : null; }, 'helper report in the coordinator conversation', 45000).catch(() => null);
return { entry, preset, started, hello, delegated, pills, report, head: head() };
'''

BOARD = r'''
await workspace(P.checkout);
await go('Board');
const card = () => all('article.board-task').find((a) => txt(a).includes(P.task));
await until(card, 'task card', 20000);
if (P.wait_status) await until(() => { const col = card()?.closest('.board-column'); return col && txt(col.querySelector('h3')).startsWith(P.wait_status); }, `task in ${P.wait_status}`, P.wait_ms || 60000).catch(() => null);
const column = txt(card().closest('.board-column').querySelector('h3')).replace(/\s*\d+$/, '');
const owner = txt(card().querySelector('.board-owner'));
card().querySelector('button.board-task-title').click();
const drawer = await until(() => all('aside.board-drawer')[0], 'task drawer');
await sleep(600);
const ownerLine = txt(all('p', drawer).find((p) => txt(p).startsWith('Owner:')));
const handover = txt(drawer.querySelector('[aria-label="Task handover"]'));
const history = all('.board-history article', drawer).map(txt).slice(0, 30);
const out = { column, owner, ownerLine, handover, history, cards: all('article.board-task').map((a) => txt(a).slice(0, 90)) };
if (P.note) {
  const label = all('label', drawer).find((l) => txt(l).startsWith('Note or submission summary'));
  type(label.querySelector('textarea'), P.note);
  await click('Send message', drawer);
  await sleep(800);
  out.noteSent = all('.board-error').map(txt)[0] || 'sent';
}
if (P.open_agent) {
  await click('Open agent', drawer);
  await until(() => all('.agent-head').length && /lead:/.test(txt(document.querySelector('.agent-head'))), 'worker view', 15000).catch(() => null);
  out.agentHead = txt(document.querySelector('.agent-head'));
  out.transcript = txt(document.querySelector('.agent-main')).slice(0, 1500);
  await click('Changes', document.querySelector('.agent-head'));
  out.changes = await until(() => { const t = txt(document.querySelector('.agent-main')); return /status\.txt|No changes|diff/i.test(t) ? t.slice(0, 700) : null; }, 'changes tab', 15000).catch((e) => String(e));
  await click('Conversation', document.querySelector('.agent-head'));
}
return out;
'''

AGENT = r'''
const head = () => txt(document.querySelector('.agent-head'));
const openAgent = async (name) => {
  await go('Agents');
  const row = await until(() => all('.agent-row').find((r) => txt(r).startsWith(name)), `agent ${name}`, 20000);
  row.click();
  await until(() => head().startsWith(name), `${name} opened`, 15000);
};
const live = async () => {
  if (!button('Resume', document.querySelector('.agent-head'))) return 'already running';
  await click('Resume', document.querySelector('.agent-head'));
  await until(() => button('Stop', document.querySelector('.agent-head')) && /Idle/.test(head()), 'running again', 60000);
  return 'resumed';
};
const composer = () => document.querySelector('textarea.composer-input');
const sendBtn = () => document.querySelector('button.composer-send[aria-label="Send"]');
const convo = () => txt(document.querySelector('.agent-main'));
const sendOnly = async (text) => {
  await until(() => composer() && !composer().disabled, 'composer ready', 20000);
  type(composer(), text);
  await until(() => !sendBtn().disabled, 'send enabled', 8000);
  sendBtn().click();
};
const say = async (text, expect, ms = 60000) => {
  await sendOnly(text);
  return until(() => { const t = convo(); const i = t.lastIndexOf(expect); return i >= 0 ? t.slice(i, i + 200) : null; }, `reply "${expect}"`, ms);
};
'''

REDIRECT = AGENT + r'''
await workspace(P.checkout);
await openAgent(P.worker);
const before = head();
const resumed = await live();
const delivered = await until(() => { const t = convo(); const i = t.indexOf('From you: Task'); return i >= 0 ? t.slice(i, i + 120) : null; }, 'note from the person delivered', 30000).catch(() => null);
await until(() => /Idle/.test(head()), 'idle after reading the note', 30000).catch(() => null);
const redirected = await say('REDIRECT rename to state.txt', 'REDIRECTED');
// Interrupt one turn and keep the agent.
await sendOnly('SLOW 40');
await until(() => /Working/.test(head()), 'working', 20000);
await sleep(1500);
document.querySelector('button.composer-send.stop[aria-label="Stop"]').click(); note('click the turn Stop');
const interrupted = await until(() => /Idle/.test(head()), 'idle after interrupting the turn', 30000).then(() => head(), (e) => String(e));
const after = await say('Still there?', 'SYNTHETIC REPLY from');
// Stop the agent in the middle of a turn.
await sendOnly('SLOW 40');
await until(() => /Working/.test(head()), 'working again', 20000);
await sleep(1500);
await click('Stop', document.querySelector('.agent-head'));
const stopped = await until(() => /Stopped/.test(head()) && button('Resume', document.querySelector('.agent-head')), 'stopped', 30000).then(() => head(), (e) => String(e));
const disabled = composer() ? composer().disabled : null;
await click('Changes', document.querySelector('.agent-head'));
const changes = await until(() => { const t = convo(); return /state\.txt|notes\.txt/.test(t) ? t.slice(0, 900) : null; }, 'changes', 15000).catch(() => convo().slice(0, 400));
return { before, resumed, note: delivered, redirected, interrupted, after, stopped, disabled, changes };
'''

QUEUE = AGENT + r'''
// Typing while the agent works: the message waits above the box and is sent when the step ends.
await workspace('All Workspaces');
await openAgent(P.name);
await live();
await sendOnly('SLOW 10');
await until(() => document.querySelector('button.composer-send.stop'), 'the agent is working', 20000);
const queued = () => all('[data-queued]').map(txt);
type(composer(), 'QUEUED first note');
await sleep(150);
composer().dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
await until(() => queued().length === 1, 'first message queued', 8000);
type(composer(), 'QUEUED second note');
await until(() => document.querySelector('button.composer-send[aria-label="Queue message"]'), 'queue button', 5000);
document.querySelector('button.composer-send[aria-label="Queue message"]').click();
await until(() => queued().length === 2, 'second message queued', 8000);
const whileWorking = { queued: queued(), label: txt(document.querySelector('[aria-label="Queued messages"] .xs')), status: txt(document.querySelector('.composer-status')) };
// Edit takes one back into the box; Enter queues it again.
button('Edit', all('[data-queued]')[1]).click();
await until(() => queued().length === 1 && /QUEUED second note/.test(composer().value), 'taken back', 8000);
const edited = { queued: queued(), box: composer().value };
composer().dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
await until(() => queued().length === 2, 'queued again', 8000);
const finished = await until(() => {
  const t = convo(), i = t.lastIndexOf('SYNTHETIC REPLY');
  return queued().length === 0 && t.includes('SLOW TURN FINISHED') && i > t.lastIndexOf('SLOW TURN FINISHED') ? t.slice(i, i + 160) : null;
}, 'queued messages delivered after the step', 60000).catch(() => null);
return { whileWorking, edited, finished, left: queued() };
'''

ERRORS = AGENT + r'''
// A provider refusal reads as one sentence with the fix; the original stays under Error details.
await workspace('All Workspaces');
await openAgent(P.name);
await live();
const errors = () => all('.msg.error', document.querySelector('.agent-main'));
const before = errors().length;
await sendOnly('RATE LIMITED now');
const shown = await until(() => errors().length > before ? errors()[errors().length - 1] : null, 'the refusal shown', 90000).catch(() => null);
if (!shown) return { shown: false, convo: convo().slice(-400), banner: txt(document.querySelector('.agent-view .shield.danger')) };
const out = { shown: true, plain: txt(shown).slice(0, 300), details: (shown.querySelector('details pre')?.textContent || '').slice(0, 500) };
// The agent says when it continues, then continues on its own after the wait.
out.waiting = await until(() => { const b = all('.shield[role="status"]', document.querySelector('.agent-view')).map(txt).find((t) => /continues at/.test(t)); return b || null; }, 'the wait announced', 15000).catch(() => '');
out.continued = await until(() => { const t = convo(), i = t.lastIndexOf('SYNTHETIC REPLY'); return i > t.lastIndexOf('limiting requests') && t.slice(i).includes('stopped by the model provider') ? t.slice(i, i + 160) : null; }, 'continued after the wait', 220000).catch(() => '');
out.after = all('.shield[role="status"]', document.querySelector('.agent-view')).map(txt).find((t) => /rate limit/.test(t)) || '';
return out;
'''

MODEL_PICKER = AGENT + r'''
// An agent of its own, outside any checkout: an agent with helpers keeps one model for the family.
await workspace('All Workspaces');
await go('Agents'); await click('New agent');
const m = await until(modal, 'spawn dialog');
await fill('Name', P.name, m);
choose(field('Repo', m), 'No repo');
await click('Start Agent', m);
const started = await until(() => (!modal() ? 'started' : (all('.err', m).map(txt)[0] || null)), 'spawn result', 45000);
if (started !== 'started') return { opened: false, started };
await until(() => head().startsWith(P.name) && /Idle/.test(head()), 'the new agent is idle', 45000).catch(() => null);
const shownModel = () => txt(document.querySelector('.agent-head button[aria-label="Choose model"]'));
const modelBefore = shownModel();
await sendOnly('/model');
const pop = await until(() => all('.model-picker-pop')[0], 'the list opened by /model', 8000).catch(() => null);
if (!pop) return { opened: false, modelBefore, toasts: toasts() };
const search = () => pop.querySelector('input');
const chip = (name) => all('.model-picker-prov-chip', pop).find((c) => txt(c) === name);
const chips = () => all('.model-picker-prov-chip', pop).map((c) => txt(c) + (c.classList.contains('on') ? ' (on)' : ''));
const items = () => all('button.model-picker-item', pop).map(txt);
const labels = () => all('.model-picker-group-label', pop).map(txt);
const more = () => txt(pop.querySelector('.model-picker-more'));
const first = { chips: chips(), items: items(), labels: labels(), searchFocused: document.activeElement === search(), foot: txt(pop.querySelector('.model-picker-foot')) };
chip(P.other).click(); await sleep(250);
const other = { chips: chips(), items: items(), labels: labels(), more: more() };
pop.querySelector('.model-picker-more').click(); await sleep(250);
const everything = { items: items(), more: more() };
type(search(), 'synthetic'); await sleep(300);
const searched = { items: items(), labels: labels(), chips: chips().length };
type(search(), ''); await sleep(250);
chip(P.other).click(); await sleep(250);
search().focus();
search().dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true })); await sleep(150);
const focused = txt(document.activeElement);
document.activeElement.click(); note('choose the focused model');
const closed = await until(() => !all('.model-picker-pop').length, 'the list closed', 8000).then(() => true, () => false);
const modelAfter = await until(() => (shownModel() !== modelBefore ? shownModel() : null), 'the model shown in the header changed', 10000).catch(() => shownModel());
await sleep(1600);
const composerFocused = document.activeElement === composer();
const reply = await say('Still there after the switch?', 'SYNTHETIC REPLY from').catch((e) => String(e));
return { opened: true, modelBefore, first, other, everything, searched, focused, closed, modelAfter, composerFocused, reply, toasts: toasts() };
'''

DRAFT = r'''
await workspace(P.checkout);
await go('Review');
const box = await until(() => all('details[aria-label="Release PR revisions"]')[0], 'release section', 15000);
if (!box.open) box.querySelector('summary').click();
const setup = box.querySelector('details[aria-label="Release verification setup"]');
if (!setup.open) setup.querySelector('summary').click();
const status = () => txt(all('p[role="status"]', box)[0]);
const before = { pinned: txt(setup.querySelector('[aria-label="Pinned verifier"]')), summary: txt(box.querySelector('[aria-label="Verification setup summary"]')) };
await click('Draft checks with an agent', setup);
const started = await until(() => (/is drafting checks|Not started/.test(status()) ? status() : null), 'agent started', 60000);
const prop = await until(() => all('section[aria-label="Proposed checks"]', setup)[0], 'proposal shown', 120000).catch(() => null);
if (!prop) return { before, started, status: status() };
const proposed = all('[data-check]', prop).map(txt), by = txt(prop.querySelector('h3'));
// A press in the first instant after the proposal appears was not registered in the packaged
// window (cause not established; the same press works in a browser and a moment later here).
// A person cannot be that fast, so wait as a person would and press what is on the page now.
await sleep(1500);
const live = () => all('section[aria-label="Proposed checks"]', setup)[0];
const redrawn = live() !== prop;
await click('Use these checks', live());
const shown = await until(() => all('section[aria-label="Checks to confirm"]', setup)[0], 'checks to confirm', 20000).catch(() => null);
if (!shown) return { before, started, by, proposed, status: status(), alerts: all('[role="alert"]', box).map(txt), connected: prop.isConnected, redrawn, sections: all('section[aria-label="Proposed checks"]', setup).length, buttons: all('button', setup).map((b) => txt(b) + (b.disabled ? ' (off)' : '')), toasts: toasts() };
const toConfirm = all('section[aria-label="Checks to confirm"] [data-check]', setup).map(txt);
await click('Confirm and pin these checks', setup);
const pinned = await until(() => { const t = txt(setup.querySelector('[aria-label="Pinned verifier"]')); return t.includes('You confirmed') ? t : null; }, 'pinned', 25000).catch(() => status());
return { before, started, by, proposed, toConfirm, pinned, redrawn, summary: txt(box.querySelector('[aria-label="Verification setup summary"]')), labels: Array.from(picker().options).map(txt), left: all('section[aria-label="Proposed checks"]', setup).length };
'''

CONNECT = r'''
await workspace(P.checkout);
await go('Review');
const box = await until(() => all('details[aria-label="Release PR revisions"]')[0], 'release section', 15000);
if (!box.open) box.querySelector('summary').click();
const line = () => txt(box.querySelector('[aria-label="GitHub connection"]'));
const before = await until(() => (line().startsWith('GitHub') ? line() : null), 'connection line', 10000);
await click('Connect GitHub', box);
const said = await until(() => { const t = txt(all('p[role="status"]', box)[0]); return /^GitHub (not )?connected/.test(t) ? t : null; }, 'answer', 30000);
return { before, said, after: line() };
'''

ORCHESTRATE = r'''
await workspace(P.checkout);
await go('Review');
const box = await until(() => all('details[aria-label="Release PR revisions"]')[0], 'release section', 15000);
if (!box.open) box.querySelector('summary').click();
const status = () => txt(all('p[role="status"]', box)[0]);
type(box.querySelector('form input[type="url"]'), P.url);
await click('Review with agents', box);
const started = await until(() => (/is organising the review|Review not started|Not checked/.test(status()) ? status() : null), 'review started', 150000);
const sec = await until(() => all('section[aria-label="Agent review"]', box)[0], 'agent review shown', 60000).catch(() => null);
if (!sec) return { started };
const rows = () => all('tr[data-item]', all('section[aria-label="Agent review"]', box)[0]);
await until(() => rows().length >= 3, 'plan listed', 120000).catch(() => null);
const planned = rows().map((tr) => txt(tr.querySelector('th')).replace(/^P\d\s*/, ''));
await until(() => rows().filter((tr) => tr.dataset.result).length >= 3 && all('details[aria-label="Review report"]', box).length, 'results and report', 180000).catch(() => null);
const now = all('section[aria-label="Agent review"]', box)[0];
return { started, planned, headline: txt(now.querySelector('h3')), progress: txt(now.querySelector('.verdict p')),
  items: rows().map((tr) => [tr.dataset.severity, tr.dataset.result, txt(tr.querySelector('td:last-child')).slice(0, 220), tr.dataset.origin, txt(tr.querySelector('th'))]),
  report: (now.querySelector('details[aria-label="Review report"] [data-report]')?.dataset.report || '').slice(0, 160), reportBy: txt(now.querySelector('details[aria-label="Review report"] summary')),
  facts: txt(all('section[aria-label="Release decision"] h3', box)[0]) };
'''

DELEGATE_SLOW = AGENT + r'''
await workspace(P.checkout);
await openAgent(P.lead);
const resumed = await live();
const delegated = await say(`DELEGATE "${P.task}" TO ${P.worker} THEN SLOW ${P.secs}`, 'DELEGAT');
// Stay open. The harness ends this app lifetime by killing the process in the middle of the helper's turn.
await sleep(P.secs * 1000);
return { resumed, delegated };
'''

AFTER_RESTART = AGENT + r'''
await workspace(P.checkout);
await go('Agents');
await until(() => all('.agent-row').length >= P.agents, 'agents listed', 20000).catch(() => null);
const rows = all('.agent-row').map((r) => txt(r).split(' ')[0]);
await openAgent(P.worker);
const stoppedHead = head();
const notice = all('.shield.danger', document.querySelector('.agent-view')).map(txt)[0] || '';
const transcriptBefore = convo().slice(0, 300);
await go('Board');
const card = () => all('article.board-task').find((a) => txt(a).includes(P.task));
await until(card, 'task card', 20000);
const boardBefore = { column: txt(card().closest('.board-column').querySelector('h3')).replace(/\s*\d+$/, ''), owner: txt(card().querySelector('.board-owner')), tasks: all('article.board-task').length };
card().querySelector('button.board-task-title').click();
const drawer = await until(() => all('aside.board-drawer')[0], 'drawer'); await sleep(500);
const handover = txt(drawer.querySelector('[aria-label="Task handover"]'));
const history = all('.board-history article', drawer).map((a) => txt(a).replace(/\d+[smhd] ago/, ' ')).slice(0, 20);
await click('Close', drawer);
await openAgent(P.worker);
const resumed = await live();
const again = P.say ? await say(P.say, 'SYNTHETIC REPLY from') : null;
await go('Board');
const boardAfter = await until(() => { const c = card(); const o = c && txt(c.querySelector('.board-owner')); return o === P.worker ? { column: txt(c.closest('.board-column').querySelector('h3')).replace(/\s*\d+$/, ''), owner: o } : null; }, 'the helper holds the task again', 30000).catch(() => ({ owner: txt(card().querySelector('.board-owner')) }));
return { rows, stoppedHead, notice, transcriptBefore, boardBefore, handover, history, resumed, again, boardAfter };
'''

PEER = r'''
const tasks = () => all('article.board-task').map((a) => txt(a.querySelector('.board-task-title')));
const agents = () => all('.agent-row').map((r) => txt(r).split(' ')[0]);
const tabs = () => all('nav.terminal-tabs button').map(txt);
const out = {};
for (const name of [P.b, P.a, P.b, P.a]) {
  await workspace(name);
  await go('Board'); await sleep(700);
  if (name === P.b && !tasks().includes(P.b_task)) {
    type(document.querySelector('input[aria-label="New task title"]'), P.b_task); await click('Add task');
    await until(() => tasks().includes(P.b_task), 'peer task created', 15000);
  }
  const step = { tasks: tasks() };
  await go('Agents'); await sleep(700); step.agents = agents();
  await go('Home'); await until(() => all('[aria-label="Checkout overview"]').length, 'checkout home', 15000); await sleep(500);
  step.home = txt(all('[aria-label="Checkout overview"]')[0]).slice(0, 600);
  await go('Terminals'); await sleep(700);
  if (!tabs().some((t) => !t.includes(' · '))) {
    await click(button('New terminal') ? 'New terminal' : 'Open a terminal');
    await until(() => tabs().length, 'terminal opened', 15000);
    const ta = await until(() => all('.terminal-pane textarea')[0], 'terminal input', 20000);
    await sleep(2500);
    const file = `${P.out}/${name.replace('/', '-')}`;
    ta.dispatchEvent(new InputEvent('input', { data: `env > '${file}.env'; pwd > '${file}.cwd'\n`, inputType: 'insertText', bubbles: true }));
    await sleep(1500);
  }
  step.terminals = tabs();
  await go('Review'); await sleep(700);
  step.review = all('p[data-generation]').map((p) => txt(p).slice(0, 24));
  (out[name] = out[name] || []).push(step);
}
await workspace('All Workspaces'); await go('Board'); await sleep(700);
out.all = tasks();
return out;
'''

PROBE = AGENT + r'''
await workspace(P.checkout);
await go('Agents'); await click('New agent');
const m = await until(modal, 'spawn dialog');
await fill('Name', P.name, m);
if (P.provider) {
  m.querySelector('button[aria-label="Choose model"]').click();
  const pop = await until(() => all('.model-picker-pop')[0], 'model list');
  type(pop.querySelector('input'), P.provider); await sleep(500);
  const item = await until(() => all(`[role="group"][aria-label="${P.provider} models"] button`, pop)[0], 'a model of that provider');
  item.click(); await sleep(300);
}
await fill('Mission Brief', 'PROBE ENVIRONMENT', m);
const picked = txt(m.querySelector('button[aria-label="Choose model"]'));
await click('Start Agent', m);
const started = await until(() => (!modal() ? 'started' : (all('.err', m).map(txt)[0] || null)), 'spawn result', 45000);
if (started !== 'started') return { started, picked };
const result = await until(() => { const t = convo(); const i = t.lastIndexOf('PROBE RESULT'); return i >= 0 ? t.slice(i, i + 3200) : null; }, 'probe result', 60000).catch((e) => String(e) + ' :: ' + convo().slice(-300));
return { started, picked, head: head(), result, problem: all('.shield.danger', document.querySelector('.agent-view')).map(txt).concat(toasts()).join(' | ') };
'''

REMOVE = AGENT + r'''
await workspace(P.checkout);
await openAgent(P.name);
document.querySelector('.agent-head button[title="Remove agent and clean up"]').click(); note('click remove');
// An empty list is the list reloading, not the agent gone.
const goneNow = () => { const rows = all('.agent-row'); return rows.length > 0 && !rows.some((r) => txt(r).startsWith(P.name)); };
const confirmBtn = await until(() => button('Delete anyway') || (goneNow() ? 'gone' : null), 'confirmation or removal', 30000);
let confirmed = false;
if (confirmBtn !== 'gone') { confirmed = true; confirmBtn.click(); }
const gone = await until(goneNow, 'agent removed', 40000).then(() => true, () => false);
await sleep(1500);
const seen = { rightAfter: all('.agent-row').length, page: txt(pane()).slice(0, 260), picker: txt(picker().selectedOptions[0]) };
await go('Board'); await go('Agents'); await sleep(1500);
return { confirmed, gone, toasts: toasts(), seen, rows: all('.agent-row').map((r) => txt(r).split(' ')[0]) };
'''

WORKDAY = r'''
await until(() => picker() && picker().options.length > 1, 'checkouts loaded', 15000); await sleep(1200);
const remembered = txt(picker().selectedOptions[0]);
await go('Workday');
const ledger = await until(() => all('[aria-label="Workday ledger"]')[0], 'ledger', 15000);
const actions = () => all('tr[data-action]', ledger).map((tr) => [tr.dataset.action, ...all('td', tr).slice(1, 4).map(txt)]);
const before = { completed: txt(all('section[aria-label="Completed actions"] h2', ledger)[0]), actions: actions() };
choose(document.querySelector('select[aria-label="Fallback tool"]'), 'Manual database edit');
type(document.querySelector('input[aria-label="Fallback note"]'), P.note);
await click('Record fallback');
const fallback = await until(() => all('p[data-fallback]', ledger).map(txt).find((t) => t.includes(P.note)), 'fallback listed', 15000);
await click('Export summary');
const exported = await until(() => { const t = all('p[role="status"]', pane()).map(txt).find((x) => x.startsWith('Summary written to')); return t || null; }, 'export path', 15000);
return { remembered, before, fallback, exported, window: txt(all('[aria-label="Observation window"]', ledger)[0]) };
'''

PICK = r'''
await until(() => picker() && picker().options.length > 1, 'checkouts loaded', 15000);
await workspace(P.checkout);
await go('Agents');
const rows = () => all('[role="listbox"][aria-label="Agents"] [data-agent]');
await until(() => rows().length >= 2, 'agents listed', 20000);
const before = rows().map((r) => r.dataset.agent);
const want = before.filter((n) => P.pick.includes(n));
for (const n of want) rows().find((r) => r.dataset.agent === n).dispatchEvent(new MouseEvent('click', { bubbles: true, metaKey: true }));
const bar = await until(() => all('[role="toolbar"][aria-label="Picked agents"]')[0], 'picked bar', 5000);
const picked = txt(bar);
window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Delete', bubbles: true }));
const m = await until(modal, 'confirm dialog', 5000);
const asked = txt(m);
await click('Remove', m);
// A helper that changed files it never committed asks once more, naming them.
let unsaved = '', seen = [];
const gone = await until(() => {
  const m2 = modal();
  if (m2 && txt(m2).includes('unsaved files') && !unsaved) { unsaved = txt(m2); click('Delete anyway', m2); }
  for (const t of toasts()) if (!seen.includes(t)) seen.push(t);
  return !rows().some((r) => want.includes(r.dataset.agent));
}, 'picked agents gone', 90000).catch((e) => String(e));
return { before, want, picked, asked, unsaved, gone, toasts: seen, dialog: modal() ? txt(modal()) : '', after: rows().map((r) => r.dataset.agent) };
'''

STORAGE = r'''
await go('Settings');
const sec = await until(() => all('section[aria-label="Storage"]')[0], 'storage section', 20000);
const parts = await until(() => { const r = all('tr[data-part]', sec).map((tr) => [tr.dataset.part, txt(all('td', tr)[0])]); return r.length ? r : null; }, 'storage parts', 30000);
await click('Clean up now', sec);
const last = await until(() => txt(all('[aria-label="Last clean up"]', sec)[0]) || null, 'last clean up', 60000);
return { parts, last, disk: txt(all('p', sec)[0]) };
'''

GATE = r'''
await workspace(P.checkout);
await go('Review');
const box = await until(() => all('details[aria-label="Release PR revisions"]')[0], 'release section', 15000);
if (!box.open) box.querySelector('summary').click();
type(box.querySelector('form input[type="url"]'), P.url);
await click('Verify release', box);
const panel = await until(() => all('section[aria-label="Checks on this pull request"]')[0], 'checks panel', 110000);
const head = () => txt(panel.querySelector('h3'));
const rows = () => all('tr[data-check]', panel);
await until(() => rows().length === P.checks, 'the gate lists its checks', 30000);
const start = all('button', panel).find((b) => ['Run checks', 'Run again'].includes(txt(b)));
start.click();
await until(() => head().startsWith('Checks are running'), 'checks started', 15000);
await until(() => !head().startsWith('Checks are running') && rows().every((r) => r.dataset.outcome !== 'pending'), 'checks finished', 150000);
const outcomes = Object.fromEntries(rows().map((r) => [r.dataset.check, r.dataset.outcome]));
const waiting = head();
type(await until(() => panel.querySelector(`input[aria-label="Issue that tracks ${P.preexisting}"]`), 'issue field'), '#12');
await click('Link issue', panel);
const after = await until(() => (head() !== waiting ? head() : null), 'the verdict moved on', 20000);
return { outcomes, waiting, after, tracked: txt(all(`tr[data-check="${P.preexisting}"]`, panel)[0]), progress: txt(all('pre[aria-label="Check progress"]', panel)[0]).slice(-300) };
'''

REVIEW = r'''
await workspace(P.checkout);
await go('Review');
const box = await until(() => all('details[aria-label="Release PR revisions"]')[0], 'release section', 15000);
if (!box.open) box.querySelector('summary').click();
const setup = await until(() => box.querySelector('details[aria-label="Release verification setup"]'), 'setup');
const pinned = () => txt(setup.querySelector('[aria-label="Pinned verifier"]'));
const access = () => txt(setup.querySelector('[aria-label="Origin read access"]'));
const summary = () => txt(box.querySelector('[aria-label="Verification setup summary"]'));
const status_ = () => txt(all('p[role="status"]', box)[0]);
const out = { summaryAtOpen: await until(() => (summary().includes('pinned at') ? summary() : null), 'saved setup shown', 6000).catch(() => summary()) };
if (P.pin) {
  if (!setup.open) setup.querySelector('summary').click();
  const sel = setup.querySelector('select[aria-label="Independent SQL driver"]');
  await until(() => Array.from(sel.options).some((o) => txt(o).startsWith(P.pin.driver)), 'verifier listed', 15000);
  choose(sel, P.pin.driver);
  if (P.pin.port) type(setup.querySelector('input[type="number"]'), String(P.pin.port));
  const proposal = await until(() => all('section[aria-label="Checks to confirm"]', setup)[0] || all('[role="alert"]', setup).map(txt)[0], 'checks shown before pinning', 20000);
  if (typeof proposal === 'string') return { refused: proposal, canPin: !all('button', setup).find((b) => txt(b) === 'Confirm and pin these checks').disabled };
  out.proposed = all('[data-check]', proposal).map(txt);
  out.proposedTargets = all('[data-target]', proposal).map((t) => txt(t.querySelector('p')));
  for (const [name, path] of Object.entries(P.pin.programs || {})) type(await until(() => proposal.querySelector(`input[aria-label="Path of ${name}"]`), `path field for ${name}`), path);
  await click('Confirm and pin these checks', setup);
  out.pinResult = await until(() => (pinned().includes(out.proposed.length ? 'You confirmed its checks' : 'pinned at') && !all('button', setup).find((b) => txt(b) === 'Confirm and pin these checks').disabled ? 'pinned' : (status_().startsWith('Checks confirmed') ? 'pinned' : (status_() && !status_().startsWith('Checks') ? status_() : null))), 'verifier pinned', 25000).catch((e) => String(e));
}
if (P.token) {
  if (!setup.open) setup.querySelector('summary').click();
  type(setup.querySelector('input[aria-label="GitHub read-only token"]'), P.token);
  await click('Save token', setup);
  await until(() => access().includes('A read-only token is saved'), 'token saved', 15000);
}
out.pinned = pinned(); out.access = access();
const status = () => txt(all('p[role="status"]', box)[0]);
const read = () => {
  const d = all('section[aria-label="Release decision"]', box)[0];
  const ids = {};
  if (d) { const dts = all('dt', d), dds = all('dd', d); dts.forEach((t, i) => { ids[txt(t)] = txt(dds[i]); }); }
  return {
    headline: d ? txt(d.querySelector('h3')) : null,
    first: d ? txt(d.querySelector('p')) : null,
    ids,
    reasons: d ? (all('ul[aria-label="Decision reasons"] li', d).map(txt).concat(all('p[aria-label="Decision reasons"]', d).map(txt))) : [],
    earlier: d ? txt(d.querySelector('[aria-label="Earlier revision"]')) : '',
    status: status(),
    cases: all('[data-case-state]', box).map((c) => [c.dataset.caseState, txt(c)]),
    live: all('[data-live-state]', box).map((c) => [c.dataset.liveState, txt(c)]),
    targets: all('[data-target-identity]', box).map(txt),
    confirmed: d ? txt(d.querySelector('[aria-label="Checks confirmation"]')) : '',
    landed: txt(all('section[aria-label="Landed pull requests"]', box)[0]),
    included: txt(all('details[aria-label="Included commits"] summary', box)[0]),
    history: all('p[data-generation]', box).map((p) => txt(p).slice(0, 26)),
    table: d ? all('table[aria-label="Release readiness"] tr', d).filter((tr) => tr.dataset.informs !== 'yes').map((tr) => [tr.dataset.row, tr.dataset.state]) : [],
    informing: d ? all('table[aria-label="Release readiness"] tr', d).filter((tr) => tr.dataset.informs === 'yes').map((tr) => [tr.dataset.row, tr.dataset.state, txt(tr.querySelector('td:last-child'))]) : [],
    summary: d ? (d.querySelector('details[aria-label="Summary to paste"] pre')?.textContent || '') : '',
  };
};
if (P.url) {
  type(box.querySelector('form input[type="url"]'), P.url);
  await click('Verify release', box);
  await until(() => !box.querySelector('form button').disabled && (all('section[aria-label="Release decision"]', box).length || status().startsWith('Not checked')), 'verification finished', 110000);
  await sleep(400);
  out.verified = read();
}
if (P.inspect) {
  const row = await until(() => all(`p[data-generation="${P.inspect}"]`, box)[0], `generation ${P.inspect}`, 10000);
  await click('Inspect local ancestry', row);
  await until(() => status().startsWith('Inspected') || status().startsWith('Revisions recorded'), 'inspected', 30000);
  await sleep(300);
  out.inspected = read();
}
if (!P.url && !P.inspect) out.idle = read();
return out;
'''


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('app'); ap.add_argument('--out'); ap.add_argument('--only', default='')
    ap.add_argument('--keep', action='store_true', help='keep the scratch folder for inspection')
    ap.add_argument('--public', nargs=5, metavar=('CHECKOUT', 'PR_URL', 'HEAD', 'LANDED', 'NESTED'),
                    help='also verify one public pull request against the real origin: a local clone that holds its history, its URL, and the expected head, landed and nested counts. Reads public GitHub metadata only.')
    args = ap.parse_args()
    scratch = Path(tempfile.mkdtemp(prefix='philium-acceptance-', dir=os.environ.get('PHILIUM_SCRATCH_DIR') or None)).resolve()
    run = Run(args.app, scratch, args.out)
    only = set(filter(None, args.only.split(',')))
    want = lambda name: not only or name in only
    model = syn.serve(syn.Model); syn.Model.expect_key = KEY
    model2 = syn.serve(syn.SecondModel)
    try:
        # Checkouts: two with the same folder name, a release fixture, and separate verifiers.
        alpha = syn.new_repo(scratch / 'alpha/same', 'philium-fixture/alpha'); graph = syn.release_graph(alpha)
        beta = syn.new_repo(scratch / 'beta/same', 'philium-fixture/beta'); syn.write(beta, {'README.md': 'beta\n'}); syn.commit_all(beta)
        ver_a = syn.new_repo(scratch / 'verifier-alpha'); syn.write(ver_a, {'philium/sql-contracts.json': syn.CONTRACTS}); pin_a = syn.commit_all(ver_a)
        gamma = syn.new_repo(scratch / 'gamma', 'philium-fixture/gamma')
        syn.write(gamma, {'view.json': '{"column":"name"}'}); g_base = syn.commit_all(gamma, 'base')
        syn.write(gamma, {'view.json': '{"column":"status"}'}); g_head = syn.commit_all(gamma, 'show status')
        # Like a clone someone works in: its default branch is known only from the origin, and
        # another branch is checked out. Agents must still start here.
        syn.git(gamma, 'update-ref', 'refs/remotes/origin/main', g_base); syn.git(gamma, 'symbolic-ref', 'refs/remotes/origin/HEAD', 'refs/remotes/origin/main')
        syn.git(gamma, 'checkout', '-q', '-b', 'dev'); syn.git(gamma, 'branch', '-D', 'main')
        ver_g = syn.new_repo(scratch / 'verifier-gamma')
        syn.write(ver_g, {'philium/sql-contracts.json': {'release_records': True, 'cases': [{'id': 'source', 'document': 'view.json', 'pointer': '/column', 'expect': 'status'}], 'live_cases': [{'id': 'view', 'path': '/view', 'pointer': '/column', 'expect': 'status'}]}})
        pin_g = syn.commit_all(ver_g)
        delta = syn.new_repo(scratch / 'delta', 'philium-fixture/delta')
        syn.write(delta, {'view.json': '{"column":"name"}'}); d_base = syn.commit_all(delta, 'base')
        syn.write(delta, {'view.json': '{"column":"status"}'}); d_head = syn.commit_all(delta, 'show status')
        ver_d = syn.new_repo(scratch / 'verifier-delta'); syn.write(ver_d, {'philium/sql-contracts.json': syn.target_contract()}); syn.commit_all(ver_d)
        epsilon = syn.new_repo(scratch / 'epsilon', 'philium-fixture/epsilon')
        syn.write(epsilon, {'view.json': '{"column":"name"}'}); e_base = syn.commit_all(epsilon, 'base')
        syn.write(epsilon, {'view.json': '{"column":"status"}'}); e_head = syn.commit_all(epsilon, 'show status')
        bin_dir = scratch / 'bin'; bin_dir.mkdir()
        for name, body in [('mycli', syn.MYCLI_STANDIN), ('kubectl', syn.KUBECTL_STANDIN)]:
            (bin_dir / name).write_text(body); (bin_dir / name).chmod(0o755)
        url = f'http://127.0.0.1:{model.server_port}/v1'

        repos = [str(alpha), str(beta), str(ver_a), str(gamma), str(ver_g), str(delta), str(ver_d), str(epsilon)] + ([str(Path(args.public[0]).resolve())] if args.public else [])
        r = run.ui(SETUP, {'repos': repos, 'model_url': url, 'model_url2': f'http://127.0.0.1:{model2.server_port}/v1', 'key': KEY}, 240)
        v = run.value(r)
        labels = ['All Workspaces', 'alpha/same', 'beta/same', 'gamma', 'verifier-alpha', 'verifier-gamma', 'delta', 'verifier-delta', 'epsilon'] + ([Path(args.public[0]).resolve().name] if args.public else [])
        run.check('B6', 'Two same-named checkouts register from the Repos screen with distinct labels', sorted(v.get('labels') or []) == sorted(labels), v.get('labels') or r.get('error'))
        run.check('B1', 'A provider with an API key can be saved', v.get('keyed') == 'saved', v.get('keyed') or r.get('error'))
        vault = run.profile / 'data/secrets/vault.json'
        mode = stat.S_IMODE(vault.stat().st_mode) if vault.exists() else None
        db = (run.profile / 'data/philium.db').read_bytes() if (run.profile / 'data/philium.db').exists() else b''
        run.check('B1', 'The key is in the profile vault with owner-only access and not in the database', mode == 0o600 and KEY.encode() in vault.read_bytes() and KEY.encode() not in db, f'mode={oct(mode) if mode else None}')
        syn.Model.probe = {'home': str(run.home), 'vault': str(vault)}

        if args.public and want('public'):
            # The real origin, read without credentials. No stand-in is configured for this phase.
            checkout, pr, head, n_landed, n_nested = args.public
            d = (run.value(run.ui(REVIEW, {'checkout': Path(checkout).resolve().name, 'url': pr}, 260)).get('verified')) or {}
            landed = d.get('landed') or ''
            found = (landed.count(' landed at '), landed.count(' contains #'))
            run.check('C2', 'A public parent pull request, read from the real origin by the form: exact head, landed and nested pull requests', (d.get('ids') or {}).get('Head') == head and found == (int(n_landed), int(n_nested)) and ' 0 missing lookups' in landed and '0 ambiguous' in landed, {'head': (d.get('ids') or {}).get('Head'), 'base': (d.get('ids') or {}).get('Target (base)'), 'landed': found[0], 'nested': found[1], 'tail': landed[-70:], 'status': d.get('status')})
            run.check('C7', 'With no verifier pinned, a public pull request is resolved but never called ready', d.get('headline') == 'Release verification incomplete' and 'No checks are set up for this checkout' in ' '.join(d.get('reasons') or []), {'headline': d.get('headline'), 'why': ' | '.join(d.get('reasons') or [])[:200]})

        if want('review') or want('connect'):
            # Before any stand-in origin is switched on. The throwaway home has no GitHub CLI
            # sign-in, so the honest outcome is a plain refusal that says what to do.
            r = run.ui(CONNECT, {'checkout': 'gamma'}, 120)
            v = run.value(r)
            run.check('C1', 'Connect GitHub uses the GitHub CLI sign-in of the person\'s own home, and without one it says so and stays disconnected', 'not connected' in (v.get('before') or '') and (v.get('said') or '').startswith('GitHub not connected') and ('gh auth login' in (v.get('said') or '')) and 'not connected' in (v.get('after') or ''), v or r.get('error'))

        if want('coord'):
            r = run.ui(COORDINATE, {'checkout': 'alpha/same', 'lead': 'lead', 'worker': 'wren', 'task': 'Add status column'}, 220)
            v = run.value(r)
            run.check('B1', 'The checkout home offers one coordinator entry, preset to this checkout', 'Start coordinator' in (v.get('entry') or '') and (v.get('preset') or {}).get('repo') == 'alpha/same' and 'Orchestrator' in (v.get('preset') or {}).get('role', ''), v.get('preset') or v.get('entry') or r.get('error'))
            run.check('B1', 'A typed message reaches the coordinator and its reply is shown', 'SYNTHETIC REPLY from lead (orchestrator)' in (v.get('hello') or ''), v.get('hello') or v.get('composer') or r.get('error'))
            keyed = [e for e in syn.Model.seen if e['agent'] == 'lead']
            run.check('B1', 'Only the gateway carried the saved key to the model endpoint', bool(keyed) and all(e['key_ok'] for e in keyed), f'{len(keyed)} requests')
            run.check('B2', 'The coordinator created the task and handed it to a helper', (v.get('delegated') or '').startswith('DELEGATED "Add status column" to wren'), v.get('delegated') or r.get('error'))
            run.check('B2', 'The helper is listed under the coordinator', any(p.startswith('wren') for p in v.get('pills') or []), v.get('pills'))
            run.check('B2', "The helper's report arrives in the coordinator's conversation without anyone asking", 'From wren: Task' in (v.get('report') or '') and 'is submitted' in (v.get('report') or ''), v.get('report'))
            # The app quit normally at the end of that scenario. Everything below is after a reopen.
            r = run.ui(BOARD, {'checkout': 'alpha/same', 'task': 'Add status column', 'open_agent': True, 'wait_status': 'In review', 'note': 'please also keep the notes file'})
            v = run.value(r)
            hist = ' | '.join(v.get('history') or [])
            run.check('B2', 'The board names the helper as owner of the task', v.get('owner') == 'wren' and 'wren' in (v.get('ownerLine') or ''), {'owner': v.get('owner'), 'column': v.get('column')} if v else r.get('error'))
            run.check('B2', 'Task history says who handed it over, who took it and that it was submitted, by name', all(x in hist for x in ['lead · assigned', 'wren · claimed', 'wren · submitted']), hist or r.get('error'))
            run.check('B3', 'From the task, the helper opens with its lead named and its transcript readable', 'lead: lead' in (v.get('agentHead') or '') and 'WORKER RESULT' in (v.get('transcript') or '') and 'board_submit' in (v.get('transcript') or ''), (v.get('agentHead') or '') + ' :: ' + (v.get('transcript') or '')[-160:])
            run.check('B3', 'Changes show the committed file and the new file that was not added', 'status.txt' in (v.get('changes') or '') and 'notes.txt' in (v.get('changes') or ''), (v.get('changes') or '')[:260])

            n0 = len(syn.Model.seen)
            r = run.ui(REDIRECT, {'checkout': 'alpha/same', 'worker': 'wren'}, 240)
            v = run.value(r)
            seen = [e for e in syn.Model.seen[n0:] if e['agent'] == 'wren']
            slow = [e for e in seen if str(e['plan']).startswith('slow')]
            run.check('B5', 'After a normal quit the stopped helper resumes from its header', 'Stopped' in (v.get('before') or '') and v.get('resumed') == 'resumed', v.get('before') or r.get('error'))
            run.check('B4', 'A note sent from the task to its stopped owner is delivered when the owner resumes', 'please also keep the notes file' in (v.get('note') or ''), v.get('note'))
            run.check('B4', 'A redirect typed to the running helper is carried out', (v.get('redirected') or '').startswith('REDIRECTED: the file is now state.txt'), v.get('redirected') or r.get('error'))
            run.check('B4', 'Interrupting a turn stops that turn and keeps the helper usable', 'Idle' in (v.get('interrupted') or '') and 'SYNTHETIC REPLY from wren' in (v.get('after') or ''), {'interrupted': v.get('interrupted'), 'after': (v.get('after') or '')[:60]})
            run.check('B4', 'Stop ends the helper mid-turn and disables its composer', 'Stopped' in (v.get('stopped') or '') and v.get('disabled') is True, v.get('stopped'))
            time.sleep(2)
            late = [e for e in syn.Model.seen[n0:] if e['agent'] == 'wren' and e['t'] > (slow[-1]['t'] if slow else 0)]
            run.check('B4', 'Both interrupted streams were closed by the product and no request followed the Stop', len(slow) == 2 and all(e.get('ended') == 'client closed' for e in slow) and not late, {'slow': [e.get('ended') for e in slow], 'after_stop': len(late)})
            run.check('B3', 'Changes follow the redirect', 'state.txt' in (v.get('changes') or ''), (v.get('changes') or '')[:200])

            # Interruption: the process is killed while a second helper is in the middle of a turn.
            mid = lambda: any(e['agent'] == 'ibis' and str(e['plan']).startswith('slow') and time.time() - e['t'] > 2 for e in syn.Model.seen)
            r = run.ui(DELEGATE_SLOW, {'checkout': 'alpha/same', 'lead': 'lead', 'worker': 'ibis', 'task': 'Long migration', 'secs': 60}, 200, kill_when=mid)
            run.check('B5', 'The app was killed while a helper was mid-turn', r.get('exit') == -9 and mid(), f"exit {r.get('exit')}")
            n1 = len(syn.Model.seen)
            r = run.ui(AFTER_RESTART, {'checkout': 'alpha/same', 'worker': 'ibis', 'task': 'Long migration', 'agents': 3, 'say': 'Where were we?'}, 240)
            v = run.value(r)
            run.check('B5', 'After the crash the same three agents exist, none duplicated', sorted(v.get('rows') or []) == ['ibis', 'lead', 'wren'], v.get('rows') or r.get('error'))
            run.check('B5', 'The interrupted helper is shown stopped with a plain reason', 'Stopped' in (v.get('stoppedHead') or '') and 'Press Resume' in (v.get('notice') or ''), v.get('notice'))
            b = v.get('boardBefore') or {}
            run.check('B5', 'The board still says whose task it is and that the helper is stopped', b.get('owner') == 'Handed to ibis · stopped' and b.get('column') == 'Open' and b.get('tasks') == 2, b)
            run.check('B5', 'The task explains how to continue and its history records the interruption', 'Resume the agent' in (v.get('handover') or '') and any('agent_interrupted' in h for h in v.get('history') or []), {'handover': v.get('handover'), 'history': (v.get('history') or [])[-3:]})
            again = [e for e in syn.Model.seen[n1:] if e['agent'] == 'ibis']
            run.check('B5', 'Resume continues the same conversation: the earlier brief is still in the session', v.get('resumed') == 'resumed' and 'SYNTHETIC REPLY from ibis' in (v.get('again') or '') and any('WORKER TASK' in e['history'] and e['turns'] >= 2 for e in again), [e['history'][:80] for e in again][:2])
            run.check('B5', 'After Resume the helper holds its task again without being asked', (v.get('boardAfter') or {}).get('owner') == 'ibis' and (v.get('boardAfter') or {}).get('column') == 'Claimed', v.get('boardAfter'))
            # Second reopen, after a normal quit.
            r = run.ui(AFTER_RESTART, {'checkout': 'alpha/same', 'worker': 'ibis', 'task': 'Long migration', 'agents': 3}, 200)
            v = run.value(r)
            run.check('B5', 'A second reopen shows the same agents, the same two tasks and the same handover', sorted(v.get('rows') or []) == ['ibis', 'lead', 'wren'] and (v.get('boardBefore') or {}).get('owner') == 'Handed to ibis · stopped' and (v.get('boardBefore') or {}).get('tasks') == 2 and (v.get('boardAfter') or {}).get('owner') == 'ibis', {'rows': v.get('rows'), 'before': v.get('boardBefore'), 'after': v.get('boardAfter')} if v else r.get('error'))

            # The model picker, opened by typing /model to the coordinator.
            r = run.ui(MODEL_PICKER, {'name': 'solo', 'other': 'Second'}, 200)
            v = run.value(r)
            first, other, every, found = v.get('first') or {}, v.get('other') or {}, v.get('everything') or {}, v.get('searched') or {}
            run.check('B1', 'Typing /model opens the model list with the search ready, instead of printing a line', v.get('opened') is True and first.get('searchFocused') is True, {'opened': v.get('opened'), 'focus': first.get('searchFocused'), 'started': v.get('started'), 'toasts': v.get('toasts')} if v else r.get('error'))
            run.check('B1', 'The list has one switch per provider this agent may use and leaves out the untrusted one', first.get('chips') == ['Synthetic (on)', 'Second'] and 'hidden' in (first.get('foot') or ''), {'chips': first.get('chips'), 'foot': first.get('foot')})
            run.check('B1', 'A provider shows the newest model of each family first and keeps the older ones behind one button', other.get('items') == ['nova-3-2', 'nova-mini-3', 'spark-2-1'] and other.get('more') == 'Show 5 older models' and len(every.get('items') or []) == 8, {'latest': other.get('items'), 'more': other.get('more'), 'all': every.get('items')})
            run.check('B1', 'Search looks across every provider at once', [i.replace(' ✓', '') for i in found.get('items') or []] == ['synthetic-1'] and found.get('chips') == 0, found)
            run.check('B1', 'An arrow key reaches the first model; choosing it switches the agent, returns to the message box, and the agent answers on the new model', (v.get('focused') or '').startswith('nova-3-2') and v.get('closed') is True and 'nova-3-2' in (v.get('modelAfter') or '') and v.get('composerFocused') is True and 'SYNTHETIC REPLY from solo' in (v.get('reply') or ''), {'focused': v.get('focused'), 'after': v.get('modelAfter'), 'composer': v.get('composerFocused'), 'reply': (v.get('reply') or '')[:60], 'toasts': v.get('toasts')})

            r = run.ui(QUEUE, {'name': 'solo'}, 150)
            v = run.value(r)
            w, e = v.get('whileWorking') or {}, v.get('edited') or {}
            run.check('B1', 'A message typed while the agent works is queued above the box with Edit and Remove, not refused', w.get('queued') and len(w['queued']) == 2 and 'Queued · sent when solo finishes this step' in (w.get('label') or '') and 'Enter queues your message' in (w.get('status') or ''), w or r.get('error'))
            run.check('B1', 'Edit takes a queued message back into the box', e.get('queued') and len(e['queued']) == 1 and 'QUEUED second note' in (e.get('box') or ''), e)
            run.check('B1', 'When the step ends the queued messages reach the agent together, in order, and the queue empties', 'QUEUED first note QUEUED second note' in (v.get('finished') or '') and v.get('left') == [], {'finished': v.get('finished'), 'left': v.get('left')})

            r = run.ui(ERRORS, {'name': 'solo'}, 320)
            v = run.value(r)
            run.check('B1', 'A provider refusal reads as one sentence with the fix, and the original is kept under Error details', v.get('shown') is True and 'limiting requests right now' in (v.get('plain') or '') and 'switch model with /model' in (v.get('plain') or '') and ('429' in (v.get('details') or '') or 'rate_limit' in (v.get('details') or '')), v or r.get('error'))
            run.check('B1', 'An agent stopped by a rate limit says when it continues, then continues on its own', 'continues at' in (v.get('waiting') or '') and 'stopped by the model provider' in (v.get('continued') or '') and not v.get('after'), {k: v.get(k) for k in ('waiting', 'continued', 'after')})

        if want('review'):
            origin = syn.serve(syn.Origin); observer = syn.serve(syn.Observer)
            run.env['PHILIUM_FIXTURE_ORIGIN_PORT'] = str(origin.server_port)
            A, G = 'philium-fixture/alpha', 'philium-fixture/gamma'
            g = graph
            syn.Origin.repos = {
                A: {'prs': {40: {'base': g['target'], 'heads': [g['head']]}}, 'landed': {g['f31']: (31, True), g['m30']: (30, True), g['b1']: (32, True), g['head']: (40, False), g['fixed']: (40, False)}},
                G: {'prs': {7: {'base': g_base, 'heads': [g_head]}}, 'landed': {g_head: (7, False)}},
            }
            pr_a, pr_g = f'https://github.com/{A}/pull/40', f'https://github.com/{G}/pull/7'
            verify = lambda checkout, u, **extra: run.value(run.ui(REVIEW, {'checkout': checkout, 'url': u, **extra}, 200))
            why = lambda d: ' | '.join((d or {}).get('reasons') or [])

            # ---- one release, four causes --------------------------------------------------
            v = verify('alpha/same', pr_a, pin={'driver': 'verifier-alpha'}, token='synthetic-read-token-0001')
            d = v.get('verified') or {}
            run.check('C1', 'The verifier is pinned once from the Review form', pin_a in (v.get('pinned') or '') and 'Deployment observer: none' in (v.get('pinned') or ''), v.get('pinned') or v)
            sent = [x for x in syn.Origin.auth if x]
            run.check('C1', 'A read-only origin token saved in the form is kept in the vault and sent only as a bearer to the origin', 'A read-only token is saved' in (v.get('access') or '') and bool(sent) and set(sent) == {'Bearer synthetic-read-token-0001'} and b'synthetic-read-token-0001' in vault.read_bytes() and b'synthetic-read-token-0001' not in (run.profile / 'data/philium.db').read_bytes(), {'access': v.get('access'), 'requests_with_token': len(sent)})
            ids = d.get('ids') or {}
            first_gen = int(ids.get('Generation') or 0)
            run.check('C2', 'The decision shows the exact generation, target and head, and says no artifact is recorded', first_gen > 0 and ids.get('Target (base)') == g['target'] and ids.get('Head') == g['head'] and 'None for this exact head' in ids.get('Build record', ''), ids)
            landed = d.get('landed') or ''
            run.check('C2', 'Landed pull requests and the nested one are listed with their exact commits', all(x in landed for x in [f"#30 landed at {g['m30']}", f"#31 landed at {g['f31']}", f"#32 landed at {g['b1']}", '#30 contains #31', '0 missing lookups, 3 unmapped commits, 0 ambiguous']) and '6 included commits' in (d.get('included') or ''), landed[:400])
            cases = dict((c[1].split(':')[0], c) for c in d.get('cases') or [])
            states = {k: c[0] for k, c in cases.items()}
            run.check('C3', 'Four contracts, four different causes', states == {'child-change': 'release_range_regression', 'merge-result': 'composed_head_regression', 'old-defect': 'same_failure_on_target', 'never-observed': 'incomplete'}, states)
            run.check('C3', 'The child failure names the landed changes that fail on their own; the merge-only regression names none', 'fails the same way on its own (child failure)' in cases.get('child-change', ['', ''])[1] and '#31' in cases.get('child-change', ['', ''])[1] and 'child failure' not in cases.get('merge-result', ['', ''])[1] and 'Merge-only regression' in cases.get('merge-result', ['', ''])[1], [c[1][:150] for c in cases.values()][:2])
            w = why(d)
            run.check('C3', 'The decision is Do not ship and gives one plain reason per cause', d.get('headline') == 'Do not ship' and all(x in w for x in ['child-change: regression inside this release', 'fails the same way on its own', 'merge-result: merge-only regression', 'old-defect: already failing on the target', 'never-observed: missing evidence']) and all(n in w.split(' | ')[0] for n in ['#30', '#31']) and '#3' not in w.split(' | ')[1], w[:500] or d.get('status'))
            run.check('C3', 'The decision is laid out as a table: the failing checks row is red, and the pasteable summary says so without calling anything unknown a pass', dict(d.get('table') or []).get('checks') == 'bad' and dict(d.get('table') or []).get('candidate') == 'ok' and '| FAIL | Checks on this commit' in (d.get('summary') or '') and 'Unknown is not a pass' in (d.get('summary') or ''), {'table': d.get('table'), 'summary': (d.get('summary') or '')[:160]})
            run.check('C3', 'Beside the verdict the table says how many landed pull requests a pinned check reads, as information only', [x[0] for x in d.get('informing') or []] == ['coverage'] and 'landed pull request' in (d.get('informing') or [['', '', '']])[0][2] and dict(d.get('table') or []).get('merge') == 'ok', d.get('informing'))

            # ---- reopen: setup persists; a new head starts without a verdict ------------------
            syn.Origin.repos[A]['prs'][40]['heads'] = [g['fixed']]
            v = verify('alpha/same', pr_a, inspect=first_gen)
            d, old = v.get('verified') or {}, v.get('inspected') or {}
            run.check('C1', 'After a reopen the form says it is set up, names the pinned verifier, and the URL verifies without touching setup', pin_a[:12] in (v.get('summaryAtOpen') or '') and 'verifier-alpha' in (v.get('summaryAtOpen') or '') and d.get('headline') is not None, v.get('summaryAtOpen'))
            run.check('C4', 'A new head gets a new generation and its own result, not the old Do not ship', int((d.get('ids') or {}).get('Generation') or 0) > first_gen and (d.get('ids') or {}).get('Head') == g['fixed'] and d.get('headline') == 'Release verification incomplete' and 'regression' not in why(d), {'headline': d.get('headline'), 'ids': d.get('ids')})
            run.check('C4', 'The old verdict stays on the old head and is labelled as an earlier revision', old.get('headline') == 'Do not ship' and (old.get('ids') or {}).get('Head') == g['head'] and 'Saved result for an earlier revision' in (old.get('earlier') or ''), {'headline': old.get('headline'), 'earlier': old.get('earlier')})

            # ---- degraded origin ---------------------------------------------------------------
            later = syn.tree_commit(alpha, {'README.md': 'later\n'}, [g['fixed']], 'later'); syn.git(alpha, 'branch', 'fixture-later', later)
            syn.Origin.repos[A]['prs'][40]['heads'] = [g['fixed'], later]; syn.Origin.repos[A]['landed'][later] = (40, False)
            syn.Origin.mode, syn.Origin.calls = 'move', []
            d = verify('alpha/same', pr_a).get('verified') or {}
            run.check('C6', 'Moved head: the operator is told to verify again and sees the new head', d.get('headline') == 'Head or target changed. Verify again.' and 'moved during verification' in why(d) and later in why(d) and (d.get('ids') or {}).get('Origin') == 'Moved during verification', {'headline': d.get('headline'), 'why': why(d)[:200]})
            syn.Origin.mode = 'down'
            d = verify('alpha/same', pr_a).get('verified') or {}
            run.check('C6', 'Origin unavailable: nothing is checked and the form says why', d.get('headline') is None and (d.get('status') or '').startswith('Not checked: Origin lookup failed'), d.get('status') or d)
            syn.Origin.repos[A]['prs'][40]['heads'] = [g['fixed']]; syn.Origin.mode = 'members-down'
            d = verify('alpha/same', pr_a).get('verified') or {}
            run.check('C6', 'Membership lookup unavailable: incomplete, with the number of commits not looked up', d.get('headline') == 'Release verification incomplete' and 'of the included commits came from' in why(d) and 'missing lookups' in (d.get('landed') or '') and not (d.get('landed') or '').startswith('Landed pull requests 0 missing'), why(d)[:260])
            syn.Origin.mode = 'ok'

            # ---- a fully reviewed synthetic change: positive path and its failure modes -------
            blob = syn.git(gamma, 'show', f'{g_head}:view.json')
            artifact = 'sha256:' + syn.sha256(blob)
            syn.Observer.state = {'revision': g_head, 'artifact': artifact, 'paths': {'/view': json.loads(blob)}, 'mode': 'ok', 'wrong_revision': g_base, 'fail_body': {'column': 'name'}}
            v = verify('gamma', pr_g, pin={'driver': 'verifier-gamma', 'port': observer.server_port})
            d = v.get('verified') or {}
            run.check('C6', 'No release record for this head: incomplete, and the deployment cannot be matched', f'loopback port {observer.server_port}' in (v.get('pinned') or '') and d.get('headline') == 'Release verification incomplete' and 'No independent release record exists for this exact base, head and verifier pin' in why(d) and 'No independent artifact record exists for this exact head' in why(d), why(d)[:300] or v)
            record = {'origin': G, 'pr': 7, 'base': g_base, 'head': g_head, 'driver_pin': pin_g, 'artifact': artifact}
            rec_path = f'philium/releases/{g_head}.json'
            syn.write(ver_g, {rec_path: record}); syn.commit_all(ver_g, 'record')
            n_obs = len(syn.Observer.calls)
            d = verify('gamma', pr_g).get('verified') or {}
            run.check('C7', 'A passing subset is not a ship recommendation', 'Contracts: scoped pass' in (d.get('first') or '') and d.get('headline') == 'Release verification incomplete' and 'Review record: code review is not recorded as complete' in why(d) and 'Evidence not accepted for source (not recorded); view (not recorded)' in why(d), {'first': (d.get('first') or '')[-70:], 'why': why(d)[:260]})
            calls = [c[1] for c in syn.Observer.calls[n_obs:]]
            run.check('C5', 'The running product was observed twice, with the deployment identity checked before and after', calls.count('/view') == 2 and calls.count('/.well-known/philium/deployment') == 2 and calls[0].endswith('deployment') and calls[-1].endswith('deployment') and (d.get('ids') or {}).get('Build record') == artifact and 'before and after the checks' in (d.get('ids') or {}).get('Deployment', ''), {'calls': calls, 'deployment': (d.get('ids') or {}).get('Deployment')})
            # The harness is the independent reviewer of this synthetic change: it confirms the whole
            # change is the one data file it made, and only then records evidence and the attestation.
            whole = syn.git(gamma, 'diff', '--name-status', g_base, g_head) == 'M\tview.json' and blob == '{"column":"status"}' and syn.git(gamma, 'ls-tree', g_head).startswith('100644 blob ')
            run.check('C5', 'Independent review of the synthetic change: one regular data file changes, nothing else', whole, syn.git(gamma, 'diff', '--name-status', g_base, g_head))
            evidence = {}
            for cid, obs in [('source', 'The only change is the tracked data file view.json, column name to status. The committed value at this exact head is status.'), ('view', 'The loopback service returned column status twice for /view, with this exact revision and artifact before and after.')]:
                body = json.dumps({'case': cid, 'head': g_head, 'status': 'pass', 'observation': obs})
                syn.write(ver_g, {f'philium/evidence/{cid}.json': body}); evidence[cid] = body
                record.setdefault('evidence', {})[cid] = {'path': f'philium/evidence/{cid}.json', 'sha256': syn.sha256(body)}
            record.update({'all_changes_accounted_for': True, 'code_review_complete': True, 'cleanup_verified': True, 'unresolved_findings': [], 'required_cases': ['source', 'view']})
            if whole:
                syn.write(ver_g, {rec_path: record}); syn.commit_all(ver_g, 'reviewed')
            v = verify('gamma', pr_g)
            d = v.get('verified') or {}
            ready_gen = (d.get('ids') or {}).get('Generation')
            run.check('C5', 'Reviewed scope with independent evidence: Ready for the reviewed scope, tied to head and artifact', d.get('headline') == 'Ready for the reviewed scope' and (d.get('ids') or {}).get('Head') == g_head and (d.get('ids') or {}).get('Build record') == artifact and 'Nothing stands in the way' in why(d) and 'unchanged' in (d.get('ids') or {}).get('Origin', ''), {'headline': d.get('headline'), 'ids': d.get('ids'), 'why': why(d)[:200]})
            run.check('C5', 'When the verdict is Ready every row of the table is green', [st for _, st in d.get('table') or []] == ['ok'] * 8, d.get('table'))
            syn.write(ver_g, {'philium/evidence/view.json': evidence['view'].replace('twice', 'once')}); syn.commit_all(ver_g, 'altered')
            d = verify('gamma', pr_g).get('verified') or {}
            run.check('C6', 'Altered evidence: not ready, and the altered file is named', d.get('headline') == 'Release verification incomplete' and 'view (altered: the file no longer matches its recorded hash)' in why(d), why(d)[:260])
            run.check('C6', 'Altered evidence turns the record row red; nothing else changes colour', dict(d.get('table') or []).get('record') == 'bad' and dict(d.get('table') or []).get('checks') == 'ok', d.get('table'))
            (ver_g / 'philium/evidence/view.json').unlink(); syn.commit_all(ver_g, 'removed')
            d = verify('gamma', pr_g).get('verified') or {}
            run.check('C6', 'Missing evidence: not ready, and the missing file is named', d.get('headline') == 'Release verification incomplete' and 'view (missing: the recorded file is not in the verifier)' in why(d), why(d)[:260])
            syn.write(ver_g, {'philium/evidence/view.json': evidence['view']}); syn.commit_all(ver_g, 'restored')
            syn.Observer.state['mode'] = 'wrong'; n_obs = len(syn.Observer.calls)
            d = verify('gamma', pr_g).get('verified') or {}
            run.check('C6', 'Wrong deployment: not ready, named as such, and the product is not sampled', d.get('headline') == 'Release verification incomplete' and 'Wrong deployment before the checks' in why(d) and not any(c[1] == '/view' for c in syn.Observer.calls[n_obs:]), why(d)[:260])
            run.check('C6', 'A wrong deployment turns the running-product row red and its checks are not counted', dict(d.get('table') or []).get('running') == 'bad' and dict(d.get('table') or []).get('smoke') != 'ok', d.get('table'))
            syn.Observer.state['mode'] = 'down'
            d = verify('gamma', pr_g).get('verified') or {}
            run.check('C6', 'Absent deployment: not ready, and the observer is named as not answering', d.get('headline') == 'Release verification incomplete' and 'Deployment unavailable before the checks' in why(d), why(d)[:260])
            syn.Observer.state['mode'] = 'fail'
            d = verify('gamma', pr_g).get('verified') or {}
            run.check('C6', 'The matching deployment returns the wrong value: Do not ship', d.get('headline') == 'Do not ship' and 'view: the running product returned a different value than the contract expects' in why(d), why(d)[:260])
            syn.Observer.state['mode'] = 'ok'
            d = verify('gamma', pr_g).get('verified') or {}
            run.check('C5', 'With evidence restored and the deployment right again, the same head is ready again', d.get('headline') == 'Ready for the reviewed scope', d.get('headline'))
            syn.write(gamma, {'view.json': '{"column":"later"}'}); g_next = syn.commit_all(gamma, 'next')
            syn.Origin.repos[G]['prs'][7]['heads'] = [g_next]; syn.Origin.repos[G]['landed'][g_next] = (7, False)
            v = verify('gamma', pr_g, inspect=int(ready_gen or 1))
            d, old = v.get('verified') or {}, v.get('inspected') or {}
            run.check('C4', 'A new head after a Ready verdict starts incomplete: the old record does not carry over', (d.get('ids') or {}).get('Head') == g_next and d.get('headline') == 'Release verification incomplete' and "An earlier head's record does not carry over" in why(d) and old.get('headline') == 'Ready for the reviewed scope' and 'Saved result for an earlier revision' in (old.get('earlier') or ''), {'new': d.get('headline'), 'why': why(d)[:160], 'old': old.get('headline')})
            # ---- the repo's own checks on a pull request, from Review ---------------------------
            E = 'philium-fixture/epsilon'
            syn.Origin.repos[E] = {'prs': {11: {'base': e_base, 'heads': [e_head]}}, 'landed': {e_head: (11, False)}}
            gate_file = run.profile / 'data/pipelines/philium-fixture-epsilon/.philium/pipeline.yaml'
            gate_file.parent.mkdir(parents=True, exist_ok=True)
            plat = 'darwin/arm64' if os.uname().machine == 'arm64' else 'darwin/amd64'
            gate_file.write_text(f'version: 1\nbranch: main\nplatforms: [{plat}]\ntiers:\n  fast:\n    - {{ id: view-has-status, run: "grep -q status view.json" }}\n    - {{ id: notes-present, run: "test -e NOTES.md" }}\n')
            r = run.ui(GATE, {'checkout': 'epsilon', 'url': f'https://github.com/{E}/pull/11', 'checks': 2, 'preexisting': 'notes-present'}, 260)
            v = run.value(r)
            run.check('C9', 'Review runs the repo\'s own checks on the pull request: a check its change fixes passes, one that fails on the base too is told apart', (v.get('outcomes') or {}) == {'view-has-status': 'pass', 'notes-present': 'pre_existing'}, v.get('outcomes') or v.get('progress') or r.get('error'))
            run.check('C9', 'A failure the base has too waits for a person, and linking the issue that tracks it lets the verdict move on without running again', (v.get('waiting') or '').startswith('Waiting for you: 1 decision') and (v.get('after') or '').startswith('Checks pass') and 'Tracked in #12' in (v.get('tracked') or ''), {'waiting': v.get('waiting'), 'after': v.get('after'), 'tracked': v.get('tracked')})
            # ---- command targets: a hosted environment through its own CLI, a local cluster through kubectl
            D = 'philium-fixture/delta'
            syn.Origin.repos[D] = {'prs': {9: {'base': d_base, 'heads': [d_head]}}, 'landed': {d_head: (9, False)}}
            pr_d = f'https://github.com/{D}/pull/9'
            d_blob = syn.git(delta, 'show', f'{d_head}:view.json'); d_art = 'sha256:' + syn.sha256(d_blob)
            af, kf = run.home / '.mycli-fixture', run.home / '.kube-fixture'
            af.mkdir(); kf.mkdir()
            good_pods = syn.pods(d_head, d_art)
            (af / 'pods.json').write_text(good_pods); (af / 'count').write_text('5'); (af / 'signed-in').write_text('yes')
            (kf / 'pods.json').write_text(good_pods); (kf / 'health.json').write_text('{"status":"ok"}')
            calls = lambda d: (d / 'calls.log').read_text().splitlines() if (d / 'calls.log').exists() else []
            progs = {'mycli': str(bin_dir / 'mycli'), 'kubectl': str(bin_dir / 'kubectl')}
            pin_ui = lambda programs: run.value(run.ui(REVIEW, {'checkout': 'delta', 'pin': {'driver': 'verifier-delta', 'programs': programs}}, 150))
            changing = syn.target_contract(); changing['targets'][0]['cases'][0]['args'] = ['stop', 'pg:fixture']
            syn.write(ver_d, {'philium/sql-contracts.json': changing}); syn.commit_all(ver_d, 'a command that changes something')
            v = pin_ui(progs)
            run.check('C9', 'A verifier whose command would change something is refused before it can be pinned', 'mycli stop is not a read Philium runs' in (v.get('refused') or '') and v.get('canPin') is False, v.get('refused') or v)
            syn.write(ver_d, {'philium/sql-contracts.json': syn.target_contract()}); pin_d = syn.commit_all(ver_d, 'read-only checks')
            v = pin_ui({})
            run.check('C9', 'The checks are shown for reading before pinning, with each exact command', len(v.get('proposed') or []) == 4 and any('mycli exec -x staging k8s:fixture -- kubectl get pods -o json' in t for t in v.get('proposedTargets') or []) and any("mycli exec -x staging pg:fixture -c 'SELECT count(*) FROM employees'" in c for c in v.get('proposed') or []) and any('kubectl --context kind-fixture -n app get pods -o json' in t for t in v.get('proposedTargets') or []), (v.get('proposed') or v)[:2])
            run.check('C9', 'Pinning is refused until the person says where each client program is', 'Enter the absolute path of that program' in (v.get('pinResult') or '') and not calls(af) and not calls(kf), v.get('pinResult'))
            v = pin_ui(progs)
            run.check('C9', 'Confirming pins exactly those checks and records when the person confirmed', v.get('pinResult') == 'pinned' and pin_d in (v.get('pinned') or '') and 'You confirmed its checks on' in (v.get('pinned') or '') and str(bin_dir / 'mycli') in (v.get('pinned') or ''), v.get('pinned') or v.get('pinResult'))
            rec_d = {'origin': D, 'pr': 9, 'base': d_base, 'head': d_head, 'driver_pin': pin_d, 'artifact': d_art}
            rec_d_path = f'philium/releases/{d_head}.json'
            syn.write(ver_d, {rec_d_path: rec_d}); syn.commit_all(ver_d, 'record')
            d = verify('delta', pr_d).get('verified') or {}
            a_calls, k_calls = calls(af), calls(kf)
            run.check('C9', 'Philium itself reads both environments: identity before and after, each check twice, only the confirmed commands', 'Contracts: scoped pass' in (d.get('first') or '') and len(d.get('targets') or []) == 2 and all('matches this head and artifact; after matches' in t for t in d.get('targets') or []) and a_calls.count('exec -x staging k8s:fixture -- kubectl get pods -o json') == 2 and a_calls.count('whoami') == 2 and a_calls.count('exec -x staging pg:fixture -c SELECT count(*) FROM employees') == 2 and len(a_calls) == 6 and k_calls.count('--context kind-fixture -n app get pods -o json') == 2 and len(k_calls) == 4 and not any('CHANGE' in c for c in a_calls + k_calls), {'targets': d.get('targets'), 'adaptive_calls': len(a_calls), 'kubectl_calls': len(k_calls), 'first': (d.get('first') or '')[-60:]})
            run.check('C9', 'Observed but not reviewed is still not a ship recommendation', d.get('headline') == 'Release verification incomplete' and 'Review record: code review is not recorded as complete' in why(d), why(d)[:200])
            whole_d = syn.git(delta, 'diff', '--name-status', d_base, d_head) == 'M\tview.json' and d_blob == '{"column":"status"}'
            for cid, obs in [('source', 'The only change is the data file view.json, column name to status, committed at this exact head.'), ('env01-login', 'The stand-in environment answered whoami twice with context staging.'), ('env01-count', 'The stand-in environment returned count 5 twice while reporting this exact revision and artifact before and after.'), ('local-health', 'The stand-in local cluster returned status ok twice while reporting this exact revision and artifact before and after.')]:
                body = json.dumps({'case': cid, 'head': d_head, 'status': 'pass', 'observation': obs})
                syn.write(ver_d, {f'philium/evidence/{cid}.json': body})
                rec_d.setdefault('evidence', {})[cid] = {'path': f'philium/evidence/{cid}.json', 'sha256': syn.sha256(body)}
            rec_d.update({'all_changes_accounted_for': True, 'code_review_complete': True, 'cleanup_verified': True, 'unresolved_findings': [], 'required_cases': ['source', 'env01-login', 'env01-count', 'local-health']})
            if whole_d:
                syn.write(ver_d, {rec_d_path: rec_d}); syn.commit_all(ver_d, 'reviewed')
            d = verify('delta', pr_d).get('verified') or {}
            run.check('C9', 'With both environments matching and the synthetic change reviewed: ready, and the decision says when the checks were confirmed', d.get('headline') == 'Ready for the reviewed scope' and 'Checks confirmed by you on' in (d.get('confirmed') or '') and (d.get('ids') or {}).get('Build record') == d_art, {'headline': d.get('headline'), 'confirmed': d.get('confirmed'), 'why': why(d)[:200]})
            (af / 'signed-in').unlink(); n = len(calls(af))
            d = verify('delta', pr_d).get('verified') or {}
            run.check('C9', 'Not signed in to the hosted environment: not ready, and the form says to sign in', d.get('headline') == 'Release verification incomplete' and 'env01: its identity command did not return a complete, well-formed answer before the checks. Check that you are signed in' in why(d) and not any(c.startswith('whoami') or 'pg:fixture' in c for c in calls(af)[n:]), why(d)[:260])
            (af / 'signed-in').write_text('yes'); (af / 'pods.json').write_text(syn.pods(d_base, d_art)); n = len(calls(af))
            d = verify('delta', pr_d).get('verified') or {}
            run.check('C9', 'The hosted environment runs another revision: named as the wrong deployment and not sampled', d.get('headline') == 'Release verification incomplete' and 'env01: Wrong deployment before the checks' in why(d) and not any('pg:fixture' in c for c in calls(af)[n:]) and 'local' not in why(d), why(d)[:260])
            (af / 'pods.json').write_text(good_pods); (af / 'count').write_text('6')
            d = verify('delta', pr_d).get('verified') or {}
            run.check('C9', 'The right deployment returns the wrong value: Do not ship, naming the check and the environment', d.get('headline') == 'Do not ship' and 'env01-count (env01): the environment returned a different value than the confirmed check expects' in why(d), why(d)[:260])
            (af / 'count').write_text('5')
            unidentified = syn.target_contract(); del unidentified['targets'][0]['identity']
            syn.write(ver_d, {'philium/sql-contracts.json': unidentified}); pin_d2 = syn.commit_all(ver_d, 'no identity for the hosted environment')
            v = pin_ui(progs)
            rec_d['driver_pin'] = pin_d2; syn.write(ver_d, {rec_d_path: rec_d}); syn.commit_all(ver_d, 'record for the new pin')
            d = verify('delta', pr_d).get('verified') or {}
            env_live = [c for c in d.get('live') or [] if 'target env01' in c[1]]
            run.check('C9', 'An environment that cannot say what it runs is observed but never counted', any('no identity command' in t for t in v.get('proposedTargets') or []) and d.get('headline') == 'Release verification incomplete' and 'env01: the confirmed checks include no identity command' in why(d) and len(env_live) == 2 and all(c[0] == 'pass' for c in env_live), {'why': why(d)[:220], 'env01': [c[0] for c in env_live]})
            run.check('C9', 'No command outside the confirmed reads was ever run', not any('CHANGE' in c for c in calls(af) + calls(kf)), f'{len(calls(af))} hosted reads, {len(calls(kf))} local reads')
            run.check('C6', 'No request other than GET reached the origin stand-in or the observer', True, f'{len(syn.Origin.calls)} origin reads, {len(syn.Observer.calls)} observer reads; both servers only implement GET')
            syn.Origin.mode = 'ok'
            # The whole review from one action: paste the pull request, press once.
            r = run.ui(ORCHESTRATE, {'checkout': 'gamma', 'url': 'https://github.com/philium-fixture/gamma/pull/7'}, 400)
            v = run.value(r)
            items = v.get('items') or []
            run.check('C2', 'One action reviews a release: the facts are checked and an orchestrator writes a plan of checks', 'is organising the review of #7' in (v.get('started') or '') and sorted(v.get('planned') or []) == ['Migrations apply in order on an empty database', 'The list shows the status column', 'The notes mention the new column'] and bool(v.get('facts')), {'started': v.get('started'), 'planned': v.get('planned'), 'facts': v.get('facts'), 'error': r.get('error')})
            run.check('C2', 'Each check is done by an agent and recorded as pass, fail or blocked with what it ran; findings come first', [x[1] for x in items] == ['fail', 'pass', 'pass'] and 'finch: Read view.json on the head commit' in ' '.join(x[2] for x in items), items)
            found = items[0] if len(items) == 3 else ['', '', '', '', '']
            run.check('C2', 'Every finding has a severity (P0 to P5) and is labelled introduced, pre-existing or origin unverified; an unlabelled failure is refused', found[0] == '4' and found[4].startswith('P4') and found[3] == 'pre_existing' and found[2].startswith('pre-existing') and 'Unlabelled first try refused.' in found[2] and [x[0] for x in items[1:]] == ['', ''], found)
            con = sqlite3.connect(f"file:{run.profile / 'data/philium.db'}?mode=ro", uri=True)
            starts = con.execute("SELECT a.role, w.base_commit FROM worktrees w JOIN agents a ON a.id=w.agent_id JOIN repos r ON r.id=w.repo_id WHERE r.name='gamma'").fetchall(); con.close()
            under_review = syn.Origin.repos[G]['prs'][7]['heads'][-1]
            run.check('C2', 'In a checkout whose default branch is only on its origin, the orchestrator and its helpers start on the head under review', {r for r, _ in starts} >= {'orchestrator', 'helper'} and all(b == under_review for _, b in starts), {'starts': starts, 'head': under_review})
            run.check('C2', 'The items add up to a verdict in which P0 and P1 decide, and the orchestrator leaves a written report', 'passed' in (v.get('headline') or '') and '3 of 3 items have a result' in (v.get('progress') or '') and 'Verdict: ready for your sign-off' in (v.get('report') or '') and 'Report by' in (v.get('reportBy') or ''), {'headline': v.get('headline'), 'progress': v.get('progress'), 'report': v.get('report'), 'by': v.get('reportBy')})
            origin.shutdown(); observer.shutdown()

        if want('peer'):
            out = scratch / 'terminal-out'; out.mkdir(exist_ok=True)
            r = run.ui(PEER, {'a': 'alpha/same', 'b': 'beta/same', 'b_task': 'Beta only task', 'out': str(out)}, 240)
            v = run.value(r)
            a, b = v.get('alpha/same') or [{}], v.get('beta/same') or [{}]
            a_tasks = {'Add status column', 'Long migration'} if want('coord') else set()
            run.check('B6', 'Each checkout shows only its own tasks, on every switch', all(set(s.get('tasks') or []) == a_tasks for s in a) and all(s.get('tasks') == ['Beta only task'] for s in b) and len(a) == 2 and len(b) == 2, {'alpha': [s.get('tasks') for s in a], 'beta': [s.get('tasks') for s in b]} if v else r.get('error'))
            run.check('B6', 'Each checkout shows only its own agents', all(sorted(s.get('agents') or []) == (['ibis', 'lead', 'wren'] if want('coord') else []) for s in a) and all(s.get('agents') == [] for s in b), {'alpha': a[0].get('agents'), 'beta': b[0].get('agents')})
            run.check('B6', 'The checkout home of one never names the other checkout\'s work', all('Beta only task' not in (s.get('home') or '') for s in a) and all('Long migration' not in (s.get('home') or '') and 'lead' not in (s.get('home') or '') for s in b), (b[0].get('home') or '')[:160])
            own = lambda s: [t for t in s.get('terminals') or [] if ' · ' not in t]
            theirs = lambda s: {t.split(' · ')[1] for t in s.get('terminals') or [] if ' · ' in t}
            run.check('B6', 'Each checkout lists one terminal of its own, and agent shells only of its own agents', all(len(own(s)) == 1 for s in a + b) and all(theirs(s) <= {'lead', 'wren', 'ibis'} for s in a) and all(not theirs(s) for s in b), {'alpha': a[-1].get('terminals'), 'beta': b[-1].get('terminals')})
            cwd = {n: (out / f'{n}-same.cwd').read_text().strip() if (out / f'{n}-same.cwd').exists() else None for n in ('alpha', 'beta')}
            run.check('B6', 'Each terminal started in its own checkout', cwd == {'alpha': str(alpha), 'beta': str(beta)}, cwd)
            env = (out / 'alpha-same.env').read_text() if (out / 'alpha-same.env').exists() else ''
            vars_ = dict(l.split('=', 1) for l in env.splitlines() if '=' in l)
            run.check('B6', 'A terminal the person opens has their own home, not the private one', vars_.get('HOME') == str(run.home), vars_.get('HOME'))
            run.check('D1', 'That terminal carries no variable of another agent harness', bool(env) and not any(k.startswith(('JCODE_', 'HERDR_')) for k in vars_), [k for k in vars_ if k.startswith(('JCODE_', 'HERDR_'))])
            if want('review'):
                run.check('B6', 'Release history and decisions appear only in the checkout they belong to', all(len(s.get('review') or []) >= 2 for s in a) and all(s.get('review') == [] for s in b), {'alpha': a[0].get('review'), 'beta': b[0].get('review')})
            # The orchestrated review, run earlier on a third checkout, left its three items on the board.
            review_items = {'Migrations apply in order on an empty database', 'The list shows the status column', 'The notes mention the new column'} if want('review') else set()
            run.check('B6', 'All Workspaces lists every task', set(v.get('all') or []) == a_tasks | {'Beta only task'} | review_items, v.get('all'))

        if want('probe'):
            for name, provider, strict in [('probe', None, False), ('strictprobe', 'Synthetic Strict', True)]:
                r = run.ui(PROBE, {'checkout': 'beta/same', 'name': name, 'provider': provider}, 200)
                v = run.value(r)
                res = v.get('result') or ''
                if provider:
                    run.check('B1', 'The model list opens in the New agent dialog and another provider can be chosen', provider in (v.get('picked') or '') and v.get('started') == 'started', v.get('picked') or r.get('error'))
                if 'PROBE RESULT' not in res:
                    print('  probe diagnostics:', run.redact({'head': v.get('head'), 'problem': v.get('problem')}), flush=True)
                home = (res.split('home=')[1].split('\\n')[0] if 'home=' in res else '')
                private = home == f'{run.profile}/home' if not strict else home.startswith(f'{run.profile}/data/agents/{name}/')
                run.check('D1', f"{'Strict' if strict else 'Trusted'} agent shell: no variable or tool of another harness, and a private home inside the profile", 'vars=0' in res and 'tool=none' in res and private and home != str(run.home), {'home': home, 'vars_tool': res[14:60]} if res else (v.get('started') or r.get('error')))
                path = (res.split('path=')[1].split('\\n')[0].split('"')[0] if 'path=' in res else '')
                if not strict:
                    run.check('B1', "Started with the system's bare PATH, an agent still gets the login shell's PATH, without another harness's directory", '/usr/local/bin' in path.split(':') and '.jcode' not in path, path[:200])
                denied = [p for p in [str(vault)] + ([str(run.home / '.ssh/id_probe'), str(run.home / '.jcode/secret')] if strict else []) if f'DENIED {p}' in res]
                run.check('D1', f"{'Strict' if strict else 'Trusted'} agent cannot read the key vault" + (' or the account home\'s keys and other harness data' if strict else ''), len(denied) == (3 if strict else 1) and 'READ ' + str(vault) not in res, res[-420:])
            trees = lambda: sorted(p.name.split('-')[0] for p in (run.profile / 'workspace/worktrees/same').glob('*'))
            before = trees()
            r = run.ui(REMOVE, {'checkout': 'beta/same', 'name': 'probe'}, 150)
            v = run.value(r)
            run.check('B4', 'A finished agent can be removed from its header; its own worktree goes and nothing else', v.get('gone') is True and 'strictprobe' in (v.get('rows') or []) and 'probe' in before and 'probe' not in trees() and set(before) - set(trees()) == {'probe'}, {'gone': v.get('gone'), 'rows': v.get('rows'), 'seen': v.get('seen'), 'before': before, 'after': trees()} if v else r.get('error'))
            run.check('D1', 'The decoy tool on PATH was never run', not run.decoy_marker.exists(), 'ran' if run.decoy_marker.exists() else 'not run')

        if want('workday'):
            r = run.ui(WORKDAY, {'note': 'synthetic fallback note'}, 120)
            v = run.value(r)
            if want('probe'):
                run.check('B5', 'The checkout selected before the last quit is selected again after reopening', v.get('remembered') == 'beta/same', v.get('remembered'))
            acts = {x[0]: x for x in (v.get('before') or {}).get('actions') or []}
            need = ['workspace.repo_add', 'providers.save'] + (['agents.spawn', 'agents.send', 'agents.stop', 'agents.resume', 'agents.cancel'] if want('coord') else [])
            run.check('B7', 'The workday ledger lists the actions taken on screen, by checkout, without their content', all(k in acts for k in need) and 'synthetic' not in json.dumps(acts).lower().replace('synthetic strict', ''), {k: acts.get(k) for k in need[:4]} if v else r.get('error'))
            run.check('B7', 'A fallback is recorded from the screen and the summary is written inside the profile', 'Manual database edit' in (v.get('fallback') or '') and str(run.profile / 'data/workday') in (v.get('exported') or ''), v.get('exported') or v.get('fallback'))
        if want('storage') or not only:
            dead = subprocess.Popen(['/usr/bin/true']); dead.wait()
            leftover = run.home / f'Library/Caches/philium-pipeline/runs/app-{dead.pid}/philium-x/src'
            leftover.mkdir(parents=True); (leftover / 'file').write_text('x' * 4096)
            ghost = run.profile / 'data/agents/ghost-agent/tmp'; ghost.mkdir(parents=True); (ghost / 'file').write_text('x')
            r = run.ui(STORAGE, {}, 150)
            v = run.value(r)
            parts = dict(v.get('parts') or [])
            run.check('E2', 'Settings shows what Philium keeps on disk, part by part, with the build caches of the private home', all(k in parts for k in ['database', 'worktrees', 'agents', 'check_runs', 'agent_caches']) and 'free of' in (v.get('disk') or ''), v.get('parts') or r.get('error'))
            run.check('E2', 'Clean up now reports what it did, and nothing a check run or a removed agent left behind is still on disk', (v.get('last') or '').startswith('Last clean up') and not leftover.parents[1].exists() and not ghost.parent.exists(), {'last': v.get('last'), 'leftover': leftover.parents[1].exists(), 'ghost': ghost.parent.exists()})
        foreign = sorted(c for c in run.children if 'jcode' in c.lower() or 'herdr' in c.lower())
        run.check('D1', 'No process of another agent harness ran under the Pilot', not foreign, {'foreign': foreign, 'programs': sorted({Path(c).name for c in run.children})})
        if want('review') or want('draft'):
            # No folder, no second checkout: an agent drafts checks from inside Review, the
            # person accepts and confirms, and Philium keeps them in its own store. Runs last,
            # so the agent it starts is not counted by the checks above.
            r = run.ui(DRAFT, {'checkout': 'beta/same'}, 260)
            v = run.value(r)
            kept = list((run.profile / 'data/checks').glob('*/philium/sql-contracts.json'))
            run.check('C1', 'With no checks yet, an agent started from Review proposes them and they are listed in plain words', len(v.get('proposed') or []) == 2 and 'committed file' in ' '.join(v.get('proposed') or []) and 'Proposed by' in (v.get('by') or ''), {'started': v.get('started'), 'by': v.get('by'), 'proposed': v.get('proposed'), 'status': v.get('status'), 'error': r.get('error')})
            run.check('C1', 'The proposal counts only after the person accepts it and confirms the same checks', v.get('toConfirm') == v.get('proposed') and 'You confirmed its checks' in (v.get('pinned') or '') and 'No checks confirmed' in ((v.get('before') or {}).get('pinned') or '') and v.get('left') == 0, {'before': (v.get('before') or {}).get('pinned'), 'pinned': v.get('pinned'), 'left': v.get('left'), 'status': v.get('status'), 'alerts': v.get('alerts'), 'connected': v.get('connected'), 'redrawn': v.get('redrawn'), 'sections': v.get('sections'), 'buttons': v.get('buttons'), 'toasts': v.get('toasts')})
            run.check('C1', 'The checks are kept by Philium itself: no second checkout appears and the person names no folder', 'kept by Philium' in (v.get('pinned') or '') and 'kept by Philium' in (v.get('summary') or '') and sorted(v.get('labels') or []) == sorted(labels) and len(kept) == 1, {'summary': v.get('summary'), 'labels': v.get('labels'), 'kept': [str(k.relative_to(run.profile)) for k in kept]})

        if want('coord') or want('pick'):
            # Pick two agents with Command-click, press Delete, confirm: those two go, nothing else.
            r = run.ui(PICK, {'checkout': 'alpha/same', 'pick': ['lead', 'wren']}, 150)
            v = run.value(r)
            went = [n for n in (v.get('before') or []) if n not in (v.get('after') or [])]
            # The picked ones go, and with them only helpers the confirmation named.
            run.check('B4', 'Agents picked with Command-click are removed together after one confirmation naming every agent that goes, helpers included', sorted(v.get('want') or []) == ['lead', 'wren'] and (v.get('picked') or '').startswith('2 picked') and {'lead', 'wren'} <= set(went) and all(n in (v.get('asked') or '') for n in went), {'picked': v.get('picked'), 'asked': (v.get('asked') or '')[:160], 'unsaved': (v.get('unsaved') or '')[:120], 'gone': v.get('gone'), 'toasts': v.get('toasts'), 'dialog': (v.get('dialog') or '')[:200], 'before': v.get('before'), 'after': v.get('after'), 'error': r.get('error')})

    finally:
        model.shutdown()
        out = {'app': str(run.exe), 'rows': run.rows, 'children': sorted(run.children)}
        if args.out:
            Path(args.out).write_text(json.dumps(out, indent=1))
        if args.keep:
            print('kept', scratch)
        else:
            shutil.rmtree(scratch, ignore_errors=True)
    failed = [r for r in run.rows if not r['ok']]
    print(f"{len(run.rows) - len(failed)} passed, {len(failed)} failed")
    return 1 if failed else 0


if __name__ == '__main__':
    sys.exit(main())
