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
| SOL pools | Prompt **WSOL** vs **native SOL** (wrap before / unwrap after) |
| DLMM | 5 strategies: curve, bid-ask, spot-tight, spot-spread, spot-wide |

## Swap router

Settings: **Jupiter** (`GET api.jup.ag/swap/v2/build` · Router raw ixs) | **Titan** (Portal `transactionFormat=1`). Same API keys as USD mode. Used for create/deposit swap-to-match and compound. Routes with ALTs are refused (tx v1 only).
