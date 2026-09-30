import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { parse } from 'jsonc-parser';
import TOML from '@iarna/toml';
import { clients, server, plan, commit, merge, entry, exportConfigs } from './clients.mjs';
function fixture(t) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "flow clients ' ")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  return { root, definition: server({ binary: path.join(root, 'flow bnb'), workspace: path.join(root, 'workspace'), config: path.join(root, 'private wallet.json') }) };
}
test('all macOS/Linux profiles preserve unrelated settings and reinstall idempotently', t => {
  const { root, definition } = fixture(t);
  for (const platform of ['darwin', 'linux']) {
    const home = path.join(root, platform); fs.mkdirSync(home);
    for (const c of clients({ home, platform, env: {}, project: home })) {
      fs.mkdirSync(path.dirname(c.file), { recursive: true });
      const before = c.format === 'toml' ? '# preserve me\nmodel = "my-model"\n[mcp_servers.other]\ncommand = "other"\n' : c.format === 'yaml' ? '' : `// preserve me\n{ "theme": "dark", "${c.key}": {"other": {"command":"other"}}, }\n`;
      if (before) fs.writeFileSync(c.file, before);
      const first = plan(c, definition); const result = commit(first);
      assert.equal(result.changed, true, c.id);
      if (before) {
        assert.equal(fs.readFileSync(result.backup, 'utf8'), before);
        assert.equal(fs.statSync(result.backup).mode & 0o777, 0o600);
        assert.match(first.after, /preserve me/);
      }
      const parsed = c.format === 'toml' ? TOML.parse(first.after) : parse(first.after);
      if (c.format === 'toml') assert.equal(parsed.mcp_servers.other.command, 'other');
      else if (c.format === 'json') { assert.equal(parsed.theme, 'dark'); assert.equal(parsed[c.key].other.command, 'other'); }
      assert.equal(commit(plan(c, definition)).changed, false, c.id);
      const upgraded = { ...definition, command: definition.command + '-v2' };
      assert.throws(() => plan(c, upgraded), /未覆盖|保留原样/);
      const p = plan(c, upgraded, { [c.file]: entry(c, definition) });
      commit(p); assert.equal(commit(plan(c, upgraded)).changed, false, c.id);
    }
  }
});
test('refuses invalid, duplicate, colliding or redirected settings; detects concurrent edits', t => {
  const { root, definition } = fixture(t);
  const c = clients({ home: root, platform: 'darwin', env: {} }).find(c => c.id === 'cursor');
  for (const bad of ['{"mcpServers":', '{"a":1,"a":2}', '{"mcpServers":[]}', '{"mcpServers":{"flow-bnb":{"command":"custom"}}}']) assert.throws(() => merge(c, bad, definition));
  fs.mkdirSync(path.dirname(c.file), { recursive: true });
  fs.writeFileSync(c.file, '{}'); const p = plan(c, definition);
  fs.writeFileSync(c.file, '{"changed":true}'); assert.throws(() => commit(p), /另一个进程/);
  assert.equal(fs.readFileSync(c.file, 'utf8'), '{"changed":true}');
  fs.unlinkSync(c.file); fs.symlinkSync(path.join(root, 'target'), c.file);
  assert.throws(() => plan(c, definition), /软链接/);
  const codex = clients({ home: root, platform: 'linux', env: {} }).find(c => c.id === 'codex');
  assert.throws(() => merge(codex, 'bad = [', definition));
});
test('profile schemas, explicit environment overrides, exports and platform boundaries', t => {
  const { root, definition } = fixture(t);
  const list = clients({ home: root, platform: 'darwin', env: { CODEX_HOME: root + '/custom', XDG_CONFIG_HOME: root + '/xdg' } });
  assert.equal(list.find(c => c.id === 'codex').file, root + '/custom/config.toml');
  assert.equal(list.find(c => c.id === 'devin').file, root + '/xdg/devin/mcp_config.json');
  const vscode = list.find(c => c.id === 'vscode'); assert.equal(vscode.key, 'servers'); assert.equal(entry(vscode, definition).type, 'stdio');
  const opencode = entry(list.find(c => c.id === 'opencode'), definition);
  assert.deepEqual(opencode.command, [definition.command, ...definition.args]); assert.deepEqual(opencode.environment, definition.env);
  assert.throws(() => clients({ platform: 'win32' }), /Windows/);
  assert.ok(!clients({ platform: 'linux', home: root, env: {} }).some(c => c.id === 'claude-desktop'));
  const exported = exportConfigs(definition);
  assert.deepEqual(exported['generic.json'].mcpServers['flow-bnb'], definition);
  const link = new URL(exported['cursor-install-link.txt']);
  assert.deepEqual(JSON.parse(Buffer.from(link.searchParams.get('config'), 'base64').toString()), definition);
  assert.ok(!JSON.stringify(exported).includes('autoApprove'));
});
