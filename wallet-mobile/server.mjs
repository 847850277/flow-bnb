import { parseArgs } from 'node:util';
import { resolve, join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { mkdirSync, chmodSync, existsSync } from 'node:fs';
import { randomBytes } from 'node:crypto';
import { startBridge, rpcReader, persist } from './bridge.mjs';
import { writeSignerExecutable } from './signer.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const { values } = parseArgs({ options: {
  account: { type: 'string' }, rpc: { type: 'string', default: 'https://bsc-dataseed.bnbchain.org' },
  'state-dir': { type: 'string', default: resolve(here, '../.flow-bnb/mobile-wallet') },
  port: { type: 'string', default: '0' }, 'max-gas': { type: 'string', default: '600000' },
  'max-gas-price-wei': { type: 'string', default: '1000000000' },
  'max-fee-wei': { type: 'string', default: '100000000000000' }, help: { type: 'boolean' }
} });
if (values.help || !values.account) {
  console.log('Usage: npm start -- --account 0xYOUR_BSC_ADDRESS [--rpc HTTPS_URL] [--port 0]\nOptional fee limits: --max-gas 600000 --max-gas-price-wei 1000000000 --max-fee-wei 100000000000000\nThis starts a loopback connection page. Connecting never requests a signature or submits a transaction.');
  process.exit(values.help ? 0 : 1);
}
if (!existsSync(join(here, 'dist/app.js'))) throw new Error('Run npm ci --ignore-scripts && npm run build in wallet-mobile first');
const stateDir = resolve(values['state-dir']); mkdirSync(stateDir, { recursive: true, mode: 0o700 }); chmodSync(stateDir, 0o700);
const sessionDir = join(stateDir, 'session-' + randomBytes(8).toString('hex')); mkdirSync(sessionDir, { mode: 0o700 });
const bridge = await startBridge({ account: values.account, stateDir, assetsDir: join(here, 'dist'), rpc: rpcReader(values.rpc), port: Number(values.port), limits: { maxGas: Number(values['max-gas']), maxGasPriceWei: values['max-gas-price-wei'], maxFeeWei: values['max-fee-wei'] } });
const configPath = join(sessionDir, 'signer.json');
persist(configPath, { origin: bridge.origin, token: bridge.signerToken, account: values.account }, true);
const executable = join(sessionDir, 'flow-bnb-wallet-mobile');
// Absolute Node interpreter works even when Rust clears the child's environment.
writeSignerExecutable(executable, configPath);
const url = `${bridge.origin}/#${bridge.browserToken}`;
persist(join(sessionDir, 'connection.json'), { url, signer: executable }, true);
console.log(`\nFlow BNB · 手机钱包连接\n\n在电脑浏览器打开（本机私有链接，请勿分享）：\n${url}\n\n预期地址：${values.account}\n网络：BSC 主网（56）\n最大单笔手续费：${values['max-fee-wei']} wei\n\n连接手机后，execute-trade 的 --signer 使用：\n${executable}\n\n连接操作不会签名或广播。保持本终端和浏览器打开。\n审计记录：${join(stateDir, 'requests')}\n`);
let stopping = false;
async function stop() { if (stopping) return; stopping = true; await bridge.close(); process.exit(0); }
process.on('SIGINT', stop); process.on('SIGTERM', stop);
