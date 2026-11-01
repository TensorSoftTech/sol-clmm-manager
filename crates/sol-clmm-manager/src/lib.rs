//! Shared foundation for Solana concentrated-liquidity tools.
//!
//! Used by protocol crates ([`orca_whirlpool`], `raydium-clmm`, `meteora-dlmm`)
//! and by the TUI / future agent binaries.
//!
//! - [`rpc`] — traced Solana RPC + Jupiter/Titan call meter
//! - [`store`] — SQLite settings + token metadata cache
//! - [`tokens`] — symbol / decimals resolution (known · Metaplex · Jupiter Tokens)
//! - [`stables`] — common mint constants
//! - [`quotes`] — Jupiter Price + Titan Portal HTTP helpers

#![forbid(unsafe_code)]

pub mod clmm_math;
pub mod detail;
pub mod fees;
pub mod list_enrich;
pub mod pool;
pub mod position;
pub mod pricing;
pub mod quotes;
pub mod rpc;
pub mod snapshots;
pub mod stables;
pub mod store;
pub mod tokens;
pub mod venue;

pub use clmm_math::{
    amounts_from_liquidity, build_liquidity_series, format_amount, format_price,
    get_sqrt_price_at_tick, read_token_account_amount, scale_u128, tick_to_price, TickPoint,
};
pub use detail::{LiquiditySample, PositionDetail, RewardInfo, TokenSide};
pub use fees::{
    collect_fees_quote, fee_growth_inside, format_amount_usd, tokens_owed_delta,
};
pub use list_enrich::enrich_rows;
pub use pool::{IndexSpace, PoolStats};
pub use position::PositionRow;
pub use pricing::{
    estimate_position_usd, estimate_position_usd_with_extra_mints, fetch_mint_usd_prices,
    format_usd, format_usd_axis, human_price_b_per_a, liquidity_l_to_usd, meteora_lb_pair_pda_v1,
    meteora_lb_pair_pda_v2, raw_to_usd, raydium_amm_config_pda, raydium_pool_pda, whirlpool_pda,
    PriceApiKeys, PriceMode, PriceSource, TokenUsd, UsdEstimate,
};
pub use snapshots::{PoolPeriodReport, PoolSnapshotInput};
pub use quotes::{
    jupiter::{self, fetch_token_meta, fetch_usd_prices as jupiter_usd_prices, TokenMeta},
    titan::{self, fetch_usd_prices_with_tier as titan_usd_prices, TITAN_PORTAL_QUOTE},
};
pub use rpc::{
    connect, fetch_status, is_public_rpc, lamports_to_sol, owned_nft_mints, Rpc, RpcCall, RpcMeter,
    RpcStatus, DEFAULT_RPC_URL, RPC_HISTORY_CAP, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID,
};
pub use stables::{
    is_stablecoin, stablecoin_mints, wsol_mint, PRICE_TICK_SPACINGS, PYUSD, USDC, USDE, USDT, USDY,
    WHIRLPOOLS_CONFIG, WSOL,
};
pub use store::{
    default_config_dir, default_db_path, display_path, expand_tilde, resolve_existing_db,
    AppSettings, CachedToken, Store, APP_DIR_NAME, DEFAULT_DB_FILE,
};
pub use tokens::{resolve_mints, resolve_tokens, TokenInfo};
pub use venue::{Venue, VenueFilter};

/// Solscan account URL (mainnet).
pub fn solscan_account_url(pubkey: &solana_pubkey::Pubkey) -> String {
    format!("https://solscan.io/account/{pubkey}")
}
