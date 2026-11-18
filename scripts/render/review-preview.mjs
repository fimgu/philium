import assert from 'node:assert/strict';

export async function checkReviewPreview(page) {
  const select = 'select[aria-label="Active workspace repository"]';
  const input = 'input[aria-label="Local review packet"]';
  const sha = 'a'.repeat(40);
  const data = {plan:{repo:'demo/philium',pr:20,head_sha:sha,cases:[{id:'one',workflow:'Check the fixture marker'}]},results:[],bundle:{head_sha:sha,included:[1,2,3,4,5,6,7],moved:[8,9]}};
  const load = p => page.$eval(input,(el,p)=>{
    const x = new DataTransfer(); x.items.add(new File([JSON.stringify(p)], 'packet.json')); el.files=x.files; el.dispatchEvent(new Event('change',{bubbles:true}));
  },p);
  await page.select(select,'r1'); await page.waitForSelector(input); await load(data);
  const context = 'details[aria-label="Workspace context"]';
  await page.click(`${context} summary`);
  await page.waitForFunction(sel => document.querySelector(sel)?.innerText.includes('lion'), {}, context);
  assert.match(await page.$eval(context,n=>n.innerText), /only you, on this device/);
  await page.waitForFunction(()=>document.querySelector('article')?.innerText.includes('Not run by Philium'));
  await page.click('article details summary');
  const text=await page.$eval('article',n=>n.innerText);
  for(const expected of [sha,'Check the fixture marker','untested','Included: #1, #2, #3, #4, #5, #6, #7','Moved elsewhere: #8, #9']) assert.ok(text.includes(expected),expected);
  await page.select(select,'r2'); assert.equal(await page.$('article'),null);
  await page.click(`${context} summary`);
  assert.doesNotMatch(await page.$eval(context,n=>n.innerText), /\blion\b/);
  await load(data); await page.waitForSelector('[aria-label="Local review preview"] [role="alert"]');
  await load({plan:{repo:'demo/hal',pr:42,head_sha:sha,cases:[]},results:[]});
  await page.waitForFunction(()=>document.querySelector('article')?.innerText.includes('PR #42'));
  assert.ok(!(await page.$eval('article',n=>n.innerText)).includes('fixture marker'));
  await page.select(select,'r1'); await page.waitForSelector(input); assert.equal(await page.$('article'),null);
  await load({...data,plan:{...data.plan,repo:'another/philium'}}); await page.waitForSelector('[aria-label="Local review preview"] [role="alert"]');
  assert.equal(await page.$('article'),null);
}
