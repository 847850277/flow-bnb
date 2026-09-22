# Binance Web3 developer experience report

Record evidence while building. Include exact documentation links, timestamps, request IDs with
sensitive values removed, and the smallest reproducible example for every issue.

## Onboarding timeline

- Developer Portal account created:
- API credentials created:
- First authenticated API call:
- First RWA token discovered:
- First transaction simulation:
- First small-value mainnet transaction:

## Documentation findings

| Page | Expected behavior | Observed behavior | Reproduction | Suggested change |
|---|---|---|---|---|
| [Trading API](https://web3.binance.com/en/dev-docs/catalog/web3-wallet/api/rest-api/trading-api) | Quote and swap-builder schemas cover BSC RWA routes | `/quote` returns per-vendor `quoteId`; `/swap` consumes it and returns unsigned EVM calldata | `flows/safe_swap_preparation.http.yml` | Publish a complete RWA approval + quote + swap example |
| [Transaction API](https://web3.binance.com/en/dev-docs/catalog/web3-wallet/api/rest-api/transaction-api) | Simulation predicts execution before broadcast | `/pre-transaction/simulate` accepts `binanceChainId=56` plus one `evmTx` object | `flows/safe_swap_preparation.http.yml` | Clarify which response changes should block an Agent automatically |
| [Wallet API](https://web3.binance.com/en/dev-docs/catalog/web3-wallet/api/rest-api/wallet-api) | Wallet state can seed an Agent plan | Balance API can exclude risk tokens; history supports BSC and pagination | `flows/wallet_snapshot.http.yml` | Add a single BSC portfolio snapshot endpoint |

## API behavior

| Endpoint | Latency | Rate-limit headers | Error handling | Edge cases |
|---|---:|---|---|---|
| `/api/v1/dex/aggregator/quote` | Pending credentialed run | Pending | HTTP 200 still requires `code == 0` | `quoteId` expires after roughly 30 seconds |
| `/api/v1/dex/aggregator/swap` | Pending credentialed run | Pending | HTTP status and business code are checked | Request parameters must match the cached quote |
| `/api/v1/dex/pre-transaction/simulate` | Pending credentialed run | Pending | Flow requires `code == 0` and `data.status == SUCCESS` | Exactly one chain-specific transaction object is allowed |
| `/api/v1/dex/balance/all-token-balances-by-address` | Pending credentialed run | Pending | HTTP status and business code are checked | Current API accepts one chain per request |

## Tokenized-stock findings

- Liquidity and slippage:
- Behavior outside traditional market hours:
- On-chain price versus reference price:
- Differences between bStocks, Ondo, and xStocks:
- Transaction simulation accuracy:

## AI and wallet stack

- Agentic Wallet or Wallet Skills used: execution handoff is modeled but not connected yet.
- What worked: MCP can select compiled templates, evaluate deterministic policy, and return an
  ordered execution plan without seeing credentials or private keys.
- What failed: no credentialed mainnet simulation evidence has been collected yet.
- Missing capabilities: isolated signer adapter, policy-bound Agentic Wallet handoff, and receipt
  verification.

## Requested capabilities

- API endpoints:
- SDK support:
- Authentication tooling:
- Error messages:
