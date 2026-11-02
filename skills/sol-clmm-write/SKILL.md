---
name: sol-clmm-write
description: >-
  Opt-in CLMM/DLMM writes (claim, deposit, withdraw, compound). Transaction v1
  only, always simulate, SOL/WSOL wrap, DLMM strategies. Use when building or
  calling write APIs behind the `write` cargo feature.
---

# sol-clmm write

## Feature gate

```bash
cargo run -p sol-clmm-tui --features write -- --keypair ~/.config/solana/id.json
```

Default build is **read-only** — no sign/send symbols. Protocol + client modules are `#[cfg(feature = "write")]`.

## Security

- SQLite may store **keypair file path** + wallet pubkey. **Never** key bytes / seed phrases.
- Load keypair from that path **only at sign time**.
- Runtime consent: Allow write? / No / Don’t ask again (settings can revoke).

## Transactions

- **SIMD-0385 v1 only** (`VersionedMessage::V1`) — not legacy, not v0.
- Resource limits via `v1::TransactionConfig` (CU, tip lamports, loaded data, heap) — not ComputeBudget ixs.
- **Always simulate** (base64) before send; refuse send on sim error.
- **Budget:** settings `auto` (probe sim + `getRecentPrioritizationFees` + optional Jupiter tip) or `manual` floors.

## Ops

| Op | Notes |
|----|--------|
| Claim fees + rewards | Per venue |
| Deposit | Dual / single · exact / max / % · optional swap-to-match |
| Withdraw | % / ΔL / max |
| **Close + claim** | Claim fees+rewards + withdraw all + close · **1 tx** |
| **Create position** | User pastes **pool** + range · **1-side or 2-side** · optional **swap-to-match** · prefer **1 tx v1** (wrap→swap→open→deposit) |
| Compound | Prefer 1 tx v1; split only if over size |
| SOL pools | Always **native SOL** UX (wrap before / unwrap after; no WSOL mode prompt) |
| DLMM | Meteora StrategyType: Spot/Curve/BidAsk × OneSide\|Balanced (ImBalanced TODO) |
| **Wallet** (`w`) | Async balance load bar · wrap / unwrap SOL · free-form Jupiter/Titan swap · **create pool** |
| Create pool | Orca `initialize_pool` (+ vault signers) · Raydium `create_pool` · Meteora stub (bail) · modes: pool only / +deposit / +swap (seed deposit after land via `n`) |
| History Δ | Position ops use pool mint ATA Δ; **wallet swap** records from→to mint Δ (SOL/WSOL = native+ATA) |

## Wallet screen

From positions list with writes live: **`w`**. Opens immediately with the left→right load bar while balances fetch. Popular destinations: SOL, USDC, USDT, jupUSD, JUP, PYUSD, USDe (+ wallet holdings). Wrap/unwrap live here — **not** under Manage. Manage is deposit / compound / withdraw / claim / close only.

## Live testing (honest)

Dust / burner only. As of latest notes:

- **Exercised briefly:** create/open on **Raydium + Meteora · SOL/USDC**.
- **Still need:** other pools, Orca open, create-pool live, wallet wrap/swap, compound, close, Token-2022 / non-SOL pairs.

Prefer **no-swap** deposit/create until autoswap (P0) is fixed. Always read sim logs.

## Swap router

Settings: **Jupiter** (`GET api.jup.ag/swap/v2/build` · Router raw ixs) | **Titan** (Portal `transactionFormat=1`). Same API keys as USD mode. Used for create/deposit swap-to-match, compound, and wallet swap. Routes with ALTs are refused (tx v1 only).
