//! Orca Whirlpool write IXs (feature `write`). Transaction assembly is in manager (tx v1).

#![cfg(feature = "write")]

use anyhow::Result;
use sol_clmm_manager::{
    CreatePoolParams, CreatePositionParams, ExpectedDeltas, PositionDetail, SolLegMode, WriteOp,
    WritePlan, ensure_atas_ixs, maybe_unwrap_after, maybe_wrap_before_deposit,
};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use std::str::FromStr;

use crate::decode::{tick_array_pda, tick_array_start_index};
use crate::whirlpool_program_id;

const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const ASSOCIATED_TOKEN_PROGRAM: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
const SYSTEM_PROGRAM: &str = "11111111111111111111111111111111";
const RENT_SYSVAR: &str = "SysvarRent111111111111111111111111111111111";

const COLLECT_FEES: [u8; 8] = [164, 152, 207, 99, 30, 186, 19, 182];
const COLLECT_REWARD: [u8; 8] = [70, 5, 132, 87, 86, 235, 177, 34];
const UPDATE_FEES_AND_REWARDS: [u8; 8] = [154, 230, 250, 13, 236, 209, 75, 223];
const INCREASE_LIQUIDITY: [u8; 8] = [46, 156, 243, 118, 13, 205, 251, 178];
const DECREASE_LIQUIDITY: [u8; 8] = [160, 38, 208, 111, 104, 91, 44, 1];
const CLOSE_POSITION: [u8; 8] = [123, 134, 81, 0, 49, 68, 98, 98];
const OPEN_POSITION: [u8; 8] = [135, 128, 47, 77, 15, 152, 240, 49];

fn program_id() -> Pubkey {
    whirlpool_program_id()
}

fn token_program() -> Pubkey {
    Pubkey::from_str(TOKEN_PROGRAM).expect("token")
}

fn associated_token_program() -> Pubkey {
    Pubkey::from_str(ASSOCIATED_TOKEN_PROGRAM).expect("ata")
}

fn system_program() -> Pubkey {
    Pubkey::from_str(SYSTEM_PROGRAM).expect("system")
}

fn rent_sysvar() -> Pubkey {
    Pubkey::from_str(RENT_SYSVAR).expect("rent")
}

fn ata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), token_program().as_ref(), mint.as_ref()],
        &associated_token_program(),
    )
    .0
}

fn position_pda(position_mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"position", position_mint.as_ref()], &program_id()).0
}

/// Position NFT token account (ATA of wallet for position mint).
pub fn position_token_account(wallet: &Pubkey, position_mint: &Pubkey) -> Pubkey {
    ata(wallet, position_mint)
}

fn tick_arrays_for_range(
    pool: &Pubkey,
    tick_lower: i32,
    tick_upper: i32,
    spacing: u16,
) -> (Pubkey, Pubkey) {
    let prog = program_id();
    let lower = tick_array_pda(pool, tick_array_start_index(tick_lower, spacing), &prog);
    let upper = tick_array_pda(pool, tick_array_start_index(tick_upper, spacing), &prog);
    (lower, upper)
}

#[allow(clippy::too_many_arguments)]
fn liquidity_accounts(
    whirlpool: Pubkey,
    authority: Pubkey,
    position: Pubkey,
    position_token: Pubkey,
    owner_a: Pubkey,
    owner_b: Pubkey,
    vault_a: Pubkey,
    vault_b: Pubkey,
    tick_lower_array: Pubkey,
    tick_upper_array: Pubkey,
) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new(whirlpool, false),
        AccountMeta::new_readonly(token_program(), false),
        AccountMeta::new_readonly(authority, true),
        AccountMeta::new(position, false),
        AccountMeta::new_readonly(position_token, false),
        AccountMeta::new(owner_a, false),
        AccountMeta::new(owner_b, false),
        AccountMeta::new(vault_a, false),
        AccountMeta::new(vault_b, false),
        AccountMeta::new(tick_lower_array, false),
        AccountMeta::new(tick_upper_array, false),
    ]
}

fn liquidity_ix_data(disc: [u8; 8], liquidity: u128, a: u64, b: u64) -> Vec<u8> {
    let mut d = disc.to_vec();
    d.extend_from_slice(&liquidity.to_le_bytes());
    d.extend_from_slice(&a.to_le_bytes());
    d.extend_from_slice(&b.to_le_bytes());
    d
}

#[allow(clippy::too_many_arguments)]
pub fn collect_fees_ix(
    whirlpool: Pubkey,
    authority: Pubkey,
    position: Pubkey,
    position_token: Pubkey,
    token_owner_a: Pubkey,
    token_vault_a: Pubkey,
    token_owner_b: Pubkey,
    token_vault_b: Pubkey,
) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new_readonly(whirlpool, false),
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(position, false),
            AccountMeta::new_readonly(position_token, false),
            AccountMeta::new(token_owner_a, false),
            AccountMeta::new(token_vault_a, false),
            AccountMeta::new(token_owner_b, false),
            AccountMeta::new(token_vault_b, false),
            AccountMeta::new_readonly(token_program(), false),
        ],
        data: COLLECT_FEES.to_vec(),
    }
}

pub fn collect_reward_ix(
    whirlpool: Pubkey,
    authority: Pubkey,
    position: Pubkey,
    position_token: Pubkey,
    reward_owner: Pubkey,
    reward_vault: Pubkey,
    reward_index: u8,
) -> Instruction {
    let mut data = COLLECT_REWARD.to_vec();
    data.push(reward_index);
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new_readonly(whirlpool, false),
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(position, false),
            AccountMeta::new_readonly(position_token, false),
            AccountMeta::new(reward_owner, false),
            AccountMeta::new(reward_vault, false),
            AccountMeta::new_readonly(token_program(), false),
        ],
        data,
    }
}

pub fn increase_liquidity_ix(
    accounts: Vec<AccountMeta>,
    liquidity: u128,
    token_max_a: u64,
    token_max_b: u64,
) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts,
        data: liquidity_ix_data(INCREASE_LIQUIDITY, liquidity, token_max_a, token_max_b),
    }
}

pub fn decrease_liquidity_ix(
    accounts: Vec<AccountMeta>,
    liquidity: u128,
    token_min_a: u64,
    token_min_b: u64,
) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts,
        data: liquidity_ix_data(DECREASE_LIQUIDITY, liquidity, token_min_a, token_min_b),
    }
}

pub fn close_position_ix(
    authority: Pubkey,
    receiver: Pubkey,
    position: Pubkey,
    position_mint: Pubkey,
    position_token: Pubkey,
) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(receiver, false),
            AccountMeta::new(position, false),
            AccountMeta::new(position_mint, false),
            AccountMeta::new(position_token, false),
            AccountMeta::new_readonly(token_program(), false),
        ],
        data: CLOSE_POSITION.to_vec(),
    }
}

/// `open_position` — funder signs; `position_mint` must also sign (fresh keypair).
#[allow(clippy::too_many_arguments)]
pub fn open_position_ix(
    funder: Pubkey,
    owner: Pubkey,
    position: Pubkey,
    position_mint: Pubkey,
    position_token: Pubkey,
    whirlpool: Pubkey,
    position_bump: u8,
    tick_lower: i32,
    tick_upper: i32,
) -> Instruction {
    let mut data = OPEN_POSITION.to_vec();
    data.push(position_bump); // OpenPositionBumps.position_bump
    data.extend_from_slice(&tick_lower.to_le_bytes());
    data.extend_from_slice(&tick_upper.to_le_bytes());
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(funder, true),
            AccountMeta::new_readonly(owner, false),
            AccountMeta::new(position, false),
            AccountMeta::new(position_mint, true),
            AccountMeta::new(position_token, false),
            AccountMeta::new_readonly(whirlpool, false),
            AccountMeta::new_readonly(token_program(), false),
            AccountMeta::new_readonly(system_program(), false),
            AccountMeta::new_readonly(rent_sysvar(), false),
            AccountMeta::new_readonly(associated_token_program(), false),
        ],
        data,
    }
}

fn require_mint(detail: &PositionDetail) -> Result<Pubkey> {
    detail
        .position_mint
        .filter(|m| *m != Pubkey::default())
        .ok_or_else(|| anyhow::anyhow!("orca position mint missing from detail"))
}

fn liquidity_metas_from_detail(
    authority: Pubkey,
    detail: &PositionDetail,
    mint: Pubkey,
) -> Vec<AccountMeta> {
    let position_token = position_token_account(&authority, &mint);
    let (tick_lo, tick_hi) = tick_arrays_for_range(
        &detail.pool.address,
        detail.row.tick_lower,
        detail.row.tick_upper,
        detail.pool.tick_spacing,
    );
    liquidity_accounts(
        detail.pool.address,
        authority,
        detail.row.address,
        position_token,
        ata(&authority, &detail.token_a.mint),
        ata(&authority, &detail.token_b.mint),
        detail.pool.token_vault_a,
        detail.pool.token_vault_b,
        tick_lo,
        tick_hi,
    )
}

/// Fee ATAs only (A/B). Reward ATAs are added only when collecting claimable rewards.
fn owner_fee_mints(detail: &PositionDetail) -> Vec<Pubkey> {
    vec![detail.token_a.mint, detail.token_b.mint]
}

fn owner_claim_mints(detail: &PositionDetail) -> Vec<Pubkey> {
    let mut mints = owner_fee_mints(detail);
    for (_, rmint, _) in &detail.reward_vaults {
        if *rmint != Pubkey::default() {
            mints.push(*rmint);
        }
    }
    mints
}

pub fn update_fees_and_rewards_ix(
    whirlpool: Pubkey,
    position: Pubkey,
    tick_lower_array: Pubkey,
    tick_upper_array: Pubkey,
) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(whirlpool, false),
            AccountMeta::new(position, false),
            AccountMeta::new_readonly(tick_lower_array, false),
            AccountMeta::new_readonly(tick_upper_array, false),
        ],
        data: UPDATE_FEES_AND_REWARDS.to_vec(),
    }
}

fn claim_ixs(authority: Pubkey, detail: &PositionDetail, mint: Pubkey) -> Vec<Instruction> {
    let position_token = position_token_account(&authority, &mint);
    // Create destination ATAs first — missing ATA → Anchor 3012 AccountNotInitialized.
    // Reward ATAs only for claimable slots (see PositionDetail::reward_vaults).
    let mut ixs = ensure_atas_ixs(&authority, &authority, &owner_claim_mints(detail));
    // Accrue fees into position (skip when L=0 — program errors LiquidityZero).
    if detail.position_liquidity > 0 {
        let (tick_lo, tick_hi) = tick_arrays_for_range(
            &detail.pool.address,
            detail.row.tick_lower,
            detail.row.tick_upper,
            detail.pool.tick_spacing,
        );
        ixs.push(update_fees_and_rewards_ix(
            detail.pool.address,
            detail.row.address,
            tick_lo,
            tick_hi,
        ));
    }
    ixs.push(collect_fees_ix(
        detail.pool.address,
        authority,
        detail.row.address,
        position_token,
        ata(&authority, &detail.token_a.mint),
        detail.pool.token_vault_a,
        ata(&authority, &detail.token_b.mint),
        detail.pool.token_vault_b,
    ));
    for (idx, rmint, vault) in &detail.reward_vaults {
        if *rmint == Pubkey::default() || *vault == Pubkey::default() {
            continue;
        }
        ixs.push(collect_reward_ix(
            detail.pool.address,
            authority,
            detail.row.address,
            position_token,
            ata(&authority, rmint),
            *vault,
            *idx,
        ));
    }
    ixs
}

/// Build claim fees + rewards plan from detail.
pub fn plan_claim(authority: Pubkey, detail: &PositionDetail) -> Result<WritePlan> {
    let mint = require_mint(detail)?;
    Ok(WritePlan {
        op: WriteOp::ClaimFeesRewards,
        venue_label: "orca whirlpool".into(),
        summary: format!(
            "claim fees ≈ {} + {} (+ {} rewards)",
            detail.fee_a_ui,
            detail.fee_b_ui,
            detail.rewards.len()
        ),
        instructions: claim_ixs(authority, detail, mint),
        follow_up: None,
        expected: ExpectedDeltas {
            amount_a: 0,
            amount_b: 0,
            fee_a: detail.fee_a,
            fee_b: detail.fee_b,
            note: "claim fees + rewards".into(),
        },
        payer: authority,
    })
}

pub fn plan_deposit(
    authority: Pubkey,
    detail: &PositionDetail,
    liquidity: u128,
    token_max_a: u64,
    token_max_b: u64,
    sol_leg: SolLegMode,
) -> Result<WritePlan> {
    anyhow::ensure!(liquidity > 0, "liquidity delta is 0");
    let mint = require_mint(detail)?;
    let wrap_amt = if detail.token_a.mint.to_string().starts_with("So111") {
        token_max_a
    } else {
        token_max_b
    };
    let mut ixs = maybe_wrap_before_deposit(
        sol_leg,
        &authority,
        &detail.token_a.mint,
        &detail.token_b.mint,
        wrap_amt,
    );
    ixs.push(increase_liquidity_ix(
        liquidity_metas_from_detail(authority, detail, mint),
        liquidity,
        token_max_a,
        token_max_b,
    ));
    ixs.extend(maybe_unwrap_after(
        sol_leg,
        &authority,
        &detail.token_a.mint,
        &detail.token_b.mint,
    ));
    Ok(WritePlan {
        op: WriteOp::Deposit,
        venue_label: "orca whirlpool".into(),
        summary: format!("deposit ΔL {liquidity} · max {token_max_a}/{token_max_b}"),
        instructions: ixs,
        follow_up: None,
        expected: ExpectedDeltas {
            amount_a: -(token_max_a as i128),
            amount_b: -(token_max_b as i128),
            note: "wrap? → increase_liquidity → unwrap?".into(),
            ..Default::default()
        },
        payer: authority,
    })
}

pub fn plan_withdraw(
    authority: Pubkey,
    detail: &PositionDetail,
    liquidity_delta: u128,
    token_min_a: u64,
    token_min_b: u64,
    sol_leg: SolLegMode,
) -> Result<WritePlan> {
    anyhow::ensure!(liquidity_delta > 0, "liquidity delta is 0");
    let mint = require_mint(detail)?;
    // Withdraw only needs fee ATAs — do not CreateIdempotent reward ATAs (rent).
    let mut ixs = ensure_atas_ixs(&authority, &authority, &owner_fee_mints(detail));
    ixs.push(decrease_liquidity_ix(
        liquidity_metas_from_detail(authority, detail, mint),
        liquidity_delta,
        token_min_a,
        token_min_b,
    ));
    ixs.extend(maybe_unwrap_after(
        sol_leg,
        &authority,
        &detail.token_a.mint,
        &detail.token_b.mint,
    ));
    Ok(WritePlan {
        op: WriteOp::Withdraw,
        venue_label: "orca whirlpool".into(),
        summary: format!("withdraw ΔL {liquidity_delta}"),
        instructions: ixs,
        follow_up: None,
        expected: ExpectedDeltas {
            note: format!("ensure ATAs → decrease ΔL={liquidity_delta} → unwrap?"),
            ..Default::default()
        },
        payer: authority,
    })
}

/// Claim fees+rewards + withdraw liquidity — position account stays open.
pub fn plan_claim_and_withdraw(
    authority: Pubkey,
    detail: &PositionDetail,
    liquidity_delta: u128,
    sol_leg: SolLegMode,
) -> Result<WritePlan> {
    let mint = require_mint(detail)?;
    let mut ixs = claim_ixs(authority, detail, mint);
    let liq = liquidity_delta.min(detail.position_liquidity);
    if liq > 0 {
        ixs.push(decrease_liquidity_ix(
            liquidity_metas_from_detail(authority, detail, mint),
            liq,
            0,
            0,
        ));
    }
    ixs.extend(maybe_unwrap_after(
        sol_leg,
        &authority,
        &detail.token_a.mint,
        &detail.token_b.mint,
    ));
    let pct = liq
        .saturating_mul(100)
        .checked_div(detail.position_liquidity)
        .unwrap_or(0)
        .min(100);
    Ok(WritePlan {
        op: WriteOp::ClaimAndWithdraw,
        venue_label: "orca whirlpool".into(),
        summary: format!(
            "claim + withdraw {pct}% · ΔL={liq} · keep position · sol={}",
            sol_leg.label()
        ),
        instructions: ixs,
        follow_up: None,
        expected: ExpectedDeltas {
            fee_a: detail.fee_a,
            fee_b: detail.fee_b,
            note: format!("collect_fees(+rewards) → decrease_liquidity({pct}%) · position stays"),
            ..Default::default()
        },
        payer: authority,
    })
}

/// Claim fees+rewards + decrease full L + close position — single tx.
pub fn plan_close_and_claim(
    authority: Pubkey,
    detail: &PositionDetail,
    sol_leg: SolLegMode,
) -> Result<WritePlan> {
    let mint = require_mint(detail)?;
    let position_token = position_token_account(&authority, &mint);
    let mut ixs = claim_ixs(authority, detail, mint);
    let liq = detail.position_liquidity;
    if liq > 0 {
        ixs.push(decrease_liquidity_ix(
            liquidity_metas_from_detail(authority, detail, mint),
            liq,
            0,
            0,
        ));
    }
    ixs.push(close_position_ix(
        authority,
        authority,
        detail.row.address,
        mint,
        position_token,
    ));
    ixs.extend(maybe_unwrap_after(
        sol_leg,
        &authority,
        &detail.token_a.mint,
        &detail.token_b.mint,
    ));
    Ok(WritePlan {
        op: WriteOp::CloseAndClaim,
        venue_label: "orca whirlpool".into(),
        summary: format!(
            "claim + withdraw all + close · L={} · sol={}",
            detail.position_liquidity,
            sol_leg.label()
        ),
        instructions: ixs,
        follow_up: None,
        expected: ExpectedDeltas {
            fee_a: detail.fee_a,
            fee_b: detail.fee_b,
            note: "collect_fees(+rewards) → decrease_liquidity(all) → close_position · 1 tx".into(),
            ..Default::default()
        },
        payer: authority,
    })
}

/// Create plan without pool vault decode — open only; Client should prefer
/// [`plan_create_funded`] once vaults/spacing are known.
pub fn plan_create_position(authority: Pubkey, params: &CreatePositionParams) -> Result<WritePlan> {
    let swap = if params.swap_to_match {
        " · swap-to-match"
    } else {
        ""
    };
    Ok(WritePlan {
        op: WriteOp::CreatePosition,
        venue_label: "orca whirlpool".into(),
        summary: format!(
            "create · {} · pool {}… · ticks [{}, {}] · {}/{} · {}{} · need mint+pool fetch for metas",
            params.shape_label(),
            &params.pool.to_string()[..8],
            params.tick_lower,
            params.tick_upper,
            params.amount_a,
            params.amount_b,
            params.sol_leg.label(),
            swap
        ),
        instructions: Vec::new(),
        follow_up: None,
        expected: ExpectedDeltas {
            amount_a: params.amount_a as i128,
            amount_b: params.amount_b as i128,
            note: "call plan_create_funded with position_mint + pool vaults".into(),
            ..Default::default()
        },
        payer: authority,
    })
}

/// Fully-funded create: open + increase with known pool vaults / spacing / mints.
/// `position_mint` must be a fresh keypair that also signs the tx.
#[allow(clippy::too_many_arguments)]
pub fn plan_create_funded(
    authority: Pubkey,
    params: &CreatePositionParams,
    position_mint: Pubkey,
    mint_a: Pubkey,
    mint_b: Pubkey,
    vault_a: Pubkey,
    vault_b: Pubkey,
    tick_spacing: u16,
    liquidity: u128,
    token_max_a: u64,
    token_max_b: u64,
) -> Result<WritePlan> {
    let (position, bump) =
        Pubkey::find_program_address(&[b"position", position_mint.as_ref()], &program_id());
    let position_token = position_token_account(&authority, &position_mint);
    let (tick_lo, tick_hi) = tick_arrays_for_range(
        &params.pool,
        params.tick_lower,
        params.tick_upper,
        tick_spacing,
    );

    // Wrap is prepended by Client::assemble (wallet SOL only — not post-swap needs).
    let mut ixs = Vec::new();
    ixs.push(open_position_ix(
        authority,
        authority,
        position,
        position_mint,
        position_token,
        params.pool,
        bump,
        params.tick_lower,
        params.tick_upper,
    ));
    if liquidity > 0 {
        ixs.push(increase_liquidity_ix(
            liquidity_accounts(
                params.pool,
                authority,
                position,
                position_token,
                ata(&authority, &mint_a),
                ata(&authority, &mint_b),
                vault_a,
                vault_b,
                tick_lo,
                tick_hi,
            ),
            liquidity,
            token_max_a,
            token_max_b,
        ));
    }
    let swap = if params.swap_to_match {
        " · swap-to-match"
    } else {
        ""
    };
    Ok(WritePlan {
        op: WriteOp::CreatePosition,
        venue_label: "orca whirlpool".into(),
        summary: format!(
            "create · {} · pool {}… · ticks [{}, {}] · L={}{}",
            params.shape_label(),
            &params.pool.to_string()[..8],
            params.tick_lower,
            params.tick_upper,
            liquidity,
            swap
        ),
        instructions: ixs,
        follow_up: None,
        expected: ExpectedDeltas {
            amount_a: -(token_max_a as i128),
            amount_b: -(token_max_b as i128),
            note: "wrap? → open_position + increase_liquidity · 1 tx v1".into(),
            ..Default::default()
        },
        payer: authority,
    })
}

#[allow(dead_code)]
fn _position_pda_export(mint: &Pubkey) -> Pubkey {
    position_pda(mint)
}

const INITIALIZE_POOL: [u8; 8] = [95, 180, 10, 172, 84, 174, 232, 40];

fn fee_tier_pda(config: &Pubkey, tick_spacing: u16) -> Pubkey {
    Pubkey::find_program_address(
        &[b"fee_tier", config.as_ref(), &tick_spacing.to_le_bytes()],
        &program_id(),
    )
    .0
}

fn whirlpool_pda(config: &Pubkey, mint_a: &Pubkey, mint_b: &Pubkey, tick_spacing: u16) -> Pubkey {
    Pubkey::find_program_address(
        &[
            b"whirlpool",
            config.as_ref(),
            mint_a.as_ref(),
            mint_b.as_ref(),
            &tick_spacing.to_le_bytes(),
        ],
        &program_id(),
    )
    .0
}

fn price_to_sqrt_x64(human_b_per_a: f64, dec_a: u8, dec_b: u8) -> anyhow::Result<u128> {
    anyhow::ensure!(
        human_b_per_a.is_finite() && human_b_per_a > 0.0,
        "initial price must be > 0"
    );
    let scale = 10f64.powi(i32::from(dec_b) - i32::from(dec_a));
    let raw = human_b_per_a * scale;
    anyhow::ensure!(raw.is_finite() && raw > 0.0, "raw price invalid");
    let sqrt = raw.sqrt();
    let q = sqrt * ((1u128 << 64) as f64);
    anyhow::ensure!(
        q.is_finite() && q > 0.0 && q < u128::MAX as f64,
        "sqrt_price overflow"
    );
    Ok(q as u128)
}

/// Initialize an Orca whirlpool (pool-only). `vault_a`/`vault_b` must sign.
pub fn plan_create_pool(
    authority: Pubkey,
    params: &CreatePoolParams,
    vault_a: Pubkey,
    vault_b: Pubkey,
) -> anyhow::Result<WritePlan> {
    use sol_clmm_manager::{CreatePoolMode, WHIRLPOOLS_CONFIG};
    anyhow::ensure!(params.mint_a != params.mint_b, "mints must differ");
    let config = Pubkey::from_str(WHIRLPOOLS_CONFIG).expect("config");
    let tick_spacing = params.fee_param.max(1);
    let (mint_a, mint_b, ordered) = if params.mint_a.to_bytes() < params.mint_b.to_bytes() {
        (params.mint_a, params.mint_b, true)
    } else {
        (params.mint_b, params.mint_a, false)
    };
    let price = if ordered {
        params.initial_price
    } else if params.initial_price > 0.0 {
        1.0 / params.initial_price
    } else {
        anyhow::bail!("initial price must be > 0");
    };
    let sqrt = price_to_sqrt_x64(
        price,
        if ordered {
            params.decimals_a
        } else {
            params.decimals_b
        },
        if ordered {
            params.decimals_b
        } else {
            params.decimals_a
        },
    )?;
    let fee_tier = fee_tier_pda(&config, tick_spacing);
    let pool = whirlpool_pda(&config, &mint_a, &mint_b, tick_spacing);
    let mut data = Vec::with_capacity(8 + 1 + 2 + 16);
    data.extend_from_slice(&INITIALIZE_POOL);
    data.push(0);
    data.extend_from_slice(&tick_spacing.to_le_bytes());
    data.extend_from_slice(&sqrt.to_le_bytes());
    let ix = Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new_readonly(config, false),
            AccountMeta::new_readonly(mint_a, false),
            AccountMeta::new_readonly(mint_b, false),
            AccountMeta::new(authority, true),
            AccountMeta::new(pool, false),
            AccountMeta::new(vault_a, true),
            AccountMeta::new(vault_b, true),
            AccountMeta::new_readonly(fee_tier, false),
            AccountMeta::new_readonly(token_program(), false),
            AccountMeta::new_readonly(system_program(), false),
            AccountMeta::new_readonly(rent_sysvar(), false),
        ],
        data,
    };
    let mode_note = match params.mode {
        CreatePoolMode::PoolOnly => "pool only".to_string(),
        CreatePoolMode::PoolDeposit | CreatePoolMode::PoolDepositSwap => {
            format!(
                "{} · after land use n create on pool {pool}",
                params.mode.label()
            )
        }
    };
    Ok(WritePlan {
        op: WriteOp::CreatePool,
        venue_label: "Orca Whirlpool".into(),
        summary: format!("create whirlpool · spacing {tick_spacing} · {mode_note}"),
        instructions: vec![ix],
        follow_up: None,
        expected: ExpectedDeltas {
            note: format!("new pool {pool}"),
            ..Default::default()
        },
        payer: authority,
    })
}
