// Exercise the actual tarball through npm's bin symlink and real MCP protocol.
// No wallet login, real account access, network API call or transaction occurs.
import * as fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { createInterface } from 'node:readline';
import { pathToFileURL } from 'node:url';
import { CallToolResultSchema, ListToolsResultSchema } from '@modelcontextprotocol/sdk/types.js';
import { AjvJsonSchemaValidator } from '@modelcontextprotocol/sdk/validation/ajv-provider.js';
const output = path.resolve(process.argv[2] || 'dist/release');
const archive = fs.readdirSync(output).find(f => f.endsWith('.tgz'));
assert.ok(archive, 'build a tarball first');
const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'flow-packaged-'));
let child;
try {
  const runtime = path.join(scratch, 'runtime'); fs.mkdirSync(runtime);
  fs.symlinkSync(process.execPath, path.join(runtime, 'node'));
  const env = { ...process.env, PATH: `${runtime}:/usr/bin:/bin`, FLOW_BNB_HOME: path.join(scratch, 'user-data'), npm_config_cache: path.join(scratch, 'npm-cache') };
  delete env.FLOW_BNB_AGENTIC_CONFIG; delete env.BINANCE_WEB3_API_KEY; delete env.BINANCE_WEB3_SECRET_KEY;
  delete env.FLOW_BNB_HANDOFF_DIR; delete env.FLOW_BNB_POLICY_FILE;
  const npm = fs.realpathSync(path.join(path.dirname(process.execPath), 'npm'));
  const install = spawnSync(process.execPath, [npm, 'install', '--offline', '--ignore-scripts', '--no-audit', '--no-fund', '--prefix', path.join(scratch, 'npm'), path.join(output, archive)], { env, encoding: 'utf8', timeout: 60000 });
  assert.equal(install.status, 0, install.stderr);
  const launcher = path.join(scratch, 'npm/node_modules/.bin/flow-bnb-desktop');
  // A not-yet-connected account is reported without creating wallet state.
  const status = spawnSync(launcher, ['status'], { env, encoding: 'utf8', timeout: 10000 });
  assert.equal(status.status, 1); assert.equal(JSON.parse(status.stdout).connected, false);
  assert.ok(!fs.existsSync(env.FLOW_BNB_HOME));
  child = spawn(launcher, ['mcp'], { env, stdio: ['pipe', 'pipe', 'pipe'] });
  let stderr = ''; child.stderr.on('data', b => { stderr += b; });
  const lines = createInterface({ input: child.stdout });
  const pending = new Map(); let sequence = 0;
  lines.on('line', line => {
    const msg = JSON.parse(line); // Any banner on stdout fails acceptance.
    const complete = pending.get(msg.id); if (complete) complete(msg);
  });
  async function call(method, params) {
    const id = ++sequence;
    const response = await new Promise((resolve, reject) => {
      const timer = setTimeout(() => { pending.delete(id); reject(new Error(`MCP timeout: ${stderr}`)); }, 15000);
      pending.set(id, msg => { clearTimeout(timer); pending.delete(id); resolve(msg); });
      child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
    });
    assert.ok(!response.error, JSON.stringify(response.error)); return response.result;
  }
  await call('initialize', { protocolVersion: '2025-11-25', capabilities: {}, clientInfo: { name: 'packaging-acceptance', version: '1' } });
  child.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n');
  // Strict SDK validation catches malformed outputSchema before clients enable
  // ANY tools (including Value roots and boolean property schemas in v0.1.2).
  const listed = ListToolsResultSchema.parse(await call('tools/list', {}));
  const validator = new AjvJsonSchemaValidator();
  const outputValidators = new Map(listed.tools.map(tool => {
    assert.ok(tool.outputSchema, `${tool.name} must describe its structured output`);
    return [tool.name, validator.getValidator(tool.outputSchema)];
  }));
  async function tool(name, args = {}) {
    const result = CallToolResultSchema.parse(await call('tools/call', { name, arguments: args }));
    assert.notEqual(result.isError, true, JSON.stringify(result));
    const checked = outputValidators.get(name)(result.structuredContent);
    assert.equal(checked.valid, true, `${name}: ${checked.errorMessage}`);
    assert.deepEqual(JSON.parse(result.content.find(c => c.type === 'text').text), result.structuredContent);
    return result.structuredContent;
  }
  assert.ok(listed.tools.some(t => t.name === 'execute_bnb_authorized_strategy'));
  assert.ok(listed.tools.some(t => t.name === 'connect_bnb_wallet'));
  const connection = await tool('get_bnb_connection');
  assert.equal(connection.connected, false);
  const generated = await tool('generate_bnb_flow', { template: 'stock_strategy' });
  const saved = await tool('save_bnb_flow', { path: 'flows/schema-check.http.yml', yaml: generated.canonical_yaml });
  const read = await tool('read_bnb_flow', { path: saved.saved_path });
  assert.equal(read.sha256, saved.sha256);
  assert.equal(read.yaml, generated.canonical_yaml);
  assert.ok(fs.existsSync(path.join(env.FLOW_BNB_HOME, 'workspace/flows/stock_strategy.http.yml')));
  assert.ok(!fs.existsSync(path.join(env.FLOW_BNB_HOME, 'workspace/.flow-bnb/agentic.json')));
  const closed = new Promise(resolve => child.once('exit', resolve)); child.stdin.end();
  const timeout = setTimeout(() => child.kill('SIGTERM'), 3000);
  await closed; clearTimeout(timeout); lines.close();
  // Check packaged paths, login preAuth wiring and version lock without a live WorkBuddy client.
  const connector = path.join(output, 'flow-bnb-connector');
  const mcp = JSON.parse(fs.readFileSync(path.join(connector, 'mcp.json')));
  const cli = JSON.parse(fs.readFileSync(path.join(connector, 'cli.json')));
  assert.equal(mcp.preAuth, 'cli'); assert.equal(cli.authWaitForExit, true);
  assert.equal(Object.keys(mcp.mcpServers).length, 1);
  assert.ok(new RegExp(cli.statusMatch).test('{"connected":true}'));
  assert.ok(!new RegExp(cli.statusMatch).test('{"connected":false}'));
  // Exercise the bundled parsers against synthetic client homes, not real clients.
  const adapters = await import(pathToFileURL(path.join(scratch, 'npm/node_modules/@flow-bnb/desktop/clients.mjs')));
  const definitions = adapters.server({ binary: '/test/flow-bnb', workspace: '/test/workspace', config: '/test/private.json' });
  const fakeHome = fs.realpathSync(scratch);
  for (const c of adapters.clients({ home: fakeHome, platform: process.platform, env: {}, project: fakeHome })) {
    const result = adapters.commit(adapters.plan(c, definitions));
    assert.equal(result.changed, true);
    assert.equal(adapters.commit(adapters.plan(c, definitions)).changed, false);
  }
  // Universal shell entry: real tarball + real native bootstrap; only Node is prepared.
  const shellHome = path.join(fs.realpathSync(scratch), 'shell-data');
  const clientFile = path.join(fs.realpathSync(scratch), 'shell-client.json');
  const shell = spawnSync('/bin/sh', [path.join(output, 'install-flow-bnb.sh'), '--client', 'cursor', '--config', clientFile, '--no-login'], {
    env: { ...env, PATH: `${path.dirname(process.execPath)}:/usr/bin:/bin`, FLOW_BNB_HOME: shellHome }, encoding: 'utf8', timeout: 30000
  });
  assert.equal(shell.status, 0, shell.stderr);
  const registered = JSON.parse(fs.readFileSync(clientFile)).mcpServers['flow-bnb'];
  assert.ok(fs.existsSync(registered.command));
  assert.ok(!fs.existsSync(registered.env.FLOW_BNB_AGENTIC_CONFIG));
  assert.ok(!fs.existsSync(path.join(shellHome, 'workspace/.flow-bnb/managed/baw-1.10.0')));
  const bundle = JSON.parse(fs.readFileSync(path.join(output, 'claude-bundle/manifest.json')));
  assert.equal(bundle.manifest_version, '0.3'); assert.equal(bundle.server.type, 'node');
  console.log(`Packaged MCP accepted: ${listed.tools.length} tools validated by the official SDK; structured output and text fallback verified; no Rust, wallet access, or external API required.`);
} finally {
  if (child && child.exitCode === null) child.kill('SIGTERM');
  fs.rmSync(scratch, { recursive: true, force: true });
}
