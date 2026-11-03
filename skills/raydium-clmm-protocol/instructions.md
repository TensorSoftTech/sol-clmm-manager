# Raydium CLMM — instructions (repo write surface)

Discs from [`write.rs`](../../crates/raydium-clmm/src/write.rs). **W/S** = writable / signer.

---

## increase_liquidity_v2

| | |
|--|--|
| **Disc** | `[133, 29, 89, 223, 69, 238, 176, 10]` |
| **Plans** | deposit |

### Args

| Size | Field |
|------|-------|
| 16 | `liquidity: u128` |
| 8 | `amount_0_max: u64` |
| 8 | `amount_1_max: u64` |
| 1 | `base_flag: Option<bool>` = `None` → `0` |

### Accounts (`increase_accounts`)

| # | Account | W | S |
|---|---------|---|---|
| 0 | nft_owner | | ✓ |
| 1 | nft_account | | |
| 2 | pool_state | ✓ | |
| 3 | protocol_position | | |
| 4 | personal_position | ✓ | |
| 5 | tick_array_lower | ✓ | |
| 6 | tick_array_upper | ✓ | |
| 7 | token_account_0 | ✓ | |
| 8 | token_account_1 | ✓ | |
| 9 | token_vault_0 | ✓ | |
| 10 | token_vault_1 | ✓ | |
| 11 | token_program | | |
| 12 | token_2022_program | | |
| 13 | vault_0_mint | | |
| 14 | vault_1_mint | | |
| 15 | tick_array_bitmap_extension | ✓ | |

---

## decrease_liquidity_v2

| | |
|--|--|
| **Disc** | `[58, 127, 188, 62, 79, 82, 196, 96]` |
| **Plans** | withdraw · claim (L=0) · close |

### Args

| Size | Field |
|------|-------|
| 16 | `liquidity: u128` |
| 8 | `amount_0_min: u64` |
| 8 | `amount_1_min: u64` |

### Accounts (`decrease_accounts`)

| # | Account | W | S | Notes |
|---|---------|---|---|-------|
| 0 | nft_owner | | ✓ | |
| 1 | nft_account | | | |
| 2 | personal_position | ✓ | | |
| 3 | pool_state | ✓ | | |
| 4 | protocol_position | | | |
| 5 | token_vault_0 | ✓ | | |
| 6 | token_vault_1 | ✓ | | |
| 7 | tick_array_lower | ✓ | | |
| 8 | tick_array_upper | ✓ | | |
| 9 | recipient_token_account_0 | ✓ | | |
| 10 | recipient_token_account_1 | ✓ | | |
| 11 | token_program | | | |
| 12 | token_2022_program | | | |
| 13 | memo_program | | | |
| 14 | vault_0_mint | | | |
| 15 | vault_1_mint | | | |
| 16 | tick_array_bitmap_extension | ✓ | remaining |

**Claim:** same IX with `liquidity = 0`, mins = 0 (pays fees/rewards).

---

## close_position

| | |
|--|--|
| **Disc** | `[123, 134, 81, 0, 49, 68, 98, 98]` (same bytes as Orca close — different program) |
| **Args** | none |

| # | Account | W | S | Notes |
|---|---------|---|---|-------|
| 0 | nft_owner | ✓ | ✓ | |
| 1 | position_nft_mint | ✓ | | |
| 2 | position_nft_account | ✓ | | |
| 3 | personal_position | ✓ | | |
| 4 | system_program | | | |
| 5 | token_program | | | Token **or** Token-2022 matching NFT |
| 6+ | pool_state | | | remaining (thaw) |

---

## open_position_with_token22_nft

| | |
|--|--|
| **Disc** | `[77, 255, 174, 82, 125, 29, 201, 46]` |
| **Plans** | `plan_create_funded` |

### Args

| Size | Field |
|------|-------|
| 4 | `tick_lower_index: i32` |
| 4 | `tick_upper_index: i32` |
| 4 | `tick_array_lower_start_index: i32` |
| 4 | `tick_array_upper_start_index: i32` |
| 16 | `liquidity: u128` |
| 8 | `amount_0_max: u64` |
| 8 | `amount_1_max: u64` |
| 1 | `with_metadata: bool` (= false) |
| 1 | `base_flag: Option<bool>` (= None → 0) |

### Accounts

| # | Account | W | S | Notes |
|---|---------|---|---|-------|
| 0 | payer | ✓ | ✓ | |
| 1 | position_nft_owner | | | usually payer |
| 2 | position_nft_mint | ✓ | ✓ | |
| 3 | position_nft_account | ✓ | | Token-2022 ATA |
| 4 | pool_state | ✓ | | |
| 5 | protocol_position | | | |
| 6 | tick_array_lower | ✓ | | |
| 7 | tick_array_upper | ✓ | | |
| 8 | personal_position | ✓ | | |
| 9 | token_account_0 | ✓ | | |
| 10 | token_account_1 | ✓ | | |
| 11 | token_vault_0 | ✓ | | |
| 12 | token_vault_1 | ✓ | | |
| 13 | rent | | | |
| 14 | system_program | | | |
| 15 | token_program | | | |
| 16 | associated_token_program | | | |
| 17 | token_2022_program | | | |
| 18 | vault_0_mint | | | |
| 19 | vault_1_mint | | | |
| 20 | tick_array_bitmap_extension | ✓ | remaining |

---

## create_pool

| | |
|--|--|
| **Disc** | `[233, 146, 209, 142, 207, 104, 64, 188]` |
| **Plans** | `plan_create_pool` |

### Args

| Size | Field |
|------|-------|
| 16 | `sqrt_price_x64: u128` |
| 8 | `open_time: u64` (crate uses `0`) |

### Accounts

| # | Account | W | S | Notes |
|---|---------|---|---|-------|
| 0 | pool_creator | ✓ | ✓ | |
| 1 | amm_config | | | PDA by fee index |
| 2 | pool_state | ✓ | | PDA |
| 3 | token_mint_0 | | | mint0 < mint1 ordered |
| 4 | token_mint_1 | | | |
| 5 | token_vault_0 | ✓ | | PDA |
| 6 | token_vault_1 | ✓ | | PDA |
| 7 | observation_state | ✓ | | PDA |
| 8 | tick_array_bitmap | ✓ | | extension PDA |
| 9 | token_program_0 | | | |
| 10 | token_program_1 | | | |
| 11 | system_program | | | |

---

## Not in crate

Separate collect_fees · OpenPosition / OpenPositionV2 (legacy) · swap · admin.
