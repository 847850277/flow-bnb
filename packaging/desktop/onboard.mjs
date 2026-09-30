import * as fs from 'node:fs';
import path from 'node:path';
import { createInterface } from 'node:readline/promises';
import { clients, server, plan, commit, exportConfigs } from './clients.mjs';

export async function onboard(args, paths, runNative) {
  let selected = [], project, config, dry = false, noLogin = false, list = false;
  for (let i = 0; i < args.length; i++) {
    switch (args[i]) {
      case '--client': if (!args[i + 1] || args[i + 1].startsWith('--')) throw new Error('--client 缺少客户端 ID'); selected.push(...args[++i].split(',')); break;
      case '--project': if (!args[i + 1] || args[i + 1].startsWith('--')) throw new Error('--project 缺少路径'); project = path.resolve(args[++i]); break;
      case '--config': if (!args[i + 1] || args[i + 1].startsWith('--')) throw new Error('--config 缺少路径'); config = path.resolve(args[++i]); break;
      case '--dry-run': dry = true; break;
      case '--no-login': noLogin = true; break;
      case '--list': list = true; break;
      default: throw new Error(`未知安装选项：${args[i]}`);
    }
  }
  const available = clients({ project });
  if (list) {
    console.log(available.map(c => `${c.id.padEnd(18)} ${c.name}${c.detected ? ' [检测到配置目录]' : ''}`).join('\n'));
    console.log('roo / continue 需加 --project <项目路径>；generic 导出通用配置。\nWorkBuddy 请导入专用连接器；Claude Desktop 也可导入 .mcpb。'); return 0;
  }
  if (!selected.length) {
    if (!process.stdin.isTTY) throw new Error('非交互安装请指定 --client，例如 --client codex,cursor；--list 查看列表');
    console.error('\n选择要连接的客户端（多个 ID 用逗号分隔）：');
    available.forEach(c => console.error(`  ${c.id.padEnd(18)} ${c.name}${c.detected ? ' ✓' : ''}`));
    console.error('  generic            其他客户端：导出通用 stdio 配置');
    const rl = createInterface({ input: process.stdin, output: process.stderr });
    try { selected = (await rl.question('客户端 ID：')).trim().split(',').map(s => s.trim()).filter(Boolean); }
    finally { rl.close(); }
    if (!selected.length) throw new Error('未选择客户端，未修改客户端配置');
  }
  selected = [...new Set(selected)];
  if (config && selected.length !== 1) throw new Error('--config 只支持一次指定一个客户端');
  const definition = server(paths);
  const receiptFile = path.join(paths.home, 'client-receipts.json');
  let receipts = {};
  if (fs.existsSync(receiptFile)) {
    if (fs.lstatSync(receiptFile).isSymbolicLink()) throw new Error('安装记录不能是软链接');
    receipts = JSON.parse(fs.readFileSync(receiptFile, 'utf8'));
  }
  const planned = selected.filter(id => id !== 'generic').map(id => {
    const c = available.find(c => c.id === id);
    if (!c) throw new Error(`未知或当前平台不可用的客户端：${id}；使用 --list 查看，Roo/Continue 需 --project`);
    return plan(config ? { ...c, file: config } : c, definition, receipts);
  });
  if (dry) {
    console.log(JSON.stringify({ dry_run: true, clients: planned.map(p => ({ client: p.client.id, file: p.client.file, changed: p.before !== p.after })), server: definition }, null, 2)); return 0;
  }
  // Validate every requested target before writing any client configuration.
  for (const p of planned) {
    const result = commit(p); receipts[p.client.file] = p.entry;
    const temp = `${receiptFile}.${process.pid}.tmp`;
    fs.writeFileSync(temp, JSON.stringify(receipts, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
    fs.renameSync(temp, receiptFile);
    console.error(`✓ ${p.client.name}：${result.changed ? '已配置' : '已是最新'} ${result.file}${result.backup ? `\n  原配置备份：${result.backup}` : ''}`);
  }
  const exports = path.join(paths.home, 'client-configs');
  fs.mkdirSync(exports, { recursive: true, mode: 0o700 });
  if (fs.lstatSync(exports).isSymbolicLink()) throw new Error('配置导出目录不能是软链接');
  for (const [name, data] of Object.entries(exportConfigs(definition))) {
    const target = path.join(exports, name);
    if (fs.existsSync(target) && fs.lstatSync(target).isSymbolicLink()) throw new Error('配置导出文件不能是软链接');
    fs.writeFileSync(target, typeof data === 'string' ? data + '\n' : JSON.stringify(data, null, 2) + '\n', { mode: 0o600 });
  }
  console.error(`\n通用配置已导出：${exports}\n请重启所选客户端，按客户端提示启用 Flow BNB。`);
  if (!noLogin) {
    console.error('\n现在准备依赖并连接 Agentic Wallet；请在手机上完成一次登录。');
    const code = await runNative(['setup']);
    if (code) { console.error('客户端配置已保留。登录未完成，可在对话中说“连接 Flow BNB 钱包”重试。'); return code; }
  } else console.error('已跳过钱包登录。可以先创建/校验策略；交易前在对话中说“连接 Flow BNB 钱包”。');
  return 0;
}
