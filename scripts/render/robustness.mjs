// Browser-driven regression checks with the render harness's Tauri fixture.
// These are UI integration checks, not native packaged acceptance.
export async function checkAgentMessaging(page) {
  return page.evaluate(async () => {
    const errors = [];
    const wait = () => new Promise((r) => setTimeout(r, 220));
    const input = () => document.querySelector('.composer textarea');
    const value = (text) => { input().value = text; input().dispatchEvent(new Event('input', { bubbles: true })); };
    const enter = () => input().dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
    const paste = async (name) => {
      const data = Uint8Array.from(atob('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aE3cAAAAASUVORK5CYII='), (c) => c.charCodeAt(0));
      const clipboardData = new DataTransfer();
      clipboardData.items.add(new File([data], name, { type: 'image/png' }));
      input().dispatchEvent(new ClipboardEvent('paste', { clipboardData, bubbles: true, cancelable: true }));
      await wait();
    };
    if (!input()) return ['composer missing'];
    value('keep this draft');
    await paste('first.png');
    const navigate = (label) => [...document.querySelectorAll('.nav-item')].find((b) => b.textContent.trim() === label)?.click();
    navigate('Home'); await wait(); navigate('Agents'); await wait();
    if (!input() || input().value !== 'keep this draft' || !document.querySelector('.composer img')) return ['navigation lost agent draft or attachment'];
    window.__agentState('running'); await wait();
    enter(); enter(); await wait();
    if (window.__test.sends.length) errors.push('busy Enter submitted');
    if (input().value !== 'keep this draft' || !document.querySelector('.composer img')) errors.push('busy draft lost');
    window.__agentState('waiting'); await wait();
    enter(); await wait();
    if (window.__test.sends.length) errors.push('waiting Enter submitted');
    window.__test.failCancel = true;
    [...document.querySelectorAll('.composer button')].find((b) => b.textContent.trim() === 'Stop')?.click();
    await wait();
    if (!document.body.innerText.includes('Cancellation unavailable')) errors.push('cancel failure not visible');
    if (document.body.innerText.includes('Some data could not load')) errors.push('cancel failure became data loading failure');
    window.__test.failCancel = false;
    window.__agentState('idle'); await wait();
    enter(); enter(); await wait();
    if (window.__test.sends.length !== 1) errors.push('duplicate pending submission');
    if (input().value !== 'keep this draft') errors.push('pending draft cleared early');
    window.__test.releaseSend?.(); await wait();
    if (input().value !== 'keep this draft' || !document.querySelector('.composer img')) errors.push('rejected draft lost');
    if (document.body.innerText.includes('Some data could not load')) errors.push('action rejection became data loading failure');
    if (!document.body.innerText.includes('Your draft was kept')) errors.push('busy error lacks actionable feedback');
    window.__test.mode = 'accept';
    enter(); await wait();
    value('new draft while sending');
    await paste('second.png');
    window.__test.releaseSend?.(); await wait();
    if (input().value !== 'new draft while sending') errors.push('accepted send cleared newer edits');
    const images = [...document.querySelectorAll('.composer img')].map((im) => im.alt);
    if (images.join() !== 'second.png') errors.push(`wrong attachments after acceptance: ${images}`);
    const retry = window.__test.sends[1];
    if (retry?.text !== 'keep this draft' || retry?.images?.[0]?.name !== 'first.png') errors.push('retry payload lost original draft');
    enter(); await wait(); window.__test.releaseSend?.(); await wait();
    if (input().value || document.querySelector('.composer img')) errors.push('accepted unchanged draft not cleared');
    if (window.__test.sends.length !== 3) errors.push('unexpected accepted send count');
    return errors;
  });
}

export async function checkChatMessaging(page) {
  return page.evaluate(async () => {
    const errors = [];
    const input = document.querySelector('.composer textarea');
    if (!input) return ['chat composer missing'];
    if ([...document.querySelectorAll('.composer button')].some((b) => b.textContent.trim() === 'Stop')) errors.push('completed chat history still busy');
    input.value = 'keep rejected chat'; input.dispatchEvent(new Event('input', { bubbles: true }));
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
    await new Promise((r) => setTimeout(r, 150));
    if (window.__test.sends.length !== 1) errors.push('chat message not attempted');
    if (input.value !== 'keep rejected chat') errors.push('chat rejection lost draft');
    if (document.body.innerText.includes('Some data could not load')) errors.push('chat action rejection became data loading failure');
    window.__test.mode = 'accept';
    const send = async (text) => {
      input.value = text; input.dispatchEvent(new Event('input', { bubbles: true }));
      input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
      await new Promise((r) => setTimeout(r, 180));
    };
    await send('first accepted');
    await send('second accepted');
    if (window.__test.sends.length !== 3 || input.value) errors.push('chat cannot complete two successive sends');
    window.__test.chatWorking = true;
    await send('keep working');
    window.__test.failCancel = true;
    const stop = [...document.querySelectorAll('.composer button')].find((b) => b.textContent.trim() === 'Stop');
    if (!stop) errors.push('working chat has no Stop');
    const activeTool = [...document.querySelectorAll('.conv-tool')].find((el) => el.innerText.includes('fixture_tool'));
    if (!activeTool?.querySelector('.dot.busy') || !activeTool.innerText.includes('Awaiting tool completion')) errors.push('chat busy state did not reach shared tool progress view');
    if (activeTool?.innerText.includes('No tool completion recorded')) errors.push('active chat tool mislabeled as inactive history');
    stop?.click(); await new Promise((r) => setTimeout(r, 150));
    if (!document.body.innerText.includes('Cancellation unavailable')) errors.push('chat cancel failure not visible');
    if (document.body.innerText.includes('Some data could not load')) errors.push('chat cancel failure became data loading failure');
    return errors;
  });
}
