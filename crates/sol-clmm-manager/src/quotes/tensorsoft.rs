//! TensorSoft data-api — pool board + history (free tier, no auth).
//!
//! Base: `https://api.tensorsoft.tech`
//! - `GET /api/v1/pools/latest`
//! - `GET /api/v1/pools/history`
//!
//! Free tier: ~1 rps / ~2500 calls/mo. Do **not** send `X-API-Key` here.

use crate::rpc::RpcMeter;
use crate::venue::Venue;
use anyhow::{Context, Result, bail};
use solana_pubkey::Pubkey;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const BASE: &str = "https://api.tensorsoft.tech";
const POOLS_LATEST: &str = "/api/v1/pools/latest";
const POOLS_HISTORY: &str = "/api/v1/pools/history";

/// Free-tier pacing (≥1s between calls; slight headroom).
const MIN_INTERVAL: Duration = Duration::from_millis(1100);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

static LAST_CALL: Mutex<Option<Instant>> = Mutex::new(None);

/// Named history window (`Range` in OpenAPI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HistoryRange {
    M15,
    H1,
    H6,
    #[default]
    H24,
    D7,
    D30,
}

impl HistoryRange {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::M15 => "m15",
            Self::H1 => "h1",
            Self::H6 => "h6",
            Self::H24 => "h24",
            Self::D7 => "d7",
            Self::D30 => "d30",
        }
    }
}

/// Series bucket (`Bucket` in OpenAPI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HistoryBucket {
    Raw,
    M1,
    M5,
    M15,
    #[default]
    H1,
    H4,
    D1,
    W1,
}

impl HistoryBucket {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::M1 => "m1",
            Self::M5 => "m5",
            Self::M15 => "m15",
            Self::H1 => "h1",
            Self::H4 => "h4",
            Self::D1 => "d1",
            Self::W1 => "w1",
        }
    }
}

/// Latest DEX/liquidity pool snapshot with APR breakdown.
#[derive(Debug, Clone, Default)]
pub struct PoolMarket {
    pub chain: String,
    pub platform: String,
    pub pool_address: String,
    pub ts: String,
    pub symbol: Option<String>,
    pub token0_symbol: Option<String>,
    pub token1_symbol: Option<String>,
    pub pool_type: Option<String>,
    pub price: Option<f64>,
    pub tvl_usd: Option<f64>,
    pub volume_24h_usd: Option<f64>,
    pub fees_24h_usd: Option<f64>,
    pub fee_bps: Option<f64>,
    pub fee_apr: Option<f64>,
    pub reward_apr: Option<f64>,
    pub apr_total: Option<f64>,
}

/// One history point for a pool.
#[derive(Debug, Clone, Default)]
pub struct PoolPoint {
    pub ts: String,
    pub tvl_usd: Option<f64>,
    pub volume_24h_usd: Option<f64>,
    pub apr_total: Option<f64>,
}

/// Soft market overlay for position detail (API preferred over SQLite volume).
#[derive(Debug, Clone, Default)]
pub struct PoolMarketStats {
    pub tvl_usd: Option<f64>,
    pub volume_24h_usd: Option<f64>,
    pub fees_24h_usd: Option<f64>,
    pub apr_total: Option<f64>,
    pub fee_apr: Option<f64>,
    pub reward_apr: Option<f64>,
    /// e.g. `tensorsoft · history h24`.
    pub source: String,
}

/// Query knobs for `/pools/latest`.
#[derive(Debug, Clone, Default)]
pub struct LatestPoolsQuery<'a> {
    pub window_minutes: Option<i32>,
    pub chain: Option<&'a str>,
    pub platform: Option<&'a str>,
    pub symbol: Option<&'a str>,
    pub min_tvl_usd: Option<f64>,
    pub min_apr: Option<f64>,
    pub sort: Option<&'a str>,
    pub dir: Option<&'a str>,
    pub limit: Option<i64>,
}

/// `GET /api/v1/pools/latest` (unauthenticated free tier).
pub fn fetch_pools_latest(
    q: &LatestPoolsQuery<'_>,
    meter: Option<&RpcMeter>,
) -> Result<Vec<PoolMarket>> {
    throttle()?;
    let mut url = format!("{BASE}{POOLS_LATEST}?");
    let mut first = true;
    let mut push = |k: &str, v: &str| {
        if !first {
            url.push('&');
        }
        first = false;
        url.push_str(k);
        url.push('=');
        url.push_str(&urlencoding_minimal(v));
    };
    if let Some(v) = q.window_minutes {
        push("window_minutes", &v.to_string());
    }
    if let Some(v) = q.chain {
        push("chain", v);
    }
    if let Some(v) = q.platform {
        push("platform", v);
    }
    if let Some(v) = q.symbol {
        push("symbol", v);
    }
    if let Some(v) = q.min_tvl_usd {
        push("min_tvl_usd", &format!("{v}"));
    }
    if let Some(v) = q.min_apr {
        push("min_apr", &format!("{v}"));
    }
    if let Some(v) = q.sort {
        push("sort", v);
    }
    if let Some(v) = q.dir {
        push("dir", v);
    }
    if let Some(v) = q.limit {
        push("limit", &v.to_string());
    }

    let note = format!(
        "{}{}",
        q.platform.unwrap_or("-"),
        q.limit.map(|n| format!(" n={n}")).unwrap_or_default()
    );

    let result = (|| -> Result<Vec<PoolMarket>> {
        let resp = http_client()?
            .get(&url)
            .header("Accept", "application/json")
            .header("User-Agent", "sol-clmm-manager/0.1 (tensorsoft free-tier)")
            .send()
            .context("tensorsoft pools/latest HTTP")?;
        let status = resp.status();
        if status.as_u16() == 429 {
            bail!("tensorsoft 429 · free tier 1 rps / monthly quota");
        }
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            bail!(
                "tensorsoft pools/latest HTTP {status} · {}",
                truncate(&body, 120)
            );
        }
        let value: serde_json::Value = resp.json().context("tensorsoft pools/latest JSON")?;
        Ok(parse_markets(&value))
    })();

    if let Some(m) = meter {
        m.record("tensorsoft.pools.latest", note, result.is_ok());
    }
    result
}

/// `GET /api/v1/pools/history` for one pool.
pub fn fetch_pool_history(
    chain: &str,
    platform: &str,
    pool_address: &str,
    range: HistoryRange,
    bucket: HistoryBucket,
    meter: Option<&RpcMeter>,
) -> Result<Vec<PoolPoint>> {
    throttle()?;
    let url = format!(
        "{BASE}{POOLS_HISTORY}?chain={}&platform={}&pool_address={}&range={}&bucket={}",
        urlencoding_minimal(chain),
        urlencoding_minimal(platform),
        urlencoding_minimal(pool_address),
        range.as_str(),
        bucket.as_str(),
    );
    let short = short_addr(pool_address);
    let note = format!("{platform} {short} {}", range.as_str());

    let result = (|| -> Result<Vec<PoolPoint>> {
        let resp = http_client()?
            .get(&url)
            .header("Accept", "application/json")
            .header("User-Agent", "sol-clmm-manager/0.1 (tensorsoft free-tier)")
            .send()
            .context("tensorsoft pools/history HTTP")?;
        let status = resp.status();
        if status.as_u16() == 429 {
            bail!("tensorsoft 429 · free tier 1 rps / monthly quota");
        }
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            bail!(
                "tensorsoft pools/history HTTP {status} · {}",
                truncate(&body, 120)
            );
        }
        let value: serde_json::Value = resp.json().context("tensorsoft pools/history JSON")?;
        Ok(parse_points(&value))
    })();

    if let Some(m) = meter {
        m.record("tensorsoft.pools.history", note, result.is_ok());
    }
    result
}

/// Soft-fail market stats for detail overlay (history h24 → last point).
pub fn try_pool_market_stats(
    venue: Venue,
    pool: &Pubkey,
    meter: Option<&RpcMeter>,
) -> Option<PoolMarketStats> {
    let platform = venue.data_api_platform();
    let addr = pool.to_string();
    let points = fetch_pool_history(
        "solana",
        platform,
        &addr,
        HistoryRange::H24,
        HistoryBucket::H1,
        meter,
    )
    .ok()?;
    let last = points.last()?;
    let has_any = last.tvl_usd.is_some()
        || last.volume_24h_usd.is_some()
        || last.apr_total.is_some();
    if !has_any {
        return None;
    }
    Some(PoolMarketStats {
        tvl_usd: last.tvl_usd,
        volume_24h_usd: last.volume_24h_usd,
        fees_24h_usd: None,
        apr_total: last.apr_total,
        fee_apr: None,
        reward_apr: None,
        source: format!("tensorsoft · {platform} · history h24"),
    })
}

fn parse_markets(value: &serde_json::Value) -> Vec<PoolMarket> {
    let Some(arr) = value.get("data").and_then(|d| d.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|v| {
            let chain = v.get("chain")?.as_str()?.to_string();
            let platform = v.get("platform")?.as_str()?.to_string();
            let pool_address = v.get("pool_address")?.as_str()?.to_string();
            let ts = v
                .get("ts")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            Some(PoolMarket {
                chain,
                platform,
                pool_address,
                ts,
                symbol: opt_str(v, "symbol"),
                token0_symbol: opt_str(v, "token0_symbol"),
                token1_symbol: opt_str(v, "token1_symbol"),
                pool_type: opt_str(v, "pool_type"),
                price: opt_f64(v, "price"),
                tvl_usd: opt_f64(v, "tvl_usd"),
                volume_24h_usd: opt_f64(v, "volume_24h_usd"),
                fees_24h_usd: opt_f64(v, "fees_24h_usd"),
                fee_bps: opt_f64(v, "fee_bps"),
                fee_apr: opt_f64(v, "fee_apr"),
                reward_apr: opt_f64(v, "reward_apr"),
                apr_total: opt_f64(v, "apr_total"),
            })
        })
        .collect()
}

fn parse_points(value: &serde_json::Value) -> Vec<PoolPoint> {
    let Some(arr) = value.get("data").and_then(|d| d.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|v| {
            let ts = v.get("ts")?.as_str()?.to_string();
            Some(PoolPoint {
                ts,
                tvl_usd: opt_f64(v, "tvl_usd"),
                volume_24h_usd: opt_f64(v, "volume_24h_usd"),
                apr_total: opt_f64(v, "apr_total"),
            })
        })
        .collect()
}

fn opt_str(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn opt_f64(v: &serde_json::Value, key: &str) -> Option<f64> {
    v.get(key).and_then(|x| x.as_f64()).filter(|n| n.is_finite())
}

fn http_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .context("reqwest client")
}

fn throttle() -> Result<()> {
    let mut guard = LAST_CALL.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(prev) = *guard {
        let elapsed = prev.elapsed();
        if elapsed < MIN_INTERVAL {
            std::thread::sleep(MIN_INTERVAL - elapsed);
        }
    }
    *guard = Some(Instant::now());
    Ok(())
}

fn urlencoding_minimal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn short_addr(addr: &str) -> String {
    if addr.len() <= 8 {
        addr.to_string()
    } else {
        format!("{}…{}", &addr[..4], &addr[addr.len() - 4..])
    }
}

fn truncate(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.len() <= max {
        t.to_string()
    } else {
        format!("{}…", &t[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_markets_envelope() {
        let v: serde_json::Value = serde_json::json!({
            "data": [{
                "chain": "solana",
                "platform": "orca",
                "pool_address": "7qbRF6YsyGuLUVs6Y1q64bdVrfe4ZcUUz1JRdoVNUJnm",
                "ts": "2026-01-01T00:00:00Z",
                "tvl_usd": 1.5e6,
                "volume_24h_usd": 2.0e5,
                "apr_total": 12.5,
                "fee_apr": 10.0,
                "reward_apr": 2.5
            }],
            "meta": { "cached": false, "generated_at": "2026-01-01T00:00:00Z" }
        });
        let rows = parse_markets(&v);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].platform, "orca");
        assert_eq!(rows[0].tvl_usd, Some(1.5e6));
        assert_eq!(rows[0].apr_total, Some(12.5));
    }

}
