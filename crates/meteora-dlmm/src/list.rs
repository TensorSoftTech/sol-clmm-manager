//! List Meteora DLMM positions (GPA PositionV2 by owner).
//!
//! Fees use claim-sim (pending + Δbin fee_per_token), not pending alone.

use super::decode::{
    OWNER_OFFSET, POSITION_V2_DISC, POSITION_V2_LEN, bin_array_pda, bin_id_to_bin_array_index,
    claim_fees_from_bins, decode_bin_array, decode_lb_pair_header, decode_position_v2,
};
use super::dlmm_program_id;
use anyhow::Result;
use sol_clmm_manager::clmm_math::{format_amount, read_token_account_amount};
use sol_clmm_manager::rpc::Rpc;
use sol_clmm_manager::store::Store;
use sol_clmm_manager::tokens::resolve_mints;
use sol_clmm_manager::{PositionRow, Venue};
use solana_client::rpc_filter::{Memcmp, RpcFilterType};
use solana_pubkey::Pubkey;
use std::collections::{BTreeMap, HashMap, HashSet};

pub fn list_positions(
    rpc: &Rpc<'_>,
    wallet: &Pubkey,
    store: Option<&Store>,
    refresh_meta: bool,
) -> Result<Vec<PositionRow>> {
    let program = dlmm_program_id();
    let filters = vec![
        RpcFilterType::DataSize(POSITION_V2_LEN),
        RpcFilterType::Memcmp(Memcmp::new_raw_bytes(0, POSITION_V2_DISC.to_vec())),
        RpcFilterType::Memcmp(Memcmp::new_raw_bytes(
            OWNER_OFFSET,
            wallet.to_bytes().to_vec(),
        )),
    ];
    let accounts = rpc.get_program_accounts_with_config(&program, filters)?;

    let mut decoded = Vec::new();
    for (addr, acc) in accounts {
        match decode_position_v2(&acc.data) {
            Ok(pos) => decoded.push((addr, pos)),
            Err(_) => continue,
        }
    }
    if decoded.is_empty() {
        return Ok(Vec::new());
    }

    let pool_keys: Vec<Pubkey> = decoded
        .iter()
        .map(|(_, p)| p.lb_pair)
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
            .and_then(|a| decode_lb_pair_header(&a.data).ok())
        {
            token_mints.insert(h.mint_x);
            token_mints.insert(h.mint_y);
            vault_keys.push(h.reserve_x);
            vault_keys.push(h.reserve_y);
            pools.insert(*pk, h);
        }
    }

    // Bin arrays covering each position range (claim-sim).
    let mut array_addrs: Vec<Pubkey> = Vec::new();
    let mut seen: HashSet<Pubkey> = HashSet::new();
    let mut pda_pool: HashMap<Pubkey, Pubkey> = HashMap::new();
    for (_, pos) in &decoded {
        let mut idx = bin_id_to_bin_array_index(pos.lower_bin);
        let end = bin_id_to_bin_array_index(pos.upper_bin);
        while idx <= end {
            let pda = bin_array_pda(&pos.lb_pair, idx, &program);
            if seen.insert(pda) {
                array_addrs.push(pda);
                pda_pool.insert(pda, pos.lb_pair);
            }
            idx += 1;
        }
    }
    let array_accounts = rpc
        .get_multiple_accounts_chunked(&array_addrs)
        .unwrap_or_default();
    // pool → bins
    let mut bins_by_pool: HashMap<Pubkey, BTreeMap<i32, _>> = HashMap::new();
    for (pda, maybe) in array_addrs.iter().zip(array_accounts) {
        let Some(pool) = pda_pool.get(pda).copied() else {
            continue;
        };
        let Some(account) = maybe else { continue };
        let Ok(arr) = decode_bin_array(&account.data) else {
            continue;
        };
        let entry = bins_by_pool.entry(pool).or_default();
        for b in arr.bins {
            if b.liquidity_supply > 0
                || b.amount_x > 0
                || b.amount_y > 0
                || b.fee_amount_x_per_token_stored != 0
                || b.fee_amount_y_per_token_stored != 0
            {
                entry.insert(b.id, b);
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
            let header = pools.get(&pos.lb_pair);
            let (
                mint_a,
                mint_b,
                active,
                fee_rate,
                bin_step,
                va,
                vb,
                fee_growth_a,
                fee_growth_b,
                pool_l,
            ) = match header {
                Some(h) => {
                    let mut fee = (h.base_factor as u64)
                        .saturating_mul(h.bin_step as u64)
                        .saturating_mul(10);
                    for _ in 0..h.base_fee_power_factor.min(8) {
                        fee = fee.saturating_mul(10);
                    }
                    let bins = bins_by_pool.get(&pos.lb_pair);
                    let mut pool_l: u128 = 0;
                    let mut fga: u128 = 0;
                    let mut fgb: u128 = 0;
                    if let Some(map) = bins {
                        for b in map.values() {
                            pool_l = pool_l.saturating_add(b.liquidity_supply);
                            fga = fga.saturating_add(b.fee_amount_x_per_token_stored);
                            fgb = fgb.saturating_add(b.fee_amount_y_per_token_stored);
                        }
                    }
                    (
                        h.mint_x,
                        h.mint_y,
                        h.active_id,
                        fee.min(u32::MAX as u64) as u32,
                        h.bin_step,
                        vault_amt.get(&h.reserve_x).copied().unwrap_or(0),
                        vault_amt.get(&h.reserve_y).copied().unwrap_or(0),
                        fga,
                        fgb,
                        pool_l.max(1),
                    )
                }
                None => (Pubkey::default(), Pubkey::default(), 0, 0, 0, 0, 0, 0, 0, 1),
            };
            let (sym_a, dec_a) = sym(&mint_a);
            let (sym_b, dec_b) = sym(&mint_b);
            let in_range = active >= pos.lower_bin && active <= pos.upper_bin;
            let range_tag = if in_range { "in" } else { "out" };

            let empty = BTreeMap::new();
            let bins = bins_by_pool.get(&pos.lb_pair).unwrap_or(&empty);
            let (fee_a, fee_b) = if bins.is_empty() {
                (0, 0)
            } else {
                claim_fees_from_bins(&pos, bins)
            };

            let mut row = PositionRow::new(
                Venue::MeteoraDlmm,
                addr,
                pos.lb_pair,
                format!("{sym_a} / {sym_b} · {range_tag}"),
                pos.lower_bin,
                pos.upper_bin,
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
            row.fee_a = fee_a;
            row.fee_b = fee_b;
            row.tick_current = active;
            row.tick_spacing = bin_step;
            row.fee_rate = fee_rate;
            row.fee_growth_global_a = fee_growth_a;
            row.fee_growth_global_b = fee_growth_b;
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
