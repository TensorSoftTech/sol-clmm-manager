//! Shared detail / chart types across venues.

use crate::pool::PoolStats;
use crate::position::PositionRow;
use crate::pricing::UsdEstimate;
use crate::snapshots::PoolPeriodReport;
use crate::tokens::TokenInfo;
use solana_pubkey::Pubkey;

/// Which token(s) a graph bar represents for *this position*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSide {
    Outside,
    A,
    B,
    Both,
}

#[derive(Debug, Clone, Copy)]
pub struct LiquiditySample {
    pub tick: i32,
    pub liquidity: u64,
    pub side: TokenSide,
    pub in_position: bool,
}

/// One reward stream (claim-simulated).
#[derive(Debug, Clone)]
pub struct RewardInfo {
    pub mint: Pubkey,
    pub symbol: String,
    pub decimals: u8,
    pub amount_raw: u64,
    pub amount_ui: String,
    pub amount_usd: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct PositionDetail {
    pub row: PositionRow,
    pub pool: PoolStats,
    pub token_a: TokenInfo,
    pub token_b: TokenInfo,
    pub position_liquidity: u128,
    pub amount_a: u128,
    pub amount_b: u128,
    pub amount_a_ui: String,
    pub amount_b_ui: String,
    /// Claim-simulated uncollected fees (raw).
    pub fee_a: u64,
    pub fee_b: u64,
    /// Human + USD, e.g. `0.12 USDC (≈ $0.12)`.
    pub fee_a_ui: String,
    pub fee_b_ui: String,
    pub fee_a_usd: Option<f64>,
    pub fee_b_usd: Option<f64>,
    pub fees_usd: Option<f64>,
    pub rewards: Vec<RewardInfo>,
    pub rewards_usd: Option<f64>,
    pub in_range: bool,
    pub series: Vec<LiquiditySample>,
    pub series_min_tick: i32,
    pub series_max_tick: i32,
    pub series_max_liquidity: u64,
    pub initialized_ticks: usize,
    pub arrays_loaded: usize,
    pub usd: Option<UsdEstimate>,
    pub vault_a_raw: Option<u64>,
    pub vault_b_raw: Option<u64>,
    pub vault_a_ui: String,
    pub vault_b_ui: String,
    /// Pool fee/volume estimate since previous SQLite snapshot (if any).
    pub period: Option<PoolPeriodReport>,
}
