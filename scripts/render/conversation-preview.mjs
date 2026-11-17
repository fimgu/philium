// Isolated, synthetic conversation preview. No Tauri RPC, provider or real history.
import { createServer } from 'vite';
import solid from 'vite-plugin-solid';
const id = new URL('./__conversation-preview.jsx', import.meta.url).pathname;
const fixture = `import { render } from 'solid-js/web';
import { createSignal } from 'solid-js';
import { Conversation, Composer } from '/src/ui/conversation.jsx';
import '/src/styles/tokens.css';
import '/src/styles/components.css';
import '/src/styles/agents.css';
const [busy, setBusy] = createSignal(true);
const [events, setEvents] = createSignal([
{type:'turn.started',turn_id:'preview',seq:1},
{type:'message.created',role:'user',content:'Check the parser and show me the result.',seq:2},
{type:'model.delta',delta:'# Parser check\\n\\nI found the relevant parser. I’ll run its focused tests next.\\n\\n- Preserve existing behavior\\n- Check edge cases\\n\\n',seq:3},
{type:'model.finished',seq:4},
{type:'tool.started',call_id:'test',tool:'mcp_philium_shell_run',arguments:{command:'npm test -- parser'},seq:5},
{type:'tool.output',call_id:'test',data:'Running parser tests…\\n',seq:6}
]);
const finish = () => { setEvents(e => [...e,{type:'tool.finished',call_id:'test',ok:true,result:'4 tests passed',duration_ms:1234,seq:7},{type:'model.delta',delta:'**Checks passed.** No files changed.\\n\\n~~~js\\nparse("hello");\\n~~~',seq:8},{type:'assistant.completed',seq:9}]); setBusy(false); };
render(() => <div style="height:100vh;display:flex;flex-direction:column"><div style="padding:8px">Synthetic preview · no provider calls <button onClick={finish} disabled={!busy()}>Finish test turn</button></div><Conversation events={events()} busy={busy()} /><Composer busy={busy()} onStop={() => { setEvents(e => [...e,{type:'turn.cancelled',seq:10}]); setBusy(false); }} onSend={async text => { setEvents(e => [...e,{type:'message.created',role:'user',content:text,seq:11}]); }} /></div>, document.getElementById('app'));
`;
const server = await createServer({ configFile: false, root: new URL('../..', import.meta.url).pathname,
plugins: [solid(), {name:'conversation-preview', configureServer(s) { s.middlewares.use('/conversation-preview', (_req,res) => { res.setHeader('Content-Type','text/html'); res.end('<div id="app"></div><script type="module" src="/@conversation-preview.jsx"></script>'); }); }, resolveId(value) { if(value === '/@conversation-preview.jsx') return id; }, load(value) { if(value === id) return fixture; }}], server: {host:'127.0.0.1',port:0} });
await server.listen();
console.log('Preview: http://127.0.0.1:' + server.httpServer.address().port + '/conversation-preview');
for (const signal of ['SIGINT','SIGTERM']) process.on(signal, async () => { server.httpServer?.closeAllConnections(); await server.close(); process.exit(0); });
