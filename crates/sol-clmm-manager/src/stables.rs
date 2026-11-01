//! Popular USD-ish stablecoin mints (mainnet) for on-chain price heuristics.

use solana_pubkey::Pubkey;
use std::str::FromStr;
use std::sync::OnceLock;

/// Mainnet Orca WhirlpoolsConfig (official docs).
pub const WHIRLPOOLS_CONFIG: &str = "2LecshUwdy9xi7meFgHtFJQNSKk4KdTrcpvaB56dP2NQ";

pub const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
pub const USDT: &str = "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB";
pub const PYUSD: &str = "2b1kV6DkPAnxd5ixfnxCpjxmKwqjjaYmCZfHsFu24GXo";
pub const USDY: &str = "A1KLoBrKBde8Ty9qtNQUtq3C2ortoC3u7twggz7sEto6";
/// Ethena USDe
pub const USDE: &str = "DEkqHyPN7GFyrVFbDp3Sy3PfX7pPDixE9eUV2oc8agnF";
pub const WSOL: &str = "So11111111111111111111111111111111111111112";

/// Tick spacings to probe when looking for a token/stable Whirlpool.
pub const PRICE_TICK_SPACINGS: &[u16] = &[1, 2, 4, 8, 16, 64, 96, 128, 256];

/// Raydium CLMM `AmmConfig` indices to probe (`["amm_config", index_be]`).
/// Covers the common mainnet fee/tick tiers (0..=20 is enough for hops).
pub const RAYDIUM_AMM_CONFIG_INDICES: &[u16] = &[
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
];

/// Meteora DLMM permissionless v1 bin steps to probe.
pub const METEORA_BIN_STEPS: &[u16] = &[1, 2, 5, 10, 15, 20, 25, 50, 80, 100, 125, 200, 250, 400];

/// Meteora DLMM v2 `base_factor` values paired with [`METEORA_BIN_STEPS`].
pub const METEORA_BASE_FACTORS: &[u16] = &[10_000, 5_000, 2_000, 1_000, 500];

pub fn stablecoin_mints() -> &'static [Pubkey] {
    static MINTS: OnceLock<Vec<Pubkey>> = OnceLock::new();
    MINTS.get_or_init(|| {
        [USDC, USDT, PYUSD, USDY, USDE]
            .into_iter()
            .filter_map(|s| Pubkey::from_str(s).ok())
            .collect()
    })
}

pub fn is_stablecoin(mint: &Pubkey) -> bool {
    stablecoin_mints().iter().any(|s| s == mint)
}

pub fn whirlpools_config() -> Pubkey {
    Pubkey::from_str(WHIRLPOOLS_CONFIG).expect("WHIRLPOOLS_CONFIG")
}

pub fn wsol_mint() -> Pubkey {
    Pubkey::from_str(WSOL).expect("WSOL")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usdc_is_stable() {
        let usdc = Pubkey::from_str(USDC).unwrap();
        assert!(is_stablecoin(&usdc));
        assert!(!is_stablecoin(&wsol_mint()));
    }
}
