//! Shared list-row type across Orca / Raydium / Meteora.

use crate::Venue;
use solana_pubkey::Pubkey;

/// One concentrated-liquidity position row (list view).
#[derive(Debug, Clone)]
pub struct PositionRow {
    pub venue: Venue,
    pub address: Pubkey,
    /// Pool / whirlpool / lb-pair account.
    pub pool: Pubkey,
    pub pool_label: String,
    /// Tick (CLMM) or bin id (DLMM) lower bound.
    pub tick_lower: i32,
    /// Tick (CLMM) or bin id (DLMM) upper bound.
    pub tick_upper: i32,
    pub liquidity: u128,
    /// Human fee strings (may include USD after enrich).
    pub fees_a: String,
    pub fees_b: String,
    /// Token mints (for enrich / detail reuse).
    pub mint_a: Pubkey,
    pub mint_b: Pubkey,
    pub decimals_a: u8,
    pub decimals_b: u8,
    pub symbol_a: String,
    pub symbol_b: String,
    /// Raw amounts / fees for USD.
    pub amount_a: u128,
    pub amount_b: u128,
    pub fee_a: u64,
    pub fee_b: u64,
    /// Pool snapshot inputs (0 if unknown).
    pub sqrt_price: u128,
    pub tick_current: i32,
    /// CLMM tick spacing or DLMM bin step (for CurrentPool USD).
    pub tick_spacing: u16,
    pub fee_rate: u32,
    pub fee_growth_global_a: u128,
    pub fee_growth_global_b: u128,
    pub protocol_fee_owed_a: u64,
    pub protocol_fee_owed_b: u64,
    pub pool_liquidity: u128,
    pub vault_a: u64,
    pub vault_b: u64,
    /// Filled by [`crate::list_enrich::enrich_rows`].
    pub position_usd: Option<f64>,
    pub pool_tvl_usd: Option<f64>,
    pub fees_usd: Option<f64>,
    pub amount_ui: String,
    pub fees_ui: String,
}

impl PositionRow {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        venue: Venue,
        address: Pubkey,
        pool: Pubkey,
        pool_label: String,
        tick_lower: i32,
        tick_upper: i32,
        liquidity: u128,
        fees_a: String,
        fees_b: String,
    ) -> Self {
        Self {
            venue,
            address,
            pool,
            pool_label,
            tick_lower,
            tick_upper,
            liquidity,
            fees_a,
            fees_b,
            mint_a: Pubkey::default(),
            mint_b: Pubkey::default(),
            decimals_a: 0,
            decimals_b: 0,
            symbol_a: String::new(),
            symbol_b: String::new(),
            amount_a: 0,
            amount_b: 0,
            fee_a: 0,
            fee_b: 0,
            sqrt_price: 0,
            tick_current: 0,
            tick_spacing: 0,
            fee_rate: 0,
            fee_growth_global_a: 0,
            fee_growth_global_b: 0,
            protocol_fee_owed_a: 0,
            protocol_fee_owed_b: 0,
            pool_liquidity: 0,
            vault_a: 0,
            vault_b: 0,
            position_usd: None,
            pool_tvl_usd: None,
            fees_usd: None,
            amount_ui: String::new(),
            fees_ui: String::new(),
        }
    }
}
