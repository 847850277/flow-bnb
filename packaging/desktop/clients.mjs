// Bundled by build.mjs: parsers are maintainer dependencies, not user dependencies.
import * as fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { randomUUID } from 'node:crypto';
import { isDeepStrictEqual } from 'node:util';
import { parse, parseTree, modify, applyEdits } from 'jsonc-parser';
import TOML from '@iarna/toml';

export function clients({ home = os.homedir(), platform = process.platform, env = process.env, project } = {}) {
  if (!['darwin', 'linux'].includes(platform)) throw new Error('当前安装器支持 macOS/Linux；Windows 原生后端尚未支持。');
  const h = (...p) => path.join(home, ...p);
  const xdg = env.XDG_CONFIG_HOME || h('.config');
  const app = platform === 'darwin' ? h('Library', 'Application Support') : xdg;
  const json = (id, name, file, extra = {}) => ({ id, name, file, format: 'json', key: 'mcpServers', ...extra });
  const list = [
    { id: 'codex', name: 'Codex', file: path.join(env.CODEX_HOME || h('.codex'), 'config.toml'), format: 'toml' },
    json('claude-code', 'Claude Code', env.CLAUDE_CONFIG_DIR ? path.join(env.CLAUDE_CONFIG_DIR, '.claude.json') : h('.claude.json')),
    json('copilot', 'GitHub Copilot CLI', h('.copilot', 'mcp-config.json'), { type: 'local' }),
    json('cursor', 'Cursor', h('.cursor', 'mcp.json')),
    json('vscode', 'VS Code / Copilot', path.join(app, 'Code', 'User', 'mcp.json'), { key: 'servers', type: 'stdio' }),
    json('vscode-insiders', 'VS Code Insiders', path.join(app, 'Code - Insiders', 'User', 'mcp.json'), { key: 'servers', type: 'stdio' }),
    json('windsurf', 'Windsurf (legacy)', h('.codeium', 'windsurf', 'mcp_config.json')),
    json('devin', 'Devin Desktop', path.join(xdg, 'devin', 'mcp_config.json')),
    json('cline', 'Cline CLI', h('.cline', 'mcp.json')),
    json('gemini', 'Gemini CLI', h('.gemini', 'settings.json')),
    json('kiro', 'Kiro', h('.kiro', 'settings', 'mcp.json')),
    json('qoder', 'Qoder CLI', h('.qoder', 'settings.json')),
    json('opencode', 'OpenCode', env.OPENCODE_CONFIG || path.join(xdg, 'opencode', fs.existsSync(path.join(xdg, 'opencode', 'opencode.jsonc')) ? 'opencode.jsonc' : 'opencode.json'), { key: 'mcp', type: 'local' }),
  ];
  if (platform === 'darwin') list.push(json('claude-desktop', 'Claude Desktop', path.join(app, 'Claude', 'claude_desktop_config.json')));
  if (project) {
    if (!path.isAbsolute(project) || !fs.statSync(project).isDirectory()) throw new Error('--project 须指向已有项目的绝对路径');
    list.push(json('roo', 'Roo Code (project)', path.join(project, '.roo', 'mcp.json')));
    list.push({ id: 'continue', name: 'Continue (project)', file: path.join(project, '.continue', 'mcpServers', 'flow-bnb.yaml'), format: 'yaml' });
  }
  return list.map(c => ({ ...c, detected: fs.existsSync(c.file) || fs.existsSync(path.dirname(c.file)) }));
}
export function server(paths) {
  return { command: paths.binary, args: ['mcp', '--root', paths.workspace], env: { FLOW_BNB_AGENTIC_CONFIG: paths.config } };
}
export function entry(client, definition) {
  if (client.id === 'copilot') return { type: 'local', ...definition, tools: ['*'] };
  if (client.id === 'opencode') return { type: 'local', command: [definition.command, ...definition.args], environment: definition.env, enabled: true };
  return client.type ? { type: client.type, ...definition } : definition;
}
function object(v) { return v !== null && typeof v === 'object' && !Array.isArray(v); }
function jsonDocument(text) {
  const errors = []; const result = parse(text, errors, { allowTrailingComma: true });
  if (errors.length || !object(result)) throw new Error('配置不是有效的 JSON/JSONC 对象，未修改');
  function check(node) {
    if (node?.type === 'object') {
      const keys = node.children.map(p => p.children[0].value);
      if (new Set(keys).size !== keys.length) throw new Error('配置存在重复键，未修改');
    }
    for (const child of node?.children || []) check(child);
  }
  check(parseTree(text)); return result;
}
const start = '# BEGIN Flow BNB managed MCP\n';
const end = '# END Flow BNB managed MCP\n';
export function merge(client, before, definition, previous) {
  const wanted = entry(client, definition);
  const allow = existing => {
    if (existing !== undefined && !isDeepStrictEqual(existing, wanted) && !isDeepStrictEqual(existing, previous))
      throw new Error(`${client.name} 已有不同的 flow-bnb 配置，保留原样；请核对后手动移除冲突项`);
  };
  if (client.format === 'yaml') {
    // JSON is a YAML subset; this is a dedicated file, not Continue's main config.
    const document = { name: 'Flow BNB', version: '1.0.0', schema: 'v1', mcpServers: [{ name: 'flow-bnb', ...wanted }] };
    const next = JSON.stringify(document, null, 2) + '\n';
    if (before && before !== next) {
      let old; try { old = JSON.parse(before); } catch { throw new Error('Continue 已有自定义 YAML，未覆盖'); }
      if (!isDeepStrictEqual(old, { ...document, mcpServers: [{ name: 'flow-bnb', ...previous }] })) throw new Error('Continue 已有自定义配置，未覆盖');
    }
    return next;
  }
  if (client.format === 'toml') {
    const data = TOML.parse(before || '');
    const existing = data.mcp_servers?.['flow-bnb']; allow(existing);
    if (isDeepStrictEqual(existing, wanted)) return before;
    let base = before || '';
    if (existing !== undefined) {
      const first = base.indexOf(start); const last = base.indexOf(end, first);
      if (first < 0 || last < 0 || base.indexOf(start, first + 1) >= 0) throw new Error('Codex 配置不是安装器管理的区块，未覆盖');
      base = base.slice(0, first) + base.slice(last + end.length);
      if (TOML.parse(base).mcp_servers?.['flow-bnb'] !== undefined) throw new Error('Codex 配置区块冲突');
    }
    const next = base + (base.endsWith('\n') ? '\n' : '\n\n') + start + TOML.stringify({ mcp_servers: { 'flow-bnb': wanted } }) + end;
    const parsed = TOML.parse(next);
    if (!isDeepStrictEqual(parsed.mcp_servers['flow-bnb'], wanted)) throw new Error('Codex 配置合并验证失败');
    return next;
  }
  const text = before || '{}\n'; const data = jsonDocument(text);
  if (data[client.key] !== undefined && !object(data[client.key])) throw new Error(`${client.key} 必须是对象`);
  const existing = data[client.key]?.['flow-bnb']; allow(existing);
  if (isDeepStrictEqual(existing, wanted)) return before;
  const next = applyEdits(text, modify(text, [client.key, 'flow-bnb'], wanted, { formattingOptions: { insertSpaces: true, tabSize: 2, eol: text.includes('\r\n') ? '\r\n' : '\n' } }));
  if (!isDeepStrictEqual(jsonDocument(next)[client.key]['flow-bnb'], wanted)) throw new Error('配置合并验证失败');
  return next;
}
function safePath(file) {
  if (!path.isAbsolute(file)) throw new Error('配置路径必须是绝对路径');
  for (let p = file; p !== path.dirname(p); p = path.dirname(p)) {
    try { if (fs.lstatSync(p).isSymbolicLink()) throw new Error(`配置路径包含软链接，未修改：${p}`); }
    catch (e) { if (e.code !== 'ENOENT') throw e; }
  }
}
function read(file) {
  try {
    const s = fs.lstatSync(file);
    if (!s.isFile() || s.isSymbolicLink() || s.size > 4 * 1024 * 1024) throw new Error(`配置文件类型或大小异常：${file}`);
    return fs.readFileSync(file, 'utf8');
  } catch (e) { if (e.code === 'ENOENT') return ''; throw e; }
}
export function plan(client, definition, receipts = {}) {
  safePath(client.file);
  const before = read(client.file);
  const after = merge(client, before, definition, receipts[client.file]);
  return { client, before, after, entry: entry(client, definition) };
}
export function commit(planned) {
  const { client, before, after } = planned;
  if (before === after) return { client: client.id, file: client.file, changed: false };
  safePath(client.file);
  fs.mkdirSync(path.dirname(client.file), { recursive: true, mode: 0o700 });
  const lock = `${client.file}.flow-bnb.lock`;
  const fd = fs.openSync(lock, 'wx', 0o600);
  const temp = `${client.file}.${randomUUID()}.tmp`;
  let backup;
  try {
    if (read(client.file) !== before) throw new Error('配置被另一个进程修改，请关闭客户端后重试');
    if (before) { backup = `${client.file}.flow-bnb-${randomUUID()}.bak`; fs.writeFileSync(backup, before, { flag: 'wx', mode: 0o600 }); }
    fs.writeFileSync(temp, after, { flag: 'wx', mode: 0o600 });
    if (read(client.file) !== before) throw new Error('配置被另一个进程修改，请关闭客户端后重试');
    fs.renameSync(temp, client.file);
    return { client: client.id, file: client.file, changed: true, backup };
  } finally {
    fs.closeSync(fd); fs.unlinkSync(lock);
    if (fs.existsSync(temp)) fs.unlinkSync(temp);
  }
}
export function exportConfigs(definition) {
  return {
    'generic.json': { mcpServers: { 'flow-bnb': definition } },
    'vscode.json': { servers: { 'flow-bnb': { type: 'stdio', ...definition } } },
    'opencode.json': { mcp: { 'flow-bnb': entry({ id: 'opencode' }, definition) } },
    'codex.toml': TOML.stringify({ mcp_servers: { 'flow-bnb': definition } }),
    'continue.yaml': JSON.stringify({ name: 'Flow BNB', version: '1.0.0', schema: 'v1', mcpServers: [{ name: 'flow-bnb', ...definition }] }, null, 2),
    'cursor-install-link.txt': `cursor://anysphere.cursor-deeplink/mcp/install?name=flow-bnb&config=${encodeURIComponent(Buffer.from(JSON.stringify(definition)).toString('base64'))}`,
  };
}
