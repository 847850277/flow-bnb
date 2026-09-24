# MCP intent handoff and local development wallet

English | [简体中文](wallet-handoff.zh-CN.md)

This increment can be developed and tested without a personal wallet, private key,
Binance API key or real funds. It adds an actual JSON-RPC development-wallet adapter
and an operator handoff, with a fully offline process-level demo.

**Evidence boundary:** the demo exercises real MCP stdio, the real CLI/TTY prompt,
the real wallet adapter process and the real Flow receipt checker against mock
API/RPC responses. It does not cryptographically sign a transaction, execute an EVM,
use an LLM, or demonstrate a mainnet trade. A real Anvil/Hardhat node supplies signing
when configured; that has not been live-validated by this demo.

## Run it

From the repository root:

```sh
python3 scripts/demo_handoff.py
```

The script builds binaries, starts a private loopback mock RPC server, starts the
actual MCP service, queues an intent via MCP, and launches the operator CLI. Inspect
the printed simulated trade, then enter the complete confirmation ID or enter
anything else to decline. Temporary fixtures and journals are removed at exit.
No production endpoint or private key is accepted by this demo script.

For an automated test confined to these fixtures:

```sh
python3 scripts/demo_handoff.py --self-test
python3 scripts/demo_handoff.py --self-test --no-build --scenario wallet-reject
python3 scripts/demo_handoff.py --self-test --no-build --scenario timeout
```

Scenarios: `success`, `decline`, `wallet-reject`, `timeout`, `expired`, `wrong-chain`,
`cancel`. `--output /new/file.json` saves clearly labelled simulation evidence.
`--self-test` owns a PTY and supplies its confirmation to the mock-only run; it is
not a production unattended-approval option.

## MCP handoff

The operator configures a private directory, separate from generated YAML:

```sh
mkdir -m 700 /absolute/path/to/trade-inbox
export FLOW_BNB_HANDOFF_DIR=/absolute/path/to/trade-inbox
cargo run --locked --bin flow-bnb-mcp -- --root /path/to/project
```

Three tools are added:

| Tool | Capability |
| --- | --- |
| `request_trade_execution` | Persist a TradeRequest for operator review; return intent ID |
| `get_trade_execution` | Read durable status plus redacted transaction hash, receipt success, balance deltas and simulation/development evidence label |
| `cancel_trade_execution` | Cancel an intent that no operator has claimed |

There is **no MCP approval or signing tool**. Tool arguments cannot select a policy,
executable, wallet endpoint or an `operator_confirmed` flag. `prepare_trade` remains
read-only; its report is not an execution capability. The queue accepts requests,
not executable transaction payloads or serialized PreparedTrade objects.

The human operator reviews an intent:

```sh
flow-bnb review-trade --handoff-dir /absolute/path/to/trade-inbox --intent-id ID
```

Then, for the bundled local development wallet:

```sh
flow-bnb approve-trade \
  --handoff-dir /absolute/path/to/trade-inbox --intent-id ID \
  --policy /absolute/path/to/policy.json \
  --wallet-config /absolute/path/to/dev-wallet.json
```

This reloads the operator's policy, obtains **fresh** balance/quote/build/simulation
evidence and asks for the new confirmation ID on `/dev/tty`. The queued intent ID
is not an approval token. A stale preparation cannot be reused. `--demo` explicitly
replaces API data with deterministic fixtures and marks the audit `simulation`.

Directory mode is 0700, intent/journal files are 0600. `create_new` on the journal
is a permanent claim barrier across processes. Cancellation and operator execution
race for the same barrier; only one wins. A cancelled or claimed ID cannot be
replayed, including after restart. Journals are append-only and fsynced before
wallet handoff. The audit records the transaction hash and settlement observations.
Incomplete journals are treated as unknown outcomes, not reset to pending.

A timeout/nonzero signer exit is conservatively `needs_attention_outcome_may_be_unknown`:
the caller may not know whether broadcast occurred. Even a known rejection inside
the fixture is reported conservatively across the generic process boundary. There
is no automatic retry. Query the wallet/chain before creating any new intent.
The replay barrier covers an intent ID, not semantic deduplication across new IDs.
Old records may be archived by the operator; there is no agent deletion tool.

This separates MCP capabilities from operator approval. It is not an OS sandbox:
an agent with unrestricted shell access under the operator's user account can access
that user's files and terminal. Deploy approval/wallet authority separately when
that is outside your intended trust model.

## Bundled development-node wallet

`flow-bnb-wallet-rpc --config /absolute/path/dev-wallet.json` consumes the existing
`flow-bnb-signer-v1` JSON protocol on stdin and returns confirmation ID/transaction
hash on stdout. No key is loaded by Flow. The node owns the unlocked development
account and nonce assignment.

Configuration:

```json
{
  "development_only": true,
  "rpc_url": "http://127.0.0.1:8545",
  "account": "0xYOUR_DEVELOPMENT_ACCOUNT",
  "max_gas": 200000,
  "max_gas_price_wei": 2000000000,
  "max_fee_wei": "400000000000000"
}
```

The node must use chain ID 56 to match the current BSC-oriented trade pipeline.
The account must be returned by `eth_accounts`. The adapter only accepts explicit
numeric IPv4 loopback HTTP endpoints and identified Anvil/Hardhat/mock clients.
These are development guardrails, not remote node attestation; configure a local
node you control, not a proxy to a funded production wallet.

It checks the exact sender/recipient/calldata, zero native value, chain and deadline,
then obtains `eth_estimateGas` and `eth_gasPrice`. It adds 20% gas headroom, enforces
both per-unit and total fee caps, and calls `eth_sendTransaction` once. It does not
retry after errors or timeouts. Redirects are disabled; Binance headers never reach
the wallet RPC. The parent process clears environment variables before starting it.

See the [Ethereum JSON-RPC specification](https://ethereum.org/developers/docs/apis/json-rpc/#eth_sendtransaction)
for the node signing/broadcast operation. The bundled adapter is not Agentic Wallet,
MetaMask or a production hardware-wallet integration. Those remain separate work.

## Remaining work

- Actual LLM/client configuration and natural-language session validation (the demo
  client is a deterministic MCP test client).
- A selected production wallet adapter with its own confirmation UX.
- Real authorization/swap simulation and small mainnet trade acceptance tests.
- RFQ typed-data/vendor adapters, deeper recovery and event-based asset accounting.

Run `scripts/check.sh` for Rust checks, YAML compilation and all seven offline
process-level scenarios. CI runs the same scenarios on Rust 1.90/Linux.
