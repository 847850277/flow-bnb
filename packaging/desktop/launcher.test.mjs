import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { prepare, platformKey, locations } from './launcher.mjs';
const here = path.dirname(fileURLToPath(import.meta.url));
const sha = b => createHash('sha256').update(b).digest('hex');
function fixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "flow-install ' "));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const pkg = path.join(root, 'package'); const home = path.join(root, 'user state');
  const key = platformKey();
  fs.mkdirSync(path.join(pkg, 'native', key), { recursive: true });
  fs.mkdirSync(path.join(pkg, 'flows'));
  fs.mkdirSync(path.join(pkg, 'scripts'));
  fs.copyFileSync(path.join(here, 'launcher.mjs'), path.join(pkg, 'launcher.mjs'));
  fs.writeFileSync(path.join(pkg, 'package.json'), JSON.stringify({ version: '0.1.0', type: 'module' }));
  const body = '#!/bin/sh\ncase "$1" in\nconnection-status) printf \'{"connected":true}\\n\' ;;\nmcp) cat ;;\n*) printf "%s\\n" "$@" > "$FLOW_BNB_AGENTIC_CONFIG.args" ;;\nesac\n';
  fs.writeFileSync(path.join(pkg, 'native', key, 'flow-bnb'), body);
  fs.writeFileSync(path.join(pkg, 'flows/stock.http.yml'), 'user-editable');
  fs.writeFileSync(path.join(pkg, 'scripts/run-cycle.sh'), '#!/bin/bash\nexit 0\n');
  fs.writeFileSync(path.join(pkg, 'manifest.json'), JSON.stringify({ version: '0.1.0', binaries: { [key]: sha(body) }, flows: { 'stock.http.yml': sha('user-editable') }, scripts: { 'run-cycle.sh': sha('#!/bin/bash\nexit 0\n') } }));
  return { pkg, home, key, root };
}
test('clean install is private, persistent, and preserves user strategy and wallet locks', t => {
  const f = fixture(t); const installed = prepare(f.pkg, f.home, f.key);
  assert.equal(fs.statSync(installed.binary).mode & 0o777, 0o700);
  assert.equal(fs.statSync(f.home).mode & 0o777, 0o700);
  const flow = path.join(installed.workspace, 'flows/stock.http.yml');
  fs.writeFileSync(flow, 'my strategy');
  fs.mkdirSync(path.dirname(installed.config), { mode: 0o700 });
  fs.writeFileSync(installed.config, 'wallet policy');
  const lock = path.join(path.dirname(installed.config), 'unresolved.lock'); fs.writeFileSync(lock, 'pending');
  prepare(f.pkg, f.home, f.key);
  assert.equal(fs.readFileSync(flow, 'utf8'), 'my strategy');
  assert.equal(fs.readFileSync(installed.config, 'utf8'), 'wallet policy');
  assert.equal(fs.readFileSync(lock, 'utf8'), 'pending');
  fs.rmSync(f.pkg, { recursive: true });
  assert.ok(fs.existsSync(installed.binary), 'npm cache eviction must not remove stable Flow binary');
  assert.equal(fs.readFileSync(path.join(path.dirname(installed.binary), 'run-cycle.sh'), 'utf8'), '#!/bin/bash\nexit 0\n');
});
test('rejects corrupted binaries, replaced installed binary, and unsupported platforms', t => {
  const f = fixture(t); const installed = prepare(f.pkg, f.home, f.key);
  fs.writeFileSync(installed.binary, 'tampered');
  assert.throws(() => prepare(f.pkg, f.home, f.key), /校验失败/);
  fs.writeFileSync(path.join(f.pkg, 'native', f.key, 'flow-bnb'), 'corrupt download');
  assert.throws(() => prepare(f.pkg, f.home, f.key), /校验失败/);
  assert.throws(() => platformKey('win32', 'x64'), /暂不支持/);
  assert.throws(() => locations('relative'), /绝对路径/);
});
test('read-only status does not create installation state; symlinks are rejected', t => {
  const f = fixture(t);
  assert.throws(() => prepare(f.pkg, f.home, f.key, false));
  assert.ok(!fs.existsSync(f.home));
  fs.symlinkSync(f.root, f.home);
  assert.throws(() => prepare(f.pkg, f.home, f.key), /私有目录/);
});
test('rejects a modified installed cycle runner', t => {
  const f = fixture(t); const installed = prepare(f.pkg, f.home, f.key);
  fs.writeFileSync(path.join(path.dirname(installed.binary), 'run-cycle.sh'), 'modified');
  assert.throws(() => prepare(f.pkg, f.home, f.key), /校验失败/);
});
test('npm-style symlink entry forwards MCP stdin/stdout and exit code, never logs on stdout', t => {
  const f = fixture(t); prepare(f.pkg, f.home, f.key);
  const bin = path.join(f.root, 'flow-bnb-desktop'); fs.symlinkSync(path.join(f.pkg, 'launcher.mjs'), bin);
  const message = '{"jsonrpc":"2.0","method":"initialize","id":1}\n';
  const out = spawnSync(process.execPath, [bin, 'mcp'], { env: { ...process.env, FLOW_BNB_HOME: f.home }, input: message, encoding: 'utf8' });
  assert.equal(out.status, 0, out.stderr); assert.equal(out.stdout, message); assert.equal(out.stderr, '');
  const status = spawnSync(process.execPath, [bin, 'status'], { env: { ...process.env, FLOW_BNB_HOME: f.home }, encoding: 'utf8' });
  assert.equal(status.status, 0, status.stderr); assert.equal(JSON.parse(status.stdout).connected, true);
  const bad = spawnSync(process.execPath, [bin, 'invalid'], { env: { ...process.env, FLOW_BNB_HOME: f.home }, encoding: 'utf8' });
  assert.equal(bad.status, 1); assert.equal(bad.stdout, '');
});
