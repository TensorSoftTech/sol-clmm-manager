//! List Whirlpool positions owned by a wallet (via position NFTs).
//!
//! Fees use claim-sim (owed + Δfee_growth_inside), not stale `fee_owed_*` alone.

use super::decode::{
    decode_position, decode_tick_array, decode_whirlpool_stats, tick_array_pda,
    tick_array_start_index,
};
use super::whirlpool_program_id;
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

    let program = whirlpool_program_id();
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
        match decode_position(&account.data) {
            Ok(pos) => decoded.push((*addr, pos)),
            Err(_) => continue,
        }
    }

    if decoded.is_empty() {
        return Ok(Vec::new());
    }

    let pool_keys: Vec<Pubkey> = decoded
        .iter()
        .map(|(_, p)| p.whirlpool)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let pool_accounts = rpc.get_multiple_accounts_chunked(&pool_keys)?;
    let mut pools = HashMap::new();
    let mut token_mints: HashSet<Pubkey> = HashSet::new();
    let mut vault_keys: Vec<Pubkey> = Vec::new();
    for (pk, maybe) in pool_keys.iter().zip(pool_accounts) {
        if let Some(h) = maybe
            .as_ref()
            .and_then(|a| decode_whirlpool_stats(*pk, &a.data).ok())
        {
            token_mints.insert(h.token_mint_a);
            token_mints.insert(h.token_mint_b);
            vault_keys.push(h.token_vault_a);
            vault_keys.push(h.token_vault_b);
            pools.insert(*pk, h);
        }
    }

    // Tick arrays for every position lower/upper (claim-sim).
    let mut array_addrs: Vec<Pubkey> = Vec::new();
    let mut array_meta: Vec<(Pubkey, i32, u16)> = Vec::new(); // pool, start, spacing
    let mut seen_arrays: HashSet<Pubkey> = HashSet::new();
    for (_, pos) in &decoded {
        let Some(pool) = pools.get(&pos.whirlpool) else {
            continue;
        };
        let spacing = pool.tick_spacing;
        for tick in [pos.tick_lower_index, pos.tick_upper_index] {
            let start = tick_array_start_index(tick, spacing);
            let pda = tick_array_pda(&pos.whirlpool, start, &program);
            if seen_arrays.insert(pda) {
                array_addrs.push(pda);
                array_meta.push((pos.whirlpool, start, spacing));
            }
        }
    }
    let array_accounts = rpc
        .get_multiple_accounts_chunked(&array_addrs)
        .unwrap_or_default();
    // (pool, tick) → (fee_growth_outside_a, fee_growth_outside_b)
    let mut tick_outside: HashMap<(Pubkey, i32), (u128, u128)> = HashMap::new();
    for ((pool, start, spacing), maybe) in array_meta.into_iter().zip(array_accounts) {
        let Some(account) = maybe else { continue };
        let Ok(arr) = decode_tick_array(&account.data) else {
            continue;
        };
        for (i, t) in arr.ticks.into_iter().enumerate() {
            let index = start + (i as i32) * spacing as i32;
            if t.initialized || t.liquidity_gross > 0 || t.fee_growth_outside_a != 0 {
                tick_outside.insert(
                    (pool, index),
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
            let pool = pools.get(&pos.whirlpool);
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
            ) = match pool {
                Some(h) => (
                    h.token_mint_a,
                    h.token_mint_b,
                    h.tick_current_index,
                    h.sqrt_price,
                    h.fee_rate,
                    h.tick_spacing,
                    h.fee_growth_global_a,
                    h.fee_growth_global_b,
                    h.protocol_fee_owed_a,
                    h.protocol_fee_owed_b,
                    h.liquidity,
                    vault_amt.get(&h.token_vault_a).copied().unwrap_or(0),
                    vault_amt.get(&h.token_vault_b).copied().unwrap_or(0),
                ),
                None => (
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
            let in_range = tick_now >= pos.tick_lower_index && tick_now < pos.tick_upper_index;
            let range_tag = if in_range { "in" } else { "out" };
            let (amount_a, amount_b) = if sqrt != 0 {
                amounts_from_liquidity(
                    pos.liquidity,
                    sqrt,
                    pos.tick_lower_index,
                    pos.tick_upper_index,
                )
            } else {
                (0, 0)
            };

            // Uninitialized / missing tick ⇒ outside growth 0 (same as on-chain default).
            let (lo_a, lo_b) = tick_outside
                .get(&(pos.whirlpool, pos.tick_lower_index))
                .copied()
                .unwrap_or((0, 0));
            let (hi_a, hi_b) = tick_outside
                .get(&(pos.whirlpool, pos.tick_upper_index))
                .copied()
                .unwrap_or((0, 0));
            let fee_a = collect_fees_quote(
                pos.fee_owed_a,
                fg_a,
                pos.fee_growth_checkpoint_a,
                lo_a,
                hi_a,
                pos.tick_lower_index,
                pos.tick_upper_index,
                tick_now,
                pos.liquidity,
            );
            let fee_b = collect_fees_quote(
                pos.fee_owed_b,
                fg_b,
                pos.fee_growth_checkpoint_b,
                lo_b,
                hi_b,
                pos.tick_lower_index,
                pos.tick_upper_index,
                tick_now,
                pos.liquidity,
            );

            let mut row = PositionRow::new(
                Venue::OrcaWhirlpool,
                addr,
                pos.whirlpool,
                format!("{sym_a} / {sym_b} · {range_tag}"),
                pos.tick_lower_index,
                pos.tick_upper_index,
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
        a.venue
            .cmp(&b.venue)
            .then(a.pool_label.cmp(&b.pool_label))
            .then(a.address.to_string().cmp(&b.address.to_string()))
    });
    Ok(rows)
}

fn short_mint(pk: &Pubkey) -> String {
    let s = pk.to_string();
    s.chars().take(4).collect()
}
