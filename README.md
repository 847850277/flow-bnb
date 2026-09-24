# flow-bnb

English | [简体中文](README.zh-CN.md)

Auditable, declarative transaction workflows for BNB Chain, powered by
[`postman-flow`](https://github.com/847850277/postman-gpui/tree/main/crates/postman-flow).

The project is being incubated for the BNB Hack Tokenized Stocks track as SDK and trading-Agent
infrastructure. It gives an Agent typed MCP tools for selecting validated BSC workflows, applying
deterministic trade policy, and producing an auditable execution plan before any signing handoff.

## Why a separate project?

`postman-flow` remains a chain-independent workflow engine. `flow-bnb` owns Binance Web3 request
authentication, BSC-specific policies, reusable RWA and trading flows, and the product-facing CLI.
Reusable engine improvements can move upstream without coupling the core runtime to one chain.

## Current capabilities

- Pins `postman-flow` to a public Git commit for reproducible builds.
- Signs Binance Web3 API requests with the required HMAC-SHA256/Base64 header scheme.
- Refuses to attach credentials to any host other than `https://web3.binance.com`.
- Reads credentials from environment variables; flow documents never contain API secrets.
- Statically checks `.http.yml` files without credentials or network access.
- Includes RWA discovery, wallet snapshot, and quote/build/simulate workflows.
- Applies BSC-only notional, slippage, price-impact, token allowlist, simulation, and confirmation
  gates through a reusable Rust policy type.
- Exposes six MCP tools for Agent-driven generation, validation, policy evaluation, and execution
  planning, plus read-only receipt tracking.
- Stops trade preparation after Binance's Transaction API simulation; the MCP never signs or
  broadcasts.
- Tracks already-broadcast transactions through a separate JSON-RPC adapter, with bounded Flow
  loops, chain/receipt validation, confirmation counts, and redacted JSON reports.

## Quick start

The crate keeps an MSRV of Rust 1.90. The local toolchain file selects Rust 1.97, while CI also
verifies the project with Rust 1.90.

```bash
cargo run --locked -- check flows/rwa_discovery.http.yml
cargo run --locked -- check flows/wallet_snapshot.http.yml
cargo run --locked -- check flows/safe_swap_preparation.http.yml
cargo run --locked -- check flows/transaction_receipt.http.yml
```

Create API credentials in the
[Binance Web3 Developer Portal](https://web3.binance.com/en/dev-portal), then expose them only to
the current process:

```bash
export BINANCE_WEB3_API_KEY='your-api-key'
export BINANCE_WEB3_SECRET_KEY='your-secret-key'

cargo run --locked -- run flows/rwa_discovery.http.yml \
  --input ticker=NVDA \
  --input platform_id=ondo
```

Add `-v` or `--verbose` to inspect the HTTP flow:

```bash
cargo run --locked -- run flows/rwa_discovery.http.yml \
  --input ticker=NVDA --input platform_id=ondo -v
```

Verbose mode writes HTTP method/URL, request/response headers and bodies, status,
and timing to stderr. The Binance API key and signature headers are displayed as
`[REDACTED]`; the secret key is never logged. Flow's sensitive inputs and known
sensitive outputs use its existing redaction. Response bodies can contain business
or wallet data, so review logs before sharing. `watch-transaction --verbose` also
works and keeps its JSON report on stdout; its RPC URL stays redacted. Without the
flag, the existing output is unchanged.

Use `--env ticker=FLOW_BNB_TICKER` when a flow input should come from an environment variable. This
keeps sensitive or operational values out of shell history.

The wallet snapshot flow requires only a wallet address in addition to the API credentials:

```bash
cargo run --locked -- run flows/wallet_snapshot.http.yml \
  --input wallet_address=0xYourBscAddress
```

The safe swap flow uses the official aggregated quote and swap builders, then submits the unsigned
EVM payload to the official simulation endpoint. `amount` is expressed in the sell token's smallest
unit. It does not sign or broadcast:

```bash
cargo run --locked -- run flows/safe_swap_preparation.http.yml \
  --input from_token_address=0xSellToken \
  --input to_token_address=0xTokenizedStock \
  --input amount=1000000 \
  --input wallet_address=0xYourBscAddress \
  --input slippage_percent=0.5
```

## Track an existing transaction

No Binance Web3 credentials or wallet private key are needed. Supply a transaction hash
already broadcast by your wallet and an HTTP(S) RPC endpoint:

```bash
export FLOW_BNB_RPC_URL='https://bsc-dataseed.bnbchain.org'
cargo run --locked -- watch-transaction \
  --tx-hash 0xYour64HexDigitTransactionHash \
  --chain-id 56 \
  --confirmations 3 \
  --timeout-ms 120000 \
  --report receipt-report.json
```

The command prints one JSON report. Only `confirmed` exits successfully; reverted, timeout,
iteration exhaustion, and RPC errors exit nonzero. `--report` saves the same result for both
successful and unsuccessful tracking and refuses to overwrite an existing file. Prefer the
RPC environment variable when a provider key is part of the URL; the URL is omitted from reports.

See [receipt tracking](docs/receipt-tracking.md) for loop limits, MCP arguments, adapter semantics,
confirmation/reorg behavior, and verification evidence.

## Agent MCP server

`flow-bnb-mcp` is a domain server built on the official Rust MCP SDK. Start it over stdio with a
workspace root. Generated files may only be written below that root, and symlink escapes are
rejected.

```bash
cargo run --locked --bin flow-bnb-mcp -- --root "$PWD"
```

The tools are:

- `list_bnb_capabilities`: templates, official API paths, and safety boundaries.
- `generate_bnb_flow`: returns canonical compiled YAML and can save it below the MCP root.
- `validate_bnb_flow`: parses and compiles Agent-generated YAML without network access.
- `evaluate_trade_policy`: returns stable machine-readable violations for a trade intent.
- `watch_transaction`: runs the bounded read-only receipt flow and returns a structured tracking report.
- `build_execution_plan`: describes quote, policy, simulation, confirmation, signer, and broadcast
  stages while keeping signing and broadcasting outside the MCP process.

The default policy permits preparation up to 100 USD, 50 bps slippage, and 100 bps price impact on
BSC. Execution mode additionally requires a successful simulation and explicit operator
confirmation. Production callers should pass a token allowlist.

## Safety boundary

Discovery and wallet flows are read-only. The swap preparation flow builds unsigned calldata and
simulates it, but cannot sign or broadcast. A future execution adapter must re-evaluate policy,
require explicit operator confirmation, and hand calldata to an isolated signer or Binance Agentic
Wallet. Never commit `.env` files, private keys, API keys, or secret keys.

## Roadmap

See [docs/roadmap.md](docs/roadmap.md) for the hackathon milestones and
[docs/developer-experience-report.md](docs/developer-experience-report.md) for the evidence log used
by the submission.

## Verification

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
for file in flows/*.http.yml; do cargo run --locked -- check "$file"; done
```

## License

MIT

## Evidence-bound trade preparation

`prepare-trade` (CLI) and `prepare_trade` (MCP) bind wallet/quote/build/simulation
responses to local execution policy and persist redacted evidence. Exact-amount
approvals and an interactive external-wallet handoff are covered by deterministic
tests. **RFQ stock orders are detected and blocked pending their vendor adapter;
this is not yet an end-to-end live stock trading demo.**

See [trade execution and current limitations](docs/trade-execution.md) for commands,
operator policy, signer protocol, tests and remaining submission work.

## Offline wallet and MCP handoff demo

Run `python3 scripts/demo_handoff.py` to queue a simulated trade through the real
MCP server, review it in the terminal, and exercise the bundled local-development
wallet adapter plus settlement checks. No private keys or funds are required.
See [wallet handoff](docs/wallet-handoff.md) for configuration and evidence limits.
