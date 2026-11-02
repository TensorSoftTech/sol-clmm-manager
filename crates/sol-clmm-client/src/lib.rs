//! Multi-venue read API for agents and the TUI.
//!
//! Depends on [`sol_clmm_manager`] + protocol crates. Prefer this over
//! re-implementing Orca/Raydium/Meteora merge in each binary.

#![forbid(unsafe_code)]

#[cfg(feature = "write")]
mod write_api;

#[cfg(feature = "write")]
pub use write_api::write_session_ok;

use anyhow::Result;
use sol_clmm_manager::enrich_rows;
use sol_clmm_manager::rpc::{self, Rpc, RpcMeter, RpcStatus};
use sol_clmm_manager::store::Store;
use solana_pubkey::Pubkey;

pub use sol_clmm_manager::{
    PoolPreview, PositionDetail, PositionRow, PriceApiKeys, PriceMode, Venue, VenueFilter,
};

/// Session knobs for list enrich / detail USD (not the Solana RPC handle).
#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub price_mode: PriceMode,
    pub price_keys: PriceApiKeys,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            price_mode: PriceMode::OnChain,
            price_keys: PriceApiKeys::default(),
        }
    }
}

/// Thin facade: multi-venue list + detail + enrich.
#[derive(Debug, Clone)]
pub struct Client {
    pub config: ClientConfig,
}

impl Client {
    pub fn new(price_mode: PriceMode, price_keys: PriceApiKeys) -> Self {
        Self {
            config: ClientConfig {
                price_mode,
                price_keys,
            },
        }
    }

    pub fn with_config(config: ClientConfig) -> Self {
        Self { config }
    }

    /// Merge venue lists (soft-fail per venue), optionally filter, then enrich USD/TVL + snapshots.
    pub fn list_positions(
        &self,
        rpc: &Rpc<'_>,
        wallet: &Pubkey,
        store: Option<&Store>,
        filter: VenueFilter,
        refresh_meta: bool,
    ) -> Result<Vec<PositionRow>> {
        let mut rows = list_all_venues(rpc, wallet, store, refresh_meta)?;
        if filter != VenueFilter::All {
            rows.retain(|r| filter.matches(r.venue));
        }
        enrich_rows(
            rpc,
            store,
            &mut rows,
            self.config.price_mode,
            &self.config.price_keys,
        )?;
        Ok(rows)
    }

    /// Same as [`Self::list_positions`] but enrich errors are ignored (TUI default).
    pub fn list_positions_lenient(
        &self,
        rpc: &Rpc<'_>,
        wallet: &Pubkey,
        store: Option<&Store>,
        filter: VenueFilter,
        refresh_meta: bool,
    ) -> Result<Vec<PositionRow>> {
        let mut rows = list_all_venues(rpc, wallet, store, refresh_meta)?;
        if filter != VenueFilter::All {
            rows.retain(|r| filter.matches(r.venue));
        }
        let _ = enrich_rows(
            rpc,
            store,
            &mut rows,
            self.config.price_mode,
            &self.config.price_keys,
        );
        Ok(rows)
    }

    /// Fetch detail for one row (dispatches on [`PositionRow::venue`]).
    /// When `wallet` is set, also refreshes ATA + native SOL balances (needed for SOL pairs).
    pub fn position_detail(
        &self,
        rpc: &Rpc<'_>,
        row: &PositionRow,
        store: Option<&Store>,
        refresh_meta: bool,
        wallet: Option<&Pubkey>,
    ) -> Result<PositionDetail> {
        let mut detail = match row.venue {
            Venue::OrcaWhirlpool => orca_whirlpool::fetch_position_detail(
                rpc,
                row,
                store,
                self.config.price_mode,
                &self.config.price_keys,
                refresh_meta,
            )?,
            Venue::RaydiumClmm => raydium_clmm::fetch_position_detail(
                rpc,
                row,
                store,
                self.config.price_mode,
                &self.config.price_keys,
                refresh_meta,
            )?,
            Venue::MeteoraDlmm => meteora_dlmm::fetch_position_detail(
                rpc,
                row,
                store,
                self.config.price_mode,
                &self.config.price_keys,
                refresh_meta,
            )?,
        };
        if let Some(w) = wallet {
            detail.wallet_balances = sol_clmm_manager::fetch_wallet_pool_balances(
                rpc,
                w,
                detail.token_a.mint,
                detail.token_b.mint,
            )
            .ok();
        }
        Ok(detail)
    }

    /// Fetch pool + liquidity series for create-wizard range picking.
    /// Resolves mint symbols via SQLite cache → Metaplex → Jupiter Tokens.
    /// When `wallet` is set, also loads ATA (+ native SOL) balances for deposit caps.
    pub fn pool_preview(
        &self,
        rpc: &Rpc<'_>,
        venue: Venue,
        pool: &Pubkey,
        wallet: Option<&Pubkey>,
        store: Option<&Store>,
        refresh_meta: bool,
    ) -> Result<PoolPreview> {
        let keys = &self.config.price_keys;
        match venue {
            Venue::OrcaWhirlpool => {
                orca_whirlpool::fetch_pool_preview(rpc, *pool, wallet, store, refresh_meta, keys)
            }
            Venue::RaydiumClmm => {
                raydium_clmm::fetch_pool_preview(rpc, *pool, wallet, store, refresh_meta, keys)
            }
            Venue::MeteoraDlmm => {
                meteora_dlmm::fetch_pool_preview(rpc, *pool, wallet, store, refresh_meta, keys)
            }
        }
    }

    /// Re-run USD / TVL / snapshot enrich on existing rows.
    pub fn enrich_list(
        &self,
        rpc: &Rpc<'_>,
        store: Option<&Store>,
        rows: &mut [PositionRow],
    ) -> Result<()> {
        enrich_rows(
            rpc,
            store,
            rows,
            self.config.price_mode,
            &self.config.price_keys,
        )
    }
}

/// Convenience: connect RPC, fetch status + positions (matches TUI list thread).
pub fn fetch_list(
    rpc_url: &str,
    wallet: Option<&Pubkey>,
    store: Option<&Store>,
    filter: VenueFilter,
    refresh_meta: bool,
    price_mode: PriceMode,
    price_keys: &PriceApiKeys,
) -> (Result<RpcStatus>, Result<Vec<PositionRow>>, RpcMeter) {
    let sol_client = rpc::connect(rpc_url);
    let meter = RpcMeter::default();
    let rpc = Rpc::new(&sol_client, &meter);
    let status = rpc::fetch_status(&rpc, wallet);
    let client = Client::new(price_mode, price_keys.clone());
    let positions = match wallet {
        Some(pk) => client.list_positions_lenient(&rpc, pk, store, filter, refresh_meta),
        None => Ok(Vec::new()),
    };
    (status, positions, meter)
}

/// Merge Orca + Raydium + Meteora. Soft-fail per venue. No enrich.
pub fn list_all_venues(
    rpc: &Rpc<'_>,
    wallet: &Pubkey,
    store: Option<&Store>,
    refresh_meta: bool,
) -> Result<Vec<PositionRow>> {
    let mut rows = Vec::new();
    let mut errors = Vec::new();

    match orca_whirlpool::list_positions(rpc, wallet, store, refresh_meta) {
        Ok(mut r) => rows.append(&mut r),
        Err(e) => errors.push(format!("orca: {e:#}")),
    }
    match raydium_clmm::list_positions(rpc, wallet, store, refresh_meta) {
        Ok(mut r) => rows.append(&mut r),
        Err(e) => errors.push(format!("raydium: {e:#}")),
    }
    match meteora_dlmm::list_positions(rpc, wallet, store, refresh_meta) {
        Ok(mut r) => rows.append(&mut r),
        Err(e) => errors.push(format!("meteora: {e:#}")),
    }

    rows.sort_by(|a, b| {
        a.venue
            .cmp(&b.venue)
            .then(a.pool_label.cmp(&b.pool_label))
            .then(a.address.to_string().cmp(&b.address.to_string()))
    });

    if rows.is_empty() && !errors.is_empty() {
        anyhow::bail!("{}", errors.join(" · "));
    }
    Ok(rows)
}
