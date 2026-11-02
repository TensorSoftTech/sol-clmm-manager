//! Raydium CLMM write IXs (feature `write`).

#![cfg(feature = "write")]

use anyhow::Result;
use sol_clmm_manager::{
    CreatePoolParams, CreatePositionParams, ExpectedDeltas, PositionDetail, SolLegMode, WriteOp,
    WritePlan, ensure_pool_atas_ixs, maybe_unwrap_after, maybe_wrap_before_deposit,
};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use std::str::FromStr;

use crate::clmm_program_id;
use crate::decode::{tick_array_pda, tick_array_start_index};

const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const TOKEN_2022_PROGRAM: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
const MEMO_PROGRAM: &str = "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr";
const ASSOCIATED_TOKEN_PROGRAM: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
const SYSTEM_PROGRAM: &str = "11111111111111111111111111111111";
const RENT_SYSVAR: &str = "SysvarRent111111111111111111111111111111111";
const METADATA_PROGRAM: &str = "metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s";

const INCREASE_LIQUIDITY_V2: [u8; 8] = [133, 29, 89, 223, 69, 238, 176, 10];
const DECREASE_LIQUIDITY_V2: [u8; 8] = [58, 127, 188, 62, 79, 82, 196, 96];
const CLOSE_POSITION: [u8; 8] = [123, 134, 81, 0, 49, 68, 98, 98];
const OPEN_POSITION_WITH_TOKEN22_NFT: [u8; 8] = [77, 255, 174, 82, 125, 29, 201, 46];

fn program_id() -> Pubkey {
    clmm_program_id()
}

fn token_program() -> Pubkey {
    Pubkey::from_str(TOKEN_PROGRAM).expect("token")
}

fn token_2022_program() -> Pubkey {
    Pubkey::from_str(TOKEN_2022_PROGRAM).expect("token2022")
}

fn memo_program() -> Pubkey {
    Pubkey::from_str(MEMO_PROGRAM).expect("memo")
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

fn metadata_program() -> Pubkey {
    Pubkey::from_str(METADATA_PROGRAM).expect("metadata")
}

fn ata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    ata_with_token_program(owner, mint, &token_program())
}

fn ata_token_2022(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    ata_with_token_program(owner, mint, &token_2022_program())
}

fn ata_with_token_program(owner: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), token_program.as_ref(), mint.as_ref()],
        &associated_token_program(),
    )
    .0
}

/// NFT ATA for an existing position — Token or Token-2022 from mint owner.
fn nft_ata(owner: &Pubkey, mint: &Pubkey, detail: &PositionDetail) -> Pubkey {
    let prog = detail
        .position_nft_token_program
        .unwrap_or_else(token_2022_program);
    ata_with_token_program(owner, mint, &prog)
}

fn nft_token_program(detail: &PositionDetail) -> Pubkey {
    detail
        .position_nft_token_program
        .unwrap_or_else(token_2022_program)
}

fn personal_position_pda(nft_mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"position", nft_mint.as_ref()], &program_id()).0
}

fn tick_array_bitmap_extension_pda(pool: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"pool_tick_array_bitmap_extension", pool.as_ref()],
        &program_id(),
    )
    .0
}

fn protocol_position_pda(pool: &Pubkey, tick_lower: i32, tick_upper: i32) -> Pubkey {
    Pubkey::find_program_address(
        &[
            b"position",
            pool.as_ref(),
            &tick_lower.to_be_bytes(),
            &tick_upper.to_be_bytes(),
        ],
        &program_id(),
    )
    .0
}

fn tick_arrays(pool: &Pubkey, tick_lower: i32, tick_upper: i32, spacing: u16) -> (Pubkey, Pubkey) {
    let prog = program_id();
    (
        tick_array_pda(pool, tick_array_start_index(tick_lower, spacing), &prog),
        tick_array_pda(pool, tick_array_start_index(tick_upper, spacing), &prog),
    )
}

fn require_mint(detail: &PositionDetail) -> Result<Pubkey> {
    detail
        .position_mint
        .filter(|m| *m != Pubkey::default())
        .ok_or_else(|| anyhow::anyhow!("raydium position nft mint missing from detail"))
}

fn decrease_accounts(
    nft_owner: Pubkey,
    nft_mint: Pubkey,
    detail: &PositionDetail,
) -> Vec<AccountMeta> {
    let nft_account = nft_ata(&nft_owner, &nft_mint, detail);
    let personal = detail.row.address;
    let (tick_lo, tick_hi) = tick_arrays(
        &detail.pool.address,
        detail.row.tick_lower,
        detail.row.tick_upper,
        detail.pool.tick_spacing,
    );
    let protocol = protocol_position_pda(
        &detail.pool.address,
        detail.row.tick_lower,
        detail.row.tick_upper,
    );
    vec![
        AccountMeta::new_readonly(nft_owner, true),
        AccountMeta::new_readonly(nft_account, false),
        AccountMeta::new(personal, false),
        AccountMeta::new(detail.pool.address, false),
        AccountMeta::new_readonly(protocol, false),
        AccountMeta::new(detail.pool.token_vault_a, false),
        AccountMeta::new(detail.pool.token_vault_b, false),
        AccountMeta::new(tick_lo, false),
        AccountMeta::new(tick_hi, false),
        AccountMeta::new(ata(&nft_owner, &detail.token_a.mint), false),
        AccountMeta::new(ata(&nft_owner, &detail.token_b.mint), false),
        AccountMeta::new_readonly(token_program(), false),
        AccountMeta::new_readonly(token_2022_program(), false),
        AccountMeta::new_readonly(memo_program(), false),
        AccountMeta::new_readonly(detail.token_a.mint, false),
        AccountMeta::new_readonly(detail.token_b.mint, false),
        // remaining[0]: bitmap extension (required when range is outside inline bitmap)
        AccountMeta::new(tick_array_bitmap_extension_pda(&detail.pool.address), false),
    ]
}

fn increase_accounts(
    nft_owner: Pubkey,
    nft_mint: Pubkey,
    detail: &PositionDetail,
) -> Vec<AccountMeta> {
    let nft_account = nft_ata(&nft_owner, &nft_mint, detail);
    let (tick_lo, tick_hi) = tick_arrays(
        &detail.pool.address,
        detail.row.tick_lower,
        detail.row.tick_upper,
        detail.pool.tick_spacing,
    );
    let protocol = protocol_position_pda(
        &detail.pool.address,
        detail.row.tick_lower,
        detail.row.tick_upper,
    );
    vec![
        AccountMeta::new_readonly(nft_owner, true),
        AccountMeta::new_readonly(nft_account, false),
        AccountMeta::new(detail.pool.address, false),
        AccountMeta::new_readonly(protocol, false),
        AccountMeta::new(detail.row.address, false),
        AccountMeta::new(tick_lo, false),
        AccountMeta::new(tick_hi, false),
        AccountMeta::new(ata(&nft_owner, &detail.token_a.mint), false),
        AccountMeta::new(ata(&nft_owner, &detail.token_b.mint), false),
        AccountMeta::new(detail.pool.token_vault_a, false),
        AccountMeta::new(detail.pool.token_vault_b, false),
        AccountMeta::new_readonly(token_program(), false),
        AccountMeta::new_readonly(token_2022_program(), false),
        AccountMeta::new_readonly(detail.token_a.mint, false),
        AccountMeta::new_readonly(detail.token_b.mint, false),
        AccountMeta::new(tick_array_bitmap_extension_pda(&detail.pool.address), false),
    ]
}

fn decrease_data(liquidity: u128, min_a: u64, min_b: u64) -> Vec<u8> {
    let mut d = DECREASE_LIQUIDITY_V2.to_vec();
    d.extend_from_slice(&liquidity.to_le_bytes());
    d.extend_from_slice(&min_a.to_le_bytes());
    d.extend_from_slice(&min_b.to_le_bytes());
    d
}

fn increase_data(liquidity: u128, max_a: u64, max_b: u64) -> Vec<u8> {
    let mut d = INCREASE_LIQUIDITY_V2.to_vec();
    d.extend_from_slice(&liquidity.to_le_bytes());
    d.extend_from_slice(&max_a.to_le_bytes());
    d.extend_from_slice(&max_b.to_le_bytes());
    d.push(0); // Option<bool> base_flag = None
    d
}

pub fn decrease_liquidity_v2(
    accounts: Vec<AccountMeta>,
    liquidity: u128,
    min_a: u64,
    min_b: u64,
) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts,
        data: decrease_data(liquidity, min_a, min_b),
    }
}

pub fn increase_liquidity_v2(
    accounts: Vec<AccountMeta>,
    liquidity: u128,
    max_a: u64,
    max_b: u64,
) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts,
        data: increase_data(liquidity, max_a, max_b),
    }
}

pub fn close_position_ix(
    nft_owner: Pubkey,
    nft_mint: Pubkey,
    personal_position: Pubkey,
    pool: Pubkey,
    nft_token_program: Pubkey,
) -> Instruction {
    let nft_account = ata_with_token_program(&nft_owner, &nft_mint, &nft_token_program);
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(nft_owner, true),
            AccountMeta::new(nft_mint, false),
            AccountMeta::new(nft_account, false),
            AccountMeta::new(personal_position, false),
            AccountMeta::new_readonly(system_program(), false),
            // TokenInterface — Token or Token-2022 matching the NFT mint owner.
            AccountMeta::new_readonly(nft_token_program, false),
            // remaining: pool for frozen-NFT thaw
            AccountMeta::new_readonly(pool, false),
        ],
        data: CLOSE_POSITION.to_vec(),
    }
}

/// Raydium collects fees/rewards via decrease (L=0 still pays out owed).
pub fn plan_claim(authority: Pubkey, detail: &PositionDetail) -> Result<WritePlan> {
    let mint = require_mint(detail)?;
    // WSOL ATA may be missing after a prior unwrap — recreate before payout.
    let mut ixs = ensure_pool_atas_ixs(&authority, &detail.token_a.mint, &detail.token_b.mint);
    ixs.push(decrease_liquidity_v2(
        decrease_accounts(authority, mint, detail),
        0,
        0,
        0,
    ));
    Ok(WritePlan {
        op: WriteOp::ClaimFeesRewards,
        venue_label: "raydium clmm".into(),
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
            note: "ensure ATAs → decrease_liquidity_v2(L=0) collects fees+rewards".into(),
            ..Default::default()
        },
        payer: authority,
    })
}

pub fn plan_deposit(
    authority: Pubkey,
    detail: &PositionDetail,
    liquidity: u128,
    max_a: u64,
    max_b: u64,
    sol_leg: SolLegMode,
) -> Result<WritePlan> {
    anyhow::ensure!(liquidity > 0, "liquidity delta is 0");
    let mint = require_mint(detail)?;
    let wrap_amt = if detail.token_a.mint.to_string().starts_with("So111") {
        max_a
    } else {
        max_b
    };
    let mut ixs = maybe_wrap_before_deposit(
        sol_leg,
        &authority,
        &detail.token_a.mint,
        &detail.token_b.mint,
        wrap_amt,
    );
    ixs.push(increase_liquidity_v2(
        increase_accounts(authority, mint, detail),
        liquidity,
        max_a,
        max_b,
    ));
    ixs.extend(maybe_unwrap_after(
        sol_leg,
        &authority,
        &detail.token_a.mint,
        &detail.token_b.mint,
    ));
    Ok(WritePlan {
        op: WriteOp::Deposit,
        venue_label: "raydium clmm".into(),
        summary: format!("deposit ΔL {liquidity}"),
        instructions: ixs,
        follow_up: None,
        expected: ExpectedDeltas {
            amount_a: -(max_a as i128),
            amount_b: -(max_b as i128),
            note: "wrap? → increase_liquidity_v2 → unwrap?".into(),
            ..Default::default()
        },
        payer: authority,
    })
}

pub fn plan_withdraw(
    authority: Pubkey,
    detail: &PositionDetail,
    liquidity_delta: u128,
    sol_leg: SolLegMode,
) -> Result<WritePlan> {
    anyhow::ensure!(liquidity_delta > 0, "liquidity delta is 0");
    let mint = require_mint(detail)?;
    // Recreate WSOL ATA if closed by a previous unwrap, then withdraw, then unwrap.
    let mut ixs = ensure_pool_atas_ixs(&authority, &detail.token_a.mint, &detail.token_b.mint);
    ixs.push(decrease_liquidity_v2(
        decrease_accounts(authority, mint, detail),
        liquidity_delta,
        0,
        0,
    ));
    ixs.extend(maybe_unwrap_after(
        sol_leg,
        &authority,
        &detail.token_a.mint,
        &detail.token_b.mint,
    ));
    Ok(WritePlan {
        op: WriteOp::Withdraw,
        venue_label: "raydium clmm".into(),
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

pub fn plan_withdraw_stub(
    authority: Pubkey,
    detail: &PositionDetail,
    liquidity_delta: u128,
) -> Result<WritePlan> {
    plan_withdraw(authority, detail, liquidity_delta, SolLegMode::Wsol)
}

pub fn plan_close_and_claim(
    authority: Pubkey,
    detail: &PositionDetail,
    sol_leg: SolLegMode,
) -> Result<WritePlan> {
    let mint = require_mint(detail)?;
    let mut ixs = ensure_pool_atas_ixs(&authority, &detail.token_a.mint, &detail.token_b.mint);
    ixs.push(decrease_liquidity_v2(
        decrease_accounts(authority, mint, detail),
        detail.position_liquidity,
        0,
        0,
    ));
    ixs.push(close_position_ix(
        authority,
        mint,
        detail.row.address,
        detail.pool.address,
        nft_token_program(detail),
    ));
    ixs.extend(maybe_unwrap_after(
        sol_leg,
        &authority,
        &detail.token_a.mint,
        &detail.token_b.mint,
    ));
    Ok(WritePlan {
        op: WriteOp::CloseAndClaim,
        venue_label: "raydium clmm".into(),
        summary: format!(
            "claim + withdraw all + close · L={} · {}",
            detail.position_liquidity,
            sol_leg.label()
        ),
        instructions: ixs,
        follow_up: None,
        expected: ExpectedDeltas {
            fee_a: detail.fee_a,
            fee_b: detail.fee_b,
            note: "ensure ATAs → decrease(all) → close → unwrap? · 1 tx".into(),
            ..Default::default()
        },
        payer: authority,
    })
}

pub fn plan_create_position(authority: Pubkey, params: &CreatePositionParams) -> Result<WritePlan> {
    let swap = if params.swap_to_match {
        " · swap-to-match"
    } else {
        ""
    };
    Ok(WritePlan {
        op: WriteOp::CreatePosition,
        venue_label: "raydium clmm".into(),
        summary: format!(
            "create · {} · pool {}… · ticks [{}, {}]{} · need mint+pool fetch for metas",
            params.shape_label(),
            &params.pool.to_string()[..8],
            params.tick_lower,
            params.tick_upper,
            swap
        ),
        instructions: Vec::new(),
        follow_up: None,
        expected: ExpectedDeltas {
            amount_a: params.amount_a as i128,
            amount_b: params.amount_b as i128,
            note: "call plan_create_funded with nft mint + pool vaults".into(),
            ..Default::default()
        },
        payer: authority,
    })
}

/// OpenPositionWithToken22Nft account list (20 accounts) + increase args in data.
#[allow(clippy::too_many_arguments)]
pub fn plan_create_funded(
    authority: Pubkey,
    params: &CreatePositionParams,
    position_nft_mint: Pubkey,
    mint_a: Pubkey,
    mint_b: Pubkey,
    vault_a: Pubkey,
    vault_b: Pubkey,
    tick_spacing: u16,
    liquidity: u128,
    amount_0_max: u64,
    amount_1_max: u64,
) -> Result<WritePlan> {
    let personal = personal_position_pda(&position_nft_mint);
    // Our create path is OpenPositionWithToken22Nft → ATA must be Token-2022.
    let nft_account = ata_token_2022(&authority, &position_nft_mint);
    let lower_start = tick_array_start_index(params.tick_lower, tick_spacing);
    let upper_start = tick_array_start_index(params.tick_upper, tick_spacing);
    let prog = program_id();
    let tick_lo = tick_array_pda(&params.pool, lower_start, &prog);
    let tick_hi = tick_array_pda(&params.pool, upper_start, &prog);
    let protocol = protocol_position_pda(&params.pool, params.tick_lower, params.tick_upper);
    let bitmap_ext = tick_array_bitmap_extension_pda(&params.pool);

    let mut data = OPEN_POSITION_WITH_TOKEN22_NFT.to_vec();
    data.extend_from_slice(&params.tick_lower.to_le_bytes());
    data.extend_from_slice(&params.tick_upper.to_le_bytes());
    data.extend_from_slice(&lower_start.to_le_bytes());
    data.extend_from_slice(&upper_start.to_le_bytes());
    data.extend_from_slice(&liquidity.to_le_bytes());
    data.extend_from_slice(&amount_0_max.to_le_bytes());
    data.extend_from_slice(&amount_1_max.to_le_bytes());
    data.push(0); // with_metadata = false (ignored for token22 path)
    data.push(0); // base_flag = None

    // Wrap is prepended by Client::assemble (wallet SOL only).
    let ixs = vec![Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(authority, true),
            AccountMeta::new_readonly(authority, false),
            AccountMeta::new(position_nft_mint, true),
            AccountMeta::new(nft_account, false),
            AccountMeta::new(params.pool, false),
            AccountMeta::new_readonly(protocol, false),
            AccountMeta::new(tick_lo, false),
            AccountMeta::new(tick_hi, false),
            AccountMeta::new(personal, false),
            AccountMeta::new(ata(&authority, &mint_a), false),
            AccountMeta::new(ata(&authority, &mint_b), false),
            AccountMeta::new(vault_a, false),
            AccountMeta::new(vault_b, false),
            AccountMeta::new_readonly(rent_sysvar(), false),
            AccountMeta::new_readonly(system_program(), false),
            AccountMeta::new_readonly(token_program(), false),
            AccountMeta::new_readonly(associated_token_program(), false),
            AccountMeta::new_readonly(token_2022_program(), false),
            AccountMeta::new_readonly(mint_a, false),
            AccountMeta::new_readonly(mint_b, false),
            // remaining[0]: TickArrayBitmapExtension (needed outside ±512 arrays)
            AccountMeta::new(bitmap_ext, false),
        ],
        data,
    }];
    // silence unused metadata helper (OpenPositionV2 path)
    let _ = metadata_program();

    let swap = if params.swap_to_match {
        " · swap-to-match"
    } else {
        ""
    };
    Ok(WritePlan {
        op: WriteOp::CreatePosition,
        venue_label: "raydium clmm".into(),
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
            amount_a: -(amount_0_max as i128),
            amount_b: -(amount_1_max as i128),
            note: "wrap? → open_position_with_token22_nft · 1 tx v1".into(),
            ..Default::default()
        },
        payer: authority,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nft_ata_uses_token_2022_not_legacy() {
        let owner = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let legacy = ata(&owner, &mint);
        let t22 = ata_token_2022(&owner, &mint);
        assert_ne!(legacy, t22, "Token-2022 ATA must differ from Tokenkeg ATA");
    }

    #[test]
    fn bitmap_extension_pda_for_known_pool() {
        let pool = Pubkey::from_str("3ucNos4NbumPLZNWztqGHNFFgkHeRMBQAVemeeomsUxv").unwrap();
        let pda = tick_array_bitmap_extension_pda(&pool);
        assert_eq!(
            pda.to_string(),
            "4NFvUKqknMpoe6CWTzK758B8ojVLzURL5pC6MtiaJ8TQ"
        );
    }
}

const CREATE_POOL: [u8; 8] = [233, 146, 209, 142, 207, 104, 64, 188];

fn amm_config_pda(index: u16) -> Pubkey {
    Pubkey::find_program_address(&[b"amm_config", &index.to_be_bytes()], &program_id()).0
}

fn pool_state_pda(amm_config: &Pubkey, mint0: &Pubkey, mint1: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"pool", amm_config.as_ref(), mint0.as_ref(), mint1.as_ref()],
        &program_id(),
    )
    .0
}

fn observation_pda(pool: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"observation", pool.as_ref()], &program_id()).0
}

fn tick_bitmap_ext_pda(pool: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"pool_tick_array_bitmap_extension", pool.as_ref()],
        &program_id(),
    )
    .0
}

fn vault_pda(pool: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"pool_vault", pool.as_ref(), mint.as_ref()],
        &program_id(),
    )
    .0
}

fn price_to_sqrt_x64(human_b_per_a: f64, dec_a: u8, dec_b: u8) -> anyhow::Result<u128> {
    anyhow::ensure!(
        human_b_per_a.is_finite() && human_b_per_a > 0.0,
        "initial price must be > 0"
    );
    let raw = human_b_per_a * 10f64.powi(i32::from(dec_b) - i32::from(dec_a));
    let q = raw.sqrt() * ((1u128 << 64) as f64);
    anyhow::ensure!(
        q.is_finite() && q > 0.0 && q < u128::MAX as f64,
        "sqrt_price overflow"
    );
    Ok(q as u128)
}

fn mint_token_program(mint: &Pubkey) -> Pubkey {
    // Prefer Tokenkeg; Token-2022 mints still work if owner matches at runtime.
    let _ = mint;
    token_program()
}

/// Initialize a Raydium CLMM pool (pool-only). `fee_param` = amm_config index.
pub fn plan_create_pool(authority: Pubkey, params: &CreatePoolParams) -> anyhow::Result<WritePlan> {
    use sol_clmm_manager::CreatePoolMode;
    anyhow::ensure!(params.mint_a != params.mint_b, "mints must differ");
    // Raydium requires mint_0 < mint_1.
    let (mint0, mint1, ordered) = if params.mint_a.to_bytes() < params.mint_b.to_bytes() {
        (params.mint_a, params.mint_b, true)
    } else {
        (params.mint_b, params.mint_a, false)
    };
    let sqrt = price_to_sqrt_x64(
        if ordered {
            params.initial_price
        } else {
            1.0 / params.initial_price
        },
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
    let open_time = 0u64;
    let amm_config = amm_config_pda(params.fee_param);
    let pool = pool_state_pda(&amm_config, &mint0, &mint1);
    let mut data = Vec::with_capacity(8 + 16 + 8);
    data.extend_from_slice(&CREATE_POOL);
    data.extend_from_slice(&sqrt.to_le_bytes());
    data.extend_from_slice(&open_time.to_le_bytes());
    let ix = Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(authority, true),
            AccountMeta::new_readonly(amm_config, false),
            AccountMeta::new(pool, false),
            AccountMeta::new_readonly(mint0, false),
            AccountMeta::new_readonly(mint1, false),
            AccountMeta::new(vault_pda(&pool, &mint0), false),
            AccountMeta::new(vault_pda(&pool, &mint1), false),
            AccountMeta::new(observation_pda(&pool), false),
            AccountMeta::new(tick_bitmap_ext_pda(&pool), false),
            AccountMeta::new_readonly(mint_token_program(&mint0), false),
            AccountMeta::new_readonly(mint_token_program(&mint1), false),
            AccountMeta::new_readonly(system_program(), false),
        ],
        data,
    };
    let mode_note = match params.mode {
        CreatePoolMode::PoolOnly => "pool only".into(),
        _ => format!(
            "{} · after land use n create on {pool}",
            params.mode.label()
        ),
    };
    Ok(WritePlan {
        op: WriteOp::CreatePool,
        venue_label: "Raydium CLMM".into(),
        summary: format!(
            "create clmm pool · config {} · {mode_note}",
            params.fee_param
        ),
        instructions: vec![ix],
        follow_up: None,
        expected: ExpectedDeltas {
            note: format!("new pool {pool}"),
            ..Default::default()
        },
        payer: authority,
    })
}
