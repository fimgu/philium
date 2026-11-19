#!/usr/bin/env python3
"""The coordinator flow of a Pilot bundle against the owner's real Claude sign-in.

    python3 scripts/check-real-model.py '/path/to/Philium Pilot.app' [--model claude-sonnet-5-5] [--out FILE] [--keep]

Why this exists: the acceptance harness drives agents with a local stand-in model, which cannot
show a defect in how requests are shaped for a real provider. This run asks a real model, in plain
words, to do what a person asks a coordinator to do.

What it does and does not do
- The owner signs in to Claude in the visible Pilot first. That step is theirs; nothing here can
  do it. The run fails early if `~/Philium Pilot` holds no Claude sign-in.
- It copies the provider, model and settings rows and the short-lived access token into a
  throwaway profile. The refresh token is never copied, so the owner's sign-in cannot be rotated
  or signed out by this run. The owner's checkouts, agents, tasks and ledger are not copied.
- It starts the bundle hidden with that throwaway profile and a synthetic checkout, then clicks,
  types and reads the rendered page: start a coordinator, delegate, read the board, redirect,
  interrupt, stop, kill the app in the middle of a command, reopen and resume.
- It never prints or stores the token, and it deletes the throwaway profile when it ends.

It spends the owner's subscription quota: about fifty short requests on the chosen model.

Do not point it at the app someone is about to open. While a hidden instance runs from a bundle,
macOS hands a Dock, Finder or Spotlight open of that bundle to the hidden instance: nothing
appears, and the hidden run may be ended. Give it a copy instead:
    ditto "$HOME/Applications/Philium Pilot.app" "$PHILIUM_SCRATCH_DIR/copy/Philium Pilot.app"
"""
import argparse, importlib.util, json, os, re, shutil, signal, sqlite3, subprocess, sys, tempfile, time
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('acceptance', HERE / 'check-acceptance.py')
acc = importlib.util.module_from_spec(spec); spec.loader.exec_module(acc)
syn = acc.syn

CHROME = 'Requests a reasoning effort level. Support varies by provider and model, and speed, cost and quality are not guaranteed.Auto None Low Medium High Max'
said = lambda t: (t or '').replace(CHROME, '').replace('Copy reply', '').strip()

TURN = r'''
const STATES = /\b(Idle|Stopped|Needs you|Failed)\b/;
const turn = async (text, ms = 180000) => {
  const n0 = convo().length;
  await sendOnly(text);
  await until(() => /Working|Starting/.test(head()) || convo().length > n0 + text.length + 60, 'turn started', 30000).catch(() => null);
  await until(() => STATES.test(head()) && !/Working/.test(head()), 'turn finished', ms);
  await sleep(900);
  return { state: (head().match(STATES) || [''])[0], tail: convo().slice(-700) };
};
const problems = () => all('.shield.danger', document.querySelector('.agent-view') || document).map(txt).concat(toasts()).join(' | ').slice(0, 400);
'''
AGENT = acc.AGENT + TURN

REGISTER = r'''
await go('Repos');
await click('Add local repo');
const m = await until(modal, 'add dialog');
await fill('Folder', P.repo, m);
await click('Register repository', m);
await until(() => !modal(), 'dialog closed', 25000);
const labels = await until(() => { const o = Array.from(picker().options).map(txt); return o.some((x) => x.includes('workbench')) ? o : null; }, 'checkout in the picker', 15000);
await go('Providers'); await sleep(2000);
const cards = all('.card', pane()).map((c) => txt(c).replace(/\S+@\S+/g, '<account>').slice(0, 160));
return { labels, cards };
'''

START = AGENT + r'''
await workspace(P.checkout);
await go('Home');
const coord = await until(() => all('section[aria-label="Coordinator"]')[0], 'coordinator section', 15000);
const entry = txt(coord).slice(0, 200);
await click('Start coordinator', coord);
const m = await until(modal, 'spawn dialog');
await fill('Name', P.lead, m);
const preset = { role: txt(field('Role', m).selectedOptions[0]), repo: txt(field('Repo', m).selectedOptions[0]) };
const pick = () => m.querySelector('button[aria-label="Choose model"]');
const defaultModel = txt(pick());
pick().click();
const pop = await until(() => all('.model-picker-pop')[0], 'model list');
type(pop.querySelector('input'), P.model); await sleep(500);
const item = await until(() => all('button.model-picker-item', pop).find((b) => txt(b).includes(P.model)), 'the model in the list', 10000);
item.click(); await sleep(300);
const chosenModel = txt(pick());
await click('Start Agent', m);
const started = await until(() => (!modal() ? 'started' : (all('.err', m).map(txt)[0] || null)), 'spawn result', 90000);
if (started !== 'started') return { entry, preset, defaultModel, chosenModel, started };
await until(() => STATES.test(head()), 'coordinator ready', 120000).catch(() => null);
const ready = head();
if (!/Idle/.test(ready)) return { entry, preset, defaultModel, chosenModel, started, ready, problems: problems(), tail: convo().slice(-500) };
const hello = await turn('In one short sentence: which repository are you working in, and what is your role here?');
if (!P.ask) return { entry, preset, defaultModel, chosenModel, started, hello, head: head(), problems: problems() };
const delegated = await turn(P.ask, 300000);
const pills = await until(() => { const p = all('.cluster-subpill').map(txt); return p.length ? p : null; }, 'helper listed under the coordinator', 30000).catch(() => []);
await go('Board');
const card = () => all('article.board-task').find((a) => txt(a).includes(P.task));
const created = await until(card, 'task card', 60000).then(() => true, () => false);
let column = null, owner = null, cards = all('article.board-task').map((a) => txt(a).slice(0, 90));
if (created) {
  await until(() => { const col = card()?.closest('.board-column'); return col && /^(In review|Done)/.test(txt(col.querySelector('h3'))); }, 'task submitted', P.work_ms).catch(() => null);
  column = txt(card().closest('.board-column').querySelector('h3')).replace(/\s*\d+$/, ''); owner = txt(card().querySelector('.board-owner'));
}
await openAgent(P.lead);
const report = await until(() => { const t = convo(); const i = t.indexOf('Mail delivered by Philium'); return i >= 0 ? t.slice(i, i + 300) : null; }, 'helper report in the coordinator conversation', 60000).catch(() => null);
await until(() => /Idle/.test(head()), 'coordinator idle again', 120000).catch(() => null);
return { entry, preset, defaultModel, chosenModel, started, hello, delegated, pills, created, column, owner, cards, report, head: head(), problems: problems(), tail: convo().slice(-900) };
'''

BOARD = acc.BOARD.replace(r'status\.txt|No changes|diff', r'NOTES|No changes|diff')

REDIRECT = AGENT + r'''
await workspace(P.checkout);
await openAgent(P.worker);
const before = head();
const resumed = await live();
const delivered = await until(() => { const t = convo(); const i = t.indexOf('From you: Task'); return i >= 0 ? t.slice(i, i + 160) : null; }, 'note from the person delivered', 40000).catch(() => null);
await sleep(2500);
await until(() => /Idle/.test(head()), 'idle after reading the note', 180000).catch(() => null);
const redirected = await turn('Change of plan: rename NOTES.md to NOTES.txt with git mv and commit that. Then reply with one short sentence.', 300000);
const long = 'Use your shell tool to run exactly this command and nothing else: sleep 90   When it has finished, reply with the word finished.';
await sendOnly(long);
await until(() => /Working/.test(head()), 'working', 40000);
await sleep(9000);
document.querySelector('button.composer-send.stop[aria-label="Stop"]').click(); note('click the turn Stop');
const interrupted = await until(() => /Idle/.test(head()), 'idle after interrupting the turn', 60000).then(() => head(), (e) => String(e));
const after = await turn('Reply with exactly one word: here', 120000);
await sendOnly(long);
await until(() => /Working/.test(head()), 'working again', 40000);
await sleep(9000);
await click('Stop', document.querySelector('.agent-head'));
const stopped = await until(() => /Stopped/.test(head()) && button('Resume', document.querySelector('.agent-head')), 'stopped', 60000).then(() => head(), (e) => String(e));
const disabled = composer() ? composer().disabled : null;
await click('Changes', document.querySelector('.agent-head'));
const changes = await until(() => { const t = convo(); return /NOTES/.test(t) ? t.slice(0, 900) : null; }, 'changes', 20000).catch(() => convo().slice(0, 400));
return { before, resumed, note: delivered, redirected, interrupted, after, stopped, disabled, changes, problems: problems() };
'''

DELEGATE_SLOW = AGENT + r'''
await workspace(P.checkout);
await openAgent(P.lead);
const resumed = await live();
await until(() => /Idle/.test(head()), 'coordinator idle', 120000).catch(() => null);
const delegated = await turn(P.ask, 300000);
const pills = all('.cluster-subpill').map(txt);
// Stay open. The harness ends this app lifetime by killing the process while the helper's command runs.
await sleep(P.secs * 1000);
return { resumed, delegated, pills, tail: convo().slice(-600), problems: problems() };
'''

EFFORT = AGENT + r'''
await workspace(P.checkout);
await openAgent(P.lead);
const resumed = await live();
// Mail that arrived while it was stopped starts a turn right after the resume. Wait that out.
await until(() => /Idle/.test(head()), 'coordinator idle', 120000).catch(() => null);
await sleep(4000);
await until(() => /Idle/.test(head()), 'coordinator idle after its mail', 240000).catch(() => null);
await sendOnly('/effort ' + P.level);
await sleep(1500);
const notices = toasts();
const reply = await turn('Reply with exactly one word: ready', 180000);
// The same small reasoning task at the lowest and the highest level, to see whether the level
// changes how much the model writes and thinks (the gateway records the output tokens).
const task = 'Without using any tool: how many five-digit palindromes are divisible by 7? Work it out, then reply with the number only.';
const depth = {};
for (const level of P.compare || []) {
  await sendOnly('/effort ' + level); await sleep(1200);
  depth[level] = (await turn(task, 240000)).tail.slice(-60);
}
// The effort menu's text sits at the end of the conversation pane: the levels this model offers.
const levels = (convo().match(/not guaranteed\.\s*([A-Za-z ]+)$/) || [])[1] || '';
return { resumed, notices, reply, levels, depth, toasts: toasts(), problems: problems() };
'''

AFTER = acc.AFTER_RESTART.replace(acc.AGENT, AGENT).replace("await say(P.say, 'SYNTHETIC REPLY from')", 'await turn(P.say, 300000)')
assert AFTER != acc.AFTER_RESTART and 'turn(P.say' in AFTER, 'the acceptance scenarios changed; update this script'


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('app'); ap.add_argument('--model', default='claude-sonnet-5-5'); ap.add_argument('--out')
    ap.add_argument('--only', choices=['effort'], help='effort: start a coordinator and check only the effort step (about one minute)')
    ap.add_argument('--keep', action='store_true', help='keep the scratch folder (the copied access token is removed either way)')
    args = ap.parse_args()
    real = Path.home() / 'Philium Pilot/data'
    try:
        vault = json.loads((real / 'secrets/vault.json').read_text())
        raw = vault['oauth:claude']
    except (OSError, KeyError, ValueError):
        sys.exit('No Claude sign-in in ~/Philium Pilot. Open the Pilot, go to Providers and sign in to Claude first.')
    creds = json.loads(raw) if isinstance(raw, str) else dict(raw)
    hours = round((creds['expires_at'] / 1000 - time.time()) / 3600, 1)
    if hours < 0.3:
        sys.exit('The access token is about to expire. Send one message in the Pilot so it refreshes, then run this again.')
    secrets = [s for s in (creds.get('access'), creds.get('refresh'), creds.get('email'), creds.get('account_id')) if s]
    creds['refresh'] = ''

    root = Path(tempfile.mkdtemp(prefix='philium-real-model-', dir=os.environ.get('PHILIUM_SCRATCH_DIR') or None)).resolve()
    run = acc.Run(args.app, root / 'run', None)
    data = run.profile / 'data'
    (data / 'secrets').mkdir(parents=True, mode=0o700)
    for d in (run.profile, data, data / 'secrets'):
        os.chmod(d, 0o700)
    src = sqlite3.connect(f'file:{real}/philium.db?mode=ro', uri=True)
    dst = sqlite3.connect(data / 'philium.db'); src.backup(dst); src.close()
    # Only the sign-in travels: the provider, its models and settings.
    for (t,) in dst.execute("select name from sqlite_master where type='table'").fetchall():
        if t in ('providers', 'models', 'kv') or t.startswith('sqlite_') or t.startswith('memories'):
            continue
        try:
            dst.execute(f'delete from "{t}"')
        except sqlite3.Error:
            pass
    dst.commit()
    assert dst.execute('select count(*) from repos').fetchone()[0] == 0
    dst.close()
    token_copy = data / 'secrets/vault.json'
    fd = os.open(token_copy, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    os.write(fd, json.dumps({'oauth:claude': json.dumps(creds) if isinstance(raw, str) else creds}).encode()); os.close(fd)
    del vault, raw, creds

    def clean(v):
        s = v if isinstance(v, str) else json.dumps(v, default=str)
        for x in secrets:
            s = s.replace(x, '<redacted>')
        s = re.sub(r'[\w.+-]+@[\w-]+\.[\w.]+', '<account>', s)
        return s.replace(str(root), '<scratch>').replace(str(Path.home()), '<home>')

    rows, raw_results = [], {}

    def save():
        if args.out:
            Path(args.out).write_text(json.dumps({'model': args.model, 'token_hours_left': hours, 'rows': rows, 'raw': json.loads(clean(raw_results))}, indent=1))

    def check(row, what, ok, observed=''):
        rows.append({'row': row, 'check': what, 'ok': bool(ok), 'observed': clean(observed)[:900]})
        print(f"{'PASS' if ok else 'FAIL'} {row} {what} :: {clean(observed)[:300]}", flush=True)
        save()

    def ui(name, body, params, seconds, **kw):
        assert seconds <= 900, 'the hidden driver accepts at most 900 seconds'
        t0 = time.time()
        r = run.ui(body, params, seconds, **kw)
        raw_results[name] = {'seconds': round(time.time() - t0), 'ok': r.get('ok'), 'error': r.get('error'), 'alerts': r.get('alerts'), 'exit': r.get('exit'), 'result': r.get('result'), 'log': (r.get('log') or [])[-12:]}
        print(f"-- {name}: {round(time.time() - t0)}s ok={r.get('ok')} error={clean(r.get('error') or '')[:300]}", flush=True)
        save()
        return r, (r.get('result') or {})

    def refusals():
        """Requests the provider refused, as the gateway recorded them: model and status only."""
        try:
            db = sqlite3.connect(f'file:{data}/philium.db?mode=ro', uri=True)
            rows_ = db.execute('select model, status, count(*) from requests where status >= 400 group by 1, 2').fetchall()
            db.close()
            return [f'{m} status {st} x{n}' for m, st, n in rows_]
        except sqlite3.Error as e:
            return [type(e).__name__]

    def marked_without_effort():
        """Models the Pilot has concluded take no effort setting. It concludes that when the provider
        refuses a request that carried one; the request is then resent without it and recorded as
        answered, so this note is the only trace of the refusal."""
        try:
            db = sqlite3.connect(f'file:{data}/philium.db?mode=ro', uri=True)
            row = db.execute("select v from kv where k like 'models_efforts.%'").fetchall()
            db.close()
            return sorted(m for (v,) in row for m, levels in json.loads(v).items() if not levels)
        except (sqlite3.Error, ValueError) as e:
            return [type(e).__name__]

    def ps():
        return subprocess.run(['/bin/ps', '-axo', 'pid=,ppid=,command='], capture_output=True, text=True).stdout.splitlines()

    def sweep(label):
        """End only what this run started: processes that carry this run's scratch folder or its marker command."""
        mine = []
        for line in ps():
            p = line.split(None, 2)
            if len(p) == 3 and int(p[0]) != os.getpid() and (str(root) in p[2] or p[2].strip().endswith('sleep 127')) and 'check-real-model.py' not in p[2]:
                mine.append(int(p[0]))
        for sig in (signal.SIGTERM, signal.SIGKILL):
            for pid in mine:
                try:
                    os.kill(pid, sig)
                except ProcessLookupError:
                    pass
            if mine:
                time.sleep(2)
        print(f'-- sweep {label}: {len(mine)} leftover process(es) ended', flush=True)

    class Done(Exception):
        """The chosen part has run; go on to the clean-up."""

    def effort_step():
        """Each model has its own levels, and a level must not cost a refused request."""
        r, v = ui('effort', EFFORT, {'checkout': 'workbench', 'lead': 'lead', 'level': 'high', 'compare': ['low', 'max']}, 800)
        refused = refusals() + [f'{m} marked as taking no effort after a refusal' for m in marked_without_effort()]
        check('real-effort', 'With effort set to High the model answers and no request is refused', said((v.get('reply') or {}).get('tail')).lower().endswith('ready') and not refused, {'reply': said((v.get('reply') or {}).get('tail'))[-80:], 'refused': refused, 'notices': v.get('notices'), 'error': r.get('error')})
        # Not a pass or fail: what the gateway recorded for the same task at the lowest and highest level.
        try:
            db = sqlite3.connect(f'file:{data}/philium.db?mode=ro', uri=True)
            last = db.execute('select tokens_out, duration_ms, status from requests order by ts desc limit 2').fetchall()
            db.close()
            raw_results['effort_depth'] = {'low': {'output_tokens': last[1][0], 'ms': last[1][1], 'status': last[1][2]}, 'max': {'output_tokens': last[0][0], 'ms': last[0][1], 'status': last[0][2]}, 'answers': v.get('depth')}
            print(f"-- same task, effort low: {last[1][0]} output tokens in {last[1][1]} ms; effort max: {last[0][0]} output tokens in {last[0][1]} ms; answers {clean(v.get('depth'))[:160]}", flush=True)
        except (sqlite3.Error, IndexError) as e:
            print(f'-- effort depth not measured: {type(e).__name__}', flush=True)
        check('real-effort', 'The effort menu lists the levels this model has, xhigh included and no None', 'xhigh' in (v.get('levels') or '').lower().replace(' ', '') and 'none' not in (v.get('levels') or '').lower(), v.get('levels'))

    try:
        repo = syn.new_repo(root / 'run/workbench')
        syn.write(repo, {'README.md': '# Workbench\n\nA tiny synthetic project used to try a coordinator.\n', 'app.py': 'print("hi")\n'})
        syn.commit_all(repo, 'Start the workbench')

        r, v = ui('register', REGISTER, {'repo': str(repo)}, 120)
        check('real-B1', 'The Claude sign-in and its models are listed, and a checkout registers', any('workbench' in x for x in v.get('labels') or []) and any('Claude' in c and 'models' in c for c in v.get('cards') or []), v or r.get('error'))

        if args.only == 'effort':
            r, v = ui('start', START, {'checkout': 'workbench', 'lead': 'lead', 'model': args.model, 'ask': '', 'task': 'NOTES', 'work_ms': 1000}, 400)
            check('real-B1', 'The real model answers a typed message in the coordinator conversation', ((v.get('hello') or {}).get('state')) == 'Idle' and 'provider error' not in ((v.get('hello') or {}).get('tail') or ''), v.get('hello') or v.get('problems') or r.get('error'))
            effort_step()
            raise Done()
        ask = ('Create one board task titled exactly "Add NOTES file". Then start one helper agent named scribe and hand that task to it. '
               'The helper must create a file NOTES.md containing exactly the line: hello from philium  Then it must commit the file and submit the task. '
               'Do not do the work yourself. Reply with one short sentence once the helper has been started.')
        r, v = ui('start', START, {'checkout': 'workbench', 'lead': 'lead', 'model': args.model, 'ask': ask, 'task': 'NOTES', 'work_ms': 300000}, 890)
        check('real-B1', 'Start coordinator opens preset to this checkout as an orchestrator, on the chosen Claude model', v.get('started') == 'started' and (v.get('preset') or {}).get('repo') == 'workbench' and 'Orchestrator' in (v.get('preset') or {}).get('role', ''), {'preset': v.get('preset'), 'default': v.get('defaultModel'), 'chosen': v.get('chosenModel'), 'started': v.get('started'), 'ready': v.get('ready'), 'error': r.get('error')})
        hello = v.get('hello') or {}
        check('real-B1', 'The real model answers a typed message in the coordinator conversation', hello.get('state') == 'Idle' and len(hello.get('tail') or '') > 120 and 'provider error' not in (hello.get('tail') or ''), hello or v.get('problems') or r.get('error'))
        deleg = v.get('delegated') or {}
        pills = v.get('pills') or []
        check('real-B2', 'Asked in plain words, the coordinator creates the task and starts a helper', bool(v.get('created')) and bool(pills), {'pills': pills, 'cards': v.get('cards'), 'reply': said(deleg.get('tail'))[-260:], 'error': r.get('error')})
        check('real-B2', 'The helper finishes: the task reaches In review or Done with the helper as owner', v.get('column') in ('In review', 'Done') and bool(v.get('owner')), {'column': v.get('column'), 'owner': v.get('owner')})
        check('real-B2', "The helper's result reaches the coordinator's conversation without anyone asking", bool(v.get('report')), v.get('report') or said(v.get('tail'))[-300:])
        worker = (pills[0].split(' ')[0] if pills else 'scribe')

        r, v = ui('board', BOARD, {'checkout': 'workbench', 'task': 'NOTES', 'open_agent': True, 'wait_status': 'In review', 'wait_ms': 5000, 'note': 'Thanks. No change needed. Acknowledge this note in one short sentence.'}, 200)
        hist = ' | '.join(v.get('history') or [])
        check('real-B2', 'After a reopen the board still names the helper as owner and the history names who did what', worker in (v.get('owner') or '') + (v.get('ownerLine') or '') and 'assigned' in hist and 'submitted' in hist, {'owner': v.get('owner'), 'column': v.get('column'), 'history': hist[:500]} if v else r.get('error'))
        check('real-B3', 'From the task the helper opens with its lead named, its transcript readable and NOTES.md in Changes', 'lead: lead' in (v.get('agentHead') or '') and len(v.get('transcript') or '') > 200 and 'NOTES.md' in (v.get('changes') or ''), {'head': v.get('agentHead'), 'changes': (v.get('changes') or '')[:300]})

        r, v = ui('redirect', REDIRECT, {'checkout': 'workbench', 'worker': worker}, 890)
        check('real-B5', 'After a normal quit the stopped helper resumes from its header', 'Stopped' in (v.get('before') or '') and v.get('resumed') == 'resumed', v.get('before') or r.get('error'))
        check('real-B4', 'A note sent from the task is delivered to the helper when it resumes', 'Acknowledge this note' in (v.get('note') or ''), v.get('note'))
        check('real-B4', 'A redirect typed to the helper is carried out: Changes show NOTES.txt', 'NOTES.txt' in (v.get('changes') or ''), {'reply': said((v.get('redirected') or {}).get('tail'))[-200:], 'changes': (v.get('changes') or '')[:300]})
        check('real-B4', 'Interrupting a turn stops it and the helper still answers', 'Idle' in (v.get('interrupted') or '') and said((v.get('after') or {}).get('tail')).lower().endswith('here'), {'interrupted': v.get('interrupted'), 'after': said((v.get('after') or {}).get('tail'))[-60:]})
        check('real-B4', 'Stop ends the helper mid-turn and disables its composer', 'Stopped' in (v.get('stopped') or '') and v.get('disabled') is True, {'stopped': v.get('stopped'), 'disabled': v.get('disabled'), 'problems': v.get('problems')})
        sweep('after redirect')

        seen = {}

        def mid():
            for line in ps():
                p = line.split(None, 2)
                if len(p) == 3 and p[2].strip().endswith('sleep 127'):
                    seen.setdefault('t', time.time())
                    return time.time() - seen['t'] > 4
            return False

        ask2 = ('Create one board task titled exactly "Slow check". Then start one helper agent named clerk and hand that task to it. '
                'The helper must first run exactly this shell command: sleep 127  Only after it finishes it must create DONE.md, commit it and submit the task. '
                'Do not do the work yourself. Reply with one short sentence once the helper has been started.')
        r, v = ui('kill', DELEGATE_SLOW, {'checkout': 'workbench', 'lead': 'lead', 'ask': ask2, 'secs': 240}, 700, kill_when=mid)
        check('real-B5', "The app was killed while the second helper's command was running", r.get('exit') == -9 and 't' in seen, f"exit {r.get('exit')} seen={'t' in seen}")
        sweep('after kill')

        r, v = ui('after', AFTER, {'checkout': 'workbench', 'worker': 'clerk', 'task': 'Slow check', 'agents': 3, 'say': 'The app was interrupted. Skip the sleep command. Create DONE.md, commit it and submit the task now.'}, 700)
        check('real-B5', 'After the crash the same three agents exist, none duplicated', sorted(v.get('rows') or []) == sorted(['clerk', 'lead', worker]), v.get('rows') or r.get('error'))
        check('real-B5', 'The interrupted helper is shown stopped with a plain reason, and the board still says whose task it is', 'Stopped' in (v.get('stoppedHead') or '') and 'Resume' in (v.get('notice') or '') and 'clerk' in ((v.get('boardBefore') or {}).get('owner') or ''), {'notice': v.get('notice'), 'board': v.get('boardBefore'), 'handover': v.get('handover')})
        again = v.get('again') or {}
        check('real-B5', 'Resume continues the same conversation and the helper finishes the task', v.get('resumed') == 'resumed' and again.get('state') == 'Idle', {'again': said(again.get('tail'))[-300:], 'after': v.get('boardAfter')})

        r, v = ui('board2', BOARD.replace('NOTES|No changes|diff', 'DONE|No changes|diff'), {'checkout': 'workbench', 'task': 'Slow check', 'open_agent': True, 'wait_status': 'In review', 'wait_ms': 5000}, 200)
        check('real-B5', "The recovered task ends submitted with DONE.md in the helper's Changes", v.get('column') in ('In review', 'Done') and 'DONE.md' in (v.get('changes') or ''), {'column': v.get('column'), 'owner': v.get('owner'), 'changes': (v.get('changes') or '')[:200]})

        effort_step()
    except Done:
        pass
    finally:
        sweep('end')
        # What the gateway recorded: that a real provider answered, without any content.
        try:
            db = sqlite3.connect(f'file:{data}/philium.db?mode=ro', uri=True)
            answered = db.execute('select model, status, count(*) from requests group by 1, 2').fetchall()
            db.close()
            raw_results['requests'] = [{'model': m, 'status': s, 'count': n} for m, s, n in answered]
            print('-- requests: ' + ', '.join(f'{m} status {s} x{n}' for m, s, n in answered), flush=True)
        except sqlite3.Error as e:
            raw_results['requests_error'] = type(e).__name__
        save()
        token_copy.unlink(missing_ok=True)
        if not args.keep:
            shutil.rmtree(root, ignore_errors=True)
        bad = [x for x in rows if not x['ok']]
        print(f'{len(rows) - len(bad)} passed, {len(bad)} failed', flush=True)
    sys.exit(1 if bad else 0)


if __name__ == '__main__':
    main()
