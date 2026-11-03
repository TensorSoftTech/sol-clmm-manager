# Orca Whirlpool — instructions (repo write surface)

Discs from [`write.rs`](../../crates/orca-whirlpool/src/write.rs). **W/S** = writable / signer.

Shared **liquidity accounts** (increase / decrease):

| # | Account | W | S |
|---|---------|---|---|
| 0 | whirlpool | ✓ | |
| 1 | token_program | | |
| 2 | position_authority | | ✓ |
| 3 | position | ✓ | |
| 4 | position_token_account | | |
| 5 | token_owner_a | ✓ | |
| 6 | token_owner_b | ✓ | |
| 7 | token_vault_a | ✓ | |
| 8 | token_vault_b | ✓ | |
| 9 | tick_array_lower | ✓ | |
| 10 | tick_array_upper | ✓ | |

Liquidity args after disc: `liquidity: u128` · `token_max/min_a: u64` · `token_max/min_b: u64`.

---

## update_fees_and_rewards

| | |
|--|--|
| **Disc** | `[154, 230, 250, 13, 236, 209, 75, 223]` |
| **Args** | none |
| **Plans** | claim path when `L > 0` |

| # | Account | W | S |
|---|---------|---|---|
| 0 | whirlpool | ✓ | |
| 1 | position | ✓ | |
| 2 | tick_array_lower | | |
| 3 | tick_array_upper | | |

---

## collect_fees

| | |
|--|--|
| **Disc** | `[164, 152, 207, 99, 30, 186, 19, 182]` |
| **Args** | none |

| # | Account | W | S |
|---|---------|---|---|
| 0 | whirlpool | | |
| 1 | position_authority | | ✓ |
| 2 | position | ✓ | |
| 3 | position_token_account | | |
| 4 | token_owner_a | ✓ | |
| 5 | token_vault_a | ✓ | |
| 6 | token_owner_b | ✓ | |
| 7 | token_vault_b | ✓ | |
| 8 | token_program | | |

---

## collect_reward

| | |
|--|--|
| **Disc** | `[70, 5, 132, 87, 86, 235, 177, 34]` |
| **Args** | `reward_index: u8` |

| # | Account | W | S |
|---|---------|---|---|
| 0 | whirlpool | | |
| 1 | position_authority | | ✓ |
| 2 | position | ✓ | |
| 3 | position_token_account | | |
| 4 | reward_owner_account | ✓ | |
| 5 | reward_vault | ✓ | |
| 6 | token_program | | |

---

## increase_liquidity

| | |
|--|--|
| **Disc** | `[46, 156, 243, 118, 13, 205, 251, 178]` |
| **Args** | `liquidity u128` · `token_max_a u64` · `token_max_b u64` |
| **Accounts** | liquidity set above |
| **Plans** | deposit / create funded |

---

## decrease_liquidity

| | |
|--|--|
| **Disc** | `[160, 38, 208, 111, 104, 91, 44, 1]` |
| **Args** | `liquidity u128` · `token_min_a u64` · `token_min_b u64` |
| **Accounts** | liquidity set above |
| **Plans** | withdraw / close |

---

## close_position

| | |
|--|--|
| **Disc** | `[123, 134, 81, 0, 49, 68, 98, 98]` |
| **Args** | none |

| # | Account | W | S |
|---|---------|---|---|
| 0 | position_authority | | ✓ |
| 1 | receiver | ✓ | |
| 2 | position | ✓ | |
| 3 | position_mint | ✓ | |
| 4 | position_token_account | ✓ | |
| 5 | token_program | | |

---

## open_position

| | |
|--|--|
| **Disc** | `[135, 128, 47, 77, 15, 152, 240, 49]` |
| **Plans** | create funded |

### Args

| Size | Field |
|------|-------|
| 1 | `OpenPositionBumps.position_bump` |
| 4 | `tick_lower_index: i32` |
| 4 | `tick_upper_index: i32` |

### Accounts

| # | Account | W | S |
|---|---------|---|---|
| 0 | funder | ✓ | ✓ |
| 1 | owner | | |
| 2 | position | ✓ | | PDA |
| 3 | position_mint | ✓ | ✓ |
| 4 | position_token_account | ✓ | |
| 5 | whirlpool | | |
| 6 | token_program | | |
| 7 | system_program | | |
| 8 | rent | | |
| 9 | associated_token_program | | |

---

## initialize_pool

| | |
|--|--|
| **Disc** | `[95, 180, 10, 172, 84, 174, 232, 40]` |
| **Plans** | `plan_create_pool` |

### Args

| Size | Field |
|------|-------|
| 1 | bumps byte (`0` in crate) |
| 2 | `tick_spacing: u16` |
| 16 | `initial_sqrt_price: u128` |

### Accounts

| # | Account | W | S | Notes |
|---|---------|---|---|-------|
| 0 | whirlpools_config | | | |
| 1 | token_mint_a | | | ordered mint_a < mint_b |
| 2 | token_mint_b | | | |
| 3 | funder | ✓ | ✓ | |
| 4 | whirlpool | ✓ | | PDA |
| 5 | token_vault_a | ✓ | ✓ | fresh keypair |
| 6 | token_vault_b | ✓ | ✓ | fresh keypair |
| 7 | fee_tier | | | PDA |
| 8 | token_program | | | |
| 9 | system_program | | | |
| 10 | rent | | | |

---

## Not in crate

Swap, open_position_with_token_extensions, adaptive fee, admin config IXs, tick_array init.
