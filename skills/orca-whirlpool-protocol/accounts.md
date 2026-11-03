# Orca Whirlpool — account layouts

Offsets absolute from account start. LE. **Read?** = [`decode.rs`](../../crates/orca-whirlpool/src/decode.rs).

---

## Position

| | |
|--|--|
| **Disc** | `[170, 188, 143, 228, 122, 64, 247, 208]` |
| **Len** | `216` (`POSITION_LEN`) — sequential after disc |

| Offset | Size | Type | Field | Read? |
|--------|------|------|-------|-------|
| 0 | 8 | disc | Position | yes |
| 8 | 32 | Pubkey | `whirlpool` | yes |
| 40 | 32 | Pubkey | `position_mint` | yes |
| 72 | 16 | `u128` | `liquidity` | yes |
| 88 | 4 | `i32` | `tick_lower_index` | yes |
| 92 | 4 | `i32` | `tick_upper_index` | yes |
| 96 | 16 | `u128` | `fee_growth_checkpoint_a` | yes |
| 112 | 8 | `u64` | `fee_owed_a` | yes |
| 120 | 16 | `u128` | `fee_growth_checkpoint_b` | yes |
| 136 | 8 | `u64` | `fee_owed_b` | yes |
| 144 | 3×24 | rewards | see below | yes |

### PositionReward (24 B × 3)

| Rel | Size | Type | Field |
|-----|------|------|-------|
| 0 | 16 | `u128` | `growth_inside_checkpoint` |
| 16 | 8 | `u64` | `amount_owed` |

---

## Whirlpool (sparse absolute map)

| | |
|--|--|
| **Disc** | `[63, 149, 209, 12, 225, 128, 99, 9]` |
| **Min** | `8+253` (`WHIRLPOOL_MIN_LEN`) |
| **Full** | `8+261+384` (`WHIRLPOOL_FULL_LEN`) — through 3×128 B reward infos |

Offsets below are **relative to byte 0**; values used by decode use `8 + N` where N is IDL body offset.

| Absolute | Size | Type | Field | Read? |
|----------|------|------|-------|-------|
| 0 | 8 | disc | Whirlpool | yes |
| 8..41 | | | config / bump / … (unread) | no |
| 8+33 | 2 | `u16` | `tick_spacing` | yes |
| 8+35..37 | | | gap | no |
| 8+37 | 2 | `u16` | `fee_rate` | yes (as `u32`) |
| 8+39..41 | | | gap | no |
| 8+41 | 16 | `u128` | `liquidity` | yes |
| 8+57 | 16 | `u128` | `sqrt_price` (Q64.64) | yes |
| 8+73 | 4 | `i32` | `tick_current_index` | yes |
| 8+77 | 8 | `u64` | `protocol_fee_owed_a` | yes |
| 8+85 | 8 | `u64` | `protocol_fee_owed_b` | yes |
| 8+93 | 32 | Pubkey | `token_mint_a` | yes |
| 8+125 | 32 | Pubkey | `token_vault_a` | yes |
| 8+157 | 16 | `u128` | `fee_growth_global_a` | yes |
| 8+173 | 32 | Pubkey | `token_mint_b` | yes |
| 8+205 | 32 | Pubkey | `token_vault_b` | yes |
| 8+237 | 16 | `u128` | `fee_growth_global_b` | yes |
| 8+253 | 8 | `u64` | `reward_last_updated_timestamp` | yes if len≥ |
| **8+261** | 3×128 | rewards | WhirlpoolRewardInfo | yes if full |

### WhirlpoolRewardInfo (128 B)

| Rel | Size | Type | Field | Read? |
|-----|------|------|-------|-------|
| 0 | 32 | Pubkey | mint | yes |
| 32 | 32 | Pubkey | vault | yes |
| 64 | 32 | | extension | skipped |
| 96 | 16 | `u128` | `emissions_per_second_x64` | yes |
| 112 | 16 | `u128` | `growth_global_x64` | yes |

---

## TickArray (fixed)

| | |
|--|--|
| **Disc** | `[69, 97, 189, 190, 110, 7, 66, 187]` |
| **Len** | `8 + 4 + 88×113 + 32` (`FIXED_TICK_ARRAY_LEN`) |
| **Tick** | `TICK_LEN = 113` |

| Offset | Size | Type | Field | Read? |
|--------|------|------|-------|-------|
| 0 | 8 | disc | | yes |
| 8 | 4 | `i32` | `start_tick_index` | yes |
| 12 | 88×113 | ticks | | yes |
| end-32 | 32 | Pubkey | whirlpool | **not** decoded |

### Fixed tick (113 B)

| Rel | Size | Type | Field |
|-----|------|------|-------|
| 0 | 1 | `bool` | initialized |
| 1 | 16 | `i128` | liquidity_net |
| 17 | 16 | `u128` | liquidity_gross |
| 33 | 16 | `u128` | fee_growth_outside_a |
| 49 | 16 | `u128` | fee_growth_outside_b |
| 65 | 3×16 | `u128` | reward_growths_outside[3] |

Tick index = `start_tick_index + i` (i = 0..87).

---

## DynamicTickArray

| | |
|--|--|
| **Disc** | `[17, 216, 246, 142, 225, 199, 218, 56]` |
| **Tick payload** | `DYNAMIC_TICK_DATA_LEN = 112` when tag=1 |

| Offset | Size | Field | Read? |
|--------|------|-------|-------|
| 0 | 8 | disc | yes |
| 8 | 4 | `start_tick_index` | yes |
| 12 | 32+16 | header (whirlpool + ?) | **seek only** |
| 8+4+32+16 | 88×(1 + 0\|112) | tagged ticks | yes |

### Tag

| Tag | Meaning |
|-----|---------|
| 0 | uninitialized (1 byte only) |
| 1 | initialized + 112 B payload (same fields as fixed tick without the init byte) |
| other | error |

Payload layout when tag=1: liquidity_net(16) · gross(16) · fee_out_a(16) · fee_out_b(16) · reward_out[3](48).
