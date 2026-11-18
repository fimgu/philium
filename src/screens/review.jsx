import { createSignal, For, Show, onCleanup } from 'solid-js';
import { activeRepoId, useStore } from '../lib/store.js';
import { Empty, Page } from '../ui/kit.jsx';
import { githubIdentity, maxPacketBytes, reviewPreview } from '../lib/review-packet.js';

function Reader(props) {
  const [data, setData] = createSignal(null), [error, setError] = createSignal('');
  let generation = 0;
  onCleanup(() => { generation++; });
  async function open(e) {
    const file = e.currentTarget.files?.[0];
    e.currentTarget.value = '';
    const seq = ++generation;
    setData(null); setError('');
    if (!file) return;
    if (file.size > maxPacketBytes) return setError('Choose a packet smaller than 1 MiB.');
    try {
      const text = await file.text();
      if (seq !== generation) return;
      let packet;
      try { packet = JSON.parse(text); } catch { throw new Error('This file is not valid JSON.'); }
      setData(reviewPreview(packet, props.workspace()));
    } catch (err) { if (seq === generation) setError(err.message); }
  }
  const numbers = values => values.map(n => `#${n}`).join(', ') || 'None listed';
  return <section aria-label="Local review preview">
    <p>Open a saved review for <strong>{props.slug}</strong>. Nothing is run or published.</p>
    <label>Local review packet <input aria-label="Local review packet" type="file" accept=".json,application/json" onChange={open} /></label>
    <Show when={error()}><p role="alert">{error()}</p></Show>
    <Show when={data()}>{d => <article>
      <h2>PR #{d().pr}</h2>
      <p><strong>Imported evidence · Not run by Philium</strong></p>
      <p>Revision: <code>{d().sha}</code></p>
      <p>{d().first ? `Needs attention: ${d().first.title} (${d().first.status})` : d().cases.length ? 'No unresolved case reported. This is not release approval.' : 'No test cases recorded.'}</p>
      <details><summary>Bundle membership</summary><Show when={d().bundle} fallback={<p>Not recorded in this packet.</p>}>{b => <><p>Included: {numbers(b().included)}</p><p>Moved elsewhere: {numbers(b().moved)}</p></>}</Show></details>
      <h3>Recorded checks</h3><For each={d().cases}>{c => <details><summary>{c.title} · {c.status}</summary><p>{c.note || 'No observation recorded.'}</p></details>}</For>
    </article>}</Show>
  </section>;
}

export default function Review() {
  const store = useStore();
  const workspace = () => store.repos()?.find(r => r.id === activeRepoId());
  const slug = () => githubIdentity(workspace()?.remote_url);
  // Key by stable values, not the repo row object: background refresh is not a switch.
  const key = () => slug() && JSON.stringify([activeRepoId(), slug()]);
  return <Page title="Review" sub="Saved results">
    <Show when={workspace()?.id} keyed>{id => <details aria-label="Workspace context">
      <summary>Context: who can see this?</summary>
      <p><strong>Saved review: only you, on this device.</strong> Opening a packet does not send it to a worker, model, runner or GitHub.</p>
      <h3>Agents in this workspace</h3>
      <p>These are configured agents, not recipients of the saved review. Starting a separate agent session can send its prompt, code map and requested code to its configured provider.</p>
      <ul><For each={(store.agents() || []).filter(a => a.repo_id === id)} fallback={<li>No agents listed for this workspace.</li>}>{a => <li>
        <strong>{a.name}</strong> · {a.model_ref || a.model || 'Unknown recipient'} · {a.strict ? 'No memory or history access' : 'May request this repository’s memory and session metadata'}
      </li>}</For></ul>
      <p>Helpers inherit the parent’s workspace and configured model. The skills tool serves the built-in review procedure, not imported personal skills or their files.</p>
    </details>}</Show>
    <Show when={key()} keyed fallback={<Empty title="Select a GitHub workspace" body="Use the workspace picker to view a saved PR review for one repository." />}>
      {_identity => <Reader workspace={workspace} slug={slug()} />}
    </Show>
  </Page>;
}
