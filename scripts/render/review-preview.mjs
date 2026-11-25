import assert from 'node:assert/strict';

export async function checkReviewPreview(page) {
  const select = 'select[aria-label="Active workspace repository"]';
  const input = 'input[aria-label="Local review packet"]';
  const sha = 'a'.repeat(40);
  // The local packet reader lives under Release engineering tools, collapsed by default.
  const tools = async () => { await page.waitForSelector('details[aria-label="More review tools"]'); await page.$eval('details[aria-label="More review tools"]', d => { d.open = true; }); await page.waitForSelector(input); };
  const data = {plan:{repo:'demo/philium',pr:20,head_sha:sha,cases:[{id:'one',workflow:'Check the fixture marker'}]},results:[],bundle:{head_sha:sha,included:[1,2,3,4,5,6,7],moved:[8,9]}};
  const load = p => page.$eval(input,(el,p)=>{
    const x = new DataTransfer(); x.items.add(new File([JSON.stringify(p)], 'packet.json')); el.files=x.files; el.dispatchEvent(new Event('change',{bubbles:true}));
  },p);
  await page.select(select,'r1'); await tools(); await load(data);
  await page.waitForFunction(()=>document.querySelector('article')?.innerText.includes('Not run by Philium'));
  await page.click('article details summary');
  const text=await page.$eval('article',n=>n.innerText);
  assert.deepEqual(await page.$$eval(`${select} option`,opts => ['r1','r3','r2'].map(id=>opts.find(o=>o.value===id)?.text)), ['Developer/philium','pilot/philium','hal']);
  await page.select(select,'r3'); await tools(); assert.equal(await page.$('article'),null);
  for(const expected of [sha,'Check the fixture marker','untested','Included: #1, #2, #3, #4, #5, #6, #7','Moved elsewhere: #8, #9']) assert.ok(text.includes(expected),expected);
  await page.select(select,'r2'); await tools(); assert.equal(await page.$('article'),null);
  await load(data); await page.waitForSelector('[aria-label="Local review preview"] [role="alert"]');
  await load({plan:{repo:'demo/hal',pr:42,head_sha:sha,cases:[]},results:[]});
  await page.waitForFunction(()=>document.querySelector('article')?.innerText.includes('PR #42'));
  assert.ok(!(await page.$eval('article',n=>n.innerText)).includes('fixture marker'));
  await page.select(select,'r1'); await tools(); assert.equal(await page.$('article'),null);
  await load({...data,plan:{...data.plan,repo:'another/philium'}}); await page.waitForSelector('[aria-label="Local review preview"] [role="alert"]');
  assert.equal(await page.$('article'),null);
}
