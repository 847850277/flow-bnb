// Shared by the loopback server and the browser. Amounts remain exact integers.
export function requireThat(condition, message) {
  if (!condition) throw new Error(message);
}
export const address = value => typeof value === 'string' && /^0x[\da-f]{40}$/i.test(value) && !/^0x0{40}$/i.test(value);
export const hash = value => typeof value === 'string' && /^0x[\da-f]{64}$/i.test(value) && !/^0x0{64}$/i.test(value);
export function validateRequest(request, account) {
  requireThat(request && Object.keys(request).sort().join() === 'chain_id,confirmation_id,expires_in_ms,kind,protocol,transaction', 'Unexpected signer fields');
  requireThat(request.protocol === 'flow-bnb-signer-v1' && request.chain_id === 56, 'Expected BSC signer protocol');
  requireThat(['approval', 'swap'].includes(request.kind), 'Unsupported transaction kind');
  requireThat(typeof request.confirmation_id === 'string' && /^[\da-f]{64}$/i.test(request.confirmation_id), 'Invalid confirmation ID');
  requireThat(Number.isInteger(request.expires_in_ms) && request.expires_in_ms > 0 && request.expires_in_ms <= 30000, 'Expired or invalid deadline');
  const tx = request.transaction;
  requireThat(tx && Object.keys(tx).sort().join() === 'data,from,to,value', 'Unexpected transaction fields');
  requireThat(address(account) && address(tx.from) && tx.from.toLowerCase() === account.toLowerCase(), 'Wallet account mismatch');
  requireThat(address(tx.to) && tx.value === '0', 'Only zero-native-value ERC-20 transactions are supported');
  requireThat(typeof tx.data === 'string' && /^0x[\da-f]+$/i.test(tx.data) && tx.data.length >= 10 && tx.data.length <= 131074 && tx.data.length % 2 === 0, 'Invalid calldata');
  if (request.kind === 'approval') {
    requireThat(/^0x095ea7b3[\da-f]{128}$/i.test(tx.data), 'Invalid ERC-20 approval');
    requireThat(/^0{24}$/i.test(tx.data.slice(10, 34)) && address('0x' + tx.data.slice(34, 74)), 'Invalid approval spender');
    const amount = BigInt('0x' + tx.data.slice(74));
    requireThat(amount > 0n && amount < 2n ** 256n - 1n, 'Zero or unlimited approval is unsupported');
  }
}
export function validateWallet(accounts, chain, expected) {
  requireThat(Array.isArray(accounts) && address(accounts[0]) && accounts[0].toLowerCase() === expected.toLowerCase(), '钱包地址不匹配，请切换到配置的账户');
  requireThat((typeof chain === 'string' || typeof chain === 'number') && BigInt(chain) === 56n, '网络不匹配，请在钱包切换到 BSC（56）');
}
export function feeBoundedTransaction(request, estimate, price, limits) {
  const gas = (BigInt(estimate) * 120n + 99n) / 100n;
  const gasPrice = BigInt(price);
  requireThat(gas > 0n && gas <= BigInt(limits.maxGas), 'Gas limit exceeded');
  requireThat(gasPrice > 0n && gasPrice <= BigInt(limits.maxGasPriceWei) && gas * gasPrice <= BigInt(limits.maxFeeWei), 'Gas price or total fee limit exceeded');
  return { ...request.transaction, value: '0x0', chainId: '0x38', gas: '0x' + gas.toString(16), gasPrice: '0x' + gasPrice.toString(16) };
}
