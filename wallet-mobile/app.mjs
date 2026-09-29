import { getProvider } from '@binance/w3w-ethereum-provider';
import { validateWallet } from './validation.mjs';
import { dispatch } from './controller.mjs';
const $ = id => document.getElementById(id);
const token = location.hash.slice(1);
history.replaceState(null, '', '/');
let provider, connected = false, busy = false, pending = null, expected = '', tickBusy = false;
const message = text => { $('message').textContent = text; };
async function api(path, value) {
  const response = await fetch(path, { method: value === undefined ? 'GET' : 'POST', headers: { Authorization: `Bearer ${token}`, ...(value === undefined ? {} : { 'Content-Type': 'application/json' }) }, body: value === undefined ? undefined : JSON.stringify(value), signal: AbortSignal.timeout(5000) });
  const result = await response.json();
  if (!response.ok) throw new Error(result.error || '本机服务不可用');
  return result;
}
async function identity() {
  const accounts = await provider.request({ method: 'eth_accounts' });
  const chain = await provider.request({ method: 'eth_chainId' });
  validateWallet(accounts, chain, expected);
  await api('/session', { accounts, chain });
  connected = true; $('status').textContent = '已连接 · BSC 主网';
}
async function invalidated() {
  connected = false; $('status').textContent = '已断开或账户 / 网络发生变化';
  $('send').disabled = true;
  await api('/session', { disconnected: true }).catch(() => {});
}
$('connect').onclick = async () => {
  $('connect').disabled = true;
  try {
    if (provider) { await invalidated(); provider.disconnect(); }
    provider = getProvider({ chainId: 56, showQrCodeModal: true });
    provider.setLng('zh-CN');
    provider.on('accountsChanged', invalidated); provider.on('chainChanged', invalidated); provider.on('disconnect', invalidated);
    message('请用币安 App 钱包扫描 SDK 弹窗中的二维码，并确认连接。此操作不转账。');
    await provider.enable(); await identity(); message('连接完成。可在另一个终端准备交易；签名请求会显示在这里。');
  } catch (error) { await invalidated(); message(error.message); }
  finally { $('connect').disabled = false; }
};
$('disconnect').onclick = async () => { await invalidated(); provider?.disconnect(); message('已断开。若已在手机确认，请先核查钱包和链上记录。'); };
$('send').onclick = async () => {
  if (busy || !pending || pending.state !== 'pending') return;
  busy = true; $('send').disabled = true; $('decline').disabled = true; $('connect').disabled = true;
  const job = pending;
  const expiry = setTimeout(() => {
    message('确认窗口已过期。如果手机还有待确认请求，请拒绝；已发送到钱包的请求不能保证撤销，请勿重复执行。');
    provider?.disconnect();
  }, Math.max(1, job.deadline_at - Date.now()));
  try {
    message('请在手机核对并确认。只会发送一次；拒签或超时后不会自动重试。');
    const txHash = await dispatch(provider, job, api, expected);
    message(`钱包返回交易哈希：${txHash}。请查看 CLI 的链上核验结果。`);
  } catch (error) { message(error.message + '。请检查钱包记录和本机审计记录。'); }
  finally { clearTimeout(expiry); busy = false; $('connect').disabled = false; await tick(); }
};
$('decline').onclick = async () => {
  if (busy || !pending) return;
  try { await api('/decline', { id: pending.id }); message('已拒绝，没有向手机发送交易请求。'); await tick(); }
  catch (error) { message(error.message); }
};
function render(job) {
  pending = job;
  const canSend = connected && !busy && job?.state === 'pending' && Date.now() < job.deadline_at;
  $('send').disabled = !canSend; $('decline').disabled = !canSend;
  $('request').hidden = !job;
  if (!job) { $('waiting').hidden = false; return; }
  $('waiting').hidden = true;
  const tx = job.request.transaction;
  $('kind').textContent = job.request.kind === 'approval' ? '代币授权' : '代币兑换';
  $('from').textContent = tx.from; $('to').textContent = tx.to; $('confirmation').textContent = job.id;
  $('payload').textContent = JSON.stringify(job.transaction ?? tx, null, 2);
  $('remaining').textContent = `${Math.max(0, Math.ceil((job.deadline_at - Date.now()) / 1000))} 秒 · ${job.state}`;
  $('fee').textContent = job.transaction ? `${BigInt(job.transaction.gas) * BigInt(job.transaction.gasPrice)} wei（按提交参数计算）` : '估算中';
  $('approval').hidden = job.request.kind !== 'approval';
  if (job.request.kind === 'approval') {
    $('spender').textContent = '0x' + tx.data.slice(34, 74);
    const amount = BigInt('0x' + tx.data.slice(74));
    $('amount').textContent = tx.to.toLowerCase() === '0x55d398326f99059ff775485246999027b3197955' ? `${amount / 10n**18n}.${(amount % 10n**18n).toString().padStart(18, '0').replace(/0+$/, '') || '0'} USDT` : `${amount} 最小单位（请在钱包核对精度）`;
  }
}
async function tick() {
  if (tickBusy) return; tickBusy = true;
  try {
    if (connected) { try { await identity(); } catch { await invalidated(); } }
    const state = await api('/state'); expected = state.account; $('account').textContent = expected;
    render(state.pending);
    $('history').textContent = state.recent.map(j => `${j.state}${j.tx_hash ? ' · ' + j.tx_hash : ''}${j.message ? '\n' + j.message : ''}`).join('\n\n');
  } catch (error) { connected = false; $('send').disabled = true; message(error.message); }
  finally { tickBusy = false; }
}
if (!/^[\da-f]{64}$/.test(token)) { $('connect').disabled = true; message('请使用启动命令打印的完整本机链接打开页面。刷新后也需重新打开该链接。'); }
else { await tick(); if (expected) message('本机服务已就绪。请先连接手机钱包。'); setInterval(tick, 1000); }
