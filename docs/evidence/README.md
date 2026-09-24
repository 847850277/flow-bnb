# Evidence inventory

Evidence is local recorded output, not a guarantee that current prices/routes are
unchanged. None of the preparation probes submitted an approval, signed an order,
or broadcast a trade. Public placeholder wallet `0x1111…1111` is not ours.

| File | What it demonstrates |
| --- | --- |
| `rwa-discovery-live.json` | Prior authenticated read-only RWA discovery |
| `receipt-mainnet.json` | Prior observation of an existing public transaction, not our trade |
| `nvda-preparation-probe.json` | Wallet API success; missing buy-token entry initially blocked diagnosis |
| `nvda-preparation-quote-probe.json` | 5 USDT quote returned business code 40375 (minimum USD amount); HTTP 200 alone is not success |
| `nvda-preparation-six-usdt.json` | 6 USDT quote and build succeeded; LiquidMesh returned SWAP for NVDAon |
| `nvda-preparation-simulation-probe.json` | Wallet, quote, build, exact approval and approval simulation succeeded; execution blocked |
| `nvda-preparation-rpc-fallback.json` | Same preparation with missing NVDAon balance confirmed as zero by BSC balanceOf; only local contract allowlists still block handoff |

Important observations from 2026-09-23:

- A separate 1-USDT probe returned code 40375 and the message “Minimum order
  amount is 5 USD.” Exactly 5 USDT also failed; the later quote's USD price was
  below 1, consistent with the distinction between token units and USD notional.
- 6 USDT produced a quote with approximately 5.999 USD notional and 18 sell-token
  decimals. Exact base-unit arithmetic is required.
- The Trading API documentation describes equity/RWA execution as RFQ, but this
  NVDAon live route returned `executionMode=SWAP`, vendor `LiquidMesh`. Both modes
  must be dispatched from actual responses. RFQ remains blocked in this adapter.
- The wallet API omitted the unheld buy token. Its absence is not evidence of
  zero balance; the implementation now offers an explicit on-chain fallback.
- The successful simulation in the simulation probe is **ERC-20 approval**, not
  swap simulation or a successful stock purchase. Router/spender allowlists remain
  empty in the probe policy, so no execution capability was produced.

These are machine observations to help the participant reproduce findings. They
are not the required human-authored Developer Experience Report.

## Offline MCP handoff evidence

`mcp-handoff-simulation.json` comes from `scripts/demo_handoff.py --self-test`.
It exercises real MCP/CLI/wallet-adapter processes with fixture API/RPC responses.
It is explicitly SIMULATION_ONLY, contains no mainnet transaction, and is not an LLM
session or EVM execution proof. The fixed transaction hash belongs to the fixture.
