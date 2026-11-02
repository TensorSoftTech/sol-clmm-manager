//! Shared CLMM amounts + chart series helpers (Orca / Raydium).
//!
//! Amount math uses integer Q64.64 √P (Uniswap/Raydium-style), not f64.

use crate::detail::{LiquiditySample, TokenSide};
use crate::fees::mul_shift_right_64;
use std::collections::BTreeMap;

const MIN_TICK: i32 = -443_636;
const MAX_TICK: i32 = 443_636;
const Q64: u128 = 1u128 << 64;

/// Estimate (amount_a, amount_b) in raw token units from L + √P + tick range.
pub fn amounts_from_liquidity(
    liquidity: u128,
    sqrt_price_x64: u128,
    tick_lower: i32,
    tick_upper: i32,
) -> (u128, u128) {
    if liquidity == 0 || tick_lower >= tick_upper {
        return (0, 0);
    }

    let Some(sa) = get_sqrt_price_at_tick(tick_lower) else {
        return (0, 0);
    };
    let Some(sb) = get_sqrt_price_at_tick(tick_upper) else {
        return (0, 0);
    };
    let sp = sqrt_price_x64;
    if sa == 0 || sb <= sa || sp == 0 {
        return (0, 0);
    }

    if sp <= sa {
        (get_amount_0_delta(sa, sb, liquidity), 0)
    } else if sp >= sb {
        (0, get_amount_1_delta(sa, sb, liquidity))
    } else {
        (
            get_amount_0_delta(sp, sb, liquidity),
            get_amount_1_delta(sa, sp, liquidity),
        )
    }
}

/// Q64.64 √P at tick — Raydium / Uniswap V3 bit product.
pub fn get_sqrt_price_at_tick(tick: i32) -> Option<u128> {
    if !(MIN_TICK..=MAX_TICK).contains(&tick) {
        return None;
    }
    let abs_tick = tick.unsigned_abs();

    let mut ratio: u128 = if abs_tick & 0x1 != 0 {
        0xfffcb933bd6fb800
    } else {
        Q64
    };

    // Magic constants are Q64.64 with high half 0 — mul >> 64 stays in u128.
    const MAG: [(u32, u128); 18] = [
        (0x2, 0xfff97272373d4000),
        (0x4, 0xfff2e50f5f657000),
        (0x8, 0xffe5caca7e10f000),
        (0x10, 0xffcb9843d60f7000),
        (0x20, 0xff973b41fa98e800),
        (0x40, 0xff2ea16466c9b000),
        (0x80, 0xfe5dee046a9a3800),
        (0x100, 0xfcbe86c7900bb000),
        (0x200, 0xf987a7253ac65800),
        (0x400, 0xf3392b0822bb6000),
        (0x800, 0xe7159475a2caf000),
        (0x1000, 0xd097f3bdfd2f2000),
        (0x2000, 0xa9f746462d9f8000),
        (0x4000, 0x70d869a156f31c00),
        (0x8000, 0x31be135f97ed3200),
        (0x10000, 0x9aa508b5b85a500),
        (0x20000, 0x5d6af8dedc582c),
        (0x40000, 0x2216e584f5fa),
    ];
    for &(bit, mag) in &MAG {
        if abs_tick & bit != 0 {
            ratio = mul_shift_right_64(ratio, mag);
        }
    }

    if tick > 0 {
        if ratio == 0 {
            return None;
        }
        ratio = u128::MAX / ratio;
    }
    Some(ratio)
}

/// Δx = L · (1/√a − 1/√b) = (L<<64)/√a − (L<<64)/√b  (floor).
fn get_amount_0_delta(mut sa: u128, mut sb: u128, liquidity: u128) -> u128 {
    if sa > sb {
        std::mem::swap(&mut sa, &mut sb);
    }
    if sa == 0 {
        return 0;
    }
    div_shl_64(liquidity, sa).saturating_sub(div_shl_64(liquidity, sb))
}

/// Δy = L · (√b − √a) / 2^64  (floor).
fn get_amount_1_delta(mut sa: u128, mut sb: u128, liquidity: u128) -> u128 {
    if sa > sb {
        std::mem::swap(&mut sa, &mut sb);
    }
    mul_shift_right_64(liquidity, sb - sa)
}

/// Token-B raw value of `amount_a` at Q64.64 √P (`price = B/A = (√P)²`).
pub fn value_a_in_b(amount_a: u128, sqrt_price_x64: u128) -> u128 {
    mul_shift_right_64(mul_shift_right_64(amount_a, sqrt_price_x64), sqrt_price_x64)
}

/// Token-A raw bought with `amount_b` at Q64.64 √P (spot, no fee).
pub fn amount_a_from_b(amount_b: u128, sqrt_price_x64: u128) -> u128 {
    if sqrt_price_x64 == 0 {
        return 0;
    }
    div_shl_64(div_shl_64(amount_b, sqrt_price_x64), sqrt_price_x64)
}

/// floor((n << 64) / d).
fn div_shl_64(n: u128, d: u128) -> u128 {
    if d == 0 {
        return 0;
    }
    // 256-bit numerator = n << 64 → hi = n >> 64, lo = n << 64
    let n_hi = n >> 64;
    let n_lo = n << 64;
    div_256_by_u128(n_hi, n_lo, d)
}

fn div_256_by_u128(n_hi: u128, n_lo: u128, d: u128) -> u128 {
    if d == 0 {
        return 0;
    }
    if n_hi == 0 {
        return n_lo / d;
    }
    let q_hi = n_hi / d;
    let rem = n_hi % d;
    let mid = (rem << 64) | (n_lo >> 64);
    let q_mid = mid / d;
    let rem2 = mid % d;
    let low = (rem2 << 64) | (n_lo as u64 as u128);
    let q_lo = low / d;
    if q_hi > 0 {
        return u128::MAX;
    }
    q_mid.saturating_mul(Q64).saturating_add(q_lo)
}

pub fn tick_to_price(tick: i32) -> f64 {
    1.0001_f64.powf(tick as f64)
}

pub fn format_price(price: f64) -> String {
    if !price.is_finite() || price <= 0.0 {
        return "—".into();
    }
    if price >= 1000.0 {
        format!("{price:.2}")
    } else if price >= 1.0 {
        format!("{price:.4}")
    } else if price >= 0.0001 {
        format!("{price:.6}")
    } else {
        format!("{price:.2e}")
    }
}

pub fn format_amount(raw: u128, decimals: u8) -> String {
    if decimals == 0 {
        return raw.to_string();
    }
    let scale = 10u128.saturating_pow(decimals as u32);
    if scale == 0 {
        return raw.to_string();
    }
    let whole = raw / scale;
    let frac = raw % scale;
    if frac == 0 {
        return whole.to_string();
    }
    let frac_str = format!("{:0width$}", frac, width = decimals as usize);
    let frac_trim = frac_str.trim_end_matches('0');
    format!("{whole}.{frac_trim}")
}

/// Parse a human amount (`"1.5"`, `"100"`) into raw base units for `decimals`.
/// Accepts `.` or `,` as decimal separator. Rejects empty / overflow.
pub fn parse_ui_amount(s: &str, decimals: u8) -> Result<u64, String> {
    let s = s.trim().replace(',', ".");
    if s.is_empty() {
        return Err("empty amount".into());
    }
    if s.chars().filter(|c| *c == '.').count() > 1 {
        return Err("invalid amount".into());
    }
    let (whole_s, frac_s) = match s.split_once('.') {
        Some((w, f)) => (w, f),
        None => (s.as_str(), ""),
    };
    if whole_s.is_empty() && frac_s.is_empty() {
        return Err("invalid amount".into());
    }
    if !whole_s.is_empty() && !whole_s.chars().all(|c| c.is_ascii_digit()) {
        return Err("invalid amount".into());
    }
    if !frac_s.chars().all(|c| c.is_ascii_digit()) {
        return Err("invalid amount".into());
    }
    if frac_s.len() > decimals as usize {
        return Err(format!("too many decimals (max {decimals})"));
    }
    let whole: u128 = if whole_s.is_empty() {
        0
    } else {
        whole_s
            .parse()
            .map_err(|_| "amount too large".to_string())?
    };
    let scale = 10u128.saturating_pow(decimals as u32);
    let mut frac: u128 = if frac_s.is_empty() {
        0
    } else {
        frac_s.parse().map_err(|_| "invalid fraction".to_string())?
    };
    // Right-pad fractional digits to `decimals`.
    let pad = decimals as usize - frac_s.len();
    for _ in 0..pad {
        frac = frac.saturating_mul(10);
    }
    let raw = whole
        .checked_mul(scale)
        .and_then(|w| w.checked_add(frac))
        .ok_or_else(|| "amount overflow".to_string())?;
    if raw > u64::MAX as u128 {
        return Err("amount overflow".into());
    }
    Ok(raw as u64)
}

#[derive(Debug, Clone, Copy)]
pub struct TickPoint {
    pub index: i32,
    pub initialized: bool,
    pub liquidity_net: i128,
    pub liquidity_gross: u128,
    pub fee_growth_outside_a: u128,
    pub fee_growth_outside_b: u128,
    pub reward_growths_outside: [u128; 3],
}

impl TickPoint {
    pub fn basic(
        index: i32,
        initialized: bool,
        liquidity_net: i128,
        liquidity_gross: u128,
    ) -> Self {
        Self {
            index,
            initialized,
            liquidity_net,
            liquidity_gross,
            fee_growth_outside_a: 0,
            fee_growth_outside_b: 0,
            reward_growths_outside: [0; 3],
        }
    }
}

/// Prefer liquidity_gross histogram; paint position range; tag A / B / Both.
///
/// `pad_spacings` widens the X window beyond `[lower, upper]` (use
/// [`crate::detail::CHART_FETCH_HALF_SPACINGS`] for SOL/USDC-scale context).
#[allow(clippy::too_many_arguments)]
pub fn build_liquidity_series(
    ticks: &BTreeMap<i32, TickPoint>,
    pool_liquidity: u128,
    position_liquidity: u128,
    current_tick: i32,
    lower: i32,
    upper: i32,
    spacing: u16,
    pad_spacings: i32,
) -> Vec<LiquiditySample> {
    let pad = spacing as i32 * pad_spacings.max(2);
    let min_t = lower.min(current_tick) - pad;
    let max_t = upper.max(current_tick) + pad;
    if min_t >= max_t {
        return Vec::new();
    }

    let mut gross: BTreeMap<i32, u128> = BTreeMap::new();
    for t in ticks.values() {
        if t.index >= min_t && t.index <= max_t && t.liquidity_gross > 0 {
            gross.insert(t.index, t.liquidity_gross);
        }
    }

    let mut active: BTreeMap<i32, u128> = BTreeMap::new();
    active.insert(current_tick, pool_liquidity);

    let mut events: Vec<(i32, i128)> = ticks
        .values()
        .filter(|t| t.index >= min_t && t.index <= max_t)
        .map(|t| (t.index, t.liquidity_net))
        .collect();
    events.sort_by_key(|(i, _)| *i);

    let mut l = pool_liquidity as i128;
    for &(idx, net) in events.iter().rev().filter(|(i, _)| *i <= current_tick) {
        l = l.saturating_sub(net);
        active.insert(idx, l.max(0) as u128);
    }
    l = pool_liquidity as i128;
    for &(idx, net) in events.iter().filter(|(i, _)| *i > current_tick) {
        l = l.saturating_add(net);
        active.insert(idx, l.max(0) as u128);
    }

    let mut merged: BTreeMap<i32, u128> = active;
    for (k, v) in gross {
        merged
            .entry(k)
            .and_modify(|e| *e = (*e).max(v))
            .or_insert(v);
    }

    let pos_h = position_liquidity.max(1);
    if merged.is_empty() {
        let step = ((upper - lower) / 24).max(spacing as i32).max(1);
        let mut t = lower;
        while t <= upper {
            merged.insert(t, pos_h);
            t += step;
        }
        merged.insert(current_tick, pool_liquidity.max(pos_h));
    } else {
        for t in [lower, upper] {
            merged.entry(t).or_insert(pos_h);
        }
    }

    let points: Vec<(i32, u128)> = merged.into_iter().collect();
    // Denser buckets so zoom-in on SOL/USDC still has local depth samples.
    downsample(&points, 128, current_tick, lower, upper)
}

fn classify_side(tick: i32, current: i32, lower: i32, upper: i32) -> (TokenSide, bool) {
    let in_position = tick >= lower && tick <= upper;
    if !in_position {
        return (TokenSide::Outside, false);
    }
    let side = if tick < current {
        TokenSide::A
    } else if tick > current {
        TokenSide::B
    } else {
        TokenSide::Both
    };
    (side, true)
}

fn downsample(
    points: &[(i32, u128)],
    buckets: usize,
    current: i32,
    lower: i32,
    upper: i32,
) -> Vec<LiquiditySample> {
    if points.is_empty() {
        return Vec::new();
    }
    let map_one = |t: i32, l: u128| {
        let (side, in_position) = classify_side(t, current, lower, upper);
        LiquiditySample {
            tick: t,
            liquidity: scale_u128(l.max(1)),
            side,
            in_position,
        }
    };

    if points.len() <= buckets {
        return points.iter().map(|(t, l)| map_one(*t, *l)).collect();
    }
    let min_t = points.first().unwrap().0;
    let max_t = points.last().unwrap().0;
    let span = (max_t - min_t).max(1) as f64;
    let mut out = Vec::with_capacity(buckets);
    for b in 0..buckets {
        let edge0 = min_t + ((b as f64 / buckets as f64) * span) as i32;
        let edge1 = min_t + (((b + 1) as f64 / buckets as f64) * span) as i32;
        let mut max_l = 0u128;
        let mut mid = (edge0 + edge1) / 2;
        for (t, l) in points {
            if *t >= edge0 && *t <= edge1 && *l >= max_l {
                max_l = *l;
                mid = *t;
            }
        }
        if current >= edge0 && current <= edge1 {
            mid = current;
        }
        out.push(map_one(mid, max_l.max(1)));
    }
    out
}

pub fn scale_u128(v: u128) -> u64 {
    if v == 0 {
        1
    } else if v <= u64::MAX as u128 {
        v as u64
    } else {
        ((v >> 32) as u64).max(1)
    }
}

/// SPL Token / Token-2022 account: `amount` u64 LE at offset 64.
pub fn read_token_account_amount(data: &[u8]) -> anyhow::Result<u64> {
    use anyhow::bail;
    if data.len() < 72 {
        bail!("token account too short for amount");
    }
    Ok(u64::from_le_bytes(data[64..72].try_into()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ui_amount_basic() {
        assert_eq!(parse_ui_amount("1.5", 9).unwrap(), 1_500_000_000);
        assert_eq!(parse_ui_amount("100", 6).unwrap(), 100_000_000);
        assert_eq!(parse_ui_amount(".5", 6).unwrap(), 500_000);
        assert_eq!(parse_ui_amount("1,25", 2).unwrap(), 125);
        assert!(parse_ui_amount("1.123", 2).is_err());
    }

    #[test]
    fn sqrt_at_zero_is_one() {
        let sp = get_sqrt_price_at_tick(0).unwrap();
        assert_eq!(sp, Q64);
    }

    #[test]
    fn below_range_is_all_a() {
        let sp = get_sqrt_price_at_tick(-1000).unwrap();
        let (a, b) = amounts_from_liquidity(1_000_000_000_000, sp, 0, 100);
        assert!(a > 0);
        assert_eq!(b, 0);
    }

    #[test]
    fn above_range_is_all_b() {
        let sp = get_sqrt_price_at_tick(1000).unwrap();
        let (a, b) = amounts_from_liquidity(1_000_000_000_000, sp, 0, 100);
        assert_eq!(a, 0);
        assert!(b > 0);
    }

    #[test]
    fn in_range_has_both() {
        let sp = get_sqrt_price_at_tick(50).unwrap();
        let (a, b) = amounts_from_liquidity(1_000_000_000_000, sp, 0, 100);
        assert!(a > 0 && b > 0);
    }

    #[test]
    fn value_a_in_b_at_price_one() {
        // tick 0 → √P = 1 → price B/A = 1 → value equals amount.
        let sp = get_sqrt_price_at_tick(0).unwrap();
        assert_eq!(value_a_in_b(1_000_000, sp), 1_000_000);
        assert_eq!(amount_a_from_b(1_000_000, sp), 1_000_000);
    }

    #[test]
    fn spot_convert_round_trip_sol_usdc_ticks() {
        // Double floor (×price then ÷price) loses a few ULPs — keep relative error tiny.
        for &tick in &[-23028, -20000, -10000, 0, 5000] {
            let sp = get_sqrt_price_at_tick(tick).unwrap();
            let a = 1_000_000_000u128; // 1 SOL-like
            let b = value_a_in_b(a, sp);
            assert!(b > 0, "tick {tick}");
            let a2 = amount_a_from_b(b, sp);
            let diff = a.abs_diff(a2);
            let rel = diff as f64 / a as f64;
            assert!(
                rel < 1e-7,
                "tick {tick}: a={a} → b={b} → a2={a2} diff={diff} rel={rel:.2e}"
            );
        }
    }

    #[test]
    fn amount_a_from_b_zero_sqrt_is_zero() {
        assert_eq!(amount_a_from_b(1_000_000, 0), 0);
        assert_eq!(value_a_in_b(1_000_000, 0), 0);
    }

    #[test]
    fn format_usdc_style() {
        assert_eq!(format_amount(1_500_000, 6), "1.5");
        assert_eq!(format_amount(1_000_000_000, 9), "1");
    }
}
