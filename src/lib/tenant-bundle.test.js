import test from 'node:test';
import assert from 'node:assert/strict';
import {tenantBundle} from './tenant-bundle.js';

test('two checkouts produce deterministic, independent least-privilege tenant plans',()=>{
  for(const id of ['../host','', 'UPPER','r1-','-x']) assert.throws(()=>tenantBundle(id));
  for(const id of ['r1','r2']) {
    const {items}=tenantBundle(id), ns=`philium-${id}`;
    assert.deepEqual(tenantBundle(id).items,items);
    assert.equal(items[0].metadata.name,ns); assert.equal(items[0].metadata.labels['pod-security.kubernetes.io/enforce'],'restricted');
    for(const x of items.slice(1)) assert.equal(x.metadata.namespace,ns);
    const by=k=>items.find(x=>x.kind===k);
    assert.deepEqual(by('NetworkPolicy').spec,{podSelector:{},policyTypes:['Ingress','Egress'],ingress:[],egress:[]});
    assert.equal(by('ResourceQuota').spec.hard.pods,'4');assert.ok(by('LimitRange').spec.limits[0].default.memory);
    const rb=by('RoleBinding'); assert.equal(rb.roleRef.name,by('Role').metadata.name);
    assert.deepEqual(rb.subjects,[{kind:'ServiceAccount',name:'check-observer',namespace:ns}]);
    assert.deepEqual(by('Role').rules.flatMap(r=>r.resources),['jobs','pods']);
    for(const r of by('Role').rules) assert.deepEqual(r.verbs,['get','list','watch']);
    for(const sa of items.filter(i=>i.kind==='ServiceAccount')) assert.equal(sa.automountServiceAccountToken,false);
  }
});
