//! Meteora DLMM account decoders (PositionV2 / LbPair / BinArray).

use anyhow::{Context, Result, bail};
use sol_clmm_manager::detail::{LiquiditySample, TokenSide};
use sol_clmm_manager::pool::{IndexSpace, PoolStats};
use solana_pubkey::Pubkey;
use std::collections::BTreeMap;

pub const POSITION_V2_DISC: [u8; 8] = [117, 176, 212, 199, 245, 180, 133, 182];
pub const BIN_ARRAY_DISC: [u8; 8] = [92, 142, 92, 220, 5, 148, 70, 181];

pub const POSITION_V2_LEN: u64 = 8120;
pub const OWNER_OFFSET: usize = 40;
pub const LOWER_BIN_OFFSET: usize = 7912;
pub const MAX_BIN_PER_ARRAY: i32 = 70;
pub const BIN_LEN: usize = 144;

#[derive(Debug, Clone)]
pub struct FeeInfoSlot {
    pub fee_x_per_token_complete: u128,
    pub fee_y_per_token_complete: u128,
    pub fee_x_pending: u64,
    pub fee_y_pending: u64,
}

/// Per-bin farming checkpoint (`UserRewardInfo`, 48 bytes).
#[derive(Debug, Clone, Copy, Default)]
pub struct UserRewardSlot {
    /// Kept for future bin-growth claim-sim if Bin gains reward_per_token.
    #[allow(dead_code)]
    pub reward_per_token_completes: [u128; 2],
    pub reward_pendings: [u64; 2],
}

#[derive(Debug, Clone)]
pub struct PositionV2 {
    pub lb_pair: Pubkey,
    pub lower_bin: i32,
    pub upper_bin: i32,
    pub liquidity_shares: Vec<u128>,
    pub liquidity: u128,
    #[allow(dead_code)]
    pub reward_infos: Vec<UserRewardSlot>,
    pub fee_infos: Vec<FeeInfoSlot>,
    /// Sum of per-bin pendings (pre-claim-sim); prefer [`claim_fees_from_bins`].
    #[allow(dead_code)]
    pub fee_x_pending: u64,
    #[allow(dead_code)]
    pub fee_y_pending: u64,
    pub reward_pending: [u64; 2],
}

#[derive(Debug, Clone)]
pub struct LbPairRewardInfo {
    pub mint: Pubkey,
    pub vault: Pubkey,
    pub reward_rate: u128,
    pub last_update_time: u64,
}

#[derive(Debug, Clone)]
pub struct LbPairHeader {
    pub active_id: i32,
    pub bin_step: u16,
    pub base_factor: u16,
    pub base_fee_power_factor: u8,
    pub mint_x: Pubkey,
    pub mint_y: Pubkey,
    pub reserve_x: Pubkey,
    pub reserve_y: Pubkey,
    pub reward_infos: [LbPairRewardInfo; 2],
}

#[derive(Debug, Clone)]
pub struct BinData {
    pub id: i32,
    pub amount_x: u64,
    pub amount_y: u64,
    pub liquidity_supply: u128,
    pub fee_amount_x_per_token_stored: u128,
    pub fee_amount_y_per_token_stored: u128,
}

#[derive(Debug, Clone)]
pub struct BinArrayData {
    pub bins: Vec<BinData>,
}

pub fn decode_position_v2(data: &[u8]) -> Result<PositionV2> {
    if data.len() < LOWER_BIN_OFFSET + 16 {
        bail!("position v2 too short: {}", data.len());
    }
    if data[0..8] != POSITION_V2_DISC {
        bail!("position v2 discriminator mismatch");
    }
    let mut o = 8;
    let lb_pair = read_pubkey(data, &mut o)?;
    o += 32; // owner
    let mut liquidity_shares = Vec::with_capacity(70);
    let mut liquidity: u128 = 0;
    for _ in 0..70 {
        let share = read_u128(data, &mut o)?;
        liquidity = liquidity.saturating_add(share);
        liquidity_shares.push(share);
    }
    // reward_infos (70 * 48) then fee_infos (70 * 48)
    let reward_infos_off = 8 + 32 + 32 + 70 * 16;
    let fee_infos_off = reward_infos_off + 70 * 48;
    let mut reward_infos = Vec::with_capacity(70);
    let mut reward_pending = [0u64; 2];
    for i in 0..70 {
        let base = reward_infos_off + i * 48;
        if base + 48 <= data.len() {
            let slot = UserRewardSlot {
                reward_per_token_completes: [
                    u128::from_le_bytes(data[base..base + 16].try_into()?),
                    u128::from_le_bytes(data[base + 16..base + 32].try_into()?),
                ],
                reward_pendings: [
                    u64::from_le_bytes(data[base + 32..base + 40].try_into()?),
                    u64::from_le_bytes(data[base + 40..base + 48].try_into()?),
                ],
            };
            reward_pending[0] = reward_pending[0].saturating_add(slot.reward_pendings[0]);
            reward_pending[1] = reward_pending[1].saturating_add(slot.reward_pendings[1]);
            reward_infos.push(slot);
        }
    }
    let mut fee_infos = Vec::with_capacity(70);
    let mut fee_x_pending: u64 = 0;
    let mut fee_y_pending: u64 = 0;
    for i in 0..70 {
        let base = fee_infos_off + i * 48;
        if base + 48 <= data.len() {
            let slot = FeeInfoSlot {
                fee_x_per_token_complete: u128::from_le_bytes(data[base..base + 16].try_into()?),
                fee_y_per_token_complete: u128::from_le_bytes(
                    data[base + 16..base + 32].try_into()?,
                ),
                fee_x_pending: u64::from_le_bytes(data[base + 32..base + 40].try_into()?),
                fee_y_pending: u64::from_le_bytes(data[base + 40..base + 48].try_into()?),
            };
            fee_x_pending = fee_x_pending.saturating_add(slot.fee_x_pending);
            fee_y_pending = fee_y_pending.saturating_add(slot.fee_y_pending);
            fee_infos.push(slot);
        }
    }
    o = LOWER_BIN_OFFSET;
    let lower_bin = read_i32(data, &mut o)?;
    let upper_bin = read_i32(data, &mut o)?;
    Ok(PositionV2 {
        lb_pair,
        lower_bin,
        upper_bin,
        liquidity_shares,
        liquidity,
        reward_infos,
        fee_infos,
        fee_x_pending,
        fee_y_pending,
        reward_pending,
    })
}

pub fn decode_lb_pair_header(data: &[u8]) -> Result<LbPairHeader> {
    const MIN: usize = 8 + 32 + 32 + 16 + 32 + 32 + 32 + 32;
    if data.len() < MIN {
        bail!("lb pair too short: {}", data.len());
    }
    let base_factor = u16::from_le_bytes(data[8..10].try_into()?);
    let base_fee_power_factor = data[8 + 26];
    let mut o = 8 + 32 + 32;
    o += 1 + 2 + 1; // bump, bin_step_seed, pair_type
    let active_id = read_i32(data, &mut o)?;
    let bin_step = read_u16(data, &mut o)?;
    o += 1 + 1 + 2 + 1 + 1; // status … creator control
    let mint_x = read_pubkey(data, &mut o)?;
    let mint_y = read_pubkey(data, &mut o)?;
    let reserve_x = read_pubkey(data, &mut o)?;
    let reserve_y = read_pubkey(data, &mut o)?;
    // protocol_fee (16) + padding1 (32) → reward_infos[2] at offset 264
    let mut reward_infos = [
        LbPairRewardInfo {
            mint: Pubkey::default(),
            vault: Pubkey::default(),
            reward_rate: 0,
            last_update_time: 0,
        },
        LbPairRewardInfo {
            mint: Pubkey::default(),
            vault: Pubkey::default(),
            reward_rate: 0,
            last_update_time: 0,
        },
    ];
    const REWARD_OFF: usize = 264;
    const REWARD_LEN: usize = 144;
    if data.len() >= REWARD_OFF + REWARD_LEN * 2 {
        for (i, slot) in reward_infos.iter_mut().enumerate() {
            let base = REWARD_OFF + i * REWARD_LEN;
            slot.mint = Pubkey::new_from_array(data[base..base + 32].try_into()?);
            slot.vault = Pubkey::new_from_array(data[base + 32..base + 64].try_into()?);
            // funder(32) + duration(8) + end(8) → rate at +112
            slot.reward_rate = u128::from_le_bytes(data[base + 112..base + 128].try_into()?);
            slot.last_update_time = u64::from_le_bytes(data[base + 128..base + 136].try_into()?);
        }
    }
    Ok(LbPairHeader {
        active_id,
        bin_step,
        base_factor,
        base_fee_power_factor,
        mint_x,
        mint_y,
        reserve_x,
        reserve_y,
        reward_infos,
    })
}

pub fn pool_stats_from_lb_pair(
    address: Pubkey,
    h: &LbPairHeader,
    pool_liquidity: u128,
    fee_growth_a: u128,
    fee_growth_b: u128,
) -> PoolStats {
    let mut fee = (h.base_factor as u64)
        .saturating_mul(h.bin_step as u64)
        .saturating_mul(10);
    for _ in 0..h.base_fee_power_factor.min(8) {
        fee = fee.saturating_mul(10);
    }
    PoolStats {
        address,
        token_mint_a: h.mint_x,
        token_mint_b: h.mint_y,
        token_vault_a: h.reserve_x,
        token_vault_b: h.reserve_y,
        tick_spacing: h.bin_step,
        fee_rate: fee.min(u32::MAX as u64) as u32,
        liquidity: pool_liquidity,
        sqrt_price: 0,
        tick_current_index: h.active_id,
        index_space: IndexSpace::Bin {
            bin_step: h.bin_step,
        },
        fee_growth_global_a: fee_growth_a,
        fee_growth_global_b: fee_growth_b,
        protocol_fee_owed_a: 0,
        protocol_fee_owed_b: 0,
    }
}

pub fn decode_bin_array(data: &[u8]) -> Result<BinArrayData> {
    // disc + index(i64) + version(u8) + pad7 + lb_pair + bins[70]
    let min = 8 + 8 + 1 + 7 + 32 + MAX_BIN_PER_ARRAY as usize * BIN_LEN;
    if data.len() < min {
        bail!("bin array too short: {}", data.len());
    }
    if data[0..8] != BIN_ARRAY_DISC {
        bail!("bin array discriminator mismatch");
    }
    let index = i64::from_le_bytes(data[8..16].try_into()?);
    let mut bins = Vec::with_capacity(MAX_BIN_PER_ARRAY as usize);
    let mut o = 8 + 8 + 1 + 7 + 32;
    for i in 0..MAX_BIN_PER_ARRAY {
        let amount_x = u64::from_le_bytes(data[o..o + 8].try_into()?);
        let amount_y = u64::from_le_bytes(data[o + 8..o + 16].try_into()?);
        let liquidity_supply = u128::from_le_bytes(data[o + 32..o + 48].try_into()?);
        // fee_amount_x/y_per_token_stored at +72 / +88 within Bin (after fulfilled+limit fees)
        // amount_x(8)+y(8)+price(16)+supply(16)+fulfilled(16)+limit_fees(16) = 80 → wait
        // 8+8+16+16 = 48 to end of supply; +8+8 fulfilled = 64; +8+8 limit = 80
        let fee_x = u128::from_le_bytes(data[o + 80..o + 96].try_into()?);
        let fee_y = u128::from_le_bytes(data[o + 96..o + 112].try_into()?);
        let bin_id = (index as i32)
            .saturating_mul(MAX_BIN_PER_ARRAY)
            .saturating_add(i);
        bins.push(BinData {
            id: bin_id,
            amount_x,
            amount_y,
            liquidity_supply,
            fee_amount_x_per_token_stored: fee_x,
            fee_amount_y_per_token_stored: fee_y,
        });
        o += BIN_LEN;
    }
    Ok(BinArrayData { bins })
}

pub fn bin_id_to_bin_array_index(bin_id: i32) -> i64 {
    let idx = bin_id.div_euclid(MAX_BIN_PER_ARRAY);
    idx as i64
}

pub fn bin_array_pda(lb_pair: &Pubkey, index: i64, program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"bin_array", lb_pair.as_ref(), &index.to_le_bytes()],
        program,
    )
    .0
}

/// Claim-sim fees — Meteora SDK: `mulShr(share >> 64, Δfee_per_token, 64)`.
///
/// Shares are Q64.64; fee growth is also Q64.64. Using the full share (as in
/// Uniswap-V3 `L * Δfg >> 64`) overstates fees by ~2^64 and blows up list USD.
pub fn claim_fees_from_bins(position: &PositionV2, bins: &BTreeMap<i32, BinData>) -> (u64, u64) {
    let mut fx: u128 = 0;
    let mut fy: u128 = 0;
    let width = (position.upper_bin - position.lower_bin + 1).max(0) as usize;
    for i in 0..width.min(position.liquidity_shares.len()) {
        let share = position.liquidity_shares[i];
        let bin_id = position.lower_bin + i as i32;
        let Some(slot) = position.fee_infos.get(i) else {
            continue;
        };
        fx = fx.saturating_add(slot.fee_x_pending as u128);
        fy = fy.saturating_add(slot.fee_y_pending as u128);
        // Integer share only (SDK `posShares[idx].shrn(SCALE_OFFSET)`).
        let share_i = share >> 64;
        if share_i == 0 {
            continue;
        }
        let Some(bin) = bins.get(&bin_id) else {
            continue;
        };
        // Checkpoint ahead of bin → no growth yet (do not wrap).
        let dx = bin
            .fee_amount_x_per_token_stored
            .saturating_sub(slot.fee_x_per_token_complete);
        let dy = bin
            .fee_amount_y_per_token_stored
            .saturating_sub(slot.fee_y_per_token_complete);
        fx = fx.saturating_add(sol_clmm_manager::fees::mul_shift_right_64(dx, share_i));
        fy = fy.saturating_add(sol_clmm_manager::fees::mul_shift_right_64(dy, share_i));
    }
    (
        fx.min(u64::MAX as u128) as u64,
        fy.min(u64::MAX as u128) as u64,
    )
}

/// Position amounts from per-bin shares × bin reserves / supply.
pub fn amounts_from_shares(position: &PositionV2, bins: &BTreeMap<i32, BinData>) -> (u128, u128) {
    let mut ax: u128 = 0;
    let mut ay: u128 = 0;
    let width = (position.upper_bin - position.lower_bin + 1).max(0) as usize;
    for i in 0..width.min(position.liquidity_shares.len()) {
        let bin_id = position.lower_bin + i as i32;
        let share = position.liquidity_shares[i];
        if share == 0 {
            continue;
        }
        let Some(bin) = bins.get(&bin_id) else {
            continue;
        };
        if bin.liquidity_supply == 0 {
            continue;
        }
        ax = ax.saturating_add(mul_div_u128(
            share,
            bin.amount_x as u128,
            bin.liquidity_supply,
        ));
        ay = ay.saturating_add(mul_div_u128(
            share,
            bin.amount_y as u128,
            bin.liquidity_supply,
        ));
    }
    (ax, ay)
}

/// `(a * b) / d` — integer only. f64 loses Q64.64 share precision.
fn mul_div_u128(a: u128, b: u128, d: u128) -> u128 {
    if d == 0 || a == 0 || b == 0 {
        return 0;
    }
    if let Some(prod) = a.checked_mul(b) {
        return prod / d;
    }
    // a*b does not fit in u128 — divide 256-bit product by d.
    let (hi, lo) = full_mul_u128(a, b);
    div_u256_by_u128(hi, lo, d)
}

fn full_mul_u128(a: u128, b: u128) -> (u128, u128) {
    const MASK: u128 = (1u128 << 64) - 1;
    let (a_hi, a_lo) = (a >> 64, a & MASK);
    let (b_hi, b_lo) = (b >> 64, b & MASK);
    let ll = a_lo * b_lo;
    let lh = a_lo * b_hi;
    let hl = a_hi * b_lo;
    let hh = a_hi * b_hi;
    let mid = (ll >> 64) + (lh & MASK) + (hl & MASK);
    let lo = (ll & MASK) | (mid << 64);
    let hi = hh + (lh >> 64) + (hl >> 64) + (mid >> 64);
    (hi, lo)
}

fn div_u256_by_u128(mut hi: u128, mut lo: u128, d: u128) -> u128 {
    if hi == 0 {
        return lo / d;
    }
    // Shift-subtract 256÷128 → 128-bit quotient (saturate if ≥ 2^128).
    let mut q = 0u128;
    for _ in 0..128 {
        let carry = (hi & (1u128 << 127)) != 0;
        hi = (hi << 1) | (lo >> 127);
        lo <<= 1;
        q <<= 1;
        if carry || hi >= d {
            hi -= d;
            q |= 1;
        }
    }
    // If remainder of high bits still ≥ d after 128 steps, quotient overflowed.
    if hi >= d {
        return u128::MAX;
    }
    q
}

/// `pad_bins` widens the X window beyond the position (use `CHART_FETCH_HALF_SPACINGS`).
/// Position `[lower, upper]` still drives bar coloring only.
pub fn build_bin_series(
    bins: &BTreeMap<i32, BinData>,
    position: &PositionV2,
    active_id: i32,
    pad_bins: i32,
) -> Vec<LiquiditySample> {
    let lower = position.lower_bin;
    let upper = position.upper_bin;
    let pad = pad_bins.max(2);
    let min_b = lower.min(active_id) - pad;
    let max_b = upper.max(active_id) + pad;

    let mut points: Vec<(i32, u128)> = Vec::new();
    for (&id, bin) in bins {
        if id < min_b || id > max_b {
            continue;
        }
        let depth = (bin.amount_x as u128)
            .saturating_add(bin.amount_y as u128)
            .max(bin.liquidity_supply);
        if depth > 0 {
            points.push((id, depth));
        }
    }
    if points.is_empty() {
        for i in 0..((upper - lower + 1).max(0) as usize).min(position.liquidity_shares.len()) {
            let id = lower + i as i32;
            let share = position.liquidity_shares[i];
            if share > 0 {
                points.push((id, share));
            }
        }
        points.push((active_id, position.liquidity.max(1)));
    }
    points.sort_by_key(|(id, _)| *id);

    // Downsample lightly if huge.
    let samples: Vec<_> = if points.len() <= 70 {
        points
    } else {
        let step = points.len() / 56;
        points.into_iter().step_by(step.max(1)).collect()
    };

    samples
        .into_iter()
        .map(|(id, l)| {
            let in_position = id >= lower && id <= upper;
            let side = if !in_position {
                TokenSide::Outside
            } else if id < active_id {
                TokenSide::A
            } else if id > active_id {
                TokenSide::B
            } else {
                TokenSide::Both
            };
            LiquiditySample {
                tick: id,
                liquidity: sol_clmm_manager::clmm_math::scale_u128(l.max(1)),
                side,
                in_position,
            }
        })
        .collect()
}

fn read_pubkey(data: &[u8], o: &mut usize) -> Result<Pubkey> {
    let end = *o + 32;
    let bytes: [u8; 32] = data.get(*o..end).context("pubkey")?.try_into()?;
    *o = end;
    Ok(Pubkey::new_from_array(bytes))
}

fn read_i32(data: &[u8], o: &mut usize) -> Result<i32> {
    let end = *o + 4;
    let v = i32::from_le_bytes(data.get(*o..end).context("i32")?.try_into()?);
    *o = end;
    Ok(v)
}

fn read_u16(data: &[u8], o: &mut usize) -> Result<u16> {
    let end = *o + 2;
    let v = u16::from_le_bytes(data.get(*o..end).context("u16")?.try_into()?);
    *o = end;
    Ok(v)
}

fn read_u128(data: &[u8], o: &mut usize) -> Result<u128> {
    let end = *o + 16;
    let v = u128::from_le_bytes(data.get(*o..end).context("u128")?.try_into()?);
    *o = end;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_match_layout() {
        assert_eq!(8 + 32 + 32 + 70 * 16 + 70 * 48 + 70 * 48, LOWER_BIN_OFFSET);
        assert_eq!(bin_id_to_bin_array_index(0), 0);
        assert_eq!(bin_id_to_bin_array_index(70), 1);
        assert_eq!(bin_id_to_bin_array_index(-1), -1);
    }

    #[test]
    fn mul_div_preserves_q64_shares() {
        // Typical Q64.64 share against bin supply — f64 would lose low bits.
        let share = 1u128 << 64;
        let amount = 1_000_000u128;
        let supply = 2u128 << 64;
        assert_eq!(mul_div_u128(share, amount, supply), 500_000);

        // Overflow path: share * amount does not fit u128.
        let big_share = 1u128 << 100;
        let big_amt = 1u128 << 40;
        let big_sup = 1u128 << 110;
        assert_eq!(mul_div_u128(big_share, big_amt, big_sup), 1u128 << 30);

        assert_eq!(
            mul_div_u128(u128::MAX / 2, 3, 2),
            (u128::MAX / 2) + (u128::MAX / 2) / 2
        );
    }

    #[test]
    fn claim_fees_use_share_integer_part() {
        // share = 5 << 64 → share_i = 5; Δfee = 3 << 64 → fee = 15
        let mut bins = BTreeMap::new();
        bins.insert(
            10,
            BinData {
                id: 10,
                amount_x: 0,
                amount_y: 0,
                liquidity_supply: 1,
                fee_amount_x_per_token_stored: 3u128 << 64,
                fee_amount_y_per_token_stored: 0,
            },
        );
        let mut shares = vec![0u128; 70];
        shares[0] = 5u128 << 64;
        let fee_infos = vec![
            FeeInfoSlot {
                fee_x_per_token_complete: 0,
                fee_y_per_token_complete: 0,
                fee_x_pending: 0,
                fee_y_pending: 0,
            };
            70
        ];
        let pos = PositionV2 {
            lb_pair: Pubkey::default(),
            lower_bin: 10,
            upper_bin: 10,
            liquidity_shares: shares,
            liquidity: 5u128 << 64,
            reward_infos: vec![UserRewardSlot::default(); 70],
            fee_infos,
            fee_x_pending: 0,
            fee_y_pending: 0,
            reward_pending: [0; 2],
        };
        let (fx, fy) = claim_fees_from_bins(&pos, &bins);
        assert_eq!(fx, 15);
        assert_eq!(fy, 0);
    }
}
