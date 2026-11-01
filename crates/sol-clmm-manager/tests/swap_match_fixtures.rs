//! Data-driven swap-to-match / open / mock-position tests.
//!
//! Fixtures live in `tests/fixtures/`:
//! - `pools.json` — mock CLMM pools (decimals, mid, spacing)
//! - `deposit_cases.json` — single/dual deposit scenarios + expectations
//! - `positions.json` — mock LP inventories from (L, range, √P)

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde::Deserialize;
use sol_clmm_manager::clmm_math::{
    amount_a_from_b, amounts_from_liquidity, get_sqrt_price_at_tick, parse_ui_amount,
    value_a_in_b,
};
use sol_clmm_manager::pricing::human_price_b_per_a;
use sol_clmm_manager::{
    DepositShape, deposit_ratio_planner, liquidity_delta_for_open, swap_to_match_hint,
};

// ── fixture schema ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct PoolsFile {
    pools: Vec<PoolFix>,
}

#[derive(Debug, Clone, Deserialize)]
struct PoolFix {
    id: String,
    name: String,
    symbol_a: String,
    symbol_b: String,
    decimals_a: u8,
    decimals_b: u8,
    tick_spacing: i32,
    human_mid_b_per_a: f64,
    #[serde(default)]
    notes: String,
}

#[derive(Debug, Deserialize)]
struct CasesFile {
    cases: Vec<DepositCase>,
}

#[derive(Debug, Deserialize)]
struct DepositCase {
    id: String,
    pool: String,
    shape: String,
    amount_a_ui: Option<String>,
    amount_b_ui: Option<String>,
    range_spacings: i32,
    band: String,
    expect: DepositExpect,
}

#[derive(Debug, Deserialize)]
struct DepositExpect {
    swap: bool,
    in_is_a: Option<bool>,
    frac_min: Option<f64>,
    frac_max: Option<f64>,
    #[serde(default)]
    legacy_raw_frac_min: Option<f64>,
    #[serde(default)]
    legacy_raw_frac_max: Option<f64>,
    open_both_legs: bool,
    funded_left_frac_min: Option<f64>,
    ratio_err_max: Option<f64>,
    #[serde(default)]
    value_share_a_min: Option<f64>,
    #[serde(default)]
    value_share_a_max: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct PositionsFile {
    positions: Vec<PositionFix>,
}

#[derive(Debug, Deserialize)]
struct PositionFix {
    id: String,
    pool: String,
    range_spacings: i32,
    band: String,
    /// Decimal string for u128 liquidity.
    liquidity: String,
    expect: PositionExpect,
}

#[derive(Debug, Deserialize)]
struct PositionExpect {
    amount_a_min: u128,
    #[serde(default)]
    amount_a_max: Option<u128>,
    amount_b_min: u128,
    #[serde(default)]
    amount_b_max: Option<u128>,
    both_legs: bool,
}

// ── pool helpers ─────────────────────────────────────────────────────────

impl PoolFix {
    fn mid_tick(&self) -> i32 {
        let raw =
            self.human_mid_b_per_a * 10f64.powi(self.decimals_b as i32 - self.decimals_a as i32);
        let tick = (raw.ln() / 1.0001f64.ln()).round() as i32;
        tick.div_euclid(self.tick_spacing) * self.tick_spacing
    }

    fn sqrt_price(&self) -> u128 {
        get_sqrt_price_at_tick(self.mid_tick()).expect("sqrt")
    }

    fn range(&self, spacings: i32, band: &str) -> (i32, i32) {
        let mid = self.mid_tick();
        let s = self.tick_spacing;
        match band {
            "symmetric" => (mid - spacings * s, mid + spacings * s),
            // Spot near lower → A-heavy deposit.
            "a_heavy" => (mid - s, mid + 8 * s),
            // Spot near upper → B-heavy deposit.
            "b_heavy" => (mid - 8 * s, mid + s),
            // Entire range below spot.
            "above_range" => (mid - 40 * s, mid - 10 * s),
            // Entire range above spot.
            "below_range" => (mid + 10 * s, mid + 40 * s),
            other => panic!("unknown band `{other}`"),
        }
    }

    fn raw_a(&self, ui: &str) -> u64 {
        parse_ui_amount(ui, self.decimals_a).unwrap_or_else(|e| panic!("{}: {e}", self.id))
    }

    fn raw_b(&self, ui: &str) -> u64 {
        parse_ui_amount(ui, self.decimals_b).unwrap_or_else(|e| panic!("{}: {e}", self.id))
    }
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn load_json<T: for<'de> Deserialize<'de>>(name: &str) -> T {
    let path = fixtures_dir().join(name);
    let raw = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn pools_by_id() -> HashMap<String, PoolFix> {
    let f: PoolsFile = load_json("pools.json");
    f.pools.into_iter().map(|p| (p.id.clone(), p)).collect()
}

fn shape(s: &str) -> DepositShape {
    match s {
        "single_a" => DepositShape::SingleA,
        "single_b" => DepositShape::SingleB,
        "dual" => DepositShape::Dual,
        other => panic!("unknown shape `{other}`"),
    }
}

fn legacy_raw_single_b_frac(sp: u128, lo: i32, hi: i32) -> f64 {
    let (ua, ub) = amounts_from_liquidity(1u128 << 48, sp, lo, hi);
    ua as f64 / (ua + ub).max(1) as f64
}

fn post_swap(
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

fn ratio_err(pa: u128, pb: u128, ua: u128, ub: u128) -> f64 {
    if pa == 0 || pb == 0 || ua == 0 || ub == 0 {
        return f64::INFINITY;
    }
    let got = pa as f64 / pb as f64;
    let want = ua as f64 / ub as f64;
    (got - want).abs() / want
}

// ── tests ────────────────────────────────────────────────────────────────

#[test]
fn fixture_pools_mid_price_sane() {
    let pools = pools_by_id();
    assert!(pools.len() >= 6, "expected ≥6 pool fixtures");
    for p in pools.values() {
        let sp = p.sqrt_price();
        let got = human_price_b_per_a(sp, p.decimals_a, p.decimals_b).unwrap();
        let rel = (got - p.human_mid_b_per_a).abs() / p.human_mid_b_per_a;
        let tol = (p.tick_spacing as f64 * 0.0001 * 2.0).max(0.01);
        assert!(
            rel < tol,
            "{} ({}/{}): price {got} vs {} rel={rel:.4} notes={}",
            p.id,
            p.symbol_a,
            p.symbol_b,
            p.human_mid_b_per_a,
            p.notes
        );
    }
}

#[test]
fn fixture_deposit_cases() {
    let pools = pools_by_id();
    let cases: CasesFile = load_json("deposit_cases.json");
    assert!(cases.cases.len() >= 10, "expected many deposit cases");

    for case in &cases.cases {
        let p = pools
            .get(&case.pool)
            .unwrap_or_else(|| panic!("case {} missing pool {}", case.id, case.pool));
        let sp = p.sqrt_price();
        let (lo, hi) = p.range(case.range_spacings, &case.band);
        let shape = shape(&case.shape);

        let amount_a = case
            .amount_a_ui
            .as_deref()
            .map(|u| u128::from(p.raw_a(u)))
            .unwrap_or(0);
        let amount_b = case
            .amount_b_ui
            .as_deref()
            .map(|u| u128::from(p.raw_b(u)))
            .unwrap_or(0);
        let budget = match shape {
            DepositShape::SingleA => amount_a,
            DepositShape::SingleB => amount_b,
            DepositShape::Dual => amount_a.max(amount_b),
        };

        let hint = swap_to_match_hint(
            amount_a.min(u128::from(u64::MAX)) as u64,
            amount_b.min(u128::from(u64::MAX)) as u64,
            sp,
            lo,
            hi,
            shape,
        );

        if !case.expect.swap {
            assert!(
                hint.is_none(),
                "{}: expected no swap, got {hint:?}",
                case.id
            );
            let plan =
                liquidity_delta_for_open(amount_a, amount_b, sp, lo, hi, shape, false);
            assert!(
                plan.liquidity_delta > 0,
                "{}: out-of-range open should work · {}",
                case.id,
                plan.note
            );
            if !case.expect.open_both_legs {
                match shape {
                    DepositShape::SingleB => assert_eq!(plan.amount_a, 0, "{}", case.id),
                    DepositShape::SingleA => assert_eq!(plan.amount_b, 0, "{}", case.id),
                    DepositShape::Dual => {}
                }
            }
            continue;
        }

        let hint = hint.unwrap_or_else(|| panic!("{}: expected swap hint", case.id));
        if let Some(dir) = case.expect.in_is_a {
            assert_eq!(hint.in_is_a, dir, "{}: swap direction", case.id);
        }

        let frac = hint.swap_in_amount as f64 / budget as f64;
        if let (Some(lo_f), Some(hi_f)) = (case.expect.frac_min, case.expect.frac_max) {
            assert!(
                (lo_f..=hi_f).contains(&frac),
                "{}: swap frac {frac:.4} not in [{lo_f},{hi_f}] · {} {}/{}",
                case.id,
                p.name,
                p.symbol_a,
                p.symbol_b
            );
        }

        if let Some(min_leg) = case.expect.legacy_raw_frac_min {
            let legacy = legacy_raw_single_b_frac(sp, lo, hi);
            assert!(
                legacy >= min_leg,
                "{}: legacy raw frac {legacy:.4} < {min_leg}",
                case.id
            );
            assert!(
                frac + 0.15 < legacy,
                "{}: value-weighted {frac:.4} must beat legacy {legacy:.4}",
                case.id
            );
        }
        if let Some(max_leg) = case.expect.legacy_raw_frac_max {
            let legacy = legacy_raw_single_b_frac(sp, lo, hi);
            assert!(
                legacy <= max_leg,
                "{}: legacy raw frac {legacy:.4} > {max_leg} (under-swap trap)",
                case.id
            );
            assert!(
                frac > legacy + 0.20,
                "{}: value-weighted {frac:.4} must lift legacy {legacy:.4}",
                case.id
            );
        }

        let (pa, pb) = post_swap(amount_a, amount_b, sp, hint.in_is_a, hint.swap_in_amount);
        if let Some(max_err) = case.expect.ratio_err_max {
            let (ua, ub) = deposit_ratio_planner(sp, lo, hi, 1u128 << 48);
            let err = ratio_err(pa, pb, ua, ub);
            assert!(
                err <= max_err,
                "{}: ratio err {err:.4} > {max_err} post=({pa},{pb}) unit=({ua},{ub})",
                case.id
            );
        }

        let plan = liquidity_delta_for_open(amount_a, amount_b, sp, lo, hi, shape, true);
        assert!(
            plan.liquidity_delta > 0,
            "{}: open ΔL=0 · {}",
            case.id,
            plan.note
        );
        if case.expect.open_both_legs {
            assert!(
                plan.amount_a > 0 && plan.amount_b > 0,
                "{}: open plan missing a leg a={} b={}",
                case.id,
                plan.amount_a,
                plan.amount_b
            );
        }
        if let Some(min_left) = case.expect.funded_left_frac_min {
            let left = match shape {
                DepositShape::SingleA => plan.amount_a as f64 / amount_a as f64,
                DepositShape::SingleB => plan.amount_b as f64 / amount_b as f64,
                DepositShape::Dual => 1.0,
            };
            assert!(
                left >= min_left,
                "{}: funded side left {left:.4} < {min_left} (raw-decimal dump bug)",
                case.id
            );
        }
        if let (Some(vmin), Some(vmax)) =
            (case.expect.value_share_a_min, case.expect.value_share_a_max)
        {
            let a_in_b = value_a_in_b(plan.amount_a, sp);
            let share = a_in_b as f64 / (a_in_b + plan.amount_b) as f64;
            assert!(
                (vmin..=vmax).contains(&share),
                "{}: value share A {share:.4} not in [{vmin},{vmax}]",
                case.id
            );
        }
    }
}

#[test]
fn fixture_mock_positions() {
    let pools = pools_by_id();
    let file: PositionsFile = load_json("positions.json");
    assert!(!file.positions.is_empty());

    for pos in &file.positions {
        let p = pools
            .get(&pos.pool)
            .unwrap_or_else(|| panic!("position {} missing pool {}", pos.id, pos.pool));
        let sp = p.sqrt_price();
        let (lo, hi) = p.range(pos.range_spacings, &pos.band);
        let liq: u128 = pos
            .liquidity
            .parse()
            .unwrap_or_else(|e| panic!("{} liquidity: {e}", pos.id));
        let (a, b) = amounts_from_liquidity(liq, sp, lo, hi);

        assert!(
            a >= pos.expect.amount_a_min,
            "{}: amount_a {a} < min {}",
            pos.id,
            pos.expect.amount_a_min
        );
        assert!(
            b >= pos.expect.amount_b_min,
            "{}: amount_b {b} < min {}",
            pos.id,
            pos.expect.amount_b_min
        );
        if let Some(max_a) = pos.expect.amount_a_max {
            assert!(a <= max_a, "{}: amount_a {a} > max {max_a}", pos.id);
        }
        if let Some(max_b) = pos.expect.amount_b_max {
            assert!(b <= max_b, "{}: amount_b {b} > max {max_b}", pos.id);
        }
        if pos.expect.both_legs {
            assert!(a > 0 && b > 0, "{}: expected both legs", pos.id);
        }
    }
}

#[test]
fn fixture_scale_invariance_wsol_usdc() {
    // Same pool/range: 1 USDC and 1000 USDC must share swap fraction.
    let pools = pools_by_id();
    let p = &pools["wsol_usdc_150"];
    let sp = p.sqrt_price();
    let (lo, hi) = p.range(2, "symmetric");
    let small = p.raw_b("1");
    let big = p.raw_b("1000");
    let hs = swap_to_match_hint(0, small, sp, lo, hi, DepositShape::SingleB).unwrap();
    let hb = swap_to_match_hint(0, big, sp, lo, hi, DepositShape::SingleB).unwrap();
    let fs = hs.swap_in_amount as f64 / small as f64;
    let fb = hb.swap_in_amount as f64 / big as f64;
    assert!(
        (fs - fb).abs() < 0.001,
        "scale invariance broken: {fs} vs {fb}"
    );
}
