//! Raydium CLMM detail: claim-sim fees · amounts · chart · snapshot volume.

use super::clmm_program_id;
use super::decode::{
    TICK_ARRAY_SIZE, decode_amm_config_trade_fee, decode_personal_position, decode_pool_header,
    decode_tick_array, pool_stats_from_header, tick_array_pda, tick_array_start_index,
};
use anyhow::{Context, Result};
use sol_clmm_manager::Venue;
use sol_clmm_manager::clmm_math::{
    amounts_from_liquidity, build_liquidity_series, read_token_account_amount,
};
use sol_clmm_manager::detail::{PoolPreview, PositionDetail, RewardInfo};
use sol_clmm_manager::fees::{
    collect_fees_quote, fee_growth_inside, format_amount_usd, tokens_owed_delta,
};
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
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn fetch_position_detail(
    rpc: &Rpc<'_>,
    row: &PositionRow,
    store: Option<&Store>,
    price_mode: PriceMode,
    price_keys: &PriceApiKeys,
    refresh_meta: bool,
) -> Result<PositionDetail> {
    let program = clmm_program_id();

    let pos_acc = rpc
        .get_account(&row.address)
        .context("get position account")?;
    let position = decode_personal_position(&pos_acc.data)?;

    let pool_acc = rpc.get_account(&row.pool).context("get pool account")?;
    let header = decode_pool_header(&pool_acc.data)?;

    let trade_fee = rpc
        .get_account(&header.amm_config)
        .ok()
        .and_then(|a| decode_amm_config_trade_fee(&a.data).ok())
        .unwrap_or(0);
    let pool = pool_stats_from_header(row.pool, &header, trade_fee);

    let (token_a, token_b) = resolve_tokens(
        rpc,
        pool.token_mint_a,
        pool.token_mint_b,
        store,
        refresh_meta,
    )?;

    let (amount_a, amount_b) = amounts_from_liquidity(
        position.liquidity,
        pool.sqrt_price,
        position.tick_lower,
        position.tick_upper,
    );

    let spacing = pool.tick_spacing;
    let (wide_lo, wide_hi) = sol_clmm_manager::PoolPreview::default_range(
        pool.tick_current_index,
        spacing,
        sol_clmm_manager::CHART_FETCH_HALF_SPACINGS,
    );
    let fetch_lo = wide_lo.min(position.tick_lower);
    let fetch_hi = wide_hi.max(position.tick_upper);
    let starts = needed_array_starts(
        fetch_lo,
        fetch_hi,
        pool.tick_current_index,
        spacing,
    );
    let addrs: Vec<Pubkey> = starts
        .iter()
        .map(|s| tick_array_pda(&row.pool, *s, &program))
        .collect();
    let accounts = rpc
        .get_multiple_accounts(&addrs)
        .context("get tick arrays")?;

    let mut ticks = BTreeMap::new();
    let mut arrays_loaded = 0usize;
    for maybe in accounts {
        let Some(account) = maybe else { continue };
        let Ok(arr) = decode_tick_array(&account.data) else {
            continue;
        };
        arrays_loaded += 1;
        for t in arr.ticks {
            if t.initialized || t.liquidity_gross > 0 || t.liquidity_net != 0 {
                ticks.insert(t.index, t);
            }
        }
    }

    let tick_lower = ticks.get(&position.tick_lower).copied();
    let tick_upper = ticks.get(&position.tick_upper).copied();
    let (fee_a, fee_b) = match (tick_lower, tick_upper) {
        (Some(lo), Some(hi)) => (
            collect_fees_quote(
                position.fees_owed0,
                pool.fee_growth_global_a,
                position.fee_growth_inside0_last_x64,
                lo.fee_growth_outside_a,
                hi.fee_growth_outside_a,
                position.tick_lower,
                position.tick_upper,
                pool.tick_current_index,
                position.liquidity,
            ),
            collect_fees_quote(
                position.fees_owed1,
                pool.fee_growth_global_b,
                position.fee_growth_inside1_last_x64,
                lo.fee_growth_outside_b,
                hi.fee_growth_outside_b,
                position.tick_lower,
                position.tick_upper,
                pool.tick_current_index,
                position.liquidity,
            ),
        ),
        _ => (position.fees_owed0, position.fees_owed1),
    };

    // Rewards: accrue global growth to now, then inside formula (same as Orca).
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut reward_mints = Vec::new();
    let mut reward_raw = [0u64; 3];
    for (i, raw) in reward_raw.iter_mut().enumerate() {
        let ri = &header.reward_infos[i];
        if ri.token_mint == Pubkey::default() {
            continue;
        }
        let mut growth = ri.reward_growth_global_x64;
        if pool.liquidity != 0 && ri.emissions_per_second_x64 != 0 {
            let last = ri.last_update_time;
            let end = if ri.end_time == 0 {
                now
            } else {
                now.min(ri.end_time)
            };
            let dt = end.saturating_sub(last);
            if dt > 0 {
                growth = growth.saturating_add(
                    ri.emissions_per_second_x64.saturating_mul(dt as u128) / pool.liquidity,
                );
            }
        }
        let (out_lo, out_hi) = match (tick_lower, tick_upper) {
            (Some(lo), Some(hi)) => (lo.reward_growths_outside[i], hi.reward_growths_outside[i]),
            _ => (0, 0),
        };
        let inside = fee_growth_inside(
            growth,
            out_lo,
            out_hi,
            position.tick_lower,
            position.tick_upper,
            pool.tick_current_index,
        );
        let delta = tokens_owed_delta(
            inside,
            position.reward_infos[i].growth_inside_last_x64,
            position.liquidity,
        );
        *raw = position.reward_infos[i]
            .reward_amount_owed
            .saturating_add(delta);
        // Skip inactive / ended reward slots (no ATA create, no meta fetch).
        if ri.claimable(*raw, now) {
            reward_mints.push(ri.token_mint);
        }
    }
    let reward_tokens = resolve_mints(rpc, &reward_mints, store, refresh_meta)?;

    let initialized_ticks = ticks.values().filter(|t| t.initialized).count();
    let series = build_liquidity_series(
        &ticks,
        pool.liquidity,
        position.liquidity,
        pool.tick_current_index,
        position.tick_lower,
        position.tick_upper,
        spacing,
        sol_clmm_manager::CHART_FETCH_HALF_SPACINGS,
    );
    let (series_min_tick, series_max_tick) = if series.is_empty() {
        (fetch_lo, fetch_hi)
    } else {
        (
            series.first().unwrap().tick.min(fetch_lo),
            series.last().unwrap().tick.max(fetch_hi),
        )
    };
    let series_max_liquidity = series.iter().map(|s| s.liquidity).max().unwrap_or(1).max(1);
    let in_range = pool.tick_current_index >= position.tick_lower
        && pool.tick_current_index < position.tick_upper;

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
    for (i, mint) in header.reward_infos.iter().map(|r| r.token_mint).enumerate() {
        if mint == Pubkey::default() || reward_raw[i] == 0 {
            continue;
        }
        let info = reward_tokens.get(&mint);
        let decimals = info.map(|t| t.decimals).unwrap_or(0);
        let symbol = info
            .map(|t| t.symbol.clone())
            .unwrap_or_else(|| mint.to_string().chars().take(4).collect());
        let usd = raw_to_usd(
            reward_raw[i] as u128,
            decimals,
            prices.get(&mint).and_then(|t| t.price),
        );
        if let Some(u) = usd {
            rewards_usd_sum += u;
            any_reward_usd = true;
        }
        rewards.push(RewardInfo {
            mint,
            symbol: symbol.clone(),
            decimals,
            amount_raw: reward_raw[i],
            amount_ui: format_amount_usd(reward_raw[i] as u128, decimals, &symbol, usd),
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
            venue: Venue::RaydiumClmm,
            pool: row.pool,
            slot,
            fee_growth_global_a: pool.fee_growth_global_a,
            fee_growth_global_b: pool.fee_growth_global_b,
            protocol_fee_owed_a: pool.protocol_fee_owed_a,
            protocol_fee_owed_b: pool.protocol_fee_owed_b,
            fee_rate: pool.fee_rate,
            liquidity: pool.liquidity,
            sqrt_price: pool.sqrt_price,
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

    // Claimable only: actively emitting (in schedule), or ended with owed > 0.
    let reward_vaults: Vec<(u8, Pubkey, Pubkey)> = header
        .reward_infos
        .iter()
        .enumerate()
        .filter(|(i, r)| r.claimable(reward_raw[*i], now))
        .map(|(i, r)| (i as u8, r.token_mint, r.token_vault))
        .collect();

    Ok(PositionDetail {
        row: row.clone(),
        pool,
        position_mint: Some(position.nft_mint),
        reward_vaults,
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

fn needed_array_starts(lower: i32, upper: i32, current: i32, spacing: u16) -> Vec<i32> {
    let mut set = std::collections::BTreeSet::new();
    let span = TICK_ARRAY_SIZE * spacing as i32;
    for t in [lower, upper, current] {
        let start = tick_array_start_index(t, spacing);
        set.insert(start);
        set.insert(start - span);
        set.insert(start + span);
    }
    let mut t = tick_array_start_index(lower, spacing);
    let end = tick_array_start_index(upper, spacing);
    while t <= end {
        set.insert(t);
        t += span;
    }
    set.into_iter().collect()
}

/// Fetch pool + tick arrays around current for create-wizard range chart.
pub fn fetch_pool_preview(
    rpc: &Rpc<'_>,
    pool: Pubkey,
    wallet: Option<&Pubkey>,
    store: Option<&Store>,
    refresh_meta: bool,
    price_keys: &PriceApiKeys,
) -> Result<PoolPreview> {
    let program = clmm_program_id();
    let pool_acc = rpc.get_account(&pool).context("get raydium pool")?;
    let header = decode_pool_header(&pool_acc.data)?;
    let trade_fee = rpc
        .get_account(&header.amm_config)
        .ok()
        .and_then(|a| decode_amm_config_trade_fee(&a.data).ok())
        .unwrap_or(0);
    let stats = pool_stats_from_header(pool, &header, trade_fee);
    let (token_a, token_b) = resolve_tokens_with_keys(
        rpc,
        stats.token_mint_a,
        stats.token_mint_b,
        store,
        refresh_meta,
        Some(price_keys),
    )?;

    let spacing = stats.tick_spacing;
    let (fetch_lo, fetch_hi) = PoolPreview::default_range(
        stats.tick_current_index,
        spacing,
        sol_clmm_manager::CHART_FETCH_HALF_SPACINGS,
    );
    let (draft_lo, draft_hi) = PoolPreview::default_range(stats.tick_current_index, spacing, 16);

    let starts = needed_array_starts(fetch_lo, fetch_hi, stats.tick_current_index, spacing);
    let addrs: Vec<Pubkey> = starts
        .iter()
        .map(|s| tick_array_pda(&pool, *s, &program))
        .collect();
    let accounts = rpc
        .get_multiple_accounts(&addrs)
        .context("get tick arrays")?;

    let mut ticks = BTreeMap::new();
    let mut arrays_loaded = 0usize;
    for maybe in accounts {
        let Some(account) = maybe else { continue };
        let Ok(arr) = decode_tick_array(&account.data) else {
            continue;
        };
        arrays_loaded += 1;
        for t in arr.ticks {
            if t.initialized || t.liquidity_gross > 0 || t.liquidity_net != 0 {
                ticks.insert(t.index, t);
            }
        }
    }

    let initialized_ticks = ticks.values().filter(|t| t.initialized).count();
    let series = build_liquidity_series(
        &ticks,
        stats.liquidity,
        0,
        stats.tick_current_index,
        fetch_lo,
        fetch_hi,
        spacing,
        16,
    );
    let series_max_liquidity = series.iter().map(|s| s.liquidity).max().unwrap_or(1).max(1);

    let balances = wallet.and_then(|w| {
        fetch_wallet_pool_balances(rpc, w, stats.token_mint_a, stats.token_mint_b).ok()
    });

    let mut preview = PoolPreview {
        venue: Venue::RaydiumClmm,
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
