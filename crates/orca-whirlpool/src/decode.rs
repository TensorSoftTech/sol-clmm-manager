//! Manual decode of Whirlpool `Position` / `Whirlpool` / `TickArray` accounts.

use anyhow::{bail, Context, Result};
use sol_clmm_manager::clmm_math::TickPoint;
use sol_clmm_manager::pool::{IndexSpace, PoolStats};
use solana_pubkey::Pubkey;

pub const POSITION_DISCRIMINATOR: [u8; 8] = [170, 188, 143, 228, 122, 64, 247, 208];
pub const WHIRLPOOL_DISCRIMINATOR: [u8; 8] = [63, 149, 209, 12, 225, 128, 99, 9];
pub const TICK_ARRAY_DISCRIMINATOR: [u8; 8] = [69, 97, 189, 190, 110, 7, 66, 187];
pub const DYNAMIC_TICK_ARRAY_DISCRIMINATOR: [u8; 8] = [17, 216, 246, 142, 225, 199, 218, 56];

pub const POSITION_LEN: usize = 216;
/// Through reward infos (8 + 261 + 384).
pub const WHIRLPOOL_FULL_LEN: usize = 8 + 261 + 384;
pub const WHIRLPOOL_MIN_LEN: usize = 8 + 253;
pub const TICK_ARRAY_SIZE: i32 = 88;
pub const TICK_LEN: usize = 113;
pub const FIXED_TICK_ARRAY_LEN: usize = 8 + 4 + (88 * TICK_LEN) + 32;
pub const DYNAMIC_TICK_DATA_LEN: usize = 112;
pub const NUM_REWARDS: usize = 3;

#[derive(Debug, Clone)]
pub struct PositionReward {
    pub growth_inside_checkpoint: u128,
    pub amount_owed: u64,
}

#[derive(Debug, Clone)]
pub struct Position {
    pub whirlpool: Pubkey,
    pub position_mint: Pubkey,
    pub liquidity: u128,
    pub tick_lower_index: i32,
    pub tick_upper_index: i32,
    pub fee_growth_checkpoint_a: u128,
    pub fee_owed_a: u64,
    pub fee_growth_checkpoint_b: u128,
    pub fee_owed_b: u64,
    pub reward_infos: [PositionReward; NUM_REWARDS],
}

#[derive(Debug, Clone)]
pub struct WhirlpoolHeader {
    pub token_mint_a: Pubkey,
    pub token_mint_b: Pubkey,
    pub tick_current_index: i32,
}

#[derive(Debug, Clone)]
pub struct WhirlpoolRewardInfo {
    pub mint: Pubkey,
    pub vault: Pubkey,
    pub emissions_per_second_x64: u128,
    pub growth_global_x64: u128,
}

pub type WhirlpoolStats = PoolStats;

#[derive(Debug, Clone)]
pub struct WhirlpoolFull {
    pub stats: PoolStats,
    pub reward_last_updated_timestamp: u64,
    pub reward_infos: [WhirlpoolRewardInfo; NUM_REWARDS],
}

#[derive(Debug, Clone)]
pub struct TickArrayData {
    pub start_tick_index: i32,
    pub ticks: Vec<TickPoint>,
}

pub fn decode_position(data: &[u8]) -> Result<Position> {
    if data.len() < POSITION_LEN {
        bail!("position account too short: {} < {POSITION_LEN}", data.len());
    }
    if data[0..8] != POSITION_DISCRIMINATOR {
        bail!("not a Position account (bad discriminator)");
    }
    let mut o = 8usize;
    let whirlpool = read_pubkey(data, &mut o)?;
    let position_mint = read_pubkey(data, &mut o)?;
    let liquidity = read_u128(data, &mut o)?;
    let tick_lower_index = read_i32(data, &mut o)?;
    let tick_upper_index = read_i32(data, &mut o)?;
    let fee_growth_checkpoint_a = read_u128(data, &mut o)?;
    let fee_owed_a = read_u64(data, &mut o)?;
    let fee_growth_checkpoint_b = read_u128(data, &mut o)?;
    let fee_owed_b = read_u64(data, &mut o)?;
    let mut reward_infos = [
        PositionReward {
            growth_inside_checkpoint: 0,
            amount_owed: 0,
        },
        PositionReward {
            growth_inside_checkpoint: 0,
            amount_owed: 0,
        },
        PositionReward {
            growth_inside_checkpoint: 0,
            amount_owed: 0,
        },
    ];
    for slot in &mut reward_infos {
        slot.growth_inside_checkpoint = read_u128(data, &mut o)?;
        slot.amount_owed = read_u64(data, &mut o)?;
    }
    Ok(Position {
        whirlpool,
        position_mint,
        liquidity,
        tick_lower_index,
        tick_upper_index,
        fee_growth_checkpoint_a,
        fee_owed_a,
        fee_growth_checkpoint_b,
        fee_owed_b,
        reward_infos,
    })
}

pub fn decode_whirlpool_header(data: &[u8]) -> Result<WhirlpoolHeader> {
    let s = decode_whirlpool_stats(Pubkey::default(), data)?;
    Ok(WhirlpoolHeader {
        token_mint_a: s.token_mint_a,
        token_mint_b: s.token_mint_b,
        tick_current_index: s.tick_current_index,
    })
}

pub fn decode_whirlpool_stats(address: Pubkey, data: &[u8]) -> Result<WhirlpoolStats> {
    Ok(decode_whirlpool_full(address, data)?.stats)
}

pub fn decode_whirlpool_full(address: Pubkey, data: &[u8]) -> Result<WhirlpoolFull> {
    if data.len() < WHIRLPOOL_MIN_LEN {
        bail!("whirlpool account too short: {}", data.len());
    }
    if data[0..8] != WHIRLPOOL_DISCRIMINATOR {
        bail!("not a Whirlpool account (bad discriminator)");
    }
    let tick_spacing = u16::from_le_bytes(data[8 + 33..8 + 35].try_into()?);
    let fee_rate = u16::from_le_bytes(data[8 + 37..8 + 39].try_into()?) as u32;
    let liquidity = u128::from_le_bytes(data[8 + 41..8 + 57].try_into()?);
    let sqrt_price = u128::from_le_bytes(data[8 + 57..8 + 73].try_into()?);
    let tick_current_index = i32::from_le_bytes(data[8 + 73..8 + 77].try_into()?);
    let protocol_fee_owed_a = u64::from_le_bytes(data[8 + 77..8 + 85].try_into()?);
    let protocol_fee_owed_b = u64::from_le_bytes(data[8 + 85..8 + 93].try_into()?);
    let token_mint_a = Pubkey::new_from_array(data[8 + 93..8 + 125].try_into()?);
    let token_vault_a = Pubkey::new_from_array(data[8 + 125..8 + 157].try_into()?);
    let fee_growth_global_a = u128::from_le_bytes(data[8 + 157..8 + 173].try_into()?);
    let token_mint_b = Pubkey::new_from_array(data[8 + 173..8 + 205].try_into()?);
    let token_vault_b = Pubkey::new_from_array(data[8 + 205..8 + 237].try_into()?);
    let fee_growth_global_b = u128::from_le_bytes(data[8 + 237..8 + 253].try_into()?);
    let reward_last_updated_timestamp = if data.len() >= 8 + 261 {
        u64::from_le_bytes(data[8 + 253..8 + 261].try_into()?)
    } else {
        0
    };

    let mut reward_infos = [
        WhirlpoolRewardInfo {
            mint: Pubkey::default(),
            vault: Pubkey::default(),
            emissions_per_second_x64: 0,
            growth_global_x64: 0,
        },
        WhirlpoolRewardInfo {
            mint: Pubkey::default(),
            vault: Pubkey::default(),
            emissions_per_second_x64: 0,
            growth_global_x64: 0,
        },
        WhirlpoolRewardInfo {
            mint: Pubkey::default(),
            vault: Pubkey::default(),
            emissions_per_second_x64: 0,
            growth_global_x64: 0,
        },
    ];
    // Each WhirlpoolRewardInfo = 128 bytes starting at offset 8+261.
    if data.len() >= WHIRLPOOL_FULL_LEN {
        let mut o = 8 + 261;
        for slot in &mut reward_infos {
            slot.mint = read_pubkey(data, &mut o)?;
            slot.vault = read_pubkey(data, &mut o)?;
            o += 32; // extension
            slot.emissions_per_second_x64 = read_u128(data, &mut o)?;
            slot.growth_global_x64 = read_u128(data, &mut o)?;
        }
    }

    Ok(WhirlpoolFull {
        stats: PoolStats {
            address,
            token_mint_a,
            token_mint_b,
            token_vault_a,
            token_vault_b,
            tick_spacing,
            fee_rate,
            liquidity,
            sqrt_price,
            tick_current_index,
            index_space: IndexSpace::Tick,
            fee_growth_global_a,
            fee_growth_global_b,
            protocol_fee_owed_a,
            protocol_fee_owed_b,
        },
        reward_last_updated_timestamp,
        reward_infos,
    })
}

pub fn decode_tick_array(data: &[u8]) -> Result<TickArrayData> {
    if data.len() < 12 {
        bail!("tick array too short");
    }
    let disc = &data[0..8];
    if disc == TICK_ARRAY_DISCRIMINATOR {
        decode_fixed_tick_array(data)
    } else if disc == DYNAMIC_TICK_ARRAY_DISCRIMINATOR {
        decode_dynamic_tick_array(data)
    } else {
        bail!("not a TickArray / DynamicTickArray account");
    }
}

fn decode_fixed_tick_array(data: &[u8]) -> Result<TickArrayData> {
    if data.len() < FIXED_TICK_ARRAY_LEN {
        bail!("fixed tick array too short: {}", data.len());
    }
    let start_tick_index = i32::from_le_bytes(data[8..12].try_into()?);
    let mut ticks = Vec::with_capacity(88);
    let mut o = 12usize;
    for i in 0..88 {
        let initialized = data[o] != 0;
        o += 1;
        let liquidity_net = i128::from_le_bytes(data[o..o + 16].try_into()?);
        o += 16;
        let liquidity_gross = u128::from_le_bytes(data[o..o + 16].try_into()?);
        o += 16;
        let fee_growth_outside_a = u128::from_le_bytes(data[o..o + 16].try_into()?);
        o += 16;
        let fee_growth_outside_b = u128::from_le_bytes(data[o..o + 16].try_into()?);
        o += 16;
        let mut reward_growths_outside = [0u128; 3];
        for r in &mut reward_growths_outside {
            *r = u128::from_le_bytes(data[o..o + 16].try_into()?);
            o += 16;
        }
        ticks.push(TickPoint {
            index: start_tick_index + i as i32,
            initialized,
            liquidity_net,
            liquidity_gross,
            fee_growth_outside_a,
            fee_growth_outside_b,
            reward_growths_outside,
        });
    }
    Ok(TickArrayData {
        start_tick_index,
        ticks,
    })
}

fn decode_dynamic_tick_array(data: &[u8]) -> Result<TickArrayData> {
    if data.len() < 8 + 4 + 32 + 16 {
        bail!("dynamic tick array too short");
    }
    let start_tick_index = i32::from_le_bytes(data[8..12].try_into()?);
    let mut o = 8 + 4 + 32 + 16;
    let mut ticks = Vec::with_capacity(88);
    for i in 0..88 {
        let tag = *data.get(o).context("dynamic tick tag")?;
        o += 1;
        let tick = match tag {
            0 => TickPoint {
                index: start_tick_index + i as i32,
                initialized: false,
                liquidity_net: 0,
                liquidity_gross: 0,
                fee_growth_outside_a: 0,
                fee_growth_outside_b: 0,
                reward_growths_outside: [0; 3],
            },
            1 => {
                let end = o + DYNAMIC_TICK_DATA_LEN;
                let chunk = data.get(o..end).context("dynamic tick data")?;
                let liquidity_net = i128::from_le_bytes(chunk[0..16].try_into()?);
                let liquidity_gross = u128::from_le_bytes(chunk[16..32].try_into()?);
                let fee_growth_outside_a = u128::from_le_bytes(chunk[32..48].try_into()?);
                let fee_growth_outside_b = u128::from_le_bytes(chunk[48..64].try_into()?);
                let mut reward_growths_outside = [0u128; 3];
                for (j, r) in reward_growths_outside.iter_mut().enumerate() {
                    let s = 64 + j * 16;
                    *r = u128::from_le_bytes(chunk[s..s + 16].try_into()?);
                }
                o = end;
                TickPoint {
                    index: start_tick_index + i as i32,
                    initialized: true,
                    liquidity_net,
                    liquidity_gross,
                    fee_growth_outside_a,
                    fee_growth_outside_b,
                    reward_growths_outside,
                }
            }
            _ => bail!("unknown DynamicTick tag {tag}"),
        };
        ticks.push(tick);
    }
    Ok(TickArrayData {
        start_tick_index,
        ticks,
    })
}

pub fn tick_array_start_index(tick: i32, tick_spacing: u16) -> i32 {
    let ticks_in_array = TICK_ARRAY_SIZE * tick_spacing as i32;
    let mut start = tick / ticks_in_array;
    if tick < 0 && tick % ticks_in_array != 0 {
        start -= 1;
    }
    start * ticks_in_array
}

pub fn tick_array_pda(whirlpool: &Pubkey, start_tick_index: i32, program: &Pubkey) -> Pubkey {
    let start = start_tick_index.to_string();
    Pubkey::find_program_address(
        &[b"tick_array", whirlpool.as_ref(), start.as_bytes()],
        program,
    )
    .0
}

fn read_pubkey(data: &[u8], o: &mut usize) -> Result<Pubkey> {
    let end = *o + 32;
    let bytes: [u8; 32] = data
        .get(*o..end)
        .context("pubkey")?
        .try_into()
        .context("pubkey len")?;
    *o = end;
    Ok(Pubkey::new_from_array(bytes))
}

fn read_u128(data: &[u8], o: &mut usize) -> Result<u128> {
    let end = *o + 16;
    let bytes: [u8; 16] = data
        .get(*o..end)
        .context("u128")?
        .try_into()
        .context("u128 len")?;
    *o = end;
    Ok(u128::from_le_bytes(bytes))
}

fn read_u64(data: &[u8], o: &mut usize) -> Result<u64> {
    let end = *o + 8;
    let bytes: [u8; 8] = data
        .get(*o..end)
        .context("u64")?
        .try_into()
        .context("u64 len")?;
    *o = end;
    Ok(u64::from_le_bytes(bytes))
}

fn read_i32(data: &[u8], o: &mut usize) -> Result<i32> {
    let end = *o + 4;
    let bytes: [u8; 4] = data
        .get(*o..end)
        .context("i32")?
        .try_into()
        .context("i32 len")?;
    *o = end;
    Ok(i32::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_short_position() {
        assert!(decode_position(&[0u8; 8]).is_err());
    }

    #[test]
    fn tick_array_start_positive() {
        assert_eq!(tick_array_start_index(200, 2), 176);
    }

    #[test]
    fn tick_array_pda_zero_golden() {
        let program: Pubkey = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc"
            .parse()
            .unwrap();
        let whirlpool: Pubkey = "2kJmUjxWBwL2NGPBV2PiA5hWtmLCqcKY6reQgkrPtaeS"
            .parse()
            .unwrap();
        let pda = tick_array_pda(&whirlpool, 0, &program);
        assert_eq!(pda.to_string(), "8PhPzk7n4wU98Z6XCbVtPai2LtXSxYnfjkmgWuoAU8Zy");
    }
}
