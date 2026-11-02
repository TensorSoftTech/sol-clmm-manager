//! Meteora DLMM strategies — on-chain [`MeteoraStrategyType`] + selectable UX subset.
//!
//! Weight / autofill math mirrors `@meteora-ag/dlmm` (`strategy.ts` + `weightToAmounts.ts`)
//! so single-sided + autoswap sizes match Balanced on-chain deposits.

use crate::write::liquidity::{DepositShape, SwapToMatchHint};

/// On-chain `StrategyType` from Meteora DLMM (borsh `u8`, IDL declaration order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MeteoraStrategyType {
    SpotOneSide = 0,
    CurveOneSide = 1,
    BidAskOneSide = 2,
    SpotBalanced = 3,
    CurveBalanced = 4,
    BidAskBalanced = 5,
    /// TODO: expose in UI when we support imbalanced deposit sizing.
    SpotImBalanced = 6,
    /// TODO: expose in UI when we support imbalanced deposit sizing.
    CurveImBalanced = 7,
    /// TODO: expose in UI when we support imbalanced deposit sizing.
    BidAskImBalanced = 8,
}

impl MeteoraStrategyType {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::SpotOneSide => "SpotOneSide",
            Self::CurveOneSide => "CurveOneSide",
            Self::BidAskOneSide => "BidAskOneSide",
            Self::SpotBalanced => "SpotBalanced",
            Self::CurveBalanced => "CurveBalanced",
            Self::BidAskBalanced => "BidAskBalanced",
            Self::SpotImBalanced => "SpotImBalanced",
            Self::CurveImBalanced => "CurveImBalanced",
            Self::BidAskImBalanced => "BidAskImBalanced",
        }
    }
}

/// Selectable DLMM strategies — Meteora labels, OneSide + Balanced only.
///
/// `*ImBalanced` variants exist on-chain ([`MeteoraStrategyType`]) but are **TODO** / not offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DlmmStrategy {
    SpotOneSide,
    CurveOneSide,
    BidAskOneSide,
    SpotBalanced,
    #[default]
    CurveBalanced,
    BidAskBalanced,
}

impl DlmmStrategy {
    /// UI / settings cycle order (ImBalanced omitted — TODO).
    pub const ALL: [DlmmStrategy; 6] = [
        Self::SpotOneSide,
        Self::CurveOneSide,
        Self::BidAskOneSide,
        Self::SpotBalanced,
        Self::CurveBalanced,
        Self::BidAskBalanced,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::SpotOneSide => "SpotOneSide · flat · X or Y only",
            Self::CurveOneSide => "CurveOneSide · peak mid · X or Y only",
            Self::BidAskOneSide => "BidAskOneSide · weight edges · X or Y only",
            Self::SpotBalanced => "SpotBalanced · flat · both sides",
            Self::CurveBalanced => "CurveBalanced · peak active · both sides",
            Self::BidAskBalanced => "BidAskBalanced · weight edges · both sides",
        }
    }

    pub fn short(self) -> &'static str {
        self.to_meteora().label()
    }

    /// Settings / store key (Meteora type name).
    pub fn setting_key(self) -> &'static str {
        self.to_meteora().label()
    }

    pub fn from_setting(s: &str) -> Self {
        match s {
            "SpotOneSide" | "spot-one-side" => Self::SpotOneSide,
            "CurveOneSide" | "curve-one-side" => Self::CurveOneSide,
            "BidAskOneSide" | "bid-ask-one-side" => Self::BidAskOneSide,
            "SpotBalanced" | "spot-balanced" | "spot-tight" | "spot-spread" | "spot-wide" => {
                Self::SpotBalanced
            }
            "BidAskBalanced" | "bid-ask" | "BidAsk" => Self::BidAskBalanced,
            // legacy "curve" + default
            "CurveBalanced" | "curve" | "Curve" | _ => Self::CurveBalanced,
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::SpotOneSide => Self::CurveOneSide,
            Self::CurveOneSide => Self::BidAskOneSide,
            Self::BidAskOneSide => Self::SpotBalanced,
            Self::SpotBalanced => Self::CurveBalanced,
            Self::CurveBalanced => Self::BidAskBalanced,
            Self::BidAskBalanced => Self::SpotOneSide,
        }
    }

    /// 1:1 with on-chain [`MeteoraStrategyType`] (selectable subset).
    pub fn to_meteora(self) -> MeteoraStrategyType {
        match self {
            Self::SpotOneSide => MeteoraStrategyType::SpotOneSide,
            Self::CurveOneSide => MeteoraStrategyType::CurveOneSide,
            Self::BidAskOneSide => MeteoraStrategyType::BidAskOneSide,
            Self::SpotBalanced => MeteoraStrategyType::SpotBalanced,
            Self::CurveBalanced => MeteoraStrategyType::CurveBalanced,
            Self::BidAskBalanced => MeteoraStrategyType::BidAskBalanced,
        }
    }

    pub fn is_one_side(self) -> bool {
        matches!(
            self,
            Self::SpotOneSide | Self::CurveOneSide | Self::BidAskOneSide
        )
    }

    pub fn is_balanced(self) -> bool {
        !self.is_one_side()
    }

    fn shape_family(self) -> ShapeFamily {
        match self {
            Self::SpotOneSide | Self::SpotBalanced => ShapeFamily::Spot,
            Self::CurveOneSide | Self::CurveBalanced => ShapeFamily::Curve,
            Self::BidAskOneSide | Self::BidAskBalanced => ShapeFamily::BidAsk,
        }
    }
}

#[derive(Clone, Copy)]
enum ShapeFamily {
    Spot,
    Curve,
    BidAsk,
}

/// Per-bin relative weights (sum may be any positive; caller normalizes).
#[derive(Debug, Clone)]
pub struct BinWeights {
    pub bins: Vec<(i32, f64)>,
}

/// Build preview weights for `[min_bin, max_bin]` from the strategy family shape.
///
/// Curve/BidAsk previews peak relative to range midpoint (UI only). On-chain Balanced
/// Curve/BidAsk peak at `active_id` — see [`balanced_weights`].
pub fn distribute_bins(strategy: DlmmStrategy, min_bin: i32, max_bin: i32) -> BinWeights {
    if min_bin > max_bin {
        return BinWeights { bins: Vec::new() };
    }
    let mid = (min_bin as i64 + max_bin as i64) / 2;
    let half = ((max_bin - min_bin) as f64 / 2.0).max(1.0);
    let mut bins = Vec::with_capacity((max_bin - min_bin + 1) as usize);
    for id in min_bin..=max_bin {
        let dist = (id as i64 - mid).unsigned_abs() as f64;
        let w = match strategy.shape_family() {
            ShapeFamily::Curve => {
                let t = (1.0 - dist / half).max(0.0);
                t * t
            }
            ShapeFamily::BidAsk => {
                let t = (dist / half).min(1.0);
                0.05 + t * t
            }
            ShapeFamily::Spot => 1.0,
        };
        if w > 0.0 {
            bins.push((id, w));
        }
    }
    BinWeights { bins }
}

const DEFAULT_MAX_WEIGHT: i32 = 2000;
const DEFAULT_MIN_WEIGHT: i32 = 200;

/// Raw bin price (Y atomic / X atomic) — same as Meteora `getPriceOfBinByBinId`.
pub fn bin_raw_price(bin_id: i32, bin_step: u16) -> f64 {
    let step = 1.0 + f64::from(bin_step) / 10_000.0;
    if !(step.is_finite() && step > 0.0) {
        return f64::NAN;
    }
    step.powf(f64::from(bin_id))
}

/// Meteora Balanced weight vector for `[min_bin, max_bin]` (integer weights).
pub fn balanced_weights(
    strategy: DlmmStrategy,
    min_bin: i32,
    max_bin: i32,
    active_id: i32,
) -> Vec<(i32, u32)> {
    if min_bin > max_bin {
        return Vec::new();
    }
    match strategy.shape_family() {
        ShapeFamily::Spot => (min_bin..=max_bin).map(|id| (id, 1u32)).collect(),
        ShapeFamily::Curve => {
            if active_id < min_bin || active_id > max_bin {
                // Out of range: descending (ask) or ascending (bid) like SDK.
                if active_id < min_bin {
                    (min_bin..=max_bin)
                        .map(|id| (id, (max_bin - id + 1) as u32))
                        .collect()
                } else {
                    (min_bin..=max_bin)
                        .map(|id| (id, (id - min_bin + 1) as u32))
                        .collect()
                }
            } else {
                to_weight_curve(min_bin, max_bin, active_id)
            }
        }
        ShapeFamily::BidAsk => {
            if active_id < min_bin || active_id > max_bin {
                if active_id < min_bin {
                    (min_bin..=max_bin)
                        .map(|id| (id, (id - min_bin + 1) as u32))
                        .collect()
                } else {
                    (min_bin..=max_bin)
                        .map(|id| (id, (max_bin - id + 1) as u32))
                        .collect()
                }
            } else {
                to_weight_bid_ask(min_bin, max_bin, active_id)
            }
        }
    }
}

fn to_weight_curve(min_bin: i32, max_bin: i32, active_id: i32) -> Vec<(i32, u32)> {
    let diff = DEFAULT_MAX_WEIGHT - DEFAULT_MIN_WEIGHT;
    let diff_min = if active_id > min_bin {
        diff / (active_id - min_bin)
    } else {
        0
    };
    let diff_max = if max_bin > active_id {
        diff / (max_bin - active_id)
    } else {
        0
    };
    (min_bin..=max_bin)
        .map(|i| {
            let w = if i < active_id {
                DEFAULT_MAX_WEIGHT - (active_id - i) * diff_min
            } else if i > active_id {
                DEFAULT_MAX_WEIGHT - (i - active_id) * diff_max
            } else {
                DEFAULT_MAX_WEIGHT
            };
            (i, w.max(0) as u32)
        })
        .collect()
}

fn to_weight_bid_ask(min_bin: i32, max_bin: i32, active_id: i32) -> Vec<(i32, u32)> {
    let diff = DEFAULT_MAX_WEIGHT - DEFAULT_MIN_WEIGHT;
    let diff_min = if active_id > min_bin {
        diff / (active_id - min_bin)
    } else {
        0
    };
    let diff_max = if max_bin > active_id {
        diff / (max_bin - active_id)
    } else {
        0
    };
    (min_bin..=max_bin)
        .map(|i| {
            let w = if i < active_id {
                DEFAULT_MIN_WEIGHT + (active_id - i) * diff_min
            } else if i > active_id {
                DEFAULT_MIN_WEIGHT + (i - active_id) * diff_max
            } else {
                DEFAULT_MIN_WEIGHT
            };
            (i, w.max(0) as u32)
        })
        .collect()
}

/// `(total_weight_x, total_weight_y)` for Balanced autofill (empty active-bin composition).
///
/// Mirrors `autoFillYByWeight` / `autoFillXByWeight` with `amountXInActiveBin = amountYInActiveBin = 0`.
pub fn balanced_weight_totals(
    strategy: DlmmStrategy,
    active_id: i32,
    bin_step: u16,
    min_bin: i32,
    max_bin: i32,
) -> Option<(f64, f64)> {
    let weights = balanced_weights(strategy, min_bin, max_bin, active_id);
    if weights.is_empty() {
        return None;
    }
    let has_active = weights.iter().any(|(id, _)| *id == active_id);
    let mut total_x = 0.0_f64;
    let mut total_y = 0.0_f64;

    if has_active {
        let w0 = weights
            .iter()
            .find(|(id, _)| *id == active_id)
            .map(|(_, w)| f64::from(*w))
            .unwrap_or(0.0);
        let p0 = bin_raw_price(active_id, bin_step);
        if !(p0.is_finite() && p0 > 0.0) {
            return None;
        }
        // Empty active bin → split weight 50/50 across X/Y (SDK).
        total_x += w0 / (p0 * 2.0);
        total_y += w0 / 2.0;
        for &(id, w) in &weights {
            if id < active_id {
                total_y += f64::from(w);
            } else if id > active_id {
                let p = bin_raw_price(id, bin_step);
                if p.is_finite() && p > 0.0 {
                    total_x += f64::from(w) / p;
                }
            }
        }
    } else {
        for &(id, w) in &weights {
            if id < active_id {
                total_y += f64::from(w);
            } else {
                let p = bin_raw_price(id, bin_step);
                if p.is_finite() && p > 0.0 {
                    total_x += f64::from(w) / p;
                }
            }
        }
    }

    if total_x > 0.0 || total_y > 0.0 {
        Some((total_x, total_y))
    } else {
        None
    }
}

/// Y amount needed for Balanced deposit given X (Meteora `autoFillYByStrategy`).
pub fn auto_fill_y(
    amount_x: u64,
    strategy: DlmmStrategy,
    active_id: i32,
    bin_step: u16,
    min_bin: i32,
    max_bin: i32,
) -> u64 {
    let Some((wx, wy)) = balanced_weight_totals(strategy, active_id, bin_step, min_bin, max_bin)
    else {
        return 0;
    };
    if wx <= 0.0 {
        return 0;
    }
    let y = (amount_x as f64) * (wy / wx);
    if !y.is_finite() || y <= 0.0 {
        0
    } else if y >= u64::MAX as f64 {
        u64::MAX
    } else {
        y.floor() as u64
    }
}

/// X amount needed for Balanced deposit given Y (Meteora `autoFillXByStrategy`).
pub fn auto_fill_x(
    amount_y: u64,
    strategy: DlmmStrategy,
    active_id: i32,
    bin_step: u16,
    min_bin: i32,
    max_bin: i32,
) -> u64 {
    let Some((wx, wy)) = balanced_weight_totals(strategy, active_id, bin_step, min_bin, max_bin)
    else {
        return 0;
    };
    if wy <= 0.0 {
        return 0;
    }
    let x = (amount_y as f64) * (wx / wy);
    if !x.is_finite() || x <= 0.0 {
        0
    } else if x >= u64::MAX as f64 {
        u64::MAX
    } else {
        x.floor() as u64
    }
}

/// Clamp `(amount_x, amount_y)` to what Balanced will actually take (`k = min(kx, ky)`).
pub fn clamp_balanced_amounts(
    amount_x: u64,
    amount_y: u64,
    strategy: DlmmStrategy,
    active_id: i32,
    bin_step: u16,
    min_bin: i32,
    max_bin: i32,
) -> (u64, u64) {
    if !strategy.is_balanced() || (amount_x == 0 && amount_y == 0) {
        return (amount_x, amount_y);
    }
    let need_y = auto_fill_y(amount_x, strategy, active_id, bin_step, min_bin, max_bin);
    if need_y <= amount_y {
        (amount_x, need_y)
    } else {
        let need_x = auto_fill_x(amount_y, strategy, active_id, bin_step, min_bin, max_bin);
        (need_x.min(amount_x), amount_y)
    }
}

/// Swap hint so post-swap wallets match Balanced strategy ratio (replaces naive half-swap).
///
/// Spot price estimate: active-bin raw price (Y/X). Jupiter quote still sets actual out.
pub fn dlmm_swap_to_match_hint(
    amount_x: u64,
    amount_y: u64,
    active_id: i32,
    bin_step: u16,
    min_bin: i32,
    max_bin: i32,
    strategy: DlmmStrategy,
    shape: DepositShape,
) -> Option<SwapToMatchHint> {
    // OneSide deposits a single mint — never autoswap into the other.
    if strategy.is_one_side() {
        return None;
    }
    if min_bin > max_bin {
        return None;
    }

    // Out of range: only one side is accepted.
    if active_id > max_bin {
        // Bid-only (Y). Funding X → swap all X→Y.
        let wrong_x = match shape {
            DepositShape::SingleA => amount_x,
            DepositShape::Dual => amount_x,
            DepositShape::SingleB => 0,
        };
        return if wrong_x > 0 {
            Some(SwapToMatchHint {
                in_is_a: true,
                swap_in_amount: wrong_x,
            })
        } else {
            None
        };
    }
    if active_id < min_bin {
        // Ask-only (X). Funding Y → swap all Y→X.
        let wrong_y = match shape {
            DepositShape::SingleB => amount_y,
            DepositShape::Dual => amount_y,
            DepositShape::SingleA => 0,
        };
        return if wrong_y > 0 {
            Some(SwapToMatchHint {
                in_is_a: false,
                swap_in_amount: wrong_y,
            })
        } else {
            None
        };
    }

    let Some((wx, wy)) = balanced_weight_totals(strategy, active_id, bin_step, min_bin, max_bin)
    else {
        return None;
    };
    // R = need_y / rem_x (raw). Spot P = Y/X at active bin.
    let p = bin_raw_price(active_id, bin_step);
    if !(p.is_finite() && p > 0.0) || wx <= 0.0 || wy <= 0.0 {
        return None;
    }
    let r = wy / wx;

    match shape {
        DepositShape::SingleA => {
            if amount_x < 2 {
                return None;
            }
            // S*P = R*(amount_x - S)  →  S = amount_x * R / (P + R)
            let mut swap_in = ((amount_x as f64) * r / (p + r)).floor() as u64;
            swap_in = swap_in.clamp(1, amount_x.saturating_sub(1));
            Some(SwapToMatchHint {
                in_is_a: true,
                swap_in_amount: swap_in,
            })
        }
        DepositShape::SingleB => {
            if amount_y < 2 {
                return None;
            }
            // rem_y = amount_y - S; x ≈ S/P; rem_y = R*x → S = amount_y * P / (P + R)
            let mut swap_in = ((amount_y as f64) * p / (p + r)).floor() as u64;
            swap_in = swap_in.clamp(1, amount_y.saturating_sub(1));
            Some(SwapToMatchHint {
                in_is_a: false,
                swap_in_amount: swap_in,
            })
        }
        DepositShape::Dual => {
            // Rebalance overweight side toward R at spot P.
            if amount_x == 0 && amount_y == 0 {
                return None;
            }
            if amount_x == 0 {
                return dlmm_swap_to_match_hint(
                    0,
                    amount_y,
                    active_id,
                    bin_step,
                    min_bin,
                    max_bin,
                    strategy,
                    DepositShape::SingleB,
                );
            }
            if amount_y == 0 {
                return dlmm_swap_to_match_hint(
                    amount_x,
                    0,
                    active_id,
                    bin_step,
                    min_bin,
                    max_bin,
                    strategy,
                    DepositShape::SingleA,
                );
            }
            let value_x = (amount_x as f64) * p;
            let value_y = amount_y as f64;
            let total = value_x + value_y;
            // Target: value split so y_raw / x_raw = R → value_y / value_x = R/P? 
            // rem_y = R * rem_x, value_y = rem_y, value_x = rem_x * P → value_y/value_x = R/P
            // target_value_x = total * P / (P + R), target_value_y = total * R / (P + R)
            let target_vx = total * p / (p + r);
            if value_x > target_vx * 1.02 {
                let excess_x = ((value_x - target_vx) / p).floor() as u64;
                let swap_in = excess_x.clamp(1, amount_x.saturating_sub(1));
                Some(SwapToMatchHint {
                    in_is_a: true,
                    swap_in_amount: swap_in,
                })
            } else if value_y > (total - target_vx) * 1.02 {
                let excess_y = (value_y - (total - target_vx)).floor() as u64;
                let swap_in = excess_y.clamp(1, amount_y.saturating_sub(1));
                Some(SwapToMatchHint {
                    in_is_a: false,
                    swap_in_amount: swap_in,
                })
            } else {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_peaks_at_mid() {
        let w = distribute_bins(DlmmStrategy::CurveBalanced, -5, 5);
        let mid = w.bins.iter().find(|(id, _)| *id == 0).unwrap().1;
        let near = w.bins.iter().find(|(id, _)| *id == 4).unwrap().1;
        assert!(mid > near);
    }

    #[test]
    fn bid_ask_peaks_at_edge() {
        let w = distribute_bins(DlmmStrategy::BidAskBalanced, -5, 5);
        let mid = w.bins.iter().find(|(id, _)| *id == 0).unwrap().1;
        let edge = w.bins.iter().find(|(id, _)| *id == 5).unwrap().1;
        assert!(edge > mid);
    }

    #[test]
    fn spot_flat() {
        let w = distribute_bins(DlmmStrategy::SpotBalanced, 0, 4);
        let first = w.bins[0].1;
        assert!(w.bins.iter().all(|(_, x)| (*x - first).abs() < 1e-9));
    }

    #[test]
    fn selectable_maps_1to1_idl() {
        for s in DlmmStrategy::ALL {
            assert_eq!(s.to_meteora().label(), s.short());
        }
        assert_eq!(DlmmStrategy::CurveBalanced.to_meteora().as_u8(), 4);
        assert_eq!(DlmmStrategy::SpotOneSide.to_meteora().as_u8(), 0);
        assert_eq!(MeteoraStrategyType::BidAskImBalanced as u8, 8);
        assert_eq!(DlmmStrategy::ALL.len(), 6);
    }

    #[test]
    fn legacy_settings_keys_still_parse() {
        assert_eq!(
            DlmmStrategy::from_setting("curve"),
            DlmmStrategy::CurveBalanced
        );
        assert_eq!(
            DlmmStrategy::from_setting("bid-ask"),
            DlmmStrategy::BidAskBalanced
        );
        assert_eq!(
            DlmmStrategy::from_setting("spot-tight"),
            DlmmStrategy::SpotBalanced
        );
    }

    #[test]
    fn one_side_never_autoswaps() {
        assert!(dlmm_swap_to_match_hint(
            1_000_000_000,
            0,
            0,
            10,
            -10,
            10,
            DlmmStrategy::CurveOneSide,
            DepositShape::SingleA,
        )
        .is_none());
    }

    #[test]
    fn balanced_single_a_swaps_less_than_half_when_x_heavy() {
        // Wide ask side (X) vs thin bid (Y) around active → keep more X than half.
        let active = 0;
        let hint = dlmm_swap_to_match_hint(
            1_000_000_000,
            0,
            active,
            10,
            -5,
            40,
            DlmmStrategy::SpotBalanced,
            DepositShape::SingleA,
        )
        .expect("hint");
        assert!(hint.in_is_a);
        assert!(
            hint.swap_in_amount < 500_000_000,
            "swap {} should be < half on X-heavy range",
            hint.swap_in_amount
        );
        assert!(hint.swap_in_amount > 0);
    }

    #[test]
    fn autofill_roundtrip_roughly_consistent() {
        let s = DlmmStrategy::CurveBalanced;
        let x = 10_000_000u64;
        let y = auto_fill_y(x, s, -5900, 10, -5932, -5867);
        assert!(y > 0, "expected Y autofill");
        let x2 = auto_fill_x(y, s, -5900, 10, -5932, -5867);
        let err = (x2 as i64 - x as i64).unsigned_abs() as f64 / x as f64;
        assert!(err < 0.02, "roundtrip x {x} → y {y} → x2 {x2} err {err}");
    }

    #[test]
    fn clamp_drops_excess_y() {
        let s = DlmmStrategy::SpotBalanced;
        let x = 10_000_000u64;
        let need_y = auto_fill_y(x, s, 0, 25, -20, 20);
        let (cx, cy) = clamp_balanced_amounts(x, need_y.saturating_mul(2), s, 0, 25, -20, 20);
        assert_eq!(cx, x);
        assert_eq!(cy, need_y);
    }

    #[test]
    fn out_of_range_bid_swaps_all_x() {
        let hint = dlmm_swap_to_match_hint(
            1_000_000,
            0,
            100,
            10,
            0,
            50,
            DlmmStrategy::CurveBalanced,
            DepositShape::SingleA,
        )
        .unwrap();
        assert!(hint.in_is_a);
        assert_eq!(hint.swap_in_amount, 1_000_000);
    }
}
