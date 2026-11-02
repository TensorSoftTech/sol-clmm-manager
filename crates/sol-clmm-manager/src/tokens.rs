//! Mint decimals + human symbol resolution (SQLite cache when available).

use crate::pricing::PriceApiKeys;
use crate::quotes::jupiter;
use crate::rpc::Rpc;
use crate::store::Store;
use anyhow::{Context, Result};
use solana_pubkey::Pubkey;
use std::collections::HashMap;
use std::str::FromStr;

const METADATA_PROGRAM: &str = "metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s";

#[derive(Debug, Clone)]
pub struct TokenInfo {
    pub mint: Pubkey,
    pub symbol: String,
    pub decimals: u8,
}

pub fn resolve_tokens(
    rpc: &Rpc<'_>,
    mint_a: Pubkey,
    mint_b: Pubkey,
    store: Option<&Store>,
    refresh: bool,
) -> Result<(TokenInfo, TokenInfo)> {
    resolve_tokens_with_keys(rpc, mint_a, mint_b, store, refresh, None)
}

/// Same as [`resolve_tokens`] with optional Jupiter/Titan keys (session overrides SQLite).
pub fn resolve_tokens_with_keys(
    rpc: &Rpc<'_>,
    mint_a: Pubkey,
    mint_b: Pubkey,
    store: Option<&Store>,
    refresh: bool,
    keys: Option<&PriceApiKeys>,
) -> Result<(TokenInfo, TokenInfo)> {
    let map = resolve_mints_with_keys(rpc, &[mint_a, mint_b], store, refresh, keys)?;
    let a = map
        .get(&mint_a)
        .cloned()
        .context("missing token A after resolve")?;
    let b = map
        .get(&mint_b)
        .cloned()
        .context("missing token B after resolve")?;
    Ok((a, b))
}

/// Resolve many mints → symbol + decimals.
///
/// Order: known table → SQLite (non-stub) → Metaplex → Jupiter Tokens v2 (stubs)
/// → short mint prefix. **Decimals** prefer SQLite / known, then always fill gaps
/// from the on-chain **mint account via RPC** (SPL / Token-2022 layout).
///
/// `refresh = true` still **keeps** good SQLite symbols; only stubs / unknowns
/// are re-fetched (and always written back).
pub fn resolve_mints(
    rpc: &Rpc<'_>,
    mints: &[Pubkey],
    store: Option<&Store>,
    refresh: bool,
) -> Result<HashMap<Pubkey, TokenInfo>> {
    resolve_mints_with_keys(rpc, mints, store, refresh, None)
}

pub fn resolve_mints_with_keys(
    rpc: &Rpc<'_>,
    mints: &[Pubkey],
    store: Option<&Store>,
    refresh: bool,
    keys: Option<&PriceApiKeys>,
) -> Result<HashMap<Pubkey, TokenInfo>> {
    let mut out: HashMap<Pubkey, TokenInfo> = HashMap::new();
    let mut missing: Vec<Pubkey> = Vec::new();

    for mint in mints {
        if out.contains_key(mint) {
            continue;
        }
        if let Some(known) = known_symbol(mint) {
            let decimals = cached(store, mint)?
                .map(|c| c.decimals)
                .filter(|d| *d > 0)
                .or_else(|| known_decimals(mint))
                .unwrap_or(0);
            // Symbol known — still OK with decimals=0; RPC mint pass fills below.
            let info = TokenInfo {
                mint: *mint,
                symbol: known,
                decimals,
            };
            out.insert(*mint, info);
            continue;
        }
        // Prefer real tickers already in SQLite (even when refresh=true).
        if let Some(info) = cached(store, mint)?
            && !is_stub_symbol(mint, &info.symbol)
        {
            out.insert(*mint, info);
            continue;
        }
        missing.push(*mint);
    }

    if !missing.is_empty() {
        let accounts = get_multiple_accounts_chunked(rpc, &missing)?;
        for (mint, maybe) in missing.iter().zip(accounts) {
            let decimals = maybe
                .as_ref()
                .and_then(|a| read_mint_decimals(&a.data).ok())
                .or_else(|| known_decimals(mint))
                .unwrap_or(0);
            // Prefer Metaplex metadata; leave stubs for Jupiter to overwrite.
            let symbol = known_symbol(mint)
                .or_else(|| fetch_metaplex_symbol(rpc, mint))
                .unwrap_or_else(|| short_symbol(mint));
            let info = TokenInfo {
                mint: *mint,
                symbol,
                decimals,
            };
            out.insert(*mint, info);
        }
    }

    // Jupiter Tokens API fills real symbols when Metaplex / cache only had mint stubs.
    let need_jup: Vec<Pubkey> = out
        .iter()
        .filter(|(mint, info)| {
            known_symbol(mint).is_none() && (refresh || is_stub_symbol(mint, &info.symbol))
        })
        .map(|(m, _)| *m)
        .collect();

    if !need_jup.is_empty() {
        let (jup_key, paid_fast) = jupiter_settings(store, keys);
        if let Ok(meta) =
            jupiter::fetch_token_meta(&need_jup, jup_key.as_deref(), paid_fast, Some(rpc.meter()))
        {
            for (mint, m) in meta {
                if let Some(info) = out.get_mut(&mint)
                    && !m.symbol.trim().is_empty()
                {
                    info.symbol = m.symbol;
                    if let Some(d) = m.decimals
                        && d > 0
                    {
                        info.decimals = d;
                    }
                }
            }
        }
    }

    // Always backfill missing decimals from on-chain mint accounts (RPC).
    fill_decimals_from_rpc(rpc, &mut out)?;

    for info in out.values() {
        persist(store, info);
    }
    Ok(out)
}

/// Read SPL / Token-2022 mint accounts for any entry still missing decimals.
fn fill_decimals_from_rpc(
    rpc: &Rpc<'_>,
    out: &mut HashMap<Pubkey, TokenInfo>,
) -> Result<()> {
    let need: Vec<Pubkey> = out
        .iter()
        .filter(|(_, info)| info.decimals == 0)
        .map(|(m, _)| *m)
        .collect();
    if need.is_empty() {
        return Ok(());
    }
    let accounts = get_multiple_accounts_chunked(rpc, &need)?;
    for (mint, maybe) in need.iter().zip(accounts) {
        let Some(acc) = maybe else { continue };
        let Ok(d) = read_mint_decimals(&acc.data) else {
            continue;
        };
        if d == 0 {
            // Rare (NFT-like); fall back to known table when RPC reports 0.
            if let Some(kd) = known_decimals(mint)
                && let Some(info) = out.get_mut(mint)
            {
                info.decimals = kd;
            }
            continue;
        }
        if let Some(info) = out.get_mut(mint) {
            info.decimals = d;
        }
    }
    Ok(())
}

fn jupiter_settings(
    store: Option<&Store>,
    keys: Option<&PriceApiKeys>,
) -> (Option<String>, bool) {
    if let Some(k) = keys {
        let from_session = k.jupiter.clone().filter(|s| !s.is_empty());
        if from_session.is_some() {
            return (from_session, k.paid_fast);
        }
        // Fall through to SQLite when session has no key.
        if let Some(store) = store
            && let Ok(s) = store.load_settings()
        {
            return (s.jupiter_api_key, k.paid_fast || s.api_paid_fast);
        }
        return (None, k.paid_fast);
    }
    let Some(store) = store else {
        return (None, false);
    };
    match store.load_settings() {
        Ok(s) => (s.jupiter_api_key, s.api_paid_fast),
        Err(_) => (None, false),
    }
}

fn get_multiple_accounts_chunked(
    rpc: &Rpc<'_>,
    keys: &[Pubkey],
) -> Result<Vec<Option<solana_account::Account>>> {
    const CHUNK: usize = 100;
    let mut out = Vec::with_capacity(keys.len());
    for chunk in keys.chunks(CHUNK) {
        let part = rpc.get_multiple_accounts(chunk)?;
        out.extend(part);
    }
    Ok(out)
}

fn cached(store: Option<&Store>, mint: &Pubkey) -> Result<Option<TokenInfo>> {
    let Some(store) = store else {
        return Ok(None);
    };
    Ok(store.get_token(&mint.to_string())?.map(|c| TokenInfo {
        mint: *mint,
        symbol: c.symbol,
        decimals: c.decimals,
    }))
}

fn persist(store: Option<&Store>, info: &TokenInfo) {
    if let Some(store) = store {
        let _ = store.upsert_token(&info.mint.to_string(), &info.symbol, info.decimals);
    }
}

fn read_mint_decimals(data: &[u8]) -> Result<u8> {
    // SPL Mint: decimals at offset 44
    data.get(44).copied().context("mint too short for decimals")
}

fn known_symbol(mint: &Pubkey) -> Option<String> {
    known_mints()
        .get(&mint.to_string())
        .map(|s| (*s).to_string())
}

fn known_decimals(mint: &Pubkey) -> Option<u8> {
    match mint.to_string().as_str() {
        "So11111111111111111111111111111111111111112" => Some(9),
        "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
        | "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB"
        | "2b1kV6DkPAnxd5ixfnxCpjxmKwqjjaYmCZfHsFu24GXo"
        | "DEkqHyPN7GFyrVFbDp3Sy3PfX7pPDixE9eUV2oc8agnF"
        | "JuprjznTrTSp2UFa3ZBUFgwdAmtZCq4MQCwysN55USD"
        | "A1KLoBrKBde8Ty9qtNQUtq3C2ortoC3u7twggz7sEto6" => Some(6),
        "JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN" => Some(6),
        _ => None,
    }
}

/// True when symbol looks like our mint-prefix fallback (not a real ticker).
fn is_stub_symbol(mint: &Pubkey, symbol: &str) -> bool {
    let s = mint.to_string();
    !symbol.is_empty() && symbol.len() <= 4 && s.starts_with(symbol)
}

fn short_symbol(mint: &Pubkey) -> String {
    let s = mint.to_string();
    s.chars().take(4).collect()
}

fn fetch_metaplex_symbol(rpc: &Rpc<'_>, mint: &Pubkey) -> Option<String> {
    let program = Pubkey::from_str(METADATA_PROGRAM).ok()?;
    let (pda, _) =
        Pubkey::find_program_address(&[b"metadata", program.as_ref(), mint.as_ref()], &program);
    let account = rpc.get_account(&pda).ok()?;
    parse_metadata_symbol(&account.data)
}

/// Legacy Metaplex metadata: after key(1)+update_auth(32)+mint(32) comes name/symbol/uri
/// as fixed 4-byte LE length + bytes (Borsh strings), name max 32, symbol max 10.
fn parse_metadata_symbol(data: &[u8]) -> Option<String> {
    if data.len() < 1 + 32 + 32 + 4 {
        return None;
    }
    let mut o = 1 + 32 + 32;
    // skip name
    let name_len = u32::from_le_bytes(data.get(o..o + 4)?.try_into().ok()?) as usize;
    if name_len > 200 {
        return None;
    }
    o += 4 + name_len;
    // symbol
    let sym_len = u32::from_le_bytes(data.get(o..o + 4)?.try_into().ok()?) as usize;
    if sym_len > 32 {
        return None;
    }
    o += 4;
    let raw = data.get(o..o + sym_len)?;
    let s = std::str::from_utf8(raw).ok()?.trim().trim_end_matches('\0');
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

fn known_mints() -> HashMap<String, &'static str> {
    HashMap::from([
        ("So11111111111111111111111111111111111111112".into(), "SOL"),
        (
            "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".into(),
            "USDC",
        ),
        (
            "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB".into(),
            "USDT",
        ),
        ("mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So".into(), "mSOL"),
        (
            "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn".into(),
            "JitoSOL",
        ),
        ("bSo13r4TkiE4KumL71LsHTPpL2euBYLFx6h9HP3piy1".into(), "bSOL"),
        (
            "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263".into(),
            "BONK",
        ),
        ("7vfCXTUXx5WJV5JADk17DUJ4ksgau7utNKj4b963voxs".into(), "ETH"),
        (
            "HZ1JovNiVvGrGNiiYvEozEVgZ58xaU3RKwX8eACQBCt3".into(),
            "PYTH",
        ),
        ("JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN".into(), "JUP"),
        (
            "JuprjznTrTSp2UFa3ZBUFgwdAmtZCq4MQCwysN55USD".into(),
            "jupUSD",
        ),
        ("4k3Dyjzvzp8eMZWUXbBCjEvwSkkk59S5iCNLY3QrkX6R".into(), "RAY"),
        ("orcaEKTdK7LKz57vaAYr9QeNsVEPfiu6QeMU1kektZE".into(), "ORCA"),
        (
            "2b1kV6DkPAnxd5ixfnxCpjxmKwqjjaYmCZfHsFu24GXo".into(),
            "PYUSD",
        ),
        (
            "DEkqHyPN7GFyrVFbDp3Sy3PfX7pPDixE9eUV2oc8agnF".into(),
            "USDe",
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_detects_mint_prefix() {
        let mint: Pubkey = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
            .parse()
            .unwrap();
        assert!(is_stub_symbol(&mint, "EPjF"));
        assert!(!is_stub_symbol(&mint, "USDC"));
    }

    #[test]
    fn known_usdc() {
        let mint: Pubkey = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
            .parse()
            .unwrap();
        assert_eq!(known_symbol(&mint).as_deref(), Some("USDC"));
    }
}
