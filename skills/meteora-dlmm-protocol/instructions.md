# Meteora DLMM — instructions (repo write surface)

All discs are 8-byte Anchor discriminators as in [`write.rs`](../../crates/meteora-dlmm/src/write.rs).  
**W/S** = writable / signer.

Event authority PDA + program id appear on most IXs.

---

## claim_fee

| | |
|--|--|
| **Disc** | `[169, 32, 79, 137, 136, 232, 70, 137]` |
| **Args** | none |
| **Plans** | `plan_claim`, `plan_close_and_claim` |

| # | Account | W | S | Notes |
|---|---------|---|---|-------|
| 0 | lb_pair | ✓ | | |
| 1 | position | ✓ | | |
| 2 | bin_array_lower | ✓ | | covers `tick_lower` |
| 3 | bin_array_upper | ✓ | | covers `tick_upper` |
| 4 | sender | | ✓ | owner |
| 5 | reserve_x | ✓ | | |
| 6 | reserve_y | ✓ | | |
| 7 | user_token_x | ✓ | | ATA |
| 8 | user_token_y | ✓ | | ATA |
| 9 | mint_x | | | |
| 10 | mint_y | | | |
| 11 | token_program | | | |
| 12 | event_authority | | | |
| 13 | program | | | |

---

## claim_reward

| | |
|--|--|
| **Disc** | `[149, 95, 181, 242, 94, 90, 158, 162]` |
| **Args** | `reward_index: u64` LE |
| **Plans** | `plan_claim` (per initialized reward) |

| # | Account | W | S | Notes |
|---|---------|---|---|-------|
| 0 | lb_pair | ✓ | | |
| 1 | position | ✓ | | |
| 2 | bin_array_lo | ✓ | | |
| 3 | bin_array_hi | ✓ | | |
| 4 | sender | | ✓ | |
| 5 | reward_vault | ✓ | | |
| 6 | reward_mint | | | |
| 7 | user_token | ✓ | | ATA |
| 8 | token_program | | | |
| 9 | event_authority | | | |
| 10 | program | | | |

---

## add_liquidity_by_strategy2

| | |
|--|--|
| **Disc** | `[3, 221, 149, 218, 111, 141, 118, 213]` |
| **Plans** | `plan_deposit_with_strategy`, `plan_create_funded` |

### Args (borsh after disc) — `LiquidityParameterByStrategy` + empty `RemainingAccountsInfo`

| Offset in args | Size | Field |
|----------------|------|-------|
| 0 | 8 | `amount_x: u64` |
| 8 | 8 | `amount_y: u64` |
| 16 | 4 | `active_id: i32` |
| 20 | 4 | `max_active_bin_slippage: i32` (crate uses `3`) |
| 24 | 4 | `min_bin_id: i32` |
| 28 | 4 | `max_bin_id: i32` |
| 32 | 1 | `strategy_type: u8` (`MeteoraStrategyType`) |
| 33 | 64 | `parameteres: [u8;64]` (zeros) |
| 97 | 4 | `RemainingAccountsInfo.slices` len = `0` |

Total args len: `8+8+4+4+4+4+1+64+4 = 105`.

### Accounts (fixed) + remaining bin arrays

| # | Account | W | S | Notes |
|---|---------|---|---|-------|
| 0 | position | ✓ | | |
| 1 | lb_pair | ✓ | | |
| 2 | bitmap_extension | ✓ | | or **program id** sentinel |
| 3 | user_token_x | ✓ | | |
| 4 | user_token_y | ✓ | | |
| 5 | reserve_x | ✓ | | |
| 6 | reserve_y | ✓ | | |
| 7 | mint_x | | | |
| 8 | mint_y | | | |
| 9 | sender | | ✓ | |
| 10 | token_program | | | |
| 11 | token_program | | | duplicated |
| 12 | event_authority | | | |
| 13 | program | | | |
| 14+ | bin_array… | ✓ | | ascending indices covering `[min,max]` |

---

## remove_liquidity

| | |
|--|--|
| **Disc** | `[80, 85, 209, 72, 24, 206, 177, 108]` |
| **Plans** | `plan_withdraw`, `plan_close_and_claim` |

### Args

Borsh `Vec<{ bin_id: i32, bps_to_remove: u16 }>`:

| | |
|--|--|
| `u32` | vec length = bin count in `[lower, upper]` |
| per bin | `i32` bin_id + `u16` bps (`1..=10_000`) |

Full exit uses `bps = 10_000` for every bin.

### Accounts

| # | Account | W | S | Notes |
|---|---------|---|---|-------|
| 0 | position | ✓ | | |
| 1 | lb_pair | ✓ | | |
| 2 | bitmap_extension | ✓ | | or program id |
| 3 | user_token_x | ✓ | | |
| 4 | user_token_y | ✓ | | |
| 5 | reserve_x | ✓ | | |
| 6 | reserve_y | ✓ | | |
| 7 | mint_x | | | |
| 8 | mint_y | | | |
| 9 | bin_array_lo | ✓ | | |
| 10 | bin_array_hi | ✓ | | |
| 11 | sender | | ✓ | |
| 12 | token_program | | | |
| 13 | token_program | | | |
| 14 | event_authority | | | |
| 15 | program | | | |

---

## close_position2

| | |
|--|--|
| **Disc** | `[174, 90, 35, 115, 186, 40, 147, 226]` |
| **Args** | none |
| **Plans** | `plan_close_and_claim` (after remove all) |

| # | Account | W | S | Notes |
|---|---------|---|---|-------|
| 0 | position | ✓ | | |
| 1 | sender | | ✓ | |
| 2 | rent_receiver | ✓ | | usually sender |
| 3 | event_authority | | | |
| 4 | program | | | |

---

## initialize_position2

| | |
|--|--|
| **Disc** | `[143, 19, 242, 145, 213, 15, 104, 115]` |
| **Plans** | `plan_create_funded` |

### Args

| Size | Field |
|------|-------|
| 4 | `lower_bin_id: i32` |
| 4 | `width: i32` (= `upper - lower + 1`) |

### Accounts

| # | Account | W | S | Notes |
|---|---------|---|---|-------|
| 0 | payer | ✓ | ✓ | |
| 1 | position | ✓ | ✓ | new keypair |
| 2 | lb_pair | | | |
| 3 | owner | | ✓ | |
| 4 | system_program | | | |
| 5 | event_authority | | | |
| 6 | program | | | |

---

## Not wired

| Op | Status |
|----|--------|
| Create lb-pair / pool | `plan_create_pool` → bail |
| Swap | not in crate |
| Bin array init | assumed already exists |
