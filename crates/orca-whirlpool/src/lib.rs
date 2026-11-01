//! Unofficial Orca Whirlpools CLMM reads (no official SDK dependency).
//!
//! Not affiliated with Orca. Layouts/math from public on-chain data.
//! Depends on [`sol_clmm_manager`] for RPC metering, SQLite, tokens, and quotes.

#![forbid(unsafe_code)]

mod amounts;
pub mod decode;
mod detail;
mod list;
mod value;
#[cfg(feature = "write")]
pub mod write;

pub use amounts::{amounts_from_liquidity, format_amount, format_price, tick_to_price};
pub use detail::{LiquiditySample, TokenSide, fetch_pool_preview, fetch_position_detail};
pub use list::list_positions;
pub use value::{
    PriceApiKeys, PriceMode, PriceSource, TokenUsd, UsdEstimate, estimate_position_usd, format_usd,
    format_usd_axis, human_price_b_per_a, liquidity_l_to_usd, whirlpool_pda,
};

pub use sol_clmm_manager::{
    PoolStats, PositionDetail, PositionRow, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID, TokenInfo,
    USDC, USDT, Venue, WHIRLPOOLS_CONFIG, WSOL, is_stablecoin, resolve_mints, resolve_tokens,
    solscan_account_url,
};

use solana_pubkey::Pubkey;

pub const WHIRLPOOL_PROGRAM_ID: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";

pub fn whirlpool_program_id() -> Pubkey {
    WHIRLPOOL_PROGRAM_ID.parse().expect("WHIRLPOOL_PROGRAM_ID")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_pda_golden() {
        let program = whirlpool_program_id();
        let mint: Pubkey = "7skBPfWRiJmVdZhLEPqjM5qMdYUFMnGYFkRqhFmpbGUd"
            .parse()
            .unwrap();
        let (pda, bump) = Pubkey::find_program_address(&[b"position", mint.as_ref()], &program);
        assert_eq!(
            pda.to_string(),
            "7p7DiHYU6eydysvQp8McpyohyG6G19a59zsEhSwhJzN7"
        );
        assert_eq!(bump, 253);
    }
}
