(() => {
  const select = document.querySelector('select[aria-label="Active workspace repository"]');
  if (!select || !Array.from(select.options).some(o => o.value === __REPO__)) return null;
  if (select.value !== __REPO__) {select.value = __REPO__; select.dispatchEvent(new Event('change', {bubbles:true})); return null;}
  const nav = Array.from(document.querySelectorAll('button.nav-item')).find(b => b.textContent.trim() === 'Review');
  if (!nav) return null;
  if (!nav.classList.contains('on')) {nav.click(); return null;}
  const details = document.querySelector('details[aria-label="Release PR revisions"]');
  if (!details) return null;
  details.open=true;
  const form=details.querySelector('form'), input=form?.querySelector('input[type="url"]');
  if (!input) return null;
  if (!form.dataset.fixtureSubmitted) {input.value='https://github.com/typicode/demo/pull/1090';input.dispatchEvent(new Event('input',{bubbles:true}));form.dataset.fixtureSubmitted='yes';form.requestSubmit();return null;}
  const result=details.querySelector('section[aria-label="Release decision"]');
  return result ? {decision:result.querySelector('h3')?.textContent, head:result.querySelector('code')?.textContent, live_case:result.innerText.includes('live-id: pass')} : null;
})()
