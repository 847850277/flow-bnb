# Evidence-bound trade preparation and wallet handoff

The `prepare-trade` CLI and `prepare_trade` MCP tool run the same read-only path.
Each Binance request executes through an embedded Flow YAML stage. Exact token
arithmetic, route validation and wallet handoff live in `flow-bnb`; no BNB-specific
step or adapter was added to the upstream Flow engine.

## Try the read-only stock probe

Configure `BINANCE_WEB3_API_KEY` and `BINANCE_WEB3_SECRET_KEY` in your process
(no secrets in request JSON, YAML, command arguments or reports), then run:

```sh
FLOW_BNB_RPC_URL=https://bsc-dataseed.bnbchain.org \
cargo run --locked -- prepare-trade \
  --request examples/nvda-probe.json \
  --policy examples/policy.json \
  --report nvda-preparation.json
```

The example uses the **public placeholder address `0x1111…1111`, not a wallet
owned or controlled by this project**. It asks for a quote to buy NVDAon with
6 USDT in BSC base units (18 decimals). It never trades. Replace it with your own
public address for your wallet diagnosis. An API query for a public address does
not establish ownership. Prices, minimum order sizes and route availability vary.

The report destination must not exist. Exit 0 means preparation completed without
blockers; exit 1 with a report means the API or a local gate blocked preparation.
A ready preparation is not a signature, order or transaction. Missing token
balances remain unknown, not zero, and block execution. If `FLOW_BNB_RPC_URL` is
configured, omitted balances are read through ERC-20 `balanceOf` after verifying
BSC chain ID; an explicit 32-byte zero result is then a verified zero observation.
`balance_sources` distinguishes `wallet_api` from `rpc_balanceOf`. The wallet stage
latency/digest includes this local enrichment when used. Read-only quote diagnosis
can continue when the wallet API omitted a balance and no fallback is configured.

The example policy deliberately has empty router/spender allowlists. Add only
contracts you have independently decided to trust. Do not copy arbitrary response
addresses into your allowlist just to make a failing run succeed.

## What is enforced

- BSC chain, valid wallet/token addresses, positive uint256 sell amount, distinct tokens.
- Quote sell amount, token identities and chain match the request.
- USD notional comes from actual quote metadata, using exact integer/decimal
  arithmetic for the limit check. The request has no caller-supplied notional or
  simulation status. Price impact uses its absolute magnitude, rounded up to bps.
- Build route identity/amount and risk are checked again. For SWAP, the reported
  minimum received must satisfy slippage. API/router calldata encoding remains a
  trust boundary: this does not decode every DEX's arbitrary swap calldata.
- Transaction sender, native value and router are validated. Router/spender
  allowlist failures block handoff but still permit read-only simulation evidence. This initial execution
  path supports ERC-20-to-ERC-20 swaps with zero native value, not native BNB swaps.
- An approval is limited to exactly the requested amount and quoted spender;
  vendor-specific spender selection is preserved. Unlimited approval and
  multi-transaction/reset-to-zero responses are rejected.
- An approval is separately simulated and confirmed. After its receipt, rerun
  preparation for a fresh swap quote. There is no automatic approval-plus-swap.
- Simulation comes from the API response to that exact unsigned transaction.
  Policy flags cannot disable simulation or operator confirmation.
- Confirmation binds request, policy, quote, simulation response hashes and exact
  unsigned transaction. It expires within at most 30 seconds from preparation
  start. Agent-provided JSON reports cannot be reloaded as execution capabilities.

A configured signer is a **trusted wallet boundary**, not sandboxed arbitrary code.
It must display/reconfirm the exact transaction, enforce its own gas limits,
check its network and account, and must not alter the approved payload. Gas and
nonce management belong to that wallet. A successful simulation is not a guarantee
of future execution or protection against a compromised signer/router/API.

## RFQ is a distinct, currently blocked stock execution path

The [official Trading API documentation](https://web3.binance.com/en/dev-docs/catalog/web3-wallet/api/rest-api/trading-api)
describes RFQ for equity/RWA routes: build an EIP-712 payload, sign it, submit an
order, then poll settlement. However, our 2026-09-23 live NVDAon probe returned a
LiquidMesh `SWAP` route. Always dispatch on the returned `executionMode`; the token
category alone is insufficient. This documentation/API discrepancy is recorded
with the live evidence. `rfq.orderId` is the submit request's `quoteId`;
it is not necessarily the original route quote ID. Vendor-specific approvals
also matter. The ordinary `safe_swap_preparation.http.yml` now rejects RFQ
explicitly instead of trying to read an EVM `tx` from it.

`prepare-trade` recognizes RFQ, records a payload digest and returns
`rfq_requires_adapter`. It does **not** interpret EIP-712 data as transaction
calldata, assert that it simulated an RFQ order, or invoke a signer. If an approval
is present, it can validate/simulate that approval for diagnosis, but RFQ blockers
still prevent execution. A successful approval simulation is not order simulation.

Remaining stock execution work: vendor-specific typed-data semantic validation,
expiry and receiver/amount binding, a documented order simulation/validation path,
an actual wallet integration, idempotent order submission, and bounded settlement
polling connected to receipts. Do not claim the stock mainnet demo is complete.

## Interactive SWAP execution

After providing a reviewed request, local policy and your own wallet adapter:

```sh
cargo run --locked -- execute-trade \
  --request my-trade.json --policy my-policy.json --report my-run.json \
  --signer /absolute/path/to/my-wallet-adapter \
  --rpc-url https://bsc-dataseed.bnbchain.org
```

This prepares anew and prints its report. The operator must enter the complete
`confirmation_id` on `/dev/tty`. There is no `--yes`, agent confirmation boolean,
MCP signing tool, or command to execute a saved preparation report. MCP can now
queue an intent for the independent operator CLI; see [wallet handoff](wallet-handoff.md). Interactive
execution currently requires a Unix terminal; read-only preparation is portable.
An expired confirmation requires a new preparation, not replay of an old quote.

The signer receives one JSON object on stdin, without a shell, with its environment
cleared and no Binance credentials. It owns private keys outside Flow. It returns
JSON on stdout; stderr and arbitrary output are not included in audit reports.
Configure any wallet access in the executable's own trusted configuration.

Request protocol:

```json
{
  "protocol": "flow-bnb-signer-v1",
  "confirmation_id": "sha256-of-reviewed-action",
  "chain_id": 56,
  "kind": "swap",
  "transaction": {"from": "0x...", "to": "0x...", "value": "0", "data": "0x..."},
  "expires_in_ms": 12000
}
```

Response:

```json
{"confirmation_id":"sha256-of-reviewed-action","tx_hash":"0x...64 hex digits..."}
```

The wallet adapter signs **and broadcasts** this one transaction. This is a tested
adapter protocol, not a bundled Agentic Wallet integration. A local Anvil/Hardhat
JSON-RPC adapter is now available; see [wallet handoff](wallet-handoff.md). There are no automatic
retries: a timeout or disconnected wallet can mean a transaction was broadcast.
Inspect the wallet/chain before doing anything again. The report is reserved before
preparation and saved before handoff; transaction hash is persisted before polling.

After handoff, a separate unauthenticated RPC client checks the submitted
transaction's hash, chain, sender, recipient, value and calldata. The existing Flow
tracker then checks its receipt, canonical block and three confirmations. Wallet
API balances are queried again. Balance changes are observations that can include
concurrent activity or indexing delay, not isolated proof of this transaction's
asset effects. A mismatch is recorded and exits with failure; it never resubmits.

## MCP configuration

Set `FLOW_BNB_POLICY_FILE` to an operator-maintained policy JSON path and configure
API credentials in the MCP server environment. `prepare_trade` takes only the
fields in `TradeRequest`; callers cannot override policy or claim simulation
success. `evaluate_trade_policy` and `build_execution_plan` remain advisory; the
latter never grants signer handoff based on a caller-supplied intent.

The generic `run` command remains a general-purpose signed HTTP Flow runner. Its
arbitrary YAML does not acquire the `prepare-trade` policy guarantees. Use the
bounded preparation API for agent trade preparation.

## Verification and delivery

Run `scripts/check.sh` for formatting, clippy, all tests and compilation of all
bundled YAML. Tests use mock transports/local processes and never sign a live
transaction. Test coverage includes amount/identity tampering, exact approval,
RFQ blocking, failed simulation, expired/wrong confirmation, external signer
protocol, exact submitted-transaction verification, receipts and balance changes.

Before submission, finish the RFQ integration and authorized small mainnet demo,
then publish reviewed code, record the actual demonstration, and have the human
participant write the developer experience report. Existing evidence files are
explicitly separated into successful read-only calls and unsuccessful probes;
none of this run's probes are evidence of a completed trade.
