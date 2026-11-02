//! Meteora DLMM detail: claim-sim fees · amounts · chart · snapshot volume.

use super::decode::{
    amounts_from_shares, bin_array_pda, bin_id_to_bin_array_index, build_bin_series,
    claim_fees_from_bins, decode_bin_array, decode_lb_pair_header, decode_position_v2,
    pool_stats_from_lb_pair,
};
use super::dlmm_program_id;
use anyhow::{Context, Result};
use sol_clmm_manager::Venue;
use sol_clmm_manager::clmm_math::read_token_account_amount;
use sol_clmm_manager::detail::{PoolPreview, PositionDetail, RewardInfo};
use sol_clmm_manager::fees::format_amount_usd;
use sol_clmm_manager::position::PositionRow;
use sol_clmm_manager::pricing::{
    PriceApiKeys, PriceMode, estimate_position_usd_with_extra_mints, fetch_mint_usd_prices,
    raw_to_usd,
};
use sol_clmm_manager::rpc::Rpc;
use sol_clmm_manager::snapshots::PoolSnapshotInput;
use sol_clmm_manager::store::Store;
use sol_clmm_manager::tokens::{resolve_mints, resolve_tokens, resolve_tokens_with_keys};
use sol_clmm_manager::wallet::fetch_wallet_pool_balances;
use solana_pubkey::Pubkey;
use std::collections::{BTreeMap, BTreeSet};

pub fn fetch_position_detail(
    rpc: &Rpc<'_>,
    row: &PositionRow,
    store: Option<&Store>,
    price_mode: PriceMode,
    price_keys: &PriceApiKeys,
    refresh_meta: bool,
) -> Result<PositionDetail> {
    let program = dlmm_program_id();

    let pos_acc = rpc
        .get_account(&row.address)
        .context("get position account")?;
    let position = decode_position_v2(&pos_acc.data)?;

    let pool_acc = rpc.get_account(&row.pool).context("get lb pair account")?;
    let header = decode_lb_pair_header(&pool_acc.data)?;

    // Wide chart context around active (same ±CHART_FETCH as Orca/Raydium).
    let (wide_lo, wide_hi) = PoolPreview::default_range(
        header.active_id,
        1,
        sol_clmm_manager::CHART_FETCH_HALF_SPACINGS,
    );
    let fetch_lo = wide_lo.min(position.lower_bin);
    let fetch_hi = wide_hi.max(position.upper_bin);
    let indices = needed_bin_array_indices(fetch_lo, fetch_hi, header.active_id);
    let addrs: Vec<Pubkey> = indices
        .iter()
        .map(|i| bin_array_pda(&row.pool, *i, &program))
        .collect();
    let accounts = rpc
        .get_multiple_accounts(&addrs)
        .context("get bin arrays")?;

    let mut bins: BTreeMap<i32, _> = BTreeMap::new();
    let mut arrays_loaded = 0usize;
    let mut pool_liquidity: u128 = 0;
    let mut fee_growth_a: u128 = 0;
    let mut fee_growth_b: u128 = 0;
    for maybe in accounts {
        let Some(account) = maybe else { continue };
        let Ok(arr) = decode_bin_array(&account.data) else {
            continue;
        };
        arrays_loaded += 1;
        for b in arr.bins {
            pool_liquidity = pool_liquidity.saturating_add(b.liquidity_supply);
            fee_growth_a = fee_growth_a.saturating_add(b.fee_amount_x_per_token_stored);
            fee_growth_b = fee_growth_b.saturating_add(b.fee_amount_y_per_token_stored);
            if b.liquidity_supply > 0 || b.amount_x > 0 || b.amount_y > 0 {
                bins.insert(b.id, b);
            }
        }
    }

    let pool = pool_stats_from_lb_pair(
        row.pool,
        &header,
        pool_liquidity.max(1),
        fee_growth_a,
        fee_growth_b,
    );
    let (token_a, token_b) = resolve_tokens(
        rpc,
        pool.token_mint_a,
        pool.token_mint_b,
        store,
        refresh_meta,
    )?;

    let (amount_a, amount_b) = amounts_from_shares(&position, &bins);
    let (fee_a, fee_b) = claim_fees_from_bins(&position, &bins);
    let series = build_bin_series(
        &bins,
        &position,
        header.active_id,
        sol_clmm_manager::CHART_FETCH_HALF_SPACINGS,
    );
    let initialized_ticks = bins.len();
    let (series_min_tick, series_max_tick) = if series.is_empty() {
        (fetch_lo, fetch_hi)
    } else {
        (
            series.first().unwrap().tick.min(fetch_lo),
            series.last().unwrap().tick.max(fetch_hi),
        )
    };
    let series_max_liquidity = series.iter().map(|s| s.liquidity).max().unwrap_or(1).max(1);
    let in_range = header.active_id >= position.lower_bin && header.active_id <= position.upper_bin;

    let vaults = rpc
        .get_multiple_accounts(&[pool.token_vault_a, pool.token_vault_b])
        .ok();
    let vault_a_raw = vaults
        .as_ref()
        .and_then(|v| v.first())
        .and_then(|a| a.as_ref())
        .and_then(|a| read_token_account_amount(&a.data).ok());
    let vault_b_raw = vaults
        .as_ref()
        .and_then(|v| v.get(1))
        .and_then(|a| a.as_ref())
        .and_then(|a| read_token_account_amount(&a.data).ok());

    // LM rewards: sum of on-chain UserRewardInfo.pendings (bin growth needs
    // reward_per_token on Bin — not in current 144B layout).
    let mut reward_mints = Vec::new();
    for ri in &header.reward_infos {
        if ri.mint != Pubkey::default() {
            reward_mints.push(ri.mint);
        }
    }
    let reward_tokens = resolve_mints(rpc, &reward_mints, store, refresh_meta)?;

    let mut price_tokens = vec![token_a.clone(), token_b.clone()];
    for mint in &reward_mints {
        if let Some(t) = reward_tokens.get(mint) {
            price_tokens.push(t.clone());
        }
    }
    let prices = fetch_mint_usd_prices(
        rpc,
        price_mode,
        price_keys,
        &price_tokens,
        std::slice::from_ref(&pool),
        Some(rpc.meter()),
    );
    let fee_a_usd = raw_to_usd(
        fee_a as u128,
        token_a.decimals,
        prices.get(&token_a.mint).and_then(|t| t.price),
    );
    let fee_b_usd = raw_to_usd(
        fee_b as u128,
        token_b.decimals,
        prices.get(&token_b.mint).and_then(|t| t.price),
    );
    let fees_usd = match (fee_a_usd, fee_b_usd) {
        (Some(a), Some(b)) => Some(a + b),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };

    let mut rewards = Vec::new();
    let mut rewards_usd_sum = 0.0;
    let mut any_reward_usd = false;
    for (i, ri) in header.reward_infos.iter().enumerate() {
        let raw = position.reward_pending[i];
        if ri.mint == Pubkey::default() || raw == 0 {
            continue;
        }
        let info = reward_tokens.get(&ri.mint);
        let decimals = info.map(|t| t.decimals).unwrap_or(0);
        let symbol = info
            .map(|t| t.symbol.clone())
            .unwrap_or_else(|| ri.mint.to_string().chars().take(4).collect());
        let usd = raw_to_usd(
            raw as u128,
            decimals,
            prices.get(&ri.mint).and_then(|t| t.price),
        );
        if let Some(u) = usd {
            rewards_usd_sum += u;
            any_reward_usd = true;
        }
        rewards.push(RewardInfo {
            mint: ri.mint,
            symbol: symbol.clone(),
            decimals,
            amount_raw: raw,
            amount_ui: format_amount_usd(raw as u128, decimals, &symbol, usd),
            amount_usd: usd,
        });
    }
    let rewards_usd = if any_reward_usd {
        Some(rewards_usd_sum)
    } else {
        None
    };

    let usd = Some(estimate_position_usd_with_extra_mints(
        rpc,
        price_mode,
        price_keys,
        &pool,
        &token_a,
        &token_b,
        amount_a,
        amount_b,
        fee_a,
        fee_b,
        vault_a_raw,
        vault_b_raw,
        &[],
        rewards_usd.unwrap_or(0.0),
        Some(rpc.meter()),
    ));

    let period = if let Some(store) = store {
        let slot = rpc.get_slot().unwrap_or(0);
        let snap = PoolSnapshotInput {
            venue: Venue::MeteoraDlmm,
            pool: row.pool,
            slot,
            fee_growth_global_a: pool.fee_growth_global_a,
            fee_growth_global_b: pool.fee_growth_global_b,
            protocol_fee_owed_a: 0,
            protocol_fee_owed_b: 0,
            fee_rate: pool.fee_rate,
            liquidity: pool.liquidity,
            sqrt_price: 0,
            tick_current: pool.tick_current_index,
            vault_a: vault_a_raw.unwrap_or(0),
            vault_b: vault_b_raw.unwrap_or(0),
            price_a_usd: prices.get(&token_a.mint).and_then(|t| t.price),
            price_b_usd: prices.get(&token_b.mint).and_then(|t| t.price),
        };
        store
            .record_pool_snapshot(
                &snap,
                token_a.decimals,
                token_b.decimals,
                &token_a.symbol,
                &token_b.symbol,
            )
            .ok()
            .flatten()
    } else {
        None
    };

    Ok(PositionDetail {
        row: row.clone(),
        pool,
        position_mint: None,
        position_nft_token_program: None,
        reward_vaults: Vec::new(),
        token_a: token_a.clone(),
        token_b: token_b.clone(),
        position_liquidity: position.liquidity,
        amount_a,
        amount_b,
        amount_a_ui: format_amount_usd(
            amount_a,
            token_a.decimals,
            &token_a.symbol,
            raw_to_usd(
                amount_a,
                token_a.decimals,
                prices.get(&token_a.mint).and_then(|t| t.price),
            ),
        ),
        amount_b_ui: format_amount_usd(
            amount_b,
            token_b.decimals,
            &token_b.symbol,
            raw_to_usd(
                amount_b,
                token_b.decimals,
                prices.get(&token_b.mint).and_then(|t| t.price),
            ),
        ),
        fee_a,
        fee_b,
        fee_a_ui: format_amount_usd(fee_a as u128, token_a.decimals, &token_a.symbol, fee_a_usd),
        fee_b_ui: format_amount_usd(fee_b as u128, token_b.decimals, &token_b.symbol, fee_b_usd),
        fee_a_usd,
        fee_b_usd,
        fees_usd,
        rewards,
        rewards_usd,
        in_range,
        series,
        series_min_tick,
        series_max_tick,
        series_max_liquidity,
        initialized_ticks,
        arrays_loaded,
        usd,
        vault_a_raw,
        vault_b_raw,
        vault_a_ui: vault_a_raw
            .map(|v| {
                format_amount_usd(
                    v as u128,
                    token_a.decimals,
                    &token_a.symbol,
                    raw_to_usd(
                        v as u128,
                        token_a.decimals,
                        prices.get(&token_a.mint).and_then(|t| t.price),
                    ),
                )
            })
            .unwrap_or_else(|| "—".into()),
        vault_b_ui: vault_b_raw
            .map(|v| {
                format_amount_usd(
                    v as u128,
                    token_b.decimals,
                    &token_b.symbol,
                    raw_to_usd(
                        v as u128,
                        token_b.decimals,
                        prices.get(&token_b.mint).and_then(|t| t.price),
                    ),
                )
            })
            .unwrap_or_else(|| "—".into()),
        period,
        wallet_balances: None,
    })
}

fn needed_bin_array_indices(lower: i32, upper: i32, active: i32) -> Vec<i64> {
    let mut set = BTreeSet::new();
    for id in [lower, upper, active] {
        let idx = bin_id_to_bin_array_index(id);
        set.insert(idx);
        set.insert(idx - 1);
        set.insert(idx + 1);
    }
    let mut i = bin_id_to_bin_array_index(lower);
    let end = bin_id_to_bin_array_index(upper);
    while i <= end {
        set.insert(i);
        i += 1;
    }
    set.into_iter().collect()
}

/// Fetch lb-pair + bin arrays around active for create-wizard range chart.
pub fn fetch_pool_preview(
    rpc: &Rpc<'_>,
    pool: Pubkey,
    wallet: Option<&Pubkey>,
    store: Option<&Store>,
    refresh_meta: bool,
    price_keys: &PriceApiKeys,
) -> Result<PoolPreview> {
    use sol_clmm_manager::detail::TokenSide;

    let program = dlmm_program_id();
    let pool_acc = rpc.get_account(&pool).context("get lb pair")?;
    let header = decode_lb_pair_header(&pool_acc.data)?;

    // spacing=1 for DLMM bin ids.
    let (fetch_lo, fetch_hi) = PoolPreview::default_range(
        header.active_id,
        1,
        sol_clmm_manager::CHART_FETCH_HALF_SPACINGS,
    );
    let (draft_lo, draft_hi) = PoolPreview::default_range(header.active_id, 1, 16);

    let indices = needed_bin_array_indices(fetch_lo, fetch_hi, header.active_id);
    let addrs: Vec<Pubkey> = indices
        .iter()
        .map(|i| bin_array_pda(&pool, *i, &program))
        .collect();
    let accounts = rpc
        .get_multiple_accounts(&addrs)
        .context("get bin arrays")?;

    let mut bins: BTreeMap<i32, _> = BTreeMap::new();
    let mut arrays_loaded = 0usize;
    let mut pool_liquidity: u128 = 0;
    let mut fee_growth_a: u128 = 0;
    let mut fee_growth_b: u128 = 0;
    for maybe in accounts {
        let Some(account) = maybe else { continue };
        let Ok(arr) = decode_bin_array(&account.data) else {
            continue;
        };
        arrays_loaded += 1;
        for b in arr.bins {
            pool_liquidity = pool_liquidity.saturating_add(b.liquidity_supply);
            fee_growth_a = fee_growth_a.saturating_add(b.fee_amount_x_per_token_stored);
            fee_growth_b = fee_growth_b.saturating_add(b.fee_amount_y_per_token_stored);
            if b.liquidity_supply > 0 || b.amount_x > 0 || b.amount_y > 0 {
                bins.insert(b.id, b);
            }
        }
    }

    let stats = pool_stats_from_lb_pair(
        pool,
        &header,
        pool_liquidity.max(1),
        fee_growth_a,
        fee_growth_b,
    );
    let (token_a, token_b) = resolve_tokens_with_keys(
        rpc,
        stats.token_mint_a,
        stats.token_mint_b,
        store,
        refresh_meta,
        Some(price_keys),
    )?;

    // Full fetch window so create-wizard zoom-out has depth context.
    let min_b = fetch_lo.min(header.active_id);
    let max_b = fetch_hi.max(header.active_id);
    let mut points: Vec<(i32, u128)> = Vec::new();
    for (&id, bin) in &bins {
        if id < min_b || id > max_b {
            continue;
        }
        let depth = (bin.amount_x as u128)
            .saturating_add(bin.amount_y as u128)
            .max(bin.liquidity_supply);
        if depth > 0 {
            points.push((id, depth));
        }
    }
    if points.is_empty() {
        points.push((header.active_id, pool_liquidity.max(1)));
        points.push((draft_lo, 1));
        points.push((draft_hi, 1));
    }
    points.sort_by_key(|(id, _)| *id);
    let samples: Vec<_> = if points.len() <= 70 {
        points
    } else {
        let step = points.len() / 56;
        points.into_iter().step_by(step.max(1)).collect()
    };

    let series: Vec<_> = samples
        .into_iter()
        .map(|(id, l)| {
            let in_position = id >= draft_lo && id <= draft_hi;
            let side = if !in_position {
                TokenSide::Outside
            } else if id < header.active_id {
                TokenSide::A
            } else if id > header.active_id {
                TokenSide::B
            } else {
                TokenSide::Both
            };
            sol_clmm_manager::LiquiditySample {
                tick: id,
                liquidity: sol_clmm_manager::clmm_math::scale_u128(l.max(1)),
                side,
                in_position,
            }
        })
        .collect();
    let series_max_liquidity = series.iter().map(|s| s.liquidity).max().unwrap_or(1).max(1);
    let initialized_ticks = bins.len();

    // DLMM uses bin_step as tick_spacing in PoolStats — nudge by 1 bin via spacing override.
    let mut stats = stats;
    stats.tick_spacing = 1;

    let balances = wallet.and_then(|w| {
        fetch_wallet_pool_balances(rpc, w, stats.token_mint_a, stats.token_mint_b).ok()
    });

    let mut preview = PoolPreview {
        venue: Venue::MeteoraDlmm,
        pool: stats,
        token_a,
        token_b,
        balances,
        tick_lower: draft_lo,
        tick_upper: draft_hi,
        series,
        series_max_liquidity,
        series_min_tick: fetch_lo,
        series_max_tick: fetch_hi,
        arrays_loaded,
        initialized_ticks,
        chart_half_spacings: sol_clmm_manager::CHART_HALF_SPACINGS_DEFAULT,
    };
    preview.apply_range(draft_lo, draft_hi);
    preview.zoom_full();
    Ok(preview)
}
