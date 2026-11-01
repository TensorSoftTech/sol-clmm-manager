//! Pool snapshots in SQLite → period fee / volume estimates from deltas.
//!
//! Volume uses the **full** retained snapshot chain (≤64 points): sum of
//! per-segment Δfee_growth × L / fee_rate, not just last→current.

use crate::fees::estimate_volume_from_fees;
use crate::venue::Venue;
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use solana_pubkey::Pubkey;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct PoolSnapshotInput {
    pub venue: Venue,
    pub pool: Pubkey,
    pub slot: u64,
    pub fee_growth_global_a: u128,
    pub fee_growth_global_b: u128,
    pub protocol_fee_owed_a: u64,
    pub protocol_fee_owed_b: u64,
    pub fee_rate: u32,
    pub liquidity: u128,
    pub sqrt_price: u128,
    pub tick_current: i32,
    pub vault_a: u64,
    pub vault_b: u64,
    pub price_a_usd: Option<f64>,
    pub price_b_usd: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct PoolSnapshotRow {
    pub observed_at: i64,
    pub slot: u64,
    pub fee_growth_global_a: u128,
    pub fee_growth_global_b: u128,
    pub protocol_fee_owed_a: u64,
    pub protocol_fee_owed_b: u64,
    pub fee_rate: u32,
    pub liquidity: u128,
    pub vault_a: u64,
    pub vault_b: u64,
    pub price_a_usd: Option<f64>,
    pub price_b_usd: Option<f64>,
}

/// Period report over the retained snapshot chain (oldest → newest).
#[derive(Debug, Clone, Default)]
pub struct PoolPeriodReport {
    pub dt_secs: u64,
    pub prev_observed_at: Option<i64>,
    /// Number of snapshots in the chain used for the sum (≥2 when report is Some).
    pub snapshot_count: usize,
    /// Number of consecutive pairs that contributed (growth / fee path).
    pub segment_count: usize,
    /// Estimated LP fees accrued on active liquidity over the window (token A raw).
    pub fees_a_raw: u128,
    pub fees_b_raw: u128,
    pub fees_a_ui: String,
    pub fees_b_ui: String,
    pub fees_a_usd: Option<f64>,
    pub fees_b_usd: Option<f64>,
    pub fees_usd: Option<f64>,
    /// Rough notional volume ≈ Σ (segment_fees_usd / fee_fraction).
    pub volume_usd: Option<f64>,
    /// Extrapolated 24h volume from the observed window rate (when dt > 0).
    pub volume_usd_24h: Option<f64>,
    pub volume_note: String,
}

pub fn migrate_snapshots(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pool_snapshots (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            venue TEXT NOT NULL,
            pool TEXT NOT NULL,
            observed_at INTEGER NOT NULL,
            slot INTEGER NOT NULL,
            fee_growth_global_a TEXT NOT NULL,
            fee_growth_global_b TEXT NOT NULL,
            protocol_fee_owed_a INTEGER NOT NULL,
            protocol_fee_owed_b INTEGER NOT NULL,
            fee_rate INTEGER NOT NULL,
            liquidity TEXT NOT NULL,
            sqrt_price TEXT NOT NULL,
            tick_current INTEGER NOT NULL,
            vault_a INTEGER NOT NULL,
            vault_b INTEGER NOT NULL,
            price_a_usd REAL,
            price_b_usd REAL
        );
        CREATE INDEX IF NOT EXISTS idx_pool_snapshots_pool_time
            ON pool_snapshots (venue, pool, observed_at DESC);
        ",
    )
    .context("migrate pool_snapshots")?;
    Ok(())
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn row_from_query(
    observed_at: i64,
    slot: i64,
    fg_a: String,
    fg_b: String,
    proto_a: i64,
    proto_b: i64,
    fee_rate: i64,
    liquidity: String,
    vault_a: i64,
    vault_b: i64,
    price_a: Option<f64>,
    price_b: Option<f64>,
) -> PoolSnapshotRow {
    PoolSnapshotRow {
        observed_at,
        slot: slot as u64,
        fee_growth_global_a: parse_u128(&fg_a),
        fee_growth_global_b: parse_u128(&fg_b),
        protocol_fee_owed_a: proto_a as u64,
        protocol_fee_owed_b: proto_b as u64,
        fee_rate: fee_rate as u32,
        liquidity: parse_u128(&liquidity),
        vault_a: vault_a as u64,
        vault_b: vault_b as u64,
        price_a_usd: price_a,
        price_b_usd: price_b,
    }
}

pub fn latest_snapshot(
    conn: &Connection,
    venue: Venue,
    pool: &Pubkey,
) -> Result<Option<PoolSnapshotRow>> {
    conn.query_row(
        "SELECT observed_at, slot, fee_growth_global_a, fee_growth_global_b,
                protocol_fee_owed_a, protocol_fee_owed_b, fee_rate, liquidity,
                vault_a, vault_b, price_a_usd, price_b_usd
         FROM pool_snapshots
         WHERE venue = ?1 AND pool = ?2
         ORDER BY observed_at DESC, id DESC
         LIMIT 1",
        params![venue.short(), pool.to_string()],
        |row| {
            Ok(row_from_query(
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
                row.get(10)?,
                row.get(11)?,
            ))
        },
    )
    .optional()
    .context("latest pool snapshot")
}

/// Oldest → newest, up to `limit` retained rows for this pool.
pub fn list_snapshots(
    conn: &Connection,
    venue: Venue,
    pool: &Pubkey,
    limit: usize,
) -> Result<Vec<PoolSnapshotRow>> {
    let lim = limit.max(1) as i64;
    let mut stmt = conn
        .prepare(
            "SELECT observed_at, slot, fee_growth_global_a, fee_growth_global_b,
                    protocol_fee_owed_a, protocol_fee_owed_b, fee_rate, liquidity,
                    vault_a, vault_b, price_a_usd, price_b_usd
             FROM (
                SELECT observed_at, slot, fee_growth_global_a, fee_growth_global_b,
                       protocol_fee_owed_a, protocol_fee_owed_b, fee_rate, liquidity,
                       vault_a, vault_b, price_a_usd, price_b_usd, id
                FROM pool_snapshots
                WHERE venue = ?1 AND pool = ?2
                ORDER BY observed_at DESC, id DESC
                LIMIT ?3
             )
             ORDER BY observed_at ASC, id ASC",
        )
        .context("prepare list snapshots")?;
    let rows = stmt
        .query_map(params![venue.short(), pool.to_string(), lim], |row| {
            Ok(row_from_query(
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
                row.get(10)?,
                row.get(11)?,
            ))
        })
        .context("query list snapshots")?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.context("snapshot row")?);
    }
    Ok(out)
}

pub fn insert_snapshot(conn: &Connection, snap: &PoolSnapshotInput) -> Result<()> {
    let observed_at = now_secs();
    conn.execute(
        "INSERT INTO pool_snapshots (
            venue, pool, observed_at, slot,
            fee_growth_global_a, fee_growth_global_b,
            protocol_fee_owed_a, protocol_fee_owed_b, fee_rate,
            liquidity, sqrt_price, tick_current,
            vault_a, vault_b, price_a_usd, price_b_usd
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
        params![
            snap.venue.short(),
            snap.pool.to_string(),
            observed_at,
            snap.slot as i64,
            snap.fee_growth_global_a.to_string(),
            snap.fee_growth_global_b.to_string(),
            snap.protocol_fee_owed_a as i64,
            snap.protocol_fee_owed_b as i64,
            snap.fee_rate as i64,
            snap.liquidity.to_string(),
            snap.sqrt_price.to_string(),
            snap.tick_current,
            snap.vault_a as i64,
            snap.vault_b as i64,
            snap.price_a_usd,
            snap.price_b_usd,
        ],
    )
    .context("insert pool snapshot")?;

    // Keep last ~64 snapshots per pool.
    conn.execute(
        "DELETE FROM pool_snapshots WHERE id IN (
            SELECT id FROM pool_snapshots
            WHERE venue = ?1 AND pool = ?2
            ORDER BY observed_at DESC, id DESC
            LIMIT -1 OFFSET 64
         )",
        params![snap.venue.short(), snap.pool.to_string()],
    )
    .ok();
    Ok(())
}

fn input_as_row(snap: &PoolSnapshotInput, observed_at: i64) -> PoolSnapshotRow {
    PoolSnapshotRow {
        observed_at,
        slot: snap.slot,
        fee_growth_global_a: snap.fee_growth_global_a,
        fee_growth_global_b: snap.fee_growth_global_b,
        protocol_fee_owed_a: snap.protocol_fee_owed_a,
        protocol_fee_owed_b: snap.protocol_fee_owed_b,
        fee_rate: snap.fee_rate,
        liquidity: snap.liquidity,
        vault_a: snap.vault_a,
        vault_b: snap.vault_b,
        price_a_usd: snap.price_a_usd,
        price_b_usd: snap.price_b_usd,
    }
}

fn avg_price(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(x), Some(y)) if x.is_finite() && y.is_finite() => Some((x + y) / 2.0),
        (Some(x), _) if x.is_finite() => Some(x),
        (_, Some(y)) if y.is_finite() => Some(y),
        _ => None,
    }
}

fn raw_ui(raw: u128, dec: u8) -> f64 {
    if dec == 0 {
        raw as f64
    } else {
        raw as f64 / 10f64.powi(dec as i32)
    }
}

fn format_dt(secs: u64) -> String {
    if secs < 60 {
        format!("~{secs}s")
    } else if secs < 3600 {
        format!("~{}m", secs / 60)
    } else if secs < 86400 {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if m == 0 {
            format!("~{h}h")
        } else {
            format!("~{h}h{m}m")
        }
    } else {
        let d = secs / 86400;
        let h = (secs % 86400) / 3600;
        if h == 0 {
            format!("~{d}d")
        } else {
            format!("~{d}d{h}h")
        }
    }
}

/// Sum fees / volume across every consecutive pair in `points` (oldest → newest).
///
/// Per segment: fees ≈ Δfee_growth × mean(L_a, L_b); volume ≈ fees_usd / fee_rate.
pub fn period_report_from_chain(
    points: &[PoolSnapshotRow],
    decimals_a: u8,
    decimals_b: u8,
    symbol_a: &str,
    symbol_b: &str,
) -> Option<PoolPeriodReport> {
    if points.len() < 2 {
        return None;
    }

    let first = points.first()?;
    let last = points.last()?;
    let dt = last.observed_at.saturating_sub(first.observed_at).max(0) as u64;

    let mut fees_a_raw: u128 = 0;
    let mut fees_b_raw: u128 = 0;
    let mut fees_a_usd_sum = 0.0;
    let mut fees_b_usd_sum = 0.0;
    let mut any_a_usd = false;
    let mut any_b_usd = false;
    let mut volume_usd_sum = 0.0;
    let mut any_volume = false;
    let mut segment_count = 0usize;

    for w in points.windows(2) {
        let a = &w[0];
        let b = &w[1];
        // Skip non-advancing time (duplicate refresh) — still allow growth if any.
        let growth_a = b.fee_growth_global_a.wrapping_sub(a.fee_growth_global_a);
        let growth_b = b.fee_growth_global_b.wrapping_sub(a.fee_growth_global_b);
        if growth_a == 0 && growth_b == 0 {
            continue;
        }

        // Trapezoid L over the segment (better than only last L when L moves).
        let l = a.liquidity / 2 + b.liquidity / 2;
        let l = l.max(1);
        let seg_a = crate::fees::mul_shift_right_64(growth_a, l);
        let seg_b = crate::fees::mul_shift_right_64(growth_b, l);
        fees_a_raw = fees_a_raw.saturating_add(seg_a);
        fees_b_raw = fees_b_raw.saturating_add(seg_b);

        let px_a = avg_price(a.price_a_usd, b.price_a_usd);
        let px_b = avg_price(a.price_b_usd, b.price_b_usd);
        let seg_a_usd = px_a.map(|p| raw_ui(seg_a, decimals_a) * p);
        let seg_b_usd = px_b.map(|p| raw_ui(seg_b, decimals_b) * p);
        if let Some(u) = seg_a_usd {
            fees_a_usd_sum += u;
            any_a_usd = true;
        }
        if let Some(u) = seg_b_usd {
            fees_b_usd_sum += u;
            any_b_usd = true;
        }

        let seg_fees_usd = match (seg_a_usd, seg_b_usd) {
            (Some(x), Some(y)) => Some(x + y),
            (Some(x), None) => Some(x),
            (None, Some(y)) => Some(y),
            (None, None) => None,
        };
        let fee_rate = if b.fee_rate != 0 {
            b.fee_rate
        } else {
            a.fee_rate
        };
        if let Some(f) = seg_fees_usd {
            if let Some(v) = estimate_volume_from_fees(f, fee_rate) {
                volume_usd_sum += v;
                any_volume = true;
            }
        }
        segment_count += 1;
    }

    let fees_a_usd = if any_a_usd {
        Some(fees_a_usd_sum)
    } else {
        None
    };
    let fees_b_usd = if any_b_usd {
        Some(fees_b_usd_sum)
    } else {
        None
    };
    let fees_usd = match (fees_a_usd, fees_b_usd) {
        (Some(a), Some(b)) => Some(a + b),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };
    let volume_usd = if any_volume {
        Some(volume_usd_sum)
    } else {
        None
    };
    let volume_usd_24h = volume_usd.and_then(|v| {
        if dt == 0 || !v.is_finite() || v <= 0.0 {
            None
        } else {
            let rate = v * 86400.0 / dt as f64;
            if rate.is_finite() && rate > 0.0 {
                Some(rate)
            } else {
                None
            }
        }
    });

    let n = points.len();
    let window = format_dt(dt);
    let volume_note = if segment_count == 0 {
        format!("{n} snaps · {window} · no fee growth yet")
    } else if volume_usd.is_some() {
        format!("{n} snaps · {segment_count} segs · {window} · Σ Δgrowth/fee_rate · rough")
    } else if fees_a_raw == 0 && fees_b_raw == 0 {
        format!("{n} snaps · {window} · no fee growth yet")
    } else {
        format!("{n} snaps · {segment_count} segs · {window} · need USD prices for volume")
    };

    Some(PoolPeriodReport {
        dt_secs: dt,
        prev_observed_at: Some(first.observed_at),
        snapshot_count: n,
        segment_count,
        fees_a_raw,
        fees_b_raw,
        fees_a_ui: crate::fees::format_amount_usd(fees_a_raw, decimals_a, symbol_a, fees_a_usd),
        fees_b_ui: crate::fees::format_amount_usd(fees_b_raw, decimals_b, symbol_b, fees_b_usd),
        fees_a_usd,
        fees_b_usd,
        fees_usd,
        volume_usd,
        volume_usd_24h,
        volume_note,
    })
}

/// Compare `prev` → `next` only (legacy two-point helper / tests).
pub fn period_report_from_delta(
    prev: &PoolSnapshotRow,
    next: &PoolSnapshotInput,
    decimals_a: u8,
    decimals_b: u8,
    symbol_a: &str,
    symbol_b: &str,
) -> PoolPeriodReport {
    let tip = input_as_row(next, now_secs());
    period_report_from_chain(&[prev.clone(), tip], decimals_a, decimals_b, symbol_a, symbol_b)
        .unwrap_or_default()
}

/// Record snapshot and return period report over the full chain including this tip.
pub fn record_pool_snapshot(
    conn: &Connection,
    snap: &PoolSnapshotInput,
    decimals_a: u8,
    decimals_b: u8,
    symbol_a: &str,
    symbol_b: &str,
) -> Result<Option<PoolPeriodReport>> {
    let mut points = list_snapshots(conn, snap.venue, &snap.pool, 64)?;
    points.push(input_as_row(snap, now_secs()));
    let report = period_report_from_chain(&points, decimals_a, decimals_b, symbol_a, symbol_b);
    insert_snapshot(conn, snap)?;
    Ok(report)
}

fn parse_u128(s: &str) -> u128 {
    s.parse().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::venue::Venue;
    use solana_pubkey::Pubkey;

    fn point(
        t: i64,
        fg_a: u128,
        fg_b: u128,
        l: u128,
        fee_rate: u32,
        pa: f64,
        pb: f64,
    ) -> PoolSnapshotRow {
        PoolSnapshotRow {
            observed_at: t,
            slot: t as u64,
            fee_growth_global_a: fg_a,
            fee_growth_global_b: fg_b,
            protocol_fee_owed_a: 0,
            protocol_fee_owed_b: 0,
            fee_rate,
            liquidity: l,
            vault_a: 0,
            vault_b: 0,
            price_a_usd: Some(pa),
            price_b_usd: Some(pb),
        }
    }

    #[test]
    fn chain_sums_segments_not_just_endpoints() {
        // Three points: growth happens in both segments.
        // L=1, growth_a += 100<<64 each segment → 100 tokens each → 200 total.
        let g = |n: u128| n << 64;
        let pts = vec![
            point(0, g(0), 0, 1, 3000, 1.0, 1.0),
            point(100, g(100), 0, 1, 3000, 1.0, 1.0),
            point(200, g(200), 0, 1, 3000, 1.0, 1.0),
        ];
        let r = period_report_from_chain(&pts, 0, 0, "A", "B").unwrap();
        assert_eq!(r.snapshot_count, 3);
        assert_eq!(r.segment_count, 2);
        assert_eq!(r.fees_a_raw, 200);
        // fees $200 / 0.003 = $66666.66…
        let v = r.volume_usd.unwrap();
        assert!((v - (200.0 / 0.003)).abs() < 1e-3);
        // 24h extrapolate: window 200s
        let v24 = r.volume_usd_24h.unwrap();
        assert!(v24 > v);
    }

    #[test]
    fn two_points_still_works() {
        let g = |n: u128| n << 64;
        let pts = vec![
            point(0, g(0), 0, 1, 3000, 1.0, 1.0),
            point(60, g(30), 0, 1, 3000, 1.0, 1.0),
        ];
        let r = period_report_from_chain(&pts, 0, 0, "A", "B").unwrap();
        assert_eq!(r.fees_a_raw, 30);
        assert_eq!(r.segment_count, 1);
    }

    #[test]
    fn single_point_is_none() {
        let pts = vec![point(0, 0, 0, 1, 3000, 1.0, 1.0)];
        assert!(period_report_from_chain(&pts, 0, 0, "A", "B").is_none());
    }

    #[test]
    fn sqlite_chain_roundtrip() {
        let conn = Connection::open_in_memory().unwrap();
        migrate_snapshots(&conn).unwrap();
        let pool = Pubkey::new_from_array([7; 32]);
        let g = |n: u128| n << 64;
        for (t, n) in [(10i64, 0u128), (20, 50), (30, 120)] {
            conn.execute(
                "INSERT INTO pool_snapshots (
                    venue, pool, observed_at, slot,
                    fee_growth_global_a, fee_growth_global_b,
                    protocol_fee_owed_a, protocol_fee_owed_b, fee_rate,
                    liquidity, sqrt_price, tick_current,
                    vault_a, vault_b, price_a_usd, price_b_usd
                 ) VALUES (?1,?2,?3,?4,?5,?6,0,0,3000,'1','0',0,0,0,1.0,1.0)",
                params![
                    Venue::OrcaWhirlpool.short(),
                    pool.to_string(),
                    t,
                    t,
                    g(n).to_string(),
                    "0",
                ],
            )
            .unwrap();
        }
        let pts = list_snapshots(&conn, Venue::OrcaWhirlpool, &pool, 64).unwrap();
        assert_eq!(pts.len(), 3);
        let r = period_report_from_chain(&pts, 0, 0, "A", "B").unwrap();
        assert_eq!(r.fees_a_raw, 120);
        assert_eq!(r.segment_count, 2);
    }
}
