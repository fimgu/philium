"""Synthetic counterparts for an acceptance run. Everything here is public or made up.

- Model: a loopback OpenAI-compatible endpoint with a deterministic brain. It answers the way a
  cooperative coordinator or worker would, using only the tools the product offered it.
- Origin: a loopback stand-in for the GitHub REST reads the Review form makes, so a moved head
  and an outage can be produced on demand. The product only talks to it from a disposable home.
- Observer: a loopback deployment observer in front of a tiny product service.
- Repositories: disposable Git checkouts, including a release graph with one cause per case.
"""
import hashlib, http.server, json, re, subprocess, threading, time
from pathlib import Path

GIT_ENV = {'PATH': '/usr/bin:/bin', 'HOME': '/var/empty', 'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_TERMINAL_PROMPT': '0'}


def git(cwd, *args, stdin=None):
    return subprocess.run(['/usr/bin/git', *args], cwd=cwd, env=GIT_ENV, input=stdin, capture_output=True, check=True).stdout.decode().strip()


def sha256(data):
    return hashlib.sha256(data if isinstance(data, bytes) else data.encode()).hexdigest()


def new_repo(path, origin=None):
    path.mkdir(parents=True)
    git(path, 'init', '-q', '-b', 'main')
    for k, v in [('user.name', 'Fixture'), ('user.email', 'fixture@example.invalid'), ('core.hooksPath', '/dev/null'), ('commit.gpgsign', 'false')]:
        git(path, 'config', k, v)
    if origin:
        git(path, 'remote', 'add', 'origin', f'https://github.com/{origin}.git')
    return path


def commit_all(path, message='fixture'):
    git(path, 'add', '-A')
    git(path, 'commit', '-q', '--allow-empty', '-m', message)
    return git(path, 'rev-parse', 'HEAD')


def write(path, files):
    for name, body in files.items():
        f = path / name
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_text(body if isinstance(body, str) else json.dumps(body))


def tree_commit(path, files, parents, message):
    """A commit with exactly these parents and these file contents on top of the first parent's tree."""
    git(path, 'read-tree', parents[0])
    for name, body in files.items():
        blob = git(path, 'hash-object', '-w', '--stdin', stdin=(body if isinstance(body, str) else json.dumps(body)).encode())
        git(path, 'update-index', '--add', '--cacheinfo', f'100644,{blob},{name}')
    tree = git(path, 'write-tree')
    args = ['commit-tree', tree, '-m', message]
    for p in parents:
        args += ['-p', p]
    oid = git(path, *args)
    git(path, 'read-tree', 'HEAD')
    return oid


def release_graph(path):
    """One release whose head shows four different causes, one per contract.

    root R -- target T (main)
    R -- A1 ------------- M30 (#30, contains #31) -- H (head: merge of M30 and B1, with its own edit)
    R -- F31 (#31) -- S2 --/                         /
    R -- B1 (#32) ----------------------------------/
    """
    ok = {'display.json': {'columns': ['name', 'status']}, 'merged.json': {'value': 1}, 'legacy.json': {'ok': False}}
    write(path, {**ok, 'README.md': 'fixture\n'})
    root = commit_all(path, 'root')
    target = tree_commit(path, {'README.md': 'target only\n'}, [root], 'target only')
    a1 = tree_commit(path, {'notes.txt': 'a1\n'}, [root], 'release branch')
    f31 = tree_commit(path, {'display.json': {'columns': ['name', 'state']}}, [root], 'child change')
    s2 = tree_commit(path, {'side.txt': 's2\n'}, [f31], 'follow-up')
    m30 = tree_commit(path, {'display.json': {'columns': ['name', 'state']}, 'side.txt': 's2\n'}, [a1, s2], 'merge feature')
    b1 = tree_commit(path, {'other.txt': 'b1\n'}, [root], 'other change')
    head = tree_commit(path, {'other.txt': 'b1\n', 'merged.json': {'value': 2}}, [m30, b1], 'integration')
    fixed = tree_commit(path, {'merged.json': {'value': 1}, 'display.json': {'columns': ['name', 'status']}}, [head], 'restore')
    for name, oid in [('target', target), ('head', head), ('fixed', fixed)]:
        git(path, 'branch', f'fixture-{name}', oid)
    return {'root': root, 'target': target, 'a1': a1, 'f31': f31, 's2': s2, 'm30': m30, 'b1': b1, 'head': head, 'fixed': fixed}


CONTRACTS = {'cases': [
    {'id': 'child-change', 'document': 'display.json', 'pointer': '/columns/1', 'expect': 'status'},
    {'id': 'merge-result', 'document': 'merged.json', 'pointer': '/value', 'expect': 1},
    {'id': 'old-defect', 'document': 'legacy.json', 'pointer': '/ok', 'expect': True},
    {'id': 'never-observed', 'document': 'absent.json', 'pointer': '/x', 'expect': 1},
]}

WHO = re.compile(r'Your agent name is "([^"]+)" \(role: `([^`]+)`\)')


# A checkout for the context probe: `Client` is used from three other files, `retry_backoff`
# is found by the words of its doc comment, and the test file names nothing worth listing.
NET_RS = 'pub struct Client { pub tries: u32 }\n\n/// Wait longer after each failed try.\npub fn retry_backoff(c: &Client) -> u64 {\n    1u64 << c.tries\n}\n'
CONTEXT_REPO = {'src/net.rs': NET_RS, 'src/net_test.rs': 'fn helper_for_tests() {}\n',
                **{f'src/{f}.rs': f'use crate::net::Client;\npub fn use_{f}(c: &Client) -> u32 {{ c.tries }}\n' for f in 'abc'}}


def text_of(m):
    c = m.get('content')
    if isinstance(c, str):
        return c
    if isinstance(c, list):
        return ' '.join(p.get('text', '') for p in c if isinstance(p, dict))
    return ''


def identity(req):
    msgs = req.get('messages', [])
    who = WHO.search(text_of(msgs[0])) if msgs and msgs[0].get('role') == 'system' else None
    return (who.group(1), who.group(2)) if who else ('desk', 'chat')


def brain(req):
    """Stateless: the plan is derived from the conversation the product sent."""
    msgs = req.get('messages', [])
    name, role = identity(req)
    users = [i for i, m in enumerate(msgs) if m.get('role') == 'user']
    last_user = text_of(msgs[users[-1]]) if users else ''
    results = [text_of(m) for m in msgs[(users[-1] + 1 if users else 0):] if m.get('role') == 'tool']
    step = len(results)
    one = lambda r: r[:160].replace('\n', ' ')

    m = re.search(r'DELEGATE "([^"]+)" TO (\w+)(?: THEN (SLOW \d+))?', last_user)
    if m:
        title, worker, slow = m.group(1), m.group(2), m.group(3)
        if step == 0:
            return {'tool': ('mcp_philium_board_create', {'title': title, 'detail': 'Synthetic acceptance task'})}
        if step == 1:
            tid = re.search(r'"id":\s*"([0-9a-f]+)"', results[0])
            if not tid:
                return f'DELEGATION FAILED creating the task: {one(results[0])}'
            work = slow or 'write the file status.txt containing the word status, commit it, then submit the task'
            return {'tool': ('mcp_philium_agent_spawn', {'name': worker, 'role': 'worker', 'task_id': tid.group(1), 'brief': f'WORKER TASK {tid.group(1)}: {work}.'})}
        if '"id"' not in results[1]:
            return f'DELEGATION FAILED starting the worker: {one(results[1])}'
        return f'DELEGATED "{title}" to {worker}.'
    # A release review: the orchestrator plans in tiers, hands one item to a helper, records the
    # two others itself and writes the report. The helper records its own item.
    if 'You are leading the review of release pull request' in last_user:
        ids = re.findall(r'"id":\s*"([0-9a-f]+)"', results[0]) if results else []
        if step == 0:
            return {'tool': ('mcp_philium_review_plan', {'items': [
                {'title': 'Migrations apply in order on an empty database', 'detail': 'Run the migration test against a fresh database.'},
                {'title': 'The list shows the status column', 'detail': 'Read view.json on the head and confirm the column.'},
                {'title': 'The notes mention the new column', 'detail': 'Read the README.'}]})}
        if len(ids) < 3:
            return f'REVIEW FAILED writing the plan: {one(results[0])}'
        if step == 1:
            return {'tool': ('mcp_philium_agent_spawn', {'name': 'finch', 'role': 'helper', 'task_id': ids[1], 'brief': f'REVIEW ITEM {ids[1]}: read view.json on the head and record the result with review_result.'})}
        if step == 2:
            return {'tool': ('mcp_philium_review_result', {'task_id': ids[0], 'result': 'pass', 'evidence': 'Ran the migration test on the head commit against an empty database: 3 passed, 0 failed.'})}
        ran = 'Read the README on the head and on the target: neither mentions any column. Same on the target.'
        if step == 3:
            return {'tool': ('mcp_philium_review_result', {'task_id': ids[2], 'result': 'fail', 'evidence': ran})}
        if step == 4:
            refused = 'needs its origin' in results[3]
            return {'tool': ('mcp_philium_review_result', {'task_id': ids[2], 'result': 'fail', 'severity': 4, 'origin': 'pre_existing', 'evidence': ran + (' Unlabelled first try refused.' if refused else ' Unlabelled first try accepted.')})}
        if step == 5:
            return {'tool': ('mcp_philium_review_report', {'markdown': '# Review of the release\n\nVerdict: ready for your sign-off. No P0 or P1 finding.\n\n| Severity | Finding | Origin |\n|---|---|---|\n| P4 | The notes never mention columns | pre-existing |\n\nPassed: migrations, the status column.'})}
        return 'REVIEW ORGANISED ' + one(results[-1])
    m = re.search(r'REVIEW ITEM ([0-9a-f]+):', last_user)
    if m:
        if step == 0:
            return {'tool': ('mcp_philium_review_result', {'task_id': m.group(1), 'result': 'pass', 'evidence': 'Read view.json on the head commit: the column is status, as the item expects.'})}
        return 'REVIEW ITEM DONE ' + one(results[0])
    if 'Draft release checks' in last_user:
        if step == 0:
            return {'tool': ('mcp_philium_checks_propose', {'contracts': {'cases': [
                {'id': 'view-column', 'why': 'The list keeps showing the status column', 'document': 'view.json', 'pointer': '/column', 'expect': 'status'},
                {'id': 'app-private', 'document': 'docs/app.json', 'pointer': '/public', 'expect': False}]}})}
        return 'PROPOSED CHECKS ' + one(results[0])
    if 'WHAT HAPPENED' in last_user:
        if step == 0:
            return {'tool': ('mcp_philium_agent_status', {})}
        if step == 1:
            return {'tool': ('mcp_philium_mail_read', {'unread_only': False})}
        return 'STATUS REPORT agents: ' + one(results[0]) + ' mail: ' + one(results[1])
    if 'PROBE ENVIRONMENT' in last_user:
        if step == 0:
            cmd = ('echo vars=$(env | grep -ciE "^(JCODE|HERDR)_"); echo tool=$(command -v jcode || echo none); echo home=$HOME; '
                   'for f in "$PROBE_REAL_HOME/.ssh/id_probe" "$PROBE_REAL_HOME/.jcode/secret" "$PROBE_VAULT"; do if cat "$f" >/dev/null 2>&1; then echo "READ $f"; else echo "DENIED $f"; fi; done; echo "path=$PATH"')
            cmd = cmd.replace('$PROBE_REAL_HOME', req.get('_probe_home', '')).replace('$PROBE_VAULT', req.get('_probe_vault', ''))
            return {'tool': ('mcp_philium_shell_run', {'command': cmd})}
        return 'PROBE RESULT ' + results[0][:3000]
    if 'CONTEXT PROBE' in last_user:
        # Look a concept up, change the same function as the other probe agent, then keep
        # making small tool calls until Philium says the two overlap, or give up.
        heard = next((r for r in results if 'your work now overlaps with' in r), None)
        if step == 0:
            return {'tool': ('mcp_philium_code_find', {'query': 'retry backoff'})}
        if step == 1:
            body = NET_RS.replace('    1u64 << c.tries', f'    // {name} waits one try longer\n    1u64 << (c.tries + 1)')
            return {'tool': ('mcp_philium_shell_run', {'command': f"cat > src/net.rs <<'EOF'\n{body}EOF\ngit diff --stat"})}
        if heard or step >= 18:
            found = 'retry_backoff' in results[0] and 'src/net.rs' in results[0]
            note = heard[heard.index('your work now overlaps with'):] if heard else results[-1]
            return f"CONTEXT RESULT {name}: find={'ok' if found else 'missed'} clash={'heard' if heard else 'not heard'} :: {one(note)}"
        return {'tool': ('mcp_philium_shell_run', {'command': 'sleep 6; echo waited'})}
    m = re.search(r'WORKER TASK ([0-9a-f]+): (SLOW (\d+))?', last_user)
    if m:
        tid = m.group(1)
        if m.group(2):
            if step == 0:
                return {'tool': ('mcp_philium_board_update', {'id': tid, 'note': 'Started the long step.'})}
            return {'delay': int(m.group(3)), 'text': f'WORKER RESULT task {tid}: long step finished.'}
        lead = re.search(r'Spawned by (\w[\w-]*)', last_user)
        plan = [
            ('write', {'path': 'status.txt', 'content': 'status\n'}),
            ('mcp_philium_shell_run', {'command': 'git add status.txt && git commit -qm "Add status" && git log --oneline -1'}),
            ('write', {'path': 'notes.txt', 'content': 'not added yet\n'}),
            ('mcp_philium_board_submit', {'id': tid, 'summary': 'Wrote status.txt and committed it.'}),
            ('mcp_philium_mail_send', {'to': lead.group(1) if lead else 'you', 'body': f'Task {tid} is submitted: status.txt committed.'}),
        ]
        if step < len(plan):
            return {'tool': plan[step]}
        return f'WORKER RESULT task {tid}: status.txt written, committed and submitted.'
    m = re.search(r'REDIRECT rename to (\S+)', last_user)
    if m:
        if step == 0:
            return {'tool': ('mcp_philium_shell_run', {'command': f'git mv status.txt {m.group(1)} && git commit -qm "Rename" && ls'})}
        return f'REDIRECTED: the file is now {m.group(1)}.'
    if 'RATE LIMITED' in last_user and step == 0:
        return {'status': 429}
    m = re.search(r'SLOW (\d+)', last_user)
    if m and step == 0:
        return {'delay': int(m.group(1)), 'text': 'SLOW TURN FINISHED'}
    return f'SYNTHETIC REPLY from {name} ({role}): {last_user[:60]}'


class Model(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    seen = []          # redacted: who asked, what it was told to do, how the stream ended
    prompts = {}       # agent name -> the system prompt of its first request
    expect_key = None
    probe = {}
    models = ['synthetic-1']   # in the provider's own order, newest first

    def log_message(self, *a):
        pass

    def _send(self, code, obj):
        b = json.dumps(obj).encode()
        self.send_response(code); self.send_header('Content-Type', 'application/json'); self.send_header('Content-Length', str(len(b))); self.end_headers(); self.wfile.write(b)

    def do_GET(self):
        if self.path.rstrip('/').endswith('/models'):
            return self._send(200, {'object': 'list', 'data': [{'id': m} for m in self.models]})
        self._send(404, {})

    def do_POST(self):
        raw = self.rfile.read(int(self.headers.get('content-length') or 0))
        try:
            req = json.loads(raw)
        except ValueError:
            return self._send(400, {})
        name, role = identity(req)
        users = [text_of(m) for m in req.get('messages', []) if m.get('role') == 'user']
        key = self.headers.get('authorization', '')
        req['_probe_home'] = Model.probe.get('home', ''); req['_probe_vault'] = Model.probe.get('vault', '')
        plan = brain(req)
        entry = {'t': time.time(), 'agent': name, 'role': role, 'turns': len(users), 'last_user': users[-1][:80] if users else '',
                 'key_ok': key == f'Bearer {Model.expect_key}' if Model.expect_key else None,
                 'history': ' | '.join(u[:40] for u in users[-4:]),
                 'plan': plan if isinstance(plan, str) else (plan.get('tool', [None])[0] or (f"status {plan['status']}" if plan.get('status') else f"slow {plan.get('delay')}")),
                 'tool_results': [text_of(m)[:900] for m in req.get('messages', []) if m.get('role') == 'tool'][-2:]}
        Model.seen.append(entry)
        if req.get('messages') and req['messages'][0].get('role') == 'system':
            Model.prompts.setdefault(name, text_of(req['messages'][0]))
        if isinstance(plan, dict) and plan.get('status'):
            # A refusal as a provider sends it, with the wait it asks for.
            b = json.dumps({'type': 'error', 'error': {'type': 'rate_limit_error', 'message': 'Synthetic rate limit for this check.'}}).encode()
            self.send_response(plan['status']); self.send_header('Content-Type', 'application/json'); self.send_header('retry-after', '2')
            self.send_header('Content-Length', str(len(b))); self.end_headers(); self.wfile.write(b)
            entry['ended'] = f"refused {plan['status']}"
            return
        if not req.get('stream'):
            return self._send(200, {'id': 'x', 'object': 'chat.completion', 'choices': [{'index': 0, 'message': {'role': 'assistant', 'content': plan if isinstance(plan, str) else ''}, 'finish_reason': 'stop'}]})
        self.send_response(200); self.send_header('Content-Type', 'text/event-stream'); self.send_header('Connection', 'close'); self.end_headers()

        def emit(obj):
            self.wfile.write(f'data: {json.dumps(obj)}\n\n'.encode()); self.wfile.flush()
        try:
            if isinstance(plan, str):
                plan = {'text': plan}
            for i in range(int(plan.get('delay', 0) * 4)):
                emit({'choices': [{'index': 0, 'delta': {'role': 'assistant', 'content': f'step {i} '}, 'finish_reason': None}]}); time.sleep(0.25)
            if plan.get('tool'):
                tool, args = plan['tool']
                emit({'choices': [{'index': 0, 'delta': {'role': 'assistant', 'tool_calls': [{'index': 0, 'id': f'call_{len(Model.seen)}', 'type': 'function', 'function': {'name': tool, 'arguments': json.dumps(args)}}]}, 'finish_reason': None}]})
                fin = 'tool_calls'
            else:
                emit({'choices': [{'index': 0, 'delta': {'role': 'assistant', 'content': plan.get('text', '')}, 'finish_reason': None}]})
                fin = 'stop'
            emit({'choices': [{'index': 0, 'delta': {}, 'finish_reason': fin}], 'usage': {'prompt_tokens': 20, 'completion_tokens': 5}})
            self.wfile.write(b'data: [DONE]\n\n'); self.wfile.flush()
            entry['ended'] = 'complete'
        except (BrokenPipeError, ConnectionResetError):
            entry['ended'] = 'client closed'


class SecondModel(Model):
    """A second trusted provider with several model families, for the model picker."""
    models = ['nova-3-2', 'nova-mini-3', 'spark-2-1', 'nova-3-1', 'nova-3', 'spark-2', 'spark-1', 'nova-2-5']


class Origin(http.server.BaseHTTPRequestHandler):
    """GitHub REST stand-in: pulls/N and commits/SHA/pulls for the fixture repositories."""
    protocol_version = 'HTTP/1.1'
    repos = {}          # 'owner/name' -> {'prs': {number: {'base':, 'heads': [first, later...]}}, 'landed': {commit: (number, merged)}}
    mode = 'ok'         # ok | down | members-down | move
    calls = []
    auth = []

    def log_message(self, *a):
        pass

    def _send(self, code, obj):
        b = json.dumps(obj).encode()
        self.send_response(code); self.send_header('Content-Type', 'application/json'); self.send_header('Content-Length', str(len(b))); self.end_headers(); self.wfile.write(b)

    def do_GET(self):
        path = self.path.split('?')[0]
        Origin.calls.append(path); Origin.auth.append(self.headers.get('authorization', ''))
        m = re.fullmatch(r'/repos/([^/]+/[^/]+)/(.+)', path)
        repo = Origin.repos.get(m.group(1)) if m else None
        if Origin.mode == 'down' or repo is None:
            return self._send(503, {'message': 'unavailable'})
        slug, rest = m.group(1), m.group(2)
        m = re.fullmatch(r'pulls/(\d+)', rest)
        if m and int(m.group(1)) in repo['prs']:
            pr = repo['prs'][int(m.group(1))]
            n = sum(1 for c in Origin.calls if c == path)
            head = pr['heads'][min(n - 1, len(pr['heads']) - 1)] if Origin.mode == 'move' else pr['heads'][0]
            return self._send(200, {'number': int(m.group(1)), 'html_url': f'https://github.com/{slug}/pull/{m.group(1)}', 'base': {'sha': pr['base'], 'repo': {'full_name': slug}}, 'head': {'sha': head}})
        m = re.fullmatch(r'commits/([0-9a-f]{40})/pulls', rest)
        if m:
            if Origin.mode == 'members-down':
                return self._send(503, {'message': 'unavailable'})
            hit = repo['landed'].get(m.group(1))
            rows = [] if not hit else [{'number': hit[0], 'merged_at': '2026-01-01T00:00:00Z' if hit[1] else None, 'merge_commit_sha': m.group(1) if hit[1] else None, 'base': {'repo': {'full_name': slug}}}]
            return self._send(200, rows)
        self._send(404, {'message': 'not found'})


class Observer(http.server.BaseHTTPRequestHandler):
    """Deployment observer plus the product path it fronts. `state` is set by the harness."""
    protocol_version = 'HTTP/1.1'
    state = {}          # revision, artifact, body (dict), mode: ok | wrong | fail
    calls = []

    def log_message(self, *a):
        pass

    def do_GET(self):
        s = Observer.state
        Observer.calls.append((time.time(), self.path))
        if s.get('mode') == 'down':
            self.send_error(503); return
        if self.path == '/.well-known/philium/deployment':
            body = {'observed_ms': int(time.time() * 1000), 'complete': True, 'replicas': [{'ready': True, 'revision': s.get('wrong_revision') if s.get('mode') == 'wrong' else s['revision'], 'digest': s['artifact']}]}
        elif self.path in s.get('paths', {}):
            body = s['paths'][self.path] if s.get('mode') != 'fail' else s.get('fail_body', {})
        else:
            self.send_error(404); return
        b = json.dumps(body).encode()
        self.send_response(200); self.send_header('Content-Type', 'application/json'); self.send_header('Content-Length', str(len(b)))
        self.send_header('X-Philium-Revision', s['revision']); self.send_header('X-Philium-Artifact', s['artifact']); self.end_headers(); self.wfile.write(b)


def serve(handler, port=0):
    srv = http.server.ThreadingHTTPServer(('127.0.0.1', port), handler)
    srv.daemon_threads = True
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv


MYCLI_STANDIN = r"""#!/bin/sh
# Stand-in for a hosted environment's CLI. State lives under the caller's HOME, the way a real
# client keeps its login there. Every call is logged so the harness can see exactly what ran.
S="$HOME/.mycli-fixture"
printf '%s\n' "$*" >> "$S/calls.log"
case "$1" in
  whoami) [ -f "$S/signed-in" ] || { echo "not logged in" >&2; exit 1; }; printf 'Login: fixture@example.invalid\nContext: staging\n';;
  exec)
    [ -f "$S/signed-in" ] || { echo "not logged in" >&2; exit 1; }
    case "$*" in
      *"k8s:fixture"*"kubectl get pods"*) cat "$S/pods.json";;
      *"pg:fixture"*"count(*)"*) printf ' count \n-------\n     %s\n(1 row)\n' "$(cat "$S/count")";;
      *) echo "unknown endpoint" >&2; exit 1;;
    esac;;
  *) echo "CHANGE ATTEMPTED: $*" >> "$S/calls.log"; exit 2;;
esac
"""

KUBECTL_STANDIN = r"""#!/bin/sh
# Stand-in for kubectl against a local kind cluster.
S="$HOME/.kube-fixture"
printf '%s\n' "$*" >> "$S/calls.log"
case "$*" in
  "--context kind-fixture -n app get pods -o json") cat "$S/pods.json";;
  "--context kind-fixture get --raw /api/v1/namespaces/app/services/app:8080/proxy/api/health") cat "$S/health.json";;
  *) echo "CHANGE ATTEMPTED: $*" >> "$S/calls.log"; exit 2;;
esac
"""


def pods(revision, digest, ready=True, count=2):
    pod = {'metadata': {'annotations': {'philium.dev/revision': revision}}, 'status': {'containerStatuses': [{'imageID': f'registry.example.invalid/app@{digest}', 'ready': ready}]}}
    return json.dumps({'items': [pod] * count})


# Declares the stand-in CLI read-only, the way a person would in their data dir.
MYCLI_CLIENTS = {'clients': {'mycli': {'reads': ['whoami'], 'exec': {'verb': 'exec', 'value_options': ['-x']}}}}


def target_contract():
    ident = {'replicas': '/items', 'revision': '/metadata/annotations/philium.dev~1revision', 'digest': '/status/containerStatuses/0/imageID', 'ready': '/status/containerStatuses/0/ready'}
    return {'release_records': True,
            'cases': [{'id': 'source', 'document': 'view.json', 'pointer': '/column', 'expect': 'status'}],
            'targets': [
                {'name': 'env01',
                 'identity': {'program': 'mycli', 'args': ['exec', '-x', 'staging', 'k8s:fixture', '--', 'kubectl', 'get', 'pods', '-o', 'json'], **ident},
                 'cases': [{'id': 'env01-login', 'program': 'mycli', 'args': ['whoami'], 'expect_contains': ['Context: staging']},
                           {'id': 'env01-count', 'program': 'mycli', 'args': ['exec', '-x', 'staging', 'pg:fixture', '-c', 'SELECT count(*) FROM employees'], 'expect_text': 'count ------- 5 (1 row)'}]},
                {'name': 'local',
                 'identity': {'program': 'kubectl', 'args': ['--context', 'kind-fixture', '-n', 'app', 'get', 'pods', '-o', 'json'], **ident},
                 'cases': [{'id': 'local-health', 'program': 'kubectl', 'args': ['--context', 'kind-fixture', 'get', '--raw', '/api/v1/namespaces/app/services/app:8080/proxy/api/health'], 'pointer': '/status', 'expect': 'ok'}]},
            ]}
