import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, writeFileSync, mkdtempSync, rmSync, existsSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { spawnSync } from 'node:child_process';
import vm from 'node:vm';

const ui = readFileSync(new URL('../../src/ui/improve.jsx', import.meta.url), 'utf8');
const rust = readFileSync(new URL('../../src-tauri/src/selfdev.rs', import.meta.url), 'utf8');
const manifest = readFileSync(new URL('../../src-tauri/Cargo.toml', import.meta.url), 'utf8');
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

test('real Rust preview helpers pass isolated filesystem and exit-101 regression tests without building Tauri', () => {
  const dir = mkdtempSync(join(process.env.PHILIUM_SCRATCH_DIR || tmpdir(), 'selfdev-focused-'));
  try {
    // Compile the production helpers and their tests without requiring a
    // previous Philium build, its GUI dependencies, or a HAL sidecar.
    const helpers = rust.slice(rust.indexOf('fn preview_state('), rust.indexOf('fn prepare_worktree('));
    const tests = rust.slice(rust.indexOf('#[cfg(test)]\nmod preview_focused_tests'), rust.indexOf('#[cfg(test)]\nmod tests'));
    const libc = manifest.match(/^libc\s*=\s*[^\r\n]+/m)?.[0];
    assert.ok(libc, 'Philium must declare its libc dependency');
    writeFileSync(join(dir, 'preview.rs'), `use std::path::{Path, PathBuf};\n${helpers}\n${tests}`);
    const cargoFile = join(dir, 'Cargo.toml');
    writeFileSync(cargoFile, `[package]\nname = "philium-preview-test"\nversion = "0.0.0"\nedition = "2021"\n[lib]\npath = "preview.rs"\n[target.'cfg(unix)'.dependencies]\n${libc}\n`);
    const result = spawnSync('cargo', ['test', '--quiet', '--manifest-path', cargoFile, '--target-dir', join(dir, 'target')], {
      encoding: 'utf8', env: { ...process.env, TMPDIR: dir },
    });
    assert.ifError(result.error);
    assert.equal(result.status, 0, result.stdout + result.stderr);
    assert.match(result.stdout, process.platform === 'win32' ? /3 passed/ : /4 passed/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
    assert.equal(existsSync(dir), false);
  }
});
