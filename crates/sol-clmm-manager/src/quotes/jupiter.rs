//! Jupiter Price API v3 (USD) via `reqwest::blocking`.
//!
//! | Path | Host | Key | Throttle |
//! |------|------|-----|----------|
//! | Free / keyless | `lite-api.jup.ag` | none | ~2.2s between calls (≤0.5 RPS) |
//! | Keyed | `api.jup.ag` | `x-api-key` | free wait, or short wait if paid/fast ON |
//!
//! Key: https://developers.jup.ag/portal — optional in settings / `JUPITER_API_KEY`.

use crate::rpc::RpcMeter;
use anyhow::{Context, Result, bail};
use solana_pubkey::Pubkey;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const LITE_PRICE: &str = "https://lite-api.jup.ag/price/v3";
const KEYED_PRICE: &str = "https://api.jup.ag/price/v3";
const LITE_TOKENS: &str = "https://lite-api.jup.ag/tokens/v2/search";
const KEYED_TOKENS: &str = "https://api.jup.ag/tokens/v2/search";

const MIN_INTERVAL_FREE: Duration = Duration::from_millis(2200);
const MIN_INTERVAL_KEYED: Duration = Duration::from_millis(200);

static LAST_CALL: Mutex<Option<Instant>> = Mutex::new(None);

#[derive(Debug, Clone)]
pub struct TokenMeta {
    pub symbol: String,
    pub decimals: Option<u8>,
}

pub fn fetch_usd_prices(
    mints: &[Pubkey],
    api_key: Option<&str>,
    paid_fast: bool,
    meter: Option<&RpcMeter>,
) -> Result<HashMap<Pubkey, f64>> {
    if mints.is_empty() {
        return Ok(HashMap::new());
    }

    let key = api_key.map(str::trim).filter(|s| !s.is_empty());
    let fast = key.is_some() && paid_fast;
    throttle(fast)?;

    let base = if key.is_some() { KEYED_PRICE } else { LITE_PRICE };
    let ids = mints
        .iter()
        .map(|m| m.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let url = format!("{base}?ids={ids}");
    let note = format!(
        "n={}{}",
        mints.len(),
        if key.is_some() { "+key" } else { "+lite" }
    );

    let mut req = http_client()?
        .get(&url)
        .header("Accept", "application/json")
        .header("User-Agent", "sol-clmm-manager/0.1 (unofficial; optional)");
    if let Some(k) = key {
        req = req.header("x-api-key", k);
    }

    let result = (|| -> Result<HashMap<Pubkey, f64>> {
        let resp = req.send().context("jupiter price HTTP")?;
        let status = resp.status();
        if status.as_u16() == 429 {
            bail!("jupiter 429 · slow down or add/upgrade API key in settings");
        }
        if !status.is_success() {
            bail!("jupiter HTTP {status}");
        }

        let value: serde_json::Value = resp.json().context("jupiter JSON")?;
        let mut out = HashMap::new();
        let Some(obj) = value.as_object() else {
            return Ok(out);
        };
        for (k, v) in obj {
            let Ok(mint) = k.parse::<Pubkey>() else {
                continue;
            };
            if let Some(p) = v.get("usdPrice").and_then(|p| p.as_f64()) {
                if p.is_finite() && p > 0.0 {
                    out.insert(mint, p);
                }
            }
        }
        Ok(out)
    })();

    if let Some(m) = meter {
        m.record("jupiter.price", note, result.is_ok());
    }
    result
}

/// Token metadata (symbol / decimals) via Tokens API v2 `search` (batch by mint).
pub fn fetch_token_meta(
    mints: &[Pubkey],
    api_key: Option<&str>,
    paid_fast: bool,
    meter: Option<&RpcMeter>,
) -> Result<HashMap<Pubkey, TokenMeta>> {
    if mints.is_empty() {
        return Ok(HashMap::new());
    }

    let key = api_key.map(str::trim).filter(|s| !s.is_empty());
    let fast = key.is_some() && paid_fast;
    throttle(fast)?;

    let base = if key.is_some() {
        KEYED_TOKENS
    } else {
        LITE_TOKENS
    };
    let ids = mints
        .iter()
        .map(|m| m.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let url = format!("{base}?query={ids}");
    let note = format!(
        "n={}{}",
        mints.len(),
        if key.is_some() { "+key" } else { "+lite" }
    );

    let mut req = http_client()?
        .get(&url)
        .header("Accept", "application/json")
        .header("User-Agent", "sol-clmm-manager/0.1 (unofficial; optional)");
    if let Some(k) = key {
        req = req.header("x-api-key", k);
    }

    let result = (|| -> Result<HashMap<Pubkey, TokenMeta>> {
        let resp = req.send().context("jupiter tokens HTTP")?;
        let status = resp.status();
        if status.as_u16() == 429 {
            bail!("jupiter tokens 429 · slow down or add/upgrade API key");
        }
        if !status.is_success() {
            bail!("jupiter tokens HTTP {status}");
        }
        let value: serde_json::Value = resp.json().context("jupiter tokens JSON")?;
        let mut out = HashMap::new();
        let arr = value.as_array().cloned().unwrap_or_default();
        for v in arr {
            let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
            let Ok(mint) = id.parse::<Pubkey>() else {
                continue;
            };
            let Some(symbol) = v
                .get("symbol")
                .and_then(|x| x.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let decimals = v
                .get("decimals")
                .and_then(|x| x.as_u64())
                .map(|d| d.min(255) as u8);
            out.insert(
                mint,
                TokenMeta {
                    symbol: symbol.to_string(),
                    decimals,
                },
            );
        }
        Ok(out)
    })();

    if let Some(m) = meter {
        m.record("jupiter.tokens", note, result.is_ok());
    }
    result
}

fn http_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .context("reqwest client")
}

fn throttle(fast: bool) -> Result<()> {
    let min = if fast {
        MIN_INTERVAL_KEYED
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
