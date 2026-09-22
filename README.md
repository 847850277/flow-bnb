# flow-bnb

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
- Exposes five MCP tools for Agent-driven generation, validation, policy evaluation, and execution
  planning.
- Stops trade workflows after Binance's Transaction API simulation; the MCP never signs or
  broadcasts.

## Quick start

The crate keeps an MSRV of Rust 1.90. The local toolchain file selects Rust 1.97, while CI also
verifies the project with Rust 1.90.

```bash
cargo run --locked -- check flows/rwa_discovery.http.yml
cargo run --locked -- check flows/wallet_snapshot.http.yml
cargo run --locked -- check flows/safe_swap_preparation.http.yml
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
