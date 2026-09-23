# Transaction receipt tracking

`watch-transaction` (CLI) and `watch_transaction` (MCP) share the same Rust execution
path and embedded `flows/transaction_receipt.http.yml`. They observe an existing
transaction; they cannot sign or broadcast. The Flow dependencies are pinned to
public commit `ff26f772a5761a018cfc39b3976b1e49ce850e49`, including bounded loops.

## Execution

1. Validate the endpoint, transaction hash, and polling limits before network I/O.
2. Call `eth_chainId` and reject an unexpected chain.
3. Use the YAML `repeat_until` to query `eth_getTransactionReceipt`.
4. A null receipt is pending. For a mined receipt, validate its transaction hash,
   block hash/number, status, and gas usage; read `eth_blockNumber` and
   `eth_getBlockByNumber` to check its canonical block and confirmation count.
5. Keep polling until the configured confirmation count, a canonical reverted
   receipt, a deadline, or an iteration limit. RPC/HTTP errors fail immediately.

A receipt in block N has one confirmation when the observed head is N. A missing
or changed canonical block, disappearing receipt, or head older than the receipt
resets the observation to pending. A reverted receipt fails as soon as it is
observed on the canonical chain, even before the requested success confirmation
count. The confirmation check describes one node's current view; it is not a
consensus-finality guarantee and does not continue monitoring after completion.

The implementation follows the [Ethereum JSON-RPC receipt API](https://ethereum.org/developers/docs/apis/json-rpc/#eth_gettransactionreceipt).
BSC endpoint choices are listed in the [official BNB Chain documentation](https://docs.bnbchain.org/bnb-smart-chain/developers/json_rpc/json-rpc-endpoint/).

## Flow and adapter responsibilities

Flow owns polling, delays, deadlines, iteration limits, and termination conditions.
The local receipt adapter owns EVM hex decoding, RPC envelope checks, canonical block
checks, and confirmation arithmetic. It annotates the receipt response with a local
`tracking` object consumed by the YAML; **this field is not returned by the RPC node**.
The original receipt `result` is retained; chain ID quantities are canonicalized to
lowercase hex. No EVM-specific expression or step kind was added to postman-flow.

The YAML compiles with `flow-bnb check` and can be generated/validated through MCP.
Execution requires `watch-transaction` / `watch_transaction` and their domain adapter;
using raw `postman-g` or the signed Binance `flow-bnb run` command is not supported.
The tracking command executes the bundled template. It does not load arbitrary
agent-provided YAML; options set its inputs and loop bounds before compilation.

## Options and reports

| Option / MCP field | Default | Meaning |
| --- | --- | --- |
| `rpc_url` | required | HTTP(S) endpoint; CLI also reads `FLOW_BNB_RPC_URL` |
| `tx_hash` | required | `0x` followed by 64 hex digits |
| `chain_id` | 56 | Expected chain ID, decimal |
| `confirmations` | 3 | 1–10,000; inclusion counts as one |
| `timeout_ms` | 120,000 | Receipt loop deadline, up to 86,400,000 ms |
| `interval_ms` | 1,000 | Delay after each unsuccessful iteration, 1–60,000 ms |
| `max_iterations` | 120 | 1–10,000 iterations |
| `request_timeout_ms` | 10,000 | Shared budget per flow read step, 1–60,000 ms |

Loop deadline and iteration bound race; the first reached ends polling. The initial
chain check is outside the loop and is bounded by the request timeout. An iteration
may issue three RPC requests that share a single budget; the adapter enforces this
budget even if an underlying transport stalls. Execution time is additional to the
interval. The engine shortens the budget to the loop's remaining time. An unshortened
request-budget timeout is an RPC error; `timeout` means the loop's deadline ended polling.

The report includes `schema_version`, `success`, `outcome`, expected chain and
transaction hash, confirmation target, iteration count, elapsed milliseconds, last
observation, and an optional local diagnostic. Outcomes are `confirmed`, `reverted`,
`timeout`, `max_iterations`, `rpc_error`, and `cancelled`. A last observation can be
`pending` or `confirming` when tracking times out. Timeout does not prove a transaction
was dropped; null receipts also cannot distinguish unknown from pending transactions.

RPC URLs and raw provider error text are omitted from reports. The separate RPC
client never reads Binance credentials, rebuilds only read-method requests, and
refuses redirects. Caller-supplied endpoints can be local nodes or provider URLs;
choose the endpoint you intend to trust. Reports contain public transaction metadata.

## MCP example

```json
{
  "name": "watch_transaction",
  "arguments": {
    "rpc_url": "https://bsc-dataseed.bnbchain.org",
    "tx_hash": "0xYour64HexDigitTransactionHash",
    "chain_id": 56,
    "confirmations": 3,
    "timeout_ms": 120000
  }
}
```

The tool returns a structured report, including unsuccessful tracking results.
Callers must inspect `success` and `outcome`. Invalid arguments are tool errors.
Generate the template with `generate_bnb_flow` and `template: "transaction_receipt"`.

## Verification

```bash
cargo +1.90.0 test --locked --all-targets
cargo +1.90.0 clippy --locked --all-targets -- -D warnings
```

Deterministic tests cover confirmation progression, reverted receipts, wrong chain,
reorg/disappearing receipts, deadline/iteration exhaustion, cancellation, malformed
and error envelopes, HTTP failures, invalid inputs, and endpoint/error redaction.
Loopback HTTP tests launch the real CLI and MCP stdio binaries, verifying report
persistence, exit codes, tool discovery/generation/execution, and absence of Binance
credentials in RPC requests. These run in the existing Rust 1.90 CI job without
provider keys or public-chain dependencies.
