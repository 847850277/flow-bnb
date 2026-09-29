import { requireThat, validateWallet, validateRequest, hash } from './validation.mjs';
// The claim is persisted BEFORE eth_sendTransaction. A send is never retried.
export async function dispatch(provider, pending, api, expectedAccount, now = Date.now) {
  validateRequest(pending.request, expectedAccount);
  const check = async () => {
    const accounts = await provider.request({ method: 'eth_accounts' });
    const chain = await provider.request({ method: 'eth_chainId' });
    validateWallet(accounts, chain, expectedAccount);
    requireThat(now() < pending.deadline_at, '交易已过期，请回终端重新准备');
  };
  await check();
  const job = await api('/claim', { id: pending.id });
  let txHash;
  try {
    requireThat(job.id === pending.id && JSON.stringify(job.request) === JSON.stringify(pending.request), 'Claim does not match reviewed request');
    const tx = job.transaction;
    requireThat(tx.from === job.request.transaction.from && tx.to === job.request.transaction.to && tx.data === job.request.transaction.data && tx.value === '0x0' && tx.chainId === '0x38', 'Wallet payload changed');
    requireThat(tx.gas === pending.transaction.gas && tx.gasPrice === pending.transaction.gasPrice, 'Fee changed after review');
    await check();
    txHash = await provider.request({ method: 'eth_sendTransaction', params: [tx] });
    requireThat(hash(txHash), '钱包未返回有效交易哈希，请检查钱包记录');
  } catch (error) {
    // Reporting errors must not trigger a second send. A claimed failure is
    // conservatively unknown even if a wallet reports rejection.
    await api('/result', { id: job.id }).catch(() => {});
    throw error;
  }
  try { await api('/result', { id: job.id, tx_hash: txHash }); }
  catch { throw new Error(`钱包已返回哈希 ${txHash}，但本机记录失败。请保存哈希并人工核查，勿重试。`); }
  return txHash;
}
