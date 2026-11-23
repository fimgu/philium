// Browser-driven regression checks with the render harness's Tauri fixture.
// These are UI integration checks, not native packaged acceptance.
export async function checkAgentMessaging(page) {
  return page.evaluate(async () => {
    const errors = [];
    const wait = () => new Promise((r) => setTimeout(r, 220));
    const input = () => document.querySelector('.composer textarea');
    const value = (text) => { input().value = text; input().dispatchEvent(new Event('input', { bubbles: true })); };
    const enter = () => input().dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
    for (let i = 0; i < 20 && !document.querySelector('.msg.user .sent-image img'); i++) await new Promise((r) => setTimeout(r, 50));
    const sent = document.querySelector('.msg.user .sent-image img');
    if (!sent || !sent.src.startsWith('blob:')) errors.push('sent image thumbnail missing from transcript');
    else {
      if (!(window.__test.attachmentCalls ?? []).every((c) => /^[0-9a-f]{64}$/.test(c.sha256) && c.id)) errors.push('attachment fetched without agent scope');
      sent.closest('button').click(); await new Promise((r) => setTimeout(r, 50));
      if (!document.querySelector('.lightbox img')) errors.push('lightbox did not open');
      document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); await new Promise((r) => setTimeout(r, 50));
      if (document.querySelector('.lightbox')) errors.push('lightbox did not close on Escape');
    }
    const paste = async (name) => {
      const data = Uint8Array.from(atob('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aE3cAAAAASUVORK5CYII='), (c) => c.charCodeAt(0));
      const clipboardData = new DataTransfer();
      clipboardData.items.add(new File([data], name, { type: 'image/png' }));
      input().dispatchEvent(new ClipboardEvent('paste', { clipboardData, bubbles: true, cancelable: true }));
      await wait();
    };
    if (!input()) return ['composer missing'];
    // Every attach path, headless: picker, drop, screenshot ok/cancel/denied.
    const png = () => Uint8Array.from(atob('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aE3cAAAAASUVORK5CYII='), (c) => c.charCodeAt(0));
    const count = () => document.querySelectorAll('.composer-images img').length;
    const menuItem = (label) => { const btn = document.querySelector('[aria-label="Attach"]'); if (btn.getAttribute('aria-expanded') !== 'true') btn.click(); return [...document.querySelectorAll('.composer-menu [role="menuitem"]')].find((b) => b.textContent.includes(label)); };
    const picker = document.querySelector('.composer input[type="file"]');
    const chosen = new DataTransfer(); chosen.items.add(new File([png()], 'picked.png', { type: 'image/png' }));
    if (!menuItem('Choose images')) errors.push('Choose images menu item missing');
    picker.files = chosen.files; picker.dispatchEvent(new Event('change', { bubbles: true })); await wait();
    if (count() !== 1) errors.push(`picker did not attach (${count()})`);
    const dropped = new DataTransfer(); dropped.items.add(new File([png()], 'dropped.png', { type: 'image/png' }));
    const target = document.querySelector('.agent-main') ?? input();
    for (const type of ['dragenter', 'dragover', 'drop']) target.dispatchEvent(new DragEvent(type, { dataTransfer: dropped, bubbles: true, cancelable: true }));
    await wait();
    if (count() !== 2) errors.push(`drop did not attach (${count()})`);
    const svg = new DataTransfer(); svg.items.add(new File(['<svg/>'], 'x.svg', { type: 'image/svg+xml' }));
    for (const type of ['dragenter', 'dragover', 'drop']) target.dispatchEvent(new DragEvent(type, { dataTransfer: svg, bubbles: true, cancelable: true }));
    await wait();
    if (count() !== 2 || !/Unsupported image type/.test(document.querySelector('.composer-status')?.textContent ?? '')) errors.push('unsupported drop not rejected visibly');
    window.__test.screenshot = png(); menuItem('Take screenshot').click(); await wait();
    if (count() !== 3 || !document.querySelector('.composer-images img[alt^="Screenshot "]')) errors.push('screenshot did not attach');
    window.__test.screenshot = new Uint8Array(); menuItem('Take screenshot').click(); await wait();
    if (count() !== 3 || document.querySelector('.composer-permission')) errors.push('cancelled screenshot changed draft or showed error');
    window.__test.screenshot = 'denied'; menuItem('Take screenshot').click(); await wait();
    const perm = document.querySelector('.composer-permission');
    if (!perm || !/Screen Recording/.test(perm.textContent) || !/⌃⇧⌘4/.test(perm.textContent)) errors.push('permission notice missing guidance');
    [...(perm?.querySelectorAll('button') ?? [])].find((b) => b.textContent === 'Open Settings')?.click(); await wait();
    if (window.__test.settingsOpened !== 1) errors.push('Open Settings did not invoke');
    [...(perm?.querySelectorAll('button') ?? [])].find((b) => b.textContent === 'Dismiss')?.click(); await wait();
    if (document.querySelector('.composer-permission')) errors.push('permission notice not dismissible');
    window.__test.screenshot = undefined;
    while (document.querySelector('.composer-images [aria-label^="Remove"]')) { document.querySelector('.composer-images [aria-label^="Remove"]').click(); await wait(); }
    if (count() !== 0) errors.push('could not remove attachments');
    value('keep this draft');
    await paste('first.png');
    const navigate = (label) => [...document.querySelectorAll('.nav-item')].find((b) => b.textContent.trim() === label)?.click();
    navigate('Home'); await wait(); navigate('Agents'); await wait();
    if (!input() || input().value !== 'keep this draft' || !document.querySelector('.composer-images img')) return ['navigation lost agent draft or attachment'];
    window.__agentState('running'); await wait();
    enter(); enter(); await wait();
    if (window.__test.sends.length) errors.push('busy Enter submitted directly');
    const queued = window.__test.queued ?? [];
    if (queued.length !== 1 || queued[0].text !== 'keep this draft' || queued[0].images?.[0]?.name !== 'first.png') errors.push('busy draft was not queued exactly once with its attachment');
    if (input().value || document.querySelector('.composer-images img')) errors.push('queued draft was not cleared');
    window.__agentState('waiting'); await wait();
    value('keep this draft');
    await paste('first.png');
    window.__test.rejectQueue = true;
    enter(); await wait();
    if (window.__test.sends.length || queued.length !== 1) errors.push('failed queue dispatch submitted twice');
    if (input().value !== 'keep this draft' || !document.querySelector('.composer-images img')) errors.push('queue failure lost the draft');
    window.__test.failCancel = true;
    [...document.querySelectorAll('.composer button')].find((b) => b.getAttribute('aria-label') === 'Stop')?.click();
    await wait();
    if (!document.body.innerText.includes('Cancellation unavailable')) errors.push('cancel failure not visible');
    if (document.body.innerText.includes('Some data could not load')) errors.push('cancel failure became data loading failure');
    window.__test.failCancel = false;
    window.__agentState('idle'); await wait();
    enter(); enter(); await wait();
    if (window.__test.sends.length !== 1) errors.push('duplicate pending submission');
    if (input().value !== 'keep this draft') errors.push('pending draft cleared early');
    window.__test.releaseSend?.(); await wait();
    if (input().value !== 'keep this draft' || !document.querySelector('.composer-images img')) errors.push('rejected draft lost');
    if (document.body.innerText.includes('Some data could not load')) errors.push('action rejection became data loading failure');
    if (!document.body.innerText.includes('Fixture queue unavailable')) errors.push('queue failure not visible after a busy send');
    window.__test.rejectQueue = false;
    window.__test.mode = 'accept';
    enter(); await wait();
    value('new draft while sending');
    await paste('second.png');
    window.__test.releaseSend?.(); await wait();
    if (input().value !== 'new draft while sending') errors.push('accepted send cleared newer edits');
    const images = [...document.querySelectorAll('.composer-images img')].map((im) => im.alt);
    if (images.join() !== 'second.png') errors.push(`wrong attachments after acceptance: ${images}`);
    const retry = window.__test.sends[1];
    if (retry?.text !== 'keep this draft' || retry?.images?.[0]?.name !== 'first.png') errors.push('retry payload lost original draft');
    const img = retry?.images?.[0] || {};
    if (!/^[A-Za-z0-9+/]+=*$/.test(img.data || '') || !/^image\//.test(img.media_type || '') || JSON.stringify(retry).includes('blob:')) errors.push(`send payload not base64 image: ${JSON.stringify(img).slice(0, 120)}`);
    enter(); await wait(); window.__test.releaseSend?.(); await wait();
    if (input().value || document.querySelector('.composer-images img')) errors.push('accepted unchanged draft not cleared');
    if (window.__test.sends.length !== 3) errors.push('unexpected accepted send count');
    return errors;
  });
}

export async function checkChatMessaging(page) {
  return page.evaluate(async () => {
    const errors = [];
    const input = document.querySelector('.composer textarea');
    if (!input) return ['chat composer missing'];
    if ([...document.querySelectorAll('.composer button')].some((b) => b.getAttribute('aria-label') === 'Stop')) errors.push('completed chat history still busy');
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
    const stop = [...document.querySelectorAll('.composer button')].find((b) => b.getAttribute('aria-label') === 'Stop');
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
