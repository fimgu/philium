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
    'release_intake.members': { landed: [5519, 5523, 5527, 5531, 5536, 5540].map((pr, i) => ({ pr, oid: (i + 3).toString(16).repeat(40) })), edges: [{ outer: 5536, inner: 5527 }], missing: 0, unmapped: 1, ambiguous: 0 },
    'workspace.overview': { resources: [{ kind: 'worktree', handle: '~/Philium Pilot/workspace/worktrees/example-repo/lead-1a2b3c', state: 'live', agent_name: 'lead' }, { kind: 'port_block', handle: '41000-41019', state: 'live', agent_name: 'scribe' }], tasks: [], agents: [] },
    'environments.profiles': ['default'],
    'environments.containers': [{ id: 'abc123def4567890', name: 'kind-control-plane', status: 'Up 3 hours', image: 'kindest/node:v1.31.0', cluster: 'dev' }, { id: 'bcd234efa5678901', name: 'pg-test', status: 'Up 3 hours', image: 'postgres:16', project: 'example-repo' }, { id: 'cde345fab6789012', name: 'redis-test', status: 'Exited (0) 2 days ago', image: 'redis:7', project: 'example-repo' }],
  };
  const reply = (method) => (method in data ? data[method] : /list$|_list$|history$|names$|jobs$|drivers$|models$/.test(method) ? [] : null);
  window.__TAURI_INTERNALS__ = {
    invoke: async (cmd, args) => (cmd === 'rpc' ? structuredClone(reply(args.method)) : cmd.startsWith('plugin:event') ? 1 : cmd === 'drafts_load' ? { text: '', images: [] } : null),
    transformCallback: () => Math.floor(Math.random() * 1e9), metadata: { currentWindow: { label: 'main' }, currentWebview: { label: 'main', windowLabel: 'main' } }, convertFileSrc: (p) => p,
  };
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const type = (el, v) => { Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(el, v); el.dispatchEvent(new Event('input', { bubbles: true })); };
  window.addEventListener('load', async () => {
    await sleep(900);
    Array.from(document.querySelectorAll('button.nav-item')).find((b) => b.textContent.trim() === screen)?.click();
    await sleep(900);
    if (screen === 'Review' && q.get('verify') !== '0') {
      const url = document.querySelector('form input[type="url"]'); if (url) { type(url, 'https://github.com/acme/example-repo/pull/5542'); url.closest('form').querySelector('button').click(); }
      await sleep(900);
      if (q.get('open')) document.querySelectorAll(q.get('open')).forEach((d) => { d.open = true; });
    }
    if (screen === 'Environments' && q.get('machine')) { Array.from(document.querySelectorAll('button')).find((b) => b.textContent.trim() === 'Inspect local machine')?.click(); await sleep(900); }
    const body = document.querySelector('.page-body'); if (q.get('y') && body) body.scrollTop = Number(q.get('y'));
    document.title = 'ready';
  });
})();
