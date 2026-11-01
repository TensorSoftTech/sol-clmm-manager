//! Unofficial Raydium CLMM reads (list / detail / amounts / ≈ USD).
//!
//! Not affiliated with Raydium. Layouts from public on-chain account data.

#![forbid(unsafe_code)]

pub mod decode;
mod detail;
mod list;
#[cfg(feature = "write")]
pub mod write;

pub use detail::{fetch_pool_preview, fetch_position_detail};
pub use list::list_positions;

pub use sol_clmm_manager::{
    LiquiditySample, PoolStats, PositionDetail, PositionRow, PriceApiKeys, PriceMode, TokenSide,
    Venue, amounts_from_liquidity, format_amount, format_price, format_usd, format_usd_axis,
    liquidity_l_to_usd, tick_to_price,
};

use solana_pubkey::Pubkey;

/// Raydium CLMM program id (mainnet).
pub const CLMM_PROGRAM_ID: &str = "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK";

pub fn venue() -> Venue {
    Venue::RaydiumClmm
}

pub fn clmm_program_id() -> Pubkey {
    CLMM_PROGRAM_ID.parse().expect("CLMM_PROGRAM_ID")
}

pub fn status() -> &'static str {
    "list + detail · Raydium CLMM"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_pda_seed() {
        let program = clmm_program_id();
        let mint = Pubkey::new_unique();
        let (pda, _) = Pubkey::find_program_address(&[b"position", mint.as_ref()], &program);
        assert_ne!(pda, mint);
    }
}
