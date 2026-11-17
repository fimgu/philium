import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import { kindResources } from "./kind-resources.js";

const row = { id: "resource-a", handle: "philium-agent-a-test", meta: JSON.stringify({ http_port: 42010, https_port: 42011, kubeconfig: "/fixture/kubeconfig" }) };

test("kind resource arrays preserve handle and decode stored metadata", () => {
  const [resource] = kindResources([row]);
  assert.equal(resource.handle, row.handle);
  assert.deepEqual(resource.meta, { http_port: 42010, https_port: 42011, kubeconfig: "/fixture/kubeconfig" });
  assert.equal(typeof row.meta, "string");
  assert.deepEqual(kindResources([{ ...row, meta: { http_port: 123 } }])[0].meta, { http_port: 123 });
});

test("empty arrays do not imply missing CLI and bad metadata remains renderable", () => {
  assert.deepEqual(kindResources([]), []);
  for (const meta of [null, "invalid", "null", "[]", 42]) {
    assert.deepEqual(kindResources([{ ...row, meta }])[0].meta, {});
  }
  assert.throws(() => kindResources({ installed: false, clusters: [] }), /Unexpected/);
});

const source = readFileSync(new URL("../screens/agents.jsx", import.meta.url), "utf8");
const start = source.indexOf("function Details(props) {") + "function Details(props) {".length;
const setup = source.slice(start, source.indexOf("  return (", start));
function detailsHarness() {
  let agent = { id: "a" }, info, fetcher;
  const calls = [], messages = [];
  const rpc = async (method, args, options) => { calls.push({ method, args, options }); return [row]; };
  const context = {
    props: { get agent() { return agent; } }, kindResources, rpc,
    createResource(key, fetch) { fetcher = fetch; return [() => info, { refetch() {} }]; },
    createSignal(initial) { let value = initial; return [() => value, next => { value = next; }]; },
    toast: text => messages.push(text),
  };
  const api = vm.runInNewContext(`${setup}\n({kindState, deleteKind})`, context);
  return { api, calls, messages, load: id => fetcher(id), setInfo: next => { info = next; }, select: id => { agent = { id }; }, fail() { context.rpc = async () => { throw new Error("gateway offline"); }; } };
}

test("Details loads resource arrays, reports errors honestly, and hides previous agent rows", async () => {
  const h = detailsHarness();
  const loaded = await h.load("a"); h.setInfo(loaded);
  assert.equal(h.api.kindState().clusters[0].handle, row.handle);
  assert.equal(h.api.kindState().clusters[0].meta.http_port, 42010);
  h.select("b");
  assert.equal(h.api.kindState(), undefined);
  h.fail();
  const failed = await h.load("b"); h.setInfo(failed);
  assert.match(h.api.kindState().error, /gateway offline/);
  assert.equal(h.api.kindState().clusters.length, 0);
  assert.equal("installed" in failed, false);
});

test("Details deletion sends exact owner id and cluster handle contract", async () => {
  const h = detailsHarness();
  await h.api.deleteKind(row.handle);
  assert.equal(h.calls[0].method, "agents.kind_delete");
  assert.equal(JSON.stringify(h.calls[0].args), JSON.stringify({ id: "a", cluster: row.handle }));
  assert.equal(h.calls[0].options.action, true);
});

test("Details JSX renders handle and metadata without inventing CLI installation state", () => {
  const details = source.slice(start, source.indexOf("function ", start));
  assert.match(details, /\{cl\.handle\}/);
  for (const field of ["http_port", "https_port", "kubeconfig"]) assert.ok(details.includes(`cl.meta.${field}`));
  assert.match(details, /deleteKind\(cl\.handle\)/);
  assert.doesNotMatch(details, /CLI not detected|installed === false/);
  assert.match(details, /Could not load kind clusters/);
});
