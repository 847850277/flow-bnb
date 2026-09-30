// Maintainer-only packaging. Users receive compiled binaries, never a Rust build.
import * as fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { build } from 'esbuild';
const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
function option(name, fallback) { const i = args.indexOf(name); return i < 0 ? fallback : args[i + 1]; }
const binaries = path.resolve(option('--binaries', path.join(repo, 'dist', 'native')));
const output = path.resolve(option('--output', path.join(repo, 'dist', 'release')));
const local = args.includes('--local');
const pkg = JSON.parse(fs.readFileSync(path.join(repo, 'packaging/desktop/package.json')));
const cargo = fs.readFileSync(path.join(repo, 'Cargo.toml'), 'utf8').match(/^version = "([^"]+)"/m)[1];
if (cargo !== pkg.version) throw new Error('Cargo/npm versions differ');
if (fs.existsSync(output)) throw new Error(`Output already exists: ${output}; choose a new --output`);
const platforms = local ? [`${process.platform}-${process.arch}`] : ['darwin-arm64', 'darwin-x64', 'linux-arm64', 'linux-x64'];
for (const key of platforms) {
  const stat = fs.lstatSync(path.join(binaries, key, 'flow-bnb'));
  if (!stat.isFile() || stat.isSymbolicLink()) throw new Error(`Missing native binary: ${key}`);
}
const dest = path.join(output, 'package');
fs.mkdirSync(dest, { recursive: true });
fs.cpSync(path.join(repo, 'packaging/desktop'), dest, { recursive: true, filter: p => !p.endsWith('.test.mjs') });
// Ship a dependency-free runtime; JSONC/TOML parsers are bundled at release time.
await build({ entryPoints: [path.join(repo, 'packaging/desktop/clients.mjs')], outfile: path.join(dest, 'clients.mjs'), bundle: true, mainFields: ['module', 'main'], platform: 'node', format: 'esm', target: 'node18', banner: { js: 'import { createRequire } from "node:module"; const require = createRequire(import.meta.url);' } });
fs.copyFileSync(path.join(repo, 'LICENSE'), path.join(dest, 'LICENSE'));
fs.mkdirSync(path.join(dest, 'flows'));
const sha = file => createHash('sha256').update(fs.readFileSync(file)).digest('hex');
const manifest = { version: pkg.version, binaries: {}, flows: {}, scripts: {} };
for (const key of platforms) {
  const to = path.join(dest, 'native', key, 'flow-bnb');
  fs.mkdirSync(path.dirname(to), { recursive: true });
  fs.copyFileSync(path.join(binaries, key, 'flow-bnb'), to); fs.chmodSync(to, 0o755);
  manifest.binaries[key] = sha(to);
}
for (const file of fs.readdirSync(path.join(repo, 'flows')).filter(f => f.endsWith('.http.yml')).sort()) {
  const to = path.join(dest, 'flows', file);
  fs.copyFileSync(path.join(repo, 'flows', file), to); manifest.flows[file] = sha(to);
}
fs.mkdirSync(path.join(dest, 'scripts'));
for (const file of ['run-cycle.sh']) {
  const to = path.join(dest, 'scripts', file);
  fs.copyFileSync(path.join(repo, 'scripts', file), to); manifest.scripts[file] = sha(to);
}
fs.writeFileSync(path.join(dest, 'manifest.json'), JSON.stringify(manifest, null, 2) + '\n');
function run(cmd, args, cwd) {
  const p = spawnSync(cmd, args, { cwd, encoding: 'utf8' });
  if (p.status !== 0) throw new Error(`${cmd} failed: ${p.error?.message || p.stderr}`);
  return p.stdout;
}
const packed = JSON.parse(run('npm', ['pack', '--ignore-scripts', '--json', '--pack-destination', output], dest))[0].filename;
const archive = `flow-bnb-desktop-${pkg.version}.tgz`;
fs.renameSync(path.join(output, packed), path.join(output, archive));
// Local connector is an importable, offline-binary preview, not a published URL.
const url = `https://github.com/847850277/flow-bnb/releases/download/v${pkg.version}/${archive}`;
const command = local ? 'node' : 'npx';
const prefix = local ? [path.join(dest, 'launcher.mjs')] : ['--yes', '--package', url, 'flow-bnb-desktop'];
const quote = s => "'" + s.replaceAll("'", "'\\''") + "'";
const invoke = mode => [command, ...prefix, mode].map(quote).join(' ');
const connector = path.join(output, 'flow-bnb-connector');
fs.mkdirSync(connector);
const json = (file, data) => fs.writeFileSync(path.join(connector, file), JSON.stringify(data, null, 2) + '\n');
json('connector-meta.json', {
  name: 'Flow BNB', name_zh: 'Flow BNB', name_en: 'Flow BNB',
  description: 'Turn natural language into editable YAML stock-token workflows with persistent execution and settlement tracking.',
  description_zh: '将自然语言转为可编辑的 YAML 交易流程：跨资产条件、持仓衔接和止盈退出。支持模拟演示、真实交易和到账核对。',
  description_en: 'Turn natural language into editable YAML workflows with cross-asset conditions, position tracking and exits. Preview with simulation or execute real trades.',
  source: 'flow-bnb', type: 'mcp', version: pkg.version, minWorkbuddyVersion: '5.0.0',
  examples_zh: ['创建英伟达报价跌 2% 后买入苹果、持仓可卖报价涨 2% 后退出的 YAML 策略，先模拟演示', '查看这轮策略的阶段、实际到账数量和执行结果'],
  examples_en: ['Create YAML to buy AAPLon after a 2% NVDAon quote-price drop and exit at a 2% gain; simulate first', 'Show this workflow cycle, received inventory and execution results']
});
json('mcp.json', { preAuth: 'cli', mcpServers: { 'flow-bnb': {
  type: 'stdio', command, args: [...prefix, 'mcp'], runtime: { type: 'node', version: '22' },
  npmRegistry: 'https://registry.npmjs.org', timeout: 30000
} } });
const platformsCommand = mode => ({ darwin: invoke(mode), linux: invoke(mode) });
json('cli.json', {
  runtime: { type: 'node', version: '22' }, npmRegistry: 'https://registry.npmjs.org',
  init: platformsCommand('install'), auth: platformsCommand('login'),
  status: platformsCommand('status'), unAuth: platformsCommand('logout'),
  statusMatch: '"connected"\\s*:\\s*true', authUrlDomain: 'web3.binance.com', authWaitForExit: true
});
fs.copyFileSync(path.join(repo, 'packaging/workbuddy/icon.svg'), path.join(connector, 'icon.svg'));
run('zip', ['-q', '-r', path.join(output, 'flow-bnb-workbuddy.zip'), 'flow-bnb-connector'], output);
const digest = sha(path.join(output, archive));
const download = local ? 'echo "Place this installer next to ' + archive + '" >&2; exit 1' : `curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --retry 2 --max-time 180 --output "$flow_archive" ${quote(url)}`;
let installer = fs.readFileSync(path.join(repo, 'packaging/install.sh.in'), 'utf8');
for (const [key, value] of Object.entries({ ARCHIVE: archive, SHA256: digest, SHORT_SHA: digest.slice(0, 12), VERSION: pkg.version, DOWNLOAD: download })) installer = installer.replaceAll(`@${key}@`, value);
for (const name of ['install-flow-bnb.sh', 'install-flow-bnb.command']) fs.writeFileSync(path.join(output, name), installer, { mode: 0o755 });
// Claude Desktop bundles the Node launcher and all native binaries. Pair through MCP.
const bundle = path.join(output, 'claude-bundle');
fs.mkdirSync(bundle); fs.cpSync(dest, path.join(bundle, 'package'), { recursive: true });
fs.writeFileSync(path.join(bundle, 'manifest.json'), JSON.stringify({
  manifest_version: '0.3', name: 'flow-bnb', display_name: 'Flow BNB', version: pkg.version,
  description: 'Create auditable BNB strategies and execute within wallet authorization. Ask to connect your wallet after installation.',
  author: { name: 'Flow BNB contributors' },
  server: { type: 'node', entry_point: 'package/launcher.mjs', mcp_config: { command: 'node', args: ['${__dirname}/package/launcher.mjs', 'mcp'] } },
  compatibility: { platforms: platforms.some(p => p.startsWith('darwin')) ? ['darwin'] : ['linux'], runtimes: { node: '>=22' } }
}, null, 2) + '\n');
run('zip', ['-q', '-r', path.join(output, 'flow-bnb.mcpb'), 'manifest.json', 'package'], bundle);
fs.writeFileSync(path.join(output, 'SHA256SUMS'), [archive, 'flow-bnb-workbuddy.zip', 'flow-bnb.mcpb', 'install-flow-bnb.sh', 'install-flow-bnb.command'].map(f => `${sha(path.join(output, f))}  ${f}`).join('\n') + '\n');
console.log(JSON.stringify({ output, local, archive, connector, publicationRequired: !local }, null, 2));
