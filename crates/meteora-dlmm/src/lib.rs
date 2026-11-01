//! Unofficial Meteora DLMM reads (list / detail / amounts / ≈ USD).
//!
//! Not affiliated with Meteora. Layouts from public on-chain account data.

#![forbid(unsafe_code)]

mod decode;
mod detail;
mod list;

pub use detail::fetch_position_detail;
pub use list::list_positions;

pub use sol_clmm_manager::{
    format_amount, format_price, format_usd, format_usd_axis, liquidity_l_to_usd, LiquiditySample,
    PoolStats, PositionDetail, PositionRow, PriceApiKeys, PriceMode, TokenSide, Venue,
};

use solana_pubkey::Pubkey;

/// Meteora DLMM program id (mainnet).
pub const DLMM_PROGRAM_ID: &str = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo";

pub fn venue() -> Venue {
    Venue::MeteoraDlmm
}

pub fn dlmm_program_id() -> Pubkey {
    DLMM_PROGRAM_ID.parse().expect("DLMM_PROGRAM_ID")
}

pub fn status() -> &'static str {
    "list + detail · Meteora DLMM"
}

/// Bin price: `(1 + bin_step/10000)^bin_id`.
pub fn bin_to_price(bin_id: i32, bin_step: u16) -> f64 {
    sol_clmm_manager::IndexSpace::Bin { bin_step }.price_at(bin_id)
}
