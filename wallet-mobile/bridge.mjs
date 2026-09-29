import http from 'node:http';
import { randomBytes } from 'node:crypto';
import { openSync, closeSync, writeFileSync, fsyncSync, renameSync, mkdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { requireThat, address, hash, validateRequest, validateWallet, feeBoundedTransaction } from './validation.mjs';

export function persist(path, data, exclusive = false) {
  const target = exclusive ? path : path + '.tmp-' + randomBytes(8).toString('hex');
  const fd = openSync(target, 'wx', 0o600);
  try { writeFileSync(fd, JSON.stringify(data, null, 2) + '\n'); fsyncSync(fd); } finally { closeSync(fd); }
  if (!exclusive) renameSync(target, path);
}
export function rpcReader(endpoint) {
  const url = new URL(endpoint);
  requireThat(url.protocol === 'https:' && !url.username && !url.password && !url.hash, 'RPC must be HTTPS without credentials or fragment');
  return async (method, params, timeout) => {
    requireThat(['eth_chainId', 'eth_estimateGas', 'eth_gasPrice'].includes(method), 'Read-only RPC method required');
    const response = await fetch(url, { method: 'POST', redirect: 'error', signal: AbortSignal.timeout(Math.max(1, Math.min(5000, timeout))), headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }) });
    requireThat(response.ok, 'Wallet preflight RPC failed');
    const data = await response.json();
    requireThat(data.jsonrpc === '2.0' && data.id === 1 && !data.error && typeof data.result === 'string' && /^0x[\da-f]+$/i.test(data.result), 'Invalid wallet preflight RPC result');
    return data.result;
  };
}
async function body(req) {
  requireThat(req.headers['content-type'] === 'application/json', 'JSON required');
  const chunks = []; let size = 0;
  for await (const chunk of req) { size += chunk.length; requireThat(size <= 262144, 'Request too large'); chunks.push(chunk); }
  return JSON.parse(Buffer.concat(chunks).toString());
}
const terminalStates = new Set(['submitted', 'not_submitted', 'unknown']);
export async function startBridge({ account, stateDir, assetsDir, rpc, limits, port = 0 }) {
  requireThat(address(account), 'Expected wallet address required');
  requireThat(Number.isSafeInteger(limits.maxGas) && limits.maxGas > 0, 'Invalid max gas');
  requireThat(/^\d+$/.test(String(limits.maxGasPriceWei)) && BigInt(limits.maxGasPriceWei) > 0n && /^\d+$/.test(String(limits.maxFeeWei)) && BigInt(limits.maxFeeWei) > 0n, 'Invalid fee limits');
  const requestDir = join(stateDir, 'requests'); mkdirSync(requestDir, { recursive: true, mode: 0o700 });
  const browserToken = randomBytes(32).toString('hex'), signerToken = randomBytes(32).toString('hex');
  let origin, session = null, active = null, inFlight = false;
  const jobs = new Map();
  function save(job) { persist(join(requestDir, job.id + '.json'), job); }
  function finish(job, state, message) {
    job.state = state; job.message = message; save(job);
    // Unknown submissions lock this bridge until the operator checks the chain
    // and restarts it. A different confirmation ID must not silently retry them.
    if (active === job && state !== 'unknown') active = null;
  }
  function expire(job) {
    if (!terminalStates.has(job.state) && Date.now() >= job.deadline_at) finish(job, job.state === 'claimed' ? 'unknown' : 'not_submitted', job.state === 'claimed' ? 'Wallet outcome unknown. Check wallet/chain; do not retry.' : 'Request expired before wallet dispatch');
  }
  function sessionReady() { return session && Date.now() - session.at < 5000; }
  const server = http.createServer(async (req, res) => {
    const reply = (status, data) => { if (!res.destroyed) { res.writeHead(status, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(data)); } };
    res.setHeader('Cache-Control', 'no-store');
    res.setHeader('Referrer-Policy', 'no-referrer');
    res.setHeader('X-Content-Type-Options', 'nosniff');
    res.setHeader('X-Frame-Options', 'DENY');
    // Relay hosts are the fixed list in the pinned Binance SDK's relay.ts.
    res.setHeader('Content-Security-Policy', "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' wss://nbstream.binance.com wss://nbstream.binance.info wss://nbstream.binance.click wss://nbstream.yshyqxx.com; frame-ancestors 'none'; base-uri 'none'; form-action 'none'");
    try {
      requireThat(req.headers.host === new URL(origin).host, 'Invalid Host');
      requireThat(!req.headers.origin || req.headers.origin === origin, 'Cross-origin request denied');
      requireThat(!req.headers['sec-fetch-site'] || ['same-origin', 'none'].includes(req.headers['sec-fetch-site']), 'Cross-site request denied');
      if (req.method === 'GET' && ['/', '/app.js', '/style.css', '/icon.svg'].includes(req.url)) {
        const files = { '/': ['index.html', 'text/html; charset=utf-8'], '/app.js': ['app.js', 'text/javascript'], '/style.css': ['style.css', 'text/css'], '/icon.svg': ['icon.svg', 'image/svg+xml'] };
        const [file, type] = files[req.url]; res.writeHead(200, { 'Content-Type': type }); res.end(readFileSync(join(assetsDir, file))); return;
      }
      const isSigner = req.url === '/sign';
      requireThat(req.headers.authorization === `Bearer ${isSigner ? signerToken : browserToken}`, 'Unauthorized');
      if (req.method === 'GET' && req.url === '/state') {
        if (active) expire(active);
        reply(200, { account, chain_id: 56, connected: !!sessionReady(), limits, pending: active, recent: [...jobs.values()].slice(-5) }); return;
      }
      requireThat(req.method === 'POST', 'Unsupported request');
      const value = await body(req);
      if (req.url === '/session') {
        if (value.disconnected) { session = null; if (active && !terminalStates.has(active.state)) finish(active, active.state === 'claimed' ? 'unknown' : 'not_submitted', 'Wallet disconnected or changed'); }
        else { validateWallet(value.accounts, value.chain, account); session = { at: Date.now() }; }
        reply(200, { ok: true }); return;
      }
      if (req.url === '/sign') {
        const request = value.request;
        validateRequest(request, account);
        const received = Date.now();
        requireThat(Number.isSafeInteger(value.deadline_at) && value.deadline_at > received && value.deadline_at <= received + request.expires_in_ms, 'Invalid absolute deadline');
        requireThat(sessionReady(), 'Connect the expected phone wallet first');
        requireThat(!inFlight && !active, 'Another request is active or has unknown outcome; inspect before restarting');
        inFlight = true;
        const job = { id: request.confirmation_id.toLowerCase(), request, deadline_at: value.deadline_at, state: 'preparing', created_at: new Date().toISOString() };
        try {
          // Exclusive durable reservation: never replay the same confirmation ID.
          persist(join(requestDir, job.id + '.json'), job, true);
          jobs.set(job.id, job); active = job;
          const read = (method, params) => { requireThat(Date.now() < job.deadline_at, 'Request expired'); return rpc(method, params, job.deadline_at - Date.now()); };
          requireThat(BigInt(await read('eth_chainId', [])) === 56n, 'RPC chain mismatch');
          const tx = { ...request.transaction, value: '0x0' };
          const estimate = await read('eth_estimateGas', [tx]);
          const price = await read('eth_gasPrice', []);
          job.transaction = feeBoundedTransaction(request, estimate, price, limits);
          requireThat(sessionReady() && job.state === 'preparing' && Date.now() < job.deadline_at, 'Wallet changed or request expired');
          job.state = 'pending'; save(job);
          while (!terminalStates.has(job.state)) { expire(job); if (!terminalStates.has(job.state)) await new Promise(resolve => setTimeout(resolve, 50)); }
          if (job.state === 'submitted') reply(200, { confirmation_id: request.confirmation_id, tx_hash: job.tx_hash });
          else reply(409, { error: job.message, state: job.state });
        } catch (error) {
          if (jobs.get(job.id) === job && !terminalStates.has(job.state)) finish(job, 'not_submitted', 'Preparation failed: ' + error.message);
          throw error;
        } finally { inFlight = false; }
        return;
      }
      const job = jobs.get(value.id); requireThat(job, 'Unknown request'); expire(job);
      if (req.url === '/claim') {
        requireThat(sessionReady() && job === active && job.state === 'pending' && Date.now() < job.deadline_at, 'Request unavailable or already claimed');
        job.state = 'claimed'; save(job); reply(200, job); return;
      }
      if (req.url === '/decline') {
        requireThat(job.state === 'pending', 'Cannot cancel an already dispatched wallet request');
        finish(job, 'not_submitted', 'Operator declined before wallet dispatch'); reply(200, { ok: true }); return;
      }
      if (req.url === '/result') {
        requireThat(job.state === 'claimed' || job.state === 'unknown', 'Request has not been dispatched or is already resolved');
        requireThat(!job.result_recorded, 'Result already recorded');
        job.result_recorded = true;
        if (hash(value.tx_hash)) {
          job.tx_hash = value.tx_hash;
          if (job.state === 'unknown' || Date.now() >= job.deadline_at) { job.late_result = true; finish(job, 'unknown', 'Late transaction hash saved. Verify this hash manually; do not replay.'); }
          else finish(job, 'submitted', 'Wallet returned a hash; CLI must verify payload and receipt');
        } else finish(job, 'unknown', 'Wallet rejected, disconnected or returned no valid hash. Inspect before retrying.');
        reply(200, { ok: true }); return;
      }
      reply(404, { error: 'Not found' });
    } catch (error) { reply(400, { error: error.code === 'EEXIST' ? 'Confirmation ID already used; never replay' : error.message }); }
  });
  server.requestTimeout = 10000; server.headersTimeout = 5000;
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(port, '127.0.0.1', resolve); });
  origin = `http://127.0.0.1:${server.address().port}`;
  return { origin, browserToken, signerToken, close: async () => { for (const job of jobs.values()) if (!terminalStates.has(job.state)) finish(job, job.state === 'claimed' ? 'unknown' : 'not_submitted', 'Bridge stopped'); server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); } };
}
