import { createResource, createSignal, For, Show } from 'solid-js';
import { rpc, useEvent } from '../lib/rpc.js';
import { Button } from './kit.jsx';

const originCall = (method, repo, args, options) =>
  rpc(`release_intake.${method}`, { repo_id: repo, ...args }, { local: true, ...options });
const sameLogin = (a, b) => a?.toLowerCase() === b?.toLowerCase();

export function useGitHubConnection(repo) {
  const [connection, { refetch }] = createResource(repo, (id, { refetching }) =>
    originCall('origin_connection', id, {}, { fresh: Boolean(refetching) }).catch(error => ({ error: String(error) })));
  useEvent('github:connection', ({ origin }) => {
    const current = connection.latest?.origin;
    if (!current || !origin || origin.toLowerCase() === current.toLowerCase()) refetch();
  });
  return [connection, refetch];
}

export function GitHubAccountPicker(props) {
  const [open, setOpen] = createSignal(false);
  const [selected, setSelected] = createSignal('');
  const [saving, setSaving] = createSignal(false);
  const [error, setError] = createSignal('');
  const [accounts, { refetch: reloadAccounts }] = createResource(
    () => (!props.compact || open()) && props.repo,
    (id, { refetching }) => originCall('origin_accounts', id, {}, { fresh: Boolean(refetching) })
      .catch(error => ({ error: String(error) })),
  );

  const connection = () => props.connection.latest;
  const login = () => connection()?.login;
  const available = () => accounts.latest?.accounts;
  const choice = () => available()?.includes(selected()) ? selected() : '';
  const stale = () => login() && available() && !available().some(account => sameLogin(account, login()));
  const disabled = () => props.disabled || saving();

  async function save(method) {
    const next = choice();
    if (disabled() || (method === 'origin_connect' && (accounts.loading || !next || sameLogin(next, login())))) return;
    setSaving(true);
    setError('');
    try {
      await originCall(method, props.repo, method === 'origin_connect' ? { login: next } : {}, { action: true });
      setSelected('');
      await props.reloadConnection();
    } catch (error) { setError(String(error)); }
    finally { setSaving(false); }
  }

  const status = () => <span aria-label="GitHub connection">
    {login() ? <>GitHub · <strong>{login()}</strong>{stale() ? ' (not found in GitHub CLI)' : ''}{connection().token_saved ? ' · read-only token saved' : ''}</>
      : connection()?.error ? 'GitHub · account status unavailable'
      : !connection() ? 'GitHub · checking account…'
      : connection().token_saved ? 'GitHub · read-only token'
      : 'GitHub · no account selected'}
  </span>;

  const settings = () => <div>
    <p class="small faint">Run <code>gh auth login --hostname github.com</code> for each account, then reload the list. No <code>gh auth switch</code> is needed. This choice does not change Git push authentication.</p>
    <div class="row" style={{ 'flex-wrap': 'wrap', gap: '8px' }}>
      <select class="select" aria-label="GitHub account" value={choice()} disabled={disabled() || accounts.loading || !available()?.length} onChange={e => setSelected(e.currentTarget.value)}>
        <option value="" selected={!choice()}>Choose a GitHub account</option>
        <For each={available()}>{account => <option value={account} selected={choice() === account}>{account}</option>}</For>
      </select>
      <Button type="button" size="sm" variant="primary" disabled={disabled() || props.connection.loading || accounts.loading || !choice() || sameLogin(choice(), login())} onClick={() => save('origin_connect')}>{login() ? 'Change account' : 'Connect GitHub'}</Button>
      <Button type="button" size="sm" disabled={disabled() || accounts.loading} onClick={() => { setError(''); return Promise.all([reloadAccounts(), props.reloadConnection()]); }}>Reload accounts</Button>
      <Show when={login()}><Button type="button" size="sm" disabled={disabled() || props.connection.loading} onClick={() => save('origin_disconnect')}>Disconnect</Button></Show>
    </div>
    <Show when={accounts.loading}><p role="status" class="small faint">Loading GitHub CLI accounts…</p></Show>
    <Show when={!accounts.loading && available()?.length === 0}><p class="small faint">No GitHub CLI accounts found.</p></Show>
    <Show when={!accounts.loading && accounts.latest?.error}><p role="alert">{accounts.latest.error}</p></Show>
    <Show when={connection()?.error}><p role="alert">{connection().error}</p></Show>
    <Show when={error()}><p role="alert">{error()}</p></Show>
  </div>;

  return props.compact
    ? <details aria-label="GitHub account settings" onToggle={e => setOpen(e.currentTarget.open)}><summary>{status()} · Manage account</summary><Show when={open()}>{settings()}</Show></details>
    : <div><p role="status">{status()}</p>{settings()}</div>;
}
