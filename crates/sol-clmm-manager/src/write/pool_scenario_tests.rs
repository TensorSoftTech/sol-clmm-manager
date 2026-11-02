//! Fixture-driven swap-to-match / open-sizing scenarios for real CLMM pairs.
//!
//! Pools are mock snapshots (decimals + human mid + spacing) — no RPC.

use super::liquidity::{
    DepositShape, analyze_deposit, deposit_ratio_planner, liquidity_delta_for_open,
    swap_to_match_hint,
};
use crate::clmm_math::{
    amount_a_from_b, amounts_from_liquidity, get_sqrt_price_at_tick, parse_ui_amount, value_a_in_b,
};
use crate::pricing::human_price_b_per_a;

/// Mock whirlpool / CLMM pool used as a deposit fixture.
#[derive(Clone, Copy, Debug)]
struct MockPool {
    name: &'static str,
    symbol_a: &'static str,
    symbol_b: &'static str,
    decimals_a: u8,
    decimals_b: u8,
    tick_spacing: i32,
    /// Human mid price as token B per token A (e.g. USDC per WSOL).
    human_mid: f64,
}

impl MockPool {
    fn tick_at_mid(self) -> i32 {
        tick_from_human(self.human_mid, self.decimals_a, self.decimals_b)
    }

    /// Align a tick to spacing (Whirlpool open requires spacing multiples).
    fn align(self, tick: i32) -> i32 {
        let s = self.tick_spacing;
        tick.div_euclid(s) * s
    }

    /// Spacing-aligned mid used as both spot and range center (avoids false skew).
    fn mid_tick(self) -> i32 {
        self.align(self.tick_at_mid())
    }

    fn sqrt_price(self) -> u128 {
        get_sqrt_price_at_tick(self.mid_tick()).expect("tick in range")
    }

    /// Symmetric ±`spacings` around aligned mid.
    fn range_spacings(self, spacings: i32) -> (i32, i32, i32) {
        let mid = self.mid_tick();
        let lower = mid - spacings * self.tick_spacing;
        let upper = mid + spacings * self.tick_spacing;
        assert!(lower < upper);
        (lower, upper, mid)
    }

    fn raw_a(self, ui: &str) -> u64 {
        parse_ui_amount(ui, self.decimals_a).expect("amount a")
    }

    fn raw_b(self, ui: &str) -> u64 {
        parse_ui_amount(ui, self.decimals_b).expect("amount b")
    }

    fn verify_mid_price(self) {
        // Unaligned theoretical tick — allow spacing snap error (~spacing * 1 bp).
        let sp = get_sqrt_price_at_tick(self.mid_tick()).unwrap();
        let got = human_price_b_per_a(sp, self.decimals_a, self.decimals_b).expect("human price");
        let rel = (got - self.human_mid).abs() / self.human_mid;
        let tol = (self.tick_spacing as f64 * 0.0001 * 2.0).max(0.01);
        assert!(
            rel < tol,
            "{}: mid tick price {got} vs fixture {} (rel {rel:.4}, tol {tol:.4})",
            self.name,
            self.human_mid
        );
    }
}

fn tick_from_human(human_b_per_a: f64, decimals_a: u8, decimals_b: u8) -> i32 {
    assert!(human_b_per_a > 0.0);
    let raw = human_b_per_a * 10f64.powi(decimals_b as i32 - decimals_a as i32);
    (raw.ln() / 1.0001f64.ln()).round() as i32
}

// ── well-known pool fixtures ─────────────────────────────────────────────

/// Orca-style SOL/USDC whirlpool (A=WSOL 9dp, B=USDC 6dp), mid ≈ $150.
const WSOL_USDC: MockPool = MockPool {
    name: "WSOL/USDC",
    symbol_a: "WSOL",
    symbol_b: "USDC",
    decimals_a: 9,
    decimals_b: 6,
    tick_spacing: 64,
    human_mid: 150.0,
};

/// Same pair at a different mid (stress tick math).
const WSOL_USDC_CHEAP: MockPool = MockPool {
    name: "WSOL/USDC@$80",
    symbol_a: "WSOL",
    symbol_b: "USDC",
    decimals_a: 9,
    decimals_b: 6,
    tick_spacing: 64,
    human_mid: 80.0,
};

/// cbBTC as A (8dp), USDC as B (6dp), mid ≈ $95,000 USDC per BTC.
const CBBTC_USDC: MockPool = MockPool {
    name: "cbBTC/USDC",
    symbol_a: "cbBTC",
    symbol_b: "USDC",
    decimals_a: 8,
    decimals_b: 6,
    tick_spacing: 64,
    human_mid: 95_000.0,
};

/// Reverse mint order: USDC A, cbBTC B — human = BTC per USDC ≈ 1/95000.
const USDC_CBBTC: MockPool = MockPool {
    name: "USDC/cbBTC",
    symbol_a: "USDC",
    symbol_b: "cbBTC",
    decimals_a: 6,
    decimals_b: 8,
    tick_spacing: 64,
    human_mid: 1.0 / 95_000.0,
};

/// Stable-ish USDC/USDT, mid ≈ 1.0, both 6dp.
const USDC_USDT: MockPool = MockPool {
    name: "USDC/USDT",
    symbol_a: "USDC",
    symbol_b: "USDT",
    decimals_a: 6,
    decimals_b: 6,
    tick_spacing: 1,
    human_mid: 1.0001,
};

/// WSOL/USDT (9/6) — same decimal trap as WSOL/USDC.
const WSOL_USDT: MockPool = MockPool {
    name: "WSOL/USDT",
    symbol_a: "WSOL",
    symbol_b: "USDT",
    decimals_a: 9,
    decimals_b: 6,
    tick_spacing: 64,
    human_mid: 150.0,
};

fn all_pools() -> [MockPool; 6] {
    [
        WSOL_USDC,
        WSOL_USDC_CHEAP,
        CBBTC_USDC,
        USDC_CBBTC,
        USDC_USDT,
        WSOL_USDT,
    ]
}

fn legacy_raw_frac_b(sp: u128, lower: i32, upper: i32) -> f64 {
    let (ua, ub) = amounts_from_liquidity(1u128 << 48, sp, lower, upper);
    ua as f64 / (ua + ub).max(1) as f64
}

fn swap_frac(hint_in: u64, budget: u64) -> f64 {
    hint_in as f64 / budget as f64
}

fn post_from_hint(
    amount_a: u128,
    amount_b: u128,
    sp: u128,
    in_is_a: bool,
    swap_in: u64,
) -> (u128, u128) {
    let s = u128::from(swap_in);
    if in_is_a {
        (
            amount_a.saturating_sub(s),
            amount_b.saturating_add(value_a_in_b(s, sp)),
        )
    } else {
        (
            amount_a.saturating_add(amount_a_from_b(s, sp)),
            amount_b.saturating_sub(s),
        )
    }
}

fn ratio_err(post_a: u128, post_b: u128, ua: u128, ub: u128) -> f64 {
    if post_a == 0 || post_b == 0 || ua == 0 || ub == 0 {
        return f64::INFINITY;
    }
    let got = post_a as f64 / post_b as f64;
    let want = ua as f64 / ub as f64;
    (got - want).abs() / want
}

// ── fixture sanity ───────────────────────────────────────────────────────

#[test]
fn fixtures_mid_price_matches_tick() {
    for p in all_pools() {
        p.verify_mid_price();
    }
}

#[test]
fn fixtures_symmetric_range_is_both_sides() {
    for p in all_pools() {
        let sp = p.sqrt_price();
        let (lo, hi, mid) = p.range_spacings(2);
        let (ua, ub) = deposit_ratio_planner(sp, lo, hi, 1u128 << 48);
        assert!(
            ua > 0 && ub > 0,
            "{}: expected both legs at mid={mid}",
            p.name
        );
    }
}

// ── WSOL/USDC mock positions ─────────────────────────────────────────────

#[test]
fn wsol_usdc_single_usdc_tiny_position_swaps_near_half() {
    let p = WSOL_USDC;
    let sp = p.sqrt_price();
    let (lo, hi, _) = p.range_spacings(1);
    // ~$0.05 USDC — matches the live debug size the user hit.
    let budget = p.raw_b("0.05");
    let hint = swap_to_match_hint(0, budget, sp, lo, hi, DepositShape::SingleB).unwrap();
    assert!(!hint.in_is_a);
    let frac = swap_frac(hint.swap_in_amount, budget);
    assert!(
        (0.35..0.65).contains(&frac),
        "{} tiny USDC swap frac {frac:.3}",
        p.name
    );
    assert!(
        legacy_raw_frac_b(sp, lo, hi) > 0.85,
        "legacy raw must still look broken on WSOL/USDC"
    );
}

#[test]
fn wsol_usdc_single_usdc_medium_and_large_same_frac() {
    let p = WSOL_USDC;
    let sp = p.sqrt_price();
    let (lo, hi, _) = p.range_spacings(2);
    let small = p.raw_b("1");
    let big = p.raw_b("1000");
    let hs = swap_to_match_hint(0, small, sp, lo, hi, DepositShape::SingleB).unwrap();
    let hb = swap_to_match_hint(0, big, sp, lo, hi, DepositShape::SingleB).unwrap();
    let fs = swap_frac(hs.swap_in_amount, small);
    let fb = swap_frac(hb.swap_in_amount, big);
    assert!(
        (fs - fb).abs() < 0.001,
        "swap frac must be scale-invariant: {fs} vs {fb}"
    );
}

#[test]
fn wsol_usdc_single_wsol_keeps_half_on_symmetric() {
    let p = WSOL_USDC;
    let sp = p.sqrt_price();
    let (lo, hi, _) = p.range_spacings(2);
    // 0.01 SOL ≈ $1.50
    let budget = p.raw_a("0.01");
    let hint = swap_to_match_hint(budget, 0, sp, lo, hi, DepositShape::SingleA).unwrap();
    let frac = swap_frac(hint.swap_in_amount, budget);
    assert!((0.35..0.65).contains(&frac), "WSOL→USDC frac {frac:.3}");
}

#[test]
fn wsol_usdc_open_single_usdc_leaves_both_legs_usd_balanced() {
    let p = WSOL_USDC;
    let sp = p.sqrt_price();
    let (lo, hi, mid) = p.range_spacings(2);
    let budget = u128::from(p.raw_b("10")); // $10 USDC
    let plan = liquidity_delta_for_open(0, budget, sp, lo, hi, DepositShape::SingleB, true);
    assert!(plan.liquidity_delta > 0, "{}", plan.note);
    assert!(plan.amount_a > 0 && plan.amount_b > 0);

    // Value both legs in USDC raw via spot.
    let a_in_b = value_a_in_b(plan.amount_a, sp);
    let total = a_in_b + plan.amount_b;
    let a_share = a_in_b as f64 / total as f64;
    assert!(
        (0.35..0.65).contains(&a_share),
        "{} open value share A={a_share:.3} (amounts a={} b={}) mid={mid}",
        p.name,
        plan.amount_a,
        plan.amount_b
    );
    // Must not recreate the explorer bug (≈6% USDC left).
    let b_left = plan.amount_b as f64 / budget as f64;
    assert!(b_left > 0.30, "USDC left frac {b_left:.3}");
}

#[test]
fn wsol_usdc_wide_vs_narrow_range_swap_frac_moves() {
    let p = WSOL_USDC;
    let sp = p.sqrt_price();
    let budget = p.raw_b("5");
    let (lo_n, hi_n, mid) = p.range_spacings(1);
    let lo_w = mid - 20 * p.tick_spacing;
    let hi_w = mid + 20 * p.tick_spacing;
    let fn_ = swap_frac(
        swap_to_match_hint(0, budget, sp, lo_n, hi_n, DepositShape::SingleB)
            .unwrap()
            .swap_in_amount,
        budget,
    );
    let fw = swap_frac(
        swap_to_match_hint(0, budget, sp, lo_w, hi_w, DepositShape::SingleB)
            .unwrap()
            .swap_in_amount,
        budget,
    );
    // Both symmetric → both near half regardless of width.
    assert!((0.40..0.60).contains(&fn_), "narrow {fn_:.3}");
    assert!((0.40..0.60).contains(&fw), "wide {fw:.3}");
}

#[test]
fn wsol_usdc_range_above_price_single_usdc_no_swap() {
    let p = WSOL_USDC;
    let mid = p.mid_tick();
    // Entire range below spot → price above range → B only.
    let lo = mid - 40 * p.tick_spacing;
    let hi = mid - 10 * p.tick_spacing;
    let sp = get_sqrt_price_at_tick(mid).unwrap();
    assert!(swap_to_match_hint(0, p.raw_b("1"), sp, lo, hi, DepositShape::SingleB).is_none());
    let plan = liquidity_delta_for_open(
        0,
        u128::from(p.raw_b("1")),
        sp,
        lo,
        hi,
        DepositShape::SingleB,
        false,
    );
    assert!(plan.liquidity_delta > 0);
    assert_eq!(plan.amount_a, 0);
}

#[test]
fn wsol_usdc_range_below_price_single_wsol_no_swap() {
    let p = WSOL_USDC;
    let mid = p.mid_tick();
    let lo = mid + 10 * p.tick_spacing;
    let hi = mid + 40 * p.tick_spacing;
    let sp = get_sqrt_price_at_tick(mid).unwrap();
    assert!(swap_to_match_hint(p.raw_a("0.05"), 0, sp, lo, hi, DepositShape::SingleA).is_none());
}

#[test]
fn wsol_usdc_mock_position_inventory_from_liquidity() {
    // Simulate an open position's token inventory at current price.
    let p = WSOL_USDC;
    let sp = p.sqrt_price();
    let (lo, hi, _) = p.range_spacings(3);
    let l = 1u128 << 40;
    let (a, b) = amounts_from_liquidity(l, sp, lo, hi);
    assert!(a > 0 && b > 0);
    // Rough USD: a_in_usdc + b ≈ order of dollars for this L — just sanity.
    let usd = value_a_in_b(a, sp) + b;
    assert!(usd > 0);
    // Human-readable non-zero.
    assert!(a >= 1_000); // ≥ 0.000001 SOL
    assert!(b >= 1); // ≥ 1 micro-USDC
}

// ── cbBTC pairs ──────────────────────────────────────────────────────────

#[test]
fn cbbtc_usdc_single_usdc_symmetric_near_half() {
    let p = CBBTC_USDC;
    let sp = p.sqrt_price();
    let (lo, hi, _) = p.range_spacings(2);
    // $50 of USDC into a BTC/USDC band.
    let budget = p.raw_b("50");
    let hint = swap_to_match_hint(0, budget, sp, lo, hi, DepositShape::SingleB).unwrap();
    let frac = swap_frac(hint.swap_in_amount, budget);
    assert!(
        (0.35..0.65).contains(&frac),
        "{} SingleB frac {frac:.3}",
        p.name
    );
    // Opposite trap vs WSOL: $1 of cbBTC ≪ $1 of USDC in raw units → legacy under-swaps.
    let legacy = legacy_raw_frac_b(sp, lo, hi);
    assert!(
        legacy < 0.05,
        "legacy raw frac should under-weight BTC ({legacy:.3})"
    );
    assert!(frac > legacy + 0.25);
}

#[test]
fn cbbtc_usdc_single_btc_symmetric_near_half() {
    let p = CBBTC_USDC;
    let sp = p.sqrt_price();
    let (lo, hi, _) = p.range_spacings(2);
    // 0.001 cbBTC ≈ $95
    let budget = p.raw_a("0.001");
    let hint = swap_to_match_hint(budget, 0, sp, lo, hi, DepositShape::SingleA).unwrap();
    let frac = swap_frac(hint.swap_in_amount, budget);
    assert!(
        (0.35..0.65).contains(&frac),
        "{} SingleA frac {frac:.3}",
        p.name
    );
}

#[test]
fn cbbtc_usdc_open_preserves_usd_balance() {
    let p = CBBTC_USDC;
    let sp = p.sqrt_price();
    let (lo, hi, _) = p.range_spacings(2);
    let budget = u128::from(p.raw_b("100"));
    let plan = liquidity_delta_for_open(0, budget, sp, lo, hi, DepositShape::SingleB, true);
    assert!(plan.liquidity_delta > 0, "{}", plan.note);
    let a_in_b = value_a_in_b(plan.amount_a, sp);
    let share = a_in_b as f64 / (a_in_b + plan.amount_b) as f64;
    assert!(
        (0.35..0.65).contains(&share),
        "cbBTC/USDC value share {share:.3}"
    );
}

#[test]
fn usdc_cbbtc_reversed_mints_single_usdc_near_half() {
    // A=USDC, B=cbBTC — SingleA funds USDC and swaps toward cbBTC.
    let p = USDC_CBBTC;
    let sp = p.sqrt_price();
    let (lo, hi, _) = p.range_spacings(2);
    let budget = p.raw_a("100"); // $100 USDC
    let hint = swap_to_match_hint(budget, 0, sp, lo, hi, DepositShape::SingleA).unwrap();
    assert!(hint.in_is_a);
    let frac = swap_frac(hint.swap_in_amount, budget);
    assert!(
        (0.40..0.60).contains(&frac),
        "{} SingleA(USDC) frac {frac:.3}",
        p.name
    );

    let (pa, pb) = post_from_hint(u128::from(budget), 0, sp, true, hint.swap_in_amount);
    let (ua, ub) = deposit_ratio_planner(sp, lo, hi, 1u128 << 48);
    let err = ratio_err(pa, pb, ua, ub);
    assert!(err < 0.03, "{} post-swap ratio err {err:.4}", p.name);
}

#[test]
fn usdc_cbbtc_single_btc_near_half() {
    let p = USDC_CBBTC;
    let sp = p.sqrt_price();
    let (lo, hi, _) = p.range_spacings(2);
    let budget = p.raw_b("0.001"); // ~$95 of BTC
    let hint = swap_to_match_hint(0, budget, sp, lo, hi, DepositShape::SingleB).unwrap();
    let frac = swap_frac(hint.swap_in_amount, budget);
    assert!(
        (0.40..0.60).contains(&frac),
        "{} SingleB frac {frac:.3}",
        p.name
    );
}

// ── stables + cross checks ───────────────────────────────────────────────

#[test]
fn usdc_usdt_near_peg_half_and_open() {
    let p = USDC_USDT;
    let sp = p.sqrt_price();
    let (lo, hi, _) = p.range_spacings(10); // spacing=1 → ±10 ticks
    let budget = p.raw_a("25");
    let hint = swap_to_match_hint(budget, 0, sp, lo, hi, DepositShape::SingleA).unwrap();
    let frac = swap_frac(hint.swap_in_amount, budget);
    assert!(
        (0.35..0.65).contains(&frac),
        "stable SingleA frac {frac:.3}"
    );
    // Same decimals → legacy raw ≈ value-weighted (no decimal trap).
    let legacy = {
        let (ua, ub) = amounts_from_liquidity(1u128 << 48, sp, lo, hi);
        ub as f64 / (ua + ub).max(1) as f64
    };
    assert!((frac - legacy).abs() < 0.05, "6/6 pair legacy≈value");
}

#[test]
fn wsol_usdt_matches_wsol_usdc_behaviour() {
    let a = WSOL_USDC;
    let b = WSOL_USDT;
    let (lo_a, hi_a, _) = a.range_spacings(2);
    let (lo_b, hi_b, _) = b.range_spacings(2);
    let budget = a.raw_b("7.5");
    let fa = swap_frac(
        swap_to_match_hint(0, budget, a.sqrt_price(), lo_a, hi_a, DepositShape::SingleB)
            .unwrap()
            .swap_in_amount,
        budget,
    );
    let fb = swap_frac(
        swap_to_match_hint(0, budget, b.sqrt_price(), lo_b, hi_b, DepositShape::SingleB)
            .unwrap()
            .swap_in_amount,
        budget,
    );
    assert!(
        (fa - fb).abs() < 0.02,
        "WSOL/USDC {fa:.3} vs WSOL/USDT {fb:.3}"
    );
}

#[test]
fn asymmetric_band_mostly_above_spot_is_a_heavy_swaps_more_usdc() {
    // Spot near lower bound → need more A → swap most of USDC.
    for p in [WSOL_USDC, CBBTC_USDC, WSOL_USDT] {
        let mid = p.mid_tick();
        let sp = get_sqrt_price_at_tick(mid).unwrap();
        let lo = mid - p.tick_spacing;
        let hi = mid + 8 * p.tick_spacing;
        let budget = p.raw_b("20");
        let hint = swap_to_match_hint(0, budget, sp, lo, hi, DepositShape::SingleB).unwrap();
        let frac = swap_frac(hint.swap_in_amount, budget);
        assert!(
            frac > 0.70,
            "{} A-heavy band ({}/{}) frac {frac:.3}",
            p.name,
            p.symbol_a,
            p.symbol_b
        );
    }
}

#[test]
fn asymmetric_band_mostly_below_spot_is_b_heavy_swaps_less_usdc() {
    // Spot near upper bound → need more B → swap little USDC.
    for p in [WSOL_USDC, CBBTC_USDC] {
        let mid = p.mid_tick();
        let sp = get_sqrt_price_at_tick(mid).unwrap();
        let lo = mid - 8 * p.tick_spacing;
        let hi = mid + p.tick_spacing;
        let budget = p.raw_b("20");
        let hint = swap_to_match_hint(0, budget, sp, lo, hi, DepositShape::SingleB).unwrap();
        let frac = swap_frac(hint.swap_in_amount, budget);
        assert!(
            frac < 0.30,
            "{} B-heavy band ({}/{}) frac {frac:.3}",
            p.name,
            p.symbol_a,
            p.symbol_b
        );
    }
}

#[test]
fn dual_skewed_usdc_heavy_rebalances_on_wsol_usdc() {
    let p = WSOL_USDC;
    let sp = p.sqrt_price();
    let (lo, hi, _) = p.range_spacings(2);
    let (ua, ub) = deposit_ratio_planner(sp, lo, hi, 1u128 << 32);
    let a = (ua / 10).max(1).min(u128::from(u64::MAX)) as u64;
    let b = ub.min(u128::from(u64::MAX)) as u64;
    let hint = swap_to_match_hint(a, b, sp, lo, hi, DepositShape::Dual).unwrap();
    assert!(!hint.in_is_a, "excess USDC should swap B→A");
    assert!(hint.swap_in_amount > 0);
}

#[test]
fn analyze_deposit_flags_swap_for_single_usdc_in_range() {
    let p = WSOL_USDC;
    let sp = p.sqrt_price();
    let (lo, hi, mid) = p.range_spacings(2);
    let an = analyze_deposit(
        0,
        u128::from(p.raw_b("3")),
        sp,
        lo,
        hi,
        mid,
        DepositShape::SingleB,
    );
    assert!(an.swap_needed);
    assert!(!an.shape_ok_without_swap);
    assert!(an.liquidity_with_swap > 0);
    assert!(an.spend_b_with_swap as f64 > 0.30 * p.raw_b("3") as f64);
}

#[test]
fn matrix_all_pools_single_funded_side_post_swap_ratio() {
    // Every fixture: fund the quote-ish side (B when B is stable, else A for USDC/cbBTC).
    let cases: [(&MockPool, DepositShape, u64); 5] = [
        (&WSOL_USDC, DepositShape::SingleB, WSOL_USDC.raw_b("12")),
        (&WSOL_USDT, DepositShape::SingleB, WSOL_USDT.raw_b("12")),
        (&CBBTC_USDC, DepositShape::SingleB, CBBTC_USDC.raw_b("40")),
        (&USDC_CBBTC, DepositShape::SingleA, USDC_CBBTC.raw_a("40")),
        (&USDC_USDT, DepositShape::SingleA, USDC_USDT.raw_a("15")),
    ];
    for (p, shape, budget) in cases {
        let sp = p.sqrt_price();
        let (lo, hi, _) = p.range_spacings(2);
        let (amount_a, amount_b) = match shape {
            DepositShape::SingleA => (u128::from(budget), 0),
            DepositShape::SingleB => (0, u128::from(budget)),
            DepositShape::Dual => unreachable!(),
        };
        let hint = swap_to_match_hint(amount_a as u64, amount_b as u64, sp, lo, hi, shape)
            .unwrap_or_else(|| panic!("{} needs swap", p.name));
        let (pa, pb) = post_from_hint(amount_a, amount_b, sp, hint.in_is_a, hint.swap_in_amount);
        let (ua, ub) = deposit_ratio_planner(sp, lo, hi, 1u128 << 48);
        let err = ratio_err(pa, pb, ua, ub);
        assert!(
            err < 0.03,
            "{} shape={shape:?} ratio err {err:.4} post=({pa},{pb})",
            p.name
        );

        let plan = liquidity_delta_for_open(amount_a, amount_b, sp, lo, hi, shape, true);
        assert!(
            plan.liquidity_delta > 0 && plan.amount_a > 0 && plan.amount_b > 0,
            "{} plan {}",
            p.name,
            plan.note
        );
    }
}

#[test]
fn cheap_sol_fixture_still_avoids_raw_decimal_trap() {
    let p = WSOL_USDC_CHEAP;
    let sp = p.sqrt_price();
    let (lo, hi, _) = p.range_spacings(2);
    let budget = p.raw_b("0.2");
    let frac = swap_frac(
        swap_to_match_hint(0, budget, sp, lo, hi, DepositShape::SingleB)
            .unwrap()
            .swap_in_amount,
        budget,
    );
    assert!((0.35..0.65).contains(&frac), "@$80 SOL frac {frac:.3}");
    assert!(legacy_raw_frac_b(sp, lo, hi) > 0.8);
}
