//! Local SQLite config + token metadata cache.
//!
//! Default DB: `~/.config/sol-clmm-manager/sol-clmm.db`
//! Active-path pointer: `~/.config/sol-clmm-manager/active_db`

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const APP_DIR_NAME: &str = "sol-clmm-manager";
pub const DEFAULT_DB_FILE: &str = "sol-clmm.db";
pub const ACTIVE_POINTER_FILE: &str = "active_db";

const KEY_WALLET: &str = "wallet_pubkey";
/// Filesystem path only — never keypair secret bytes.
const KEY_KEYPAIR: &str = "keypair_path";
const KEY_RPC: &str = "rpc_url";
const KEY_PRIVACY: &str = "privacy_trim";
const KEY_PRICE_MODE: &str = "price_mode";
const KEY_JUPITER_API: &str = "jupiter_api_key";
const KEY_TITAN_API: &str = "titan_api_key";
const KEY_API_PAID_FAST: &str = "api_paid_fast";
const KEY_WRITE_ALLOWED: &str = "write_allowed";
const KEY_WRITE_PROMPT_SKIP: &str = "write_prompt_skip";
const KEY_SWAP_MODE: &str = "swap_mode";
const KEY_BUDGET_MODE: &str = "tx_budget_mode";
const KEY_CU_LIMIT: &str = "tx_cu_limit";
const KEY_TIP_LAMPORTS: &str = "tx_tip_lamports";
const KEY_LOADED_DATA: &str = "tx_loaded_data";
const KEY_HEAP: &str = "tx_heap";
const KEY_CU_MARGIN_BPS: &str = "tx_cu_margin_bps";
const KEY_TIP_FLOOR: &str = "tx_tip_floor";
const KEY_TIP_CEILING: &str = "tx_tip_ceiling";
const KEY_DLMM_STRATEGY: &str = "dlmm_strategy";

#[derive(Debug, Clone, Default)]
pub struct AppSettings {
    pub wallet_pubkey: Option<String>,
    /// Path to JSON keypair file (not the secret).
    pub keypair_path: Option<String>,
    pub rpc_url: Option<String>,
    pub privacy_trim: bool,
    /// `onchain` | `jupiter` | `titan`
    pub price_mode: Option<String>,
    pub jupiter_api_key: Option<String>,
    pub titan_api_key: Option<String>,
    /// Use short throttle when API keys are set (paid / higher plan).
    pub api_paid_fast: bool,
    /// `None` = never asked; `Some(true/false)` = consent.
    pub write_allowed: Option<bool>,
    /// Skip the “allow write access?” prompt on startup.
    pub write_prompt_skip: bool,
    /// `jupiter` | `titan`
    pub swap_mode: Option<String>,
    /// `auto` | `manual` for tx v1 budget.
    pub tx_budget_mode: Option<String>,
    pub tx_cu_limit: Option<u32>,
    pub tx_tip_lamports: Option<u64>,
    pub tx_loaded_data: Option<u32>,
    pub tx_heap: Option<u32>,
    pub tx_cu_margin_bps: Option<u32>,
    pub tx_tip_floor: Option<u64>,
    pub tx_tip_ceiling: Option<u64>,
    /// Last DLMM deposit strategy short name.
    pub dlmm_strategy: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CachedToken {
    pub mint: String,
    pub symbol: String,
    pub decimals: u8,
}

/// Pool identity stored with every landed write (even when Δ fetch fails).
#[derive(Debug, Clone)]
pub struct WritePoolMeta {
    /// e.g. `SOL / USDC`
    pub pool_label: String,
    pub mint_a: String,
    pub mint_b: String,
    pub symbol_a: String,
    pub symbol_b: String,
    pub decimals_a: u8,
    pub decimals_b: u8,
}

/// Pool-mint ATA deltas recorded with a landed write (after − before).
#[derive(Debug, Clone)]
pub struct WriteTokenDelta {
    pub mint_a: String,
    pub mint_b: String,
    pub symbol_a: String,
    pub symbol_b: String,
    pub decimals_a: u8,
    pub decimals_b: u8,
    pub delta_a: i64,
    pub delta_b: i64,
}

/// One landed write from [`Store::record_write_tx`] / history list.
#[derive(Debug, Clone)]
pub struct WriteTxRow {
    pub id: u64,
    /// Unix seconds as decimal string (legacy storage).
    pub created_at: String,
    pub signature: String,
    pub op: String,
    pub venue: Option<String>,
    pub position: Option<String>,
    pub pool: Option<String>,
    /// Human pool pair (`SOL / USDC`).
    pub pool_label: Option<String>,
    pub tip_lamports: Option<u64>,
    pub fee_lamports: Option<u64>,
    pub sol_spent_lamports: Option<u64>,
    pub summary: Option<String>,
    pub paid_label: Option<String>,
    /// Pool mint A (base58) when recorded.
    pub mint_a: Option<String>,
    pub mint_b: Option<String>,
    pub symbol_a: Option<String>,
    pub symbol_b: Option<String>,
    pub decimals_a: Option<u8>,
    pub decimals_b: Option<u8>,
    /// ATA raw Δ (after − before); negative = spent.
    pub delta_a: Option<i64>,
    pub delta_b: Option<i64>,
}

impl WriteTxRow {
    /// Parsed unix seconds, if `created_at` is numeric.
    pub fn created_unix(&self) -> Option<u64> {
        self.created_at.parse().ok()
    }

    /// UTC stamp with date + hour (`YYYY-MM-DD HH:MM`).
    pub fn time_label(&self) -> String {
        let Some(secs) = self.created_unix() else {
            return "—".into();
        };
        // Civil date from unix days (proleptic Gregorian) — no chrono dep.
        let days = (secs / 86_400) as i64;
        let tod = secs % 86_400;
        let h = tod / 3600;
        let m = (tod % 3600) / 60;
        let (y, mo, d) = unix_days_to_ymd(days);
        format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}")
    }

    /// Pair label for the table (`SOL/USDC`).
    pub fn pair_label(&self) -> String {
        if let Some(lbl) = self.pool_label.as_deref().filter(|s| !s.is_empty()) {
            return lbl.replace(" / ", "/");
        }
        match (self.symbol_a.as_deref(), self.symbol_b.as_deref()) {
            (Some(a), Some(b)) => format!("{a}/{b}"),
            _ => self
                .pool
                .as_deref()
                .map(Self::short_pk)
                .unwrap_or_else(|| "—".into()),
        }
    }

    pub fn short_sig(&self) -> String {
        let s = self.signature.as_str();
        if s.len() <= 12 {
            s.to_string()
        } else {
            format!("{}…{}", &s[..6], &s[s.len().saturating_sub(4)..])
        }
    }

    pub fn short_pk(pk: &str) -> String {
        if pk.len() <= 10 {
            pk.to_string()
        } else {
            format!("{}…{}", &pk[..4], &pk[pk.len().saturating_sub(4)..])
        }
    }

    /// `SOL −0.0012` (spent shown as minus).
    pub fn sol_delta_label(&self) -> String {
        match self.sol_spent_lamports {
            Some(0) => "SOL 0".into(),
            Some(l) => format!("SOL −{}", crate::format_amount(u128::from(l), 9)),
            None => "SOL —".into(),
        }
    }

    /// `USDC −1.2 · SOL +0.05` (zero legs omitted).
    pub fn token_delta_label(&self) -> String {
        let a = self.format_token_delta(true);
        let b = self.format_token_delta(false);
        match (a, b) {
            (None, None) => "—".into(),
            (Some(a), None) | (None, Some(a)) => a,
            (Some(a), Some(b)) => format!("{a} · {b}"),
        }
    }

    /// One-line Δ for the history table (tokens preferred; else native SOL spend).
    pub fn deltas_label(&self) -> String {
        let tokens = self.token_delta_label();
        if tokens != "—" {
            // If neither leg is SOL/WSOL, prepend native spend when known.
            let has_sol_leg = self
                .symbol_a
                .as_deref()
                .is_some_and(|s| s == "SOL" || s == "WSOL")
                || self
                    .symbol_b
                    .as_deref()
                    .is_some_and(|s| s == "SOL" || s == "WSOL");
            if has_sol_leg {
                tokens
            } else {
                match self.sol_spent_lamports {
                    Some(l) if l > 0 => format!("{} · {}", self.sol_delta_label(), tokens),
                    _ => tokens,
                }
            }
        } else {
            self.sol_delta_label()
        }
    }

    fn format_token_delta(&self, side_a: bool) -> Option<String> {
        let (sym, dec, delta) = if side_a {
            (
                self.symbol_a.as_deref()?,
                self.decimals_a.unwrap_or(0),
                self.delta_a?,
            )
        } else {
            (
                self.symbol_b.as_deref()?,
                self.decimals_b.unwrap_or(0),
                self.delta_b?,
            )
        };
        if delta == 0 {
            return None;
        }
        let abs = crate::format_amount(u128::from(delta.unsigned_abs()), dec);
        if delta < 0 {
            Some(format!("{sym} −{abs}"))
        } else {
            Some(format!("{sym} +{abs}"))
        }
    }
}

/// Add token-Δ / pool_label columns to older DBs (ignore if already present).
fn migrate_write_txs_token_deltas(conn: &Connection) -> Result<()> {
    for sql in [
        "ALTER TABLE write_txs ADD COLUMN mint_a TEXT",
        "ALTER TABLE write_txs ADD COLUMN mint_b TEXT",
        "ALTER TABLE write_txs ADD COLUMN symbol_a TEXT",
        "ALTER TABLE write_txs ADD COLUMN symbol_b TEXT",
        "ALTER TABLE write_txs ADD COLUMN decimals_a INTEGER",
        "ALTER TABLE write_txs ADD COLUMN decimals_b INTEGER",
        "ALTER TABLE write_txs ADD COLUMN delta_a INTEGER",
        "ALTER TABLE write_txs ADD COLUMN delta_b INTEGER",
        "ALTER TABLE write_txs ADD COLUMN pool_label TEXT",
    ] {
        let _ = conn.execute(sql, []);
    }
    Ok(())
}

/// Civil `(year, month, day)` from days since Unix epoch (UTC).
fn unix_days_to_ymd(days: i64) -> (i32, u32, u32) {
    // Howard Hinnant / civil_from_days algorithm.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64 + era * 400) as i32;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

pub struct Store {
    conn: Connection,
    path: PathBuf,
}

impl Store {
    pub fn open(path: &Path, overwrite: bool) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }

        if overwrite && path.exists() {
            fs::remove_file(path)
                .with_context(|| format!("overwrite remove {}", path.display()))?;
        }

        let conn =
            Connection::open(path).with_context(|| format!("open sqlite {}", path.display()))?;
        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS tokens (
                mint       TEXT PRIMARY KEY NOT NULL,
                symbol     TEXT NOT NULL,
                decimals   INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS write_txs (
                id                 INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at         TEXT NOT NULL,
                signature          TEXT NOT NULL UNIQUE,
                op                 TEXT NOT NULL,
                venue              TEXT,
                position           TEXT,
                pool               TEXT,
                tip_lamports       INTEGER,
                fee_lamports       INTEGER,
                sol_spent_lamports INTEGER,
                summary            TEXT,
                paid_label         TEXT
            );
            ",
        )
        .context("migrate schema")?;

        crate::snapshots::migrate_snapshots(&conn)?;
        migrate_write_txs_token_deltas(&conn)?;

        let store = Self {
            conn,
            path: path.to_path_buf(),
        };
        write_active_pointer(path)?;
        Ok(store)
    }

    /// Persist a pool snapshot and return fee/volume over the full retained chain.
    pub fn record_pool_snapshot(
        &self,
        snap: &crate::snapshots::PoolSnapshotInput,
        decimals_a: u8,
        decimals_b: u8,
        symbol_a: &str,
        symbol_b: &str,
    ) -> Result<Option<crate::snapshots::PoolPeriodReport>> {
        crate::snapshots::record_pool_snapshot(
            &self.conn, snap, decimals_a, decimals_b, symbol_a, symbol_b,
        )
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// On-disk size of the SQLite file (bytes), if the path exists.
    pub fn file_size_bytes(&self) -> Option<u64> {
        fs::metadata(&self.path).ok().map(|m| m.len())
    }

    /// Compact size for the TUI footer (`12K`, `1.2M`, …).
    pub fn file_size_label(&self) -> String {
        match self.file_size_bytes() {
            Some(n) => format_db_size(n),
            None => "—".into(),
        }
    }

    pub fn load_settings(&self) -> Result<AppSettings> {
        Ok(AppSettings {
            wallet_pubkey: self.get_setting(KEY_WALLET)?,
            keypair_path: self.get_setting(KEY_KEYPAIR)?,
            rpc_url: self.get_setting(KEY_RPC)?,
            privacy_trim: self
                .get_setting(KEY_PRIVACY)?
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            price_mode: self.get_setting(KEY_PRICE_MODE)?,
            jupiter_api_key: self.get_setting(KEY_JUPITER_API)?,
            titan_api_key: self.get_setting(KEY_TITAN_API)?,
            api_paid_fast: self
                .get_setting(KEY_API_PAID_FAST)?
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            write_allowed: self
                .get_setting(KEY_WRITE_ALLOWED)?
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true")),
            write_prompt_skip: self
                .get_setting(KEY_WRITE_PROMPT_SKIP)?
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            swap_mode: self.get_setting(KEY_SWAP_MODE)?,
            tx_budget_mode: self.get_setting(KEY_BUDGET_MODE)?,
            tx_cu_limit: self.get_setting(KEY_CU_LIMIT)?.and_then(|s| s.parse().ok()),
            tx_tip_lamports: self
                .get_setting(KEY_TIP_LAMPORTS)?
                .and_then(|s| s.parse().ok()),
            tx_loaded_data: self
                .get_setting(KEY_LOADED_DATA)?
                .and_then(|s| s.parse().ok()),
            tx_heap: self.get_setting(KEY_HEAP)?.and_then(|s| s.parse().ok()),
            tx_cu_margin_bps: self
                .get_setting(KEY_CU_MARGIN_BPS)?
                .and_then(|s| s.parse().ok()),
            tx_tip_floor: self
                .get_setting(KEY_TIP_FLOOR)?
                .and_then(|s| s.parse().ok()),
            tx_tip_ceiling: self
                .get_setting(KEY_TIP_CEILING)?
                .and_then(|s| s.parse().ok()),
            dlmm_strategy: self.get_setting(KEY_DLMM_STRATEGY)?,
        })
    }

    pub fn set_wallet_pubkey(&self, pubkey: &str) -> Result<()> {
        self.set_setting(KEY_WALLET, pubkey)
    }

    pub fn clear_wallet(&self) -> Result<()> {
        self.delete_setting(KEY_WALLET)?;
        self.delete_setting(KEY_KEYPAIR)?;
        Ok(())
    }

    /// Persist keypair **file path** only — never secret key material.
    pub fn set_keypair_path(&self, path: &str) -> Result<()> {
        self.set_setting(KEY_KEYPAIR, path)
    }

    pub fn set_rpc_url(&self, url: &str) -> Result<()> {
        self.set_setting(KEY_RPC, url)
    }

    pub fn set_privacy_trim(&self, on: bool) -> Result<()> {
        self.set_setting(KEY_PRIVACY, if on { "1" } else { "0" })
    }

    pub fn set_price_mode(&self, mode: &str) -> Result<()> {
        self.set_setting(KEY_PRICE_MODE, mode)
    }

    pub fn set_jupiter_api_key(&self, key: Option<&str>) -> Result<()> {
        match key.map(str::trim).filter(|s| !s.is_empty()) {
            Some(k) => self.set_setting(KEY_JUPITER_API, k),
            None => self.delete_setting(KEY_JUPITER_API),
        }
    }

    pub fn set_titan_api_key(&self, key: Option<&str>) -> Result<()> {
        match key.map(str::trim).filter(|s| !s.is_empty()) {
            Some(k) => self.set_setting(KEY_TITAN_API, k),
            None => self.delete_setting(KEY_TITAN_API),
        }
    }

    pub fn set_api_paid_fast(&self, on: bool) -> Result<()> {
        self.set_setting(KEY_API_PAID_FAST, if on { "1" } else { "0" })
    }

    pub fn set_write_allowed(&self, allowed: bool) -> Result<()> {
        self.set_setting(KEY_WRITE_ALLOWED, if allowed { "1" } else { "0" })
    }

    pub fn set_write_prompt_skip(&self, skip: bool) -> Result<()> {
        self.set_setting(KEY_WRITE_PROMPT_SKIP, if skip { "1" } else { "0" })
    }

    pub fn set_swap_mode(&self, mode: &str) -> Result<()> {
        self.set_setting(KEY_SWAP_MODE, mode)
    }

    pub fn set_tx_budget_mode(&self, mode: &str) -> Result<()> {
        self.set_setting(KEY_BUDGET_MODE, mode)
    }

    pub fn set_tx_cu_limit(&self, v: u32) -> Result<()> {
        self.set_setting(KEY_CU_LIMIT, &v.to_string())
    }

    pub fn set_tx_tip_lamports(&self, v: u64) -> Result<()> {
        self.set_setting(KEY_TIP_LAMPORTS, &v.to_string())
    }

    pub fn set_tx_loaded_data(&self, v: u32) -> Result<()> {
        self.set_setting(KEY_LOADED_DATA, &v.to_string())
    }

    pub fn set_tx_heap(&self, v: u32) -> Result<()> {
        self.set_setting(KEY_HEAP, &v.to_string())
    }

    pub fn set_tx_cu_margin_bps(&self, v: u32) -> Result<()> {
        self.set_setting(KEY_CU_MARGIN_BPS, &v.to_string())
    }

    pub fn set_tx_tip_floor(&self, v: u64) -> Result<()> {
        self.set_setting(KEY_TIP_FLOOR, &v.to_string())
    }

    pub fn set_tx_tip_ceiling(&self, v: u64) -> Result<()> {
        self.set_setting(KEY_TIP_CEILING, &v.to_string())
    }

    pub fn set_dlmm_strategy(&self, s: &str) -> Result<()> {
        self.set_setting(KEY_DLMM_STRATEGY, s)
    }

    /// Persist a landed write tx (manage / create). More analytics later.
    #[allow(clippy::too_many_arguments)]
    pub fn record_write_tx(
        &self,
        signature: &str,
        op: &str,
        venue: Option<&str>,
        position: Option<&str>,
        pool: Option<&str>,
        tip_lamports: Option<u64>,
        fee_lamports: Option<u64>,
        sol_spent_lamports: Option<u64>,
        summary: Option<&str>,
        paid_label: Option<&str>,
        pool_meta: Option<&WritePoolMeta>,
        token_delta: Option<&WriteTokenDelta>,
    ) -> Result<()> {
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs().to_string())
            .unwrap_or_else(|_| "0".into());
        let pm = pool_meta;
        let td = token_delta;
        // Prefer measured Δ symbols (WSOL→SOL); fall back to pool meta for identity.
        let mint_a = td
            .map(|t| t.mint_a.as_str())
            .or_else(|| pm.map(|m| m.mint_a.as_str()));
        let mint_b = td
            .map(|t| t.mint_b.as_str())
            .or_else(|| pm.map(|m| m.mint_b.as_str()));
        let symbol_a = td
            .map(|t| t.symbol_a.as_str())
            .or_else(|| pm.map(|m| m.symbol_a.as_str()));
        let symbol_b = td
            .map(|t| t.symbol_b.as_str())
            .or_else(|| pm.map(|m| m.symbol_b.as_str()));
        let decimals_a = td
            .map(|t| t.decimals_a as i64)
            .or_else(|| pm.map(|m| m.decimals_a as i64));
        let decimals_b = td
            .map(|t| t.decimals_b as i64)
            .or_else(|| pm.map(|m| m.decimals_b as i64));
        let pool_label = pm.map(|m| m.pool_label.as_str());
        self.conn
            .execute(
                "INSERT OR REPLACE INTO write_txs (
                    created_at, signature, op, venue, position, pool, pool_label,
                    tip_lamports, fee_lamports, sol_spent_lamports, summary, paid_label,
                    mint_a, mint_b, symbol_a, symbol_b, decimals_a, decimals_b, delta_a, delta_b
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                           ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
                params![
                    created_at,
                    signature,
                    op,
                    venue,
                    position,
                    pool,
                    pool_label,
                    tip_lamports.map(|v| v as i64),
                    fee_lamports.map(|v| v as i64),
                    sol_spent_lamports.map(|v| v as i64),
                    summary,
                    paid_label,
                    mint_a,
                    mint_b,
                    symbol_a,
                    symbol_b,
                    decimals_a,
                    decimals_b,
                    td.map(|t| t.delta_a),
                    td.map(|t| t.delta_b),
                ],
            )
            .context("record_write_tx")?;
        Ok(())
    }

    /// Newest-first write history. `position` filters to one position pubkey when set.
    pub fn list_write_txs(&self, limit: usize, position: Option<&str>) -> Result<Vec<WriteTxRow>> {
        let lim = limit.clamp(1, 5_000) as i64;
        let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<WriteTxRow> {
            Ok(WriteTxRow {
                id: row.get::<_, i64>(0)? as u64,
                created_at: row.get(1)?,
                signature: row.get(2)?,
                op: row.get(3)?,
                venue: row.get(4)?,
                position: row.get(5)?,
                pool: row.get(6)?,
                pool_label: row.get(7)?,
                tip_lamports: row.get::<_, Option<i64>>(8)?.map(|v| v.max(0) as u64),
                fee_lamports: row.get::<_, Option<i64>>(9)?.map(|v| v.max(0) as u64),
                sol_spent_lamports: row.get::<_, Option<i64>>(10)?.map(|v| v.max(0) as u64),
                summary: row.get(11)?,
                paid_label: row.get(12)?,
                mint_a: row.get(13)?,
                mint_b: row.get(14)?,
                symbol_a: row.get(15)?,
                symbol_b: row.get(16)?,
                decimals_a: row.get::<_, Option<i64>>(17)?.map(|v| v.clamp(0, 18) as u8),
                decimals_b: row.get::<_, Option<i64>>(18)?.map(|v| v.clamp(0, 18) as u8),
                delta_a: row.get(19)?,
                delta_b: row.get(20)?,
            })
        };
        let sql_all = "SELECT id, created_at, signature, op, venue, position, pool, pool_label,
                        tip_lamports, fee_lamports, sol_spent_lamports, summary, paid_label,
                        mint_a, mint_b, symbol_a, symbol_b, decimals_a, decimals_b, delta_a, delta_b
                 FROM write_txs
                 ORDER BY id DESC
                 LIMIT ?1";
        let sql_pos = "SELECT id, created_at, signature, op, venue, position, pool, pool_label,
                        tip_lamports, fee_lamports, sol_spent_lamports, summary, paid_label,
                        mint_a, mint_b, symbol_a, symbol_b, decimals_a, decimals_b, delta_a, delta_b
                 FROM write_txs
                 WHERE position = ?1
                 ORDER BY id DESC
                 LIMIT ?2";
        let rows = if let Some(pos) = position {
            let mut stmt = self
                .conn
                .prepare(sql_pos)
                .context("list_write_txs prepare position")?;
            stmt.query_map(params![pos, lim], map_row)
                .context("list_write_txs query position")?
                .collect::<Result<Vec<_>, _>>()
                .context("list_write_txs rows position")?
        } else {
            let mut stmt = self
                .conn
                .prepare(sql_all)
                .context("list_write_txs prepare")?;
            stmt.query_map(params![lim], map_row)
                .context("list_write_txs query")?
                .collect::<Result<Vec<_>, _>>()
                .context("list_write_txs rows")?
        };
        Ok(rows)
    }

    pub fn get_token(&self, mint: &str) -> Result<Option<CachedToken>> {
        self.conn
            .query_row(
                "SELECT mint, symbol, decimals FROM tokens WHERE mint = ?1",
                params![mint],
                |row| {
                    Ok(CachedToken {
                        mint: row.get(0)?,
                        symbol: row.get(1)?,
                        decimals: row.get::<_, i64>(2)? as u8,
                    })
                },
            )
            .optional()
            .context("get token")
    }

    pub fn upsert_token(&self, mint: &str, symbol: &str, decimals: u8) -> Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.conn
            .execute(
                "INSERT INTO tokens (mint, symbol, decimals, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(mint) DO UPDATE SET
                   symbol = excluded.symbol,
                   decimals = excluded.decimals,
                   updated_at = excluded.updated_at",
                params![mint, symbol, decimals as i64, now],
            )
            .context("upsert token")?;
        Ok(())
    }

    fn get_setting(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .with_context(|| format!("get setting {key}"))
    }

    fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .with_context(|| format!("set setting {key}"))?;
        Ok(())
    }

    fn delete_setting(&self, key: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM settings WHERE key = ?1", params![key])
            .with_context(|| format!("delete setting {key}"))?;
        Ok(())
    }
}

/// `~/.config/sol-clmm-manager`
pub fn default_config_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config").join(APP_DIR_NAME))
}

pub fn default_db_path() -> Result<PathBuf> {
    Ok(default_config_dir()?.join(DEFAULT_DB_FILE))
}

pub fn active_pointer_path() -> Result<PathBuf> {
    Ok(default_config_dir()?.join(ACTIVE_POINTER_FILE))
}

pub fn read_active_pointer() -> Result<Option<PathBuf>> {
    let pointer = active_pointer_path()?;
    if !pointer.exists() {
        return Ok(None);
    }
    let raw =
        fs::read_to_string(&pointer).with_context(|| format!("read {}", pointer.display()))?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    Ok(Some(PathBuf::from(trimmed)))
}

pub fn write_active_pointer(db_path: &Path) -> Result<()> {
    let dir = default_config_dir()?;
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let abs = if db_path.is_absolute() {
        db_path.to_path_buf()
    } else {
        std::env::current_dir().context("cwd")?.join(db_path)
    };
    let pointer = active_pointer_path()?;
    fs::write(&pointer, format!("{}\n", abs.display()))
        .with_context(|| format!("write {}", pointer.display()))?;
    Ok(())
}

pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return Path::new(&home).join(rest);
    }
    if path == "~"
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home);
    }
    PathBuf::from(path)
}

/// Resolve DB path for this launch: `--db` wins, else last active pointer if file exists.
pub fn resolve_existing_db(cli_db: Option<&Path>) -> Result<Option<PathBuf>> {
    if let Some(p) = cli_db {
        let path = expand_tilde(&p.display().to_string());
        return Ok(Some(path));
    }
    if let Some(path) = read_active_pointer()?
        && path.exists()
    {
        return Ok(Some(path));
    }
    Ok(None)
}

pub fn display_path(path: &Path) -> String {
    let s = path.display().to_string();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        if let Ok(rest) = path.strip_prefix(&home) {
            return format!("~/{}", rest.display());
        }
    }
    s
}

fn format_db_size(n: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let x = n as f64;
    if x >= GB {
        format!("{:.1}G", x / GB)
    } else if x >= MB {
        format!("{:.1}M", x / MB)
    } else if x >= KB {
        format!("{:.0}K", x / KB)
    } else {
        format!("{n}B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn settings_and_tokens_roundtrip() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("whirpool-store-{stamp}.db"));
        let _ = fs::remove_file(&path);

        let store = Store::open(&path, false).unwrap();
        store.set_wallet_pubkey("Abc").unwrap();
        store.set_rpc_url("https://example.com").unwrap();
        store.set_privacy_trim(true).unwrap();
        store.upsert_token("mint1", "USDC", 6).unwrap();

        let s = store.load_settings().unwrap();
        assert_eq!(s.wallet_pubkey.as_deref(), Some("Abc"));
        assert_eq!(s.rpc_url.as_deref(), Some("https://example.com"));
        assert!(s.privacy_trim);

        let t = store.get_token("mint1").unwrap().unwrap();
        assert_eq!(t.symbol, "USDC");
        assert_eq!(t.decimals, 6);

        let store2 = Store::open(&path, true).unwrap();
        assert!(store2.load_settings().unwrap().wallet_pubkey.is_none());
        assert!(store2.get_token("mint1").unwrap().is_none());

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn overwrite_requires_existing_file_gone() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("whirpool-store-ow-{stamp}.db"));
        fs::write(&path, b"not-sqlite").unwrap();
        // opening without overwrite should fail on corrupt? sqlite may create over or fail
        // overwrite true removes first
        let store = Store::open(&path, true).unwrap();
        store.set_rpc_url("https://x").unwrap();
        assert!(path.exists());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn expand_tilde_home() {
        if std::env::var_os("HOME").is_some() {
            let p = expand_tilde("~/.config/sol-clmm-manager/sol-clmm.db");
            assert!(p.is_absolute());
            assert!(p.ends_with("sol-clmm.db"));
        }
    }

    #[test]
    fn default_paths_ok() {
        if std::env::var_os("HOME").is_none() {
            return;
        }
        let p = default_db_path().unwrap();
        assert!(p.ends_with(DEFAULT_DB_FILE));
    }

    #[test]
    fn resolve_cli_db() {
        let p = resolve_existing_db(Some(Path::new("/tmp/x.db")))
            .unwrap()
            .unwrap();
        assert_eq!(p, PathBuf::from("/tmp/x.db"));
    }

    #[test]
    fn file_size_label_after_open() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("sol-clmm-size-{stamp}.db"));
        let _ = fs::remove_file(&path);
        let store = Store::open(&path, false).unwrap();
        store.set_rpc_url("https://x").unwrap();
        let n = store.file_size_bytes().unwrap();
        assert!(n > 0);
        assert!(!store.file_size_label().is_empty());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn format_db_size_units() {
        assert_eq!(format_db_size(500), "500B");
        assert_eq!(format_db_size(2048), "2K");
        assert_eq!(format_db_size(2 * 1024 * 1024), "2.0M");
    }

    #[test]
    fn write_tx_roundtrip_list_newest_first() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("sol-clmm-wtx-{stamp}.db"));
        let _ = fs::remove_file(&path);
        let store = Store::open(&path, false).unwrap();
        let meta = WritePoolMeta {
            pool_label: "SOL / USDC".into(),
            mint_a: "MintA".into(),
            mint_b: "MintB".into(),
            symbol_a: "SOL".into(),
            symbol_b: "USDC".into(),
            decimals_a: 9,
            decimals_b: 6,
        };
        store
            .record_write_tx(
                "sigAAA",
                "claim",
                Some("orca"),
                Some("pos1"),
                Some("pool1"),
                Some(100),
                Some(5000),
                Some(6000),
                Some("claim fees"),
                None,
                Some(&meta),
                None,
            )
            .unwrap();
        let td = WriteTokenDelta {
            mint_a: "MintA".into(),
            mint_b: "MintB".into(),
            symbol_a: "SOL".into(),
            symbol_b: "USDC".into(),
            decimals_a: 9,
            decimals_b: 6,
            delta_a: -1_000_000,
            delta_b: 500_000,
        };
        store
            .record_write_tx(
                "sigBBB",
                "compound",
                Some("orca"),
                Some("pos1"),
                Some("pool1"),
                Some(200),
                None,
                None,
                Some("compound no-swap"),
                None,
                Some(&meta),
                Some(&td),
            )
            .unwrap();
        store
            .record_write_tx(
                "sigCCC",
                "deposit",
                Some("orca"),
                Some("pos2"),
                Some("pool2"),
                None,
                None,
                None,
                Some("other position"),
                None,
                Some(&meta),
                None,
            )
            .unwrap();
        let rows = store.list_write_txs(10, None).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].signature, "sigCCC");
        let pos1 = store.list_write_txs(10, Some("pos1")).unwrap();
        assert_eq!(pos1.len(), 2);
        assert_eq!(pos1[0].signature, "sigBBB");
        assert_eq!(pos1[0].delta_a, Some(-1_000_000));
        assert_eq!(pos1[0].pair_label(), "SOL/USDC");
        assert!(pos1[0].token_delta_label().contains("USDC"));
        assert_eq!(pos1[1].op, "claim");
        assert!(pos1[1].delta_a.is_none());
        assert_eq!(pos1[1].pool_label.as_deref(), Some("SOL / USDC"));
        let _ = fs::remove_file(&path);
    }
}
