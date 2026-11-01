# Orca Whirlpool — deposit / swap test cases

Manual + automated checklist for **manage deposit** and **create** on Orca Whirlpools (CLMM).  
Pool under test: **SOL/USDC** (mint A = WSOL `So111…112`, mint B = USDC). Treat SOL as **native** in the TUI; on-chain leg is WSOL.

Related code:

- `sol_clmm_manager::range_deposit_side` / `swap_to_match_hint` / `analyze_deposit`
- `sol_clmm_client::assemble_manage_deposit` / `assemble_create_position`
- Fixtures: `crates/sol-clmm-manager/tests/fixtures/{pools,deposit_cases,positions}.json`
- Unit tests: `cargo test -p sol-clmm-manager --features write`

---

## Range vocabulary

| Pool price vs position `[tick_lower, tick_upper)` | Side | Tokens the position accepts |
|--------------------------------------------------|------|-----------------------------|
| **Below** range (`√P ≤ √P_lower`) | `TokenAOnly` | **SOL only** |
| **Inside** range | `Both` | SOL **+** USDC (fixed ratio) |
| **Above** range (`√P ≥ √P_upper`) | `TokenBOnly` | **USDC only** |

---

## A. Out of range — binary swap rule

Out-of-range is **not** a proportional ratio swap.

| # | Range | You fund | Expected swap | Expected deposit |
|---|-------|----------|---------------|------------------|
| A1 | Below (A only) | **0.01 SOL** (SingleA) | **none** | all SOL → increase L · 0 USDC |
| A2 | Below (A only) | **10 USDC** (SingleB) | **full** USDC→SOL | all proceeds as SOL · 0 USDC left for B leg |
| A3 | Above (B only) | **10 USDC** (SingleB) | **none** | all USDC → increase L · 0 SOL |
| A4 | Above (B only) | **0.01 SOL** (SingleA) | **full** SOL→USDC | all proceeds as USDC · 0 SOL left for A leg |
| A5 | Below | Dual, SOL only typed (pair fill 0) | **none** | same as A1 |
| A6 | Below | Dual, SOL + leftover USDC in wallet amounts | **full** USDC→SOL | convert B, then deposit A |
| A7 | Above | Dual, USDC only | **none** | same as A3 |
| A8 | Above | Dual with SOL budget | **full** SOL→USDC | convert A, then deposit B |

### Pass / fail (A1 — the bug we hit)

- **Fail (old):** Jupiter swaps most of 0.01 SOL → tiny USDC, then `increase_liquidity` with leftover WSOL + **0 USDC**.
- **Pass:** no Jupiter ix · wrap? → `increase_liquidity` SOL only → unwrap/close WSOL ATA.

### Unit coverage

```bash
cargo test -p sol-clmm-manager --features write -- \
  single_a_below_range_no_hint \
  single_b_above_range_no_hint \
  single_a_above_range_full_swap_to_b \
  single_b_below_range_full_swap_to_a
```

| Test | Asserts |
|------|---------|
| `single_a_below_range_no_hint` | A-only + SingleA → `swap_to_match_hint` is `None` |
| `single_b_above_range_no_hint` | B-only + SingleB → `None` |
| `single_a_above_range_full_swap_to_b` | B-only + SingleA → `in_is_a` · `swap_in == amount_a` |
| `single_b_below_range_full_swap_to_a` | A-only + SingleB → `!in_is_a` · `swap_in == amount_b` |

---

## B. In range — proportional ratio swap

Price inside `[lower, upper)`. 1-side must leave some of the funded token for the other leg (never 100%).

| # | Shape | Fund | Expected swap | Notes |
|---|-------|------|---------------|-------|
| B1 | SingleA | SOL | **partial** SOL→USDC | leave ≥1 lamport SOL; both legs after swap |
| B2 | SingleB | USDC | **partial** USDC→SOL | leave ≥1 raw USDC; both legs |
| B3 | Dual | SOL+USDC on-ratio | **none** | already matched |
| B4 | Dual | SOL-heavy | **partial** SOL→USDC | excess A only |
| B5 | Dual | USDC-heavy | **partial** USDC→SOL | excess B only |
| B6 | SingleB | tiny USDC (e.g. 0.05) | partial, **value-weighted** | must not swap ~all USDC (9/6 decimal trap) |

### Fixture cases (`deposit_cases.json`)

Symmetric in-range bands on `wsol_usdc_150` / related pools — see ids like:

- `wsol_usdc_tiny_usdc_sym` … `wsol_usdc_1000_usdc_sym`
- `wsol_usdc_0.01_sol_sym` (and larger SOL single-A cases)

```bash
cargo test -p sol-clmm-manager --test swap_match_fixtures --features write
```

---

## C. SOL wrap / unwrap (native SOL policy)

Jupiter: always `wrapAndUnwrapSol=false`. We own wrap/unwrap.

| # | Op | Start | Middle | End |
|---|----|-------|--------|-----|
| C1 | Deposit A-only (out of range), fund native SOL | `wrap_sol_ixs` | `increase_liquidity` | `unwrap_sol_ix` (close WSOL ATA) |
| C2 | Deposit + full wrong-side swap | wrap if funding SOL · Jupiter keeps WSOL | swap · increase | unwrap |
| C3 | Deposit in-range SingleA | wrap SOL budget | proportional swap · increase | unwrap |
| C4 | Claim / withdraw / close | `CreateIdempotent` ATAs (WSOL may be gone) | venue ixs | unwrap |
| C5 | Manage → wrap SOL / unwrap SOL | standalone `wrap_sol_ixs` / `unwrap_sol_ix` | — | — |

### Pass / fail

- **Fail:** Jupiter cleanup unwraps WSOL before deposit · or missing WSOL ATA after prior unwrap on withdraw.
- **Pass:** wrap prefix · swap/deposit · unwrap suffix; withdraw recreates ATA then unwraps.

---

## D. Manage deposit UX

| # | Situation | UI / wizard |
|---|-----------|-------------|
| D1 | Open deposit on out-of-range A-only | default shape = **SingleA** · label `no swap` |
| D2 | Open deposit on out-of-range B-only | default = **SingleB** · `no swap` |
| D3 | Open deposit in-range | default = Dual; 1-side shows `+ ratio swap` |
| D4 | Out-of-range wrong token selected | shows `+ full swap` · slippage step appears |
| D5 | Out-of-range correct token | **no** slippage step · Ready after amount |
| D6 | After landed tx | wallet SOL/ATA line refreshed on confirm Done |

---

## E. Live smoke (manual, write feature)

Use a throwaway wallet + private RPC. Position: Orca SOL/USDC Whirlpool.

### E1 — Out of range, correct side (no swap)

1. Find / create a position with price **below** range (needs SOL only).
2. Manage → deposit → **SOL only** → `0.01`.
3. Confirm plan: **no** Jupiter/Titan swap ixs.
4. Sim + send.
5. Explorer: wrap? → `increase_liquidity` with SOL/WSOL only → unwrap. **No** aggregator swap.

### E2 — Out of range, wrong side (full swap)

1. Same A-only position.
2. Deposit **USDC only** (e.g. `5`).
3. Plan: Jupiter **full** USDC→SOL · then increase (SOL only).
4. Explorer: one swap consuming ~all USDC input · deposit SOL · unwrap.

### E3 — In range, 1-side SOL

1. In-range position.
2. Deposit SingleA `0.01` SOL · RTSE slip.
3. Plan: wrap · **partial** SOL→USDC · increase both · unwrap.
4. Both tokenMax used; leftover funded SOL still in deposit.

### E4 — Withdraw / close on SOL pool

1. After E1, withdraw or close.
2. Plan starts with ATA `CreateIdempotent` · ends with unwrap.
3. Wallet native SOL increases; WSOL ATA closed (or empty).

---

## F. Regression matrix (quick)

| Case | Swap? | Swap size | Deposit legs |
|------|-------|-----------|--------------|
| Below + SOL | no | — | A |
| Below + USDC | yes | **100%** USDC | A (from swap) |
| Above + USDC | no | — | B |
| Above + SOL | yes | **100%** SOL | B (from swap) |
| In + SOL 1-side | yes | **ratio share** (&lt;100%) | A+B |
| In + USDC 1-side | yes | **ratio share** (&lt;100%) | A+B |
| In + Dual matched | no | — | A+B |

---

## G. Reference pool (fixture)

| Field | Value |
|-------|--------|
| Fixture id | `wsol_usdc_150` |
| Symbols | WSOL / USDC |
| Decimals | 9 / 6 |
| Tick spacing | 64 |
| Approx mid | 150 USDC per SOL |

Live mainnet pool pubkey: paste into create wizard when smoke-testing (do not hard-code secrets/keys here).

---

## H. Implementation map

| Behavior | Primary symbol |
|----------|----------------|
| Side classify | `range_deposit_side` |
| Swap size | `swap_to_match_hint` |
| Need-swap? | `analyze_deposit` · manage `swap_needed` |
| Assemble tx | `Client::assemble_manage_deposit` |
| Wrap / unwrap | `wrap_sol_ixs` · `unwrap_sol_ix` · `ensure_pool_atas_ixs` |
| Jupiter flag | `wrapAndUnwrapSol=false` in `write/swap.rs` |
