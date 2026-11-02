---
name: sol-clmm
description: >-
  Unofficial Solana concentrated-liquidity workspace (Orca Whirlpool, Raydium CLMM,
  Meteora DLMM). Use when working in whirpool-terminal / sol-clmm-manager, wiring
  TUI or agent reads, or explaining crate layout, risk, and where chart paint vs
  series math live.
---

# sol-clmm workspace

## Risk (always)

Unofficial · WIP · not financial advice · funds at risk. Layouts/math are independent reimplementations — not vendor SDKs. Cross-check balances/fees before acting. No affiliation with Orca / Raydium / Meteora.

## Crate map

| Crate | Role |
|-------|------|
| `sol-clmm-manager` | Shared domain: RPC+meter, SQLite, tokens, fees math, enrich, snapshots, pricing |
| `sol-clmm-client` | **Agent/TUI API** — multi-venue `list` / `detail` / enrich (depends on manager + protocols) |
| `orca-whirlpool` / `raydium-clmm` / `meteora-dlmm` | Venue decode + list/detail |
| `sol-clmm-tui` | Ratatui UI only (incl. chart **paint**); calls `sol-clmm-client` |

Dependency direction: protocols → manager; client → manager + protocols; TUI → client.

## Charts

- **Series data** (`LiquiditySample`, `build_liquidity_series`) — manager (pure data).
- **Drawing** (ratatui bars, current-price marker) — TUI `ui.rs` only. Do not move paint into manager.

## When to use what

- Interactive human → `sol-clmm-tui` (`?` / `h` for in-app help).
- Programmatic / agent reads → `sol_clmm_client::Client` (preferred) or call one protocol crate directly.
- Writes (`--features write`) → `skills/sol-clmm-write` (wallet `w`, manage `m`, create `n`, history Δ).
- Quotes HTTP (Jupiter/Titan) still live under manager `quotes` for now; future extract → `sol-clmm-quotes`.

## Writes smoke (do not overclaim)

Open position briefly tested on **Raydium + Meteora SOL/USDC** only. Other pools / create-pool / wallet flows still need dust testing. Prefer no-swap until autoswap P0 is fixed.

## Deferred (do not churn now)

- `sol-clmm-core` / `sol-clmm-quotes` split
- `sol-clmm-agent` binary

See also: `skills/sol-clmm-read`, `skills/sol-clmm-pricing`, `skills/sol-clmm-write`.
