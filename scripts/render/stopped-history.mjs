// Controlled browser fixtures only. Not packaged/native acceptance.
export async function checkStoppedHistory(page, shot) {
  const errors = await page.evaluate(async () => {
    const errors = [], t = window.__test;
    const wait = () => new Promise((r) => setTimeout(r, 250));
    const text = () => document.querySelector('.conv')?.innerText ?? '';
    const input = () => document.querySelector('.composer textarea');
    const click = (label) => [...document.querySelectorAll('button')].find((b) => b.textContent.trim() === label)?.click();
    const navigate = async (id) => { window.__philiumGo('agents', { id }); await wait(); };
    const event = (seq, content) => ({ seq, type: 'message.created', role: 'user', content });
    input().value = 'stopped draft kept'; input().dispatchEvent(new Event('input', { bubbles: true }));
    const clipboardData = new DataTransfer();
    clipboardData.items.add(new File([Uint8Array.from(atob('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aE3cAAAAASUVORK5CYII='), (c) => c.charCodeAt(0))], 'history-draft.png', { type: 'image/png' }));
    input().dispatchEvent(new ClipboardEvent('paste', { clipboardData, bubbles: true, cancelable: true })); await wait();
    t.historyMode = true; t.historyCalls = []; t.historyActions = []; t.historyPending = [];
    t.historyAgents = {
      a1: { id: 'a1', name: 'lion', live: false, state: 'stopped', model_ref: 'fixture/model' },
      a2: { id: 'a2', name: 'otter', live: false, state: 'stopped', model_ref: 'fixture/model' },
    };
    t.historyEvents = { a1: [event(1, 'saved A history'), { seq: 2, type: 'permission.requested', request_id: 'old', action: 'mcp', resource: 'philium/browser_open' }], a2: [event(1, 'saved B history')] };
    window.__philiumGo('home', {}); await wait(); await navigate('a1');
    if (!text().includes('saved A history')) errors.push('stopped saved history missing');
    if (document.querySelector('.perm button')) errors.push('stopped permission actionable');
    if (document.body.innerText.includes('Some data could not load')) errors.push('stopped history global banner');
    // A failed live-transition reload retains history and the draft.
    if (input().value !== 'stopped draft kept') errors.push('initial stopped selection lost draft');
    t.failHistory = true;
    click('Resume'); await wait();
    if (!document.querySelector('.history-error')) errors.push('genuine history error not local');
    if (!text().includes('saved A history')) errors.push('failed reload discarded saved history');
    if (!document.querySelector('.composer img[alt="history-draft.png"]')) errors.push('failed reload lost draft attachment');
    if (input().value !== 'stopped draft kept') errors.push('failed reload lost draft');
    if (document.body.innerText.includes('Some data could not load')) errors.push('local error leaked globally');
    t.failHistory = false; click('Retry history'); await wait();
    if (document.querySelector('.history-error')) errors.push('retry did not clear local error');
    const beforeState = t.historyCalls.length;
    t.historyAgents.a1.state = 'running'; window.__emit('agents:changed', { id: 'a1' }); await wait();
    if (t.historyCalls.length !== beforeState) errors.push('token/state-only change reloaded history');
    t.holdHistory = true;
    click('Stop'); await wait();
    if (!t.historyPending.length) errors.push('stop live transition did not reload history');
    window.__emit('agents:events', { id: 'a1', events: [event(3, 'stream during history fetch'), event(1, 'saved A history')] });
    t.historyPending.splice(0).forEach((p) => p.resolve()); t.holdHistory = false; await wait();
    const messages = [...document.querySelectorAll('.conv .msg.user')].map((e) => e.textContent);
    if (messages.join('|') !== 'saved A history|stream during history fetch') errors.push(`fetch/stream merge or dedupe failed: ${messages}`);
    if (document.querySelector('.perm button')) errors.push('permission actionable after explicit stop');
    if (t.historyActions.join() !== 'agents.resume,agents.stop') errors.push('unexpected automatic resume/stop');
    // Hold A then B then A, release latest first and stale requests afterwards.
    window.__philiumGo('home', {}); await wait();
    t.holdHistory = true;
    await navigate('a1'); await navigate('a2'); await navigate('a1');
    const pending = t.historyPending.splice(0);
    if (pending.length < 3) errors.push('A B A fetch generations not exercised');
    pending.at(-1)?.resolve(); await wait();
    pending.slice(0, -1).reverse().forEach((p) => { p.response.splice(0, p.response.length, event(9, 'STALE WRONG HISTORY')); p.resolve(); });
    t.holdHistory = false; await wait();
    if (!text().includes('saved A history') || text().includes('STALE') || text().includes('saved B')) errors.push('A B A stale response isolation failed');
    await navigate('a2');
    if (!text().includes('saved B history') || text().includes('saved A')) errors.push('peer history isolation failed');
    window.__emit('agents:events', { id: 'a1', events: [event(10, 'wrong peer streamed')] }); await wait();
    if (text().includes('wrong peer')) errors.push('peer stream isolation failed');
    t.historyEvents.a2 = []; window.__philiumGo('home', {}); await wait(); await navigate('a2');
    if (!text().includes('No saved messages')) errors.push('empty stopped history not explicit');
    await navigate('a1');
    if (!document.querySelector('.composer img[alt="history-draft.png"]')) errors.push('switching agents lost draft attachment');
    if (input().value !== 'stopped draft kept') errors.push('switching agents lost draft');
    if (t.historyActions.join() !== 'agents.resume,agents.stop') errors.push('inspection auto-resumed runtime');
    return errors;
  });
  await shot(page, 'agents-stopped-history');
  return errors;
}
