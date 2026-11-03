# Raydium CLMM — account layouts

Offsets absolute. LE unless noted. **Read?** = [`decode.rs`](../../crates/raydium-clmm/src/decode.rs).

---

## PersonalPosition

| | |
|--|--|
| **Disc** | `[70, 111, 150, 126, 230, 15, 25, 117]` |
| **Min** | `8+1+32+32+4+4+16+16+16+8+8+3×24` |

| Offset | Size | Type | Field | Read? |
|--------|------|------|-------|-------|
| 0 | 8 | disc | | yes |
| 8 | 1 | `u8` | bump | skipped |
| 9 | 32 | Pubkey | `nft_mint` | yes |
| 41 | 32 | Pubkey | `pool_id` | yes |
| 73 | 4 | `i32` | `tick_lower` | yes |
| 77 | 4 | `i32` | `tick_upper` | yes |
| 81 | 16 | `u128` | `liquidity` | yes |
| 97 | 16 | `u128` | `fee_growth_inside0_last_x64` | yes |
| 113 | 16 | `u128` | `fee_growth_inside1_last_x64` | yes |
| 129 | 8 | `u64` | `fees_owed0` | yes |
| 137 | 8 | `u64` | `fees_owed1` | yes |
| 145 | 3×24 | rewards | growth_inside_last_x64 + owed | yes |

### PositionReward (24 B)

| Rel | Size | Type | Field |
|-----|------|------|-------|
| 0 | 16 | `u128` | `growth_inside_last_x64` |
| 16 | 8 | `u64` | `reward_amount_owed` |

---

## PoolState (header — partial, disc unchecked)

Decode starts at `8+1` (skips disc + bump). No disc assert.

| Walk | Size | Type | Field | Read? |
|------|------|------|-------|-------|
| +0 after bump | 32 | Pubkey | `amm_config` | yes |
| +32 | 32 | | owner | skipped |
| | 32 | Pubkey | mint0 | yes |
| | 32 | Pubkey | mint1 | yes |
| | 32 | Pubkey | vault0 | yes |
| | 32 | Pubkey | vault1 | yes |
| | 32 | | observation | skipped |
| | 1+1 | | mint decimals | skipped |
| | 2 | `u16` | `tick_spacing` | yes |
| | 16 | `u128` | `liquidity` | yes |
| | 16 | `u128` | `sqrt_price` | yes |
| | 4 | `i32` | `tick_current` | yes |
| | 2+2 | | padding | skipped |
| | 16 | `u128` | `fee_growth_global0_x64` | yes |
| | 16 | `u128` | `fee_growth_global1_x64` | yes |
| | 8 | `u64` | `protocol_fees_token0` | yes |
| | 8 | `u64` | `protocol_fees_token1` | yes |
| **397** abs | 3×169 | rewards | see below | yes if long enough |

### Pool RewardInfo (packed, `REWARD_INFO_LEN = 169` @ `REWARD_INFOS_OFFSET = 397`)

| Rel | Size | Type | Field | Read? |
|-----|------|------|-------|-------|
| 0 | 1 | `u8` | `reward_state` (0 uninit · 1 init · 2 opening · 3 ended) | yes |
| 1 | 8 | `u64` | `open_time` | yes |
| 9 | 8 | `u64` | `end_time` | yes |
| 17 | 8 | `u64` | `last_update_time` | yes |
| 25 | 16 | `u128` | `emissions_per_second_x64` | yes |
| 41 | 8+8 | | total_emitted + claimed | skipped |
| 57 | 32 | Pubkey | `token_mint` | yes |
| 89 | 32 | Pubkey | `token_vault` | yes |
| 121 | 32 | | authority | skipped |
| 153 | 16 | `u128` | `reward_growth_global_x64` | yes |

---

## AmmConfig (partial)

| | |
|--|--|
| **Disc** | `[218, 244, 33, 104, 203, 203, 43, 111]` |

Decode only reads **`trade_fee_rate: u32`** after bump(1) + index(2) + owner(32) + protocol_fee_rate(4) → offset `8+1+2+32+4`.

---

## TickArray

| | |
|--|--|
| **Disc** | `[192, 155, 85, 205, 49, 249, 129, 42]` |
| **Size** | 60 ticks × `TICK_STATE_LEN = 168` |
| **Header** | disc(8) + pool(32) + start_index(`i32`) — start not returned by decode |

| Offset | Size | Field | Read? |
|--------|------|-------|-------|
| 0 | 8 | disc | yes |
| 8 | 32 | pool | skipped |
| 40 | 4 | start_tick_index | skipped |
| 44 | 60×168 | TickState | partial |

### TickState (168 B) — fields read

| Rel | Size | Type | Field |
|-----|------|------|-------|
| 0 | 4 | `i32` | tick index (absolute) |
| 4 | 16 | `i128` | liquidity_net |
| 20 | 16 | `u128` | liquidity_gross |
| 36 | 16 | `u128` | fee_growth_outside_0 |
| 52 | 16 | `u128` | fee_growth_outside_1 |
| 68 | 3×16 | `u128` | reward_growths_outside |
| 116…168 | | | trailing padding unread |

`initialized` inferred as `liquidity_gross > 0`.

---

## Used in writes, not decoded

| Account | Role |
|---------|------|
| ProtocolPosition | PDA per (pool, lower, upper) |
| TickArrayBitmapExtension | remaining account on liquidity / open / create_pool |
