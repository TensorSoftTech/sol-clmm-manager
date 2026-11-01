//! Uniswap-V3-style fee / reward claim simulation (integer Q64.64, no float).

/// `fee_growth_inside = global − below − above` (wrapping), matching Orca / Raydium.
pub fn fee_growth_inside(
    global: u128,
    outside_lower: u128,
    outside_upper: u128,
    tick_lower: i32,
    tick_upper: i32,
    tick_current: i32,
) -> u128 {
    let mut below = outside_lower;
    let mut above = outside_upper;
    if tick_current < tick_lower {
        below = global.wrapping_sub(outside_lower);
    }
    if tick_current >= tick_upper {
        above = global.wrapping_sub(outside_upper);
    }
    global.wrapping_sub(below).wrapping_sub(above)
}

/// `(growth_inside − checkpoint) * liquidity >> 64` as u64 (saturating).
pub fn tokens_owed_delta(growth_inside: u128, checkpoint: u128, liquidity: u128) -> u64 {
    let delta = growth_inside.wrapping_sub(checkpoint);
    let raw = mul_shift_right_64(delta, liquidity);
    raw.min(u64::MAX as u128) as u64
}

/// Claimable = stored owed + growth delta since checkpoint.
#[allow(clippy::too_many_arguments)]
pub fn collect_fees_quote(
    fee_owed: u64,
    growth_global: u128,
    growth_checkpoint: u128,
    outside_lower: u128,
    outside_upper: u128,
    tick_lower: i32,
    tick_upper: i32,
    tick_current: i32,
    liquidity: u128,
) -> u64 {
    let inside = fee_growth_inside(
        growth_global,
        outside_lower,
        outside_upper,
        tick_lower,
        tick_upper,
        tick_current,
    );
    let delta = tokens_owed_delta(inside, growth_checkpoint, liquidity);
    fee_owed.saturating_add(delta)
}

/// Floor((a * b) / 2^64) without requiring a 256-bit crate.
pub fn mul_shift_right_64(a: u128, b: u128) -> u128 {
    let a_lo = a as u64 as u128;
    let a_hi = a >> 64;
    let b_lo = b as u64 as u128;
    let b_hi = b >> 64;

    let lo_lo = a_lo * b_lo;
    let (mid, c1) = (a_lo * b_hi).overflowing_add(a_hi * b_lo);
    let mut hi = a_hi * b_hi;
    if c1 {
        hi = hi.saturating_add(1 << 64);
    }
    let (mid2, c2) = mid.overflowing_add(lo_lo >> 64);
    if c2 {
        hi = hi.saturating_add(1);
    }
    // (a*b)>>64 = (hi << 64) + mid2  — saturate if hi != 0 beyond u128
    if hi > u64::MAX as u128 {
        return u128::MAX;
    }
    hi.saturating_mul(1u128 << 64).saturating_add(mid2)
}

/// Rough period volume from LP fee tokens and fee rate.
///
/// `fee_rate` is hundredths of a bip (1e-6): fee_fraction = fee_rate / 1_000_000.
/// `volume ≈ fees / fee_fraction`.
pub fn estimate_volume_from_fees(fee_tokens_as_usd: f64, fee_rate: u32) -> Option<f64> {
    if !(fee_tokens_as_usd.is_finite() && fee_tokens_as_usd > 0.0) || fee_rate == 0 {
        return None;
    }
    let frac = fee_rate as f64 / 1_000_000.0;
    if frac <= 0.0 {
        return None;
    }
    let v = fee_tokens_as_usd / frac;
    if v.is_finite() && v > 0.0 {
        Some(v)
    } else {
        None
    }
}

/// Human amount + optional USD, e.g. `1.2345 USDC (≈ $1.23)`.
pub fn format_amount_usd(raw: u128, decimals: u8, symbol: &str, usd: Option<f64>) -> String {
    let amt = crate::clmm_math::format_amount(raw, decimals);
    match usd {
        Some(u) if u.is_finite() => {
            format!("{amt} {symbol} (≈ {})", crate::pricing::format_usd(Some(u)))
        }
        _ => format!("{amt} {symbol}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mul_shift_basic() {
        // 3 * 2^64 * 5 >> 64 = 15
        assert_eq!(mul_shift_right_64(3u128 << 64, 5), 15);
        assert_eq!(mul_shift_right_64(1, 1), 0);
    }

    #[test]
    fn collect_when_current_in_range() {
        // global=1000<<64, outside 0, checkpoint 0, L=1 → owed = 1000
        let global = 1000u128 << 64;
        let owed = collect_fees_quote(0, global, 0, 0, 0, 0, 100, 50, 1);
        assert_eq!(owed, 1000);
    }

    #[test]
    fn volume_from_fee_rate() {
        // 30 bps = 3000 hundredths of a bip → 0.003
        let v = estimate_volume_from_fees(3.0, 3000).unwrap();
        assert!((v - 1000.0).abs() < 1e-6);
    }
}
