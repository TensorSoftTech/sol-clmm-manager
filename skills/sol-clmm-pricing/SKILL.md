---
name: sol-clmm-pricing
description: >-
  USD pricing for sol-clmm positions (on-chain Orca/Raydium/Meteora hops,
  Jupiter Price API, Titan Portal). Use when setting PriceMode, API keys,
  throttle, or debugging missing ≈USD / TVL on list or detail.
---

# sol-clmm pricing

## Modes (`PriceMode`)

| Mode | Needs | Notes |
|------|-------|-------|
| `OnChain` | Solana RPC | PDA hops: Orca Whirlpool + Raydium CLMM, then Meteora DLMM vs stables / SOL — no HTTPS |
| `Jupiter` | HTTPS | Optional `JUPITER_API_KEY`; free tier throttled |
| `Titan` | HTTPS + key | `TITAN_API_KEY` required (sandbox or paid) |

Persisted in SQLite settings; TUI Settings cycles with Enter on “USD estimate”.

## On-chain hop ladder

For each mint (after stable parity / current-pool short-circuit):

1. Direct vs each stable in `stables::stablecoin_mints()` via `price_in_pool_quote`
2. Else vs WSOL × recursive SOL/USD (`SolHop`)
3. Else `Missing`

`price_in_pool_quote` probes:

1. **Orca** — Whirlpool PDAs for `PRICE_TICK_SPACINGS`
2. **Raydium** — pool PDAs for `RAYDIUM_AMM_CONFIG_INDICES` × sorted mints
3. **Meteora** (only if both miss) — LbPair v1/v2 PDAs for `METEORA_BIN_STEPS` × `METEORA_BASE_FACTORS`

Among hits, prefer highest active liquidity (Raydium/Orca `L`; Meteora prefers tighter `bin_step`). DLMM prices use active bin (`IndexSpace::Bin`), same as CurrentPool.

## Keys (`PriceApiKeys`)

```rust
PriceApiKeys {
  jupiter: Option<String>,
  titan: Option<String>,
  paid_fast: bool, // shorter throttle when keyed
}
```

Env: `JUPITER_API_KEY`, `TITAN_API_KEY`. Also settable in TUI Settings.

## Where prices are applied

- List enrich → `position_usd`, `pool_tvl_usd`, `fees_usd` on `PositionRow`.
- Detail → `UsdEstimate` + reward mint prices when claim-sim rewards exist.

## Agent tips

- Default agents to `OnChain` unless keys are configured.
- Meter: RPC / Jupiter / Titan call counts live on `RpcMeter` (TUI footer).
- Future: Jupiter/Titan HTTP may move to `sol-clmm-quotes`; Client API stays stable.
