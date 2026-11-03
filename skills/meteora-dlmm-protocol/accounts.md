# Meteora DLMM — account layouts

Offsets are **absolute from start of account data**. First 8 bytes are usually Anchor discriminator.  
**Read?** = what [`decode.rs`](../../crates/meteora-dlmm/src/decode.rs) uses today.

Endian: little-endian unless noted.

---

## PositionV2

| | |
|--|--|
| **Disc** | `[117, 176, 212, 199, 245, 180, 133, 182]` (`POSITION_V2_DISC`) |
| **Len** | `8120` (`POSITION_V2_LEN`) |
| **Const** | `OWNER_OFFSET = 40`, `LOWER_BIN_OFFSET = 7912` |
| **Layout check** | `8 + 32 + 32 + 70×16 + 70×48 + 70×48 == 7912` |

| Offset | Size | Type | Field | Meaning | Read? |
|--------|------|------|-------|---------|-------|
| 0 | 8 | `[u8;8]` | discriminator | PositionV2 | yes |
| 8 | 32 | Pubkey | `lb_pair` | Pool | yes |
| 40 | 32 | Pubkey | `owner` | Position owner | skipped (seek) |
| 72 | 70×16 | `[u128;70]` | `liquidity_shares` | Per-bin Q64.64 shares (index 0 = `lower_bin`) | yes |
| 1192 | 70×48 | `UserRewardInfo[70]` | `reward_infos` | Farming checkpoints | yes |
| 4552 | 70×48 | `FeeInfo[70]` | `fee_infos` | Fee checkpoints + pendings | yes |
| 7912 | 4 | `i32` | `lower_bin_id` | Inclusive | yes |
| 7916 | 4 | `i32` | `upper_bin_id` | Inclusive | yes |
| 7920… | … | … | trailing | Not decoded in-crate | no |

### UserRewardInfo (48 B, per bin)

| Rel | Size | Type | Field | Read? |
|-----|------|------|-------|-------|
| 0 | 16 | `u128` | `reward_per_token_completes[0]` | stored |
| 16 | 16 | `u128` | `reward_per_token_completes[1]` | stored |
| 32 | 8 | `u64` | `reward_pendings[0]` | yes (summed) |
| 40 | 8 | `u64` | `reward_pendings[1]` | yes (summed) |

### FeeInfo (48 B, per bin)

| Rel | Size | Type | Field | Read? |
|-----|------|------|-------|-------|
| 0 | 16 | `u128` | `fee_x_per_token_complete` | yes (claim-sim) |
| 16 | 16 | `u128` | `fee_y_per_token_complete` | yes |
| 32 | 8 | `u64` | `fee_x_pending` | yes |
| 40 | 8 | `u64` | `fee_y_pending` | yes |

**Claim-sim note:** growth uses `share >> 64` (integer part of Q64.64) × `Δfee_per_token` via `mul_shift_right_64` — not full share (that overstates by ~2^64).

**Amounts:** `share * bin.amount / bin.liquidity_supply` integer `mul_div_u128`.

---

## LbPair (header — partial)

| | |
|--|--|
| **Disc** | **Not checked** in-crate (decode assumes LbPair data) |
| **Decode** | Sparse walk + absolute reward block |

| Offset | Size | Type | Field | Read? |
|--------|------|------|-------|-------|
| 0 | 8 | disc | (unchecked) | no |
| 8 | 2 | `u16` | `base_factor` | yes |
| 8+26 | 1 | `u8` | `base_fee_power_factor` | yes |
| … | 32+32 | | StaticParameters / padding region | skipped |
| after skip | 1+2+1 | | bump, bin_step_seed, pair_type | skipped |
| cursor | 4 | `i32` | `active_id` | yes |
| +4 | 2 | `u16` | `bin_step` | yes |
| +2 | 1+1+2+1+1 | | status … creator control | skipped |
| | 32 | Pubkey | `mint_x` | yes |
| | 32 | Pubkey | `mint_y` | yes |
| | 32 | Pubkey | `reserve_x` | yes |
| | 32 | Pubkey | `reserve_y` | yes |
| … | 16+32 | | protocol_fee + padding1 | skipped |
| **264** | 2×144 | `RewardInfo[2]` | farming rewards | partial |

### LbPair RewardInfo (144 B @ 264)

| Rel | Size | Type | Field | Read? |
|-----|------|------|-------|-------|
| 0 | 32 | Pubkey | mint | yes |
| 32 | 32 | Pubkey | vault | yes |
| 64 | 32+8+8 | funder + duration + end | skipped |
| 112 | 16 | `u128` | `reward_rate` | yes |
| 128 | 8 | `u64` | `last_update_time` | yes |
| 136… | … | | rest of 144 | no |

**Fee rate estimate (UI):** `base_factor * bin_step * 10 * 10^base_fee_power_factor` (capped).

---

## BinArray

| | |
|--|--|
| **Disc** | `[92, 142, 92, 220, 5, 148, 70, 181]` |
| **Bins** | 70 (`MAX_BIN_PER_ARRAY`) × 144 B (`BIN_LEN`) |
| **Header** | disc(8) + index(`i64`) + version(`u8`) + pad(7) + lb_pair(32) |

| Offset | Size | Type | Field | Read? |
|--------|------|------|-------|-------|
| 0 | 8 | disc | BinArray | yes |
| 8 | 8 | `i64` | `index` | yes |
| 16 | 1 | `u8` | version | skipped |
| 17 | 7 | pad | | skipped |
| 24 | 32 | Pubkey | lb_pair | skipped |
| 56 | 70×144 | `Bin[70]` | bins | partial |

**Bin id:** `index * 70 + i` (i = 0..69).

### Bin (144 B) — fields this crate reads

| Rel | Size | Type | Field | Read? |
|-----|------|------|-------|-------|
| 0 | 8 | `u64` | `amount_x` | yes |
| 8 | 8 | `u64` | `amount_y` | yes |
| 16 | 16 | | price (unread) | no |
| 32 | 16 | `u128` | `liquidity_supply` | yes |
| 48…80 | | | fulfilled / limit fees region | no |
| 80 | 16 | `u128` | `fee_amount_x_per_token_stored` | yes |
| 96 | 16 | `u128` | `fee_amount_y_per_token_stored` | yes |
| 112…144 | | | rest (incl. reward_per_token) | no |
