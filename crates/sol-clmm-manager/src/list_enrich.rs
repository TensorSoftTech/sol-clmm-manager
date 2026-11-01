//! Prefetch USD / TVL for list rows + write pool snapshots.

use crate::fees::format_amount_usd;
use crate::pool::{IndexSpace, PoolStats};
use crate::position::PositionRow;
use crate::pricing::{fetch_mint_usd_prices, raw_to_usd, PriceApiKeys, PriceMode};
use crate::rpc::Rpc;
use crate::snapshots::PoolSnapshotInput;
use crate::store::Store;
use crate::tokens::TokenInfo;
use crate::Venue;
use anyhow::Result;
use solana_pubkey::Pubkey;
use std::collections::{HashMap, HashSet};

/// Price list rows, format human+USD strings, snapshot each unique pool.
pub fn enrich_rows(
    rpc: &Rpc<'_>,
    store: Option<&Store>,
    rows: &mut [PositionRow],
    price_mode: PriceMode,
    price_keys: &PriceApiKeys,
) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }

    let mut mint_set: HashSet<Pubkey> = HashSet::new();
    let mut pool_stats: HashMap<Pubkey, PoolStats> = HashMap::new();
    for r in rows.iter() {
        if r.mint_a != Pubkey::default() {
            mint_set.insert(r.mint_a);
        }
        if r.mint_b != Pubkey::default() {
            mint_set.insert(r.mint_b);
        }
        if r.pool != Pubkey::default() {
            pool_stats.entry(r.pool).or_insert_with(|| {
                let index_space = if r.venue == Venue::MeteoraDlmm && r.tick_spacing > 0 {
                    IndexSpace::Bin {
                        bin_step: r.tick_spacing,
                    }
                } else {
                    IndexSpace::Tick
                };
                PoolStats {
                    address: r.pool,
                    token_mint_a: r.mint_a,
                    token_mint_b: r.mint_b,
                    token_vault_a: Pubkey::default(),
                    token_vault_b: Pubkey::default(),
                    tick_spacing: r.tick_spacing,
                    fee_rate: r.fee_rate,
                    liquidity: r.pool_liquidity,
                    sqrt_price: r.sqrt_price,
                    tick_current_index: r.tick_current,
                    index_space,
                    fee_growth_global_a: r.fee_growth_global_a,
                    fee_growth_global_b: r.fee_growth_global_b,
                    protocol_fee_owed_a: r.protocol_fee_owed_a,
                    protocol_fee_owed_b: r.protocol_fee_owed_b,
                }
            });
        }
    }

    let tokens: Vec<TokenInfo> = mint_set
        .into_iter()
        .map(|mint| {
            let (sym, dec) = rows
                .iter()
                .find(|r| r.mint_a == mint)
                .map(|r| (r.symbol_a.clone(), r.decimals_a))
                .or_else(|| {
                    rows.iter()
                        .find(|r| r.mint_b == mint)
                        .map(|r| (r.symbol_b.clone(), r.decimals_b))
                })
                .unwrap_or_else(|| (mint.to_string().chars().take(4).collect(), 0));
            TokenInfo {
                mint,
                symbol: sym,
                decimals: dec,
            }
        })
        .collect();

    let pools: Vec<PoolStats> = pool_stats.values().cloned().collect();
    let prices = fetch_mint_usd_prices(
        rpc,
        price_mode,
        price_keys,
        &tokens,
        &pools,
        Some(rpc.meter()),
    );

    let slot = rpc.get_slot().unwrap_or(0);
    let mut snapshotted: HashSet<Pubkey> = HashSet::new();

    for r in rows.iter_mut() {
        let pa = prices.get(&r.mint_a).cloned();
        let pb = prices.get(&r.mint_b).cloned();
        let a_usd = raw_to_usd(r.amount_a, r.decimals_a, pa.as_ref().and_then(|t| t.price));
        let b_usd = raw_to_usd(r.amount_b, r.decimals_b, pb.as_ref().and_then(|t| t.price));
        let fa_usd = raw_to_usd(
            r.fee_a as u128,
            r.decimals_a,
            pa.as_ref().and_then(|t| t.price),
        );
        let fb_usd = raw_to_usd(
            r.fee_b as u128,
            r.decimals_b,
            pb.as_ref().and_then(|t| t.price),
        );

        r.position_usd = sum_opt(&[a_usd, b_usd, fa_usd, fb_usd]);
        r.fees_usd = sum_opt(&[fa_usd, fb_usd]);

        let vault_a_usd = raw_to_usd(
            r.vault_a as u128,
            r.decimals_a,
            pa.as_ref().and_then(|t| t.price),
        );
        let vault_b_usd = raw_to_usd(
            r.vault_b as u128,
            r.decimals_b,
            pb.as_ref().and_then(|t| t.price),
        );
        r.pool_tvl_usd = sum_opt(&[vault_a_usd, vault_b_usd]);

        let sym_a = if r.symbol_a.is_empty() {
            "A"
        } else {
            &r.symbol_a
        };
        let sym_b = if r.symbol_b.is_empty() {
            "B"
        } else {
            &r.symbol_b
        };

        r.amount_ui = format!(
            "{} + {}",
            format_amount_usd(r.amount_a, r.decimals_a, sym_a, a_usd),
            format_amount_usd(r.amount_b, r.decimals_b, sym_b, b_usd),
        );
        r.fees_ui = format!(
            "{} + {}",
            format_amount_usd(r.fee_a as u128, r.decimals_a, sym_a, fa_usd),
            format_amount_usd(r.fee_b as u128, r.decimals_b, sym_b, fb_usd),
        );
        r.fees_a = format_amount_usd(r.fee_a as u128, r.decimals_a, sym_a, fa_usd);
        r.fees_b = format_amount_usd(r.fee_b as u128, r.decimals_b, sym_b, fb_usd);

        if let Some(store) = store {
            if r.pool != Pubkey::default() && snapshotted.insert(r.pool) {
                let snap = PoolSnapshotInput {
                    venue: r.venue,
                    pool: r.pool,
                    slot,
                    fee_growth_global_a: r.fee_growth_global_a,
                    fee_growth_global_b: r.fee_growth_global_b,
                    protocol_fee_owed_a: r.protocol_fee_owed_a,
                    protocol_fee_owed_b: r.protocol_fee_owed_b,
                    fee_rate: r.fee_rate,
                    liquidity: r.pool_liquidity.max(1),
                    sqrt_price: r.sqrt_price,
                    tick_current: r.tick_current,
                    vault_a: r.vault_a,
                    vault_b: r.vault_b,
                    price_a_usd: pa.as_ref().and_then(|t| t.price),
                    price_b_usd: pb.as_ref().and_then(|t| t.price),
                };
                let _ = store.record_pool_snapshot(
                    &snap,
                    r.decimals_a,
                    r.decimals_b,
                    sym_a,
                    sym_b,
                );
            }
        }
    }

    Ok(())
}

fn sum_opt(parts: &[Option<f64>]) -> Option<f64> {
    let mut s = 0.0;
    let mut any = false;
    for p in parts {
        if let Some(v) = p {
            if v.is_finite() {
                s += v;
                any = true;
            }
        }
    }
    if any {
        Some(s)
    } else {
        None
    }
}
