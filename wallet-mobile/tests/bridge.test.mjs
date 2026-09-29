import test from 'node:test';
import http from 'node:http';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawn } from 'node:child_process';
import { writeSignerExecutable } from '../signer.mjs';
import { startBridge } from '../bridge.mjs';
import { validateRequest, feeBoundedTransaction } from '../validation.mjs';
import { dispatch } from '../controller.mjs';
const account = '0x' + '1'.repeat(40), token = '0x' + '2'.repeat(40), spender = '3'.repeat(40);
const txHash = '0x' + '4'.repeat(64);
const limits = { maxGas: 600000, maxGasPriceWei: '1000000000', maxFeeWei: '100000000000000' };
function request(id = 'a'.repeat(64), ms = 3000) { return { protocol: 'flow-bnb-signer-v1', confirmation_id: id, chain_id: 56, kind: 'approval', expires_in_ms: ms, transaction: { from: account, to: token, value: '0', data: '0x095ea7b3' + spender.padStart(64, '0') + (10n**18n).toString(16).padStart(64, '0') } }; }
async function fixture(t, options = {}) {
  const stateDir = mkdtempSync(join(tmpdir(), 'flow-mobile-test-')); const calls = [];
  const bridge = await startBridge({ account, stateDir, assetsDir: resolve(import.meta.dirname, '../dist'), limits, rpc: async (method) => { calls.push(method); return { eth_chainId: '0x38', eth_estimateGas: '0xb5f0', eth_gasPrice: '0x2faf080' }[method]; }, ...options });
  t.after(async () => { await bridge.close(); rmSync(stateDir, { recursive: true, force: true }); });
  const api = async (path, value, headers = {}) => {
    const response = await fetch(bridge.origin + path, { method: value === undefined ? 'GET' : 'POST', headers: { Authorization: `Bearer ${path === '/sign' ? bridge.signerToken : bridge.browserToken}`, ...(value === undefined ? {} : { 'Content-Type': 'application/json' }), ...headers }, body: value === undefined ? undefined : JSON.stringify(value) });
    return { status: response.status, body: await response.json() };
  };
  const connect = () => api('/session', { accounts: [account], chain: '0x38' });
  const sign = r => api('/sign', { request: r, deadline_at: Date.now() + r.expires_in_ms - 10 });
  const pending = async () => { for (let i = 0; i < 100; i++) { const j = (await api('/state')).body.pending; if (j?.state === 'pending') return j; await new Promise(r => setTimeout(r, 10)); } throw new Error('No pending request'); };
  return { bridge, stateDir, api, connect, sign, pending, calls };
}
test('binds request, fees, one claim and exact signer response; replay denied', async t => {
  const f = await fixture(t); await f.connect(); const result = f.sign(request()); const job = await f.pending();
  assert.equal(job.transaction.gas, '0xda54'); assert.equal(job.transaction.value, '0x0');
  assert.equal((await f.api('/claim', { id: job.id })).status, 200);
  assert.equal((await f.api('/claim', { id: job.id })).status, 400);
  await f.api('/result', { id: job.id, tx_hash: txHash });
  assert.deepEqual((await result).body, { confirmation_id: job.id, tx_hash: txHash });
  assert.equal((await f.sign(request())).status, 400);
  assert.deepEqual(f.calls, ['eth_chainId', 'eth_estimateGas', 'eth_gasPrice']);
  assert.equal(JSON.parse(readFileSync(join(f.stateDir, 'requests', job.id + '.json'))).state, 'submitted');
});
test('requires separate capabilities, exact Host and same Origin', async t => {
  const f = await fixture(t);
  assert.equal((await f.api('/state', undefined, { Authorization: 'Bearer invalid' })).status, 400);
  assert.equal((await f.api('/state', undefined, { Origin: 'https://evil.example' })).status, 400);
  const badHost = await new Promise(resolve => { const req = http.get(f.bridge.origin + '/state', { headers: { Host: 'evil.example', Authorization: `Bearer ${f.bridge.browserToken}` } }, res => { res.resume(); resolve(res.statusCode); }); req.on('error', () => resolve(0)); });
  assert.equal(badHost, 400);
  assert.equal((await f.api('/state', undefined, { 'Sec-Fetch-Site': 'cross-site' })).status, 400);
  assert.equal((await f.api('/sign', {}, { Authorization: `Bearer ${f.bridge.browserToken}` })).status, 400);
  const denied = await fetch(f.bridge.origin + '/state?token=' + f.bridge.browserToken); assert.equal(denied.status, 400);
  const page = await fetch(f.bridge.origin); assert.match(page.headers.get('content-security-policy'), /frame-ancestors 'none'/);
  assert.doesNotMatch(await page.text(), new RegExp(f.bridge.signerToken));
});
test('wrong account, wrong chain and no connected wallet fail closed', async t => {
  const f = await fixture(t);
  assert.equal((await f.sign(request())).status, 400);
  assert.equal((await f.api('/session', { accounts: [token], chain: '0x38' })).status, 400);
  assert.equal((await f.api('/session', { accounts: [account], chain: '0x1' })).status, 400);
  await f.connect(); const r = request(); r.transaction.from = token;
  assert.equal((await f.sign(r)).status, 400); assert.equal(f.calls.length, 0);
});
test('native transfers, extra fields, unlimited approval and expiry rejected', () => {
  for (const change of [r => r.transaction.value = '1', r => r.transaction.nonce = '0x1', r => r.chain_id = 1, r => r.expires_in_ms = 0, r => r.expires_in_ms = 30001, r => r.transaction.data = '0x095ea7b3' + spender.padStart(64, '0') + 'f'.repeat(64)]) {
    const r = request(); change(r); assert.throws(() => validateRequest(r, account));
  }
  assert.throws(() => feeBoundedTransaction(request(), '0xb5f0', '0xffffffffff', limits));
});
test('RPC chain and fee limits are checked before browser dispatch', async t => {
  for (const mode of ['chain', 'fee']) {
    const f = await fixture(t, { rpc: async method => method === 'eth_chainId' ? mode === 'chain' ? '0x1' : '0x38' : '0xffffffffff' });
    await f.connect(); assert.equal((await f.sign(request())).status, 400);
    const state = (await f.api('/state')).body; assert.equal(state.pending, null); assert.equal(state.recent[0].state, 'not_submitted');
  }
});
test('operator decline never reaches wallet dispatch', async t => {
  const f = await fixture(t); await f.connect(); const result = f.sign(request()); const job = await f.pending();
  await f.api('/decline', { id: job.id }); assert.equal((await result).body.state, 'not_submitted');
  assert.equal((await f.api('/claim', { id: job.id })).status, 400);
});
test('deadline before claim blocks dispatch', async t => {
  const f = await fixture(t); await f.connect(); const r = request('a'.repeat(64), 100);
  assert.equal((await f.sign(r)).body.state, 'not_submitted');
  assert.equal((await f.api('/claim', { id: r.confirmation_id })).status, 400);
});
test('timeout after claim stays unknown; late hash persisted and new requests blocked', async t => {
  const f = await fixture(t); await f.connect(); const result = f.sign(request('a'.repeat(64), 250)); const job = await f.pending();
  await f.api('/claim', { id: job.id }); assert.equal((await result).body.state, 'unknown');
  assert.equal((await f.sign(request('b'.repeat(64)))).status, 400);
  assert.equal((await f.api('/result', { id: job.id, tx_hash: txHash })).status, 200);
  const saved = JSON.parse(readFileSync(join(f.stateDir, 'requests', job.id + '.json')));
  assert.equal(saved.tx_hash, txHash); assert.equal(saved.late_result, true); assert.equal(saved.state, 'unknown');
});
test('wallet error never becomes success or permits retry', async t => {
  const f = await fixture(t); await f.connect(); const result = f.sign(request()); const job = await f.pending();
  await f.api('/claim', { id: job.id }); await f.api('/result', { id: job.id });
  assert.equal((await result).body.state, 'unknown'); assert.equal((await f.api('/result', { id: job.id, tx_hash: txHash })).status, 400);
});
test('durable reservation prevents replay in another bridge process', async t => {
  const f = await fixture(t); await f.connect(); const result = f.sign(request()); const job = await f.pending();
  await f.api('/decline', { id: job.id }); await result;
  const other = await startBridge({ account, stateDir: f.stateDir, assetsDir: '', limits, rpc: async () => { throw new Error('Must not call RPC'); } });
  t.after(() => other.close());
  const post = (path, value, token) => fetch(other.origin + path, { method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${token}` }, body: JSON.stringify(value) });
  await post('/session', { accounts: [account], chain: '0x38' }, other.browserToken);
  const response = await post('/sign', { request: request(), deadline_at: Date.now() + 2500 }, other.signerToken);
  assert.equal(response.status, 400); assert.match((await response.json()).error, /already used/);
});
test('real signer process works with cleared environment and protocol-only stdout', async t => {
  const f = await fixture(t); await f.connect(); const configPath = join(f.stateDir, 'config.json');
  writeFileSync(configPath, JSON.stringify({ origin: f.bridge.origin, token: f.bridge.signerToken, account }));
  const executable = join(f.stateDir, 'flow-bnb-wallet-mobile');
  writeSignerExecutable(executable, configPath);
  const child = spawn(executable, [], { env: {}, stdio: ['pipe', 'pipe', 'pipe'] });
  let output = '', errors = ''; child.stdout.on('data', b => output += b); child.stderr.on('data', b => errors += b);
  const exit = new Promise(resolve => child.on('exit', resolve)); child.stdin.end(JSON.stringify(request()));
  const job = await f.pending(); await f.api('/claim', { id: job.id }); await f.api('/result', { id: job.id, tx_hash: txHash });
  assert.equal(await exit, 0, errors); assert.deepEqual(JSON.parse(output), { confirmation_id: job.id, tx_hash: txHash });
});
function browserFixture({ reject = false, chainAfterClaim = '0x38' } = {}) {
  let claimed = false, sends = 0; const reports = [];
  const r = request(); const pending = { id: r.confirmation_id, request: r, deadline_at: Date.now() + 3000, transaction: feeBoundedTransaction(r, '0xb5f0', '0x2faf080', limits) };
  const provider = { request: async ({ method, params }) => {
    if (method === 'eth_accounts') return [account];
    if (method === 'eth_chainId') return claimed ? chainAfterClaim : '0x38';
    assert.equal(method, 'eth_sendTransaction'); sends++; assert.deepEqual(params[0], pending.transaction);
    if (reject) throw new Error('User rejected'); return txHash;
  } };
  const api = async (path, value) => { if (path === '/claim') { assert.equal(claimed, false); claimed = true; return pending; } reports.push(value); };
  return { provider, pending, api, reports, sends: () => sends };
}
test('browser sends exact claimed transaction once, records hash', async () => {
  const f = browserFixture(); assert.equal(await dispatch(f.provider, f.pending, f.api, account), txHash); assert.equal(f.sends(), 1); assert.equal(f.reports[0].tx_hash, txHash);
});
test('browser handles rejection without retries', async () => {
  const f = browserFixture({ reject: true }); await assert.rejects(dispatch(f.provider, f.pending, f.api, account), /rejected/); assert.equal(f.sends(), 1); assert.equal(f.reports[0].tx_hash, undefined);
});
test('browser rechecks chain after claiming and checks deadline before sending', async () => {
  const f = browserFixture({ chainAfterClaim: '0x1' }); await assert.rejects(dispatch(f.provider, f.pending, f.api, account)); assert.equal(f.sends(), 0);
  const expired = browserFixture(); await assert.rejects(dispatch(expired.provider, expired.pending, expired.api, account, () => Date.now() + 10000)); assert.equal(expired.sends(), 0);
});
