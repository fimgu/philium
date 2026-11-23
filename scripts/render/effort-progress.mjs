// Rendered production UI with controlled RPC/event fixtures, not native/provider acceptance.
export async function checkLiveTps(page) {
  return page.evaluate(async () => {
    const errors = [];
    const wait = (ms) => new Promise((r) => setTimeout(r, ms));
    const rates = () => document.querySelector('.agent-view')?.innerText.match(/~[\d.]+ TPS/g) ?? [];
    const sendProgress = (agent, tokens) => window.__emit('gateway:progress', { agent, id: 410, phase: 'text', tokens });
    sendProgress('lion', 10); sendProgress('otter', 10);
    await wait(550);
    sendProgress('otter', 105);
    if (rates().length) errors.push('another agent’s TPS appeared in the selected header');
    sendProgress('lion', 50);
    const out = document.querySelector('.agent-view .kv-telemetry-top .kv-legend-item.out');
    if (rates().length !== 1 || !/^~[\d.]+ TPS$/.test(out?.nextElementSibling?.innerText ?? '')) {
      errors.push('expected one live TPS readout immediately after Out in the selected session telemetry row');
    }
    window.__emit('activity', { agent: 'lion', id: 410, state: 'done' });
    if (rates().length) errors.push('TPS remained after the model request completed');
    return errors;
  });
}

export async function checkEffortProgress(page, shot) {
  const errors = await page.evaluate(async () => {
    const errors = [], t = window.__test;
    const wait = (ms = 100) => new Promise((r) => setTimeout(r, ms));
    const controls = () => [...document.querySelectorAll('.speed button')];
    const fast = () => controls().find((b) => b.textContent.trim() === 'Fast');
    const input = document.querySelector('.composer textarea');
    const previousDraft = input.value;
    input.value = 'effort preserves this draft'; input.dispatchEvent(new Event('input', { bubbles: true }));
    if (!document.querySelector('[aria-label="Reasoning effort"]')) errors.push('effort accessible group missing');
    if (!/Support varies by provider and model/.test(document.getElementById('effort-caveat')?.textContent ?? '') || document.querySelector('[aria-label="Reasoning effort"]')?.getAttribute('aria-describedby') !== 'effort-caveat') errors.push('provider support caveat missing');
    t.holdEffort = true;
    fast().click(); await wait();
    if (!controls().every((b) => b.disabled)) errors.push('effort controls not disabled during save');
    if (!document.body.innerText.includes('Updating effort')) errors.push('pending wording missing');
    if (document.body.innerText.includes('Saving effort')) errors.push('misleading old pending wording remains');
    t.holdEffort = false; t.releaseEffort?.(); await wait();
    if (fast().getAttribute('aria-pressed') !== 'true') errors.push('saved low effort not selected');
    if (t.efforts?.a1 !== 'low') errors.push('wrong effort request payload');
    if (input.value !== 'effort preserves this draft') errors.push('effort edit lost draft');
    window.__agentState('running'); await wait();
    window.__emit('agents:events', { id: 'a1', events: [{ type: 'tool.started', seq: 9001, call_id: 'progress-fixture', tool: 'mcp_philium_shell_run', arguments: { command: 'echo progress-fixture' } }] });
    await wait(1250);
    const tool = [...document.querySelectorAll('.conv-tool')].find((el) => el.innerText.includes('progress-fixture'));
    if (!tool?.innerText.includes('Awaiting tool completion')) errors.push('running tool not visibly awaiting completion');
    if (!tool?.querySelector('.dot.busy')) errors.push('running tool lacks busy marker');
    if (!/observed [1-9]\d*s/.test(tool?.innerText ?? '')) errors.push('tool elapsed observation did not advance');
    window.__emit('agents:events', { id: 'a1', events: [{ type: 'tool.finished', seq: 9002, call_id: 'progress-fixture', ok: false, duration_ms: 1250, result: 'fixture failure' }] });
    await wait();
    const finished = [...document.querySelectorAll('.conv-tool')].find((el) => el.innerText.includes('progress-fixture'));
    if (finished?.innerText.includes('Awaiting tool completion')) errors.push('completed tool still shows pending timer');
    if (!finished?.querySelector('.dot.bad')) errors.push('tool failure displayed as success');
    input.value = previousDraft; input.dispatchEvent(new Event('input', { bubbles: true }));
    window.__agentState('idle');
    return errors;
  });
  await shot(page, 'agents-effort-progress-checked');
  return errors;
}
