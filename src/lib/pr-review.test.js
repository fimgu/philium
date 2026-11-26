import test from 'node:test';
import assert from 'node:assert/strict';
import { tally, tallyLine, groupByKind, sortWorstFirst, weight, verdictLabel, verdictTone, itemMark, verifyLine, originLabel, severityLabel, basisLabel, coverageLine } from './pr-review.js';

const items = () => [
  { kind: 'claim', status: 'open', title: 'claim 1' },
  { kind: 'risk', status: 'fail', severity: 1, origin: 'introduced', verify: 'confirmed', verifier: 'heron', title: 'risk 1' },
  { kind: 'thread', status: 'not_addressed', human: 1, title: 'thread 1' },
  { kind: 'ci', status: 'pass', title: 'ci 1' },
  { kind: 'note', status: 'fail', title: 'note 1', verify: 'needed' },
  { kind: 'comment', status: 'addressed', title: 'comment 1' },
];

test('tally counts done items and per-status counts', () => {
  const t = tally(items());
  assert.equal(t.total, 6);
  assert.equal(t.done, 5); // all but the open claim
  assert.equal(t.counts.pass, 1);
  assert.equal(t.counts.fail, 2);
});

test('tallyLine names severities for fails and plain labels otherwise', () => {
  const line = tallyLine(items());
  assert.match(line, /^6 items:/);
  assert.match(line, /1 P1/);
  assert.match(line, /1 Pass/);
  assert.equal(tallyLine([]), 'No items yet.');
});

test('sortWorstFirst puts needing-verification and fails before pass/addressed', () => {
  const sorted = sortWorstFirst(items());
  assert.equal(sorted[0].title, 'risk 1'); // fail, severity 1
  const last = sorted[sorted.length - 1];
  assert.ok(['comment 1', 'ci 1'].includes(last.title));
});

test('weight orders lower severity fails before higher severity ones', () => {
  const p1 = { status: 'fail', severity: 1 };
  const p4 = { status: 'fail', severity: 4 };
  assert.ok(weight(p1) < weight(p4));
});

test('groupByKind groups comment/review/thread together, labelled, sorted worst first, drops empty groups', () => {
  const groups = groupByKind(items());
  const labels = groups.map((g) => g.label);
  assert.deepEqual(labels, ['Changes & risks', 'Comments & threads', 'Description claims', 'CI', 'Your notes']);
  const commentsGroup = groups.find((g) => g.label === 'Comments & threads');
  assert.equal(commentsGroup.items.length, 2);
  assert.equal(commentsGroup.items[0].title, 'thread 1'); // not_addressed before addressed
});

test('verdictLabel/verdictTone reflect posted state over the stored verdict', () => {
  assert.equal(verdictLabel(null), 'Pending');
  assert.equal(verdictLabel({ verdict: 'needs_you' }), 'Needs you');
  assert.equal(verdictLabel({ verdict: 'approve', state: 'posted' }), 'Posted');
  assert.equal(verdictTone({ verdict: 'request_changes' }), 'bad');
  assert.equal(verdictTone({ verdict: 'approve', state: 'posted' }), 'ok');
});

test('itemMark gives a glyph and tone per status, blocked explains what is needed', () => {
  assert.deepEqual(itemMark({ status: 'pass' }).slice(0, 2), ['ok', '✓']);
  assert.deepEqual(itemMark({ status: 'fail' }).slice(0, 2), ['bad', '✕']);
  const blocked = itemMark({ status: 'blocked', detail: 'needs a token' });
  assert.equal(blocked[0], 'warn');
  assert.match(blocked[2], /needs a token/);
});

test('verifyLine and origin/severity labels only show for relevant items', () => {
  assert.equal(verifyLine({ verify: 'needed' }), 'Needs verification');
  assert.equal(verifyLine({ verify: 'confirmed', verifier: 'heron' }), 'Confirmed by heron');
  assert.equal(verifyLine({ verify: 'refuted' }), 'Refuted');
  assert.equal(verifyLine({ verify: 'none' }), '');
  assert.equal(originLabel({ status: 'fail', origin: 'pre_existing' }), 'pre-existing');
  assert.equal(originLabel({ status: 'pass' }), '');
  assert.equal(severityLabel({ status: 'fail', severity: 2 }), 'P2');
  assert.equal(severityLabel({ status: 'pass' }), '');
});

test('basisLabel chips how an item was established, empty when unset', () => {
  assert.equal(basisLabel({ basis: 'runtime' }), 'ran it');
  assert.equal(basisLabel({ basis: 'source_trace' }), 'read code');
  assert.equal(basisLabel({ basis: 'runtime_and_source' }), 'ran + read');
  assert.equal(basisLabel({}), '');
  assert.equal(basisLabel(null), '');
});

test('coverageLine reports counts per kind and warns when fetched < total or errors exist', () => {
  const full = { comments: { fetched: 21, total: 21 }, reviews: { fetched: 1, total: 1 }, threads: { fetched: 4, total: 4 }, checks: { fetched: 12, total: 12 } };
  const ok = coverageLine(full, []);
  assert.equal(ok.text, 'Read 21/21 comments, 1/1 reviews, 4/4 threads, 12/12 checks');
  assert.equal(ok.warn, false);
  const partial = { ...full, threads: { fetched: 2, total: 4 } };
  assert.equal(coverageLine(partial, []).warn, true);
  assert.equal(coverageLine(full, ['fetch failed']).warn, true);
  assert.equal(coverageLine(null), null);
  assert.equal(coverageLine({}), null);
});

import * as P from './pr-review.js';
test('policy: ceiling, allowed, effective', () => {
  assert.equal(P.modeCeiling(null), 'enforce');
  assert.equal(P.modeCeiling({ source: 'default', mode: 'observe' }), 'enforce');
  assert.equal(P.modeCeiling({ source: 'invalid', mode: 'observe', error: 'x' }), 'observe');
  assert.equal(P.modeCeiling({ source: 'base:abc', mode: 'comment' }), 'comment');
  assert.equal(P.modeAllowed('enforce', 'comment'), false);
  assert.equal(P.modeAllowed('observe', 'comment'), true);
  assert.equal(P.effectiveMode('comment', 'enforce'), 'comment');
  assert.equal(P.effectiveMode('enforce', 'observe'), 'observe');
  assert.equal(P.effectiveMode('comment', null), 'comment');
});
test('policy: labels and budget', () => {
  assert.equal(P.policySourceLabel({ source: 'base:0123456789ab' }), 'Base 0123456');
  assert.equal(P.policySourceLabel({ source: 'invalid' }), 'Invalid policy file');
  assert.deepEqual(P.budgetLine(1200000, 2000000), { text: '1.2M of 2M tokens', over: false });
  assert.equal(P.budgetLine(2000000, 2000000).over, true);
  assert.equal(P.budgetLine(0, null), null);
  assert.equal(P.budgetLine(5400, null).text, '5.4k tokens');
  assert.equal(P.wouldPostLabel('REQUEST_CHANGES'), 'Request changes');
  assert.equal(P.humansLine(1), '1 human approval required');
  assert.equal(P.parsePolicy('{"mode":"observe"}').mode, 'observe');
  assert.equal(P.normalisePolicyGet({ policy: '{"mode":"comment"}', raw: null, override: 'observe' }).policy.mode, 'comment');
});
