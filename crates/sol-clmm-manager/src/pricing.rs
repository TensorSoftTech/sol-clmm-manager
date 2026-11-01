//! Approximate USD valuation (on-chain multi-venue hop · Jupiter · Titan).
//!
//! On-chain hops probe **Orca Whirlpool** + **Raydium CLMM** PDAs (then
//! **Meteora DLMM** bin-step PDAs) vs stables / SOL — no HTTPS price API.

use crate::pool::{IndexSpace, PoolStats};
use crate::quotes::{jupiter, titan};
use crate::rpc::{Rpc, RpcMeter};
use crate::stables::{
    self, METEORA_BASE_FACTORS, METEORA_BIN_STEPS, PRICE_TICK_SPACINGS, RAYDIUM_AMM_CONFIG_INDICES,
    is_stablecoin, whirlpools_config, wsol_mint,
};
use crate::tokens::TokenInfo;
use solana_pubkey::Pubkey;
use std::collections::HashMap;
use std::str::FromStr;

const WHIRLPOOL_PROGRAM_ID: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";
const WHIRLPOOL_DISC: [u8; 8] = [63, 149, 209, 12, 225, 128, 99, 9];
const RAYDIUM_CLMM_PROGRAM_ID: &str = "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK";
const METEORA_DLMM_PROGRAM_ID: &str = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PriceMode {
    #[default]
    OnChain,
    Jupiter,
    Titan,
}

impl PriceMode {
    pub const ALL: [PriceMode; 3] = [PriceMode::OnChain, PriceMode::Jupiter, PriceMode::Titan];

    pub fn label(self) -> &'static str {
        match self {
            Self::OnChain => "on-chain · Orca / Raydium / Meteora vs stables (+ SOL hop)",
            Self::Jupiter => "Jupiter Price API · key optional (free throttled)",
            Self::Titan => "Titan Portal · API key (free sandbox or paid)",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Self::OnChain => "on-chain",
            Self::Jupiter => "jupiter",
            Self::Titan => "titan",
        }
    }

    pub fn from_setting(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "jupiter" | "jup" => Self::Jupiter,
            "titan" => Self::Titan,
            _ => Self::OnChain,
        }
    }

    pub fn as_setting(self) -> &'static str {
        match self {
            Self::OnChain => "onchain",
            Self::Jupiter => "jupiter",
            Self::Titan => "titan",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::OnChain => Self::Jupiter,
            Self::Jupiter => Self::Titan,
            Self::Titan => Self::OnChain,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceSource {
    StableParity,
    CurrentPool,
    StablePool,
    SolHop,
    Jupiter,
    Titan,
    Missing,
}

impl PriceSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::StableParity => "stable≈$1",
            Self::CurrentPool => "this pool",
            Self::StablePool => "pool/stable",
            Self::SolHop => "via SOL",
            Self::Jupiter => "jupiter",
            Self::Titan => "titan",
            Self::Missing => "n/a",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TokenUsd {
    pub price: Option<f64>,
    pub source: PriceSource,
}

#[derive(Debug, Clone)]
pub struct UsdEstimate {
    pub mode: PriceMode,
    pub price_a: TokenUsd,
    pub price_b: TokenUsd,
    pub amount_a_usd: Option<f64>,
    pub amount_b_usd: Option<f64>,
    pub fees_a_usd: Option<f64>,
    pub fees_b_usd: Option<f64>,
    pub position_usd: Option<f64>,
    pub pool_vault_a_usd: Option<f64>,
    pub pool_vault_b_usd: Option<f64>,
    pub pool_tvl_usd: Option<f64>,
    pub note: String,
}

#[derive(Debug, Clone, Default)]
pub struct PriceApiKeys {
    pub jupiter: Option<String>,
    pub titan: Option<String>,
    pub paid_fast: bool,
}

pub fn human_price_b_per_a(sqrt_price_x64: u128, decimals_a: u8, decimals_b: u8) -> Option<f64> {
    let sp = (sqrt_price_x64 as f64) / ((1u128 << 64) as f64);
    if !(sp.is_finite() && sp > 0.0) {
        return None;
    }
    let raw = sp * sp;
    let scale = 10f64.powi(decimals_a as i32 - decimals_b as i32);
    let p = raw * scale;
    if p.is_finite() && p > 0.0 {
        Some(p)
    } else {
        None
    }
}

fn whirlpool_program_id() -> Pubkey {
    Pubkey::from_str(WHIRLPOOL_PROGRAM_ID).expect("WHIRLPOOL_PROGRAM_ID")
}

fn raydium_clmm_program_id() -> Pubkey {
    Pubkey::from_str(RAYDIUM_CLMM_PROGRAM_ID).expect("RAYDIUM_CLMM_PROGRAM_ID")
}

fn meteora_dlmm_program_id() -> Pubkey {
    Pubkey::from_str(METEORA_DLMM_PROGRAM_ID).expect("METEORA_DLMM_PROGRAM_ID")
}

fn sort_mints(a: &Pubkey, b: &Pubkey) -> (Pubkey, Pubkey) {
    if a.to_bytes() < b.to_bytes() {
        (*a, *b)
    } else {
        (*b, *a)
    }
}

pub fn whirlpool_pda(mint_x: &Pubkey, mint_y: &Pubkey, tick_spacing: u16) -> Pubkey {
    let (a, b) = sort_mints(mint_x, mint_y);
    let program = whirlpool_program_id();
    let config = whirlpools_config();
    Pubkey::find_program_address(
        &[
            b"whirlpool",
            config.as_ref(),
            a.as_ref(),
            b.as_ref(),
            &tick_spacing.to_le_bytes(),
        ],
        &program,
    )
    .0
}

/// Raydium CLMM `AmmConfig` PDA for `index` (u16 big-endian seed).
pub fn raydium_amm_config_pda(index: u16) -> Pubkey {
    let program = raydium_clmm_program_id();
    Pubkey::find_program_address(&[b"amm_config", &index.to_be_bytes()], &program).0
}

/// Raydium CLMM pool PDA for `(amm_config, mint_a, mint_b)` — mints sorted.
pub fn raydium_pool_pda(amm_config: &Pubkey, mint_x: &Pubkey, mint_y: &Pubkey) -> Pubkey {
    let (a, b) = sort_mints(mint_x, mint_y);
    let program = raydium_clmm_program_id();
    Pubkey::find_program_address(
        &[b"pool", amm_config.as_ref(), a.as_ref(), b.as_ref()],
        &program,
    )
    .0
}

/// Meteora DLMM permissionless v1 LbPair PDA: sorted mints + `bin_step` LE.
pub fn meteora_lb_pair_pda_v1(mint_x: &Pubkey, mint_y: &Pubkey, bin_step: u16) -> Pubkey {
    let (a, b) = sort_mints(mint_x, mint_y);
    let program = meteora_dlmm_program_id();
    Pubkey::find_program_address(&[a.as_ref(), b.as_ref(), &bin_step.to_le_bytes()], &program).0
}

/// Meteora DLMM permissionless v2 LbPair PDA: sorted mints + `bin_step` + `base_factor`.
pub fn meteora_lb_pair_pda_v2(
    mint_x: &Pubkey,
    mint_y: &Pubkey,
    bin_step: u16,
    base_factor: u16,
) -> Pubkey {
    let (a, b) = sort_mints(mint_x, mint_y);
    let program = meteora_dlmm_program_id();
    Pubkey::find_program_address(
        &[
            a.as_ref(),
            b.as_ref(),
            &bin_step.to_le_bytes(),
            &base_factor.to_le_bytes(),
        ],
        &program,
    )
    .0
}

fn decode_whirlpool_for_price(address: Pubkey, data: &[u8]) -> Option<PoolStats> {
    if data.len() < 8 + 237 || data[0..8] != WHIRLPOOL_DISC {
        return None;
    }
    let tick_spacing = u16::from_le_bytes(data[8 + 33..8 + 35].try_into().ok()?);
    let fee_rate = u16::from_le_bytes(data[8 + 37..8 + 39].try_into().ok()?) as u32;
    let liquidity = u128::from_le_bytes(data[8 + 41..8 + 57].try_into().ok()?);
    let sqrt_price = u128::from_le_bytes(data[8 + 57..8 + 73].try_into().ok()?);
    let tick_current_index = i32::from_le_bytes(data[8 + 73..8 + 77].try_into().ok()?);
    let token_mint_a = Pubkey::new_from_array(data[8 + 93..8 + 125].try_into().ok()?);
    let token_vault_a = Pubkey::new_from_array(data[8 + 125..8 + 157].try_into().ok()?);
    let token_mint_b = Pubkey::new_from_array(data[8 + 173..8 + 205].try_into().ok()?);
    let token_vault_b = Pubkey::new_from_array(data[8 + 205..8 + 237].try_into().ok()?);
    Some(PoolStats {
        address,
        token_mint_a,
        token_mint_b,
        token_vault_a,
        token_vault_b,
        tick_spacing,
        fee_rate,
        liquidity,
        sqrt_price,
        tick_current_index,
        index_space: IndexSpace::Tick,
        fee_growth_global_a: 0,
        fee_growth_global_b: 0,
        protocol_fee_owed_a: 0,
        protocol_fee_owed_b: 0,
    })
}

/// Lightweight Raydium PoolState decode for √P hops (mirrors raydium-clmm offsets).
fn decode_raydium_for_price(address: Pubkey, data: &[u8]) -> Option<PoolStats> {
    // disc(8)+bump(1)+amm_config+owner+mint0+mint1+vault0+vault1+obs + decimals + tick_spacing + L + √P + tick
    const MIN: usize = 8 + 1 + 32 * 7 + 1 + 1 + 2 + 16 + 16 + 4;
    if data.len() < MIN {
        return None;
    }
    let mut o = 8 + 1;
    o += 32; // amm_config
    o += 32; // owner
    let token_mint_a = Pubkey::new_from_array(data[o..o + 32].try_into().ok()?);
    o += 32;
    let token_mint_b = Pubkey::new_from_array(data[o..o + 32].try_into().ok()?);
    o += 32;
    let token_vault_a = Pubkey::new_from_array(data[o..o + 32].try_into().ok()?);
    o += 32;
    let token_vault_b = Pubkey::new_from_array(data[o..o + 32].try_into().ok()?);
    o += 32;
    o += 32; // observation
    o += 1 + 1; // mint decimals
    let tick_spacing = u16::from_le_bytes(data[o..o + 2].try_into().ok()?);
    o += 2;
    let liquidity = u128::from_le_bytes(data[o..o + 16].try_into().ok()?);
    o += 16;
    let sqrt_price = u128::from_le_bytes(data[o..o + 16].try_into().ok()?);
    o += 16;
    let tick_current_index = i32::from_le_bytes(data[o..o + 4].try_into().ok()?);
    if sqrt_price == 0 {
        return None;
    }
    Some(PoolStats {
        address,
        token_mint_a,
        token_mint_b,
        token_vault_a,
        token_vault_b,
        tick_spacing,
        fee_rate: 0,
        liquidity,
        sqrt_price,
        tick_current_index,
        index_space: IndexSpace::Tick,
        fee_growth_global_a: 0,
        fee_growth_global_b: 0,
        protocol_fee_owed_a: 0,
        protocol_fee_owed_b: 0,
    })
}

/// Lightweight Meteora LbPair decode for active-bin hops.
fn decode_meteora_for_price(address: Pubkey, data: &[u8]) -> Option<PoolStats> {
    // Same layout as meteora-dlmm::decode_lb_pair_header (mint_x @ 87).
    const MINT_X: usize = 87;
    const MINT_Y: usize = 119;
    const RESERVE_X: usize = 151;
    const RESERVE_Y: usize = 183;
    if data.len() < RESERVE_Y + 32 {
        return None;
    }
    let active_id = i32::from_le_bytes(data[76..80].try_into().ok()?);
    let bin_step = u16::from_le_bytes(data[80..82].try_into().ok()?);
    if bin_step == 0 {
        return None;
    }
    let token_mint_a = Pubkey::new_from_array(data[MINT_X..MINT_X + 32].try_into().ok()?);
    let token_mint_b = Pubkey::new_from_array(data[MINT_Y..MINT_Y + 32].try_into().ok()?);
    let token_vault_a = Pubkey::new_from_array(data[RESERVE_X..RESERVE_X + 32].try_into().ok()?);
    let token_vault_b = Pubkey::new_from_array(data[RESERVE_Y..RESERVE_Y + 32].try_into().ok()?);
    Some(PoolStats {
        address,
        token_mint_a,
        token_mint_b,
        token_vault_a,
        token_vault_b,
        tick_spacing: bin_step,
        fee_rate: 0,
        // No Uniswap-style L on LbPair — prefer tighter bin_step when ranking.
        liquidity: u128::from(u32::MAX.saturating_sub(bin_step as u32)),
        sqrt_price: 0,
        tick_current_index: active_id,
        index_space: IndexSpace::Bin { bin_step },
        fee_growth_global_a: 0,
        fee_growth_global_b: 0,
        protocol_fee_owed_a: 0,
        protocol_fee_owed_b: 0,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn estimate_position_usd(
    rpc: &Rpc<'_>,
    mode: PriceMode,
    keys: &PriceApiKeys,
    pool: &PoolStats,
    token_a: &TokenInfo,
    token_b: &TokenInfo,
    amount_a: u128,
    amount_b: u128,
    fee_a: u64,
    fee_b: u64,
    vault_a_raw: Option<u64>,
    vault_b_raw: Option<u64>,
    meter: Option<&RpcMeter>,
) -> UsdEstimate {
    estimate_position_usd_with_extra_mints(
        rpc,
        mode,
        keys,
        pool,
        token_a,
        token_b,
        amount_a,
        amount_b,
        fee_a,
        fee_b,
        vault_a_raw,
        vault_b_raw,
        &[],
        0.0,
        meter,
    )
}

/// Like [`estimate_position_usd`] but folds extra reward USD into `position_usd`
/// and returns prices for arbitrary mints via the returned map side-channel…
/// Callers that need reward mint prices should use [`fetch_mint_usd_prices`].
#[allow(clippy::too_many_arguments)]
pub fn estimate_position_usd_with_extra_mints(
    rpc: &Rpc<'_>,
    mode: PriceMode,
    keys: &PriceApiKeys,
    pool: &PoolStats,
    token_a: &TokenInfo,
    token_b: &TokenInfo,
    amount_a: u128,
    amount_b: u128,
    fee_a: u64,
    fee_b: u64,
    vault_a_raw: Option<u64>,
    vault_b_raw: Option<u64>,
    extra_tokens: &[TokenInfo],
    rewards_usd: f64,
    meter: Option<&RpcMeter>,
) -> UsdEstimate {
    let mut tokens = vec![token_a.clone(), token_b.clone()];
    tokens.extend(extra_tokens.iter().cloned());
    let prices = fetch_mint_usd_prices(rpc, mode, keys, &tokens, std::slice::from_ref(pool), meter);
    let mut est = estimate_usd_from_prices(
        mode,
        keys,
        token_a,
        token_b,
        prices.get(&token_a.mint).cloned(),
        prices.get(&token_b.mint).cloned(),
        amount_a,
        amount_b,
        fee_a,
        fee_b,
        vault_a_raw,
        vault_b_raw,
    );
    if rewards_usd.is_finite() && rewards_usd > 0.0 {
        est.position_usd = Some(est.position_usd.unwrap_or(0.0) + rewards_usd);
        if !est.note.contains("rewards") {
            est.note = format!("{} · +rewards", est.note);
        }
    }
    // Stash unused prices map by extending note only — callers fetch separately.
    let _ = prices;
    est
}

/// USD for a raw token amount given an optional unit price.
pub fn raw_to_usd(raw: u128, decimals: u8, price: Option<f64>) -> Option<f64> {
    let p = price?;
    if !p.is_finite() {
        return None;
    }
    let ui = if decimals == 0 {
        raw as f64
    } else {
        raw as f64 / 10f64.powi(decimals as i32)
    };
    let v = ui * p;
    if v.is_finite() { Some(v) } else { None }
}

pub fn fetch_mint_usd_prices(
    rpc: &Rpc<'_>,
    mode: PriceMode,
    keys: &PriceApiKeys,
    tokens: &[TokenInfo],
    pools: &[PoolStats],
    meter: Option<&RpcMeter>,
) -> HashMap<Pubkey, TokenUsd> {
    let mut out: HashMap<Pubkey, TokenUsd> = HashMap::new();
    if tokens.is_empty() {
        return out;
    }

    match mode {
        PriceMode::Jupiter => {
            let mints: Vec<Pubkey> = tokens.iter().map(|t| t.mint).collect();
            let map =
                jupiter::fetch_usd_prices(&mints, keys.jupiter.as_deref(), keys.paid_fast, meter)
                    .unwrap_or_default();
            for t in tokens {
                out.insert(
                    t.mint,
                    token_usd_from_map(&map, &t.mint, PriceSource::Jupiter),
                );
            }
        }
        PriceMode::Titan => {
            let mints: Vec<Pubkey> = tokens.iter().map(|t| t.mint).collect();
            let decimals: Vec<u8> = tokens.iter().map(|t| t.decimals).collect();
            let map = titan::fetch_usd_prices_with_tier(
                &mints,
                &decimals,
                keys.titan.as_deref(),
                keys.paid_fast,
                meter,
            )
            .unwrap_or_default();
            for t in tokens {
                out.insert(
                    t.mint,
                    token_usd_from_map(&map, &t.mint, PriceSource::Titan),
                );
            }
        }
        PriceMode::OnChain => {
            let mut cache: HashMap<Pubkey, TokenUsd> = HashMap::new();
            for pool in pools {
                let Some(ta) = tokens.iter().find(|t| t.mint == pool.token_mint_a) else {
                    continue;
                };
                let Some(tb) = tokens.iter().find(|t| t.mint == pool.token_mint_b) else {
                    continue;
                };
                if let Some((pa, pb)) = prices_from_pool_if_stable_pair(
                    pool,
                    &ta.mint,
                    ta.decimals,
                    &tb.mint,
                    tb.decimals,
                ) {
                    cache.insert(ta.mint, pa);
                    cache.insert(tb.mint, pb);
                }
            }
            for t in tokens {
                if out.contains_key(&t.mint) {
                    continue;
                }
                let p = price_onchain(rpc, &t.mint, t.decimals, &mut cache);
                out.insert(t.mint, p);
            }
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
pub fn estimate_usd_from_prices(
    mode: PriceMode,
    keys: &PriceApiKeys,
    token_a: &TokenInfo,
    token_b: &TokenInfo,
    price_a: Option<TokenUsd>,
    price_b: Option<TokenUsd>,
    amount_a: u128,
    amount_b: u128,
    fee_a: u64,
    fee_b: u64,
    vault_a_raw: Option<u64>,
    vault_b_raw: Option<u64>,
) -> UsdEstimate {
    let price_a = price_a.unwrap_or(TokenUsd {
        price: None,
        source: PriceSource::Missing,
    });
    let price_b = price_b.unwrap_or(TokenUsd {
        price: None,
        source: PriceSource::Missing,
    });

    let ui = |raw: u128, decimals: u8| -> f64 {
        if decimals == 0 {
            raw as f64
        } else {
            raw as f64 / 10f64.powi(decimals as i32)
        }
    };

    let amount_a_usd = price_a.price.map(|p| ui(amount_a, token_a.decimals) * p);
    let amount_b_usd = price_b.price.map(|p| ui(amount_b, token_b.decimals) * p);
    let fees_a_usd = price_a
        .price
        .map(|p| ui(fee_a as u128, token_a.decimals) * p);
    let fees_b_usd = price_b
        .price
        .map(|p| ui(fee_b as u128, token_b.decimals) * p);

    let mut sum = 0.0;
    let mut any = false;
    for v in [amount_a_usd, amount_b_usd, fees_a_usd, fees_b_usd]
        .into_iter()
        .flatten()
    {
        sum += v;
        any = true;
    }

    let pool_vault_a_usd = match (vault_a_raw, price_a.price) {
        (Some(raw), Some(p)) => Some(ui(raw as u128, token_a.decimals) * p),
        _ => None,
    };
    let pool_vault_b_usd = match (vault_b_raw, price_b.price) {
        (Some(raw), Some(p)) => Some(ui(raw as u128, token_b.decimals) * p),
        _ => None,
    };
    let pool_tvl_usd = match (pool_vault_a_usd, pool_vault_b_usd) {
        (Some(a), Some(b)) => Some(a + b),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };

    let keyed = match mode {
        PriceMode::Jupiter => keys.jupiter.as_ref().is_some_and(|s| !s.is_empty()),
        PriceMode::Titan => keys.titan.as_ref().is_some_and(|s| !s.is_empty()),
        PriceMode::OnChain => false,
    };
    let key_tag = match mode {
        PriceMode::OnChain => "",
        _ if keyed && keys.paid_fast => " · keyed/fast",
        _ if keyed => " · keyed/polite",
        _ => " · free/throttle",
    };
    let note = format!(
        "{}{key_tag} · A {} · B {}",
        mode.short(),
        price_a.source.label(),
        price_b.source.label()
    );

    UsdEstimate {
        mode,
        price_a,
        price_b,
        amount_a_usd,
        amount_b_usd,
        fees_a_usd,
        fees_b_usd,
        position_usd: if any { Some(sum) } else { None },
        pool_vault_a_usd,
        pool_vault_b_usd,
        pool_tvl_usd,
        note,
    }
}

fn token_usd_from_map(map: &HashMap<Pubkey, f64>, mint: &Pubkey, source: PriceSource) -> TokenUsd {
    if is_stablecoin(mint) {
        return TokenUsd {
            price: Some(1.0),
            source: PriceSource::StableParity,
        };
    }
    match map.get(mint).copied() {
        Some(p) => TokenUsd {
            price: Some(p),
            source,
        },
        None => TokenUsd {
            price: None,
            source: PriceSource::Missing,
        },
    }
}

fn prices_from_pool_if_stable_pair(
    pool: &PoolStats,
    mint_a: &Pubkey,
    dec_a: u8,
    mint_b: &Pubkey,
    dec_b: u8,
) -> Option<(TokenUsd, TokenUsd)> {
    let price_b_per_a = if pool.sqrt_price > 0 {
        human_price_b_per_a(pool.sqrt_price, dec_a, dec_b)?
    } else {
        // DLMM: approximate from active bin / bin_step.
        let raw = pool.index_space.price_at(pool.tick_current_index);
        if !(raw.is_finite() && raw > 0.0) {
            return None;
        }
        raw * 10f64.powi(dec_a as i32 - dec_b as i32)
    };
    let a_stable = is_stablecoin(mint_a);
    let b_stable = is_stablecoin(mint_b);
    if a_stable && b_stable {
        return Some((
            TokenUsd {
                price: Some(1.0),
                source: PriceSource::StableParity,
            },
            TokenUsd {
                price: Some(1.0),
                source: PriceSource::StableParity,
            },
        ));
    }
    if b_stable {
        return Some((
            TokenUsd {
                price: Some(price_b_per_a),
                source: PriceSource::CurrentPool,
            },
            TokenUsd {
                price: Some(1.0),
                source: PriceSource::StableParity,
            },
        ));
    }
    if a_stable {
        if price_b_per_a <= 0.0 {
            return None;
        }
        return Some((
            TokenUsd {
                price: Some(1.0),
                source: PriceSource::StableParity,
            },
            TokenUsd {
                price: Some(1.0 / price_b_per_a),
                source: PriceSource::CurrentPool,
            },
        ));
    }
    None
}

fn price_onchain(
    rpc: &Rpc<'_>,
    mint: &Pubkey,
    decimals: u8,
    cache: &mut HashMap<Pubkey, TokenUsd>,
) -> TokenUsd {
    if let Some(hit) = cache.get(mint) {
        return hit.clone();
    }
    if is_stablecoin(mint) {
        let t = TokenUsd {
            price: Some(1.0),
            source: PriceSource::StableParity,
        };
        cache.insert(*mint, t.clone());
        return t;
    }

    if let Some(t) = price_vs_stables(rpc, mint, decimals) {
        cache.insert(*mint, t.clone());
        return t;
    }

    let wsol = wsol_mint();
    if *mint != wsol
        && let Some(vs_sol) = price_in_pool_quote(rpc, mint, decimals, &wsol, 9)
    {
        let sol_usd = price_onchain(rpc, &wsol, 9, cache);
        if let Some(su) = sol_usd.price {
            let t = TokenUsd {
                price: Some(vs_sol * su),
                source: PriceSource::SolHop,
            };
            cache.insert(*mint, t.clone());
            return t;
        }
    }

    let t = TokenUsd {
        price: None,
        source: PriceSource::Missing,
    };
    cache.insert(*mint, t.clone());
    t
}

fn price_vs_stables(rpc: &Rpc<'_>, mint: &Pubkey, decimals: u8) -> Option<TokenUsd> {
    for stable in stables::stablecoin_mints() {
        let stable_decimals: u8 = 6;
        if let Some(usd) = price_in_pool_quote(rpc, mint, decimals, stable, stable_decimals) {
            return Some(TokenUsd {
                price: Some(usd),
                source: PriceSource::StablePool,
            });
        }
    }
    None
}

fn price_in_pool_quote(
    rpc: &Rpc<'_>,
    mint: &Pubkey,
    mint_decimals: u8,
    quote: &Pubkey,
    quote_decimals: u8,
) -> Option<f64> {
    if mint == quote {
        return Some(1.0);
    }

    // 1) Orca + Raydium PDA batches (cheap); keep highest-liquidity hit.
    let mut best: Option<(u128, f64)> = None;
    consider_orca_pools(rpc, mint, mint_decimals, quote, quote_decimals, &mut best);
    consider_raydium_pools(rpc, mint, mint_decimals, quote, quote_decimals, &mut best);
    if let Some((_, usd)) = best {
        return Some(usd);
    }

    // 2) Meteora DLMM only when CLMM PDAs miss (more addresses).
    consider_meteora_pools(rpc, mint, mint_decimals, quote, quote_decimals, &mut best);
    best.map(|(_, usd)| usd)
}

fn bump_best(best: &mut Option<(u128, f64)>, liquidity: u128, usd: f64) {
    if !(usd.is_finite() && usd > 0.0) {
        return;
    }
    match best {
        Some((l, _)) if *l >= liquidity => {}
        _ => *best = Some((liquidity, usd)),
    }
}

fn consider_orca_pools(
    rpc: &Rpc<'_>,
    mint: &Pubkey,
    mint_decimals: u8,
    quote: &Pubkey,
    quote_decimals: u8,
    best: &mut Option<(u128, f64)>,
) {
    let pdas: Vec<Pubkey> = PRICE_TICK_SPACINGS
        .iter()
        .map(|&ts| whirlpool_pda(mint, quote, ts))
        .collect();
    let Ok(accounts) = rpc.get_multiple_accounts(&pdas) else {
        return;
    };
    for (pda, maybe) in pdas.iter().zip(accounts) {
        let Some(acc) = maybe else { continue };
        let Some(pool) = decode_whirlpool_for_price(*pda, &acc.data) else {
            continue;
        };
        if let Some(usd) = usd_per_mint_from_pool(&pool, mint, mint_decimals, quote, quote_decimals)
        {
            bump_best(best, pool.liquidity, usd);
        }
    }
}

fn consider_raydium_pools(
    rpc: &Rpc<'_>,
    mint: &Pubkey,
    mint_decimals: u8,
    quote: &Pubkey,
    quote_decimals: u8,
    best: &mut Option<(u128, f64)>,
) {
    let pdas: Vec<Pubkey> = RAYDIUM_AMM_CONFIG_INDICES
        .iter()
        .map(|&idx| {
            let cfg = raydium_amm_config_pda(idx);
            raydium_pool_pda(&cfg, mint, quote)
        })
        .collect();
    let Ok(accounts) = rpc.get_multiple_accounts_chunked(&pdas) else {
        return;
    };
    for (pda, maybe) in pdas.iter().zip(accounts) {
        let Some(acc) = maybe else { continue };
        let Some(pool) = decode_raydium_for_price(*pda, &acc.data) else {
            continue;
        };
        // Require the pair we asked for (sorted mint0/mint1 on-chain).
        let pair_ok = (pool.token_mint_a == *mint && pool.token_mint_b == *quote)
            || (pool.token_mint_a == *quote && pool.token_mint_b == *mint);
        if !pair_ok {
            continue;
        }
        if let Some(usd) = usd_per_mint_from_pool(&pool, mint, mint_decimals, quote, quote_decimals)
        {
            bump_best(best, pool.liquidity, usd);
        }
    }
}

fn consider_meteora_pools(
    rpc: &Rpc<'_>,
    mint: &Pubkey,
    mint_decimals: u8,
    quote: &Pubkey,
    quote_decimals: u8,
    best: &mut Option<(u128, f64)>,
) {
    let mut pdas = Vec::with_capacity(METEORA_BIN_STEPS.len() * (1 + METEORA_BASE_FACTORS.len()));
    for &bs in METEORA_BIN_STEPS {
        pdas.push(meteora_lb_pair_pda_v1(mint, quote, bs));
        for &bf in METEORA_BASE_FACTORS {
            pdas.push(meteora_lb_pair_pda_v2(mint, quote, bs, bf));
        }
    }
    let Ok(accounts) = rpc.get_multiple_accounts_chunked(&pdas) else {
        return;
    };
    for (pda, maybe) in pdas.iter().zip(accounts) {
        let Some(acc) = maybe else { continue };
        let Some(pool) = decode_meteora_for_price(*pda, &acc.data) else {
            continue;
        };
        let pair_ok = (pool.token_mint_a == *mint && pool.token_mint_b == *quote)
            || (pool.token_mint_a == *quote && pool.token_mint_b == *mint);
        if !pair_ok {
            continue;
        }
        if let Some(usd) = usd_per_mint_from_pool(&pool, mint, mint_decimals, quote, quote_decimals)
        {
            bump_best(best, pool.liquidity, usd);
        }
    }
}

fn usd_per_mint_from_pool(
    pool: &PoolStats,
    mint: &Pubkey,
    mint_decimals: u8,
    quote: &Pubkey,
    quote_decimals: u8,
) -> Option<f64> {
    let dec_a = if pool.token_mint_a == *mint {
        mint_decimals
    } else if pool.token_mint_a == *quote {
        quote_decimals
    } else {
        return None;
    };
    let dec_b = if pool.token_mint_b == *mint {
        mint_decimals
    } else if pool.token_mint_b == *quote {
        quote_decimals
    } else {
        return None;
    };
    let p = if pool.sqrt_price > 0 {
        human_price_b_per_a(pool.sqrt_price, dec_a, dec_b)?
    } else {
        // DLMM: human B/A from active bin (same as CurrentPool path).
        let raw = pool.index_space.price_at(pool.tick_current_index);
        if !(raw.is_finite() && raw > 0.0) {
            return None;
        }
        raw * 10f64.powi(dec_a as i32 - dec_b as i32)
    };

    if pool.token_mint_a == *mint && pool.token_mint_b == *quote {
        Some(p)
    } else if pool.token_mint_b == *mint && pool.token_mint_a == *quote {
        if p > 0.0 { Some(1.0 / p) } else { None }
    } else {
        None
    }
}

pub fn format_usd(v: Option<f64>) -> String {
    match v {
        Some(x) if x.is_finite() => format_usd_axis(x),
        _ => "—".into(),
    }
}

pub fn format_usd_axis(x: f64) -> String {
    if !x.is_finite() || x < 0.0 {
        return "—".into();
    }
    if x >= 1_000_000.0 {
        format!("${:.1}M", x / 1_000_000.0)
    } else if x >= 10_000.0 {
        format!("${:.0}k", x / 1_000.0)
    } else if x >= 1000.0 {
        format!("${:.1}k", x / 1_000.0)
    } else if x >= 1.0 {
        format!("${x:.2}")
    } else if x >= 0.01 {
        format!("${x:.4}")
    } else if x == 0.0 {
        "$0".into()
    } else {
        format!("${x:.2e}")
    }
}

pub fn liquidity_l_to_usd(l: u64, pool_liquidity: u128, pool_tvl_usd: f64) -> Option<f64> {
    if pool_liquidity == 0 || !pool_tvl_usd.is_finite() || pool_tvl_usd < 0.0 {
        return None;
    }
    Some(pool_tvl_usd * (l as f64 / pool_liquidity as f64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn sol_usdc_ts8_pda_golden() {
        let sol = Pubkey::from_str(stables::WSOL).unwrap();
        let usdc = Pubkey::from_str(stables::USDC).unwrap();
        let pda = whirlpool_pda(&sol, &usdc, 8);
        assert_eq!(
            pda.to_string(),
            "7qbRF6YsyGuLUVs6Y1q64bdVrfe4ZcUUz1JRdoVNUJnm"
        );
    }

    #[test]
    fn raydium_amm_config0_deterministic() {
        let cfg0 = raydium_amm_config_pda(0);
        let cfg1 = raydium_amm_config_pda(1);
        assert_ne!(cfg0, cfg1);
        let sol = Pubkey::from_str(stables::WSOL).unwrap();
        let usdc = Pubkey::from_str(stables::USDC).unwrap();
        let p_ab = raydium_pool_pda(&cfg0, &sol, &usdc);
        let p_ba = raydium_pool_pda(&cfg0, &usdc, &sol);
        assert_eq!(p_ab, p_ba, "pool PDA must sort mints");
    }

    #[test]
    fn meteora_lb_pair_pda_sorts_mints() {
        let sol = Pubkey::from_str(stables::WSOL).unwrap();
        let usdc = Pubkey::from_str(stables::USDC).unwrap();
        let a = meteora_lb_pair_pda_v1(&sol, &usdc, 10);
        let b = meteora_lb_pair_pda_v1(&usdc, &sol, 10);
        assert_eq!(a, b);
        let v2 = meteora_lb_pair_pda_v2(&sol, &usdc, 10, 10_000);
        assert_ne!(a, v2);
    }

    #[test]
    fn usd_per_mint_dlmm_bin_path() {
        let sol = Pubkey::from_str(stables::WSOL).unwrap();
        let usdc = Pubkey::from_str(stables::USDC).unwrap();
        // active_id such that (1+10/10000)^id * 10^(9-6) ≈ round number is hard;
        // just ensure finite positive for a mid bin.
        let pool = PoolStats {
            address: Pubkey::new_unique(),
            token_mint_a: sol,
            token_mint_b: usdc,
            token_vault_a: Pubkey::new_unique(),
            token_vault_b: Pubkey::new_unique(),
            tick_spacing: 10,
            fee_rate: 0,
            liquidity: 1,
            sqrt_price: 0,
            tick_current_index: 0,
            index_space: IndexSpace::Bin { bin_step: 10 },
            fee_growth_global_a: 0,
            fee_growth_global_b: 0,
            protocol_fee_owed_a: 0,
            protocol_fee_owed_b: 0,
        };
        let usd = usd_per_mint_from_pool(&pool, &sol, 9, &usdc, 6).unwrap();
        // bin 0 → raw 1 → human * 10^3 = 1000 USDC per SOL at parity bin.
        assert!((usd - 1000.0).abs() < 1e-6);
    }

    #[test]
    fn price_mode_cycle() {
        assert_eq!(PriceMode::OnChain.next(), PriceMode::Jupiter);
        assert_eq!(PriceMode::Jupiter.next(), PriceMode::Titan);
        assert_eq!(PriceMode::Titan.next(), PriceMode::OnChain);
        assert_eq!(PriceMode::from_setting("titan"), PriceMode::Titan);
    }

    #[test]
    fn human_price_scale() {
        let sp = 1u128 << 64;
        let p = human_price_b_per_a(sp, 9, 6).unwrap();
        assert!((p - 1000.0).abs() < 1e-6);
    }

    #[test]
    #[ignore = "needs mainnet RPC"]
    fn live_sol_usdc_multi_venue_hits() {
        use crate::rpc::{self, Rpc, RpcMeter};
        let url = std::env::var("SOLANA_RPC_URL")
            .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".into());
        let client = rpc::connect(&url);
        let meter = RpcMeter::default();
        let rpc = Rpc::new(&client, &meter);
        let sol = Pubkey::from_str(stables::WSOL).unwrap();
        let usdc = Pubkey::from_str(stables::USDC).unwrap();
        let px = price_in_pool_quote(&rpc, &sol, 9, &usdc, 6);
        eprintln!("SOL/USDC on-chain ≈ {:?}", px);
        assert!(px.is_some());
        let p = px.unwrap();
        assert!(p > 50.0 && p < 1000.0, "implausible SOL price {p}");
    }
}
