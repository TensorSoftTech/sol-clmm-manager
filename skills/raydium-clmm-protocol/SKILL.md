---
name: raydium-clmm-protocol
description: >-
  Raydium CLMM account byte layouts and instruction accounts/data as implemented
  in this repo. Use when decoding PersonalPosition / PoolState / TickArray /
  AmmConfig, or building increase/decrease_v2, open Token-2022 NFT, close, or
  create_pool IXs for Raydium CLMM.
---

# Raydium CLMM protocol (repo map)

> **Unofficial.** Not affiliated with Raydium. Layouts are independent reimplementations from public on-chain data. Verify against [`crates/raydium-clmm`](../../crates/raydium-clmm) + chain / published IDL before trusting.

## Program

| | |
|--|--|
| **Program id** | `CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK` |
| **Crate** | [`crates/raydium-clmm`](../../crates/raydium-clmm) |
| **Decode** | [`src/decode.rs`](../../crates/raydium-clmm/src/decode.rs) |
| **Write** | [`src/write.rs`](../../crates/raydium-clmm/src/write.rs) (`feature = "write"`) |

## Index

| Topic | File |
|-------|------|
| Account byte maps | [`accounts.md`](accounts.md) |
| Instruction discs / args / accounts | [`instructions.md`](instructions.md) |

## PDAs used in-crate

| Seeds | Result |
|-------|--------|
| `["tick_array", pool, start_tick_i32_be]` | TickArray (**BE** start index) |
| `["position", nft_mint]` | PersonalPosition |
| `["position", pool, tick_lower_be, tick_upper_be]` | ProtocolPosition |
| `["pool_tick_array_bitmap_extension", pool]` | Bitmap extension |
| AmmConfig / pool / vault / observation | see create_pool in instructions |

`TICK_ARRAY_SIZE = 60`. Start index same floor rule as Orca, but PDA uses **big-endian** tick.

## Gaps

- PoolState discriminator **not** checked; observation / bitmap / full padding unread
- ProtocolPosition & TickArrayBitmapExtension used in writes, **never decoded**
- No separate collect_fees (claim = `decrease_liquidity_v2` with L=0)
- Legacy OpenPosition / OpenPositionV2 unused; create uses Token-2022 NFT path
- TickState bytes after rewards unread
- Swap / admin IXs absent

## How to use

1. Offsets → [`accounts.md`](accounts.md).
2. IX surface → [`instructions.md`](instructions.md).
3. Prefer crate constants; BE vs LE tick PDA seeds matter.
