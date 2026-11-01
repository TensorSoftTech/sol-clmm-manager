//! Titan Developers Portal — https://developer.titan.exchange/
//!
//! `GET /api/v1/quote/swap` → `metadata.inputPriceUSD` (quote only; no swap send).
//! Via `reqwest::blocking`. Portal expects `x-api-key` (Sandbox free OK).

use crate::rpc::RpcMeter;
use anyhow::{Context, Result, bail};
use solana_pubkey::Pubkey;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const TITAN_PORTAL_QUOTE: &str = "https://portal.api.titan.exchange/api/v1/quote/swap";

const MIN_INTERVAL_FREE: Duration = Duration::from_millis(1100);
const MIN_INTERVAL_PAID: Duration = Duration::from_millis(200);

const QUOTE_WALLET: &str = "11111111111111111111111111111111";

static LAST_CALL: Mutex<Option<Instant>> = Mutex::new(None);

pub fn fetch_usd_prices_with_tier(
    mints: &[Pubkey],
    decimals: &[u8],
    api_key: Option<&str>,
    paid_plan: bool,
    meter: Option<&RpcMeter>,
) -> Result<HashMap<Pubkey, f64>> {
    let key = api_key.map(str::trim).filter(|s| !s.is_empty());
    let Some(key) = key else {
        bail!(
            "Titan needs an API key (free Sandbox OK) · developer.titan.exchange · settings"
        );
    };
    if mints.is_empty() {
        return Ok(HashMap::new());
    }

    let usdc: Pubkey = crate::stables::USDC.parse().context("USDC")?;
    let mut out = HashMap::new();

    for (mint, &dec) in mints.iter().zip(decimals.iter()) {
        if crate::stables::is_stablecoin(mint) {
            out.insert(*mint, 1.0);
            continue;
        }
        throttle(paid_plan)?;
        let short = {
            let s = mint.to_string();
            format!("{}…{}", &s[..4.min(s.len())], &s[s.len().saturating_sub(4)..])
        };
        match quote_mint_usd(mint, dec, &usdc, key) {
            Ok(p) => {
                if let Some(m) = meter {
                    m.record("titan.quote", short, true);
                }
                out.insert(*mint, p);
            }
            Err(_) => {
                if let Some(m) = meter {
                    m.record("titan.quote", short, false);
                }
            }
        }
    }
    Ok(out)
}

fn quote_mint_usd(mint: &Pubkey, decimals: u8, usdc: &Pubkey, api_key: &str) -> Result<f64> {
    let amount = 10u64.saturating_pow(decimals.min(9) as u32).max(1);
    let url = format!(
        "{TITAN_PORTAL_QUOTE}?inputMint={mint}&outputMint={usdc}&amount={amount}&userPublicKey={QUOTE_WALLET}&numQuotes=1&simulate=false"
    );

    let resp = http_client()?
        .get(&url)
        .header("x-api-key", api_key)
        .header("Accept", "application/json")
        .header("User-Agent", "sol-clmm-manager/0.1 (unofficial; quote-only)")
        .send()
        .context("titan quote HTTP")?;

    let status = resp.status();
    if status.as_u16() == 401 {
        bail!("titan invalid API key (401)");
    }
    if status.as_u16() == 429 {
        bail!("titan 429 · sandbox is ~1 RPS · wait or upgrade plan");
    }
    if !status.is_success() {
        bail!("titan HTTP {status}");
    }

    let value: serde_json::Value = resp.json().context("titan JSON")?;
    let meta = value.get("metadata");

    if let Some(p) = meta
        .and_then(|m| m.get("inputPriceUSD"))
        .and_then(|v| v.as_f64())
    {
        if p.is_finite() && p > 0.0 {
            return Ok(p);
        }
    }

    let winner = meta
        .and_then(|m| {
            m.get("ExpectedWinner")
                .or_else(|| m.get("expectedWinner"))
                .and_then(|v| v.as_str())
        })
        .map(|s| s.to_string());

    let quotes = value
        .get("quotes")
        .and_then(|q| q.as_object())
        .context("titan quotes missing")?;

    let route = if let Some(w) = winner.as_deref() {
        quotes.get(w)
    } else {
        quotes.values().next()
    }
    .context("titan no route")?;

    let in_amount = route
        .get("inAmount")
        .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f as u64)))
        .unwrap_or(amount) as f64;
    let out_amount = route
        .get("outAmount")
        .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f as u64)))
        .context("titan outAmount")? as f64;

    let in_ui = in_amount / 10f64.powi(decimals as i32);
    let out_ui = out_amount / 1e6;
    if in_ui <= 0.0 {
        bail!("titan zero in");
    }
    Ok(out_ui / in_ui)
}

fn http_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .context("reqwest client")
}

fn throttle(paid_plan: bool) -> Result<()> {
    let min = if paid_plan {
        MIN_INTERVAL_PAID
    } else {
        MIN_INTERVAL_FREE
    };
    let mut guard = LAST_CALL.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(prev) = *guard {
        let elapsed = prev.elapsed();
        if elapsed < min {
            std::thread::sleep(min - elapsed);
        }
    }
    *guard = Some(Instant::now());
    Ok(())
}
