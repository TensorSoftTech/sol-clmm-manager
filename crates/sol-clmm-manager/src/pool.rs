//! Venue-agnostic pool snapshot used by detail / USD / charts.

use solana_pubkey::Pubkey;

/// How the pool indexes price (ticks vs DLMM bins).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IndexSpace {
    #[default]
    Tick,
    /// Meteora-style bins; `bin_step` is in basis points of price step.
    Bin { bin_step: u16 },
}

impl IndexSpace {
    pub fn price_at(self, index: i32) -> f64 {
        match self {
            Self::Tick => 1.0001_f64.powf(index as f64),
            Self::Bin { bin_step } => {
                let step = 1.0 + (bin_step as f64) / 10_000.0;
                if step <= 0.0 {
                    return f64::NAN;
                }
                step.powf(index as f64)
            }
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Tick => "ticks",
            Self::Bin { .. } => "bins",
        }
    }

    pub fn arrays_label(self) -> &'static str {
        match self {
            Self::Tick => "tick arrays",
            Self::Bin { .. } => "bin arrays",
        }
    }
}

/// Shared pool fields for detail UI (Orca / Raydium / Meteora).
#[derive(Debug, Clone)]
pub struct PoolStats {
    pub address: Pubkey,
    pub token_mint_a: Pubkey,
    pub token_mint_b: Pubkey,
    pub token_vault_a: Pubkey,
    pub token_vault_b: Pubkey,
    /// Tick spacing (CLMM) or bin step (DLMM).
    pub tick_spacing: u16,
    /// Fee in hundredths of a bip (same display contract as Orca: `/100` → bps).
    pub fee_rate: u32,
    pub liquidity: u128,
    /// Q64.64 √P for CLMM; 0 for DLMM (amounts precomputed).
    pub sqrt_price: u128,
    pub tick_current_index: i32,
    pub index_space: IndexSpace,
    /// Q64.64 cumulative fee growth (for claim-sim + snapshots).
    pub fee_growth_global_a: u128,
    pub fee_growth_global_b: u128,
    pub protocol_fee_owed_a: u64,
    pub protocol_fee_owed_b: u64,
}
