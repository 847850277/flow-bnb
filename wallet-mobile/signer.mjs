// Imported by the generated executable. stdout is reserved for the Rust protocol.
import { readFileSync, writeFileSync } from 'node:fs';
import { validateRequest, requireThat, hash } from './validation.mjs';
export function writeSignerExecutable(path, configPath) {
  requireThat(!/\s/.test(process.execPath), 'Node interpreter path must not contain whitespace');
  // Dynamic import also works in an extensionless executable on Node 22.
  writeFileSync(path, `#!${process.execPath}\nimport(${JSON.stringify(import.meta.url)}).then(({sign}) => sign(${JSON.stringify(configPath)})).catch(() => { console.error('Mobile wallet failed or timed out; inspect the wallet and local journal before retrying.'); process.exitCode = 1; });\n`, { flag: 'wx', mode: 0o700 });
}
export async function sign(configPath) {
  const config = JSON.parse(readFileSync(configPath, 'utf8'));
  const url = new URL(config.origin);
  requireThat(url.protocol === 'http:' && url.hostname === '127.0.0.1' && url.pathname === '/' && !url.username && !url.password && !url.search && !url.hash, 'Invalid local bridge URL');
  let size = 0; const chunks = [];
  for await (const chunk of process.stdin) { size += chunk.length; requireThat(size <= 262144, 'Signer request too large'); chunks.push(chunk); }
  const request = JSON.parse(Buffer.concat(chunks).toString());
  validateRequest(request, config.account);
  const deadline_at = Date.now() + request.expires_in_ms;
  const response = await fetch(new URL('/sign', config.origin), {
    method: 'POST', redirect: 'error', signal: AbortSignal.timeout(request.expires_in_ms),
    headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${config.token}` },
    body: JSON.stringify({ request, deadline_at })
  });
  const text = await response.text(); requireThat(text.length <= 8192, 'Bridge response too large');
  const result = JSON.parse(text);
  requireThat(response.ok, result.error || 'Wallet result unknown; inspect the bridge journal');
  requireThat(result.confirmation_id === request.confirmation_id && hash(result.tx_hash), 'Signer response mismatch');
  process.stdout.write(JSON.stringify({ confirmation_id: result.confirmation_id, tx_hash: result.tx_hash }) + '\n');
}
