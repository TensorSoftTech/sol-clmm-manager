---
name: orca-whirlpool-protocol
description: >-
  Orca Whirlpools account byte layouts and instruction accounts/data as
  implemented in this repo. Use when decoding Position / Whirlpool / TickArray /
  DynamicTickArray, or building collect / open / increase / decrease / close /
  initialize_pool IXs for Orca Whirlpool.
---

# Orca Whirlpool protocol (repo map)

> **Unofficial.** Not affiliated with Orca. Layouts are independent reimplementations from public on-chain data. Verify against [`crates/orca-whirlpool`](../../crates/orca-whirlpool) + chain / published IDL before trusting.

## Program

| | |
|--|--|
| **Program id** | `whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc` |
| **Config** | `WHIRLPOOLS_CONFIG` in manager (mainnet) |
| **Crate** | [`crates/orca-whirlpool`](../../crates/orca-whirlpool) |
| **Decode** | [`src/decode.rs`](../../crates/orca-whirlpool/src/decode.rs) |
| **Write** | [`src/write.rs`](../../crates/orca-whirlpool/src/write.rs) (`feature = "write"`) |

## Index

| Topic | File |
|-------|------|
| Account byte maps | [`accounts.md`](accounts.md) |
| Instruction discs / args / accounts | [`instructions.md`](instructions.md) |

## PDAs used in-crate

| Seeds | Result |
|-------|--------|
| `["tick_array", whirlpool, start_tick_i32_le]` | TickArray / DynamicTickArray |
| `["position", position_mint]` | Position account |
| `["whirlpool", config, mint_a, mint_b, tick_spacing_u16_le]` | Pool |
| `["fee_tier", config, tick_spacing_u16_le]` | FeeTier |

`TICK_ARRAY_SIZE = 88`. Start index: `tick / (88 * spacing)` floored toward −∞.

## Gaps

- FeeTier / WhirlpoolsConfig accounts not decoded (only PDAs for create)
- Whirlpool early fields (config, bump, …) unread — absolute slices from tick_spacing onward
- No swap / Token-2022 position / adaptive-fee / bundle IXs
- TickArray trailing whirlpool pubkey after ticks not decoded
- DynamicTickArray header fields between start and ticks unnamed (seek only)

## How to use

1. Offsets → [`accounts.md`](accounts.md).
2. IX surface → [`instructions.md`](instructions.md).
3. Crate constants beat chat hex.
