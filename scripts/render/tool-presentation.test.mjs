import test from 'node:test';
import assert from 'node:assert/strict';
import { toolOutput, cleanTerminalText, shellStatus } from '../../src/lib/tool-presentation.js';
test('shell envelopes become readable output without hiding lifecycle', () => {
 const result = {content:[{type:'text',text:JSON.stringify({output:'hello\n',running:true,exit_code:null})}]};
 assert.equal(toolOutput(result), 'hello\n');
 assert.equal(shellStatus(result), 'Still running');
 assert.equal(shellStatus({exit_code:7}), 'Exit 7');
 assert.equal(shellStatus({state:'interrupted'}), 'Interrupted');
 assert.equal(shellStatus('unknown'), null);
 assert.equal(toolOutput({stdout:'out',stderr:'err'}), 'out\nerr');
 assert.equal(toolOutput(null,'stream'), 'stream');
 assert.equal(toolOutput({custom:true}), '{\n  "custom": true\n}');
});
test('terminal escapes stripped, HTML remains inert text', () => {
 assert.equal(cleanTerminalText('\x1b[31mred\x1b[0m\r\n<script>x</script>'), 'red\n<script>x</script>');
 assert.equal(cleanTerminalText('\x1b]0;title\x07ok'), 'ok');
});
