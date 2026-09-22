# Tokenized Stocks roadmap

## Milestone 0 — reproducible read-only foundation

- Standalone Rust CLI backed by `postman-flow`.
- Binance Web3 header authentication without credentials in flow documents.
- RWA platform discovery, ticker search, and reference-price comparison.
- Offline YAML compilation in CI.

## Milestone 1 — safe transaction preparation

- [x] Query BSC wallet balances and recent transactions.
- [x] Request aggregated spot quotes.
- [x] Build unsigned swap calldata and simulate it before signing.
- [x] Enforce maximum value, price impact, slippage, chain ID, contract allowlists, simulation,
  and operator confirmation in a deterministic policy module.
- [x] Expose templates, policy evaluation, and auditable execution plans through MCP.
- [ ] Add the optional ERC-20 approval branch when the selected quote requires it.
- [ ] Persist redacted run reports with request IDs and latency measurements.

## Milestone 2 — controlled mainnet execution

- Integrate an isolated wallet signer or Binance Agentic Wallet.
- Require explicit operator confirmation for irreversible actions.
- Broadcast a small-value BSC mainnet transaction.
- Verify the receipt and resulting wallet position.
- Produce a redacted audit report that can be replayed in dry-run mode.

## Milestone 3 — product and ecosystem proof

- Add a visual workflow and run timeline.
- Publish reusable bStocks, Ondo, and xStocks templates.
- Add rebalancing and market-hours spread-monitor examples.
- Validate the flows with at least two BNB ecosystem builders.
