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
- [x] Track an existing transaction: validate chain/receipt, poll with Flow, check canonical block and confirmations, return CLI/MCP reports.
- [ ] Verify the resulting wallet position and integrate tracking with the future signer/broadcast adapter.
- Produce a redacted audit report that can be replayed in dry-run mode.

## Milestone 3 — product and ecosystem proof

- Add a visual workflow and run timeline.
- Publish reusable bStocks, Ondo, and xStocks templates.
- Add rebalancing and market-hours spread-monitor examples.
- Validate the flows with at least two BNB ecosystem builders.

## Evidence-bound preparation update

- [x] CLI/MCP preparation uses actual API evidence and operator-local policy.
- [x] Quote-derived notional with exact arithmetic; build/identity/risk validation.
- [x] Exact-amount approval validation and simulation (single approval only).
- [x] Redacted per-stage HTTP/business status, latency and response digests.
- [x] Interactive, expiring confirmation bound to the exact prepared action.
- [x] External signer process protocol; submitted payload verification, receipt and
  balance observations tested with mocks. No bundled production wallet adapter yet.
- [x] Ordinary SWAP template explicitly rejects RFQ routes.
- [ ] RFQ vendor-specific typed-data validation and order simulation/validation.
- [ ] Actual wallet integration, RFQ submission and settlement monitoring.
- [ ] Authorized small mainnet stock trade and recorded demonstration.
- [ ] Publish reviewed repository and human-authored developer experience report.

The checkboxes in earlier milestones describe components, not live verification.
See `docs/trade-execution.md` and `docs/evidence/` for the current evidence boundary.

## Local wallet handoff update

- [x] Loopback development-node JSON-RPC wallet adapter with gas/fee/account/chain gates.
- [x] MCP durable intent queue, status and cancellation; independent terminal confirmation.
- [x] Fresh preparation on operator handoff, persistent claim barrier, no same-ID replay.
- [x] Offline process-level happy path and six failure/cancellation cases.
- [ ] Actual production wallet integration and real model client session.
- [ ] Mainnet acceptance; local mock evidence does not establish signing or real settlement.
