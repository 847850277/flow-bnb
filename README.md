# flow-bnb

Auditable, declarative transaction workflows for BNB Chain, powered by
[`postman-flow`](https://github.com/847850277/postman-gpui/tree/main/crates/postman-flow).

The project is being incubated for the BNB Hack Tokenized Stocks track. Its first milestone is a
read-only RWA discovery flow that searches a tokenized stock on BSC and compares its on-chain price
with the underlying reference price. Transaction simulation, policy checks, wallet signing, and
small-value mainnet execution follow in later milestones.

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
- Includes a three-step RWA discovery and price-comparison flow.

## Quick start

The crate keeps an MSRV of Rust 1.90. The local toolchain file selects Rust 1.97, while CI also
verifies the project with Rust 1.90.

```bash
cargo run --locked -- check flows/rwa_discovery.http.yml
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

## Safety boundary

The initial flow is read-only. Mainnet write flows will require an explicit execution input, a
successful transaction simulation, maximum value and slippage checks, and a separate signer. Never
commit `.env` files, private keys, API keys, or secret keys.

## Roadmap

See [docs/roadmap.md](docs/roadmap.md) for the hackathon milestones and
[docs/developer-experience-report.md](docs/developer-experience-report.md) for the evidence log used
by the submission.

## Verification

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo run --locked -- check flows/rwa_discovery.http.yml
```

## License

MIT
