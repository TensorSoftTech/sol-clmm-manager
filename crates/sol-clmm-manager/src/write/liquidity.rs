//! Shared deposit / withdraw sizing (CLMM √P + DLMM planner hooks).

use crate::clmm_math::{
    amount_a_from_b, amounts_from_liquidity, get_sqrt_price_at_tick, value_a_in_b,
};
use crate::write::dlmm_strategy::DlmmStrategy;

/// Which tokens a draft range consumes at the current pool price.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeDepositSide {
    /// Price below range → deposit token A only.
    TokenAOnly,
    /// Price above range → deposit token B only.
    TokenBOnly,
    /// Price inside range → both tokens at a fixed ratio.
    Both,
}

impl RangeDepositSide {
    pub fn label(self, sym_a: &str, sym_b: &str) -> String {
        match self {
            Self::TokenAOnly => format!("{sym_a} only · price below range"),
            Self::TokenBOnly => format!("{sym_b} only · price above range"),
            Self::Both => format!("{sym_a} + {sym_b} · price in range"),
        }
    }

    /// Wizard shape index that matches this side without a swap (0 dual · 1 A · 2 B).
    pub fn recommended_shape_idx(self) -> usize {
        match self {
            Self::Both => 0,
            Self::TokenAOnly => 1,
            Self::TokenBOnly => 2,
        }
    }

    pub fn shape_fits_without_swap(self, shape: DepositShape) -> bool {
        match (self, shape) {
            (Self::Both, DepositShape::Dual) => true,
            (Self::TokenAOnly, DepositShape::SingleA | DepositShape::Dual) => true,
            (Self::TokenBOnly, DepositShape::SingleB | DepositShape::Dual) => true,
            _ => false,
        }
    }
}

/// Classify draft ticks vs current √P (CLMM) or active index (DLMM when √P=0).
pub fn range_deposit_side(
    sqrt_price: u128,
    tick_lower: i32,
    tick_upper: i32,
    tick_current: i32,
) -> RangeDepositSide {
    if tick_lower >= tick_upper {
        return RangeDepositSide::Both;
    }
    if sqrt_price > 0 {
        let Some(sa) = get_sqrt_price_at_tick(tick_lower) else {
            return RangeDepositSide::Both;
        };
        let Some(sb) = get_sqrt_price_at_tick(tick_upper) else {
            return RangeDepositSide::Both;
        };
        if sqrt_price <= sa {
            RangeDepositSide::TokenAOnly
        } else if sqrt_price >= sb {
            RangeDepositSide::TokenBOnly
        } else {
            RangeDepositSide::Both
        }
    } else if tick_current < tick_lower {
        RangeDepositSide::TokenAOnly
    } else if tick_current >= tick_upper {
        RangeDepositSide::TokenBOnly
    } else {
        RangeDepositSide::Both
    }
}

/// Live fit of wallet amounts + shape against the draft range (imbalance / swap).
#[derive(Debug, Clone)]
pub struct DepositAnalysis {
    pub side: RangeDepositSide,
    /// Unit raw A/B for in-range ratio (0 on unused side when out of range).
    pub unit_a: u128,
    pub unit_b: u128,
    /// Matching B for the given A (in-range), if A > 0.
    pub paired_b_for_a: Option<u128>,
    /// Matching A for the given B (in-range), if B > 0.
    pub paired_a_for_b: Option<u128>,
    pub shape_ok_without_swap: bool,
    pub swap_needed: bool,
    pub swap: Option<SwapToMatchHint>,
    /// ΔL / spent without packing a swap.
    pub liquidity_no_swap: u128,
    pub spend_a_no_swap: u128,
    pub spend_b_no_swap: u128,
    /// ΔL / spent if swap-to-match is applied (same as no-swap when swap unused).
    pub liquidity_with_swap: u128,
    pub spend_a_with_swap: u128,
    pub spend_b_with_swap: u128,
    pub note: String,
}

/// Analyze amounts vs range: pair targets, whether swap is required, depositable ΔL.
pub fn analyze_deposit(
    amount_a: u128,
    amount_b: u128,
    sqrt_price: u128,
    tick_lower: i32,
    tick_upper: i32,
    tick_current: i32,
    shape: DepositShape,
) -> DepositAnalysis {
    let side = range_deposit_side(sqrt_price, tick_lower, tick_upper, tick_current);
    let user_shape = shape;
    let shape = effective_deposit_shape(amount_a, amount_b, shape);
    let (unit_a, unit_b) = if sqrt_price > 0 {
        deposit_ratio_planner(sqrt_price, tick_lower, tick_upper, 1u128 << 48)
    } else {
        (0, 0)
    };

    let paired_b_for_a = if side == RangeDepositSide::Both && unit_a > 0 && amount_a > 0 {
        Some(amount_a.saturating_mul(unit_b) / unit_a)
    } else if side == RangeDepositSide::TokenAOnly {
        Some(0)
    } else {
        None
    };
    let paired_a_for_b = if side == RangeDepositSide::Both && unit_b > 0 && amount_b > 0 {
        Some(amount_b.saturating_mul(unit_a) / unit_b)
    } else if side == RangeDepositSide::TokenBOnly {
        Some(0)
    } else {
        None
    };

    // Judge the wizard shape the user picked (not the A=0/B=0 coercion).
    let shape_ok = side.shape_fits_without_swap(user_shape);
    let (no_swap, with_swap, swap) = if sqrt_price > 0 {
        let no_swap = liquidity_delta_for_open(
            amount_a,
            amount_b,
            sqrt_price,
            tick_lower,
            tick_upper,
            shape,
            false,
        );
        let with_swap = liquidity_delta_for_open(
            amount_a,
            amount_b,
            sqrt_price,
            tick_lower,
            tick_upper,
            shape,
            true,
        );
        let swap = swap_to_match_hint(
            amount_a.min(u128::from(u64::MAX)) as u64,
            amount_b.min(u128::from(u64::MAX)) as u64,
            sqrt_price,
            tick_lower,
            tick_upper,
            shape,
        );
        (no_swap, with_swap, swap)
    } else {
        (
            zero_liquidity_plan(shape, "DLMM · no CLMM √P sizing"),
            zero_liquidity_plan(shape, "DLMM · no CLMM √P sizing"),
            None,
        )
    };

    // Swap needed when shape/amounts can't open without it, but can with it —
    // or when dual budgets are clearly off-ratio.
    let ratio_off = amount_a > 0
        && amount_b > 0
        && match (paired_b_for_a, paired_a_for_b) {
            (Some(need_b), _) => {
                let lo = need_b.saturating_mul(95) / 100;
                let hi = need_b.saturating_mul(105) / 100;
                amount_b < lo || amount_b > hi
            }
            (_, Some(need_a)) => {
                let lo = need_a.saturating_mul(95) / 100;
                let hi = need_a.saturating_mul(105) / 100;
                amount_a < lo || amount_a > hi
            }
            _ => false,
        };
    let swap_needed = if !shape_ok {
        true
    } else if swap.is_some() {
        no_swap.liquidity_delta < with_swap.liquidity_delta || ratio_off
    } else {
        no_swap.liquidity_delta == 0 && (amount_a > 0 || amount_b > 0)
    };

    let note = if sqrt_price == 0 {
        "DLMM · ratio hint unavailable (no √P)".into()
    } else if swap_needed {
        if let Some(h) = swap {
            let dir = if h.in_is_a { "A→B" } else { "B→A" };
            format!(
                "imbalance · swap ~{} {dir} to match · ΔL {}→{}",
                h.swap_in_amount, no_swap.liquidity_delta, with_swap.liquidity_delta
            )
        } else if !shape_ok {
            format!(
                "shape mismatch · range wants {} · enable swap-to-match or change shape",
                match side {
                    RangeDepositSide::TokenAOnly => "A only",
                    RangeDepositSide::TokenBOnly => "B only",
                    RangeDepositSide::Both => "A+B",
                }
            )
        } else {
            "amounts cannot open this range without a rebalance".into()
        }
    } else if no_swap.liquidity_delta > 0 {
        format!(
            "ok · ΔL {} · spend ~{} A + {} B",
            no_swap.liquidity_delta, no_swap.amount_a, no_swap.amount_b
        )
    } else {
        "enter amounts to size deposit".into()
    };

    DepositAnalysis {
        side,
        unit_a,
        unit_b,
        paired_b_for_a,
        paired_a_for_b,
        shape_ok_without_swap: shape_ok,
        swap_needed,
        swap,
        liquidity_no_swap: no_swap.liquidity_delta,
        spend_a_no_swap: no_swap.amount_a,
        spend_b_no_swap: no_swap.amount_b,
        liquidity_with_swap: with_swap.liquidity_delta,
        spend_a_with_swap: with_swap.amount_a,
        spend_b_with_swap: with_swap.amount_b,
        note,
    }
}

/// Dual vs single-sided deposit shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DepositShape {
    #[default]
    Dual,
    SingleA,
    SingleB,
}

impl DepositShape {
    pub fn label(self) -> &'static str {
        match self {
            Self::Dual => "dual · A + B",
            Self::SingleA => "single · A only",
            Self::SingleB => "single · B only",
        }
    }

    /// Labels with resolved mint symbols (e.g. `dual · SOL + USDC`).
    pub fn label_with(self, sym_a: &str, sym_b: &str) -> String {
        match self {
            Self::Dual => format!("dual · {sym_a} + {sym_b}"),
            Self::SingleA => format!("single · {sym_a} only"),
            Self::SingleB => format!("single · {sym_b} only"),
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Dual => Self::SingleA,
            Self::SingleA => Self::SingleB,
            Self::SingleB => Self::Dual,
        }
    }
}

/// How amounts are chosen in the wizard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DepositSizing {
    #[default]
    Exact,
    MaxAffordable,
    PercentOfWallet,
}

impl DepositSizing {
    pub fn label(self) -> &'static str {
        match self {
            Self::Exact => "exact amounts",
            Self::MaxAffordable => "max affordable",
            Self::PercentOfWallet => "% of wallet",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Exact => Self::MaxAffordable,
            Self::MaxAffordable => Self::PercentOfWallet,
            Self::PercentOfWallet => Self::Exact,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LiquidityDeltaPlan {
    pub liquidity_delta: u128,
    pub amount_a: u128,
    pub amount_b: u128,
    pub shape: DepositShape,
    pub sizing: DepositSizing,
    /// DLMM only — ignored for CLMM.
    pub dlmm_strategy: Option<DlmmStrategy>,
    pub note: String,
}

/// Amounts required for a CLMM liquidity increase (same math as position amounts).
pub fn amounts_for_liquidity_delta(
    liquidity_delta: u128,
    sqrt_price: u128,
    tick_lower: i32,
    tick_upper: i32,
) -> (u128, u128) {
    amounts_from_liquidity(liquidity_delta, sqrt_price, tick_lower, tick_upper)
}

/// Coerce dual → single when only one side has a budget (avoids L=0 open-only creates).
pub fn effective_deposit_shape(amount_a: u128, amount_b: u128, shape: DepositShape) -> DepositShape {
    match shape {
        DepositShape::Dual if amount_a > 0 && amount_b == 0 => DepositShape::SingleA,
        DepositShape::Dual if amount_b > 0 && amount_a == 0 => DepositShape::SingleB,
        other => other,
    }
}

/// Rough inverse: given wallet amounts and shape, pick a ΔL that fits (binary search).
///
/// Single-A / single-B use an unconstrained other-side cap — only valid when the
/// current price sits outside the range on that side. Prefer
/// [`liquidity_delta_for_open`] for create flows (validates + optional swap-to-match).
pub fn liquidity_delta_from_amounts(
    amount_a: u128,
    amount_b: u128,
    sqrt_price: u128,
    tick_lower: i32,
    tick_upper: i32,
    shape: DepositShape,
) -> LiquidityDeltaPlan {
    let shape = effective_deposit_shape(amount_a, amount_b, shape);
    let (cap_a, cap_b) = match shape {
        DepositShape::Dual => (amount_a, amount_b),
        DepositShape::SingleA => (amount_a, u128::MAX),
        DepositShape::SingleB => (u128::MAX, amount_b),
    };
    // Empty budget → no deposit (also avoids Q64 truncation giving phantom ΔL with 0 spend).
    if cap_a == 0 && cap_b == 0 {
        return zero_liquidity_plan(shape, "ΔL 0 · no amounts");
    }

    // hi = 2^96 needs up to ~96 iterations to converge on small affordable L;
    // 64 was truncating to L=0 for modest single-side budgets on narrow ranges.
    let mut lo = 0u128;
    let mut hi = 1u128 << 96;
    for _ in 0..128 {
        if hi - lo <= 1 {
            break;
        }
        let mid = lo + (hi - lo) / 2;
        if mid == 0 {
            break;
        }
        let (need_a, need_b) = amounts_from_liquidity(mid, sqrt_price, tick_lower, tick_upper);
        // Require a real token spend — tiny L can round to (0,0) and would otherwise
        // "fit" any budget, including 0, producing ΔL>0 with spend 0.
        let fits = (need_a > 0 || need_b > 0) && need_a <= cap_a && need_b <= cap_b;
        if fits {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let (final_a, final_b) = amounts_from_liquidity(lo, sqrt_price, tick_lower, tick_upper);
    if final_a == 0 && final_b == 0 {
        return zero_liquidity_plan(shape, "ΔL 0 · amounts round to zero");
    }
    // Report required amounts for the deposit (not min'd to 0 on the unfunded side).
    let (report_a, report_b) = match shape {
        DepositShape::Dual => (final_a.min(amount_a), final_b.min(amount_b)),
        DepositShape::SingleA => (final_a.min(amount_a), final_b),
        DepositShape::SingleB => (final_a, final_b.min(amount_b)),
    };
    LiquidityDeltaPlan {
        liquidity_delta: lo,
        amount_a: report_a,
        amount_b: report_b,
        shape,
        sizing: DepositSizing::Exact,
        dlmm_strategy: None,
        note: format!("ΔL {lo} · needs ~{final_a} A + {final_b} B"),
    }
}

/// Spot estimate of balances after a swap-to-match rebalance (no router quote).
fn estimate_post_swap_amounts(
    amount_a: u128,
    amount_b: u128,
    sqrt_price: u128,
    tick_lower: i32,
    tick_upper: i32,
    shape: DepositShape,
) -> Option<(u128, u128)> {
    let a64 = amount_a.min(u128::from(u64::MAX)) as u64;
    let b64 = amount_b.min(u128::from(u64::MAX)) as u64;
    let hint = swap_to_match_hint(a64, b64, sqrt_price, tick_lower, tick_upper, shape)?;
    let swap_in = u128::from(hint.swap_in_amount);
    if hint.in_is_a {
        // A → B at spot: out_b = value_a_in_b(swap_in)
        let rem_a = amount_a.saturating_sub(swap_in);
        let out_b = value_a_in_b(swap_in, sqrt_price);
        Some((rem_a, amount_b.saturating_add(out_b)))
    } else {
        // B → A at spot
        let rem_b = amount_b.saturating_sub(swap_in);
        let out_a = amount_a_from_b(swap_in, sqrt_price);
        Some((amount_a.saturating_add(out_a), rem_b))
    }
}

fn zero_liquidity_plan(shape: DepositShape, note: impl Into<String>) -> LiquidityDeltaPlan {
    LiquidityDeltaPlan {
        liquidity_delta: 0,
        amount_a: 0,
        amount_b: 0,
        shape,
        sizing: DepositSizing::Exact,
        dlmm_strategy: None,
        note: note.into(),
    }
}

/// Create-position ΔL: applies swap-to-match sizing when set, and refuses single-sided
/// shapes that still need the other token at the current price.
pub fn liquidity_delta_for_open(
    amount_a: u128,
    amount_b: u128,
    sqrt_price: u128,
    tick_lower: i32,
    tick_upper: i32,
    shape: DepositShape,
    swap_to_match: bool,
) -> LiquidityDeltaPlan {
    let shape = effective_deposit_shape(amount_a, amount_b, shape);

    if swap_to_match {
        if let Some((post_a, post_b)) =
            estimate_post_swap_amounts(amount_a, amount_b, sqrt_price, tick_lower, tick_upper, shape)
        {
            let mut plan = liquidity_delta_from_amounts(
                post_a,
                post_b,
                sqrt_price,
                tick_lower,
                tick_upper,
                DepositShape::Dual,
            );
            plan.note = format!("swap-to-match · {}", plan.note);
            return plan;
        }
        // No swap needed (already matched / out-of-range single-side) — fall through.
    }

    let plan = liquidity_delta_from_amounts(
        amount_a,
        amount_b,
        sqrt_price,
        tick_lower,
        tick_upper,
        shape,
    );
    if plan.liquidity_delta == 0 {
        return plan;
    }
    // Single-sided open is only valid when the range needs solely that token.
    match shape {
        DepositShape::SingleA if plan.amount_b > 0 => zero_liquidity_plan(
            shape,
            "single-A · range still needs token B · enable swap-to-match or pick range above price",
        ),
        DepositShape::SingleB if plan.amount_a > 0 => zero_liquidity_plan(
            shape,
            "single-B · range still needs token A · enable swap-to-match or pick range below price",
        ),
        _ => plan,
    }
}

/// Target A/B ratio at current price for swap-to-match (human B per A scale ignored — raw).
pub fn deposit_ratio_planner(
    sqrt_price: u128,
    tick_lower: i32,
    tick_upper: i32,
    unit_liquidity: u128,
) -> (u128, u128) {
    amounts_from_liquidity(unit_liquidity.max(1), sqrt_price, tick_lower, tick_upper)
}

/// Suggested swap leg to rebalance wallet amounts toward the in-range deposit ratio.
#[derive(Debug, Clone, Copy)]
pub struct SwapToMatchHint {
    /// If true, swap token A → B; else B → A.
    pub in_is_a: bool,
    pub swap_in_amount: u64,
}

/// Heuristic: spend part of the overweight side so remaining A/B ≈ pool unit ratio.
///
/// Single-side sizing weights `(ua, ub)` by spot value in token B — never add raw
/// amounts (WSOL lamports ≫ USDC micro-units would swap ~all of a USDC budget).
pub fn swap_to_match_hint(
    amount_a: u64,
    amount_b: u64,
    sqrt_price: u128,
    tick_lower: i32,
    tick_upper: i32,
    shape: DepositShape,
) -> Option<SwapToMatchHint> {
    let (ua, ub) = amounts_from_liquidity(1u128 << 48, sqrt_price, tick_lower, tick_upper);
    if ua == 0 && ub == 0 {
        return None;
    }
    let side = range_deposit_side(sqrt_price, tick_lower, tick_upper, tick_lower);

    // Out of range is binary:
    //   correct token  → no swap
    //   wrong token    → full swap into the needed token
    match side {
        RangeDepositSide::TokenAOnly => {
            // Need A only. Funding B (or Dual with B) → swap all B→A.
            let wrong_b = match shape {
                DepositShape::SingleB => amount_b,
                DepositShape::Dual => amount_b,
                DepositShape::SingleA => 0,
            };
            if wrong_b > 0 {
                return Some(SwapToMatchHint {
                    in_is_a: false,
                    swap_in_amount: wrong_b,
                });
            }
            return None;
        }
        RangeDepositSide::TokenBOnly => {
            // Need B only. Funding A (or Dual with A) → swap all A→B.
            let wrong_a = match shape {
                DepositShape::SingleA => amount_a,
                DepositShape::Dual => amount_a,
                DepositShape::SingleB => 0,
            };
            if wrong_a > 0 {
                return Some(SwapToMatchHint {
                    in_is_a: true,
                    swap_in_amount: wrong_a,
                });
            }
            return None;
        }
        RangeDepositSide::Both => {}
    }

    // In-range: proportional swap to match deposit ratio (leave ≥1 of input for the other leg).
    let va = value_a_in_b(ua, sqrt_price);
    let vb = ub;
    match shape {
        DepositShape::SingleA => {
            if amount_a < 2 || vb == 0 {
                return None;
            }
            let denom = va.saturating_add(vb).max(1);
            let mut swap_in = ((amount_a as u128) * vb / denom) as u64;
            swap_in = swap_in.clamp(1, amount_a.saturating_sub(1));
            Some(SwapToMatchHint {
                in_is_a: true,
                swap_in_amount: swap_in,
            })
        }
        DepositShape::SingleB => {
            if amount_b < 2 || va == 0 {
                return None;
            }
            let denom = va.saturating_add(vb).max(1);
            let mut swap_in = ((amount_b as u128) * va / denom) as u64;
            swap_in = swap_in.clamp(1, amount_b.saturating_sub(1));
            Some(SwapToMatchHint {
                in_is_a: false,
                swap_in_amount: swap_in,
            })
        }
        DepositShape::Dual => {
            if amount_a == 0 || amount_b == 0 || ua == 0 || ub == 0 {
                return None;
            }
            let scale_a = (amount_a as u128).saturating_mul(ub);
            let scale_b = (amount_b as u128).saturating_mul(ua);
            if scale_a > scale_b {
                let target_a = (amount_b as u128).saturating_mul(ua) / ub;
                let excess = (amount_a as u128).saturating_sub(target_a) as u64;
                if excess == 0 {
                    return None;
                }
                Some(SwapToMatchHint {
                    in_is_a: true,
                    swap_in_amount: excess.min(amount_a),
                })
            } else if scale_b > scale_a {
                let target_b = (amount_a as u128).saturating_mul(ub) / ua;
                let excess = (amount_b as u128).saturating_sub(target_b) as u64;
                if excess == 0 {
                    return None;
                }
                Some(SwapToMatchHint {
                    in_is_a: false,
                    swap_in_amount: excess.min(amount_b),
                })
            } else {
                None
            }
        }
    }
}

#[cfg(test)]
mod liq_shape_tests {
    use super::*;
    use crate::clmm_math::get_sqrt_price_at_tick;

    #[test]
    fn range_side_above_is_b_only() {
        let sp = get_sqrt_price_at_tick(-19000).unwrap();
        assert_eq!(
            range_deposit_side(sp, -23544, -23472, -19000),
            RangeDepositSide::TokenBOnly
        );
    }

    #[test]
    fn analyze_single_b_above_no_swap() {
        let sp = get_sqrt_price_at_tick(-19000).unwrap();
        let an = analyze_deposit(
            0,
            217_989,
            sp,
            -23544,
            -23472,
            -19000,
            DepositShape::SingleB,
        );
        assert!(!an.swap_needed);
        assert!(an.liquidity_no_swap > 0);
        assert!(an.shape_ok_without_swap);
    }

    #[test]
    fn analyze_single_b_in_range_needs_swap() {
        let sp = get_sqrt_price_at_tick(-23500).unwrap();
        let an = analyze_deposit(
            0,
            10_000_000,
            sp,
            -23544,
            -23472,
            -23500,
            DepositShape::SingleB,
        );
        assert!(!an.shape_ok_without_swap);
        assert!(an.swap_needed);
        assert!(an.liquidity_with_swap > 0);
    }

    #[test]
    fn zero_budget_no_phantom_liquidity() {
        // Narrow in-range band near upper: tiny L rounds to (0,0) amounts in Q64.
        // Must not report ΔL>0 with spend 0 (create wizard "ok · ΔL 46 · spend ~0").
        let tick = -23493;
        let sp = get_sqrt_price_at_tick(tick).unwrap();
        let lower = -23936;
        let upper = -23436;
        let plan = liquidity_delta_from_amounts(0, 0, sp, lower, upper, DepositShape::Dual);
        assert_eq!(plan.liquidity_delta, 0);
        assert_eq!(plan.amount_a, 0);
        assert_eq!(plan.amount_b, 0);
        let an = analyze_deposit(0, 0, sp, lower, upper, tick, DepositShape::Dual);
        assert_eq!(an.liquidity_no_swap, 0);
        assert_eq!(an.spend_a_no_swap, 0);
        assert_eq!(an.spend_b_no_swap, 0);
    }

    #[test]
    fn single_b_above_range_small_budget() {
        // price above range → only B; modest USDC must yield L>0 (was 0 with 64-iter search).
        let sp = get_sqrt_price_at_tick(-19000).unwrap();
        let plan = liquidity_delta_for_open(
            0,
            217_989,
            sp,
            -23544,
            -23472,
            DepositShape::SingleB,
            false,
        );
        assert!(plan.liquidity_delta > 0, "note={}", plan.note);
        assert_eq!(plan.amount_a, 0);
        assert!(plan.amount_b > 0 && plan.amount_b <= 217_989);
    }

    #[test]
    fn single_b_in_range_without_swap_is_zero() {
        let sp = get_sqrt_price_at_tick(-23500).unwrap();
        let plan = liquidity_delta_for_open(
            0,
            217_989,
            sp,
            -23544,
            -23472,
            DepositShape::SingleB,
            false,
        );
        assert_eq!(plan.liquidity_delta, 0);
    }

    #[test]
    fn single_b_in_range_with_swap_nonzero() {
        let sp = get_sqrt_price_at_tick(-23500).unwrap();
        let plan = liquidity_delta_for_open(
            0,
            10_000_000,
            sp,
            -23544,
            -23472,
            DepositShape::SingleB,
            true,
        );
        assert!(plan.liquidity_delta > 0, "note={}", plan.note);
        assert!(plan.amount_a > 0 && plan.amount_b > 0);
    }

    #[test]
    fn dual_a0_b_coerces_above_range() {
        let sp = get_sqrt_price_at_tick(-19000).unwrap();
        let plan = liquidity_delta_for_open(
            0,
            217_989,
            sp,
            -23544,
            -23472,
            DepositShape::Dual,
            false,
        );
        assert_eq!(plan.shape, DepositShape::SingleB);
        assert!(plan.liquidity_delta > 0);
    }

    // ── helpers ──────────────────────────────────────────────────────────

    fn sol_usdc_spot() -> (i32, u128) {
        let tick = -23028;
        (tick, get_sqrt_price_at_tick(tick).unwrap())
    }

    /// Old (buggy) single-B fraction: raw ua/(ua+ub) — overweights lamports vs USDC.
    fn legacy_raw_single_b_frac(sp: u128, lower: i32, upper: i32) -> f64 {
        let (ua, ub) = amounts_from_liquidity(1u128 << 48, sp, lower, upper);
        ua as f64 / (ua + ub).max(1) as f64
    }

    fn value_weighted_single_b_frac(sp: u128, lower: i32, upper: i32) -> f64 {
        let (ua, ub) = amounts_from_liquidity(1u128 << 48, sp, lower, upper);
        let va = value_a_in_b(ua, sp) as f64;
        let vb = ub as f64;
        va / (va + vb).max(1.0)
    }

    fn post_swap_from_hint(
        amount_a: u128,
        amount_b: u128,
        sp: u128,
        hint: SwapToMatchHint,
    ) -> (u128, u128) {
        let swap_in = u128::from(hint.swap_in_amount);
        if hint.in_is_a {
            (
                amount_a.saturating_sub(swap_in),
                amount_b.saturating_add(value_a_in_b(swap_in, sp)),
            )
        } else {
            (
                amount_a.saturating_add(amount_a_from_b(swap_in, sp)),
                amount_b.saturating_sub(swap_in),
            )
        }
    }

    /// Relative error of post-swap A/B vs unit deposit ratio (value-space tolerant).
    fn ratio_err(post_a: u128, post_b: u128, ua: u128, ub: u128) -> f64 {
        if ua == 0 || ub == 0 || post_a == 0 || post_b == 0 {
            return f64::INFINITY;
        }
        let got = post_a as f64 / post_b as f64;
        let want = ua as f64 / ub as f64;
        (got - want).abs() / want
    }

    // ── swap_to_match_hint: SingleB ──────────────────────────────────────

    #[test]
    fn single_b_symmetric_range_swaps_near_half() {
        // SOL/USDC-like: raw A (9dp) ≫ raw B (6dp). Old ua/(ua+ub) swapped ~90%+ of USDC.
        let (tick, sp) = sol_usdc_spot();
        let lower = tick - 64;
        let upper = tick + 64;
        let amount_b = 1_000_000u64; // 1 USDC
        let hint = swap_to_match_hint(0, amount_b, sp, lower, upper, DepositShape::SingleB)
            .expect("in-range single-B needs a swap");
        assert!(!hint.in_is_a);
        let frac = hint.swap_in_amount as f64 / amount_b as f64;
        assert!(
            (0.35..0.65).contains(&frac),
            "expected ~50% of USDC swapped, got {frac:.3} ({})",
            hint.swap_in_amount
        );
        let legacy = legacy_raw_single_b_frac(sp, lower, upper);
        assert!(
            legacy > 0.85,
            "regression guard: legacy raw frac should still be huge ({legacy:.3})"
        );
        assert!(
            frac + 0.2 < legacy,
            "value-weighted ({frac:.3}) must beat raw ({legacy:.3})"
        );
    }

    #[test]
    fn single_b_near_upper_swaps_less_than_half() {
        let lower = -23544;
        let upper = -23472;
        let tick = -23480;
        let sp = get_sqrt_price_at_tick(tick).unwrap();
        let amount_b = 10_000_000u64;
        let hint = swap_to_match_hint(0, amount_b, sp, lower, upper, DepositShape::SingleB)
            .expect("still in range");
        let frac = hint.swap_in_amount as f64 / amount_b as f64;
        assert!(
            frac < 0.45,
            "B-heavy range should swap under half, got {frac:.3}"
        );
    }

    #[test]
    fn single_b_near_lower_swaps_more_than_half() {
        // Spot near lower → A-heavy deposit → most of USDC must become WSOL.
        let lower = -23544;
        let upper = -23472;
        let tick = -23536;
        let sp = get_sqrt_price_at_tick(tick).unwrap();
        let amount_b = 10_000_000u64;
        let hint = swap_to_match_hint(0, amount_b, sp, lower, upper, DepositShape::SingleB)
            .expect("still in range");
        let frac = hint.swap_in_amount as f64 / amount_b as f64;
        assert!(
            frac > 0.55,
            "A-heavy range should swap over half of B, got {frac:.3}"
        );
    }

    #[test]
    fn single_b_above_range_no_hint() {
        let sp = get_sqrt_price_at_tick(-19000).unwrap();
        assert!(
            swap_to_match_hint(0, 1_000_000, sp, -23544, -23472, DepositShape::SingleB).is_none(),
            "B-only range needs no swap"
        );
    }

    #[test]
    fn single_a_above_range_full_swap_to_b() {
        // price above range → need B only · funding A → full A→B
        let sp = get_sqrt_price_at_tick(-19000).unwrap();
        let amount_a = 10_000_000u64;
        let hint = swap_to_match_hint(amount_a, 0, sp, -23544, -23472, DepositShape::SingleA)
            .expect("wrong-side out-of-range must full-swap");
        assert!(hint.in_is_a);
        assert_eq!(hint.swap_in_amount, amount_a);
    }

    #[test]
    fn single_b_below_range_full_swap_to_a() {
        // price below range → need A only · funding B → full B→A
        let sp = get_sqrt_price_at_tick(-24000).unwrap();
        let amount_b = 5_000_000u64;
        let hint = swap_to_match_hint(0, amount_b, sp, -23544, -23472, DepositShape::SingleB)
            .expect("wrong-side out-of-range must full-swap");
        assert!(!hint.in_is_a);
        assert_eq!(hint.swap_in_amount, amount_b);
    }

    #[test]
    fn single_b_dust_budget_no_hint() {
        let (tick, sp) = sol_usdc_spot();
        assert!(
            swap_to_match_hint(0, 1, sp, tick - 64, tick + 64, DepositShape::SingleB).is_none()
        );
    }

    #[test]
    fn single_b_never_swaps_entire_budget() {
        let (tick, sp) = sol_usdc_spot();
        for &budget in &[2u64, 100, 1_000_000, 50_000_000, u64::MAX / 4] {
            if let Some(h) =
                swap_to_match_hint(0, budget, sp, tick - 128, tick + 128, DepositShape::SingleB)
            {
                assert!(
                    h.swap_in_amount < budget,
                    "must leave ≥1 raw B (budget={budget}, swap={})",
                    h.swap_in_amount
                );
                assert!(h.swap_in_amount >= 1);
            }
        }
    }

    #[test]
    fn single_b_frac_matches_value_weights_across_widths() {
        let (tick, sp) = sol_usdc_spot();
        let budget = 5_000_000u64;
        for half in [8, 16, 32, 64, 128, 256, 512] {
            let lower = tick - half;
            let upper = tick + half;
            let hint =
                swap_to_match_hint(0, budget, sp, lower, upper, DepositShape::SingleB).unwrap();
            let got = hint.swap_in_amount as f64 / budget as f64;
            let want = value_weighted_single_b_frac(sp, lower, upper);
            assert!(
                (got - want).abs() < 0.002,
                "half={half}: got={got:.4} want={want:.4}"
            );
        }
    }

    #[test]
    fn single_b_post_swap_ratio_tracks_unit_deposit() {
        let (tick, sp) = sol_usdc_spot();
        for half in [32, 64, 128, 256] {
            let lower = tick - half;
            let upper = tick + half;
            let budget = 10_000_000u128;
            let hint = swap_to_match_hint(
                0,
                budget as u64,
                sp,
                lower,
                upper,
                DepositShape::SingleB,
            )
            .unwrap();
            let (pa, pb) = post_swap_from_hint(0, budget, sp, hint);
            let (ua, ub) = deposit_ratio_planner(sp, lower, upper, 1u128 << 48);
            let err = ratio_err(pa, pb, ua, ub);
            assert!(
                err < 0.02,
                "half={half}: ratio err {err:.4} post=({pa},{pb}) unit=({ua},{ub})"
            );
        }
    }

    // ── swap_to_match_hint: SingleA ──────────────────────────────────────

    #[test]
    fn single_a_symmetric_swaps_near_half() {
        let (tick, sp) = sol_usdc_spot();
        let lower = tick - 64;
        let upper = tick + 64;
        let amount_a = 100_000_000u64; // 0.1 SOL
        let hint = swap_to_match_hint(amount_a, 0, sp, lower, upper, DepositShape::SingleA)
            .expect("needs swap");
        assert!(hint.in_is_a);
        let frac = hint.swap_in_amount as f64 / amount_a as f64;
        assert!(
            (0.35..0.65).contains(&frac),
            "expected ~50% of A swapped, got {frac:.3}"
        );
    }

    #[test]
    fn single_a_near_lower_swaps_less_than_half() {
        // Near lower → A-heavy → keep most A, swap little to B.
        let lower = -23544;
        let upper = -23472;
        let tick = -23536;
        let sp = get_sqrt_price_at_tick(tick).unwrap();
        let amount_a = 100_000_000u64;
        let hint = swap_to_match_hint(amount_a, 0, sp, lower, upper, DepositShape::SingleA)
            .unwrap();
        let frac = hint.swap_in_amount as f64 / amount_a as f64;
        assert!(frac < 0.45, "A-heavy should swap under half of A, got {frac:.3}");
    }

    #[test]
    fn single_a_below_range_no_hint() {
        let sp = get_sqrt_price_at_tick(-30000).unwrap();
        assert!(
            swap_to_match_hint(1_000_000_000, 0, sp, -23544, -23472, DepositShape::SingleA)
                .is_none(),
            "A-only range needs no swap"
        );
    }

    #[test]
    fn single_a_post_swap_ratio_tracks_unit_deposit() {
        let (tick, sp) = sol_usdc_spot();
        let lower = tick - 128;
        let upper = tick + 128;
        let budget = 500_000_000u128; // 0.5 SOL
        let hint =
            swap_to_match_hint(budget as u64, 0, sp, lower, upper, DepositShape::SingleA).unwrap();
        let (pa, pb) = post_swap_from_hint(budget, 0, sp, hint);
        let (ua, ub) = deposit_ratio_planner(sp, lower, upper, 1u128 << 48);
        let err = ratio_err(pa, pb, ua, ub);
        assert!(err < 0.02, "ratio err {err:.4} post=({pa},{pb}) unit=({ua},{ub})");
    }

    // ── Dual rebalance ───────────────────────────────────────────────────

    #[test]
    fn dual_balanced_no_hint() {
        let (tick, sp) = sol_usdc_spot();
        let lower = tick - 64;
        let upper = tick + 64;
        // Same unit L as swap_to_match_hint — ratio must already match.
        let (ua, ub) = deposit_ratio_planner(sp, lower, upper, 1u128 << 48);
        assert!(ua <= u128::from(u64::MAX) && ub <= u128::from(u64::MAX));
        let a = ua as u64;
        let b = ub as u64;
        assert!(
            swap_to_match_hint(a, b, sp, lower, upper, DepositShape::Dual).is_none(),
            "already matched a={a} b={b}"
        );
        // Scaled copy stays matched (linear in L).
        let a2 = (ua / 1_000).max(1) as u64;
        let b2 = (ub / 1_000).max(1) as u64;
        match swap_to_match_hint(a2, b2, sp, lower, upper, DepositShape::Dual) {
            None => {}
            Some(h) => {
                let base = if h.in_is_a { a2 } else { b2 };
                let dust = h.swap_in_amount as f64 / base as f64;
                assert!(dust < 0.002, "scaled match only allows dust swap, got {dust:.4}");
            }
        }
    }

    #[test]
    fn dual_excess_b_swaps_b_to_a() {
        let (tick, sp) = sol_usdc_spot();
        let lower = tick - 64;
        let upper = tick + 64;
        let (ua, ub) = deposit_ratio_planner(sp, lower, upper, 1u128 << 40);
        let a = (ua / 4).min(u128::from(u64::MAX)) as u64;
        let b = ub.min(u128::from(u64::MAX)) as u64; // 4× overweight B vs matched A
        let hint = swap_to_match_hint(a, b, sp, lower, upper, DepositShape::Dual).unwrap();
        assert!(!hint.in_is_a);
        assert!(hint.swap_in_amount > 0 && hint.swap_in_amount < b);
    }

    #[test]
    fn dual_excess_a_swaps_a_to_b() {
        let (tick, sp) = sol_usdc_spot();
        let lower = tick - 64;
        let upper = tick + 64;
        let (ua, ub) = deposit_ratio_planner(sp, lower, upper, 1u128 << 40);
        let a = ua.min(u128::from(u64::MAX)) as u64;
        let b = (ub / 4).min(u128::from(u64::MAX)) as u64;
        let hint = swap_to_match_hint(a, b, sp, lower, upper, DepositShape::Dual).unwrap();
        assert!(hint.in_is_a);
        assert!(hint.swap_in_amount > 0 && hint.swap_in_amount < a);
    }

    // ── liquidity_delta_for_open + analyze ───────────────────────────────

    #[test]
    fn open_single_b_with_swap_spends_both_near_unit_ratio() {
        let (tick, sp) = sol_usdc_spot();
        let lower = tick - 64;
        let upper = tick + 64;
        let budget = 2_000_000u128; // 2 USDC
        let plan = liquidity_delta_for_open(
            0,
            budget,
            sp,
            lower,
            upper,
            DepositShape::SingleB,
            true,
        );
        assert!(plan.liquidity_delta > 0, "note={}", plan.note);
        assert!(plan.amount_a > 0 && plan.amount_b > 0);
        let (ua, ub) = deposit_ratio_planner(sp, lower, upper, 1u128 << 48);
        let err = ratio_err(plan.amount_a, plan.amount_b, ua, ub);
        assert!(
            err < 0.05,
            "open plan ratio err {err:.4} plan=({},{}) unit=({ua},{ub})",
            plan.amount_a,
            plan.amount_b
        );
        // Must not dump almost-all USDC into WSOL on a symmetric range.
        let usdc_left_frac = plan.amount_b as f64 / budget as f64;
        assert!(
            usdc_left_frac > 0.30,
            "symmetric open left only {usdc_left_frac:.3} of USDC (bug: raw ua+ub)"
        );
    }

    #[test]
    fn open_single_a_with_swap_keeps_meaningful_a() {
        let (tick, sp) = sol_usdc_spot();
        let lower = tick - 64;
        let upper = tick + 64;
        let budget = 200_000_000u128; // 0.2 SOL
        let plan = liquidity_delta_for_open(
            budget,
            0,
            sp,
            lower,
            upper,
            DepositShape::SingleA,
            true,
        );
        assert!(plan.liquidity_delta > 0, "note={}", plan.note);
        let a_left_frac = plan.amount_a as f64 / budget as f64;
        assert!(
            a_left_frac > 0.30,
            "symmetric open left only {a_left_frac:.3} of A"
        );
    }

    #[test]
    fn analyze_single_b_symmetric_swap_note_sane() {
        let (tick, sp) = sol_usdc_spot();
        let lower = tick - 64;
        let upper = tick + 64;
        let budget = 1_000_000u128;
        let an = analyze_deposit(
            0,
            budget,
            sp,
            lower,
            upper,
            tick,
            DepositShape::SingleB,
        );
        assert!(an.swap_needed);
        assert!(!an.shape_ok_without_swap);
        assert!(an.liquidity_with_swap > an.liquidity_no_swap);
        let spend_b = an.spend_b_with_swap;
        // After value-weighted swap, remaining B in the matched plan should be substantial.
        assert!(
            spend_b as f64 > 0.30 * budget as f64,
            "spend_b_with_swap={spend_b} budget={budget}"
        );
    }

    #[test]
    fn single_a_and_single_b_value_fracs_sum_near_one() {
        // For the same range, A→B frac + B→A frac ≈ 1 (complementary value shares).
        let (tick, sp) = sol_usdc_spot();
        for half in [32i32, 64, 128, 256] {
            let lower = tick - half;
            let upper = tick + half;
            let a_budget = 1_000_000_000u64;
            let b_budget = 100_000_000u64;
            let ha =
                swap_to_match_hint(a_budget, 0, sp, lower, upper, DepositShape::SingleA).unwrap();
            let hb =
                swap_to_match_hint(0, b_budget, sp, lower, upper, DepositShape::SingleB).unwrap();
            let fa = ha.swap_in_amount as f64 / a_budget as f64;
            let fb = hb.swap_in_amount as f64 / b_budget as f64;
            assert!(
                (fa + fb - 1.0).abs() < 0.01,
                "half={half}: fa={fa:.4} fb={fb:.4} sum={}",
                fa + fb
            );
        }
    }

    #[test]
    fn price_one_symmetric_near_exact_half() {
        // At tick 0, price=1 and symmetric ticks → near-equal raw legs → ~50%
        // (1 ulp of amount math can yield 499999 vs 500000).
        let sp = get_sqrt_price_at_tick(0).unwrap();
        let lower = -64;
        let upper = 64;
        let budget = 1_000_000u64;
        let hb = swap_to_match_hint(0, budget, sp, lower, upper, DepositShape::SingleB).unwrap();
        let ha = swap_to_match_hint(budget, 0, sp, lower, upper, DepositShape::SingleA).unwrap();
        assert!(
            hb.swap_in_amount.abs_diff(budget / 2) <= 1,
            "SingleB swap {} vs half {}",
            hb.swap_in_amount,
            budget / 2
        );
        assert!(
            ha.swap_in_amount.abs_diff(budget / 2) <= 1,
            "SingleA swap {} vs half {}",
            ha.swap_in_amount,
            budget / 2
        );
        assert!((ha.swap_in_amount as i64 + hb.swap_in_amount as i64 - budget as i64).abs() <= 2);
    }
}