import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, writeFileSync, mkdtempSync, rmSync, existsSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { spawnSync } from 'node:child_process';
import vm from 'node:vm';

const ui = readFileSync(new URL('../../src/ui/improve.jsx', import.meta.url), 'utf8');
const rust = readFileSync(new URL('../../src-tauri/src/selfdev.rs', import.meta.url), 'utf8');
const label = vm.runInNewContext(`(${ui.slice(ui.indexOf('export function previewLabel'), ui.indexOf('// Recent errors')).replace('export ', '')})`);

test('preview labels distinguish active CLI from readiness and failed/stopped/missing terminals', () => {
  assert.equal(label('active'), 'Preview process active');
  assert.equal(label('failed'), 'Preview failed');
  assert.equal(label('stopped'), 'Preview stopped');
  assert.equal(label(undefined), 'Preview status unavailable');
  assert.doesNotMatch(ui, /Preview running/);
});

test('each terminal has its own output action and preview failure retains exit code', () => {
  for (const [key, text] of [['test_terminal', 'Test output'], ['preview_terminal', 'Preview output'], ['build_terminal', 'Build output']]) {
    assert.ok(ui.includes(`["${key}", "${text}"]`));
  }
  assert.match(ui, /go\("terminals", \{ id: s\(\)\[key\] \}\)/);
  assert.match(ui, /exit \{s\(\)\.preview_exit_code\}/);
  assert.match(ui, /onCleanup\(\(\) => clearInterval\(poll\)\)/);
});

test('real Rust preview helpers pass isolated filesystem and exit-101 regression tests without cargo', () => {
  const dir = mkdtempSync(join(process.env.JCODE_SCRATCH_DIR || tmpdir(), 'selfdev-focused-'));
  try {
    // Compile actual helper source against the already-built libc dependency.
    const helpers = rust.slice(rust.indexOf('fn preview_state('), rust.indexOf('fn prepare_worktree('));
    const tests = rust.slice(rust.indexOf('#[cfg(test)]\nmod preview_focused_tests'), rust.indexOf('#[cfg(test)]\nmod tests'));
    const source = join(dir, 'preview.rs');
    const binary = join(dir, 'preview-tests');
    writeFileSync(source, `use std::path::{Path, PathBuf};\n${helpers}\n${tests}`);
    const deps = new URL('../../src-tauri/target/debug/deps/', import.meta.url).pathname;
    const libc = process.platform === 'win32' ? null : readdirSync(deps).filter(name => /^liblibc-.*\.rlib$/.test(name));
    const candidates = libc?.length ? libc : [null];
    let compile;
    for (const candidate of candidates) {
      compile = spawnSync('rustc', ['--edition=2021', '--test', source, '-o', binary, ...(candidate ? ['--extern', `libc=${join(deps, candidate)}`] : [])], { encoding: 'utf8' });
      if (compile.status === 0) break;
    }
    assert.equal(compile.status, 0, compile.stderr);
    const result = spawnSync(binary, [], { encoding: 'utf8', env: { ...process.env, TMPDIR: dir } });
    assert.equal(result.status, 0, result.stdout + result.stderr);
    assert.match(result.stdout, process.platform === 'win32' ? /3 passed/ : /4 passed/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
    assert.equal(existsSync(dir), false);
  }
});
