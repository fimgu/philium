import { test } from 'node:test';
import assert from 'node:assert/strict';
import { reviewPreview, githubIdentity } from './review-packet.js';
const sha = 'a'.repeat(40);
const repo = { id:'one', remote_url:'git@github.com:org/one.git' };
const data = () => ({plan:{repo:'org/one',pr:5,head_sha:sha,cases:[{id:'a',workflow:'See A'},{id:'b',workflow:'See B'}]},results:[{id:'b',status:'pass',head_sha:'b'.repeat(40)}]});
test('a missing or stale row never becomes a pass', () => {
  const p = reviewPreview(data(), repo);
  assert.deepEqual(p.cases.map(c=>c.status), ['untested','wrong revision']);
  assert.equal(p.first.id,'b'); assert.equal(p.bundle,null);
});
test('mismatched identities fail without disclosing contents', () => {
  for (const w of [{}, {id:'two',remote_url:'https://github.com/another/one'}, {id:'one',remote_url:'https://wrong.example/org/one'}]) assert.throws(()=>reviewPreview(data(), w), /does not match/);
  assert.equal(githubIdentity('ssh://git@github.com/Org/One.git'), 'org/one');
});
test('reject ambiguous or unpinned input', () => {
  for(const mutate of [p=>p.results.push(p.results[0]), p=>p.plan.cases.push(p.plan.cases[0]),p=>p.plan.head_sha='main',p=>p.results[0].status='probably',p=>p.bundle={head_sha:sha,included:[1],moved:[1]}]) {
    const p=data(); mutate(p); assert.throws(()=>reviewPreview(p,repo));
  }
});
