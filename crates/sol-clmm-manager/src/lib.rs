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
pub mod wallet;

#[cfg(feature = "write")]
pub mod write;

pub use clmm_math::{
    TickPoint, amounts_from_liquidity, build_liquidity_series, format_amount, format_price,
    get_sqrt_price_at_tick, parse_ui_amount, read_token_account_amount, scale_u128, tick_to_price,
};
pub use detail::{
    CHART_FETCH_HALF_SPACINGS, CHART_HALF_SPACINGS_DEFAULT, CHART_HALF_SPACINGS_MIN,
    LiquiditySample, PoolPreview, PositionDetail, RewardInfo, TokenSide, chart_view_window,
    slice_liquidity_series,
};
pub use fees::{collect_fees_quote, fee_growth_inside, format_amount_usd, tokens_owed_delta};
pub use list_enrich::enrich_rows;
pub use pool::{IndexSpace, PoolStats};
pub use position::PositionRow;
pub use pricing::{
    PriceApiKeys, PriceMode, PriceSource, TokenUsd, UsdEstimate, estimate_position_usd,
    estimate_position_usd_with_extra_mints, fetch_mint_usd_prices, format_usd, format_usd_axis,
    human_price_b_per_a, liquidity_l_to_usd, meteora_lb_pair_pda_v1, meteora_lb_pair_pda_v2,
    raw_to_usd, raydium_amm_config_pda, raydium_pool_pda, whirlpool_pda,
};
pub use quotes::{
    jupiter::{self, TokenMeta, fetch_token_meta, fetch_usd_prices as jupiter_usd_prices},
    titan::{self, TITAN_PORTAL_QUOTE, fetch_usd_prices_with_tier as titan_usd_prices},
};
pub use rpc::{
    DEFAULT_RPC_URL, RPC_HISTORY_CAP, Rpc, RpcCall, RpcMeter, RpcStatus, TOKEN_2022_PROGRAM_ID,
    TOKEN_PROGRAM_ID, connect, fetch_status, is_public_rpc, lamports_to_sol, owned_nft_mints,
};
pub use snapshots::{PoolPeriodReport, PoolSnapshotInput};
pub use stables::{
    PRICE_TICK_SPACINGS, PYUSD, USDC, USDE, USDT, USDY, WHIRLPOOLS_CONFIG, WSOL, is_stablecoin,
    stablecoin_mints, wsol_mint,
};
pub use store::{
    APP_DIR_NAME, AppSettings, CachedToken, DEFAULT_DB_FILE, Store, WriteTokenDelta, WriteTxRow,
    default_config_dir, default_db_path, display_path, expand_tilde, resolve_existing_db,
};
pub use tokens::{
    TokenInfo, resolve_mints, resolve_mints_with_keys, resolve_tokens, resolve_tokens_with_keys,
};
pub use venue::{Venue, VenueFilter};
pub use wallet::{
    SOL_FEE_RESERVE_LAMPORTS, SideBalance, WalletPoolBalances, ata_address,
    fetch_wallet_pool_balances,
};

#[cfg(feature = "write")]
pub use write::{
    BinWeights, BudgetMode, CompoundFeePlan, CreatePositionParams, DepositAnalysis, DepositShape,
    DepositSizing, DlmmStrategy, ExpectedDeltas, LiquidityDeltaPlan, PreparedTx, RangeDepositSide,
    SendOutcome, SolLegMode, SwapMode, SwapPlan, SwapQuote, SwapToMatchHint, TxBudget, TxBuildError,
    WriteOp, WritePlan, amounts_for_liquidity_delta, analyze_deposit, compound_fee_plan,
    deposit_ratio_planner, distribute_bins, effective_deposit_shape, estimate_budget,
    estimate_budget_signers, fetch_swap_instructions, is_wsol, create_ata_idempotent_ix,
    dedupe_ata_create_idempotent, ensure_atas_ixs, ensure_pool_atas_ixs, liquidity_delta_for_open,
    liquidity_delta_from_amounts, maybe_unwrap_after, maybe_wrap_before_deposit, plan_unwrap_sol,
    plan_wrap_sol, pool_has_wsol, range_deposit_side, swap_to_match_hint,
    prepare_estimate_simulate_send, prepare_estimate_simulate_send_signers, prepare_v1_tx,
    prepare_v1_tx_signers, simulate_and_send_v1, simulate_v1_tx, unwrap_sol_ix, wrap_sol_ixs,
    wsol_ata,
};

/// Solscan account URL (mainnet).
pub fn solscan_account_url(pubkey: &solana_pubkey::Pubkey) -> String {
    format!("https://solscan.io/account/{pubkey}")
}

/// Solscan transaction URL (mainnet).
pub fn solscan_tx_url(signature: &str) -> String {
    format!("https://solscan.io/tx/{signature}")
}

/// `true` when this binary was compiled with the `write` feature.
pub const WRITE_ENABLED: bool = cfg!(feature = "write");
