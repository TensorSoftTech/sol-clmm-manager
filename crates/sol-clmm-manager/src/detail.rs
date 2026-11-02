//! Shared detail / chart types across venues.

use crate::pool::PoolStats;
use crate::position::PositionRow;
use crate::pricing::UsdEstimate;
use crate::snapshots::PoolPeriodReport;
use crate::tokens::TokenInfo;
use crate::venue::Venue;
use crate::wallet::WalletPoolBalances;
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
    /// Position NFT mint (Orca / Raydium). `None` for Meteora DLMM.
    pub position_mint: Option<Pubkey>,
    /// Token program that owns the position NFT mint (Token or Token-2022).
    /// Raydium: either (legacy `OpenPositionV2` vs `OpenPositionWithToken22Nft`).
    /// Orca: classic Token. `None` for Meteora.
    pub position_nft_token_program: Option<Pubkey>,
    /// Claimable pool rewards: `(reward_index, mint, vault)`.
    /// Empty, inactive, and ended slots omitted (no reward ATA CreateIdempotent).
    /// Included when actively emitting, or inactive/ended with owed > 0.
    pub reward_vaults: Vec<(u8, Pubkey, Pubkey)>,
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
    /// Wallet ATA + native SOL for this pool's mints (refreshed with detail).
    /// Critical for SOL pairs: wrap/unwrap changes native vs WSOL split.
    pub wallet_balances: Option<WalletPoolBalances>,
}

/// Pool chart preview for create-position range picking (no position account).
#[derive(Debug, Clone)]
pub struct PoolPreview {
    pub venue: Venue,
    pub pool: PoolStats,
    pub token_a: TokenInfo,
    pub token_b: TokenInfo,
    /// Wallet balances for mint A/B (+ native SOL). `None` if no wallet / fetch failed.
    pub balances: Option<WalletPoolBalances>,
    /// Draft range (aligned to spacing / 1 bin).
    pub tick_lower: i32,
    pub tick_upper: i32,
    pub series: Vec<LiquiditySample>,
    pub series_max_liquidity: u64,
    /// Fetched / build window (not just samples with depth) — drives zoom-out max.
    pub series_min_tick: i32,
    pub series_max_tick: i32,
    pub arrays_loaded: usize,
    pub initialized_ticks: usize,
    /// Chart X zoom: visible ±N tick-spacings around current.
    /// Larger = zoomed out. SOL/USDC pools need zoom-out to see wide bands.
    pub chart_half_spacings: i32,
}

/// Start wide: use full fetched window (see [`PoolPreview::zoom_full`]).
/// Kept as a large sentinel so previews init before `series_min/max` are known.
pub const CHART_HALF_SPACINGS_DEFAULT: i32 = 10_000;
/// Tightest zoom (± spacings) for fine tick picking.
pub const CHART_HALF_SPACINGS_MIN: i32 = 4;
/// Create-wizard tick-array fetch width (± spacings) — the zoom-out ceiling.
pub const CHART_FETCH_HALF_SPACINGS: i32 = 512;

impl PoolPreview {
    /// Align an index down to `spacing` (CLMM) or leave as-is for 1-bin DLMM.
    pub fn align_index(index: i32, spacing: u16) -> i32 {
        let s = spacing.max(1) as i32;
        index.div_euclid(s) * s
    }

    /// Default draft window around current: ±`widths` spacings (min width 2).
    pub fn default_range(current: i32, spacing: u16, widths: i32) -> (i32, i32) {
        let step = spacing.max(1) as i32;
        let cur = Self::align_index(current, spacing);
        let w = widths.max(2) * step;
        (cur - w, cur + w)
    }

    /// Series tick extent (fetch window; empty → current).
    pub fn series_extent(&self) -> (i32, i32) {
        if self.series_min_tick < self.series_max_tick {
            (self.series_min_tick, self.series_max_tick)
        } else {
            match (self.series.first(), self.series.last()) {
                (Some(a), Some(b)) => (a.tick, b.tick),
                _ => (self.pool.tick_current_index, self.pool.tick_current_index),
            }
        }
    }

    /// Visible chart window for current zoom (± half spacings around spot).
    pub fn chart_view_window(&self) -> (i32, i32) {
        chart_view_window(
            self.pool.tick_current_index,
            self.pool.tick_spacing,
            self.effective_half_spacings(),
            self.series_extent(),
        )
    }

    /// Slice series to the zoomed window (for drawing).
    pub fn chart_series_view(&self) -> (Vec<LiquiditySample>, u64) {
        let (lo, hi) = self.chart_view_window();
        slice_liquidity_series(&self.series, lo, hi)
    }

    pub fn max_chart_half(&self) -> i32 {
        let (smin, smax) = self.series_extent();
        let step = self.pool.tick_spacing.max(1) as i32;
        let span = (smax - smin).max(step);
        (span / step / 2).max(CHART_HALF_SPACINGS_MIN)
    }

    /// Clamp zoom to the fetched window (call after series bounds are set).
    pub fn zoom_full(&mut self) {
        self.chart_half_spacings = self.max_chart_half();
    }

    /// Effective half-spacings (sentinel DEFAULT → full window).
    pub fn effective_half_spacings(&self) -> i32 {
        self.chart_half_spacings
            .min(self.max_chart_half())
            .max(CHART_HALF_SPACINGS_MIN)
    }

    /// `true` if zoom changed. Geometric step (~⅔) so wide→tight has many useful levels.
    pub fn zoom_in(&mut self) -> bool {
        let cur = self.effective_half_spacings();
        let next = ((cur * 2) / 3).max(CHART_HALF_SPACINGS_MIN);
        // Guarantee progress when *2/3 stalls on small ints (e.g. 5→3→4 clamp).
        let next = if next >= cur {
            (cur - 1).max(CHART_HALF_SPACINGS_MIN)
        } else {
            next
        };
        if next == cur {
            return false;
        }
        self.chart_half_spacings = next;
        true
    }

    /// `true` if zoom changed. Geometric step (~×1.5) toward full fetch.
    pub fn zoom_out(&mut self) -> bool {
        let cur = self.effective_half_spacings();
        let max = self.max_chart_half();
        let next = (cur.saturating_mul(3) / 2).max(cur + 1).min(max);
        if next == cur {
            return false;
        }
        self.chart_half_spacings = next;
        true
    }

    /// Set zoom so the draft range fits with ~25% pad (tighter than full fetch).
    pub fn zoom_fit_range(&mut self) {
        let step = self.pool.tick_spacing.max(1) as i32;
        let cur = self.pool.tick_current_index;
        let dist = (self.tick_lower - cur)
            .abs()
            .max((self.tick_upper - cur).abs())
            .max(step);
        // 25% pad beyond the farther bound.
        let need = ((dist * 5) / (step * 4) + 2).max(CHART_HALF_SPACINGS_MIN);
        self.chart_half_spacings = need.min(self.max_chart_half());
    }

    /// If draft bounds fall outside the window, widen just enough (never shrink).
    pub fn ensure_range_visible(&mut self) {
        let step = self.pool.tick_spacing.max(1) as i32;
        let cur_tick = self.pool.tick_current_index;
        let half = self.effective_half_spacings() * step;
        let mut need = self.effective_half_spacings();
        if self.tick_lower < cur_tick - half {
            need = need.max((cur_tick - self.tick_lower) / step + 2);
        }
        if self.tick_upper > cur_tick + half {
            need = need.max((self.tick_upper - cur_tick) / step + 2);
        }
        self.chart_half_spacings = need.min(self.max_chart_half());
    }

    /// Recolor series bars for the draft range (in-range A/B/Both · outside dim).
    pub fn apply_range(&mut self, lower: i32, upper: i32) {
        if lower >= upper {
            return;
        }
        self.tick_lower = lower;
        self.tick_upper = upper;
        let current = self.pool.tick_current_index;
        for s in &mut self.series {
            let in_position = s.tick >= lower && s.tick <= upper;
            s.in_position = in_position;
            s.side = if !in_position {
                TokenSide::Outside
            } else if s.tick < current {
                TokenSide::A
            } else if s.tick > current {
                TokenSide::B
            } else {
                TokenSide::Both
            };
        }
        // Keep ▌▐ on-screen when presets/nudges widen past the zoom window.
        self.ensure_range_visible();
    }

    pub fn nudge_lower(&mut self, delta_steps: i32) {
        let step = self.pool.tick_spacing.max(1) as i32;
        let mut lo = self.tick_lower + delta_steps * step;
        let hi = self.tick_upper;
        // Keep at least one spacing of width.
        if lo >= hi {
            lo = hi - step;
        }
        self.apply_range(lo, hi);
    }

    pub fn nudge_upper(&mut self, delta_steps: i32) {
        let step = self.pool.tick_spacing.max(1) as i32;
        let lo = self.tick_lower;
        let mut hi = self.tick_upper + delta_steps * step;
        if hi <= lo {
            hi = lo + step;
        }
        self.apply_range(lo, hi);
    }

    pub fn shift_range(&mut self, delta_steps: i32) {
        let step = self.pool.tick_spacing.max(1) as i32;
        let d = delta_steps * step;
        self.apply_range(self.tick_lower + d, self.tick_upper + d);
    }
}

/// Visible `[lo, hi]` ticks for a chart zoom level (± half spacings around spot).
pub fn chart_view_window(
    current: i32,
    spacing: u16,
    half_spacings: i32,
    series_extent: (i32, i32),
) -> (i32, i32) {
    let step = spacing.max(1) as i32;
    let half = half_spacings.max(CHART_HALF_SPACINGS_MIN) * step;
    let (smin, smax) = series_extent;
    let lo = (current - half).max(smin);
    let hi = (current + half).min(smax);
    if lo >= hi { (smin, smax) } else { (lo, hi) }
}

/// Filter liquidity samples to a tick window; recompute peak for Y scale.
/// Pins `view_lo` / `view_hi` so the X axis matches zoom even when edges have no depth.
pub fn slice_liquidity_series(
    series: &[LiquiditySample],
    view_lo: i32,
    view_hi: i32,
) -> (Vec<LiquiditySample>, u64) {
    if series.is_empty() {
        return (Vec::new(), 1);
    }
    let mut out: Vec<LiquiditySample> = series
        .iter()
        .copied()
        .filter(|s| s.tick >= view_lo && s.tick <= view_hi)
        .collect();
    if out.is_empty() {
        out = series.to_vec();
    }
    let edge = |tick: i32| LiquiditySample {
        tick,
        liquidity: 1,
        side: TokenSide::Outside,
        in_position: false,
    };
    if out.first().is_none_or(|s| s.tick > view_lo) {
        out.insert(0, edge(view_lo));
    }
    if out.last().is_none_or(|s| s.tick < view_hi) {
        out.push(edge(view_hi));
    }
    let max_l = out
        .iter()
        .map(|s| s.liquidity)
        .filter(|&l| l > 1)
        .max()
        .or_else(|| out.iter().map(|s| s.liquidity).max())
        .unwrap_or(1)
        .max(1);
    (out, max_l)
}

#[cfg(test)]
mod chart_zoom_tests {
    use super::*;

    #[test]
    fn view_window_clamps_to_series() {
        let (lo, hi) = chart_view_window(-23500, 4, 8, (-24000, -23000));
        assert_eq!(lo, -23500 - 8 * 4);
        assert_eq!(hi, -23500 + 8 * 4);
        let (lo2, hi2) = chart_view_window(-23500, 4, 10_000, (-23600, -23400));
        assert_eq!((lo2, hi2), (-23600, -23400));
    }

    #[test]
    fn slice_keeps_mid_and_pins_edges() {
        let series = [
            LiquiditySample {
                tick: 0,
                liquidity: 10,
                side: TokenSide::Outside,
                in_position: false,
            },
            LiquiditySample {
                tick: 100,
                liquidity: 50,
                side: TokenSide::Both,
                in_position: true,
            },
            LiquiditySample {
                tick: 200,
                liquidity: 20,
                side: TokenSide::Outside,
                in_position: false,
            },
        ];
        let (view, max_l) = slice_liquidity_series(&series, 50, 150);
        assert_eq!(view.first().unwrap().tick, 50);
        assert_eq!(view.last().unwrap().tick, 150);
        assert!(view.iter().any(|s| s.tick == 100));
        assert_eq!(max_l, 50);
    }

    #[test]
    fn zoom_geometric_reaches_full_and_min() {
        let mut half = 512i32;
        let max = 512;
        let min = CHART_HALF_SPACINGS_MIN;
        let mut steps_in = 0;
        while half > min {
            let next = ((half * 2) / 3).max(min);
            let next = if next >= half {
                (half - 1).max(min)
            } else {
                next
            };
            if next == half {
                break;
            }
            half = next;
            steps_in += 1;
            assert!(steps_in < 40, "zoom-in should converge");
        }
        assert_eq!(half, min);
        assert!(
            steps_in >= 8,
            "wide→tight should take many steps, got {steps_in}"
        );

        half = min;
        let mut steps_out = 0;
        while half < max {
            let next = (half.saturating_mul(3) / 2).max(half + 1).min(max);
            if next == half {
                break;
            }
            half = next;
            steps_out += 1;
        }
        assert_eq!(half, max);
        assert!(
            steps_out >= 8,
            "tight→wide should take many steps, got {steps_out}"
        );
    }
}
