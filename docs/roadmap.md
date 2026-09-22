# Tokenized Stocks roadmap

## Milestone 0 — reproducible read-only foundation

- Standalone Rust CLI backed by `postman-flow`.
- Binance Web3 header authentication without credentials in flow documents.
- RWA platform discovery, ticker search, and reference-price comparison.
- Offline YAML compilation in CI.

## Milestone 1 — safe transaction preparation

- Query BSC wallet balances and positions.
- Request aggregated spot quotes and ERC-20 approval transactions.
- Build and simulate a tokenized-stock swap.
- Enforce maximum value, price impact, slippage, chain ID, and contract allowlists.
- Emit a human-readable execution plan before any signature request.

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

