//! Raydium CLMM account decoders (PersonalPosition / PoolState / TickArray / AmmConfig).

use anyhow::{Context, Result, bail};
use sol_clmm_manager::clmm_math::TickPoint;
use sol_clmm_manager::pool::{IndexSpace, PoolStats};
use solana_pubkey::Pubkey;

pub const PERSONAL_POSITION_DISC: [u8; 8] = [70, 111, 150, 126, 230, 15, 25, 117];
pub const TICK_ARRAY_DISC: [u8; 8] = [192, 155, 85, 205, 49, 249, 129, 42];
pub const AMM_CONFIG_DISC: [u8; 8] = [218, 244, 33, 104, 203, 203, 43, 111];

pub const TICK_ARRAY_SIZE: i32 = 60;
pub const TICK_STATE_LEN: usize = 168;
pub const NUM_REWARDS: usize = 3;

#[derive(Debug, Clone)]
pub struct PositionReward {
    pub growth_inside_last_x64: u128,
    pub reward_amount_owed: u64,
}

#[derive(Debug, Clone)]
pub struct PersonalPosition {
    pub nft_mint: Pubkey,
    pub pool_id: Pubkey,
    pub tick_lower: i32,
    pub tick_upper: i32,
    pub liquidity: u128,
    pub fee_growth_inside0_last_x64: u128,
    pub fee_growth_inside1_last_x64: u128,
    pub fees_owed0: u64,
    pub fees_owed1: u64,
    pub reward_infos: [PositionReward; NUM_REWARDS],
}

/// Raydium `RewardState`: 0 uninit · 1 initialized · 2 opening · 3 ended.
const REWARD_STATE_ENDED: u8 = 3;

#[derive(Debug, Clone)]
pub struct PoolRewardInfo {
    #[allow(dead_code)]
    pub reward_state: u8,
    #[allow(dead_code)]
    pub open_time: u64,
    pub end_time: u64,
    pub last_update_time: u64,
    pub emissions_per_second_x64: u128,
    pub token_mint: Pubkey,
    pub token_vault: Pubkey,
    pub reward_growth_global_x64: u128,
}

impl PoolRewardInfo {
    pub fn initialized(&self) -> bool {
        self.token_mint != Pubkey::default() && self.token_vault != Pubkey::default()
    }

    /// Emitting now (not ended / not started / zero rate).
    pub fn actively_emitting(&self, now: u64) -> bool {
        if !self.initialized() || self.emissions_per_second_x64 == 0 {
            return false;
        }
        if self.reward_state == REWARD_STATE_ENDED {
            return false;
        }
        if self.open_time > 0 && now < self.open_time {
            return false;
        }
        if self.end_time > 0 && now >= self.end_time {
            return false;
        }
        true
    }

    /// Need collect + destination ATA: active schedule or unclaimed owed.
    pub fn claimable(&self, amount_owed: u64, now: u64) -> bool {
        self.initialized() && (amount_owed > 0 || self.actively_emitting(now))
    }
}

#[derive(Debug, Clone)]
pub struct PoolHeader {
    pub amm_config: Pubkey,
    pub mint0: Pubkey,
    pub mint1: Pubkey,
    pub vault0: Pubkey,
    pub vault1: Pubkey,
    pub tick_spacing: u16,
    pub liquidity: u128,
    pub sqrt_price: u128,
    pub tick_current: i32,
    pub fee_growth_global0_x64: u128,
    pub fee_growth_global1_x64: u128,
    pub protocol_fees_token0: u64,
    pub protocol_fees_token1: u64,
    pub reward_infos: [PoolRewardInfo; NUM_REWARDS],
}

#[derive(Debug, Clone)]
pub struct TickArrayData {
    pub ticks: Vec<TickPoint>,
}

pub fn decode_personal_position(data: &[u8]) -> Result<PersonalPosition> {
    // disc + bump + nft + pool + lower + upper + liq + fg0 + fg1 + owed0 + owed1 + 3*reward
    const MIN: usize = 8 + 1 + 32 + 32 + 4 + 4 + 16 + 16 + 16 + 8 + 8 + 3 * 24;
    if data.len() < MIN {
        bail!("personal position too short: {}", data.len());
    }
    if data[0..8] != PERSONAL_POSITION_DISC {
        bail!("personal position discriminator mismatch");
    }
    let mut o = 8 + 1; // disc + bump
    let nft_mint = read_pubkey(data, &mut o)?;
    let pool_id = read_pubkey(data, &mut o)?;
    let tick_lower = read_i32(data, &mut o)?;
    let tick_upper = read_i32(data, &mut o)?;
    let liquidity = read_u128(data, &mut o)?;
    let fee_growth_inside0_last_x64 = read_u128(data, &mut o)?;
    let fee_growth_inside1_last_x64 = read_u128(data, &mut o)?;
    let fees_owed0 = read_u64(data, &mut o)?;
    let fees_owed1 = read_u64(data, &mut o)?;
    let mut reward_infos = [
        PositionReward {
            growth_inside_last_x64: 0,
            reward_amount_owed: 0,
        },
        PositionReward {
            growth_inside_last_x64: 0,
            reward_amount_owed: 0,
        },
        PositionReward {
            growth_inside_last_x64: 0,
            reward_amount_owed: 0,
        },
    ];
    for slot in &mut reward_infos {
        slot.growth_inside_last_x64 = read_u128(data, &mut o)?;
        slot.reward_amount_owed = read_u64(data, &mut o)?;
    }
    Ok(PersonalPosition {
        nft_mint,
        pool_id,
        tick_lower,
        tick_upper,
        liquidity,
        fee_growth_inside0_last_x64,
        fee_growth_inside1_last_x64,
        fees_owed0,
        fees_owed1,
        reward_infos,
    })
}

/// `RewardInfo::LEN` (packed) and start offset inside PoolState account data.
pub const REWARD_INFO_LEN: usize = 169;
pub const REWARD_INFOS_OFFSET: usize = 397;

pub fn decode_pool_header(data: &[u8]) -> Result<PoolHeader> {
    const MIN: usize = 8 + 1 + 32 * 7 + 1 + 1 + 2 + 16 + 16 + 4 + 2 + 2 + 16 + 16 + 8 + 8;
    if data.len() < MIN {
        bail!("pool state too short: {}", data.len());
    }
    let mut o = 8 + 1;
    let amm_config = read_pubkey(data, &mut o)?;
    o += 32; // owner
    let mint0 = read_pubkey(data, &mut o)?;
    let mint1 = read_pubkey(data, &mut o)?;
    let vault0 = read_pubkey(data, &mut o)?;
    let vault1 = read_pubkey(data, &mut o)?;
    o += 32; // observation
    o += 1 + 1; // decimals
    let tick_spacing = read_u16(data, &mut o)?;
    let liquidity = read_u128(data, &mut o)?;
    let sqrt_price = read_u128(data, &mut o)?;
    let tick_current = read_i32(data, &mut o)?;
    o += 2 + 2; // padding3/4
    let fee_growth_global0_x64 = read_u128(data, &mut o)?;
    let fee_growth_global1_x64 = read_u128(data, &mut o)?;
    let protocol_fees_token0 = read_u64(data, &mut o)?;
    let protocol_fees_token1 = read_u64(data, &mut o)?;

    let mut reward_infos = [
        PoolRewardInfo {
            reward_state: 0,
            open_time: 0,
            end_time: 0,
            last_update_time: 0,
            emissions_per_second_x64: 0,
            token_mint: Pubkey::default(),
            token_vault: Pubkey::default(),
            reward_growth_global_x64: 0,
        },
        PoolRewardInfo {
            reward_state: 0,
            open_time: 0,
            end_time: 0,
            last_update_time: 0,
            emissions_per_second_x64: 0,
            token_mint: Pubkey::default(),
            token_vault: Pubkey::default(),
            reward_growth_global_x64: 0,
        },
        PoolRewardInfo {
            reward_state: 0,
            open_time: 0,
            end_time: 0,
            last_update_time: 0,
            emissions_per_second_x64: 0,
            token_mint: Pubkey::default(),
            token_vault: Pubkey::default(),
            reward_growth_global_x64: 0,
        },
    ];
    let need = REWARD_INFOS_OFFSET + REWARD_INFO_LEN * NUM_REWARDS;
    if data.len() >= need {
        for (i, slot) in reward_infos.iter_mut().enumerate() {
            let base = REWARD_INFOS_OFFSET + i * REWARD_INFO_LEN;
            // packed: u8 + 3*u64 + u128 + 2*u64 + 3*Pubkey + u128
            *slot = PoolRewardInfo {
                reward_state: data[base],
                open_time: u64::from_le_bytes(data[base + 1..base + 9].try_into()?),
                end_time: u64::from_le_bytes(data[base + 9..base + 17].try_into()?),
                last_update_time: u64::from_le_bytes(data[base + 17..base + 25].try_into()?),
                emissions_per_second_x64: u128::from_le_bytes(
                    data[base + 25..base + 41].try_into()?,
                ),
                // skip reward_total_emitted(8) + reward_claimed(8) = 16 → mint at +57
                token_mint: Pubkey::new_from_array(data[base + 57..base + 89].try_into()?),
                token_vault: Pubkey::new_from_array(data[base + 89..base + 121].try_into()?),
                // mint(32)+vault(32)+authority(32)=96 → growth at +57+96 = +153
                reward_growth_global_x64: u128::from_le_bytes(
                    data[base + 153..base + 169].try_into()?,
                ),
            };
        }
    }

    Ok(PoolHeader {
        amm_config,
        mint0,
        mint1,
        vault0,
        vault1,
        tick_spacing,
        liquidity,
        sqrt_price,
        tick_current,
        fee_growth_global0_x64,
        fee_growth_global1_x64,
        protocol_fees_token0,
        protocol_fees_token1,
        reward_infos,
    })
}

pub fn decode_amm_config_trade_fee(data: &[u8]) -> Result<u32> {
    if data.len() < 8 + 1 + 2 + 32 + 4 + 4 {
        bail!("amm config too short");
    }
    if data[0..8] != AMM_CONFIG_DISC {
        bail!("amm config discriminator mismatch");
    }
    let o = 8 + 1 + 2 + 32 + 4;
    Ok(u32::from_le_bytes(data[o..o + 4].try_into()?))
}

pub fn pool_stats_from_header(address: Pubkey, h: &PoolHeader, trade_fee_rate: u32) -> PoolStats {
    PoolStats {
        address,
        token_mint_a: h.mint0,
        token_mint_b: h.mint1,
        token_vault_a: h.vault0,
        token_vault_b: h.vault1,
        tick_spacing: h.tick_spacing,
        fee_rate: trade_fee_rate,
        liquidity: h.liquidity,
        sqrt_price: h.sqrt_price,
        tick_current_index: h.tick_current,
        index_space: IndexSpace::Tick,
        fee_growth_global_a: h.fee_growth_global0_x64,
        fee_growth_global_b: h.fee_growth_global1_x64,
        protocol_fee_owed_a: h.protocol_fees_token0,
        protocol_fee_owed_b: h.protocol_fees_token1,
    }
}

pub fn decode_tick_array(data: &[u8]) -> Result<TickArrayData> {
    let min = 8 + 32 + 4 + TICK_ARRAY_SIZE as usize * TICK_STATE_LEN;
    if data.len() < min {
        bail!("tick array too short: {}", data.len());
    }
    if data[0..8] != TICK_ARRAY_DISC {
        bail!("tick array discriminator mismatch");
    }
    let mut ticks = Vec::with_capacity(TICK_ARRAY_SIZE as usize);
    let mut o = 8 + 32 + 4;
    for _ in 0..TICK_ARRAY_SIZE {
        let tick = i32::from_le_bytes(data[o..o + 4].try_into()?);
        let liquidity_net = i128::from_le_bytes(data[o + 4..o + 20].try_into()?);
        let liquidity_gross = u128::from_le_bytes(data[o + 20..o + 36].try_into()?);
        let fee_growth_outside_a = u128::from_le_bytes(data[o + 36..o + 52].try_into()?);
        let fee_growth_outside_b = u128::from_le_bytes(data[o + 52..o + 68].try_into()?);
        let mut reward_growths_outside = [0u128; 3];
        for (j, r) in reward_growths_outside.iter_mut().enumerate() {
            let s = o + 68 + j * 16;
            *r = u128::from_le_bytes(data[s..s + 16].try_into()?);
        }
        ticks.push(TickPoint {
            index: tick,
            initialized: liquidity_gross > 0,
            liquidity_net,
            liquidity_gross,
            fee_growth_outside_a,
            fee_growth_outside_b,
            reward_growths_outside,
        });
        o += TICK_STATE_LEN;
    }
    Ok(TickArrayData { ticks })
}

pub fn tick_array_start_index(tick: i32, tick_spacing: u16) -> i32 {
    let ticks_in_array = TICK_ARRAY_SIZE * tick_spacing as i32;
    let mut start = tick / ticks_in_array;
    if tick < 0 && tick % ticks_in_array != 0 {
        start -= 1;
    }
    start * ticks_in_array
}

pub fn tick_array_pda(pool: &Pubkey, start_tick_index: i32, program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[
            b"tick_array",
            pool.as_ref(),
            &start_tick_index.to_be_bytes(),
        ],
        program,
    )
    .0
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

fn read_u64(data: &[u8], o: &mut usize) -> Result<u64> {
    let end = *o + 8;
    let v = u64::from_le_bytes(data.get(*o..end).context("u64")?.try_into()?);
    *o = end;
    Ok(v)
}

fn read_u128(data: &[u8], o: &mut usize) -> Result<u128> {
    let end = *o + 16;
    let v = u128::from_le_bytes(data.get(*o..end).context("u128")?.try_into()?);
    *o = end;
    Ok(v)
}
