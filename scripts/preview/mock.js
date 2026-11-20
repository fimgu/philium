// Sample data for looking at screens outside the app. Nothing here is real.
(() => {
  const q = new URLSearchParams(location.search), screen = q.get('screen') || 'Review', state = q.get('state') || 'incomplete';
  localStorage.setItem('philium.activeRepo', 'r1');
  const now = Date.now(), H = 'a3f9c1d2e4b5a6978877665544332211ffeeddcc', B = '5b1e0c9d8a7f6e5d4c3b2a1908f7e6d5c4b3a291';
  const cases = [
    { id: 'api-git-key-rotation', state: 'candidate_pass' }, { id: 'api-git-cli-signing', state: 'candidate_pass' },
    { id: 'github-app-permissions', state: state === 'bad' ? 'release_range_regression' : 'candidate_pass', landed_evidence: state === 'bad' ? [{ pr: 5531, oid: 'c0ffee1234567890', result: 'same_failure_as_candidate' }] : [] },
    { id: 'mapping-one-account-per-user', state: 'candidate_pass', new_at_head: true }, { id: 'mapping-is-per-workspace', state: 'candidate_pass' },
  ];
  const full = state === 'ready';
  const receipt = {
    pr: 5542, head: H, target: B, cases, live_cases: full ? [{ id: 'approvals-list', state: 'pass', target: 'staging' }, { id: 'health-version', state: 'pass', target: 'staging' }] : [],
    targets: full ? [{ name: 'staging', identity_before: { state: 'match' }, identity_after: { state: 'match' } }] : [],
    observer_configured: false, release_records: true, driver_pin: '0ae9e53c71d2', release_record_pin: full ? '0ae9e53c71d2' : null,
    review_attestation: full ? { reviewer: 'filip' } : null, evidence_status: full ? { 'api-git-key-rotation': 'verified', 'approvals-list': 'verified' } : {},
    checks_confirmed_ms: now - 3600e3, release_decision: 'observed', artifact: full ? 'sha256:9f2c41d0aa7b' : null,
  };
  const decision = { pr: 5542, generation: 2, head: H, base: B, artifact: receipt.artifact, checked_ms: now - 120e3, origin_current: true,
    mergeable: state === 'bad' ? { state: 'conflicts', count: 4, files: ['example-repo-app/cli/cmd/main.go', 'example-repo-app/cli/cmd/cli/connect.go', 'example-repo-app/cli/cmd/plugins_test.go', 'example-repo-app/internal/entrypoint/background.go'] } : { state: 'clean' },
    decision: { ready: 'ready_for_reviewed_scope', bad: 'not_ready', incomplete: 'incomplete' }[state], contract_decision: state === 'bad' ? 'regression_in_release' : 'no_regression', receipt,
    why: state === 'ready' ? [] : state === 'bad'
      ? ['github-app-permissions fails on this head and passes on the target: a regression inside this release. Landed #5531 fails the same way on its own.']
      : ['No independent artifact record for this exact head.', 'No running-product checks are pinned, so nothing was observed on a deployment.', 'No recorded review for this exact head.'] };
  const data = {
    'workspace.repos_list': [{ id: 'r1', name: 'example-repo', path: '/Users/you/Developer/example-repo', remote_url: 'https://github.com/acme/example-repo.git', default_branch: 'release/v1.1.x' }],
    'app.settings_get': 'r1', 'app.status': { version: '0.1.0', pilot: true }, 'providers.list': [], 'providers.models': [], 'providers.default': null, 'agents.list': [],
    'workday.summary': { started: now - 5 * 3600e3, completed: 37, tools: ['Jcode', 'Herdr', 'Manual database edit', 'Assistant intervention', 'Other'],
      fallbacks: [{ tool: 'Jcode', ts: now - 2 * 3600e3, note: 'Needed the old review gate for a hotfix' }, { tool: 'Manual database edit', ts: now - 40 * 60e3, note: 'Reset a stuck task' }],
      actions: [['agents.send', 'Sent a message to an agent', 'example-repo', 14, 0], ['agents.spawn', 'Started an agent', 'example-repo', 4, 0], ['board.assign', 'Handed a task to an agent', 'example-repo', 6, 0], ['release_intake.verify_url', 'Verified a release', 'example-repo', 3, 1], ['agents.stop', 'Stopped an agent', 'exo', 2, 0], ['terminals.open', 'Opened a terminal', '', 8, 0]]
        .map(([method, action, checkout, completed, failed], i) => ({ method, action, checkout, completed, failed, last: now - (i + 1) * 17 * 60e3 })) },
    'release_intake.sql_profile': { driver: 'v1', driver_name: 'philium-verifier-example-repo', pin: '0ae9e53c71d2aa41', confirmed: now - 3600e3, observer_port: null, programs: [], checks: { pin: '0ae9e53c71d2aa41', cases: cases.map((c) => ({ id: c.id, kind: 'document', what: 'docs/github-app-manifest.json /default_permissions/contents', expect: 'read' })), live_cases: [], targets: [], programs: [], release_records: true } },
    'release_intake.origin_access': { token_saved: true }, 'release_intake.sql_drivers': [{ id: 'v1abcdef1234', name: 'philium-verifier-example-repo' }],
    'release_intake.history': [{ seq: 2, pr: 5542, observed: now - 130e3, base: B, head: H }, { seq: 1, pr: 5542, observed: now - 26 * 3600e3, base: B, head: '77aa88bb99cc00dd11ee22ff33445566778899aa' }],
    'release_intake.verify_url': decision, 'release_intake.decision': decision, 'release_intake.sql_receipt': receipt,
    'release_intake.graph': { head: H, target: B, merge_base: B, commits: Array.from({ length: 6 }, (_, i) => ({ oid: (i + 10).toString(16).repeat(20), parents: i % 3 ? ['x'] : ['x', 'y'] })) },
    'release_intake.followup': { mode: 'full', reason: 'No code review is recorded for this pull request yet.', commits: [] },
    'release_intake.drafter': q.get('review') === 'failed' ? { id: 'd1', name: 'otter', state: 'failed', started: false, error: 'Agents here start from release/v0.9.0, and this checkout has no such branch or commit, locally or from origin. Fetch it in the checkout (git fetch origin release/v0.9.0), or choose another starting branch for this checkout under Repos.' } : null,
    'pipeline.results': q.get('checks') === 'none' ? { configured: false, local_gate: '~/Philium Pilot/data/pipelines/example-org-example-repo/.philium/pipeline.yaml' } : { configured: true, pr: 5542, head: H, base: B, from: 'local pipeline.yaml', pinned_gate: false, running: false, lines: [],
      verdict: q.get('checks') === 'waiting'
        ? { state: 'blocked', pinned: false, reasons: ['go-test-touched is flaky (failed, then passed); a person must quarantine it', 'migrations-postgres fails on the base too, so this change did not cause it; link the issue that tracks it'],
          outcomes: [['gofmt-changed', { outcome: 'pass' }], ['go-test-touched', { outcome: 'flaky' }], ['migrations-postgres', { outcome: 'pre_existing' }]],
          waiting: [{ check: 'go-test-touched', kind: 'quarantine', text: 'go-test-touched failed, then passed when run again.' }, { check: 'migrations-postgres', kind: 'known_issue', text: 'migrations-postgres fails on the base too.' }] }
        : { state: 'fail', pinned: false, reasons: ['go-test-touched fails on this change and passes on the base'],
          outcomes: [['gofmt-changed', { outcome: 'pass' }], ['go-test-touched', { outcome: 'introduced' }], ['migrations-postgres', { outcome: 'pre_existing' }]], waiting: [] },
      decisions: q.get('checks') === 'waiting' ? { known_issues: [], quarantined: [] } : { known_issues: [{ check_id: 'migrations-postgres', issue: 'example-org/example-repo#5601', by: 'you', at: now - 600e3 }], quarantined: [] },
      checks: [
        { id: 'gofmt-changed', candidate: { status: 'pass', duration_ms: 420 }, output: '12 changed Go files are gofmt-clean' },
        { id: 'go-test-touched', candidate: { status: 'fail', duration_ms: 111000 }, base: { status: 'pass' }, output: '--- FAIL: TestHTTPServerHeaderValidation (0.02s)\nFAIL', base_output: 'ok  internal/handler 3.1s' },
        { id: 'migrations-postgres', candidate: { status: 'fail', duration_ms: 64000 }, base: { status: 'fail' }, output: '--- FAIL: TestManualMigrationSchemaAndHistoryFailuresPostgres', base_output: '--- FAIL: TestManualMigrationSchemaAndHistoryFailuresPostgres' },
      ] },
    'release_intake.review_items': q.get('review') === 'failed' ? { pr: 5542, headline: 'none', run: { pr: 5542 }, tiers: [], items: [], report: null,
      lead: { id: 'a1', name: 'falcon', state: 'failed', started: false, error: 'Agents here start from release/v0.9.0, and this checkout has no such branch or commit, locally or from origin. Fetch it in the checkout (git fetch origin release/v0.9.0), or choose another starting branch for this checkout under Repos.' } }
      : q.get('review') ? { pr: 5542, headline: q.get('review'), run: { pr: 5542 },
      tiers: ['can lose data, break security or stop the release from installing', 'the main behaviour each pull request changes', 'neighbouring behaviour that could regress', 'error handling and edge cases', 'configuration, deployment files and documentation', 'polish'].map((means, i) => ({ tier: 'P' + i, means })),
      items: [
        [0, 'Migrations 59 to 64 apply in order on an empty database', 'done', 'pass', 'heron: Ran make test-integration on a798d73 against a fresh Postgres: 9 migrations applied, 0 errors.'],
        [0, 'Conflicts with release/v1.1.x', 'done', 'fail', 'lead: Introduced by #5501: git merge-tree on a798d73 onto 1bc788d: conflicts in 4 files under example-repo-app/cli and internal/entrypoint.', null, 'introduced'],
        [2, 'Audit log export keeps its column order', 'done', 'fail', 'heron: go test ./internal/audit/... fails TestExportOrder on a798d73 and the same way on the target 1bc788d.', null, 'pre_existing'],
        [1, '#5501 git access: key rotation and CLI signing endpoints', 'done', 'pass', 'finch: Ran go test ./internal/gitaccess/... on a798d73: 212 passed. Both endpoints are in swagger.json.'],
        [1, '#5546 image overrides file is read by the chart', 'claimed', null, null, 'wren'],
        [1, '#5556 CLI session survives an interruption', 'done', 'blocked', 'ibis: The test needs a running session service; none is available in the sandbox. Needs a local cluster or staging.'],
        [2, 'Existing database connections still open after #5544', 'open', null, null],
        [4, 'Helm values document the new image override keys', 'open', null, null],
      ].map(([tier, title, status, result, evidence, owner, origin], i) => ({ id: 't' + i, title, status, result, evidence, owner: owner || null, origin: origin || null, severity: result === 'fail' ? tier : null })),
      report: q.get('review') === 'do_not_ship' ? { by: 'lead', at: now, text: '# Review of release #5542\n\n**Verdict: do not ship.** One P0 item failed: the head conflicts with the target in 4 files.\n\n| Tier | Item | Result |\n|---|---|---|\n| P0 | Migrations 59 to 64 | pass |\n| P0 | Conflicts with the target | fail |\n| P1 | #5501 git access | pass |\n| P1 | #5556 CLI session | blocked |\n\nCould not be checked: #5556 needs a running session service.' } : null } : null,
    'release_intake.coverage': { check_files: 3, prs: [5519, 5523, 5527, 5531, 5536, 5540].map((pr, i) => ({ pr, files: 12 + i * 9, read: i < 2 ? ['docs/github-app-manifest.json'] : [] })), untouched: state === 'ready' ? [] : [5527, 5531, 5536, 5540] },
    'release_intake.members': { landed: [5519, 5523, 5527, 5531, 5536, 5540].map((pr, i) => ({ pr, oid: (i + 3).toString(16).repeat(40) })), edges: [{ outer: 5536, inner: 5527 }], missing: 0, unmapped: 1, ambiguous: 0 },
    'workspace.overview': { resources: [{ kind: 'worktree', handle: '~/Philium Pilot/workspace/worktrees/example-repo/lead-1a2b3c', state: 'live', agent_name: 'lead' }, { kind: 'port_block', handle: '41000-41019', state: 'live', agent_name: 'scribe' }], tasks: [], agents: [] },
    'pipeline.replay_history': (() => {
      const bugs = ['autoimages-gofmt', 'request-email-escape', 'mcp-token-client-credentials', 'request-type-edit-counts', 'migration-lock-connection', 'timed-request-approvers'];
      const plan = [[3, 5], [4, 5], [4, 5], [5, 6]];
      const runs = plan.map(([caught, total], i) => ({ id: 'r' + i, digest: ['3f9a01c2', '3f9a01c2', '77b0e4d1', 'c41d9e02'][i], at: now - (plan.length - i) * 26 * 3600e3, caught, total, rate: caught / total, rejected_below: null }));
      return { runs, bar: { rate: 5 / 6 }, running: q.get('quality') === 'running', lines: q.get('quality') === 'running' ? ['measuring the checks from local pipeline.yaml against 6 real past bugs', 'autoimages-gofmt: gofmt-changed on aca60825a9d5', 'autoimages-gofmt: caught'] : [], error: null, list_path: '~/Philium Pilot/data/pipelines/example-org-example-repo/.philium/replay.yaml',
        listed: bugs.map((id, i) => ({ id, check: i === 0 ? 'gofmt-changed' : i === 4 ? 'migrations-postgres' : 'go-test-touched', text: ['autoimages.go is not gofmt-clean', 'Custom request emails did not escape user-controlled text', 'The MCP token endpoint advertised client_credentials', 'Request type edits counted closed requests', 'The migration lock check could panic or create a stray table', 'Timed requests ignored the JIT endpoint approvers'][i] })),
        bugs: bugs.map((id, i) => ({ id, check: '', results: runs.filter((r, j) => i < 5 || j === 3).map((r, j) => ({ run: r.id, caught: i === 4 ? false : i === 3 ? j > 0 : true, reason: i === 4 ? 'failed, but fail on the fix too and the failure does not show "FAIL: TestMigrationLockPostgres"' : 'caught: the control passed' })) })) };
    })(),
    'reaper.ledger': [], 'reaper.proposals': [],
    'storage.status': { parts: [
        ['database', 'Records', 0.21e9, 'Agents, reviews, checks and the code index. Space is given back at start when much of it is free.'],
        ['worktrees', 'Agent worktrees', 6.78e9, 'One per agent, removed with the agent (Agents, Remove finished).'],
        ['agents', 'Agent folders', 0.39e9, "Each agent's own folder. Its temp folder is emptied when Philium starts; the folder goes with the agent."],
        ['check_runs', 'Check runs', 0, "Each run's own copy of the repo, deleted when the run ends."],
        ['check_caches', 'Check caches', 2.4e9, "Build caches for repo checks. Go's are kept under the budget, oldest files first."],
        ['mirrors', 'Repo mirrors', 1.1e9, "One copy of each repo's history for checks, fetched as needed."],
        ['agent_caches', 'Agent build caches', 8.1e9, "Shared by every agent. Go's are kept under the budget, oldest files first."],
      ].map(([key, label, bytes, rule]) => ({ key, label, bytes, rule, partial: false })),
      disk: { free: q.get('disk') === 'low' ? 6.2e9 : 145e9, total: 994e9, min_free_for_checks: 10737418240 }, budget_gb: 10,
      last: { at: now - 3 * 60e3, text: 'Freed 12.9 GB', steps: ['Removed 1 folder left by check runs that ended without cleaning up', 'Emptied the temp folder of 1 stopped agent', 'Trimmed Go build caches from 17.1 GB to 8.0 GB (budget 10.0 GB)'] } },
    'environments.profiles': ['default'],
    'environments.containers': [{ id: 'abc123def4567890', name: 'kind-control-plane', status: 'Up 3 hours', image: 'kindest/node:v1.31.0', cluster: 'dev' }, { id: 'bcd234efa5678901', name: 'pg-test', status: 'Up 3 hours', image: 'postgres:16', project: 'example-repo' }, { id: 'cde345fab6789012', name: 'redis-test', status: 'Exited (0) 2 days ago', image: 'redis:7', project: 'example-repo' }],
  };
  // setup=none: a checkout with no checks yet and one proposal from an agent.
  if (q.get('setup') === 'none') {
    data['release_intake.sql_profile'] = null;
    data['release_intake.sql_proposals'] = [{ id: 1, agent: 'heron', created: now - 60e3, checks: { cases: [
      { id: 'github-app-private', why: 'The GitHub app stays private to your organisation', kind: 'committed file', what: 'docs/github-app-manifest.json at /public', expect: false },
      { id: 'mapping-one-account-per-user', why: 'One GitHub account maps to exactly one user', kind: 'database fact', what: "SELECT github_user_id FROM github_user_mappings WHERE user_id='u1'", expect: [[100]] }], live_cases: [], targets: [], programs: [], release_records: false } }];
  }
  const reply = (method) => (method in data ? data[method] : /list$|_list$|history$|names$|jobs$|drivers$|models$/.test(method) ? [] : null);
  window.__TAURI_INTERNALS__ = {
    invoke: async (cmd, args) => (cmd === 'rpc' && (window.__calls = window.__calls || []).push(args.method), cmd === 'rpc' ? structuredClone(reply(args.method)) : cmd.startsWith('plugin:event') ? 1 : cmd === 'drafts_load' ? { text: '', images: [] } : null),
    transformCallback: () => Math.floor(Math.random() * 1e9), metadata: { currentWindow: { label: 'main' }, currentWebview: { label: 'main', windowLabel: 'main' } }, convertFileSrc: (p) => p,
  };
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const type = (el, v) => { Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(el, v); el.dispatchEvent(new Event('input', { bubbles: true })); };
  window.addEventListener('load', async () => {
    await sleep(900);
    Array.from(document.querySelectorAll('button.nav-item')).find((b) => b.textContent.trim() === screen)?.click();
    await sleep(900);
    if (screen === 'Review' && q.get('verify') === '0' && q.get('open')) document.querySelectorAll(q.get('open')).forEach((d) => { d.open = true; });
    if (screen === 'Review' && q.get('verify') !== '0') {
      const url = document.querySelector('form input[type="url"]'); if (url) { type(url, 'https://github.com/acme/example-repo/pull/5542'); url.closest('form').querySelector('button').click(); }
      await sleep(900);
      if (q.get('open')) document.querySelectorAll(q.get('open')).forEach((d) => { d.open = true; });
    }
    if (screen === 'Environments' && q.get('machine')) { Array.from(document.querySelectorAll('button')).find((b) => b.textContent.trim() === 'Inspect local machine')?.click(); await sleep(900); }
    // click=<button text>: press it, then show which backend calls the page made, for debugging.
    if (q.get('click')) {
      const before = (window.__calls || []).length;
      Array.from(document.querySelectorAll('button')).find((b) => b.textContent.trim() === q.get('click'))?.click();
      await sleep(900);
      const note = document.createElement('pre'); note.style.cssText = 'position:fixed;right:8px;bottom:8px;z-index:9999;background:#000;color:#0f0;padding:8px;font-size:11px;max-width:520px;white-space:pre-wrap';
      note.textContent = 'calls after click: ' + (window.__calls || []).slice(before).join(', ') + '\nstatus: ' + Array.from(document.querySelectorAll('p[role="status"]')).map((p) => p.textContent).join(' | ');
      document.body.appendChild(note);
    }
    const body = document.querySelector('.page-body'); if (q.get('y') && body) body.scrollTop = Number(q.get('y'));
    document.title = 'ready';
  });
})();
