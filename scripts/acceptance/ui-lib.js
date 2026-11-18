// Page helpers for the hidden Pilot window. Everything here is something a person
// can do with a pointer and a keyboard: find what is rendered, click it, type into it,
// read it back. The Pilot refuses scripts that reach for the backend bridge.
(() => {
  if (!window.__acc) {
    const A = window.__acc = { log: [], done: false };
    const P = __PARAMS__;
    const note = (m) => { A.log.push(String(m)); };
    const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
    const until = async (fn, what, ms = 20000) => {
      const t0 = Date.now();
      for (;;) {
        try { const v = await fn(); if (v) return v; } catch (e) { A.last = String(e); }
        if (Date.now() - t0 > ms) throw new Error(`Timed out waiting for ${what}`);
        await sleep(80);
      }
    };
    const shown = (el) => !!el && !el.closest('.hidden') && !el.closest('[hidden]');
    const all = (sel, root = document) => Array.from(root.querySelectorAll(sel)).filter(shown);
    const txt = (el) => (el?.innerText ?? el?.textContent ?? '').replace(/\s+/g, ' ').trim();
    const pane = () => document.querySelector('.screen-pane:not(.hidden)') || document;
    const button = (label, root) => all('button', root || document).find((b) => txt(b) === label)
      || all('button', root || document).find((b) => txt(b).includes(label));
    const click = async (label, root) => {
      const b = await until(() => { const x = button(label, root); return x && !x.disabled ? x : null; }, `button "${label}"`);
      b.click(); note(`click ${label}`); await sleep(60); return b;
    };
    const type = (el, value) => {
      const proto = el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : el.tagName === 'SELECT' ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
      Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, value);
      el.dispatchEvent(new Event(el.tagName === 'SELECT' ? 'change' : 'input', { bubbles: true }));
    };
    const field = (label, root) => {
      const f = all('.field', root || document).find((x) => txt(x.querySelector('label')) === label);
      return f?.querySelector('input,textarea,select') || null;
    };
    const fill = async (label, value, root) => {
      const el = await until(() => field(label, root), `field "${label}"`);
      type(el, value); note(`type ${label}`); await sleep(40); return el;
    };
    const choose = (select, wanted) => {
      const o = Array.from(select.options).find((x) => x.value === wanted || txt(x) === wanted) || Array.from(select.options).find((x) => txt(x).includes(wanted));
      if (!o) throw new Error(`No option "${wanted}"`);
      type(select, o.value); return o.value;
    };
    const go = async (label) => {
      const b = await until(() => all('button.nav-item').find((x) => txt(x) === label), `nav ${label}`);
      if (!b.classList.contains('on')) b.click();
      await until(() => b.classList.contains('on'), `screen ${label}`);
      note(`open ${label}`); await sleep(120);
    };
    const picker = () => document.querySelector('select[aria-label="Active workspace repository"]');
    const workspace = async (label) => {
      const s = await until(() => { const p = picker(); return p && Array.from(p.options).some((o) => txt(o).includes(label)) ? p : null; }, `workspace ${label}`);
      choose(s, label); note(`workspace ${label}`); await sleep(200);
    };
    const modal = () => all('.modal[role="dialog"]')[0] || null;
    const toasts = () => all('.toast').map(txt);
    const alerts = () => all('[role="alert"], .shield.danger, .err, .board-error').map(txt).filter(Boolean);
    A.api = { P, note, sleep, until, all, txt, pane, button, click, type, field, fill, choose, go, picker, workspace, modal, toasts, alerts };
    (async () => { const { P, note, sleep, until, all, txt, pane, button, click, type, field, fill, choose, go, picker, workspace, modal, toasts, alerts } = A.api;
/*SCENARIO*/
    })().then((r) => { A.result = r ?? {}; }, (e) => { A.error = String(e?.message || e); A.alerts = (A.api.alerts() || []).slice(0, 6); }).finally(() => { A.done = true; });
  }
  const A = window.__acc;
  return { done: A.done, ok: !A.error, error: A.error || null, alerts: A.alerts || null, result: A.result || null, log: A.log.slice(-40) };
})()
