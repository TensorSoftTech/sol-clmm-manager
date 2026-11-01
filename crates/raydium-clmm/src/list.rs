//! List Raydium CLMM positions owned by a wallet (via position NFTs).
//!
//! Fees use claim-sim (owed + Δfee_growth_inside), not stale `fees_owed*` alone.

use super::clmm_program_id;
use super::decode::{
    decode_amm_config_trade_fee, decode_personal_position, decode_pool_header, decode_tick_array,
    pool_stats_from_header, tick_array_pda, tick_array_start_index,
};
use anyhow::Result;
use sol_clmm_manager::clmm_math::{
    amounts_from_liquidity, format_amount, read_token_account_amount,
};
use sol_clmm_manager::fees::collect_fees_quote;
use sol_clmm_manager::rpc::{Rpc, owned_nft_mints};
use sol_clmm_manager::store::Store;
use sol_clmm_manager::tokens::resolve_mints;
use sol_clmm_manager::{PositionRow, Venue};
use solana_pubkey::Pubkey;
use std::collections::{HashMap, HashSet};

pub fn list_positions(
    rpc: &Rpc<'_>,
    wallet: &Pubkey,
    store: Option<&Store>,
    refresh_meta: bool,
) -> Result<Vec<PositionRow>> {
    let mints = owned_nft_mints(rpc, wallet)?;
    if mints.is_empty() {
        return Ok(Vec::new());
    }

    let program = clmm_program_id();
    let mut position_addrs = Vec::with_capacity(mints.len());
    for mint in &mints {
        let (pda, _) = Pubkey::find_program_address(&[b"position", mint.as_ref()], &program);
        position_addrs.push(pda);
    }

    let accounts = rpc.get_multiple_accounts_chunked(&position_addrs)?;
    let mut decoded = Vec::new();
    for (addr, maybe) in position_addrs.iter().zip(accounts) {
        let Some(account) = maybe else { continue };
        if account.owner != program {
            continue;
        }
        match decode_personal_position(&account.data) {
            Ok(pos) => decoded.push((*addr, pos)),
            Err(_) => continue,
        }
    }

    if decoded.is_empty() {
        return Ok(Vec::new());
    }

    let pool_keys: Vec<Pubkey> = decoded
        .iter()
        .map(|(_, p)| p.pool_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let pool_accounts = rpc.get_multiple_accounts_chunked(&pool_keys)?;
    let mut headers = HashMap::new();
    let mut config_keys: HashSet<Pubkey> = HashSet::new();
    let mut token_mints: HashSet<Pubkey> = HashSet::new();
    let mut vault_keys: Vec<Pubkey> = Vec::new();
    for (pk, maybe) in pool_keys.iter().zip(pool_accounts) {
        if let Some(h) = maybe
            .as_ref()
            .and_then(|a| decode_pool_header(&a.data).ok())
        {
            token_mints.insert(h.mint0);
            token_mints.insert(h.mint1);
            config_keys.insert(h.amm_config);
            vault_keys.push(h.vault0);
            vault_keys.push(h.vault1);
            headers.insert(*pk, h);
        }
    }

    let config_list: Vec<Pubkey> = config_keys.into_iter().collect();
    let config_accounts = rpc
        .get_multiple_accounts_chunked(&config_list)
        .unwrap_or_default();
    let mut fee_map: HashMap<Pubkey, u32> = HashMap::new();
    for (pk, maybe) in config_list.iter().zip(config_accounts) {
        if let Some(a) = maybe.as_ref()
            && let Ok(f) = decode_amm_config_trade_fee(&a.data)
        {
            fee_map.insert(*pk, f);
        }
    }

    // Tick arrays for lower/upper claim-sim.
    let mut array_addrs: Vec<Pubkey> = Vec::new();
    let mut seen_arrays: HashSet<Pubkey> = HashSet::new();
    for (_, pos) in &decoded {
        let Some(h) = headers.get(&pos.pool_id) else {
            continue;
        };
        for tick in [pos.tick_lower, pos.tick_upper] {
            let start = tick_array_start_index(tick, h.tick_spacing);
            let pda = tick_array_pda(&pos.pool_id, start, &program);
            if seen_arrays.insert(pda) {
                array_addrs.push(pda);
            }
        }
    }
    let array_accounts = rpc
        .get_multiple_accounts_chunked(&array_addrs)
        .unwrap_or_default();
    let mut tick_outside: HashMap<(Pubkey, i32), (u128, u128)> = HashMap::new();
    // Raydium TickState stores absolute tick index; array PDA is per pool — we need
    // pool association. Re-derive by matching which PDAs we requested per pool.
    let mut pda_to_pool: HashMap<Pubkey, Pubkey> = HashMap::new();
    for (_, pos) in &decoded {
        let Some(h) = headers.get(&pos.pool_id) else {
            continue;
        };
        for tick in [pos.tick_lower, pos.tick_upper] {
            let start = tick_array_start_index(tick, h.tick_spacing);
            let pda = tick_array_pda(&pos.pool_id, start, &program);
            pda_to_pool.insert(pda, pos.pool_id);
        }
    }
    for (pda, maybe) in array_addrs.iter().zip(array_accounts) {
        let Some(pool) = pda_to_pool.get(pda).copied() else {
            continue;
        };
        let Some(account) = maybe else { continue };
        let Ok(arr) = decode_tick_array(&account.data) else {
            continue;
        };
        for t in arr.ticks {
            if t.initialized || t.liquidity_gross > 0 || t.fee_growth_outside_a != 0 {
                tick_outside.insert(
                    (pool, t.index),
                    (t.fee_growth_outside_a, t.fee_growth_outside_b),
                );
            }
        }
    }

    let vault_accounts = rpc
        .get_multiple_accounts_chunked(&vault_keys)
        .unwrap_or_default();
    let mut vault_amt: HashMap<Pubkey, u64> = HashMap::new();
    for (pk, maybe) in vault_keys.iter().zip(vault_accounts) {
        if let Some(a) = maybe.as_ref()
            && let Ok(amt) = read_token_account_amount(&a.data)
        {
            vault_amt.insert(*pk, amt);
        }
    }

    let symbols = resolve_mints(
        rpc,
        &token_mints.into_iter().collect::<Vec<_>>(),
        store,
        refresh_meta,
    )?;
    let sym = |mint: &Pubkey| -> (String, u8) {
        symbols
            .get(mint)
            .map(|t| (t.symbol.clone(), t.decimals))
            .unwrap_or_else(|| (short_mint(mint), 0))
    };

    let mut rows: Vec<PositionRow> = decoded
        .into_iter()
        .map(|(addr, pos)| {
            let header = headers.get(&pos.pool_id);
            let trade_fee = header
                .map(|h| fee_map.get(&h.amm_config).copied().unwrap_or(0))
                .unwrap_or(0);
            let stats = header.map(|h| pool_stats_from_header(pos.pool_id, h, trade_fee));
            let (
                mint_a,
                mint_b,
                tick_now,
                sqrt,
                fee_rate,
                spacing,
                fg_a,
                fg_b,
                proto_a,
                proto_b,
                pool_l,
                va,
                vb,
            ) = match (&header, &stats) {
                (Some(h), Some(s)) => (
                    h.mint0,
                    h.mint1,
                    h.tick_current,
                    h.sqrt_price,
                    s.fee_rate,
                    h.tick_spacing,
                    s.fee_growth_global_a,
                    s.fee_growth_global_b,
                    s.protocol_fee_owed_a,
                    s.protocol_fee_owed_b,
                    s.liquidity,
                    vault_amt.get(&h.vault0).copied().unwrap_or(0),
                    vault_amt.get(&h.vault1).copied().unwrap_or(0),
                ),
                _ => (
                    Pubkey::default(),
                    Pubkey::default(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                ),
            };
            let (sym_a, dec_a) = sym(&mint_a);
            let (sym_b, dec_b) = sym(&mint_b);
            let in_range = tick_now >= pos.tick_lower && tick_now < pos.tick_upper;
            let range_tag = if in_range { "in" } else { "out" };
            let (amount_a, amount_b) = if sqrt != 0 {
                amounts_from_liquidity(pos.liquidity, sqrt, pos.tick_lower, pos.tick_upper)
            } else {
                (0, 0)
            };

            let (lo_a, lo_b) = tick_outside
                .get(&(pos.pool_id, pos.tick_lower))
                .copied()
                .unwrap_or((0, 0));
            let (hi_a, hi_b) = tick_outside
                .get(&(pos.pool_id, pos.tick_upper))
                .copied()
                .unwrap_or((0, 0));
            let fee_a = collect_fees_quote(
                pos.fees_owed0,
                fg_a,
                pos.fee_growth_inside0_last_x64,
                lo_a,
                hi_a,
                pos.tick_lower,
                pos.tick_upper,
                tick_now,
                pos.liquidity,
            );
            let fee_b = collect_fees_quote(
                pos.fees_owed1,
                fg_b,
                pos.fee_growth_inside1_last_x64,
                lo_b,
                hi_b,
                pos.tick_lower,
                pos.tick_upper,
                tick_now,
                pos.liquidity,
            );

            let mut row = PositionRow::new(
                Venue::RaydiumClmm,
                addr,
                pos.pool_id,
                format!("{sym_a} / {sym_b} · {range_tag}"),
                pos.tick_lower,
                pos.tick_upper,
                pos.liquidity,
                format_amount(fee_a as u128, dec_a),
                format_amount(fee_b as u128, dec_b),
            );
            row.mint_a = mint_a;
            row.mint_b = mint_b;
            row.decimals_a = dec_a;
            row.decimals_b = dec_b;
            row.symbol_a = sym_a;
            row.symbol_b = sym_b;
            row.amount_a = amount_a;
            row.amount_b = amount_b;
            row.fee_a = fee_a;
            row.fee_b = fee_b;
            row.sqrt_price = sqrt;
            row.tick_current = tick_now;
            row.tick_spacing = spacing;
            row.fee_rate = fee_rate;
            row.fee_growth_global_a = fg_a;
            row.fee_growth_global_b = fg_b;
            row.protocol_fee_owed_a = proto_a;
            row.protocol_fee_owed_b = proto_b;
            row.pool_liquidity = pool_l;
            row.vault_a = va;
            row.vault_b = vb;
            row
        })
        .collect();

    rows.sort_by(|a, b| {
        a.pool_label
            .cmp(&b.pool_label)
            .then(a.address.to_string().cmp(&b.address.to_string()))
    });
    Ok(rows)
}

fn short_mint(pk: &Pubkey) -> String {
    pk.to_string().chars().take(4).collect()
}
