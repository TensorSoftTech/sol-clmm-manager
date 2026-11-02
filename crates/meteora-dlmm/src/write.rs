//! Meteora DLMM write IXs + strategy-aware deposit planning (feature `write`).

#![cfg(feature = "write")]

use anyhow::Result;
use sol_clmm_manager::{
    BinWeights, CreatePositionParams, DlmmStrategy, ExpectedDeltas, MeteoraStrategyType,
    PositionDetail, SolLegMode, WriteOp, WritePlan, distribute_bins, ensure_pool_atas_ixs,
    maybe_unwrap_after, maybe_wrap_before_deposit,
};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use std::str::FromStr;

use crate::decode::{bin_array_pda, bin_id_to_bin_array_index};
use crate::dlmm_program_id;

const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const SYSTEM_PROGRAM: &str = "11111111111111111111111111111111";
const ASSOCIATED_TOKEN_PROGRAM: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

const CLAIM_FEE: [u8; 8] = [169, 32, 79, 137, 136, 232, 70, 137];
const CLAIM_REWARD: [u8; 8] = [149, 95, 181, 242, 94, 90, 158, 162];
const ADD_LIQUIDITY_BY_STRATEGY2: [u8; 8] = [3, 221, 149, 218, 111, 141, 118, 213];
const REMOVE_LIQUIDITY: [u8; 8] = [80, 85, 209, 72, 24, 206, 177, 108];
const CLOSE_POSITION2: [u8; 8] = [174, 90, 35, 115, 186, 40, 147, 226];
const INITIALIZE_POSITION2: [u8; 8] = [143, 19, 242, 145, 213, 15, 104, 115];

fn program_id() -> Pubkey {
    dlmm_program_id()
}

fn token_program() -> Pubkey {
    Pubkey::from_str(TOKEN_PROGRAM).expect("token")
}

fn system_program() -> Pubkey {
    Pubkey::from_str(SYSTEM_PROGRAM).expect("system")
}

fn associated_token_program() -> Pubkey {
    Pubkey::from_str(ASSOCIATED_TOKEN_PROGRAM).expect("ata")
}

fn ata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), token_program().as_ref(), mint.as_ref()],
        &associated_token_program(),
    )
    .0
}

/// `["__event_authority"]` PDA used by DLMM event CPI.
pub fn event_authority() -> Pubkey {
    Pubkey::find_program_address(&[b"__event_authority"], &program_id()).0
}

/// Optional bitmap extension: program id sentinel = absent.
fn bitmap_extension_or_sentinel() -> Pubkey {
    program_id()
}

fn bin_arrays_for_range(lb_pair: &Pubkey, lower_bin: i32, upper_bin: i32) -> (Pubkey, Pubkey) {
    let prog = program_id();
    let lo = bin_array_pda(lb_pair, bin_id_to_bin_array_index(lower_bin), &prog);
    let hi = bin_array_pda(lb_pair, bin_id_to_bin_array_index(upper_bin), &prog);
    (lo, hi)
}

fn claim_fee_accounts(authority: Pubkey, detail: &PositionDetail) -> Vec<AccountMeta> {
    let (bin_lo, bin_hi) = bin_arrays_for_range(
        &detail.pool.address,
        detail.row.tick_lower,
        detail.row.tick_upper,
    );
    vec![
        AccountMeta::new(detail.pool.address, false),
        AccountMeta::new(detail.row.address, false),
        AccountMeta::new(bin_lo, false),
        AccountMeta::new(bin_hi, false),
        AccountMeta::new_readonly(authority, true),
        AccountMeta::new(detail.pool.token_vault_a, false),
        AccountMeta::new(detail.pool.token_vault_b, false),
        AccountMeta::new(ata(&authority, &detail.token_a.mint), false),
        AccountMeta::new(ata(&authority, &detail.token_b.mint), false),
        AccountMeta::new_readonly(detail.token_a.mint, false),
        AccountMeta::new_readonly(detail.token_b.mint, false),
        AccountMeta::new_readonly(token_program(), false),
        AccountMeta::new_readonly(event_authority(), false),
        AccountMeta::new_readonly(program_id(), false),
    ]
}

pub fn claim_fee_ix(accounts: Vec<AccountMeta>) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts,
        data: CLAIM_FEE.to_vec(),
    }
}

pub fn claim_reward_ix(accounts: Vec<AccountMeta>, reward_index: u64) -> Instruction {
    let mut data = CLAIM_REWARD.to_vec();
    data.extend_from_slice(&reward_index.to_le_bytes());
    Instruction {
        program_id: program_id(),
        accounts,
        data,
    }
}

fn remove_liquidity_accounts(authority: Pubkey, detail: &PositionDetail) -> Vec<AccountMeta> {
    let (bin_lo, bin_hi) = bin_arrays_for_range(
        &detail.pool.address,
        detail.row.tick_lower,
        detail.row.tick_upper,
    );
    let token = token_program();
    vec![
        AccountMeta::new(detail.row.address, false),
        AccountMeta::new(detail.pool.address, false),
        AccountMeta::new(bitmap_extension_or_sentinel(), false),
        AccountMeta::new(ata(&authority, &detail.token_a.mint), false),
        AccountMeta::new(ata(&authority, &detail.token_b.mint), false),
        AccountMeta::new(detail.pool.token_vault_a, false),
        AccountMeta::new(detail.pool.token_vault_b, false),
        AccountMeta::new_readonly(detail.token_a.mint, false),
        AccountMeta::new_readonly(detail.token_b.mint, false),
        AccountMeta::new(bin_lo, false),
        AccountMeta::new(bin_hi, false),
        AccountMeta::new_readonly(authority, true),
        AccountMeta::new_readonly(token, false),
        AccountMeta::new_readonly(token, false),
        AccountMeta::new_readonly(event_authority(), false),
        AccountMeta::new_readonly(program_id(), false),
    ]
}

/// Borsh: `Vec<{ bin_id: i32, bps_to_remove: u16 }>` for each bin in `[lower, upper]`.
/// `bps` is 1..=10_000 (basis points of each bin's liquidity).
fn remove_bins_data(lower: i32, upper: i32, bps: u16) -> Vec<u8> {
    let bps = bps.clamp(1, 10_000);
    let n = (upper - lower + 1).max(0) as u32;
    let mut data = REMOVE_LIQUIDITY.to_vec();
    data.extend_from_slice(&n.to_le_bytes());
    for bin_id in lower..=upper {
        data.extend_from_slice(&bin_id.to_le_bytes());
        data.extend_from_slice(&bps.to_le_bytes());
    }
    data
}

fn remove_all_bins_data(lower: i32, upper: i32) -> Vec<u8> {
    remove_bins_data(lower, upper, 10_000)
}

pub fn remove_liquidity_ix(accounts: Vec<AccountMeta>, data_tail: Vec<u8>) -> Instruction {
    let mut data = REMOVE_LIQUIDITY.to_vec();
    data.extend_from_slice(&data_tail);
    Instruction {
        program_id: program_id(),
        accounts,
        data,
    }
}

fn add_liquidity_accounts(authority: Pubkey, detail: &PositionDetail) -> Vec<AccountMeta> {
    let token = token_program();
    let (bin_lo, bin_hi) = bin_arrays_for_range(
        &detail.pool.address,
        detail.row.tick_lower,
        detail.row.tick_upper,
    );
    let mut accounts = vec![
        AccountMeta::new(detail.row.address, false),
        AccountMeta::new(detail.pool.address, false),
        AccountMeta::new(bitmap_extension_or_sentinel(), false),
        AccountMeta::new(ata(&authority, &detail.token_a.mint), false),
        AccountMeta::new(ata(&authority, &detail.token_b.mint), false),
        AccountMeta::new(detail.pool.token_vault_a, false),
        AccountMeta::new(detail.pool.token_vault_b, false),
        AccountMeta::new_readonly(detail.token_a.mint, false),
        AccountMeta::new_readonly(detail.token_b.mint, false),
        AccountMeta::new_readonly(authority, true),
        AccountMeta::new_readonly(token, false),
        AccountMeta::new_readonly(token, false),
        AccountMeta::new_readonly(event_authority(), false),
        AccountMeta::new_readonly(program_id(), false),
    ];
    // remaining: bin arrays covering the range (ascending)
    let mut idx = bin_id_to_bin_array_index(detail.row.tick_lower);
    let end = bin_id_to_bin_array_index(detail.row.tick_upper);
    let prog = program_id();
    let _ = (bin_lo, bin_hi);
    while idx <= end {
        let pda = bin_array_pda(&detail.pool.address, idx, &prog);
        if !accounts.iter().any(|m| m.pubkey == pda) {
            accounts.push(AccountMeta::new(pda, false));
        }
        idx += 1;
    }
    accounts
}

pub fn add_liquidity_by_strategy2_ix(
    accounts: Vec<AccountMeta>,
    data_tail: Vec<u8>,
) -> Instruction {
    let mut data = ADD_LIQUIDITY_BY_STRATEGY2.to_vec();
    data.extend_from_slice(&data_tail);
    Instruction {
        program_id: program_id(),
        accounts,
        data,
    }
}

/// `add_liquidity_by_strategy2` args:
/// `LiquidityParameterByStrategy` + `RemainingAccountsInfo` (borsh).
fn strategy_params_data(
    amount_x: u64,
    amount_y: u64,
    active_id: i32,
    max_active_bin_slippage: i32,
    strategy_type: MeteoraStrategyType,
    min_bin: i32,
    max_bin: i32,
) -> Vec<u8> {
    // LiquidityParameterByStrategy {
    //   amount_x: u64, amount_y: u64, active_id: i32, max_active_bin_slippage: i32,
    //   strategy_parameters: { min_bin_id: i32, max_bin_id: i32, strategy_type: u8, parameteres: [u8;64] }
    // }
    // RemainingAccountsInfo { slices: Vec } — empty = u32 len 0
    let mut d = Vec::with_capacity(8 + 8 + 4 + 4 + 4 + 4 + 1 + 64 + 4);
    d.extend_from_slice(&amount_x.to_le_bytes());
    d.extend_from_slice(&amount_y.to_le_bytes());
    d.extend_from_slice(&active_id.to_le_bytes());
    d.extend_from_slice(&max_active_bin_slippage.to_le_bytes());
    d.extend_from_slice(&min_bin.to_le_bytes());
    d.extend_from_slice(&max_bin.to_le_bytes());
    d.push(strategy_type.as_u8());
    d.extend_from_slice(&[0u8; 64]);
    d.extend_from_slice(&0u32.to_le_bytes()); // RemainingAccountsInfo.slices = []
    d
}

/// Plan preview weights; range comes from the position (or ±12 bins around active).
pub fn plan_bin_distribution(
    strategy: DlmmStrategy,
    active_id: i32,
    min_bin: Option<i32>,
    max_bin: Option<i32>,
) -> BinWeights {
    const DEFAULT_HALF: i32 = 12;
    let min_b = min_bin.unwrap_or(active_id - DEFAULT_HALF);
    let max_b = max_bin.unwrap_or(active_id + DEFAULT_HALF);
    distribute_bins(strategy, min_b, max_b)
}

pub fn plan_claim(authority: Pubkey, detail: &PositionDetail) -> Result<WritePlan> {
    // WSOL ATA may be missing after a prior unwrap — recreate before payout.
    let mut ixs = ensure_pool_atas_ixs(&authority, &detail.token_a.mint, &detail.token_b.mint);
    ixs.push(claim_fee_ix(claim_fee_accounts(authority, detail)));
    for (idx, mint, vault) in &detail.reward_vaults {
        if *mint == Pubkey::default() || *vault == Pubkey::default() {
            continue;
        }
        // claim_reward accounts (classic layout): lb_pair, position, bin_arrays, sender,
        // reward_vault, reward_mint, user_token, token_program, event_authority, program
        let (bin_lo, bin_hi) = bin_arrays_for_range(
            &detail.pool.address,
            detail.row.tick_lower,
            detail.row.tick_upper,
        );
        ixs.push(claim_reward_ix(
            vec![
                AccountMeta::new(detail.pool.address, false),
                AccountMeta::new(detail.row.address, false),
                AccountMeta::new(bin_lo, false),
                AccountMeta::new(bin_hi, false),
                AccountMeta::new_readonly(authority, true),
                AccountMeta::new(*vault, false),
                AccountMeta::new_readonly(*mint, false),
                AccountMeta::new(ata(&authority, mint), false),
                AccountMeta::new_readonly(token_program(), false),
                AccountMeta::new_readonly(event_authority(), false),
                AccountMeta::new_readonly(program_id(), false),
            ],
            u64::from(*idx),
        ));
    }
    Ok(WritePlan {
        op: WriteOp::ClaimFeesRewards,
        venue_label: "meteora dlmm".into(),
        summary: format!(
            "claim fees ≈ {} + {} (+ {} rewards)",
            detail.fee_a_ui,
            detail.fee_b_ui,
            detail.rewards.len()
        ),
        instructions: ixs,
        follow_up: None,
        expected: ExpectedDeltas {
            fee_a: detail.fee_a,
            fee_b: detail.fee_b,
            note: "claim_fee (+ claim_reward)".into(),
            ..Default::default()
        },
        payer: authority,
    })
}

pub fn plan_deposit_with_strategy(
    authority: Pubkey,
    detail: &PositionDetail,
    strategy: DlmmStrategy,
    amount_x: u64,
    amount_y: u64,
    sol_leg: SolLegMode,
) -> Result<WritePlan> {
    let weights = plan_bin_distribution(
        strategy,
        detail.pool.tick_current_index,
        Some(detail.row.tick_lower),
        Some(detail.row.tick_upper),
    );
    let wrap_amt = if detail.token_a.mint.to_string().starts_with("So111") {
        amount_x
    } else {
        amount_y
    };
    let mut ixs = maybe_wrap_before_deposit(
        sol_leg,
        &authority,
        &detail.token_a.mint,
        &detail.token_b.mint,
        wrap_amt,
    );
    let meteora_ty = strategy.to_meteora();
    let tail = strategy_params_data(
        amount_x,
        amount_y,
        detail.pool.tick_current_index,
        3,
        meteora_ty,
        detail.row.tick_lower,
        detail.row.tick_upper,
    );
    ixs.push(add_liquidity_by_strategy2_ix(
        add_liquidity_accounts(authority, detail),
        tail,
    ));
    ixs.extend(maybe_unwrap_after(
        sol_leg,
        &authority,
        &detail.token_a.mint,
        &detail.token_b.mint,
    ));
    Ok(WritePlan {
        op: WriteOp::Deposit,
        venue_label: "meteora dlmm".into(),
        summary: format!(
            "deposit · {} · {} bins",
            meteora_ty.label(),
            weights.bins.len()
        ),
        instructions: ixs,
        follow_up: None,
        expected: ExpectedDeltas {
            amount_a: -(amount_x as i128),
            amount_b: -(amount_y as i128),
            note: format!(
                "wrap? → {} · add_liquidity_by_strategy2 → unwrap?",
                meteora_ty.label()
            ),
            ..Default::default()
        },
        payer: authority,
    })
}

/// Withdraw `bps`/10_000 of each bin in the position range (1..=10_000).
pub fn plan_withdraw(
    authority: Pubkey,
    detail: &PositionDetail,
    bps: u16,
    sol_leg: SolLegMode,
) -> Result<WritePlan> {
    let bps = bps.clamp(1, 10_000);
    let data = remove_bins_data(detail.row.tick_lower, detail.row.tick_upper, bps);
    // Recreate WSOL ATA if closed by a previous unwrap, then withdraw, then unwrap.
    let mut ixs = ensure_pool_atas_ixs(&authority, &detail.token_a.mint, &detail.token_b.mint);
    ixs.push(Instruction {
        program_id: program_id(),
        accounts: remove_liquidity_accounts(authority, detail),
        data,
    });
    ixs.extend(maybe_unwrap_after(
        sol_leg,
        &authority,
        &detail.token_a.mint,
        &detail.token_b.mint,
    ));
    let pct = bps as f64 / 100.0;
    Ok(WritePlan {
        op: WriteOp::Withdraw,
        venue_label: "meteora dlmm".into(),
        summary: format!(
            "withdraw {pct}% bins [{}, {}] · {bps} bps",
            detail.row.tick_lower, detail.row.tick_upper
        ),
        instructions: ixs,
        follow_up: None,
        expected: ExpectedDeltas {
            note: format!("ensure ATAs → remove_liquidity {bps} bps → unwrap?"),
            ..Default::default()
        },
        payer: authority,
    })
}

pub fn plan_close_and_claim(
    authority: Pubkey,
    detail: &PositionDetail,
    sol_leg: SolLegMode,
) -> Result<WritePlan> {
    // plan_claim already ensures pool ATAs (incl. WSOL).
    let mut ixs = plan_claim(authority, detail)?.instructions;
    ixs.push(Instruction {
        program_id: program_id(),
        accounts: remove_liquidity_accounts(authority, detail),
        data: remove_all_bins_data(detail.row.tick_lower, detail.row.tick_upper),
    });
    // close_position2: position, sender, rent_receiver, event_authority, program
    ixs.push(Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(detail.row.address, false),
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(authority, false),
            AccountMeta::new_readonly(event_authority(), false),
            AccountMeta::new_readonly(program_id(), false),
        ],
        data: CLOSE_POSITION2.to_vec(),
    });
    ixs.extend(maybe_unwrap_after(
        sol_leg,
        &authority,
        &detail.token_a.mint,
        &detail.token_b.mint,
    ));
    Ok(WritePlan {
        op: WriteOp::CloseAndClaim,
        venue_label: "meteora dlmm".into(),
        summary: format!(
            "claim + remove all + close · bins [{}, {}] · {}",
            detail.row.tick_lower,
            detail.row.tick_upper,
            sol_leg.label()
        ),
        instructions: ixs,
        follow_up: None,
        expected: ExpectedDeltas {
            fee_a: detail.fee_a,
            fee_b: detail.fee_b,
            note: "ensure ATAs → claim → remove(all) → close → unwrap? · 1 tx".into(),
            ..Default::default()
        },
        payer: authority,
    })
}

pub fn plan_create_position(authority: Pubkey, params: &CreatePositionParams) -> Result<WritePlan> {
    let strategy = params.dlmm_strategy.unwrap_or_default();
    let weights = plan_bin_distribution(
        strategy,
        (params.tick_lower + params.tick_upper) / 2,
        Some(params.tick_lower),
        Some(params.tick_upper),
    );
    let swap = if params.swap_to_match {
        " · swap-to-match"
    } else {
        ""
    };
    Ok(WritePlan {
        op: WriteOp::CreatePosition,
        venue_label: "meteora dlmm".into(),
        summary: format!(
            "create · {} · pool {}… · bins [{}, {}] · {} · {} bins{} · need position key + reserves",
            params.shape_label(),
            &params.pool.to_string()[..8],
            params.tick_lower,
            params.tick_upper,
            strategy.short(),
            weights.bins.len(),
            swap
        ),
        instructions: Vec::new(),
        follow_up: None,
        expected: ExpectedDeltas {
            amount_a: params.amount_a as i128,
            amount_b: params.amount_b as i128,
            note: "call plan_create_funded with position pubkey + pool reserves".into(),
            ..Default::default()
        },
        payer: authority,
    })
}

/// `initialize_position2` + `add_liquidity_by_strategy2` in one plan.
#[allow(clippy::too_many_arguments)]
pub fn plan_create_funded(
    authority: Pubkey,
    params: &CreatePositionParams,
    position: Pubkey,
    mint_x: Pubkey,
    mint_y: Pubkey,
    reserve_x: Pubkey,
    reserve_y: Pubkey,
    amount_x: u64,
    amount_y: u64,
    // Pool active_id (not range midpoint) — slippage check + strategy.
    active_id: i32,
) -> Result<WritePlan> {
    let strategy = params.dlmm_strategy.unwrap_or_default();
    // Wrap is prepended by Client::assemble (wallet SOL only).
    let mut ixs = Vec::new();

    // initialize_position2 args: lower_bin_id: i32, width: i32
    let width = (params.tick_upper - params.tick_lower + 1).max(1);
    let mut init_data = INITIALIZE_POSITION2.to_vec();
    init_data.extend_from_slice(&params.tick_lower.to_le_bytes());
    init_data.extend_from_slice(&width.to_le_bytes());
    // initialize_position2 accounts (no rent): payer, position, lb_pair, owner,
    // system_program, event_authority, program.
    ixs.push(Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(authority, true), // payer
            AccountMeta::new(position, true),  // position (signer / new key)
            AccountMeta::new_readonly(params.pool, false),
            AccountMeta::new_readonly(authority, true), // owner (signer)
            AccountMeta::new_readonly(system_program(), false),
            AccountMeta::new_readonly(event_authority(), false),
            AccountMeta::new_readonly(program_id(), false),
        ],
        data: init_data,
    });

    let (bin_lo, bin_hi) = bin_arrays_for_range(&params.pool, params.tick_lower, params.tick_upper);
    let token = token_program();
    let mut add_accounts = vec![
        AccountMeta::new(position, false),
        AccountMeta::new(params.pool, false),
        AccountMeta::new(bitmap_extension_or_sentinel(), false),
        AccountMeta::new(ata(&authority, &mint_x), false),
        AccountMeta::new(ata(&authority, &mint_y), false),
        AccountMeta::new(reserve_x, false),
        AccountMeta::new(reserve_y, false),
        AccountMeta::new_readonly(mint_x, false),
        AccountMeta::new_readonly(mint_y, false),
        AccountMeta::new_readonly(authority, true),
        AccountMeta::new_readonly(token, false),
        AccountMeta::new_readonly(token, false),
        AccountMeta::new_readonly(event_authority(), false),
        AccountMeta::new_readonly(program_id(), false),
    ];
    let mut idx = bin_id_to_bin_array_index(params.tick_lower);
    let end = bin_id_to_bin_array_index(params.tick_upper);
    let prog = program_id();
    while idx <= end {
        add_accounts.push(AccountMeta::new(
            bin_array_pda(&params.pool, idx, &prog),
            false,
        ));
        idx += 1;
    }
    let _ = (bin_lo, bin_hi);
    let meteora_ty = strategy.to_meteora();
    let tail = strategy_params_data(
        amount_x,
        amount_y,
        active_id,
        3,
        meteora_ty,
        params.tick_lower,
        params.tick_upper,
    );
    ixs.push(add_liquidity_by_strategy2_ix(add_accounts, tail));

    let swap = if params.swap_to_match {
        " · swap-to-match"
    } else {
        ""
    };
    Ok(WritePlan {
        op: WriteOp::CreatePosition,
        venue_label: "meteora dlmm".into(),
        summary: format!(
            "create · {} · pool {}… · bins [{}, {}] · {}{}",
            params.shape_label(),
            &params.pool.to_string()[..8],
            params.tick_lower,
            params.tick_upper,
            meteora_ty.label(),
            swap
        ),
        instructions: ixs,
        follow_up: None,
        expected: ExpectedDeltas {
            amount_a: -(amount_x as i128),
            amount_b: -(amount_y as i128),
            note: format!(
                "wrap? → initialize_position2 + add_liquidity_by_strategy2 ({}) · 1 tx v1",
                meteora_ty.label()
            ),
            ..Default::default()
        },
        payer: authority,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_authority_matches_meteora() {
        let ea = event_authority();
        assert_eq!(
            ea.to_string(),
            "D1ZN9Wj1fRSUQfCjhvnu1hqDMT7hzjzBBpi12nVniYD6",
            "got {ea}"
        );
    }

    #[test]
    fn strategy2_args_len_matches_idl() {
        // u64+u64+i32+i32 + i32+i32+u8+[u8;64] + RemainingAccountsInfo(u32=0)
        let d = strategy_params_data(
            1,
            2,
            -5900,
            3,
            MeteoraStrategyType::CurveBalanced,
            -5931,
            -5864,
        );
        assert_eq!(d.len(), 8 + 8 + 4 + 4 + 4 + 4 + 1 + 64 + 4);
        assert_eq!(&d[16..20], &(-5900i32).to_le_bytes()); // active_id
        assert_eq!(&d[20..24], &3i32.to_le_bytes()); // max_active_bin_slippage
        assert_eq!(d[32], MeteoraStrategyType::CurveBalanced.as_u8());
        assert_eq!(&d[d.len() - 4..], &0u32.to_le_bytes()); // empty remaining slices
    }
}

/// Create a Meteora DLMM lb-pair (customizable permissionless).
///
/// Account graph (token badges, bitmap extension, fee packing) not fully wired yet.
pub fn plan_create_pool(
    _authority: Pubkey,
    params: &sol_clmm_manager::CreatePoolParams,
) -> anyhow::Result<WritePlan> {
    anyhow::bail!(
        "Meteora create pool not wired yet (bin_step={}, mode={}) · use Orca/Raydium or n create on an existing DLMM pool",
        params.fee_param,
        params.mode.label()
    )
}
