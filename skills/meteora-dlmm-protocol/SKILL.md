---
name: meteora-dlmm-protocol
description: >-
  Meteora DLMM account byte layouts and instruction accounts/data as implemented
  in this repo. Use when decoding PositionV2 / LbPair / BinArray, building claim /
  deposit / withdraw / close / create-position IXs, or asking what a discriminator
  or offset means for Meteora DLMM.
---

# Meteora DLMM protocol (repo map)

> **Unofficial.** Not affiliated with Meteora. Layouts are independent reimplementations from public on-chain data. Verify against [`crates/meteora-dlmm`](../../crates/meteora-dlmm) + chain / published IDL before trusting. Programs upgrade — this skill can lag.

## Program

| | |
|--|--|
| **Program id** | `LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo` |
| **Crate** | [`crates/meteora-dlmm`](../../crates/meteora-dlmm) |
| **Decode** | [`src/decode.rs`](../../crates/meteora-dlmm/src/decode.rs) |
| **Write** | [`src/write.rs`](../../crates/meteora-dlmm/src/write.rs) (`feature = "write"`) |

## Index

| Topic | File |
|-------|------|
| Account byte maps (PositionV2, LbPair, BinArray) | [`accounts.md`](accounts.md) |
| Instruction discriminators, args, account lists | [`instructions.md`](instructions.md) |

## PDAs used in-crate

| Seed | Result |
|------|--------|
| `["bin_array", lb_pair, index_i64_le]` | BinArray |
| `["__event_authority"]` | Event authority (`D1ZN9Wj1fRSUQfCjhvnu1hqDMT7hzjzBBpi12nVniYD6`) |

**Bitmap extension:** when absent, writes pass **program id** as a sentinel (not a real account).

## Gaps (not in this crate yet)

- Create lb-pair / customizable pool account graph (`plan_create_pool` bails)
- PositionV1, full StaticParameters / variable params / oracle on LbPair
- `claim_fee2` / Token-2022 variants
- Swap IXs, admin/reward-fund IXs, bin-array init IXs
- Mid-Bin fields (price, etc.) mostly unread — only amounts + fee_per_token used

## How to use

1. Need offset meaning → [`accounts.md`](accounts.md).
2. Need ix disc / metas / args → [`instructions.md`](instructions.md).
3. Prefer crate constants over copying hex from chat — crates are source of truth.
