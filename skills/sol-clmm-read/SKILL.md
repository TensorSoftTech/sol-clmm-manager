---
name: sol-clmm-read
description: >-
  Read Solana CLMM/DLMM positions via sol-clmm-client (list, detail, enrich,
  snapshots). Use when listing or inspecting Orca/Raydium/Meteora positions,
  claim-sim fees, volume from SQLite snapshot chains, or VenueFilter.
---

# sol-clmm reads

## Preferred API (`sol-clmm-client`)

```rust
use sol_clmm_client::Client;
use sol_clmm_manager::rpc::{connect, Rpc, RpcMeter};
use sol_clmm_manager::{PriceApiKeys, PriceMode, VenueFilter};

let meter = RpcMeter::default();
let sol = connect(rpc_url);
let rpc = Rpc::new(&sol, &meter);
let client = Client::new(PriceMode::OnChain, PriceApiKeys::default());
let rows = client.list_positions(&rpc, &wallet, Some(&store), VenueFilter::All, false)?;
let detail = client.position_detail(&rpc, &rows[0], Some(&store), false, Some(&wallet))?;
```

- `list_positions` — soft-fail per venue; merges Orca + Raydium + Meteora; claim-sim fees; enrich (USD/TVL + snapshots). Propagates enrich errors.
- `list_positions_lenient` — same but enrich errors are ignored (TUI uses this).
- `position_detail` — dispatches by `row.venue`; claim-sim fees/rewards; chart series; period volume from snapshot chain.
- `enrich_list` — if you already have rows and only need prices/snapshots.
- `list_all_venues` — merge without enrich (escape hatch).

## Fees

List and detail use **claim-sim** (`owed + Δfee_growth_inside`), not stale on-chain `fee_owed` alone. Needs tick arrays (CLMM) or bin arrays (DLMM).

## Volume

SQLite `pool_snapshots` (full history per pool; clean later if DB grows). Volume = Σ over consecutive segments `Δfee_growth × mean(L) / fee_rate`. Need ≥2 snaps (`r` refresh more than once). ≈24h = window × `86400/dt`. Rough — not an indexer.

## Venue filter

`VenueFilter::All` or `Only(Venue::…)`. TUI cycles with `v`.

## Direct protocol calls (escape hatch)

`orca_whirlpool::list_positions` / `fetch_position_detail` (same for raydium/meteora) — then `sol_clmm_manager::enrich_rows` yourself. Prefer Client so agents do not re-copy multi-venue wiring.
