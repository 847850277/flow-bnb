#!/usr/bin/env node
// No postinstall scripts or shell interpolation. WorkBuddy supplies Node; the
// package supplies Flow. Wallet state lives outside npm's replaceable cache.
import { createHash, randomUUID } from 'node:crypto';
import * as fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';

const packageRoot = path.dirname(fileURLToPath(import.meta.url));
export function platformKey(platform = process.platform, arch = process.arch) {
  const key = `${platform}-${arch}`;
  if (!['darwin-arm64', 'darwin-x64', 'linux-arm64', 'linux-x64'].includes(key))
    throw new Error(`暂不支持 ${key}；请使用 macOS 或 Linux 的 arm64/x64 版本。`);
  return key;
}
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
function privateDir(dir) {
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
  const stat = fs.lstatSync(dir);
  if (!stat.isDirectory() || stat.isSymbolicLink() || (stat.mode & 0o077))
    throw new Error(`安装目录须为当前用户的私有目录（0700）：${dir}`);
}
function checkedFile(file, expected) {
  const stat = fs.lstatSync(file);
  if (!stat.isFile() || stat.isSymbolicLink() || hash(fs.readFileSync(file)) !== expected)
    throw new Error(`文件校验失败，未启动程序；请重新安装连接器：${file}`);
}
// Publish complete files without replacing an existing install or user strategy.
function publish(file, bytes, mode) {
  const temporary = `${file}.${randomUUID()}.tmp`;
  fs.writeFileSync(temporary, bytes, { flag: 'wx', mode });
  try {
    try { fs.linkSync(temporary, file); }
    catch (e) { if (e.code !== 'EEXIST') throw e; }
  } finally { fs.unlinkSync(temporary); }
}
export function locations(home = process.env.FLOW_BNB_HOME || path.join(os.homedir(), '.local', 'share', 'flow-bnb')) {
  if (!path.isAbsolute(home)) throw new Error('FLOW_BNB_HOME 必须是绝对路径');
  return { home, workspace: path.join(home, 'workspace'), config: path.join(home, 'workspace', '.flow-bnb', 'agentic.json') };
}
export function prepare(root = packageRoot, home = locations().home, key = platformKey(), write = true) {
  const manifest = JSON.parse(fs.readFileSync(path.join(root, 'manifest.json'), 'utf8'));
  const pkg = JSON.parse(fs.readFileSync(path.join(root, 'package.json'), 'utf8'));
  if (manifest.version !== pkg.version || !/^\d+\.\d+\.\d+(?:-[a-zA-Z0-9.-]+)?$/.test(pkg.version))
    throw new Error('安装包版本不一致');
  const digest = manifest.binaries?.[key];
  if (!/^[0-9a-f]{64}$/.test(digest || '')) throw new Error(`安装包不包含 ${key}`);
  const source = path.join(root, 'native', key, 'flow-bnb');
  checkedFile(source, digest);
  const dirs = locations(home);
  const versionDir = path.join(home, 'versions', `${pkg.version}-${key}`);
  const binary = path.join(versionDir, 'flow-bnb');
  if (write) {
    privateDir(home); privateDir(path.join(home, 'versions')); privateDir(versionDir);
    privateDir(dirs.workspace); privateDir(path.join(dirs.workspace, 'flows'));
    if (!fs.existsSync(binary)) publish(binary, fs.readFileSync(source), 0o700);
    for (const [name, checksum] of Object.entries(manifest.flows || {})) {
      if (!/^[a-z0-9_-]+\.http\.yml$/.test(name)) throw new Error('安装包包含非法模板路径');
      const input = path.join(root, 'flows', name);
      checkedFile(input, checksum);
      const dest = path.join(dirs.workspace, 'flows', name);
      // User-owned templates are never overwritten during updates/reinstallation.
      if (!fs.existsSync(dest)) publish(dest, fs.readFileSync(input), 0o600);
    }
  }
  checkedFile(binary, digest);
  return { ...dirs, binary };
}
export async function main(args = process.argv.slice(2)) {
  const mode = args[0] || 'mcp';
  // Status never downloads, installs, pairs a wallet, or writes configuration.
  if (!['install', 'login', 'status', 'logout', 'mcp', 'cli', 'onboard'].includes(mode))
    throw new Error('用法：flow-bnb-desktop [onboard --client <客户端>|install|login|status|logout|mcp|cli <Flow 命令>]');
  let paths;
  try { paths = prepare(packageRoot, locations().home, platformKey(), !['status', 'logout'].includes(mode)); }
  catch (error) {
    if (mode === 'logout' && error.code === 'ENOENT') { console.log('{\"connected\":false}'); return 0; }
    throw error;
  }
  const env = { ...process.env, FLOW_BNB_AGENTIC_CONFIG: paths.config,
    PATH: [path.dirname(process.execPath), process.env.PATH || '', '/usr/bin', '/bin'].join(path.delimiter) };
  if (mode === 'onboard') {
    const { onboard } = await import('./onboard.mjs');
    return onboard(args.slice(1), paths, nativeArgs => runNative(paths, env, nativeArgs));
  }
  let nativeArgs;
  switch (mode) {
    case 'install': nativeArgs = ['setup', '--no-login', '--no-open']; break;
    case 'login': nativeArgs = ['setup', '--no-open']; break;
    case 'status': nativeArgs = ['connection-status']; break;
    case 'logout': nativeArgs = ['disconnect']; break;
    case 'mcp': nativeArgs = ['mcp', '--root', paths.workspace]; break;
    case 'cli': nativeArgs = args.slice(1); break;
    default: throw new Error('用法：flow-bnb-desktop [onboard --client <客户端>|install|login|status|logout|mcp|cli <Flow 命令>]');
  }
  if (mode !== 'cli' && args.length > 1) throw new Error('该连接器命令不接受额外参数');
  return runNative(paths, env, nativeArgs);
}
export async function runNative(paths, env, nativeArgs) {
  return await new Promise((resolve, reject) => {
    const child = spawn(paths.binary, nativeArgs, { cwd: paths.workspace, env, stdio: 'inherit', detached: true });
    const forward = signal => {
      try { if (child.pid) process.kill(-child.pid, signal); }
      catch (e) { if (e.code !== 'ESRCH') throw e; }
    };
    const term = () => forward('SIGTERM'); const interrupt = () => forward('SIGINT');
    process.on('SIGTERM', term); process.on('SIGINT', interrupt);
    const cleanup = () => { process.off('SIGTERM', term); process.off('SIGINT', interrupt); };
    child.once('error', e => { cleanup(); reject(new Error(`Flow 无法启动：${e.message}`)); });
    child.once('exit', (code, signal) => { cleanup(); resolve(code ?? (signal === 'SIGINT' ? 130 : 143)); });
  });
}
if (process.argv[1] && fs.realpathSync(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().then(code => { process.exitCode = code; }).catch(error => {
    // stdout belongs exclusively to MCP (or structured status output).
    console.error(`Flow BNB：${error.message}`);
    if (process.argv[2] === 'status') console.log('{"connected":false}');
    process.exitCode = 1;
  });
}
